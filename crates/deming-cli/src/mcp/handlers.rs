//! MCP tool execution handlers and workspace path sandboxing.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use deming_archive::RunArchiveReader;
use deming_auth::{create_offline_account, AccountStore, StoredAccount};
use deming_core::{DemingConfig, ModLoader, ModSource, ModsManifest, ProcessPriority, TestJobSpec};
use deming_ipc::{IpcClient, IpcMessage};
use deming_runner::AbortHandle;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::McpServer;
use crate::crash_analyzer::{analyze_crash_text, analyze_instance_crash};
use crate::orchestrator::execute_job_with_ipc;
use crate::pack_ops::{save_manifest_auto, sync_manifest};

/// Single operation in `batch_update_mods`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BatchModOp {
    Add {
        slug: String,
        #[serde(default)]
        version_id: Option<String>,
    },
    Remove {
        target: String,
    },
    Enable {
        filename: String,
    },
    Disable {
        filename: String,
    },
}

/// Resolves `input` relative to `workspace_dir` (or validates an absolute `input`),
/// rejecting any path traversal (`..`) or symlink that escapes `workspace_dir`.
pub fn resolve_sandboxed_path(workspace_dir: &Path, input: &Path) -> Result<PathBuf> {
    if input.as_os_str().is_empty() {
        anyhow::bail!("path cannot be empty");
    }

    let ws_abs = if workspace_dir.is_absolute() {
        workspace_dir.to_path_buf()
    } else {
        std::env::current_dir()?.join(workspace_dir)
    };
    let ws_canon = ws_abs
        .canonicalize()
        .unwrap_or_else(|_| lexical_normalize(&ws_abs));

    let candidate = if input.is_absolute() {
        input.to_path_buf()
    } else {
        ws_canon.join(input)
    };

    let normalized = lexical_normalize(&candidate);
    if !normalized.starts_with(&ws_canon) {
        anyhow::bail!(
            "access denied: path '{}' escapes workspace '{}'",
            input.display(),
            ws_canon.display()
        );
    }

    let mut probe = normalized.as_path();
    while !probe.exists() {
        match probe.parent() {
            Some(p) => probe = p,
            None => break,
        }
    }
    if probe.exists() {
        let probe_canon = probe.canonicalize()?;
        if !probe_canon.starts_with(&ws_canon) {
            anyhow::bail!(
                "access denied: symlinked path '{}' escapes workspace '{}'",
                input.display(),
                ws_canon.display()
            );
        }
    }

    Ok(normalized)
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::Normal(s) => out.push(s),
        }
    }
    out
}

impl McpServer {
    pub(crate) async fn call_tool(&self, name: &str, args: Value) -> Result<Value> {
        match name {
            "get_config" => {
                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                Ok(serde_json::to_value(cfg)?)
            }
            "set_config" => {
                let mut cfg = DemingConfig::load_or_default(&self.config_path)?;
                if let Some(ws) = args.get("workspace_dir").and_then(|v| v.as_str()) {
                    cfg.workspace_dir = PathBuf::from(ws);
                }
                if let Some(v) = args.get("max_total_ram_mb").and_then(|v| v.as_u64()) {
                    cfg.global_limits.max_total_ram_mb = v;
                }
                if let Some(v) = args
                    .get("max_concurrent_instances")
                    .and_then(|v| v.as_u64())
                {
                    cfg.global_limits.max_concurrent_instances = v as usize;
                }
                if let Some(v) = args.get("default_instance_ram_mb").and_then(|v| v.as_u64()) {
                    cfg.global_limits.default_instance_ram_mb = v;
                }
                if let Some(v) = args
                    .get("default_cpu_cores_per_instance")
                    .and_then(|v| v.as_u64())
                {
                    cfg.global_limits.default_cpu_cores_per_instance = v as usize;
                }
                if let Some(p) = args.get("process_priority").and_then(|v| v.as_str()) {
                    cfg.global_limits.process_priority = match p {
                        "below_normal" | "below-normal" => ProcessPriority::BelowNormal,
                        "normal" => ProcessPriority::Normal,
                        "above_normal" | "above-normal" => ProcessPriority::AboveNormal,
                        "high" => ProcessPriority::High,
                        other => anyhow::bail!("invalid process_priority '{other}'"),
                    };
                }
                cfg.save(&self.config_path)?;
                Ok(serde_json::to_value(cfg)?)
            }
            "list_accounts" => {
                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                let store = AccountStore::from_config(&cfg);
                let accounts = store.load()?;
                Ok(serde_json::to_value(accounts)?)
            }
            "add_offline_account" => {
                let username = args
                    .get("username")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing required parameter 'username'"))?;
                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                let store = AccountStore::from_config(&cfg);
                let offline = create_offline_account(username)?;
                let stored = StoredAccount::from_offline(&offline);
                store.upsert(stored.clone())?;
                Ok(serde_json::to_value(stored)?)
            }
            "submit_test_run" => {
                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                let dry_run = args
                    .get("dry_run")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let job_val = args.get("job").cloned().unwrap_or(args);
                let job: TestJobSpec = serde_json::from_value(job_val)?;
                if dry_run {
                    return Ok(json!({
                        "dry_run": true,
                        "valid": true,
                        "job_name": job.name,
                        "instance_count": job.instances.len(),
                        "has_server": job.server.is_some(),
                        "timeout_secs": job.timeout_secs,
                    }));
                }
                let abort = Arc::new(AbortHandle::new());
                let manifest = execute_job_with_ipc(&cfg, &job, abort).await?;
                Ok(serde_json::to_value(manifest)?)
            }
            "apply_modpack" => {
                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                let manifest_raw = args
                    .get("manifest_path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing required parameter 'manifest_path'"))?;
                let instance_raw = args
                    .get("instance_dir")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing required parameter 'instance_dir'"))?;
                let dry_run = args
                    .get("dry_run")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let manifest_path =
                    resolve_sandboxed_path(&cfg.workspace_dir, Path::new(manifest_raw))?;
                let instance_dir =
                    resolve_sandboxed_path(&cfg.workspace_dir, Path::new(instance_raw))?;
                let plan =
                    sync_manifest(&cfg, &manifest_path, &instance_dir, dry_run, None).await?;
                Ok(json!({
                    "dry_run": dry_run,
                    "plan": plan,
                }))
            }
            "update_mod_state" => {
                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                let instance_raw = args
                    .get("instance_dir")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing required parameter 'instance_dir'"))?;
                let filename = args
                    .get("filename")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing required parameter 'filename'"))?;
                let state = args
                    .get("state")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing required parameter 'state'"))?;
                let dry_run = args
                    .get("dry_run")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let instance_dir =
                    resolve_sandboxed_path(&cfg.workspace_dir, Path::new(instance_raw))?;
                let mods_dir = instance_dir.join("mods");
                let clean_name = Path::new(filename)
                    .file_name()
                    .ok_or_else(|| anyhow::anyhow!("invalid mod filename '{filename}'"))?;
                let target_path = resolve_sandboxed_path(&mods_dir, Path::new(clean_name))?;

                apply_mod_file_state(&mods_dir, &target_path, state, dry_run)
            }
            "delete_instance" => {
                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                let instance_raw = args
                    .get("instance_dir")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing required parameter 'instance_dir'"))?;
                let dry_run = args
                    .get("dry_run")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let instance_dir =
                    resolve_sandboxed_path(&cfg.workspace_dir, Path::new(instance_raw))?;
                let exists = instance_dir.exists();
                if !dry_run && exists {
                    std::fs::remove_dir_all(&instance_dir)?;
                }
                Ok(json!({
                    "dry_run": dry_run,
                    "instance_dir": instance_dir,
                    "existed": exists,
                    "deleted": !dry_run && exists,
                }))
            }
            "analyze_crash_log" => {
                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                if let Some(raw_text) = args.get("log_text").and_then(|v| v.as_str()) {
                    let analysis = analyze_crash_text(raw_text);
                    return Ok(serde_json::to_value(analysis)?);
                }
                let dir_raw = args
                    .get("instance_dir")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        anyhow::anyhow!("missing 'instance_dir' or 'log_text' parameter")
                    })?;
                let dir = resolve_sandboxed_path(&cfg.workspace_dir, Path::new(dir_raw))?;
                let analysis = analyze_instance_crash(&dir)?;
                Ok(serde_json::to_value(analysis)?)
            }
            "batch_update_mods" => self.handle_batch_update_mods(args),
            "get_run_status" => {
                let run_id = args
                    .get("run_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing required parameter 'run_id'"))?;
                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                let reader = RunArchiveReader::new(cfg.runs_dir());
                let run = reader.find_run(run_id)?;
                let manifest = reader.load_manifest(&run.path)?;
                Ok(serde_json::to_value(manifest)?)
            }
            "list_runs" => {
                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                let reader = RunArchiveReader::new(cfg.runs_dir());
                let runs = reader.list_runs()?;
                Ok(serde_json::to_value(runs)?)
            }
            "tail_instance_logs" => {
                let run_id = args
                    .get("run_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing required parameter 'run_id'"))?;
                let instance_id = args
                    .get("instance_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("server");
                let log_file = args.get("log_file").and_then(|v| v.as_str());
                let max_lines = args
                    .get("max_lines")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(100) as usize;

                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                let reader = RunArchiveReader::new(cfg.runs_dir());
                let run = reader.find_run(run_id)?;
                let lines = reader.tail_logs(&run.path, instance_id, log_file, max_lines)?;
                Ok(json!({
                    "run_id": run_id,
                    "source": instance_id,
                    "lines": lines,
                }))
            }
            "capture_screenshot" => {
                let run_id = args
                    .get("run_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing required parameter 'run_id'"))?;
                let instance_id = args
                    .get("instance_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing required parameter 'instance_id'"))?;

                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                if let Ok(ipc) = IpcClient::connect(&cfg.workspace_dir).await {
                    if ipc.is_connected() {
                        let _ = ipc
                            .send(&IpcMessage::RequestScreenshot {
                                run_id: run_id.to_string(),
                                instance_id: instance_id.to_string(),
                            })
                            .await;
                        if let Ok(Some(IpcMessage::ScreenshotResult { path, error, .. })) =
                            ipc.recv_timeout(Duration::from_secs(5)).await
                        {
                            if let Some(err) = error {
                                anyhow::bail!("screenshot capture failed: {err}");
                            }
                            if let Some(p) = path {
                                return Ok(json!({
                                    "run_id": run_id,
                                    "instance_id": instance_id,
                                    "path": p,
                                }));
                            }
                        }
                    }
                }

                let reader = RunArchiveReader::new(cfg.runs_dir());
                let run = reader.find_run(run_id)?;
                let shots = reader.list_screenshots(&run.path, Some(instance_id))?;
                Ok(json!({
                    "run_id": run_id,
                    "instance_id": instance_id,
                    "screenshots": shots,
                }))
            }
            "abort_run" => {
                let run_id = args.get("run_id").and_then(|v| v.as_str());
                let cfg = DemingConfig::load_or_default(&self.config_path)?;
                let ipc = IpcClient::connect(&cfg.workspace_dir).await?;
                let sent = if let Some(rid) = run_id {
                    ipc.abort_run(rid).await?
                } else {
                    ipc.abort_all().await?
                };
                Ok(json!({
                    "aborted": sent,
                    "run_id": run_id,
                }))
            }
            unknown => anyhow::bail!("unknown tool: {unknown}"),
        }
    }

    fn handle_batch_update_mods(&self, args: Value) -> Result<Value> {
        let cfg = DemingConfig::load_or_default(&self.config_path)?;
        let instance_raw = args
            .get("instance_dir")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required parameter 'instance_dir'"))?;
        let instance_dir = resolve_sandboxed_path(&cfg.workspace_dir, Path::new(instance_raw))?;
        let mods_dir = instance_dir.join("mods");
        let dry_run = args
            .get("dry_run")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let manifest_path = if let Some(m_raw) = args.get("manifest_path").and_then(|v| v.as_str())
        {
            Some(resolve_sandboxed_path(
                &cfg.workspace_dir,
                Path::new(m_raw),
            )?)
        } else {
            None
        };

        let ops_val = args
            .get("operations")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing required parameter 'operations'"))?;
        let ops: Vec<BatchModOp> = serde_json::from_value(ops_val)?;

        let mut manifest = if let Some(ref mp) = manifest_path {
            if mp.exists() {
                ModsManifest::load_auto(mp)?
            } else {
                ModsManifest {
                    mc_version: args
                        .get("mc_version")
                        .and_then(|v| v.as_str())
                        .unwrap_or("1.21.4")
                        .to_string(),
                    loader: ModLoader::Fabric,
                    loader_version: None,
                    mods: Vec::new(),
                }
            }
        } else {
            ModsManifest {
                mc_version: "1.21.4".to_string(),
                loader: ModLoader::Fabric,
                loader_version: None,
                mods: Vec::new(),
            }
        };

        let mut results = Vec::new();
        for op in ops {
            match op {
                BatchModOp::Add { slug, version_id } => {
                    let exists = manifest.mods.iter().any(|m| match m {
                        ModSource::Modrinth {
                            project_id_or_slug, ..
                        } => project_id_or_slug.eq_ignore_ascii_case(&slug),
                        _ => false,
                    });
                    if !exists {
                        manifest.mods.push(ModSource::Modrinth {
                            project_id_or_slug: slug.clone(),
                            version_id: version_id.clone(),
                        });
                    }
                    results.push(json!({
                        "action": "add",
                        "slug": slug,
                        "added": !exists,
                    }));
                }
                BatchModOp::Remove { target } => {
                    let before = manifest.mods.len();
                    manifest.mods.retain(|m| match m {
                        ModSource::Modrinth {
                            project_id_or_slug, ..
                        } => !project_id_or_slug.eq_ignore_ascii_case(&target),
                        ModSource::LocalPath(p) => {
                            p.file_name().and_then(|s| s.to_str()) != Some(target.as_str())
                        }
                    });
                    let removed_from_manifest = manifest.mods.len() != before;
                    let file_res =
                        if let Ok(p) = resolve_sandboxed_path(&mods_dir, Path::new(&target)) {
                            apply_mod_file_state(&mods_dir, &p, "remove", dry_run)
                                .unwrap_or_else(|_| json!({ "removed": false }))
                        } else {
                            json!({ "removed": false })
                        };
                    results.push(json!({
                        "action": "remove",
                        "target": target,
                        "removed_from_manifest": removed_from_manifest,
                        "file": file_res,
                    }));
                }
                BatchModOp::Enable { filename } => {
                    let clean = Path::new(&filename)
                        .file_name()
                        .ok_or_else(|| anyhow::anyhow!("invalid filename '{filename}'"))?;
                    let p = resolve_sandboxed_path(&mods_dir, Path::new(clean))?;
                    results.push(apply_mod_file_state(&mods_dir, &p, "enable", dry_run)?);
                }
                BatchModOp::Disable { filename } => {
                    let clean = Path::new(&filename)
                        .file_name()
                        .ok_or_else(|| anyhow::anyhow!("invalid filename '{filename}'"))?;
                    let p = resolve_sandboxed_path(&mods_dir, Path::new(clean))?;
                    results.push(apply_mod_file_state(&mods_dir, &p, "disable", dry_run)?);
                }
            }
        }

        if !dry_run {
            if let Some(ref mp) = manifest_path {
                save_manifest_auto(mp, &manifest)?;
            }
        }

        Ok(json!({
            "dry_run": dry_run,
            "results": results,
            "manifest_mod_count": manifest.mods.len(),
        }))
    }
}

fn apply_mod_file_state(
    mods_dir: &Path,
    target_path: &Path,
    state: &str,
    dry_run: bool,
) -> Result<Value> {
    let fname = target_path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid mod filename"))?;

    let (base_jar, disabled_jar) = if let Some(enabled_name) = fname.strip_suffix(".disabled") {
        (mods_dir.join(enabled_name), target_path.to_path_buf())
    } else {
        (
            target_path.to_path_buf(),
            mods_dir.join(format!("{fname}.disabled")),
        )
    };

    match state {
        "disable" => {
            let can_disable = base_jar.exists();
            if !dry_run && can_disable {
                std::fs::rename(&base_jar, &disabled_jar)?;
            }
            Ok(json!({
                "action": "disable",
                "dry_run": dry_run,
                "from": base_jar,
                "to": disabled_jar,
                "changed": can_disable,
            }))
        }
        "enable" => {
            let can_enable = disabled_jar.exists();
            if !dry_run && can_enable {
                std::fs::rename(&disabled_jar, &base_jar)?;
            }
            Ok(json!({
                "action": "enable",
                "dry_run": dry_run,
                "from": disabled_jar,
                "to": base_jar,
                "changed": can_enable,
            }))
        }
        "remove" => {
            let removed = if base_jar.exists() {
                if !dry_run {
                    std::fs::remove_file(&base_jar)?;
                }
                true
            } else if disabled_jar.exists() {
                if !dry_run {
                    std::fs::remove_file(&disabled_jar)?;
                }
                true
            } else {
                false
            };
            Ok(json!({
                "action": "remove",
                "dry_run": dry_run,
                "changed": removed,
            }))
        }
        other => anyhow::bail!("invalid mod state '{other}'; expected enable, disable, or remove"),
    }
}
