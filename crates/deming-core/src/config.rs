//! Global configuration, resource limits, and retention policies.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// OS scheduling priority for launched Minecraft client and server processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProcessPriority {
    /// Lower than normal scheduling priority.
    BelowNormal,
    /// Standard OS scheduling priority.
    #[default]
    Normal,
    /// Elevated scheduling priority.
    AboveNormal,
    /// High scheduling priority.
    High,
}

/// Global CPU, memory, and concurrency limits enforced by the orchestrator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GlobalResourceLimits {
    /// Maximum combined RAM (in MiB) across all concurrently running instances/servers.
    pub max_total_ram_mb: u64,
    /// Maximum number of instances allowed to run simultaneously.
    pub max_concurrent_instances: usize,
    /// Default RAM allocation (in MiB) when an instance does not specify `ram_mb`.
    pub default_instance_ram_mb: u64,
    /// Default CPU core affinity/limit per instance when `cpu_cores` is unset.
    pub default_cpu_cores_per_instance: usize,
    /// Process priority applied to spawned JVM processes.
    pub process_priority: ProcessPriority,
}

impl Default for GlobalResourceLimits {
    fn default() -> Self {
        Self {
            max_total_ram_mb: 16_384,
            max_concurrent_instances: 4,
            default_instance_ram_mb: 2_048,
            default_cpu_cores_per_instance: 2,
            process_priority: ProcessPriority::Normal,
        }
    }
}

/// Artifact retention and pruning configuration for completed test runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RetentionPolicy {
    /// Maximum number of run directories to retain before pruning oldest runs.
    pub max_runs: usize,
    /// Compress run artifacts into archives after this many newer runs exist.
    pub compress_after_runs: usize,
    /// Whether pruning/compression runs automatically after each test job.
    pub auto_prune: bool,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            max_runs: 50,
            compress_after_runs: 10,
            auto_prune: true,
        }
    }
}

/// Top-level Deming configuration persisted to disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DemingConfig {
    /// Root workspace directory for caches, instances, servers, runs, and accounts.
    pub workspace_dir: PathBuf,
    /// Global resource limits for concurrent test execution.
    pub global_limits: GlobalResourceLimits,
    /// Retention policy for historical run artifacts.
    pub retention: RetentionPolicy,
}

impl Default for DemingConfig {
    fn default() -> Self {
        let base = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
        Self {
            workspace_dir: base.join("deming"),
            global_limits: GlobalResourceLimits::default(),
            retention: RetentionPolicy::default(),
        }
    }
}

impl DemingConfig {
    /// Directory for downloaded Minecraft jars, libraries, assets, and mod caches.
    pub fn cache_dir(&self) -> PathBuf {
        self.workspace_dir.join("cache")
    }

    /// Directory containing prepared client instance working directories.
    pub fn instances_dir(&self) -> PathBuf {
        self.workspace_dir.join("instances")
    }

    /// Directory containing prepared dedicated server working directories.
    pub fn servers_dir(&self) -> PathBuf {
        self.workspace_dir.join("servers")
    }

    /// Directory containing per-run manifests, logs, screenshots, and bundles.
    pub fn runs_dir(&self) -> PathBuf {
        self.workspace_dir.join("runs")
    }

    /// Path to the persisted accounts store JSON file.
    pub fn accounts_file(&self) -> PathBuf {
        self.workspace_dir.join("accounts.json")
    }

    /// Loads configuration from `path`, or returns [`DemingConfig::default()`] if the file does not exist.
    pub fn load_or_default(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)?;
        let config = serde_json::from_str(&raw)?;
        Ok(config)
    }

    /// Saves this configuration as pretty-printed JSON to `path`, creating parent directories if needed.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_paths_are_under_workspace_dir() {
        let cfg = DemingConfig {
            workspace_dir: PathBuf::from("C:/deming-ws"),
            ..DemingConfig::default()
        };

        assert_eq!(cfg.cache_dir(), PathBuf::from("C:/deming-ws/cache"));
        assert_eq!(cfg.instances_dir(), PathBuf::from("C:/deming-ws/instances"));
        assert_eq!(cfg.servers_dir(), PathBuf::from("C:/deming-ws/servers"));
        assert_eq!(cfg.runs_dir(), PathBuf::from("C:/deming-ws/runs"));
        assert_eq!(cfg.accounts_file(), PathBuf::from("C:/deming-ws/accounts.json"));
    }

    #[test]
    fn config_save_and_load_round_trip() {
        let temp_dir = std::env::temp_dir().join(format!("deming-core-test-{}", uuid::Uuid::new_v4()));
        let config_path = temp_dir.join("nested").join("config.json");

        // Missing file returns default
        let missing = DemingConfig::load_or_default(&config_path).unwrap();
        assert_eq!(missing, DemingConfig::default());

        let custom = DemingConfig {
            workspace_dir: temp_dir.join("ws"),
            global_limits: GlobalResourceLimits {
                max_total_ram_mb: 32_768,
                max_concurrent_instances: 6,
                default_instance_ram_mb: 4_096,
                default_cpu_cores_per_instance: 4,
                process_priority: ProcessPriority::AboveNormal,
            },
            retention: RetentionPolicy {
                max_runs: 100,
                compress_after_runs: 20,
                auto_prune: false,
            },
        };

        custom.save(&config_path).unwrap();
        let loaded = DemingConfig::load_or_default(&config_path).unwrap();
        assert_eq!(loaded, custom);

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
