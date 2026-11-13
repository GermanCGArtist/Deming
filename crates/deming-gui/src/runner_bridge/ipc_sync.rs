//! IPC server lifecycle, incoming IPC message handling, abort controls, and screenshot capture.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use deming_core::RunEvent;
use deming_ipc::{IpcMessage, IpcServer};
use deming_runner::capture_instance_screenshot;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use super::{emit_ui, BridgeMessage, GuiBridge, LocalRunControl};

impl GuiBridge {
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
        let local_run_ids = Arc::clone(&self.local_run_ids);

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
                                        handle_incoming_ipc(
                                            msg,
                                            &ui_tx,
                                            &egui_ctx,
                                            &local_runs,
                                            &local_run_ids,
                                        );
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
            runs.get(run_id).map(|ctrl| {
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
                let archive_dir = ctrl
                    .archive_dir
                    .lock()
                    .ok()
                    .map(|p| p.clone())
                    .unwrap_or_default();
                (pid, inst_dir, archive_dir)
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
}

fn is_local_run(local_run_ids: &Arc<Mutex<HashSet<String>>>, run_id: &str) -> bool {
    local_run_ids
        .lock()
        .map(|s| s.contains(run_id))
        .unwrap_or(false)
}

fn handle_incoming_ipc(
    msg: IpcMessage,
    ui_tx: &mpsc::UnboundedSender<BridgeMessage>,
    egui_ctx: &Arc<Mutex<Option<egui::Context>>>,
    local_runs: &Arc<Mutex<HashMap<String, LocalRunControl>>>,
    local_run_ids: &Arc<Mutex<HashSet<String>>>,
) {
    match msg {
        IpcMessage::RunStarted { run_id, name } => {
            if !is_local_run(local_run_ids, &run_id) {
                emit_ui(ui_tx, egui_ctx, BridgeMessage::IpcRunStarted { run_id, name });
            }
        }
        IpcMessage::PublishEvent { run_id, event } => {
            if !is_local_run(local_run_ids, &run_id) {
                emit_ui(
                    ui_tx,
                    egui_ctx,
                    BridgeMessage::RunEventReceived { run_id, event },
                );
            }
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
