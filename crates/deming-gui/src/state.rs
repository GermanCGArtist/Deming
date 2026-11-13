//! Shared UI state structures for all 5 tabs and live run tracking.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use deming_archive::{LogMatch, RetentionReport, RunInfo, ScreenshotInfo};
use deming_auth::{DeviceCodePrompt, StoredAccount};
use deming_core::{
    DemingConfig, DemingLockfile, GlobalResourceLimits, InstanceState, ModsManifest,
    ProcessPriority, RetentionPolicy, RunEvent, RunManifest, RunStatus, TestJobSpec,
};
use deming_fetch::SyncPlan;

use crate::preset::BuilderState;
use crate::runner_bridge::{parse_pid_from_message, DEFAULT_MSA_CLIENT_ID};

const MAX_LIVE_LOG_LINES: usize = 2000;

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

/// Incremental progress reported via [`RunEvent::Progress`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressInfo {
    pub phase: String,
    pub completed: u64,
    pub total: u64,
    pub message: Option<String>,
    pub updated_at: DateTime<Utc>,
}

impl ProgressInfo {
    /// Returns progress fraction in `0.0..=1.0`.
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.completed as f32 / self.total as f32).clamp(0.0, 1.0)
        }
    }
}

/// Live state of an individual instance inside an active/recent run card.
#[derive(Debug, Clone)]
pub struct LiveInstanceState {
    pub instance_id: String,
    pub state: InstanceState,
    pub status_message: Option<String>,
    pub ram_mb: u64,
    pub cpu_cores: usize,
    pub pid: Option<u32>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub progress: Option<ProgressInfo>,
    pub logs: Vec<String>,
    pub screenshots: Vec<PathBuf>,
    pub passed: Option<bool>,
    pub reason: Option<String>,
}

/// Live active or recently completed run card displayed on the Dashboard.
#[derive(Debug, Clone)]
pub struct LiveRunCard {
    pub run_id: String,
    pub job_name: String,
    pub source_label: String,
    pub status: RunStatus,
    pub timeout_secs: u64,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub server_port: Option<u16>,
    pub server_progress: Option<ProgressInfo>,
    pub server_logs: Vec<String>,
    pub instances: Vec<LiveInstanceState>,
}

impl LiveRunCard {
    pub fn from_local_spec(
        run_id: String,
        job_name: String,
        spec: &TestJobSpec,
        config: &DemingConfig,
    ) -> Self {
        let instances = spec
            .instances
            .iter()
            .map(|inst| LiveInstanceState {
                instance_id: inst.id.clone(),
                state: InstanceState::Queued,
                status_message: Some("Queued".to_string()),
                ram_mb: inst
                    .ram_mb
                    .unwrap_or(config.global_limits.default_instance_ram_mb),
                cpu_cores: inst
                    .cpu_cores
                    .unwrap_or(config.global_limits.default_cpu_cores_per_instance),
                pid: None,
                started_at: None,
                finished_at: None,
                progress: None,
                logs: Vec::new(),
                screenshots: Vec::new(),
                passed: None,
                reason: None,
            })
            .collect();

        Self {
            run_id,
            job_name,
            source_label: "Local GUI".to_string(),
            status: RunStatus::Running,
            timeout_secs: spec.timeout_secs,
            started_at: Utc::now(),
            finished_at: None,
            server_port: None,
            server_progress: None,
            server_logs: Vec::new(),
            instances,
        }
    }

    pub fn from_ipc(run_id: String, job_name: String) -> Self {
        Self {
            run_id,
            job_name,
            source_label: "IPC (CLI / MCP)".to_string(),
            status: RunStatus::Running,
            timeout_secs: 0,
            started_at: Utc::now(),
            finished_at: None,
            server_port: None,
            server_progress: None,
            server_logs: Vec::new(),
            instances: Vec::new(),
        }
    }

    /// Returns `(finished_count, total_count)` across instances in this run.
    pub fn completion_counts(&self) -> (usize, usize) {
        let total = self.instances.len();
        let finished = self
            .instances
            .iter()
            .filter(|i| {
                matches!(
                    i.state,
                    InstanceState::Passed
                        | InstanceState::Failed
                        | InstanceState::Aborted
                        | InstanceState::TimedOut
                )
            })
            .count();
        (finished, total)
    }

    /// Applies an incoming [`RunEvent`] to update the live card state.
    pub fn apply_event(&mut self, event: &RunEvent, defaults: &GlobalResourceLimits) {
        match event {
            RunEvent::RunStarted {
                job_name,
                timestamp,
                ..
            } => {
                if !job_name.is_empty() {
                    self.job_name = job_name.clone();
                }
                self.status = RunStatus::Running;
                self.started_at = *timestamp;
            }
            RunEvent::ServerReady { port, .. } => {
                self.server_port = Some(*port);
                self.server_progress = None;
            }
            RunEvent::InstanceStateChanged {
                instance_id,
                state,
                message,
                timestamp,
                ..
            } => {
                let inst = self.ensure_instance(instance_id, defaults);
                inst.state = *state;
                if *state == InstanceState::Running && inst.started_at.is_none() {
                    inst.started_at = Some(*timestamp);
                }
                if matches!(
                    state,
                    InstanceState::Passed
                        | InstanceState::Failed
                        | InstanceState::Aborted
                        | InstanceState::TimedOut
                ) {
                    inst.finished_at = Some(*timestamp);
                }
                if let Some(msg) = message {
                    inst.status_message = Some(msg.clone());
                    if let Some(pid) = parse_pid_from_message(msg) {
                        inst.pid = Some(pid);
                    }
                }
            }
            RunEvent::LogLine { source, line, .. } => {
                if source == "server" {
                    push_bounded(&mut self.server_logs, line.clone());
                } else {
                    let inst = self.ensure_instance(source, defaults);
                    push_bounded(&mut inst.logs, line.clone());
                }
            }
            RunEvent::ScreenshotCaptured {
                instance_id, path, ..
            } => {
                let inst = self.ensure_instance(instance_id, defaults);
                if !inst.screenshots.contains(path) {
                    inst.screenshots.push(path.clone());
                }
            }
            RunEvent::TestEvaluated {
                instance_id,
                passed,
                reason,
                ..
            } => {
                let inst = self.ensure_instance(instance_id, defaults);
                inst.passed = Some(*passed);
                inst.reason = Some(reason.clone());
            }
            RunEvent::Progress {
                timestamp,
                instance_id,
                phase,
                completed,
                total,
                message,
                ..
            } => {
                let info = ProgressInfo {
                    phase: phase.clone(),
                    completed: *completed,
                    total: *total,
                    message: message.clone(),
                    updated_at: *timestamp,
                };
                if let Some(inst_id) = instance_id {
                    let inst = self.ensure_instance(inst_id, defaults);
                    inst.progress = Some(info);
                } else {
                    self.server_progress = Some(info);
                }
            }
            RunEvent::RunFinished {
                status, timestamp, ..
            } => {
                self.status = *status;
                self.finished_at = Some(*timestamp);
            }
        }
    }

    fn ensure_instance(
        &mut self,
        instance_id: &str,
        defaults: &GlobalResourceLimits,
    ) -> &mut LiveInstanceState {
        if let Some(pos) = self
            .instances
            .iter()
            .position(|i| i.instance_id == instance_id)
        {
            return &mut self.instances[pos];
        }
        self.instances.push(LiveInstanceState {
            instance_id: instance_id.to_string(),
            state: InstanceState::Queued,
            status_message: None,
            ram_mb: defaults.default_instance_ram_mb,
            cpu_cores: defaults.default_cpu_cores_per_instance,
            pid: None,
            started_at: None,
            finished_at: None,
            progress: None,
            logs: Vec::new(),
            screenshots: Vec::new(),
            passed: None,
            reason: None,
        });
        let last = self.instances.len() - 1;
        &mut self.instances[last]
    }
}

fn push_bounded(buf: &mut Vec<String>, line: String) {
    if buf.len() >= MAX_LIVE_LOG_LINES {
        let excess = buf.len() - MAX_LIVE_LOG_LINES + 1;
        buf.drain(0..excess);
    }
    buf.push(line);
}

/// State for the Dashboard tab.
#[derive(Debug, Clone)]
pub struct DashboardState {
    pub runs: Vec<LiveRunCard>,
    pub selected_log_target: Option<(String, String)>,
    pub log_filter: String,
    pub auto_scroll_logs: bool,
}

impl Default for DashboardState {
    fn default() -> Self {
        Self {
            runs: Vec::new(),
            selected_log_target: None,
            log_filter: String::new(),
            auto_scroll_logs: true,
        }
    }
}

/// State for the Run History & Archive tab.
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

impl Default for HistoryState {
    fn default() -> Self {
        Self {
            runs: Vec::new(),
            selected_run_idx: None,
            selected_manifest: None,
            load_error: None,
            log_regex: String::new(),
            log_case_sensitive: false,
            log_instance_filter: String::new(),
            log_file_filter: String::new(),
            log_matches: Vec::new(),
            log_search_error: None,
            screenshots: Vec::new(),
            selected_screenshot_idx: None,
            loaded_textures: HashMap::new(),
            screenshot_load_error: None,
            last_retention_report: None,
        }
    }
}

/// State for the Accounts tab.
#[derive(Debug, Clone)]
pub struct AccountsState {
    pub accounts: Vec<StoredAccount>,
    pub offline_username: String,
    pub popup_login_in_flight: bool,
    pub popup_status_msg: Option<String>,
    pub popup_is_error: bool,
    pub browser_code_input: String,
    pub browser_login_in_flight: bool,
    pub browser_status_msg: Option<String>,
    pub browser_is_error: bool,
    pub msa_client_id: String,
    pub msa_prompt: Option<DeviceCodePrompt>,
    pub msa_polling: bool,
    pub msa_status_msg: Option<String>,
    pub msa_is_error: bool,
}

impl Default for AccountsState {
    fn default() -> Self {
        Self {
            accounts: Vec::new(),
            offline_username: String::new(),
            popup_login_in_flight: false,
            popup_status_msg: None,
            popup_is_error: false,
            browser_code_input: String::new(),
            browser_login_in_flight: false,
            browser_status_msg: None,
            browser_is_error: false,
            msa_client_id: DEFAULT_MSA_CLIENT_ID.to_string(),
            msa_prompt: None,
            msa_polling: false,
            msa_status_msg: None,
            msa_is_error: false,
        }
    }
}

/// Editable Settings form state backed by [`DemingConfig`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsDraft {
    pub workspace_dir: String,
    pub max_total_ram_mb: u64,
    pub max_concurrent_instances: usize,
    pub default_instance_ram_mb: u64,
    pub default_cpu_cores_per_instance: usize,
    pub process_priority: ProcessPriority,
    pub max_runs: usize,
    pub compress_after_runs: usize,
    pub auto_prune: bool,
    pub curseforge_api_key: String,
    pub show_cf_api_key: bool,
}

impl SettingsDraft {
    pub fn from_config(cfg: &DemingConfig) -> Self {
        Self {
            workspace_dir: cfg.workspace_dir.to_string_lossy().to_string(),
            max_total_ram_mb: cfg.global_limits.max_total_ram_mb,
            max_concurrent_instances: cfg.global_limits.max_concurrent_instances,
            default_instance_ram_mb: cfg.global_limits.default_instance_ram_mb,
            default_cpu_cores_per_instance: cfg.global_limits.default_cpu_cores_per_instance,
            process_priority: cfg.global_limits.process_priority,
            max_runs: cfg.retention.max_runs,
            compress_after_runs: cfg.retention.compress_after_runs,
            auto_prune: cfg.retention.auto_prune,
            curseforge_api_key: cfg.curseforge_api_key.clone().unwrap_or_default(),
            show_cf_api_key: false,
        }
    }

    pub fn to_config(&self) -> Result<DemingConfig, String> {
        let ws = self.workspace_dir.trim();
        if ws.is_empty() {
            return Err("Workspace directory cannot be empty".to_string());
        }
        if self.max_concurrent_instances == 0 {
            return Err("max_concurrent_instances must be at least 1".to_string());
        }
        if self.default_instance_ram_mb < 256 {
            return Err("default_instance_ram_mb must be at least 256 MiB".to_string());
        }
        if self.max_total_ram_mb < self.default_instance_ram_mb {
            return Err(
                "max_total_ram_mb must be >= default_instance_ram_mb".to_string(),
            );
        }
        if self.default_cpu_cores_per_instance == 0 {
            return Err("default_cpu_cores_per_instance must be at least 1".to_string());
        }

        let cf_key = self.curseforge_api_key.trim();

        Ok(DemingConfig {
            workspace_dir: PathBuf::from(ws),
            global_limits: GlobalResourceLimits {
                max_total_ram_mb: self.max_total_ram_mb,
                max_concurrent_instances: self.max_concurrent_instances,
                default_instance_ram_mb: self.default_instance_ram_mb,
                default_cpu_cores_per_instance: self.default_cpu_cores_per_instance,
                process_priority: self.process_priority,
            },
            retention: RetentionPolicy {
                max_runs: self.max_runs.max(1),
                compress_after_runs: self.compress_after_runs,
                auto_prune: self.auto_prune,
            },
            curseforge_api_key: if cf_key.is_empty() {
                None
            } else {
                Some(cf_key.to_string())
            },
        })
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use deming_core::{AccountRef, InstanceSpec, ModLoader, TestCompletionRule};
    use uuid::Uuid;

    #[test]
    fn live_run_card_applies_events_and_parses_pid() {
        let cfg = DemingConfig::default();
        let spec = TestJobSpec {
            name: "test-job".to_string(),
            instances: vec![InstanceSpec {
                id: "c1".to_string(),
                mc_version: "1.21.4".to_string(),
                loader: ModLoader::Fabric,
                loader_version: None,
                mods: vec![],
                account: AccountRef::Offline {
                    username: "Dev".to_string(),
                },
                ram_mb: Some(1536),
                cpu_cores: Some(2),
                jvm_args: vec![],
                game_args: vec![],
                test_rule: TestCompletionRule::ExitCodeZero,
            }],
            server: None,
            timeout_secs: 120,
        };

        let run_id = Uuid::new_v4();
        let mut card =
            LiveRunCard::from_local_spec(run_id.to_string(), spec.name.clone(), &spec, &cfg);
        assert_eq!(card.instances.len(), 1);
        assert_eq!(card.instances[0].ram_mb, 1536);

        card.apply_event(
            &RunEvent::InstanceStateChanged {
                run_id,
                instance_id: "c1".to_string(),
                state: InstanceState::Running,
                message: Some("running (pid 4242)".to_string()),
                timestamp: Utc::now(),
            },
            &cfg.global_limits,
        );
        assert_eq!(card.instances[0].state, InstanceState::Running);
        assert_eq!(card.instances[0].pid, Some(4242));

        card.apply_event(
            &RunEvent::InstanceStateChanged {
                run_id,
                instance_id: "c1".to_string(),
                state: InstanceState::Running,
                message: Some("pid=98765".to_string()),
                timestamp: Utc::now(),
            },
            &cfg.global_limits,
        );
        assert_eq!(card.instances[0].pid, Some(98765));

        card.apply_event(
            &RunEvent::LogLine {
                run_id,
                source: "c1".to_string(),
                line: "[main/INFO]: Hello world".to_string(),
                timestamp: Utc::now(),
            },
            &cfg.global_limits,
        );
        assert_eq!(card.instances[0].logs.len(), 1);

        card.apply_event(
            &RunEvent::TestEvaluated {
                run_id,
                instance_id: "c1".to_string(),
                passed: true,
                reason: "ExitCodeZero passed".to_string(),
                timestamp: Utc::now(),
            },
            &cfg.global_limits,
        );
        assert_eq!(card.instances[0].passed, Some(true));

        card.apply_event(
            &RunEvent::RunFinished {
                run_id,
                status: RunStatus::Passed,
                timestamp: Utc::now(),
            },
            &cfg.global_limits,
        );
        assert_eq!(card.status, RunStatus::Passed);
        assert!(card.finished_at.is_some());
    }

    #[test]
    fn settings_draft_round_trip_and_validation() {
        let cfg = DemingConfig::default();
        let mut draft = SettingsDraft::from_config(&cfg);
        let converted = draft.to_config().expect("valid default settings");
        assert_eq!(converted, cfg);

        draft.max_concurrent_instances = 0;
        assert!(draft.to_config().is_err());

        draft.max_concurrent_instances = 2;
        draft.max_total_ram_mb = 512;
        draft.default_instance_ram_mb = 1024;
        assert!(draft.to_config().is_err());

        let mut cf_draft = SettingsDraft::from_config(&cfg);
        cf_draft.curseforge_api_key = "  cf-secret-key  ".to_string();
        let cf_cfg = cf_draft.to_config().expect("valid settings with cf key");
        assert_eq!(cf_cfg.curseforge_api_key.as_deref(), Some("cf-secret-key"));
        cf_draft.curseforge_api_key = "   ".to_string();
        assert_eq!(cf_draft.to_config().unwrap().curseforge_api_key, None);
    }

    #[test]
    fn progress_events_update_server_and_instance_progress() {
        let cfg = DemingConfig::default();
        let run_id = Uuid::new_v4();
        let mut card = LiveRunCard::from_ipc(run_id.to_string(), "progress-test".to_string());
        card.timeout_secs = 90;

        card.apply_event(
            &RunEvent::Progress {
                timestamp: Utc::now(),
                run_id: run_id.to_string(),
                instance_id: None,
                phase: "server-loader".to_string(),
                completed: 3,
                total: 10,
                message: Some("Downloading server libraries".to_string()),
            },
            &cfg.global_limits,
        );
        let srv_prog = card
            .server_progress
            .as_ref()
            .expect("server_progress should be recorded");
        assert_eq!(srv_prog.phase, "server-loader");
        assert!((srv_prog.fraction() - 0.3).abs() < 1e-4);

        card.apply_event(
            &RunEvent::Progress {
                timestamp: Utc::now(),
                run_id: run_id.to_string(),
                instance_id: Some("bot-1".to_string()),
                phase: "java".to_string(),
                completed: 1,
                total: 2,
                message: Some("Extracting Java 21".to_string()),
            },
            &cfg.global_limits,
        );
        assert_eq!(card.instances.len(), 1);
        let inst_prog = card.instances[0]
            .progress
            .as_ref()
            .expect("instance progress should be recorded");
        assert_eq!(inst_prog.phase, "java");
        assert_eq!(inst_prog.completed, 1);
        assert_eq!(inst_prog.total, 2);
        assert!((inst_prog.fraction() - 0.5).abs() < 1e-4);

        let t0 = Utc::now();
        card.apply_event(
            &RunEvent::InstanceStateChanged {
                run_id,
                instance_id: "bot-1".to_string(),
                state: InstanceState::Running,
                message: Some("running (pid 999)".to_string()),
                timestamp: t0,
            },
            &cfg.global_limits,
        );
        assert_eq!(card.instances[0].started_at, Some(t0));
        assert_eq!(card.completion_counts(), (0, 1));

        let t1 = Utc::now();
        card.apply_event(
            &RunEvent::InstanceStateChanged {
                run_id,
                instance_id: "bot-1".to_string(),
                state: InstanceState::Passed,
                message: Some("ok".to_string()),
                timestamp: t1,
            },
            &cfg.global_limits,
        );
        assert_eq!(card.instances[0].finished_at, Some(t1));
        assert_eq!(card.completion_counts(), (1, 1));

        // ServerReady clears active server setup progress.
        card.apply_event(
            &RunEvent::ServerReady {
                run_id,
                port: 25565,
                timestamp: Utc::now(),
            },
            &cfg.global_limits,
        );
        assert!(card.server_progress.is_none());
    }
}
