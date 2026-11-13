//! Account management and Settings draft state.

use std::path::PathBuf;

use deming_auth::{DeviceCodePrompt, StoredAccount};
use deming_core::{DemingConfig, GlobalResourceLimits, ProcessPriority, RetentionPolicy};

use crate::runner_bridge::DEFAULT_MSA_CLIENT_ID;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
