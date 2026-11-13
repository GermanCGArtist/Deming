//! Background Tokio runtime, IPC server bridge, local job executor, and MSA login tasks.

mod fetch_ops;
mod ipc_sync;
mod job_runner;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use deming_auth::{DeviceCodePrompt, MsaSession};
use deming_core::{DemingLockfile, ModsManifest, RunEvent, RunManifest, TestJobSpec};
use deming_fetch::SyncPlan;
use deming_ipc::IpcBroadcaster;
use deming_runner::AbortHandle;
use tokio::runtime::Runtime;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

pub use job_runner::parse_pid_from_message;

/// Default Microsoft Azure public client ID for Minecraft OAuth flows.
pub const DEFAULT_MSA_CLIENT_ID: &str = deming_engine::DEFAULT_MSA_CLIENT_ID;

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

/// Summary payload emitted when a `.mrpack` or CurseForge `.zip` modpack is imported.
#[derive(Debug, Clone)]
pub struct ImportedPackPayload {
    pub pack_name: String,
    pub manifest: ModsManifest,
    pub lockfile: Option<DemingLockfile>,
    pub summary: String,
    pub warnings: Vec<String>,
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
    MsaBrowserLoginFinished(Result<MsaSession, String>),
    MsaPopupLoginFinished(Result<MsaSession, String>),
    ModpackImported(Result<ImportedPackPayload, String>),
    ModpackExported(Result<PathBuf, String>),
    ModsSynced {
        dry_run: bool,
        result: Result<SyncPlan, String>,
    },
}

#[derive(Debug, Clone)]
pub(super) struct LocalRunControl {
    pub(super) run_abort: Arc<AbortHandle>,
    pub(super) instance_aborts: Arc<Mutex<HashMap<String, AbortHandle>>>,
    pub(super) instance_dirs: Arc<Mutex<HashMap<String, PathBuf>>>,
    pub(super) instance_pids: Arc<Mutex<HashMap<String, u32>>>,
    pub(super) archive_dir: Arc<Mutex<PathBuf>>,
}

/// Owns the multi-threaded Tokio runtime and coordinates IPC + local run execution.
pub struct GuiBridge {
    pub(super) runtime: Runtime,
    pub(super) ui_tx: mpsc::UnboundedSender<BridgeMessage>,
    ui_rx: mpsc::UnboundedReceiver<BridgeMessage>,
    pub(super) ipc_broadcaster: Arc<Mutex<Option<IpcBroadcaster>>>,
    pub(super) ipc_shutdown: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    pub(super) local_runs: Arc<Mutex<HashMap<String, LocalRunControl>>>,
    pub(super) local_run_ids: Arc<Mutex<HashSet<String>>>,
    pub(super) msa_cancel: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    pub(super) egui_ctx: Arc<Mutex<Option<egui::Context>>>,
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
            local_run_ids: Arc::new(Mutex::new(HashSet::new())),
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
}

impl Default for GuiBridge {
    fn default() -> Self {
        Self::new()
    }
}

pub(super) fn emit_ui(
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
