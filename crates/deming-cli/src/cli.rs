//! Clap CLI definitions and command handlers for `deming`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use deming_archive::{LogSearchFilter, RunArchiveReader};
use deming_auth::{create_offline_account, AccountStore, MsaClient, StoredAccount};
use deming_core::{DemingConfig, ModLoader, ProcessPriority, TestJobSpec};
use deming_ipc::{IpcClient, IpcMessage};
use deming_runner::AbortHandle;
use serde_json::json;

use crate::mcp;
use crate::orchestrator::{
    default_config_path, execute_job_with_ipc_options, DEFAULT_MSA_CLIENT_ID,
};
use crate::pack_ops::{
    add_mod_to_manifest, resolve_default_manifest_path, search_mods, sync_manifest, SearchSource,
};

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
        /// Cloak launched Minecraft client windows (auto-enabled when connected to GUI).
        #[arg(long)]
        cloak: bool,
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

/// Applies [`ConfigSetArgs`] to a [`DemingConfig`].
pub fn apply_config_updates(config: &mut DemingConfig, args: &ConfigSetArgs) {
    if let Some(ws) = &args.workspace {
        config.workspace_dir = ws.clone();
    }
    if let Some(max_ram) = args.max_ram_mb {
        config.global_limits.max_total_ram_mb = max_ram;
    }
    if let Some(max_conc) = args.max_concurrent {
        config.global_limits.max_concurrent_instances = max_conc;
    }
    if let Some(cores) = args.cpu_cores {
        config.global_limits.default_cpu_cores_per_instance = cores;
    }
    if let Some(def_ram) = args.default_ram_mb {
        config.global_limits.default_instance_ram_mb = def_ram;
    }
    if let Some(prio) = args.priority {
        config.global_limits.process_priority = prio.into();
    }
}

/// Executes the parsed CLI command.
pub async fn dispatch(cli: Cli) -> Result<()> {
    let config_path = cli.config.unwrap_or_else(default_config_path);

    let Some(command) = cli.command else {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    };

    if matches!(command, Commands::Mcp) {
        return mcp::run_stdio(config_path).await;
    }

    let mut config = DemingConfig::load_or_default(&config_path)
        .with_context(|| format!("failed to load config from {}", config_path.display()))?;

    match command {
        Commands::Config(cmd) => handle_config(cmd, &mut config, &config_path, cli.json)?,
        Commands::Account(cmd) => handle_account(cmd, &config, cli.json).await?,
        Commands::Run(cmd) => handle_run(cmd, &config, cli.json).await?,
        Commands::Search(args) => handle_search(args, &config, cli.json).await?,
        Commands::Add(args) => handle_add(args, cli.json)?,
        Commands::Sync(args) => handle_sync(args, &config, cli.json).await?,
        Commands::Mcp => unreachable!(),
    }

    Ok(())
}

fn handle_config(
    cmd: ConfigCommand,
    config: &mut DemingConfig,
    config_path: &Path,
    json_out: bool,
) -> Result<()> {
    match cmd.action {
        ConfigAction::Get => {
            if json_out {
                println!("{}", serde_json::to_string_pretty(config)?);
            } else {
                println!("Config path: {}", config_path.display());
                println!("Workspace:   {}", config.workspace_dir.display());
                println!(
                    "Limits:      max_ram={} MiB, max_concurrent={}, default_ram={} MiB, cpu_cores={}, priority={:?}",
                    config.global_limits.max_total_ram_mb,
                    config.global_limits.max_concurrent_instances,
                    config.global_limits.default_instance_ram_mb,
                    config.global_limits.default_cpu_cores_per_instance,
                    config.global_limits.process_priority
                );
            }
        }
        ConfigAction::Set(args) => {
            apply_config_updates(config, &args);
            config
                .save(config_path)
                .with_context(|| format!("failed to save config to {}", config_path.display()))?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(config)?);
            } else {
                println!("Saved configuration to {}", config_path.display());
            }
        }
    }
    Ok(())
}

async fn handle_account(cmd: AccountCommand, config: &DemingConfig, json_out: bool) -> Result<()> {
    let store = AccountStore::from_config(config);

    match cmd.action {
        AccountAction::List => {
            let accounts = store.load()?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&accounts)?);
            } else if accounts.is_empty() {
                println!("No accounts stored.");
            } else {
                for acc in accounts {
                    let kind = match acc {
                        StoredAccount::Offline { .. } => "offline",
                        StoredAccount::Microsoft { .. } => "microsoft",
                    };
                    println!("{} ({}) [{}]", acc.username(), acc.uuid(), kind);
                }
            }
        }
        AccountAction::AddOffline { username } => {
            let offline = create_offline_account(username)?;
            let stored = StoredAccount::from_offline(&offline);
            store.upsert(stored.clone())?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&stored)?);
            } else {
                println!("Added offline account {} ({})", offline.username, offline.uuid_hyphenated());
            }
        }
        AccountAction::LoginMsa { client_id } => {
            let cid = client_id
                .or_else(|| std::env::var("DEMING_MSA_CLIENT_ID").ok())
                .unwrap_or_else(|| DEFAULT_MSA_CLIENT_ID.to_string());
            let client = MsaClient::new(cid);
            let prompt = client.start_device_code().await?;
            if json_out {
                eprintln!("{}", serde_json::to_string(&prompt)?);
            } else {
                println!(
                    "Open {} and enter code: {}",
                    prompt.verification_uri, prompt.user_code
                );
                println!("Waiting for Microsoft authorization...");
            }
            let session = client.poll_and_complete_login(&prompt).await?;
            let stored = StoredAccount::from_msa(&session);
            store.upsert(stored.clone())?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&stored)?);
            } else {
                println!("Signed in as {} ({})", session.username, session.uuid);
            }
        }
        AccountAction::Remove { uuid } => {
            let removed = store.remove_by_uuid(&uuid)?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&json!({ "uuid": uuid, "removed": removed }))?);
            } else if removed {
                println!("Removed account {uuid}");
            } else {
                println!("No account found with UUID {uuid}");
            }
        }
    }

    Ok(())
}

async fn handle_run(cmd: RunCommand, config: &DemingConfig, json_out: bool) -> Result<()> {
    let reader = RunArchiveReader::new(config.runs_dir());

    match cmd.action {
        RunAction::Submit { job_file, cloak } => {
            let raw = std::fs::read_to_string(&job_file)
                .with_context(|| format!("failed to read job file {}", job_file.display()))?;
            let job: TestJobSpec = serde_json::from_str(&raw)
                .with_context(|| format!("invalid TestJobSpec JSON in {}", job_file.display()))?;

            let abort = Arc::new(AbortHandle::new());
            let abort_for_sig = Arc::clone(&abort);
            let sig_task = tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    let _ = abort_for_sig.abort();
                }
            });
            let manifest = execute_job_with_ipc_options(config, &job, abort, cloak).await?;
            sig_task.abort();

            if json_out {
                println!("{}", serde_json::to_string_pretty(&manifest)?);
            } else {
                println!(
                    "Run {} ({}) finished with status {:?}",
                    manifest.run_id, manifest.job_name, manifest.status
                );
                for inst in &manifest.instances {
                    println!(
                        "  - {}: {:?} (passed={}) {}",
                        inst.instance_id,
                        inst.state,
                        inst.passed,
                        inst.reason.as_deref().unwrap_or("")
                    );
                }
            }
        }
        RunAction::Status { run_id } => {
            let run = reader.find_run(&run_id)?;
            let manifest = reader.load_manifest(&run.path)?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&manifest)?);
            } else {
                println!(
                    "Run {} ({}): {:?}",
                    manifest.run_id, manifest.job_name, manifest.status
                );
                for inst in &manifest.instances {
                    println!(
                        "  - {}: {:?} (passed={}, exit_code={:?})",
                        inst.instance_id, inst.state, inst.passed, inst.exit_code
                    );
                }
            }
        }
        RunAction::Logs {
            run_id,
            instance,
            grep,
        } => {
            let run = reader.find_run(&run_id)?;
            let filter = LogSearchFilter {
                instance_id: instance,
                log_file: None,
                regex: grep,
                case_sensitive: false,
                include_server: true,
                limit: Some(500),
            };
            let matches = reader.search_logs(&run.path, &filter)?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&matches)?);
            } else {
                for m in matches {
                    println!("[{}/{}:{}] {}", m.source, m.log_file, m.line_number, m.line);
                }
            }
        }
        RunAction::Screenshot {
            run_id,
            instance_id,
        } => {
            if let Ok(ipc) = IpcClient::connect(&config.workspace_dir).await {
                if ipc.is_connected() {
                    let _ = ipc
                        .send(&IpcMessage::RequestScreenshot {
                            run_id: run_id.clone(),
                            instance_id: instance_id.clone(),
                        })
                        .await;
                    if let Ok(Some(IpcMessage::ScreenshotResult {
                        path,
                        error,
                        ..
                    })) = ipc.recv_timeout(Duration::from_secs(5)).await
                    {
                        if let Some(err) = error {
                            anyhow::bail!("screenshot capture failed: {err}");
                        }
                        if let Some(p) = path {
                            if json_out {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&json!({
                                        "run_id": run_id,
                                        "instance_id": instance_id,
                                        "path": p
                                    }))?
                                );
                            } else {
                                println!("Captured screenshot: {p}");
                            }
                            return Ok(());
                        }
                    }
                }
            }

            let run = reader.find_run(&run_id)?;
            let shots = reader.list_screenshots(&run.path, Some(&instance_id))?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&shots)?);
            } else if shots.is_empty() {
                println!("No screenshots found for instance '{instance_id}' in run {run_id}.");
            } else {
                for s in shots {
                    println!("{} ({} bytes)", s.relative_path, s.size_bytes);
                }
            }
        }
        RunAction::Abort { run_id } => {
            let ipc = IpcClient::connect(&config.workspace_dir).await?;
            let sent = if let Some(ref rid) = run_id {
                ipc.abort_run(rid).await?
            } else {
                ipc.abort_all().await?
            };
            if json_out {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "aborted": sent,
                        "run_id": run_id,
                    }))?
                );
            } else if sent {
                println!("Abort signal sent.");
            } else {
                println!("No active IPC server found in workspace.");
            }
        }
        RunAction::List => {
            let runs = reader.list_runs()?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&runs)?);
            } else if runs.is_empty() {
                println!("No runs found.");
            } else {
                for r in runs {
                    println!(
                        "{} | {} | {:?} | compressed={}",
                        r.id.map(|u| u.to_string()).unwrap_or_else(|| r.name.clone()),
                        r.job_name.as_deref().unwrap_or("-"),
                        r.status,
                        r.is_compressed
                    );
                }
            }
        }
    }

    Ok(())
}

async fn handle_search(args: SearchArgs, config: &DemingConfig, json_out: bool) -> Result<()> {
    let hits = search_mods(
        config,
        &args.query,
        args.mc_version.as_deref(),
        args.loader.map(Into::into),
        args.source.into(),
        args.limit,
    )
    .await?;

    if json_out {
        println!("{}", serde_json::to_string_pretty(&hits)?);
    } else if hits.is_empty() {
        println!("No mods found matching '{}'.", args.query);
    } else {
        for h in hits {
            println!("{} ({}) — {} [{} downloads]", h.title, h.slug, h.description, h.downloads);
        }
    }
    Ok(())
}

fn handle_add(args: AddArgs, json_out: bool) -> Result<()> {
    let manifest_path = resolve_default_manifest_path(args.manifest);
    let manifest = add_mod_to_manifest(
        &manifest_path,
        &args.slug,
        args.mc_version.as_deref(),
        args.loader.map(Into::into),
        args.version_id.as_deref(),
    )?;

    if json_out {
        println!("{}", serde_json::to_string_pretty(&manifest)?);
    } else {
        println!(
            "Updated {} ({} mods, MC {}, {:?})",
            manifest_path.display(),
            manifest.mods.len(),
            manifest.mc_version,
            manifest.loader
        );
    }
    Ok(())
}

async fn handle_sync(args: SyncArgs, config: &DemingConfig, json_out: bool) -> Result<()> {
    let manifest_path = resolve_default_manifest_path(args.manifest);
    let instance_dir = args
        .instance
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let plan = sync_manifest(config, &manifest_path, &instance_dir, args.dry_run, None).await?;

    if json_out {
        println!("{}", serde_json::to_string_pretty(&plan)?);
    } else {
        let prefix = if args.dry_run { "[dry-run] " } else { "" };
        println!(
            "{}Sync complete: +{} added, ~{} updated, -{} removed, {} unchanged",
            prefix,
            plan.added.len(),
            plan.updated.len(),
            plan.removed.len(),
            plan.unchanged.len()
        );
    }
    Ok(())
}
