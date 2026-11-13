//! Clap CLI argument and subcommand definitions for `deming`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use deming_core::{ModLoader, ProcessPriority};

use crate::pack_ops::SearchSource;

/// Deming: Pure Rust Minecraft Java Edition mod test orchestrator.
#[derive(Debug, Parser)]
#[command(name = "deming", version, about = "Minecraft Java Edition automated mod test runner")]
pub struct Cli {
    /// Emit machine-readable JSON output.
    #[arg(long, global = true)]
    pub json: bool,

    /// Path to the Deming config JSON file.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Inspect or update global Deming configuration.
    Config(ConfigCommand),
    /// Manage offline and Microsoft accounts.
    Account(AccountCommand),
    /// Submit, inspect, or abort test runs.
    Run(RunCommand),
    /// Search Modrinth or CurseForge for mods.
    Search(SearchArgs),
    /// Add a mod to a `deming.toml` or `mods.json` manifest.
    Add(AddArgs),
    /// Synchronize an instance's `mods/` directory and `deming.lock` with a manifest.
    Sync(SyncArgs),
    /// Launch the stdio JSON-RPC 2.0 MCP server.
    Mcp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LoaderArg {
    Vanilla,
    Fabric,
    #[value(name = "neoforge")]
    NeoForge,
    Forge,
    Quilt,
}

impl From<LoaderArg> for ModLoader {
    fn from(value: LoaderArg) -> Self {
        match value {
            LoaderArg::Vanilla => ModLoader::Vanilla,
            LoaderArg::Fabric => ModLoader::Fabric,
            LoaderArg::NeoForge => ModLoader::NeoForge,
            LoaderArg::Forge => ModLoader::Forge,
            LoaderArg::Quilt => ModLoader::Quilt,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SourceArg {
    Modrinth,
    #[value(name = "curseforge")]
    CurseForge,
}

impl From<SourceArg> for SearchSource {
    fn from(value: SourceArg) -> Self {
        match value {
            SourceArg::Modrinth => SearchSource::Modrinth,
            SourceArg::CurseForge => SearchSource::CurseForge,
        }
    }
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Search query string.
    pub query: String,
    /// Filter by Minecraft version (e.g. `1.21.4`).
    #[arg(long)]
    pub mc_version: Option<String>,
    /// Filter by mod loader.
    #[arg(long, value_enum)]
    pub loader: Option<LoaderArg>,
    /// Mod source provider.
    #[arg(long, value_enum, default_value_t = SourceArg::Modrinth)]
    pub source: SourceArg,
    /// Maximum number of results to return.
    #[arg(long, default_value_t = 10)]
    pub limit: usize,
}

#[derive(Debug, Args)]
pub struct AddArgs {
    /// Modrinth project slug or ID to add.
    pub slug: String,
    /// Path to `deming.toml` or `mods.json`.
    #[arg(long)]
    pub manifest: Option<PathBuf>,
    /// Target Minecraft version (used if creating or updating manifest).
    #[arg(long)]
    pub mc_version: Option<String>,
    /// Target mod loader (used if creating or updating manifest).
    #[arg(long, value_enum)]
    pub loader: Option<LoaderArg>,
    /// Optional pinned Modrinth version ID.
    #[arg(long)]
    pub version_id: Option<String>,
}

#[derive(Debug, Args)]
pub struct SyncArgs {
    /// Path to `deming.toml` or `mods.json`.
    #[arg(long)]
    pub manifest: Option<PathBuf>,
    /// Target instance directory containing `mods/` and `deming.lock`.
    #[arg(long)]
    pub instance: Option<PathBuf>,
    /// Preview diff and lockfile without modifying disk.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct ConfigCommand {
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Print the current configuration.
    Get,
    /// Update configuration fields and persist to disk.
    Set(ConfigSetArgs),
}

#[derive(Debug, Args, Default)]
pub struct ConfigSetArgs {
    /// Root workspace directory for caches, instances, servers, runs, and accounts.
    #[arg(long)]
    pub workspace: Option<PathBuf>,
    /// Maximum combined RAM in MiB across concurrent instances.
    #[arg(long)]
    pub max_ram_mb: Option<u64>,
    /// Maximum number of concurrent instances.
    #[arg(long)]
    pub max_concurrent: Option<usize>,
    /// Default CPU cores per instance.
    #[arg(long)]
    pub cpu_cores: Option<usize>,
    /// Default RAM in MiB per instance.
    #[arg(long)]
    pub default_ram_mb: Option<u64>,
    /// OS process priority for launched JVMs.
    #[arg(long, value_enum)]
    pub priority: Option<PriorityArg>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PriorityArg {
    #[value(name = "below-normal")]
    BelowNormal,
    #[value(name = "normal")]
    Normal,
    #[value(name = "above-normal")]
    AboveNormal,
    #[value(name = "high")]
    High,
}

impl From<PriorityArg> for ProcessPriority {
    fn from(value: PriorityArg) -> Self {
        match value {
            PriorityArg::BelowNormal => ProcessPriority::BelowNormal,
            PriorityArg::Normal => ProcessPriority::Normal,
            PriorityArg::AboveNormal => ProcessPriority::AboveNormal,
            PriorityArg::High => ProcessPriority::High,
        }
    }
}

#[derive(Debug, Args)]
pub struct AccountCommand {
    #[command(subcommand)]
    pub action: AccountAction,
}

#[derive(Debug, Subcommand)]
pub enum AccountAction {
    /// List all stored accounts.
    List,
    /// Create and store a deterministic offline/dev account.
    AddOffline {
        /// Minecraft username (1-16 ASCII alphanumeric or underscore).
        username: String,
    },
    /// Authenticate a Microsoft account via OAuth Device-Code flow.
    LoginMsa {
        /// Optional Azure/Xbox OAuth client ID override.
        #[arg(long)]
        client_id: Option<String>,
    },
    /// Remove a stored account by UUID.
    Remove {
        /// Account UUID to remove.
        uuid: String,
    },
}

#[derive(Debug, Args)]
pub struct RunCommand {
    #[command(subcommand)]
    pub action: RunAction,
}

#[derive(Debug, Subcommand)]
pub enum RunAction {
    /// Submit and execute a test job from a JSON specification file.
    Submit {
        /// Path to the TestJobSpec JSON file.
        job_file: PathBuf,
    },
    /// Show the manifest/status of a run.
    Status {
        /// Run UUID or run folder/archive name.
        run_id: String,
    },
    /// Search or view logs from a run.
    Logs {
        /// Run UUID or run folder/archive name.
        run_id: String,
        /// Filter logs to a specific instance ID (or "server").
        #[arg(long)]
        instance: Option<String>,
        /// Filter log lines using a regular expression.
        #[arg(long)]
        grep: Option<String>,
    },
    /// Capture or inspect screenshots for an instance in a run.
    Screenshot {
        /// Run UUID or run folder/archive name.
        run_id: String,
        /// Target instance ID.
        instance_id: String,
    },
    /// Request abort of a specific run or all active runs via IPC.
    Abort {
        /// Optional run ID; aborts all runs if omitted.
        run_id: Option<String>,
    },
    /// List archived and recent test runs.
    List,
}
