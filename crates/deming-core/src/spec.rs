//! Job, instance, server, mod, account, and completion-rule specifications.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Supported Minecraft mod loaders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModLoader {
    Vanilla,
    Fabric,
    NeoForge,
    Forge,
    Quilt,
}

/// Source from which a mod artifact is resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModSource {
    /// Local `.jar` file on disk.
    LocalPath(PathBuf),
    /// Mod hosted on Modrinth.
    Modrinth {
        project_id_or_slug: String,
        version_id: Option<String>,
    },
}

/// Account reference used to launch a Minecraft client instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountRef {
    /// Offline/development account with an arbitrary username.
    Offline { username: String },
    /// Authenticated Microsoft account.
    Microsoft { uuid: String, username: String },
}

/// Rule describing how an instance's test outcome is evaluated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestCompletionRule {
    /// Passes if the process exits with code `0`.
    ExitCodeZero,
    /// Evaluates log output against success and optional failure regular expressions.
    LogPattern {
        success_regex: String,
        failure_regex: Option<String>,
    },
    /// Evaluates GameTest XML/JSON report files matching a glob pattern.
    GameTestReport { report_path_glob: String },
    /// Requires all nested completion rules to pass.
    Composite(Vec<TestCompletionRule>),
}

/// Specification for a single Minecraft client instance in a test job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceSpec {
    /// Unique identifier for this instance within the job.
    pub id: String,
    /// Target Minecraft version (e.g., `"1.21.4"`).
    pub mc_version: String,
    /// Mod loader to install and launch.
    pub loader: ModLoader,
    /// Optional specific mod loader version; uses latest compatible if `None`.
    pub loader_version: Option<String>,
    /// Mods to provision into this instance.
    #[serde(default)]
    pub mods: Vec<ModSource>,
    /// Account used to launch the client.
    pub account: AccountRef,
    /// Optional RAM limit in MiB; falls back to global default if `None`.
    pub ram_mb: Option<u64>,
    /// Optional CPU core limit; falls back to global default if `None`.
    pub cpu_cores: Option<usize>,
    /// Additional JVM arguments.
    #[serde(default)]
    pub jvm_args: Vec<String>,
    /// Additional Minecraft game arguments.
    #[serde(default)]
    pub game_args: Vec<String>,
    /// Rule used to determine whether this instance passed or failed.
    pub test_rule: TestCompletionRule,
}

/// Specification for an optional dedicated server launched alongside client instances.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSpec {
    /// Target Minecraft version.
    pub mc_version: String,
    /// Mod loader to install and launch.
    pub loader: ModLoader,
    /// Optional specific mod loader version.
    pub loader_version: Option<String>,
    /// Mods to provision onto the server.
    #[serde(default)]
    pub mods: Vec<ModSource>,
    /// Whether `online-mode` is enabled in `server.properties`.
    pub online_mode: bool,
    /// Optional TCP port override; chooses an available port if `None`.
    pub port: Option<u16>,
    /// Extra `server.properties` key-value overrides.
    #[serde(default)]
    pub server_properties: BTreeMap<String, String>,
    /// Regex pattern in server stdout indicating the server is ready for client connections.
    pub ready_pattern: String,
    /// Whether client instances should automatically connect to this server on launch.
    pub auto_connect_clients: bool,
    /// Optional RAM limit in MiB for the server JVM.
    pub ram_mb: Option<u64>,
}

/// Top-level specification for a multi-instance (and optional server) test job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestJobSpec {
    /// Human-readable name of the test job.
    pub name: String,
    /// Client instances to run as part of this job.
    pub instances: Vec<InstanceSpec>,
    /// Optional dedicated server to start before connecting clients.
    pub server: Option<ServerSpec>,
    /// Overall job timeout in seconds.
    pub timeout_secs: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_job_spec_json_round_trip() {
        let mut server_props = BTreeMap::new();
        server_props.insert("gamemode".to_string(), "creative".to_string());
        server_props.insert("difficulty".to_string(), "peaceful".to_string());

        let spec = TestJobSpec {
            name: "fabric-multiplayer-smoke".to_string(),
            instances: vec![
                InstanceSpec {
                    id: "client-1".to_string(),
                    mc_version: "1.21.4".to_string(),
                    loader: ModLoader::Fabric,
                    loader_version: Some("0.16.10".to_string()),
                    mods: vec![
                        ModSource::LocalPath(PathBuf::from("mods/example-mod.jar")),
                        ModSource::Modrinth {
                            project_id_or_slug: "fabric-api".to_string(),
                            version_id: Some("0.115.0+1.21.4".to_string()),
                        },
                    ],
                    account: AccountRef::Offline {
                        username: "TesterOne".to_string(),
                    },
                    ram_mb: Some(2048),
                    cpu_cores: Some(2),
                    jvm_args: vec!["-XX:+UseG1GC".to_string()],
                    game_args: vec!["--width".to_string(), "1280".to_string()],
                    test_rule: TestCompletionRule::Composite(vec![
                        TestCompletionRule::ExitCodeZero,
                        TestCompletionRule::LogPattern {
                            success_regex: r"All tests passed".to_string(),
                            failure_regex: Some(r"FATAL|AssertionError".to_string()),
                        },
                        TestCompletionRule::GameTestReport {
                            report_path_glob: "reports/*.xml".to_string(),
                        },
                    ]),
                },
                InstanceSpec {
                    id: "client-2".to_string(),
                    mc_version: "1.21.4".to_string(),
                    loader: ModLoader::NeoForge,
                    loader_version: None,
                    mods: vec![],
                    account: AccountRef::Microsoft {
                        uuid: "00000000-0000-0000-0000-000000000001".to_string(),
                        username: "TesterTwo".to_string(),
                    },
                    ram_mb: None,
                    cpu_cores: None,
                    jvm_args: vec![],
                    game_args: vec![],
                    test_rule: TestCompletionRule::ExitCodeZero,
                },
            ],
            server: Some(ServerSpec {
                mc_version: "1.21.4".to_string(),
                loader: ModLoader::Fabric,
                loader_version: Some("0.16.10".to_string()),
                mods: vec![ModSource::LocalPath(PathBuf::from("mods/example-mod.jar"))],
                online_mode: false,
                port: Some(25565),
                server_properties: server_props,
                ready_pattern: r"Done \([0-9.]+s\)! For help, type".to_string(),
                auto_connect_clients: true,
                ram_mb: Some(4096),
            }),
            timeout_secs: 300,
        };

        let json = serde_json::to_string_pretty(&spec).unwrap();
        let parsed: TestJobSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, spec);
    }
}
