//! Minecraft client JVM launch, auto-connect flags, stdout/stderr streaming, and timeout enforcement.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use deming_core::{AccountRef, DemingError, InstanceSpec, InstanceState, ProcessPriority, Result, RunEvent};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::scheduler::AbortHandle;
use crate::win_job::{configure_tokio_child, JobHandle};

/// Determines whether the given Minecraft version supports `--quickPlayMultiplayer` (`>= 1.20`).
pub fn supports_quick_play(mc_version: &str) -> bool {
    let v = mc_version.trim();
    if v.is_empty() {
        return false;
    }

    // Handle Mojang snapshot format like "23w14a" (1.20 snapshot week 14 of 2023) or "24w01a".
    if let Some((year_str, rest)) = v.split_once('w') {
        if let Ok(year) = year_str.parse::<u32>() {
            let week_digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(week) = week_digits.parse::<u32>() {
                return year > 23 || (year == 23 && week >= 14);
            }
        }
    }

    // Strip pre-release / rc / qualifier suffixes (e.g. "1.20-pre1", "1.21.4-rc1").
    let core = v.split(['-', '_', '+']).next().unwrap_or(v);
    let mut parts = core.split('.');
    let major = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let minor = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);

    (major, minor) >= (1, 20)
}

/// Builds the auto-connect game arguments for connecting to `host:port` based on `mc_version`.
pub fn build_connect_args(mc_version: &str, host: &str, port: u16) -> Vec<String> {
    if supports_quick_play(mc_version) {
        vec![
            "--quickPlayMultiplayer".to_string(),
            format!("{host}:{port}"),
        ]
    } else {
        vec![
            "--server".to_string(),
            host.to_string(),
            "--port".to_string(),
            port.to_string(),
        ]
    }
}

/// Configuration for launching a single Minecraft client instance.
#[derive(Debug, Clone)]
pub struct ClientLaunchConfig {
    /// Run ID for emitted [`RunEvent`]s.
    pub run_id: Uuid,
    /// Path to the `java` executable (or custom executable when testing).
    pub java_exe: PathBuf,
    /// Instance working directory (`--gameDir`).
    pub instance_dir: PathBuf,
    /// Instance specification.
    pub spec: InstanceSpec,
    /// Main class to invoke (e.g., `net.minecraft.client.main.Main`).
    /// When `None`, `classpath` / `jar` is omitted unless specified, allowing direct command testing.
    pub main_class: Option<String>,
    /// Classpath entries joined with the OS path separator (`;` on Windows, `:` elsewhere).
    pub classpath: Vec<PathBuf>,
    /// Optional natives directory (`-Djava.library.path=...`).
    pub natives_dir: Option<PathBuf>,
    /// Optional assets directory (`--assetsDir`).
    pub assets_dir: Option<PathBuf>,
    /// Optional asset index name (`--assetIndex`).
    pub asset_index: Option<String>,
    /// Optional access token for authenticated Microsoft launches (`--accessToken`).
    pub access_token: Option<String>,
    /// Resolved RAM budget in MiB (`-Xmx{ram_mb}M`).
    pub ram_mb: u64,
    /// Resolved CPU core affinity limit.
    pub cpu_cores: Option<usize>,
    /// OS scheduling priority.
    pub priority: ProcessPriority,
    /// Optional `(host, port)` to auto-connect to on startup.
    pub connect_server: Option<(String, u16)>,
    /// Execution timeout in seconds (`0` disables the timeout).
    pub timeout_secs: u64,
    /// Additional JVM flags appended after `-Xmx` and `spec.jvm_args`.
    pub extra_jvm_args: Vec<String>,
    /// Additional game arguments appended after `spec.game_args`.
    pub extra_game_args: Vec<String>,
}

impl ClientLaunchConfig {
    /// Builds the full argument vector for launching the client JVM.
    pub fn build_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        if self.main_class.is_some() {
            if self.ram_mb > 0 {
                let xms = self.ram_mb.min(512).max(128.min(self.ram_mb));
                args.push(format!("-Xms{xms}M"));
                args.push(format!("-Xmx{}M", self.ram_mb));
            }
            if let Some(natives) = &self.natives_dir {
                args.push(format!("-Djava.library.path={}", natives.display()));
            }
        }

        args.extend(self.spec.jvm_args.iter().cloned());
        args.extend(self.extra_jvm_args.iter().cloned());

        if !self.classpath.is_empty() {
            let sep = if cfg!(windows) { ";" } else { ":" };
            let cp = self
                .classpath
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(sep);
            args.push("-cp".to_string());
            args.push(cp);
        }

        if let Some(main_class) = &self.main_class {
            args.push(main_class.clone());
            args.push("--version".to_string());
            args.push(self.spec.mc_version.clone());
            args.push("--gameDir".to_string());
            args.push(self.instance_dir.to_string_lossy().into_owned());

            if let Some(assets_dir) = &self.assets_dir {
                args.push("--assetsDir".to_string());
                args.push(assets_dir.to_string_lossy().into_owned());
            }
            if let Some(asset_index) = &self.asset_index {
                args.push("--assetIndex".to_string());
                args.push(asset_index.clone());
            }

            match &self.spec.account {
                AccountRef::Offline { username } => {
                    args.push("--username".to_string());
                    args.push(username.clone());
                    args.push("--userType".to_string());
                    args.push("legacy".to_string());
                    args.push("--accessToken".to_string());
                    args.push(self.access_token.clone().unwrap_or_else(|| "0".to_string()));
                }
                AccountRef::Microsoft { uuid, username } => {
                    args.push("--username".to_string());
                    args.push(username.clone());
                    args.push("--uuid".to_string());
                    args.push(uuid.clone());
                    args.push("--userType".to_string());
                    args.push("msa".to_string());
                    if let Some(token) = &self.access_token {
                        args.push("--accessToken".to_string());
                        args.push(token.clone());
                    }
                }
            }
        }

        if let Some((host, port)) = &self.connect_server {
            args.extend(build_connect_args(&self.spec.mc_version, host, *port));
        }

        args.extend(self.spec.game_args.iter().cloned());
        args.extend(self.extra_game_args.iter().cloned());

        args
    }
}

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

    std::fs::create_dir_all(&config.instance_dir)?;

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
    let mut cmd = Command::new(&config.java_exe);
    cmd.args(&args)
        .current_dir(&config.instance_dir)
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

    let mut reader_tasks = Vec::new();

    if let Some(out) = stdout {
        let out_ref = Arc::clone(&stdout_buf);
        let comb_ref = Arc::clone(&combined_buf);
        let events_ref = events.clone();
        let run_id = config.run_id;
        let source = config.spec.id.clone();
        reader_tasks.push(tokio::spawn(async move {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(mut g) = out_ref.lock() {
                    g.push(line.clone());
                }
                if let Ok(mut g) = comb_ref.lock() {
                    g.push(line.clone());
                }
                if let Some(tx) = &events_ref {
                    let _ = tx.send(RunEvent::LogLine {
                        run_id,
                        source: source.clone(),
                        line,
                        timestamp: Utc::now(),
                    });
                }
            }
        }));
    }

    if let Some(err_stream) = stderr {
        let err_ref = Arc::clone(&stderr_buf);
        let comb_ref = Arc::clone(&combined_buf);
        let events_ref = events.clone();
        let run_id = config.run_id;
        let source = config.spec.id.clone();
        reader_tasks.push(tokio::spawn(async move {
            let mut lines = BufReader::new(err_stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(mut g) = err_ref.lock() {
                    g.push(line.clone());
                }
                if let Ok(mut g) = comb_ref.lock() {
                    g.push(line.clone());
                }
                if let Some(tx) = &events_ref {
                    let _ = tx.send(RunEvent::LogLine {
                        run_id,
                        source: source.clone(),
                        line,
                        timestamp: Utc::now(),
                    });
                }
            }
        }));
    }

    let mut timed_out = false;
    let mut aborted = false;
    let mut exit_code = None;

    if config.timeout_secs > 0 {
        let sleep = tokio::time::sleep(Duration::from_secs(config.timeout_secs));
        tokio::pin!(sleep);

        tokio::select! {
            status = child.wait() => {
                exit_code = status?.code();
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

fn emit_state(
    events: &Option<mpsc::UnboundedSender<RunEvent>>,
    run_id: Uuid,
    instance_id: &str,
    state: InstanceState,
    message: Option<String>,
) {
    if let Some(tx) = events {
        let _ = tx.send(RunEvent::InstanceStateChanged {
            run_id,
            instance_id: instance_id.to_string(),
            state,
            message,
            timestamp: Utc::now(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deming_core::{ModLoader, TestCompletionRule};

    #[test]
    fn build_args_includes_ram_classpath_account_and_connect_flags() {
        let cfg = ClientLaunchConfig {
            run_id: Uuid::new_v4(),
            java_exe: PathBuf::from("java"),
            instance_dir: PathBuf::from("instances/c1"),
            spec: InstanceSpec {
                id: "c1".to_string(),
                mc_version: "1.21.4".to_string(),
                loader: ModLoader::Fabric,
                loader_version: None,
                mods: vec![],
                account: AccountRef::Offline {
                    username: "TesterOne".to_string(),
                },
                ram_mb: Some(2048),
                cpu_cores: Some(2),
                jvm_args: vec!["-XX:+UseG1GC".to_string()],
                game_args: vec!["--width".to_string(), "1280".to_string()],
                test_rule: TestCompletionRule::ExitCodeZero,
            },
            main_class: Some("net.fabricmc.loader.impl.launch.knot.KnotClient".to_string()),
            classpath: vec![PathBuf::from("a.jar"), PathBuf::from("b.jar")],
            natives_dir: Some(PathBuf::from("natives")),
            assets_dir: Some(PathBuf::from("assets")),
            asset_index: Some("19".to_string()),
            access_token: None,
            ram_mb: 2048,
            cpu_cores: Some(2),
            priority: ProcessPriority::Normal,
            connect_server: Some(("127.0.0.1".to_string(), 25577)),
            timeout_secs: 60,
            extra_jvm_args: vec![],
            extra_game_args: vec![],
        };

        let args = cfg.build_args();
        assert!(args.contains(&"-Xmx2048M".to_string()));
        assert!(args.contains(&"-XX:+UseG1GC".to_string()));
        assert!(args.contains(&"--quickPlayMultiplayer".to_string()));
        assert!(args.contains(&"127.0.0.1:25577".to_string()));
        assert!(args.contains(&"--username".to_string()));
        assert!(args.contains(&"TesterOne".to_string()));
    }
}
