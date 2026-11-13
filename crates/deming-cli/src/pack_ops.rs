//! Package management helpers for searching, adding, and syncing mods.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use deming_core::{DemingConfig, ModLoader, ModSource, ModsManifest};
use deming_fetch::{
    sync_instance_mods, CurseForgeClient, ModSearchHit, ModrinthClient, ProgressCallback, SyncPlan,
};

/// Supported mod search providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSource {
    Modrinth,
    CurseForge,
}

/// Searches Modrinth or CurseForge for mods matching `query`.
pub async fn search_mods(
    config: &DemingConfig,
    query: &str,
    mc_version: Option<&str>,
    loader: Option<ModLoader>,
    source: SearchSource,
    limit: usize,
) -> Result<Vec<ModSearchHit>> {
    match source {
        SearchSource::Modrinth => {
            let client = ModrinthClient::new()?;
            Ok(client
                .search_projects(query, mc_version, loader, limit)
                .await?)
        }
        SearchSource::CurseForge => {
            let client = CurseForgeClient::from_config(config)?;
            Ok(client
                .search_mods(query, mc_version, loader, limit)
                .await?)
        }
    }
}

/// Adds a Modrinth mod slug/ID to a `deming.toml` or `mods.json` manifest (creating it if needed).
pub fn add_mod_to_manifest(
    manifest_path: &Path,
    slug: &str,
    mc_version: Option<&str>,
    loader: Option<ModLoader>,
    version_id: Option<&str>,
) -> Result<ModsManifest> {
    let mut manifest = if manifest_path.exists() {
        ModsManifest::load_auto(manifest_path)
            .with_context(|| format!("failed to load manifest {}", manifest_path.display()))?
    } else {
        ModsManifest {
            mc_version: mc_version.unwrap_or("1.21.4").to_string(),
            loader: loader.unwrap_or(ModLoader::Fabric),
            loader_version: None,
            mods: Vec::new(),
        }
    };

    if let Some(mc) = mc_version {
        manifest.mc_version = mc.to_string();
    }
    if let Some(l) = loader {
        manifest.loader = l;
    }

    let already_exists = manifest.mods.iter().any(|m| match m {
        ModSource::Modrinth {
            project_id_or_slug, ..
        } => project_id_or_slug.eq_ignore_ascii_case(slug),
        _ => false,
    });

    if !already_exists {
        manifest.mods.push(ModSource::Modrinth {
            project_id_or_slug: slug.to_string(),
            version_id: version_id.map(|s| s.to_string()),
        });
    }

    save_manifest_auto(manifest_path, &manifest)?;
    Ok(manifest)
}

/// Saves a [`ModsManifest`] as JSON or TOML based on file extension (defaults to TOML).
pub fn save_manifest_auto(path: &Path, manifest: &ModsManifest) -> Result<()> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    if ext.as_deref() == Some("json") {
        manifest.save_json(path)?;
    } else {
        manifest.save_toml(path)?;
    }
    Ok(())
}

/// Loads `manifest_path` and runs [`sync_instance_mods`] against `instance_dir`.
pub async fn sync_manifest(
    config: &DemingConfig,
    manifest_path: &Path,
    instance_dir: &Path,
    dry_run: bool,
    progress: Option<ProgressCallback>,
) -> Result<SyncPlan> {
    let manifest = ModsManifest::load_auto(manifest_path)
        .with_context(|| format!("failed to load manifest {}", manifest_path.display()))?;
    let plan = sync_instance_mods(config, &manifest, instance_dir, dry_run, progress).await?;
    Ok(plan)
}

/// Resolves the default manifest path (`deming.toml` if present, else `mods.json` if present, else `deming.toml`).
pub fn resolve_default_manifest_path(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(p) = explicit {
        return p;
    }
    let toml_path = PathBuf::from("deming.toml");
    if toml_path.exists() {
        return toml_path;
    }
    let json_path = PathBuf::from("mods.json");
    if json_path.exists() {
        return json_path;
    }
    toml_path
}
