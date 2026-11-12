//! Background Tokio runtime, IPC server bridge, local job executor, and MSA login tasks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use deming_archive::{apply_retention, RunArchiveWriter};
use deming_auth::{AccountStore, DeviceCodePrompt, MsaClient, MsaSession, StoredAccount};
use deming_core::{
    AccountRef, DemingConfig, InstanceResult, InstanceSpec, InstanceState, ModLoader, RunEvent,
    RunManifest, RunStatus, TestJobSpec,
};
use deming_fetch::{
    ensure_java_runtime, resolve_and_install_loader, resolve_mods, LaunchProfile, LoaderSide,
    ModrinthClient,
};
use deming_ipc::{IpcBroadcaster, IpcMessage, IpcServer};
use deming_runner::{
    capture_instance_screenshot, evaluate_rule, index_screenshots, run_client, ClientLaunchConfig,
    EvaluationContext, ResourceScheduler, ServerLaunchConfig, ServerProcess, AbortHandle,
};
use tokio::runtime::Runtime;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

/// Default Microsoft Azure public client ID for Minecraft OAuth Device-Code flow.
pub const DEFAULT_MSA_CLIENT_ID: &str = "00000000402b5328";

/// Resolves the path where `DemingConfig` is persisted on disk.
pub fn default_config_path() -> PathBuf {
    if let Ok(custom) = std::env::var("DEMING_CONFIG") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("deming")
        .join("config.json")
}

/// Messages sent from background Tokio tasks to the egui UI thread.
#[derive(Debug, Clone)]
pub enum BridgeMessage {
    IpcServerStarted {
        port: u16,
    },
    IpcServerError(String),
    IpcRunStarted {
        run_id: String,
        name: String,
    },
    LocalRunInitialized {
        run_id: Uuid,
        job_name: String,
        spec: TestJobSpec,
    },
    RunEventReceived {
        run_id: String,
        event: RunEvent,
    },
    ScreenshotResult {
        run_id: String,
        instance_id: String,
        path: Option<String>,
        error: Option<String>,
    },
    LocalRunFinished {
        run_id: Uuid,
        result: Result<RunManifest, String>,
    },
    MsaDeviceCodeReady(Result<DeviceCodePrompt, String>),
    MsaLoginFinished(Result<MsaSession, String>),
}

#[derive(Debug, Clone)]
struct LocalRunControl {
    run_abort: AbortHandle,
    instance_aborts: Arc<Mutex<HashMap<String, AbortHandle>>>,
    instance_dirs: Arc<Mutex<HashMap<String, PathBuf>>>,
    instance_pids: Arc<Mutex<HashMap<String, u32>>>,
    archive_dir: PathBuf,
}

/// Owns the multi-threaded Tokio runtime and coordinates IPC + local run execution.
pub struct GuiBridge {
    runtime: Runtime,
    ui_tx: mpsc::UnboundedSender<BridgeMessage>,
    ui_rx: mpsc::UnboundedReceiver<BridgeMessage>,
    ipc_broadcaster: Arc<Mutex<Option<IpcBroadcaster>>>,
    ipc_shutdown: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    local_runs: Arc<Mutex<HashMap<String, LocalRunControl>>>,
    msa_cancel: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    egui_ctx: Arc<Mutex<Option<egui::Context>>>,
}

impl GuiBridge {
    pub fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("deming-gui-tokio")
            .build()
            .expect("failed to build Tokio runtime for deming-gui");

        let (ui_tx, ui_rx) = mpsc::unbounded_channel();

        Self {
            runtime,
            ui_tx,
            ui_rx,
            ipc_broadcaster: Arc::new(Mutex::new(None)),
            ipc_shutdown: Arc::new(Mutex::new(None)),
            local_runs: Arc::new(Mutex::new(HashMap::new())),
            msa_cancel: Arc::new(Mutex::new(None)),
            egui_ctx: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_egui_ctx(&self, ctx: &egui::Context) {
        if let Ok(mut g) = self.egui_ctx.lock() {
            if g.is_none() {
                *g = Some(ctx.clone());
            }
        }
    }

    /// Drains all pending messages from background tasks without blocking.
    pub fn drain_messages(&mut self) -> Vec<BridgeMessage> {
        let mut out = Vec::new();
        while let Ok(msg) = self.ui_rx.try_recv() {
            out.push(msg);
        }
        out
    }

    /// Starts (or restarts) the `deming-ipc::IpcServer` bound to `workspace_dir`.
    pub fn start_ipc_server(&self, workspace_dir: PathBuf) {
        if let Ok(mut g) = self.ipc_shutdown.lock() {
            if let Some(tx) = g.take() {
                let _ = tx.send(());
            }
        }

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        if let Ok(mut g) = self.ipc_shutdown.lock() {
            *g = Some(shutdown_tx);
        }

        let ui_tx = self.ui_tx.clone();
        let egui_ctx = Arc::clone(&self.egui_ctx);
        let broadcaster_slot = Arc::clone(&self.ipc_broadcaster);
        let local_runs = Arc::clone(&self.local_runs);

        self.runtime.spawn(async move {
            match IpcServer::bind(&workspace_dir).await {
                Ok(mut server) => {
                    let port = server.port();
                    let broadcaster = server.broadcaster();
                    if let Ok(mut g) = broadcaster_slot.lock() {
                        *g = Some(broadcaster);
                    }
                    emit_ui(
                        &ui_tx,
                        &egui_ctx,
                        BridgeMessage::IpcServerStarted { port },
                    );

                    loop {
                        tokio::select! {
                            _ = &mut shutdown_rx => {
                                server.stop().await;
                                break;
                            }
                            msg_opt = server.recv() => {
                                match msg_opt {
                                    Some(msg) => {
                                        handle_incoming_ipc(msg, &ui_tx, &egui_ctx, &local_runs);
                                    }
                                    None => break,
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    emit_ui(
                        &ui_tx,
                        &egui_ctx,
                        BridgeMessage::IpcServerError(err.to_string()),
                    );
                }
            }
        });
    }

    /// Aborts every active local run and broadcasts [`IpcMessage::AbortAll`].
    pub fn abort_all(&self) {
        if let Ok(runs) = self.local_runs.lock() {
            for ctrl in runs.values() {
                let _ = ctrl.run_abort.abort();
                if let Ok(insts) = ctrl.instance_aborts.lock() {
                    for abort in insts.values() {
                        let _ = abort.abort();
                    }
                }
            }
        }
        self.broadcast_ipc(IpcMessage::AbortAll);
    }

    /// Kills a single instance (local via per-instance `AbortHandle`, or remote via `AbortRun`).
    pub fn kill_instance(&self, run_id: &str, instance_id: &str) {
        let mut handled_locally = false;
        if let Ok(runs) = self.local_runs.lock() {
            if let Some(ctrl) = runs.get(run_id) {
                handled_locally = true;
                if let Ok(insts) = ctrl.instance_aborts.lock() {
                    if let Some(abort) = insts.get(instance_id) {
                        let _ = abort.abort();
                    } else {
                        let _ = ctrl.run_abort.abort();
                    }
                }
            }
        }
        if !handled_locally {
            self.broadcast_ipc(IpcMessage::AbortRun {
                run_id: run_id.to_string(),
            });
        }
    }

    /// Captures a screenshot for an active instance (locally by PID if known, and via IPC).
    pub fn request_screenshot(&self, run_id: &str, instance_id: &str) {
        let local_target = self.local_runs.lock().ok().and_then(|runs| {
            runs.get(run_id).and_then(|ctrl| {
                let pid = ctrl
                    .instance_pids
                    .lock()
                    .ok()
                    .and_then(|m| m.get(instance_id).copied());
                let inst_dir = ctrl
                    .instance_dirs
                    .lock()
                    .ok()
                    .and_then(|m| m.get(instance_id).cloned());
                Some((pid, inst_dir, ctrl.archive_dir.clone()))
            })
        });

        if let Some((pid_opt, inst_dir_opt, archive_dir)) = local_target {
            if let (Some(pid), Some(inst_dir)) = (pid_opt, inst_dir_opt) {
                let ui_tx = self.ui_tx.clone();
                let egui_ctx = Arc::clone(&self.egui_ctx);
                let run_id_owned = run_id.to_string();
                let inst_id_owned = instance_id.to_string();
                let inst_id_blocking = inst_id_owned.clone();
                let broadcaster_slot = Arc::clone(&self.ipc_broadcaster);

                self.runtime.spawn(async move {
                    let res = tokio::task::spawn_blocking(move || {
                        let captured =
                            capture_instance_screenshot(pid, &inst_dir, Some("manual"))?;
                        // Also mirror into the run archive screenshots folder.
                        let archive_shots = archive_dir
                            .join("instances")
                            .join(&inst_id_blocking)
                            .join("screenshots");
                        std::fs::create_dir_all(&archive_shots)?;
                        if let Some(fname) = captured.file_name() {
                            let dest = archive_shots.join(fname);
                            if dest != captured {
                                let _ = std::fs::copy(&captured, &dest);
                            }
                            return Ok::<(PathBuf, String), deming_core::DemingError>((
                                dest,
                                inst_id_blocking,
                            ));
                        }
                        Ok((captured, inst_id_blocking))
                    })
                    .await;

                    match res {
                        Ok(Ok((path, inst_id))) => {
                            if let Ok(uuid) = Uuid::parse_str(&run_id_owned) {
                                let ev = RunEvent::ScreenshotCaptured {
                                    run_id: uuid,
                                    instance_id: inst_id.clone(),
                                    path: path.clone(),
                                    timestamp: Utc::now(),
                                };
                                emit_ui(
                                    &ui_tx,
                                    &egui_ctx,
                                    BridgeMessage::RunEventReceived {
                                        run_id: run_id_owned.clone(),
                                        event: ev.clone(),
                                    },
                                );
                                if let Some(b) =
                                    broadcaster_slot.lock().ok().and_then(|g| g.clone())
                                {
                                    let _ = b
                                        .broadcast(&IpcMessage::PublishEvent {
                                            run_id: run_id_owned.clone(),
                                            event: ev,
                                        })
                                        .await;
                                }
                            }
                            emit_ui(
                                &ui_tx,
                                &egui_ctx,
                                BridgeMessage::ScreenshotResult {
                                    run_id: run_id_owned,
                                    instance_id: inst_id,
                                    path: Some(path.to_string_lossy().to_string()),
                                    error: None,
                                },
                            );
                        }
                        Ok(Err(e)) => {
                            emit_ui(
                                &ui_tx,
                                &egui_ctx,
                                BridgeMessage::ScreenshotResult {
                                    run_id: run_id_owned,
                                    instance_id: inst_id_owned,
                                    path: None,
                                    error: Some(e.to_string()),
                                },
                            );
                        }
                        Err(e) => {
                            emit_ui(
                                &ui_tx,
                                &egui_ctx,
                                BridgeMessage::ScreenshotResult {
                                    run_id: run_id_owned,
                                    instance_id: inst_id_owned,
                                    path: None,
                                    error: Some(format!("screenshot task failed: {e}")),
                                },
                            );
                        }
                    }
                });
                return;
            }
        }

        self.broadcast_ipc(IpcMessage::RequestScreenshot {
            run_id: run_id.to_string(),
            instance_id: instance_id.to_string(),
        });
    }

    fn broadcast_ipc(&self, msg: IpcMessage) {
        let broadcaster = self.ipc_broadcaster.lock().ok().and_then(|g| g.clone());
        if let Some(b) = broadcaster {
            self.runtime.spawn(async move {
                let _ = b.broadcast(&msg).await;
            });
        }
    }

    /// Starts a Microsoft OAuth Device-Code flow and polls in background until authorized or cancelled.
    pub fn start_msa_login(&self, config: DemingConfig, client_id: String) {
        self.cancel_msa_login();
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
        if let Ok(mut g) = self.msa_cancel.lock() {
            *g = Some(cancel_tx);
        }

        let ui_tx = self.ui_tx.clone();
        let egui_ctx = Arc::clone(&self.egui_ctx);

        self.runtime.spawn(async move {
            let client = MsaClient::new(client_id);
            let prompt = match client.start_device_code().await {
                Ok(p) => {
                    emit_ui(&ui_tx, &egui_ctx, BridgeMessage::MsaDeviceCodeReady(Ok(p.clone())));
                    p
                }
                Err(e) => {
                    emit_ui(
                        &ui_tx,
                        &egui_ctx,
                        BridgeMessage::MsaDeviceCodeReady(Err(e.to_string())),
                    );
                    return;
                }
            };

            let poll_fut = client.poll_and_complete_login(&prompt);
            tokio::select! {
                _ = &mut cancel_rx => {
                    emit_ui(
                        &ui_tx,
                        &egui_ctx,
                        BridgeMessage::MsaLoginFinished(Err("Microsoft login cancelled".to_string())),
                    );
                }
                res = poll_fut => {
                    match res {
                        Ok(session) => {
                            let store = AccountStore::from_config(&config);
                            if let Err(e) = store.upsert(StoredAccount::from_msa(&session)) {
                                emit_ui(
                                    &ui_tx,
                                    &egui_ctx,
                                    BridgeMessage::MsaLoginFinished(Err(format!("Login succeeded but failed to save account: {e}"))),
                                );
                            } else {
                                emit_ui(&ui_tx, &egui_ctx, BridgeMessage::MsaLoginFinished(Ok(session)));
                            }
                        }
                        Err(e) => {
                            emit_ui(&ui_tx, &egui_ctx, BridgeMessage::MsaLoginFinished(Err(e.to_string())));
                        }
                    }
                }
            }
        });
    }

    pub fn cancel_msa_login(&self) {
        if let Ok(mut g) = self.msa_cancel.lock() {
            if let Some(tx) = g.take() {
                let _ = tx.send(());
            }
        }
    }

    /// Launches a [`TestJobSpec`] asynchronously on the Tokio runtime and streams events to the Dashboard & archive.
    pub fn launch_job(&self, config: DemingConfig, spec: TestJobSpec) {
        let run_id = Uuid::new_v4();
        let run_id_str = run_id.to_string();
        let run_abort = AbortHandle::new();

        let ui_tx = self.ui_tx.clone();
        let egui_ctx = Arc::clone(&self.egui_ctx);
        let broadcaster_slot = Arc::clone(&self.ipc_broadcaster);
        let local_runs = Arc::clone(&self.local_runs);

        self.runtime.spawn(async move {
            let result = execute_local_job(
                config,
                spec,
                run_id,
                run_abort,
                ui_tx.clone(),
                egui_ctx.clone(),
                broadcaster_slot,
                Arc::clone(&local_runs),
            )
            .await;

            if let Ok(mut g) = local_runs.lock() {
                g.remove(&run_id_str);
            }

            emit_ui(
                &ui_tx,
                &egui_ctx,
                BridgeMessage::LocalRunFinished { run_id, result },
            );
        });
    }
}

impl Default for GuiBridge {
    fn default() -> Self {
        Self::new()
    }
}

fn emit_ui(
    tx: &mpsc::UnboundedSender<BridgeMessage>,
    egui_ctx: &Arc<Mutex<Option<egui::Context>>>,
    msg: BridgeMessage,
) {
    let _ = tx.send(msg);
    if let Ok(g) = egui_ctx.lock() {
        if let Some(ctx) = g.as_ref() {
            ctx.request_repaint();
        }
    }
}

fn handle_incoming_ipc(
    msg: IpcMessage,
    ui_tx: &mpsc::UnboundedSender<BridgeMessage>,
    egui_ctx: &Arc<Mutex<Option<egui::Context>>>,
    local_runs: &Arc<Mutex<HashMap<String, LocalRunControl>>>,
) {
    match msg {
        IpcMessage::RunStarted { run_id, name } => {
            emit_ui(ui_tx, egui_ctx, BridgeMessage::IpcRunStarted { run_id, name });
        }
        IpcMessage::PublishEvent { run_id, event } => {
            emit_ui(
                ui_tx,
                egui_ctx,
                BridgeMessage::RunEventReceived { run_id, event },
            );
        }
        IpcMessage::ScreenshotResult {
            run_id,
            instance_id,
            path,
            error,
        } => {
            emit_ui(
                ui_tx,
                egui_ctx,
                BridgeMessage::ScreenshotResult {
                    run_id,
                    instance_id,
                    path,
                    error,
                },
            );
        }
        IpcMessage::AbortRun { run_id } => {
            if let Ok(runs) = local_runs.lock() {
                if let Some(ctrl) = runs.get(&run_id) {
                    let _ = ctrl.run_abort.abort();
                }
            }
        }
        IpcMessage::AbortAll => {
            if let Ok(runs) = local_runs.lock() {
                for ctrl in runs.values() {
                    let _ = ctrl.run_abort.abort();
                }
            }
        }
        IpcMessage::RequestScreenshot { .. } | IpcMessage::Ping | IpcMessage::Pong => {}
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_local_job(
    config: DemingConfig,
    spec: TestJobSpec,
    run_id: Uuid,
    run_abort: AbortHandle,
    ui_tx: mpsc::UnboundedSender<BridgeMessage>,
    egui_ctx: Arc<Mutex<Option<egui::Context>>>,
    broadcaster_slot: Arc<Mutex<Option<IpcBroadcaster>>>,
    local_runs: Arc<Mutex<HashMap<String, LocalRunControl>>>,
) -> Result<RunManifest, String> {
    let run_id_str = run_id.to_string();
    let created_at = Utc::now();

    let writer = RunArchiveWriter::with_timestamp(config.runs_dir(), run_id, created_at)
        .map_err(|e| format!("failed to create run archive: {e}"))?;
    let archive_dir = writer.run_dir().to_path_buf();

    let instance_aborts = Arc::new(Mutex::new(HashMap::new()));
    let instance_dirs = Arc::new(Mutex::new(HashMap::new()));
    let instance_pids = Arc::new(Mutex::new(HashMap::new()));

    if let Ok(mut g) = local_runs.lock() {
        g.insert(
            run_id_str.clone(),
            LocalRunControl {
                run_abort: run_abort.clone(),
                instance_aborts: Arc::clone(&instance_aborts),
                instance_dirs: Arc::clone(&instance_dirs),
                instance_pids: Arc::clone(&instance_pids),
                archive_dir: archive_dir.clone(),
            },
        );
    }

    let initial_instances: Vec<InstanceResult> = spec
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
        job_name: spec.name.clone(),
        status: RunStatus::Running,
        created_at,
        started_at: Some(Utc::now()),
        finished_at: None,
        spec: spec.clone(),
        instances: initial_instances,
        server_log_path: None,
        bundle_path: None,
    };

    writer
        .write_manifest(&manifest)
        .map_err(|e| format!("failed to write initial manifest: {e}"))?;

    emit_ui(
        &ui_tx,
        &egui_ctx,
        BridgeMessage::LocalRunInitialized {
            run_id,
            job_name: spec.name.clone(),
            spec: spec.clone(),
        },
    );

    let writer = Arc::new(Mutex::new(writer));

    let emit_event = {
        let ui_tx = ui_tx.clone();
        let egui_ctx = Arc::clone(&egui_ctx);
        let broadcaster_slot = Arc::clone(&broadcaster_slot);
        let writer = Arc::clone(&writer);
        let run_id_str = run_id_str.clone();
        let instance_pids = Arc::clone(&instance_pids);

        move |event: RunEvent| {
            // Track live PID from InstanceStateChanged("running (pid N)")
            if let RunEvent::InstanceStateChanged {
                ref instance_id,
                state: InstanceState::Running,
                message: Some(ref msg),
                ..
            } = event
            {
                if let Some(pid) = parse_pid_from_message(msg) {
                    if let Ok(mut pids) = instance_pids.lock() {
                        pids.insert(instance_id.clone(), pid);
                    }
                }
            }

            if let Ok(mut w) = writer.lock() {
                let _ = w.append_event(&event);
                if let RunEvent::LogLine {
                    ref source,
                    ref line,
                    ..
                } = event
                {
                    if source == "server" {
                        let _ = w.append_server_line(line);
                    } else {
                        let _ = w.append_instance_stdout(source, line);
                        let _ = w.append_instance_latest_log(source, line);
                    }
                }
            }

            emit_ui(
                &ui_tx,
                &egui_ctx,
                BridgeMessage::RunEventReceived {
                    run_id: run_id_str.clone(),
                    event: event.clone(),
                },
            );

            let broadcaster = broadcaster_slot.lock().ok().and_then(|g| g.clone());
            if let Some(b) = broadcaster {
                let r_id = run_id_str.clone();
                tokio::spawn(async move {
                    let _ = b
                        .broadcast(&IpcMessage::PublishEvent {
                            run_id: r_id,
                            event,
                        })
                        .await;
                });
            }
        }
    };

    if let Some(b) = broadcaster_slot.lock().ok().and_then(|g| g.clone()) {
        let _ = b
            .broadcast(&IpcMessage::RunStarted {
                run_id: run_id_str.clone(),
                name: spec.name.clone(),
            })
            .await;
    }

    emit_event(RunEvent::RunStarted {
        run_id,
        job_name: spec.name.clone(),
        timestamp: Utc::now(),
    });

    // Event forwarder channel for runner tasks (`run_client` / `ServerProcess::start`)
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<RunEvent>();
    let emit_event_clone = emit_event.clone();
    let forwarder_task = tokio::spawn(async move {
        while let Some(ev) = ev_rx.recv().await {
            emit_event_clone(ev);
        }
    });

    // Optional dedicated server startup
    let mut running_server: Option<ServerProcess> = None;
    let mut server_connect_port: Option<u16> = None;

    if let Some(ref srv_spec) = spec.server {
        let server_dir = config
            .servers_dir()
            .join(format!("{}_{}", spec.name.replace(' ', "_"), run_id));
        let cache_dir = config.cache_dir();

        let java_rt = match ensure_java_runtime(
            &cache_dir,
            deming_fetch::required_java_version(&srv_spec.mc_version),
        )
        .await
        {
            Ok(rt) => rt,
            Err(e) => {
                let err_msg = format!("Server Java provisioning failed: {e}");
                let _ = ev_tx.send(RunEvent::LogLine {
                    run_id,
                    source: "server".to_string(),
                    line: err_msg.clone(),
                    timestamp: Utc::now(),
                });
                return finalize_failed_run(
                    manifest,
                    &writer,
                    emit_event,
                    ev_tx,
                    forwarder_task,
                    &config,
                    RunStatus::Failed,
                    err_msg,
                )
                .await;
            }
        };

        let profile = match resolve_and_install_loader(
            &config,
            &srv_spec.mc_version,
            srv_spec.loader,
            srv_spec.loader_version.as_deref(),
            LoaderSide::Server,
            &server_dir,
            Some(&java_rt.executable),
        )
        .await
        {
            Ok(p) => p,
            Err(e) => {
                let err_msg = format!("Server loader installation failed: {e}");
                let _ = ev_tx.send(RunEvent::LogLine {
                    run_id,
                    source: "server".to_string(),
                    line: err_msg.clone(),
                    timestamp: Utc::now(),
                });
                return finalize_failed_run(
                    manifest,
                    &writer,
                    emit_event,
                    ev_tx,
                    forwarder_task,
                    &config,
                    RunStatus::Failed,
                    err_msg,
                )
                .await;
            }
        };

        if !srv_spec.mods.is_empty() {
            let modrinth = ModrinthClient::new().map_err(|e| e.to_string())?;
            if let Err(e) = resolve_mods(
                &modrinth,
                &srv_spec.mods,
                &srv_spec.mc_version,
                srv_spec.loader,
                &cache_dir,
                &server_dir.join("mods"),
            )
            .await
            {
                let err_msg = format!("Server mod resolution failed: {e}");
                let _ = ev_tx.send(RunEvent::LogLine {
                    run_id,
                    source: "server".to_string(),
                    line: err_msg.clone(),
                    timestamp: Utc::now(),
                });
                return finalize_failed_run(
                    manifest,
                    &writer,
                    emit_event,
                    ev_tx,
                    forwarder_task,
                    &config,
                    RunStatus::Failed,
                    err_msg,
                )
                .await;
            }
        }

        let ram_mb = srv_spec
            .ram_mb
            .unwrap_or(config.global_limits.default_instance_ram_mb);
        let mut jvm_args = profile.jvm_args.clone();
        let mut program_args = profile.game_args.clone();
        if profile.server_jar.is_none() && !profile.main_class.is_empty() {
            // Quilt server profile uses main_class + classpath
            let sep = if cfg!(windows) { ";" } else { ":" };
            let cp = profile
                .classpath
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(sep);
            jvm_args.push(format!("-Xmx{ram_mb}M"));
            jvm_args.push("-cp".to_string());
            jvm_args.push(cp);
            jvm_args.push(profile.main_class.clone());
            if program_args.is_empty() {
                program_args.push("nogui".to_string());
            }
        } else if profile.server_jar.is_none() && !jvm_args.is_empty() {
            // Forge/NeoForge server uses @win_args.txt
            jvm_args.insert(0, format!("-Xmx{ram_mb}M"));
        }

        let srv_cfg = ServerLaunchConfig {
            run_id,
            java_exe: java_rt.executable,
            server_dir: server_dir.clone(),
            server_jar: profile.server_jar,
            jvm_args,
            program_args,
            spec: srv_spec.clone(),
            ram_mb,
            cpu_cores: Some(config.global_limits.default_cpu_cores_per_instance),
            priority: config.global_limits.process_priority,
            startup_timeout_secs: spec.timeout_secs.max(60),
        };

        match ServerProcess::start(srv_cfg, &run_abort, Some(ev_tx.clone())).await {
            Ok(srv) => {
                server_connect_port = Some(srv.port());
                running_server = Some(srv);
                manifest.server_log_path = Some(archive_dir.join("server").join("latest.log"));
            }
            Err(e) => {
                let status = if run_abort.is_aborted() {
                    RunStatus::Aborted
                } else {
                    RunStatus::Failed
                };
                return finalize_failed_run(
                    manifest,
                    &writer,
                    emit_event,
                    ev_tx,
                    forwarder_task,
                    &config,
                    status,
                    format!("Server failed to start: {e}"),
                )
                .await;
            }
        }
    }

    // Launch all client instances concurrently under ResourceScheduler
    let scheduler = ResourceScheduler::from_limits(&config.global_limits);
    let connect_server = spec
        .server
        .as_ref()
        .filter(|s| s.auto_connect_clients)
        .and(server_connect_port)
        .map(|port| ("127.0.0.1".to_string(), port));

    let mut join_handles = Vec::with_capacity(spec.instances.len());

    for inst_spec in spec.instances.clone() {
        let inst_abort = AbortHandle::new();
        if let Ok(mut g) = instance_aborts.lock() {
            g.insert(inst_spec.id.clone(), inst_abort.clone());
        }

        let inst_dir = config
            .instances_dir()
            .join(format!("{}_{}", inst_spec.id, run_id));
        if let Ok(mut g) = instance_dirs.lock() {
            g.insert(inst_spec.id.clone(), inst_dir.clone());
        }

        let config_c = config.clone();
        let scheduler_c = scheduler.clone();
        let run_abort_c = run_abort.clone();
        let ev_tx_c = ev_tx.clone();
        let connect_c = connect_server.clone();
        let timeout_secs = spec.timeout_secs;
        let writer_c = Arc::clone(&writer);
        let archive_dir_c = archive_dir.clone();

        join_handles.push(tokio::spawn(async move {
            run_single_instance(
                run_id,
                config_c,
                inst_spec,
                inst_dir,
                archive_dir_c,
                scheduler_c,
                run_abort_c,
                inst_abort,
                ev_tx_c,
                connect_c,
                timeout_secs,
                writer_c,
            )
            .await
        }));
    }

    let mut results = Vec::with_capacity(join_handles.len());
    for handle in join_handles {
        match handle.await {
            Ok(res) => results.push(res),
            Err(e) => results.push(InstanceResult {
                instance_id: "unknown".to_string(),
                state: InstanceState::Failed,
                exit_code: None,
                passed: false,
                reason: Some(format!("instance task panicked: {e}")),
                started_at: None,
                finished_at: Some(Utc::now()),
                log_path: None,
                screenshots: Vec::new(),
            }),
        }
    }

    if let Some(mut srv) = running_server {
        let _ = srv.shutdown(Duration::from_secs(10)).await;
    }

    manifest.instances = results;
    manifest.finished_at = Some(Utc::now());
    manifest.status = if run_abort.is_aborted()
        || manifest
            .instances
            .iter()
            .any(|i| i.state == InstanceState::Aborted)
    {
        RunStatus::Aborted
    } else if manifest.instances.iter().all(|i| i.passed) {
        RunStatus::Passed
    } else {
        RunStatus::Failed
    };

    drop(ev_tx);
    let _ = forwarder_task.await;

    if let Ok(w) = writer.lock() {
        let _ = w.write_manifest(&manifest);
    }

    emit_event(RunEvent::RunFinished {
        run_id,
        status: manifest.status,
        timestamp: Utc::now(),
    });

    if config.retention.auto_prune {
        let _ = apply_retention(config.runs_dir(), &config.retention);
    }

    Ok(manifest)
}

#[allow(clippy::too_many_arguments)]
async fn finalize_failed_run(
    mut manifest: RunManifest,
    writer: &Arc<Mutex<RunArchiveWriter>>,
    emit_event: impl Fn(RunEvent),
    ev_tx: mpsc::UnboundedSender<RunEvent>,
    forwarder_task: tokio::task::JoinHandle<()>,
    config: &DemingConfig,
    status: RunStatus,
    reason: String,
) -> Result<RunManifest, String> {
    manifest.status = status;
    manifest.finished_at = Some(Utc::now());
    for inst in &mut manifest.instances {
        inst.state = if status == RunStatus::Aborted {
            InstanceState::Aborted
        } else {
            InstanceState::Failed
        };
        inst.passed = false;
        inst.reason = Some(reason.clone());
        inst.finished_at = Some(Utc::now());
    }
    drop(ev_tx);
    let _ = forwarder_task.await;
    if let Ok(w) = writer.lock() {
        let _ = w.write_manifest(&manifest);
    }
    emit_event(RunEvent::RunFinished {
        run_id: manifest.run_id,
        status,
        timestamp: Utc::now(),
    });
    if config.retention.auto_prune {
        let _ = apply_retention(config.runs_dir(), &config.retention);
    }
    Err(reason)
}

#[allow(clippy::too_many_arguments)]
async fn run_single_instance(
    run_id: Uuid,
    config: DemingConfig,
    spec: InstanceSpec,
    inst_dir: PathBuf,
    archive_dir: PathBuf,
    scheduler: ResourceScheduler,
    run_abort: AbortHandle,
    inst_abort: AbortHandle,
    ev_tx: mpsc::UnboundedSender<RunEvent>,
    connect_server: Option<(String, u16)>,
    timeout_secs: u64,
    writer: Arc<Mutex<RunArchiveWriter>>,
) -> InstanceResult {
    let ram_mb = spec
        .ram_mb
        .unwrap_or(config.global_limits.default_instance_ram_mb);
    let cpu_cores = spec
        .cpu_cores
        .or(Some(config.global_limits.default_cpu_cores_per_instance));

    // Link run_abort -> inst_abort via a lightweight watcher task
    let inst_abort_link = inst_abort.clone();
    let run_abort_link = run_abort.clone();
    let link_task = tokio::spawn(async move {
        run_abort_link.cancelled().await;
        let _ = inst_abort_link.abort();
    });

    let _permit = match scheduler.acquire(ram_mb, Some(&inst_abort)).await {
        Ok(p) => p,
        Err(e) => {
            link_task.abort();
            let state = if inst_abort.is_aborted() || run_abort.is_aborted() {
                InstanceState::Aborted
            } else {
                InstanceState::Failed
            };
            let _ = ev_tx.send(RunEvent::InstanceStateChanged {
                run_id,
                instance_id: spec.id.clone(),
                state,
                message: Some(e.to_string()),
                timestamp: Utc::now(),
            });
            return InstanceResult {
                instance_id: spec.id,
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

    let _ = ev_tx.send(RunEvent::InstanceStateChanged {
        run_id,
        instance_id: spec.id.clone(),
        state: InstanceState::Preparing,
        message: Some("provisioning Java, loader, and mods".to_string()),
        timestamp: Utc::now(),
    });

    let cache_dir = config.cache_dir();
    let java_rt = match ensure_java_runtime(
        &cache_dir,
        deming_fetch::required_java_version(&spec.mc_version),
    )
    .await
    {
        Ok(rt) => rt,
        Err(e) => {
            link_task.abort();
            return fail_instance(&ev_tx, run_id, &spec.id, &inst_abort, format!("Java error: {e}"));
        }
    };

    let profile = match resolve_and_install_loader(
        &config,
        &spec.mc_version,
        spec.loader,
        spec.loader_version.as_deref(),
        LoaderSide::Client,
        &inst_dir,
        Some(&java_rt.executable),
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            link_task.abort();
            return fail_instance(
                &ev_tx,
                run_id,
                &spec.id,
                &inst_abort,
                format!("Loader install failed: {e}"),
            );
        }
    };

    if !spec.mods.is_empty() {
        match ModrinthClient::new() {
            Ok(client) => {
                if let Err(e) = resolve_mods(
                    &client,
                    &spec.mods,
                    &spec.mc_version,
                    spec.loader,
                    &cache_dir,
                    &inst_dir.join("mods"),
                )
                .await
                {
                    link_task.abort();
                    return fail_instance(
                        &ev_tx,
                        run_id,
                        &spec.id,
                        &inst_abort,
                        format!("Mod resolution failed: {e}"),
                    );
                }
            }
            Err(e) => {
                link_task.abort();
                return fail_instance(
                    &ev_tx,
                    run_id,
                    &spec.id,
                    &inst_abort,
                    format!("HTTP client error: {e}"),
                );
            }
        }
    }

    let access_token = match resolve_access_token(&config, &spec.account).await {
        Ok(tok) => tok,
        Err(e) => {
            link_task.abort();
            return fail_instance(
                &ev_tx,
                run_id,
                &spec.id,
                &inst_abort,
                format!("Account auth failed: {e}"),
            );
        }
    };

    let (extra_jvm_args, extra_game_args) =
        sanitize_launch_profile_args(&profile, &inst_dir, spec.loader);

    let launch_cfg = ClientLaunchConfig {
        run_id,
        java_exe: java_rt.executable,
        instance_dir: inst_dir.clone(),
        spec: spec.clone(),
        main_class: Some(profile.main_class),
        classpath: profile.classpath,
        natives_dir: Some(profile.natives_dir),
        assets_dir: Some(profile.assets_dir),
        asset_index: Some(profile.asset_index),
        access_token,
        ram_mb,
        cpu_cores,
        priority: config.global_limits.process_priority,
        connect_server,
        timeout_secs,
        extra_jvm_args,
        extra_game_args,
    };

    let outcome = match run_client(launch_cfg, &inst_abort, Some(ev_tx.clone())).await {
        Ok(o) => o,
        Err(e) => {
            link_task.abort();
            return fail_instance(&ev_tx, run_id, &spec.id, &inst_abort, e.to_string());
        }
    };

    link_task.abort();

    let eval_ctx = EvaluationContext {
        exit_code: outcome.exit_code,
        timed_out: outcome.timed_out,
        aborted: outcome.aborted,
        logs: outcome.combined_logs.clone(),
        instance_dir: inst_dir.clone(),
    };

    let eval = evaluate_rule(&spec.test_rule, &eval_ctx).unwrap_or_else(|e| {
        deming_runner::EvaluationResult {
            passed: false,
            reason: format!("Rule evaluation error: {e}"),
        }
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

    let _ = ev_tx.send(RunEvent::TestEvaluated {
        run_id,
        instance_id: spec.id.clone(),
        passed: eval.passed,
        reason: eval.reason.clone(),
        timestamp: Utc::now(),
    });
    let _ = ev_tx.send(RunEvent::InstanceStateChanged {
        run_id,
        instance_id: spec.id.clone(),
        state: final_state,
        message: Some(eval.reason.clone()),
        timestamp: Utc::now(),
    });

    // Archive screenshots from instance_dir/screenshots into archive_dir/instances/<id>/screenshots
    let mut archived_shots = Vec::new();
    if let Ok(shots) = index_screenshots(&inst_dir) {
        for shot in shots {
            if let Some(fname) = shot.file_name().and_then(|s| s.to_str()) {
                if let Ok(bytes) = std::fs::read(&shot) {
                    if let Ok(mut w) = writer.lock() {
                        if let Ok(saved) = w.save_screenshot(&spec.id, fname, &bytes) {
                            archived_shots.push(saved);
                        }
                    }
                }
            }
        }
    }
    // Include any screenshots saved directly into archive screenshots dir (e.g. manual captures)
    let archive_shots_dir = archive_dir
        .join("instances")
        .join(&spec.id)
        .join("screenshots");
    if let Ok(entries) = std::fs::read_dir(&archive_shots_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() && !archived_shots.contains(&p) {
                archived_shots.push(p);
            }
        }
    }

    InstanceResult {
        instance_id: spec.id,
        state: final_state,
        exit_code: outcome.exit_code,
        passed: eval.passed,
        reason: Some(eval.reason),
        started_at: Some(outcome.started_at),
        finished_at: Some(outcome.finished_at),
        log_path: Some(
            archive_dir
                .join("instances")
                .join(&outcome.instance_id)
                .join("latest.log"),
        ),
        screenshots: archived_shots,
    }
}

fn fail_instance(
    ev_tx: &mpsc::UnboundedSender<RunEvent>,
    run_id: Uuid,
    instance_id: &str,
    inst_abort: &AbortHandle,
    msg: String,
) -> InstanceResult {
    let state = if inst_abort.is_aborted() {
        InstanceState::Aborted
    } else {
        InstanceState::Failed
    };
    let _ = ev_tx.send(RunEvent::InstanceStateChanged {
        run_id,
        instance_id: instance_id.to_string(),
        state,
        message: Some(msg.clone()),
        timestamp: Utc::now(),
    });
    InstanceResult {
        instance_id: instance_id.to_string(),
        state,
        exit_code: None,
        passed: false,
        reason: Some(msg),
        started_at: None,
        finished_at: Some(Utc::now()),
        log_path: None,
        screenshots: Vec::new(),
    }
}

async fn resolve_access_token(
    config: &DemingConfig,
    account: &AccountRef,
) -> Result<Option<String>, String> {
    match account {
        AccountRef::Offline { .. } => Ok(None),
        AccountRef::Microsoft { .. } => {
            let store = AccountStore::from_config(config);
            let stored = store
                .find_by_ref(account)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "Microsoft account not found in AccountStore".to_string())?;
            match stored {
                StoredAccount::Microsoft {
                    mc_access_token,
                    msa_refresh_token,
                    expires_at,
                    ..
                } => {
                    if Utc::now() < expires_at {
                        Ok(Some(mc_access_token))
                    } else {
                        let client = MsaClient::new(DEFAULT_MSA_CLIENT_ID);
                        let refreshed = client
                            .refresh_token(&msa_refresh_token)
                            .await
                            .map_err(|e| e.to_string())?;
                        let _ = store.upsert(StoredAccount::from_msa(&refreshed));
                        Ok(Some(refreshed.mc_access_token))
                    }
                }
                StoredAccount::Offline { .. } => Ok(None),
            }
        }
    }
}

fn sanitize_launch_profile_args(
    profile: &LaunchProfile,
    inst_dir: &Path,
    loader: ModLoader,
) -> (Vec<String>, Vec<String>) {
    let sep = if cfg!(windows) { ";" } else { ":" };
    let lib_dir = inst_dir.join("libraries").to_string_lossy().into_owned();
    let natives = profile.natives_dir.to_string_lossy().into_owned();

    let replace_vars = |s: &str| -> String {
        s.replace("${natives_directory}", &natives)
            .replace("${launcher_name}", "Deming")
            .replace("${launcher_version}", "0.1.0")
            .replace("${classpath_separator}", sep)
            .replace("${library_directory}", &lib_dir)
    };

    let mut jvm_out = Vec::new();
    let mut skip_next = false;
    for arg in &profile.jvm_args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "-cp" || arg == "-classpath" {
            skip_next = true;
            continue;
        }
        if arg == "${classpath}" || arg.starts_with("-Djava.library.path=") {
            continue;
        }
        jvm_out.push(replace_vars(arg));
    }

    let mut game_out = Vec::new();
    if matches!(loader, ModLoader::Forge | ModLoader::NeoForge) {
        // Keep only non-templated FML / loader args that ClientLaunchConfig does not already set
        let mut skip_next_val = false;
        for arg in &profile.game_args {
            if skip_next_val {
                skip_next_val = false;
                continue;
            }
            if matches!(
                arg.as_str(),
                "--username"
                    | "--version"
                    | "--gameDir"
                    | "--assetsDir"
                    | "--assetIndex"
                    | "--uuid"
                    | "--accessToken"
                    | "--clientId"
                    | "--xuid"
                    | "--userType"
                    | "--versionType"
            ) {
                skip_next_val = true;
                continue;
            }
            if !arg.contains("${") {
                game_out.push(arg.clone());
            }
        }
    }

    (jvm_out, game_out)
}

/// Parses `"running (pid 12345)"` emitted by `deming-runner::client`.
pub fn parse_pid_from_message(msg: &str) -> Option<u32> {
    let marker = "(pid ";
    let start = msg.find(marker)? + marker.len();
    let rest = &msg[start..];
    let end = rest.find(')')?;
    rest[..end].trim().parse::<u32>().ok()
}
