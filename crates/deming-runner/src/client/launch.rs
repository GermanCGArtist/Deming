use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use deming_core::{DemingError, InstanceState, Result, RunEvent};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

use crate::scheduler::AbortHandle;
use crate::win_job::{configure_tokio_child, JobHandle};
use crate::{absolutize_executable, absolutize_path};

use super::config::{ensure_default_options_txt, ClientLaunchConfig};
use super::monitor::{
    emit_state, rule_supports_early_completion, spawn_log_reader, EarlyMatchTracker,
};

/// Captured execution details from running a client instance to completion, timeout, or abort.
#[derive(Debug, Clone)]
pub struct ClientRunOutcome {
    /// Instance identifier.
    pub instance_id: String,
    /// OS process ID assigned to the launched process, if available.
    pub pid: Option<u32>,
    /// Exit code returned by the process, if it exited cleanly or with a code.
    pub exit_code: Option<i32>,
    /// `true` if the client exceeded `timeout_secs` and was terminated.
    pub timed_out: bool,
    /// `true` if the run was aborted via [`AbortHandle`].
    pub aborted: bool,
    /// Captured stdout lines in order.
    pub stdout_lines: Vec<String>,
    /// Captured stderr lines in order.
    pub stderr_lines: Vec<String>,
    /// Combined stdout + stderr lines in arrival order.
    pub combined_logs: Vec<String>,
    /// Timestamp when the process was spawned.
    pub started_at: DateTime<Utc>,
    /// Timestamp when the process exited or was terminated.
    pub finished_at: DateTime<Utc>,
}

/// Spawns a client JVM inside a Windows Job Object, streams stdout/stderr,
/// applies CPU affinity/priority, and enforces `config.timeout_secs` and `abort`.
pub async fn run_client(
    config: ClientLaunchConfig,
    abort: &AbortHandle,
    events: Option<mpsc::UnboundedSender<RunEvent>>,
) -> Result<ClientRunOutcome> {
    if abort.is_aborted() {
        return Err(DemingError::Process(format!(
            "client '{}' skipped because run is aborted",
            config.spec.id
        )));
    }

    let instance_dir = absolutize_path(&config.instance_dir);
    let java_exe = absolutize_executable(&config.java_exe);

    std::fs::create_dir_all(&instance_dir)?;
    ensure_default_options_txt(&instance_dir)?;

    let job = Arc::new(JobHandle::new()?);
    abort.register_job(Arc::clone(&job))?;

    emit_state(
        &events,
        config.run_id,
        &config.spec.id,
        InstanceState::Starting,
        Some("spawning client process".to_string()),
    );

    let args = config.build_args();
    let mut cmd = Command::new(&java_exe);
    cmd.args(&args)
        .current_dir(&instance_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let started_at = Utc::now();
    let mut child = cmd.spawn().map_err(|e| {
        DemingError::Process(format!(
            "failed to spawn client '{}' ({}): {e}",
            config.spec.id,
            config.java_exe.display()
        ))
    })?;

    let pid = child.id();

    if let Err(err) = job.assign_tokio_child(&child) {
        let _ = child.start_kill();
        let _ = job.terminate_all();
        return Err(err);
    }

    if let Err(err) = configure_tokio_child(&child, config.cpu_cores, config.priority) {
        let _ = job.terminate_all();
        return Err(err);
    }

    if let (Some(p), Some(cb)) = (pid, &config.on_pid) {
        cb(p);
    }

    emit_state(
        &events,
        config.run_id,
        &config.spec.id,
        InstanceState::Running,
        pid.map(|p| format!("running (pid {p})")),
    );

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_buf = Arc::new(Mutex::new(Vec::new()));
    let stderr_buf = Arc::new(Mutex::new(Vec::new()));
    let combined_buf = Arc::new(Mutex::new(Vec::new()));

    let (early_tracker, early_rx) = match rule_supports_early_completion(&config.spec.test_rule) {
        Some((success_res, failure_res)) => {
            let (tx, rx) = oneshot::channel::<()>();
            (
                Some(Arc::new(Mutex::new(EarlyMatchTracker::new(
                    success_res,
                    failure_res,
                    tx,
                )))),
                Some(rx),
            )
        }
        None => (None, None),
    };

    let mut reader_tasks = Vec::new();

    if let Some(out) = stdout {
        reader_tasks.push(spawn_log_reader(
            out,
            Arc::clone(&stdout_buf),
            Arc::clone(&combined_buf),
            events.clone(),
            config.run_id,
            config.spec.id.clone(),
            early_tracker.clone(),
        ));
    }

    if let Some(err_stream) = stderr {
        reader_tasks.push(spawn_log_reader(
            err_stream,
            Arc::clone(&stderr_buf),
            Arc::clone(&combined_buf),
            events.clone(),
            config.run_id,
            config.spec.id.clone(),
            early_tracker.clone(),
        ));
    }

    let mut timed_out = false;
    let mut aborted = false;
    let mut exit_code = None;

    let early_fut = async move {
        match early_rx {
            Some(rx) => {
                let _ = rx.await;
            }
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(early_fut);

    if config.timeout_secs > 0 {
        let sleep = tokio::time::sleep(Duration::from_secs(config.timeout_secs));
        tokio::pin!(sleep);

        tokio::select! {
            status = child.wait() => {
                exit_code = status?.code();
            }
            _ = &mut early_fut => {
                let _ = job.terminate_all();
                let _ = child.wait().await;
            }
            _ = abort.cancelled() => {
                aborted = true;
                let _ = job.terminate_all();
                let _ = child.wait().await;
            }
            _ = &mut sleep => {
                timed_out = true;
                let _ = job.terminate_all();
                let _ = child.wait().await;
            }
        }
    } else {
        tokio::select! {
            status = child.wait() => {
                exit_code = status?.code();
            }
            _ = &mut early_fut => {
                let _ = job.terminate_all();
                let _ = child.wait().await;
            }
            _ = abort.cancelled() => {
                aborted = true;
                let _ = job.terminate_all();
                let _ = child.wait().await;
            }
        }
    }

    for task in reader_tasks {
        let _ = task.await;
    }

    let finished_at = Utc::now();
    let stdout_lines = stdout_buf.lock().map(|g| g.clone()).unwrap_or_default();
    let stderr_lines = stderr_buf.lock().map(|g| g.clone()).unwrap_or_default();
    let combined_logs = combined_buf.lock().map(|g| g.clone()).unwrap_or_default();

    Ok(ClientRunOutcome {
        instance_id: config.spec.id,
        pid,
        exit_code,
        timed_out,
        aborted,
        stdout_lines,
        stderr_lines,
        combined_logs,
        started_at,
        finished_at,
    })
}
