//! Local test job execution delegated to `deming-engine`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use deming_core::{DemingConfig, InstanceState, RunEvent, TestJobSpec};
use deming_engine::{execute_job_with_ipc_options, JobExecutionOptions};
use deming_runner::AbortHandle;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::{emit_ui, BridgeMessage, GuiBridge, LocalRunControl};

impl GuiBridge {
    /// Launches a [`TestJobSpec`] asynchronously on the Tokio runtime via `deming_engine`
    /// and streams events to the Dashboard & archive.
    pub fn launch_job(&self, config: DemingConfig, spec: TestJobSpec) {
        let run_id = Uuid::new_v4();
        let run_id_str = run_id.to_string();
        let run_abort = Arc::new(AbortHandle::new());

        let ui_tx = self.ui_tx.clone();
        let egui_ctx = Arc::clone(&self.egui_ctx);
        let local_runs = Arc::clone(&self.local_runs);
        let local_run_ids = Arc::clone(&self.local_run_ids);

        if let Ok(mut ids) = local_run_ids.lock() {
            ids.insert(run_id_str.clone());
        }

        self.runtime.spawn(async move {
            let instance_aborts = Arc::new(Mutex::new(HashMap::new()));
            let instance_dirs = Arc::new(Mutex::new(HashMap::new()));
            let instance_pids = Arc::new(Mutex::new(HashMap::new()));
            let archive_dir = Arc::new(Mutex::new(config.runs_dir()));

            if let Ok(mut g) = local_runs.lock() {
                g.insert(
                    run_id_str.clone(),
                    LocalRunControl {
                        run_abort: Arc::clone(&run_abort),
                        instance_aborts: Arc::clone(&instance_aborts),
                        instance_dirs: Arc::clone(&instance_dirs),
                        instance_pids: Arc::clone(&instance_pids),
                        archive_dir: Arc::clone(&archive_dir),
                    },
                );
            }

            emit_ui(
                &ui_tx,
                &egui_ctx,
                BridgeMessage::LocalRunInitialized {
                    run_id,
                    job_name: spec.name.clone(),
                    spec: spec.clone(),
                },
            );

            let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<RunEvent>();
            let ui_tx_ev = ui_tx.clone();
            let egui_ctx_ev = Arc::clone(&egui_ctx);
            let pids_ev = Arc::clone(&instance_pids);
            let run_id_ev = run_id_str.clone();
            let forwarder = tokio::spawn(async move {
                while let Some(event) = ev_rx.recv().await {
                    if let RunEvent::InstanceStateChanged {
                        ref instance_id,
                        state: InstanceState::Running,
                        message: Some(ref msg),
                        ..
                    } = event
                    {
                        if let Some(pid) = parse_pid_from_message(msg) {
                            if let Ok(mut pids) = pids_ev.lock() {
                                pids.insert(instance_id.clone(), pid);
                            }
                        }
                    }
                    emit_ui(
                        &ui_tx_ev,
                        &egui_ctx_ev,
                        BridgeMessage::RunEventReceived {
                            run_id: run_id_ev.clone(),
                            event,
                        },
                    );
                }
            });

            let archive_dir_cb = Arc::clone(&archive_dir);
            let aborts_cb = Arc::clone(&instance_aborts);
            let dirs_cb = Arc::clone(&instance_dirs);

            let options = JobExecutionOptions {
                run_id: Some(run_id),
                event_tx: Some(ev_tx),
                on_initialized: Some(Arc::new(move |_id, run_dir: &Path| {
                    if let Ok(mut dir_slot) = archive_dir_cb.lock() {
                        *dir_slot = run_dir.to_path_buf();
                    }
                })),
                on_instance_registered: Some(Arc::new(
                    move |inst_id: &str, inst_abort: AbortHandle, inst_dir: &Path| {
                        if let Ok(mut aborts) = aborts_cb.lock() {
                            aborts.insert(inst_id.to_string(), inst_abort);
                        }
                        if let Ok(mut dirs) = dirs_cb.lock() {
                            dirs.insert(inst_id.to_string(), inst_dir.to_path_buf());
                        }
                    },
                )),
            };

            let result = execute_job_with_ipc_options(&config, &spec, run_abort, options)
                .await
                .map_err(|e| format!("{e:#}"));

            let _ = forwarder.await;

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

/// Parses `"pid=12345"` or `"running (pid 12345)"` emitted by `deming-runner::client`.
pub fn parse_pid_from_message(msg: &str) -> Option<u32> {
    if let Some(idx) = msg.find("pid=") {
        let digits: String = msg[idx + 4..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(pid) = digits.parse::<u32>() {
            return Some(pid);
        }
    }
    let marker = "(pid ";
    let start = msg.find(marker)? + marker.len();
    let rest = &msg[start..];
    let end = rest.find(')')?;
    rest[..end].trim().parse::<u32>().ok()
}
