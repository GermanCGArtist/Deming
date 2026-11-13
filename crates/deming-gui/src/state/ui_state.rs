//! Root UI state, navigation tabs, history viewer state, diagnostics, and status toasts.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use deming_archive::{LogMatch, RetentionReport, RunInfo, ScreenshotInfo};
use deming_core::{DemingConfig, DemingLockfile, ModsManifest, RunManifest};
use deming_fetch::SyncPlan;

use crate::preset::BuilderState;

use super::{AccountsState, DashboardState, SettingsDraft};

/// Top-level navigation tabs in Deming GUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveTab {
    Dashboard,
    TestBuilder,
    RunHistory,
    Accounts,
    Settings,
}

impl ActiveTab {
    pub const ALL: [ActiveTab; 5] = [
        Self::Dashboard,
        Self::TestBuilder,
        Self::RunHistory,
        Self::Accounts,
        Self::Settings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Dashboard => "Dashboard",
            Self::TestBuilder => "Test Builder",
            Self::RunHistory => "Run History & Archive",
            Self::Accounts => "Accounts",
            Self::Settings => "Settings",
        }
    }
}

/// State for the Run History & Archive tab.
#[derive(Default)]
pub struct HistoryState {
    pub runs: Vec<RunInfo>,
    pub selected_run_idx: Option<usize>,
    pub selected_manifest: Option<RunManifest>,
    pub load_error: Option<String>,
    pub log_regex: String,
    pub log_case_sensitive: bool,
    pub log_instance_filter: String,
    pub log_file_filter: String,
    pub log_matches: Vec<LogMatch>,
    pub log_search_error: Option<String>,
    pub screenshots: Vec<ScreenshotInfo>,
    pub selected_screenshot_idx: Option<usize>,
    pub loaded_textures: HashMap<String, egui::TextureHandle>,
    pub screenshot_load_error: Option<String>,
    pub last_retention_report: Option<RetentionReport>,
}

/// Diagnostics and mod sync/lockfile summary state shared across Builder and Dashboard.
#[derive(Debug, Clone, Default)]
pub struct ModSyncDiagnosticsState {
    pub in_flight: bool,
    pub target_instance_id: String,
    pub last_sync_plan: Option<SyncPlan>,
    pub last_lockfile: Option<DemingLockfile>,
    pub last_manifest: Option<ModsManifest>,
    pub last_pack_summary: Option<String>,
    pub last_error: Option<String>,
}

/// Notification toast shown in the top/bottom status bar.
#[derive(Debug, Clone)]
pub struct StatusToast {
    pub is_error: bool,
    pub text: String,
    pub created_at: DateTime<Utc>,
}

/// Root UI state passed to views.
pub struct UiState {
    pub active_tab: ActiveTab,
    pub config_path: PathBuf,
    pub config: DemingConfig,
    pub ipc_port: Option<u16>,
    pub ipc_error: Option<String>,
    pub toast: Option<StatusToast>,
    pub dashboard: DashboardState,
    pub builder: BuilderState,
    pub history: HistoryState,
    pub accounts: AccountsState,
    pub settings: SettingsDraft,
    pub diagnostics: ModSyncDiagnosticsState,
}
