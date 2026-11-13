//! End-to-end test job orchestrator shared by CLI, MCP, and GUI.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use deming_archive::{apply_retention, RunArchiveWriter};
use deming_auth::AccountStore;
use deming_core::{
    DemingConfig, InstanceResult, InstanceSpec, InstanceState, RunEvent, RunManifest, RunStatus,
    TestJobSpec,
};
use deming_fetch::{
    ensure_java_runtime_with_progress, required_java_version,
    resolve_and_install_loader_with_progress, resolve_mods_with_progress, LoaderSide,
    ModrinthClient,
};
use deming_ipc::{IpcClient, IpcMessage, IpcServer};
use deming_runner::{
    evaluate_rule, index_screenshots, run_client, AbortHandle, ClientLaunchConfig,
    EvaluationContext, ResourceScheduler, ServerLaunchConfig, ServerProcess,
};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::helpers::{
    emit_instance_state, fail_result, make_progress_callback, resolve_account_token,
    strip_unexpanded_mojang_args,
};

/// Callback invoked once a run's [`RunArchiveWriter`] directory has been created.
pub type RunInitializedCallback = Arc<dyn Fn(Uuid, &Path) + Send + Sync>;

/// Callback invoked when an instance is registered so callers (e.g. GUI) can attach per-instance controls.
pub type InstanceRegisteredCallback = Arc<dyn Fn(&str, AbortHandle, &Path) + Send + Sync>;

/// Optional hooks and overrides for [`execute_job_with_options`] and [`execute_job_with_ipc_options`].
#[derive(Clone, Default)]
pub struct JobExecutionOptions {
    /// Pre-assigned run identifier (generates a random UUID v4 when `None`).
    pub run_id: Option<Uuid>,
    /// Optional live [`RunEvent`] sink (in addition to `RunArchiveWriter` and `IpcClient`).
    pub event_tx: Option<mpsc::UnboundedSender<RunEvent>>,
    /// Invoked once the run archive directory and initial `manifest.json` are created.
    pub on_initialized: Option<RunInitializedCallback>,
    /// Invoked for each instance before launch with its dedicated [`AbortHandle`] and working directory.
    pub on_instance_registered: Option<InstanceRegisteredCallback>,
}

/// Executes [`execute_job`] while ensuring an IPC endpoint is active in `config.workspace_dir`.
///
/// If no GUI [`IpcServer`] is already running, binds a temporary [`IpcServer`] for the duration
/// of the job so `deming run abort` or MCP `abort_run` can abort CLI-launched runs.
pub async fn execute_job_with_ipc(
    config: &DemingConfig,
    job: &TestJobSpec,
    abort: Arc<AbortHandle>,
) -> Result<RunManifest> {
    execute_job_with_ipc_options(config, job, abort, JobExecutionOptions::default()).await
}

/// Executes [`execute_job_with_options`] while ensuring an IPC endpoint is active in `config.workspace_dir`.
pub async fn execute_job_with_ipc_options(
    config: &DemingConfig,
    job: &TestJobSpec,
    abort: Arc<AbortHandle>,
    options: JobExecutionOptions,
) -> Result<RunManifest> {
    let mut abs_ws = config.workspace_dir.clone();
    if abs_ws.is_relative() {
        abs_ws = std::env::current_dir()
            .context("failed to determine current working directory")?
            .join(&abs_ws);
    }

    let existing_client = IpcClient::connect(&abs_ws).await.ok();
    let has_existing_server = existing_client.as_ref().is_some_and(|c| c.is_connected());

    let mut local_server_task = None;
    let mut local_server_stop = None;
    let mut active_client = existing_client;

    if !has_existing_server {
        if let Ok(mut server) = IpcServer::bind(&abs_ws).await {
            let port = server.local_addr().port();
            active_client = IpcClient::connect_to_port(port).await.ok();
            let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
            let abort_clone = Arc::clone(&abort);
            let task = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = &mut stop_rx => break,
                        msg = server.recv() => {
                            match msg {
                                Some(IpcMessage::AbortAll) | Some(IpcMessage::AbortRun { .. }) => {
                                    let _ = abort_clone.abort();
                                }
                                None => break,
                                _ => {}
                            }
                        }
                    }
                }
                server.stop().await;
            });
            local_server_task = Some(task);
            local_server_stop = Some(stop_tx);
        }
    }

    let result =
        execute_job_with_options(config, job, abort, active_client.as_ref(), options).await;

    if let Some(tx) = local_server_stop {
        let _ = tx.send(());
    }
    if let Some(task) = local_server_task {
        let _ = task.await;
    }

    result
}

/// Executes a [`TestJobSpec`] end-to-end with default [`JobExecutionOptions`].
pub async fn execute_job(
    config: &DemingConfig,
    job: &TestJobSpec,
    abort: Arc<AbortHandle>,
    ipc: Option<&IpcClient>,
) -> Result<RunManifest> {
    execute_job_with_options(config, job, abort, ipc, JobExecutionOptions::default()).await
}

/// Executes a [`TestJobSpec`] end-to-end with custom [`JobExecutionOptions`].
pub async fn execute_job_with_options(
    config: &DemingConfig,
    job: &TestJobSpec,
    abort: Arc<AbortHandle>,
    ipc: Option<&IpcClient>,
    options: JobExecutionOptions,
) -> Result<RunManifest> {
    let mut abs_config = config.clone();
    if abs_config.workspace_dir.is_relative() {
        abs_config.workspace_dir = std::env::current_dir()
            .context("failed to determine current working directory")?
            .join(&abs_config.workspace_dir);
    }
    let config = &abs_config;

    let run_id = options.run_id.unwrap_or_else(Uuid::new_v4);
    let created_at = Utc::now();

    let archive = Arc::new(Mutex::new(
        RunArchiveWriter::with_timestamp(config.runs_dir(), run_id, created_at)
            .context("failed to initialize RunArchiveWriter")?,
    ));

    let initial_instances: Vec<InstanceResult> = job
        .instances
        .iter()
        .map(|inst| InstanceResult {
            instance_id: inst.id.clone(),
            state: InstanceState::Queued,
            exit_code: None,
            passed: false,
            reason: None,
            started_at: None,
            finished_at: None,
            log_path: None,
            screenshots: Vec::new(),
        })
        .collect();

    let mut manifest = RunManifest {
        run_id,
        job_name: job.name.clone(),
        status: RunStatus::Running,
        created_at,
        started_at: Some(created_at),
        finished_at: None,
        spec: job.clone(),
        instances: initial_instances,
        server_log_path: None,
        bundle_path: None,
    };

    {
        let writer = archive.lock().await;
        writer
            .write_manifest(&manifest)
            .context("failed to write initial manifest.json")?;
        if let Some(ref cb) = options.on_initialized {
            cb(run_id, writer.run_dir());
        }
    }

    let run_started_event = RunEvent::RunStarted {
        run_id,
        job_name: job.name.clone(),
        timestamp: created_at,
    };
    {
        let mut writer = archive.lock().await;
        writer.append_event(&run_started_event)?;
    }
    if let Some(ref sink) = options.event_tx {
        let _ = sink.send(run_started_event.clone());
    }
    if let Some(client) = ipc {
        let _ = client.run_started(run_id.to_string(), &job.name).await;
        let _ = client
            .publish_event(run_id.to_string(), run_started_event)
            .await;
    }

    // Event forwarding task: persists RunEvents to archive, forwards to optional live sink, and broadcasts over IPC.
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<RunEvent>();
    let archive_for_events = Arc::clone(&archive);
    let ipc_for_events = ipc.cloned();
    let live_sink = options.event_tx.clone();
    let run_id_str = run_id.to_string();
    let event_task = tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            {
                let mut writer = archive_for_events.lock().await;
                let _ = writer.append_event(&event);
                if let RunEvent::LogLine { source, line, .. } = &event {
                    if source == "server" {
                        let _ = writer.append_server_line(line);
                    } else {
                        let _ = writer.append_instance_stdout(source, line);
                        let _ = writer.append_instance_latest_log(source, line);
                    }
                }
            }
            if let Some(ref sink) = live_sink {
                let _ = sink.send(event.clone());
            }
            if let Some(ref client) = ipc_for_events {
                let _ = client.publish_event(&run_id_str, event).await;
            }
        }
    });

    // Optional IPC command listener for remote AbortRun / AbortAll.
    let ipc_listener_task = ipc.filter(|c| c.is_connected()).cloned().map(|client| {
        let abort_clone = Arc::clone(&abort);
        let target_run_id = run_id.to_string();
        tokio::spawn(async move {
            while let Ok(Some(msg)) = client.recv().await {
                match msg {
                    IpcMessage::AbortAll => {
                        let _ = abort_clone.abort();
                        break;
                    }
                    IpcMessage::AbortRun { run_id: rid } if rid == target_run_id => {
                        let _ = abort_clone.abort();
                        break;
                    }
                    _ => {}
                }
            }
        })
    });

    let account_store = AccountStore::from_config(config);

    struct PrepState {
        modrinth: ModrinthClient,
        server_process: Option<ServerProcess>,
        connect_target: Option<(String, u16)>,
    }

    // Resolve mods client & optional dedicated server.
    let prep_result: Result<PrepState> = async {
        let modrinth = ModrinthClient::new().context("failed to initialize ModrinthClient")?;
        let mut server_process: Option<ServerProcess> = None;
        let mut connect_target: Option<(String, u16)> = None;

        if let Some(server_spec) = &job.server {
            let server_dir = config.servers_dir().join(run_id.to_string());
            tokio::fs::create_dir_all(&server_dir).await?;

            let java_ver = required_java_version(&server_spec.mc_version);
            let java = ensure_java_runtime_with_progress(
                &config.cache_dir(),
                java_ver,
                Some(make_progress_callback(
                    event_tx.clone(),
                    run_id,
                    None,
                    "java",
                )),
            )
            .await
            .context("failed to provision Java runtime for server")?;

            let profile = resolve_and_install_loader_with_progress(
                config,
                &server_spec.mc_version,
                server_spec.loader,
                server_spec.loader_version.as_deref(),
                LoaderSide::Server,
                &server_dir,
                Some(&java.executable),
                Some(make_progress_callback(
                    event_tx.clone(),
                    run_id,
                    None,
                    "loader",
                )),
            )
            .await
            .context("failed to resolve/install server loader")?;

            resolve_mods_with_progress(
                &modrinth,
                &server_spec.mods,
                &server_spec.mc_version,
                server_spec.loader,
                &config.cache_dir(),
                &server_dir.join("mods"),
                Some(make_progress_callback(
                    event_tx.clone(),
                    run_id,
                    None,
                    "mods",
                )),
            )
            .await
            .context("failed to resolve server mods")?;

            let ram_mb = server_spec
                .ram_mb
                .unwrap_or(config.global_limits.default_instance_ram_mb);

            let (jvm_args, program_args) = if profile.server_jar.is_some() {
                let mut prog = profile.game_args.clone();
                if !prog.iter().any(|a| a == "nogui") {
                    prog.push("nogui".to_string());
                }
                (profile.jvm_args.clone(), prog)
            } else {
                let mut jvm = Vec::new();
                if ram_mb > 0 {
                    let xms = ram_mb.min(512).max(128.min(ram_mb));
                    jvm.push(format!("-Xms{xms}M"));
                    jvm.push(format!("-Xmx{ram_mb}M"));
                }
                jvm.extend(profile.jvm_args.iter().cloned());
                if !profile.classpath.is_empty() {
                    let sep = if cfg!(windows) { ";" } else { ":" };
                    let cp = profile
                        .classpath
                        .iter()
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join(sep);
                    jvm.push("-cp".to_string());
                    jvm.push(cp);
                }
                let mut prog = Vec::new();
                if !profile.main_class.is_empty() {
                    prog.push(profile.main_class.clone());
                }
                prog.extend(profile.game_args.iter().cloned());
                if !prog.iter().any(|a| a == "nogui") {
                    prog.push("nogui".to_string());
                }
                (jvm, prog)
            };

            let server_cfg = ServerLaunchConfig {
                run_id,
                java_exe: java.executable,
                server_dir,
                server_jar: profile.server_jar,
                jvm_args,
                program_args,
                spec: server_spec.clone(),
                ram_mb,
                cpu_cores: Some(config.global_limits.default_cpu_cores_per_instance),
                priority: config.global_limits.process_priority,
                startup_timeout_secs: job.timeout_secs.max(30),
            };

            let proc = ServerProcess::start(server_cfg, &abort, Some(event_tx.clone()))
                .await
                .context("failed to start dedicated server")?;
            let port = proc.port();
            if server_spec.auto_connect_clients {
                connect_target = Some(("127.0.0.1".to_string(), port));
            }
            server_process = Some(proc);
        }

        Ok(PrepState {
            modrinth,
            server_process,
            connect_target,
        })
    }
    .await;

    let PrepState {
        modrinth,
        server_process,
        connect_target,
    } = match prep_result {
        Ok(vals) => vals,
        Err(e) => {
            let finished_at = Utc::now();
            manifest.status = if abort.is_aborted() {
                RunStatus::Aborted
            } else {
                RunStatus::Failed
            };
            manifest.finished_at = Some(finished_at);
            let _ = event_tx.send(RunEvent::RunFinished {
                run_id,
                status: manifest.status,
                timestamp: finished_at,
            });
            drop(event_tx);
            let _ = event_task.await;
            if let Some(listener) = ipc_listener_task {
                listener.abort();
            }
            let writer = archive.lock().await;
            let _ = writer.write_manifest(&manifest);
            return Err(e);
        }
    };

    if server_process.is_some() {
        manifest.server_log_path = Some(PathBuf::from("server").join("latest.log"));
    }

    // Schedule and run all client instances in parallel.
    let scheduler = Arc::new(ResourceScheduler::from_limits(&config.global_limits));
    let mut join_set = JoinSet::new();

    for inst in job.instances.clone() {
        let cfg_clone = config.clone();
        let abort_clone = Arc::clone(&abort);
        let scheduler_clone = Arc::clone(&scheduler);
        let archive_clone = Arc::clone(&archive);
        let event_tx_clone = event_tx.clone();
        let modrinth_clone = modrinth.clone();
        let store_clone = account_store.clone();
        let connect_clone = connect_target.clone();
        let timeout_secs = job.timeout_secs;
        let on_inst_cb = options.on_instance_registered.clone();

        join_set.spawn(async move {
            run_single_instance(
                run_id,
                &cfg_clone,
                inst,
                timeout_secs,
                connect_clone,
                abort_clone,
                scheduler_clone,
                archive_clone,
                event_tx_clone,
                modrinth_clone,
                store_clone,
                on_inst_cb,
            )
            .await
        });
    }

    let mut results = Vec::with_capacity(job.instances.len());
    while let Some(joined) = join_set.join_next().await {
        match joined {
            Ok(res) => results.push(res),
            Err(e) => {
                results.push(InstanceResult {
                    instance_id: "unknown".to_string(),
                    state: InstanceState::Failed,
                    exit_code: None,
                    passed: false,
                    reason: Some(format!("instance task panicked: {e}")),
                    started_at: None,
                    finished_at: Some(Utc::now()),
                    log_path: None,
                    screenshots: Vec::new(),
                });
            }
        }
    }

    if let Some(mut server) = server_process {
        let _ = server.shutdown(Duration::from_secs(10)).await;
    }

    if let Some(listener) = ipc_listener_task {
        listener.abort();
    }

    let any_aborted =
        abort.is_aborted() || results.iter().any(|r| r.state == InstanceState::Aborted);
    let all_passed = !results.is_empty() && results.iter().all(|r| r.passed);
    let final_status = if any_aborted {
        RunStatus::Aborted
    } else if all_passed {
        RunStatus::Passed
    } else {
        RunStatus::Failed
    };

    let finished_at = Utc::now();
    manifest.instances = results;
    manifest.status = final_status;
    manifest.finished_at = Some(finished_at);

    let _ = event_tx.send(RunEvent::RunFinished {
        run_id,
        status: final_status,
        timestamp: finished_at,
    });
    drop(event_tx);
    let _ = event_task.await;

    {
        let writer = archive.lock().await;
        writer
            .write_manifest(&manifest)
            .context("failed to write final manifest.json")?;
    }

    if config.retention.auto_prune {
        if let Err(e) = apply_retention(config.runs_dir(), &config.retention) {
            eprintln!("warning: failed to apply retention policy: {e}");
        }
    }

    Ok(manifest)
}

#[allow(clippy::too_many_arguments)]
async fn run_single_instance(
    run_id: Uuid,
    config: &DemingConfig,
    inst: InstanceSpec,
    timeout_secs: u64,
    connect_server: Option<(String, u16)>,
    run_abort: Arc<AbortHandle>,
    scheduler: Arc<ResourceScheduler>,
    archive: Arc<Mutex<RunArchiveWriter>>,
    event_tx: mpsc::UnboundedSender<RunEvent>,
    modrinth: ModrinthClient,
    account_store: AccountStore,
    on_instance_registered: Option<InstanceRegisteredCallback>,
) -> InstanceResult {
    let instance_dir = config
        .instances_dir()
        .join(run_id.to_string())
        .join(&inst.id);

    let inst_abort = AbortHandle::new();
    if let Some(ref cb) = on_instance_registered {
        cb(&inst.id, inst_abort.clone(), &instance_dir);
    }

    let run_abort_link = Arc::clone(&run_abort);
    let inst_abort_link = inst_abort.clone();
    let abort_link_task = tokio::spawn(async move {
        run_abort_link.cancelled().await;
        let _ = inst_abort_link.abort();
    });

    let ram_mb = inst
        .ram_mb
        .unwrap_or(config.global_limits.default_instance_ram_mb);

    let _permit = match scheduler.acquire(ram_mb, Some(&inst_abort)).await {
        Ok(p) => p,
        Err(e) => {
            abort_link_task.abort();
            let state = if inst_abort.is_aborted() || run_abort.is_aborted() {
                InstanceState::Aborted
            } else {
                InstanceState::Failed
            };
            emit_instance_state(&event_tx, run_id, &inst.id, state, Some(e.to_string()));
            return InstanceResult {
                instance_id: inst.id,
                state,
                exit_code: None,
                passed: false,
                reason: Some(e.to_string()),
                started_at: None,
                finished_at: Some(Utc::now()),
                log_path: None,
                screenshots: Vec::new(),
            };
        }
    };

    emit_instance_state(
        &event_tx,
        run_id,
        &inst.id,
        InstanceState::Preparing,
        Some("resolving runtime, loader, mods, and account".to_string()),
    );

    if let Err(e) = tokio::fs::create_dir_all(&instance_dir).await {
        abort_link_task.abort();
        return fail_result(&event_tx, run_id, &inst.id, format!("create dir failed: {e}"));
    }

    let java_ver = required_java_version(&inst.mc_version);
    let java = match ensure_java_runtime_with_progress(
        &config.cache_dir(),
        java_ver,
        Some(make_progress_callback(
            event_tx.clone(),
            run_id,
            Some(inst.id.clone()),
            "java",
        )),
    )
    .await
    {
        Ok(j) => j,
        Err(e) => {
            abort_link_task.abort();
            return fail_result(&event_tx, run_id, &inst.id, format!("Java error: {e}"));
        }
    };

    let profile = match resolve_and_install_loader_with_progress(
        config,
        &inst.mc_version,
        inst.loader,
        inst.loader_version.as_deref(),
        LoaderSide::Client,
        &instance_dir,
        Some(&java.executable),
        Some(make_progress_callback(
            event_tx.clone(),
            run_id,
            Some(inst.id.clone()),
            "loader",
        )),
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            abort_link_task.abort();
            return fail_result(&event_tx, run_id, &inst.id, format!("Loader error: {e}"));
        }
    };

    if let Err(e) = resolve_mods_with_progress(
        &modrinth,
        &inst.mods,
        &inst.mc_version,
        inst.loader,
        &config.cache_dir(),
        &instance_dir.join("mods"),
        Some(make_progress_callback(
            event_tx.clone(),
            run_id,
            Some(inst.id.clone()),
            "mods",
        )),
    )
    .await
    {
        abort_link_task.abort();
        return fail_result(&event_tx, run_id, &inst.id, format!("Mod error: {e}"));
    }

    let access_token = match resolve_account_token(&account_store, &inst.account).await {
        Ok(tok) => tok,
        Err(e) => {
            abort_link_task.abort();
            return fail_result(&event_tx, run_id, &inst.id, format!("Auth error: {e}"));
        }
    };

    let pid_tx = event_tx.clone();
    let pid_inst_id = inst.id.clone();

    let launch_cfg = ClientLaunchConfig {
        run_id,
        java_exe: java.executable,
        instance_dir: instance_dir.clone(),
        spec: inst.clone(),
        main_class: if profile.main_class.is_empty() {
            None
        } else {
            Some(profile.main_class)
        },
        classpath: profile.classpath,
        natives_dir: Some(profile.natives_dir),
        assets_dir: Some(profile.assets_dir),
        asset_index: Some(profile.asset_index),
        access_token,
        ram_mb,
        cpu_cores: Some(
            inst.cpu_cores
                .unwrap_or(config.global_limits.default_cpu_cores_per_instance),
        ),
        priority: config.global_limits.process_priority,
        connect_server,
        timeout_secs,
        extra_jvm_args: strip_unexpanded_mojang_args(&profile.jvm_args),
        extra_game_args: strip_unexpanded_mojang_args(&profile.game_args),
        on_pid: Some(Arc::new(move |pid| {
            let _ = pid_tx.send(RunEvent::InstanceStateChanged {
                run_id,
                instance_id: pid_inst_id.clone(),
                state: InstanceState::Running,
                message: Some(format!("pid={pid}")),
                timestamp: Utc::now(),
            });
        })),
    };

    let outcome = match run_client(launch_cfg, &inst_abort, Some(event_tx.clone())).await {
        Ok(o) => {
            abort_link_task.abort();
            o
        }
        Err(e) => {
            abort_link_task.abort();
            let state = if inst_abort.is_aborted() || run_abort.is_aborted() {
                InstanceState::Aborted
            } else {
                InstanceState::Failed
            };
            emit_instance_state(&event_tx, run_id, &inst.id, state, Some(e.to_string()));
            return InstanceResult {
                instance_id: inst.id,
                state,
                exit_code: None,
                passed: false,
                reason: Some(e.to_string()),
                started_at: None,
                finished_at: Some(Utc::now()),
                log_path: None,
                screenshots: Vec::new(),
            };
        }
    };

    // Archive stdout/stderr logs and screenshots.
    let mut archived_screenshots = Vec::new();
    {
        let mut writer = archive.lock().await;
        let _ = writer.write_instance_log(&inst.id, "stdout.log", &outcome.stdout_lines.join("\n"));
        let _ = writer.write_instance_log(&inst.id, "stderr.log", &outcome.stderr_lines.join("\n"));

        if let Ok(shots) = index_screenshots(&instance_dir) {
            for shot_path in shots {
                if let Ok(bytes) = std::fs::read(&shot_path) {
                    let fname = shot_path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("screenshot.png");
                    if let Ok(saved_path) = writer.save_screenshot(&inst.id, fname, &bytes) {
                        archived_screenshots.push(saved_path.clone());
                        let _ = event_tx.send(RunEvent::ScreenshotCaptured {
                            run_id,
                            instance_id: inst.id.clone(),
                            path: saved_path,
                            timestamp: Utc::now(),
                        });
                    }
                }
            }
        }
    }

    let eval_ctx = EvaluationContext {
        exit_code: outcome.exit_code,
        timed_out: outcome.timed_out,
        aborted: outcome.aborted,
        logs: outcome.combined_logs,
        instance_dir,
    };

    let eval = match evaluate_rule(&inst.test_rule, &eval_ctx) {
        Ok(res) => res,
        Err(e) => deming_runner::EvaluationResult {
            passed: false,
            reason: format!("rule evaluation error: {e}"),
        },
    };

    let _ = event_tx.send(RunEvent::TestEvaluated {
        run_id,
        instance_id: inst.id.clone(),
        passed: eval.passed,
        reason: eval.reason.clone(),
        timestamp: Utc::now(),
    });

    let final_state = if outcome.aborted {
        InstanceState::Aborted
    } else if outcome.timed_out {
        InstanceState::TimedOut
    } else if eval.passed {
        InstanceState::Passed
    } else {
        InstanceState::Failed
    };

    emit_instance_state(
        &event_tx,
        run_id,
        &inst.id,
        final_state,
        Some(eval.reason.clone()),
    );

    let log_rel = Path::new("instances").join(&inst.id).join("latest.log");
    InstanceResult {
        instance_id: inst.id,
        state: final_state,
        exit_code: outcome.exit_code,
        passed: eval.passed,
        reason: Some(eval.reason),
        started_at: Some(outcome.started_at),
        finished_at: Some(outcome.finished_at),
        log_path: Some(log_rel),
        screenshots: archived_screenshots,
    }
}
