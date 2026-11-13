//! Test Builder UI draft schema types and helper constants.

use deming_core::ModLoader;
use serde::{Deserialize, Serialize};

/// Editable mod entry inside the Test Builder UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModEntryDraft {
    pub is_modrinth: bool,
    pub local_path: String,
    pub modrinth_slug: String,
    pub modrinth_version_id: String,
}

impl ModEntryDraft {
    pub fn local(path: impl Into<String>) -> Self {
        Self {
            is_modrinth: false,
            local_path: path.into(),
            modrinth_slug: String::new(),
            modrinth_version_id: String::new(),
        }
    }

    pub fn modrinth(slug: impl Into<String>, version_id: Option<String>) -> Self {
        Self {
            is_modrinth: true,
            local_path: String::new(),
            modrinth_slug: slug.into(),
            modrinth_version_id: version_id.unwrap_or_default(),
        }
    }
}

/// Kind selector for [`CompletionRuleDraft`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompletionRuleKind {
    ExitCodeZero,
    LogPattern,
    GameTestReport,
    Composite,
}

impl CompletionRuleKind {
    pub const ALL: [CompletionRuleKind; 4] = [
        Self::ExitCodeZero,
        Self::LogPattern,
        Self::GameTestReport,
        Self::Composite,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::ExitCodeZero => "Exit Code 0",
            Self::LogPattern => "Log Pattern (Regex)",
            Self::GameTestReport => "GameTest Report (Glob)",
            Self::Composite => "Composite (All Must Pass)",
        }
    }
}

/// Editable [`deming_core::TestCompletionRule`] draft for the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionRuleDraft {
    pub kind: CompletionRuleKind,
    pub success_regex: String,
    pub failure_regex: String,
    pub report_path_glob: String,
    pub sub_rules: Vec<CompletionRuleDraft>,
}

impl Default for CompletionRuleDraft {
    fn default() -> Self {
        Self {
            kind: CompletionRuleKind::ExitCodeZero,
            success_regex: r"Done \([0-9.]+s\)!|All tests passed".to_string(),
            failure_regex: r"FATAL|AssertionError|MixinApplyError".to_string(),
            report_path_glob: "reports/*.xml".to_string(),
            sub_rules: Vec::new(),
        }
    }
}

impl CompletionRuleDraft {
    pub fn exit_code_zero() -> Self {
        Self {
            kind: CompletionRuleKind::ExitCodeZero,
            ..Default::default()
        }
    }
}

/// Editable account choice for an instance in the Test Builder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountDraft {
    pub is_microsoft: bool,
    pub username: String,
    pub uuid: String,
}

impl Default for AccountDraft {
    fn default() -> Self {
        Self {
            is_microsoft: false,
            username: "TesterOne".to_string(),
            uuid: String::new(),
        }
    }
}

impl AccountDraft {
    pub fn display_label(&self) -> String {
        if self.is_microsoft {
            format!("MSA: {}", self.username)
        } else {
            format!("Offline: {}", self.username)
        }
    }
}

/// Editable draft for a single [`deming_core::InstanceSpec`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceDraft {
    pub id: String,
    pub mc_version: String,
    pub loader: ModLoader,
    pub loader_version: String,
    pub mods: Vec<ModEntryDraft>,
    pub account: AccountDraft,
    pub use_custom_ram: bool,
    pub ram_mb: u64,
    pub use_custom_cpu: bool,
    pub cpu_cores: usize,
    pub jvm_args_text: String,
    pub game_args_text: String,
    pub test_rule: CompletionRuleDraft,
}

impl InstanceDraft {
    pub fn new_with_index(index: usize) -> Self {
        Self {
            id: format!("client-{index}"),
            mc_version: "1.21.4".to_string(),
            loader: ModLoader::Fabric,
            loader_version: String::new(),
            mods: Vec::new(),
            account: AccountDraft {
                is_microsoft: false,
                username: format!("Tester{index}"),
                uuid: String::new(),
            },
            use_custom_ram: false,
            ram_mb: 2048,
            use_custom_cpu: false,
            cpu_cores: 2,
            jvm_args_text: String::new(),
            game_args_text: String::new(),
            test_rule: CompletionRuleDraft::default(),
        }
    }
}

/// Editable draft for an optional [`deming_core::ServerSpec`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerDraft {
    pub enabled: bool,
    pub mc_version: String,
    pub loader: ModLoader,
    pub loader_version: String,
    pub mods: Vec<ModEntryDraft>,
    pub online_mode: bool,
    pub auto_port: bool,
    pub port: u16,
    pub ready_pattern: String,
    pub auto_connect_clients: bool,
    pub use_custom_ram: bool,
    pub ram_mb: u64,
    pub properties_text: String,
}

impl Default for ServerDraft {
    fn default() -> Self {
        Self {
            enabled: false,
            mc_version: "1.21.4".to_string(),
            loader: ModLoader::Fabric,
            loader_version: String::new(),
            mods: Vec::new(),
            online_mode: false,
            auto_port: true,
            port: 25565,
            ready_pattern: r"Done \([0-9.]+s\)! For help, type".to_string(),
            auto_connect_clients: true,
            use_custom_ram: false,
            ram_mb: 2048,
            properties_text: "gamemode=creative\ndifficulty=peaceful".to_string(),
        }
    }
}

/// Complete editable Test Builder state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuilderState {
    pub job_name: String,
    pub timeout_secs: u64,
    pub instances: Vec<InstanceDraft>,
    pub server: ServerDraft,
}

impl Default for BuilderState {
    fn default() -> Self {
        Self {
            job_name: "mod-smoke-test".to_string(),
            timeout_secs: 300,
            instances: vec![InstanceDraft::new_with_index(1)],
            server: ServerDraft::default(),
        }
    }
}

/// All supported [`ModLoader`] variants for dropdown iteration.
pub const ALL_MOD_LOADERS: [ModLoader; 5] = [
    ModLoader::Vanilla,
    ModLoader::Fabric,
    ModLoader::NeoForge,
    ModLoader::Forge,
    ModLoader::Quilt,
];

pub fn mod_loader_label(loader: ModLoader) -> &'static str {
    match loader {
        ModLoader::Vanilla => "Vanilla",
        ModLoader::Fabric => "Fabric",
        ModLoader::NeoForge => "NeoForge",
        ModLoader::Forge => "Forge",
        ModLoader::Quilt => "Quilt",
    }
}
