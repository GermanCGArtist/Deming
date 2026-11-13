//! Shared UI state structures for all 5 tabs and live run tracking.

mod config_state;
mod job_state;
mod ui_state;

pub use config_state::{AccountsState, SettingsDraft};
pub use job_state::{DashboardState, LiveInstanceState, LiveRunCard, ProgressInfo};
pub use ui_state::{ActiveTab, HistoryState, ModSyncDiagnosticsState, StatusToast, UiState};
