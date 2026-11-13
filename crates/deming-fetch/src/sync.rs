//! Atomic instance mod syncing and `deming.lock` diffing/staging.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use deming_core::{
    DemingConfig, DemingError, DemingLockfile, LockedMod, ModLoader, ModSource, ModsManifest,
    Result,
};
use futures_util::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};

use crate::modrinth::{
    resolve_modrinth_metadata, resolve_modrinth_to_blob, ModrinthClient,
};
use crate::util::{compute_sha256, link_or_copy, store_blob_sha256, verify_sha256};
use crate::{report_progress, ProgressCallback};

/// Computed diff and resulting lockfile for [`sync_instance_mods`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPlan {
    pub added: Vec<LockedMod>,
    pub updated: Vec<(LockedMod, LockedMod)>,
    pub removed: Vec<LockedMod>,
    pub unchanged: Vec<LockedMod>,
    pub lockfile: DemingLockfile,
}

/// Alias for [`SyncPlan`].
pub type SyncDiff = SyncPlan;

struct StagedEntry {
    locked: LockedMod,
    blob_path: Option<PathBuf>,
}

/// Synchronizes `instance_dir/mods/` and `instance_dir/deming.lock` with `manifest`.
///
/// When `dry_run` is `true`, resolves the diff and returns [`SyncPlan`] without modifying
/// `instance_dir/mods/` or writing `deming.lock`.
/// When `dry_run` is `false`, stages mods in `instance_dir/mods.staging/`, verifies SHA-256
/// checksums, writes `deming.lock`, and atomically swaps `mods.staging/` into `mods/`.
pub async fn sync_instance_mods(
    config: &DemingConfig,
    manifest: &ModsManifest,
    instance_dir: &Path,
    dry_run: bool,
    progress: Option<ProgressCallback>,
) -> Result<SyncPlan> {
    let lockfile_path = instance_dir.join("deming.lock");
    let previous = if lockfile_path.is_file() {
        Some(DemingLockfile::load(&lockfile_path)?)
    } else {
        None
    };

    let client = ModrinthClient::new()?;
    let cache_dir = config.cache_dir();
    let total = manifest.mods.len() as u64;
    let completed = Arc::new(AtomicU64::new(0));

    let results: Vec<Result<Vec<StagedEntry>>> = stream::iter(manifest.mods.iter().cloned())
        .map(|source| {
            let client = client.clone();
            let cache_dir = cache_dir.clone();
            let mc_version = manifest.mc_version.clone();
            let loader = manifest.loader;
            let completed = Arc::clone(&completed);
            let progress = progress.clone();
            async move {
                let entries = resolve_source_for_sync(
                    &client,
                    &source,
                    &mc_version,
                    loader,
                    &cache_dir,
                    dry_run,
                )
                .await?;
                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                report_progress(
                    progress.as_ref(),
                    done,
                    total.max(1),
                    &format!("Resolved sync entry {done}/{}", total.max(1)),
                );
                Ok(entries)
            }
        })
        .buffer_unordered(8)
        .collect()
        .await;

    let mut staged_entries = Vec::new();
    for res in results {
        staged_entries.extend(res?);
    }

    let next_mods: Vec<LockedMod> = staged_entries.iter().map(|e| e.locked.clone()).collect();
    let new_lockfile = DemingLockfile::new(
        manifest.mc_version.clone(),
        manifest.loader,
        manifest.loader_version.clone(),
        next_mods.clone(),
    );

    let diff = compute_diff(previous.as_ref(), &next_mods);
    let plan = SyncPlan {
        added: diff.added,
        updated: diff.updated,
        removed: diff.removed,
        unchanged: diff.unchanged,
        lockfile: new_lockfile.clone(),
    };

    if dry_run {
        return Ok(plan);
    }

    tokio::fs::create_dir_all(instance_dir).await?;
    let staging_dir = instance_dir.join("mods.staging");
    if staging_dir.exists() {
        tokio::fs::remove_dir_all(&staging_dir).await?;
    }
    tokio::fs::create_dir_all(&staging_dir).await?;

    for entry in &staged_entries {
        let blob_path = entry.blob_path.as_ref().ok_or_else(|| {
            DemingError::Other(format!(
                "missing blob path for staged mod '{}'",
                entry.locked.filename
            ))
        })?;
        let dest = staging_dir.join(&entry.locked.filename);
        link_or_copy(blob_path, &dest)?;
        if !verify_sha256(&dest, &entry.locked.sha256)? {
            let _ = tokio::fs::remove_dir_all(&staging_dir).await;
            return Err(DemingError::Network(format!(
                "SHA-256 verification failed for staged mod '{}'",
                entry.locked.filename
            )));
        }
    }

    new_lockfile.save(&lockfile_path)?;

    let mods_dir = instance_dir.join("mods");
    let backup_dir = instance_dir.join("mods.backup");
    if backup_dir.exists() {
        tokio::fs::remove_dir_all(&backup_dir).await?;
    }
    if mods_dir.exists() {
        tokio::fs::rename(&mods_dir, &backup_dir).await?;
    }
    tokio::fs::rename(&staging_dir, &mods_dir).await?;
    if backup_dir.exists() {
        let _ = tokio::fs::remove_dir_all(&backup_dir).await;
    }

    report_progress(
        progress.as_ref(),
        total.max(1),
        total.max(1),
        &format!("Synced {} mod(s)", next_mods.len()),
    );

    Ok(plan)
}

async fn resolve_source_for_sync(
    client: &ModrinthClient,
    source: &ModSource,
    mc_version: &str,
    loader: ModLoader,
    cache_dir: &Path,
    dry_run: bool,
) -> Result<Vec<StagedEntry>> {
    match source {
        ModSource::LocalPath(path) => {
            if !path.exists() {
                return Err(DemingError::InvalidSpec(format!(
                    "local mod path does not exist: {}",
                    path.display()
                )));
            }
            let mut files = Vec::new();
            if path.is_file() {
                if path.extension().and_then(|e| e.to_str()) != Some("jar") {
                    return Err(DemingError::InvalidSpec(format!(
                        "local mod file is not a .jar: {}",
                        path.display()
                    )));
                }
                files.push(path.clone());
            } else if path.is_dir() {
                let mut entries = tokio::fs::read_dir(path).await?;
                while let Some(entry) = entries.next_entry().await? {
                    let p = entry.path();
                    if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("jar") {
                        files.push(p);
                    }
                }
            }

            let mut out = Vec::new();
            for file_path in files {
                let filename = file_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| {
                        DemingError::InvalidSpec(format!(
                            "invalid mod filename: {}",
                            file_path.display()
                        ))
                    })?
                    .to_string();
                let sha256 = compute_sha256(&file_path)?;
                let blob_path = if dry_run {
                    None
                } else {
                    let tmp = cache_dir
                        .join("blobs")
                        .join("tmp")
                        .join(format!("local-{sha256}-{filename}"));
                    if let Some(parent) = tmp.parent() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                    tokio::fs::copy(&file_path, &tmp).await?;
                    Some(store_blob_sha256(cache_dir, &tmp, &sha256).await?)
                };
                out.push(StagedEntry {
                    locked: LockedMod {
                        id: filename.clone(),
                        name: filename.clone(),
                        source: "local".to_string(),
                        version_id: sha256[..12.min(sha256.len())].to_string(),
                        filename,
                        url: format!("file://{}", file_path.display()),
                        sha256,
                        sha512: None,
                    },
                    blob_path,
                });
            }
            Ok(out)
        }
        ModSource::Modrinth {
            project_id_or_slug,
            version_id,
        } => {
            if dry_run {
                let (version, file) = resolve_modrinth_metadata(
                    client,
                    project_id_or_slug,
                    version_id.as_deref(),
                    mc_version,
                    loader,
                )
                .await?;
                Ok(vec![StagedEntry {
                    locked: LockedMod {
                        id: project_id_or_slug.clone(),
                        name: project_id_or_slug.clone(),
                        source: "modrinth".to_string(),
                        version_id: version.id,
                        filename: file.filename,
                        url: file.url,
                        sha256: file.hashes.sha256.unwrap_or_default(),
                        sha512: file.hashes.sha512,
                    },
                    blob_path: None,
                }])
            } else {
                let (locked, blob_path) = resolve_modrinth_to_blob(
                    client,
                    project_id_or_slug,
                    version_id.as_deref(),
                    mc_version,
                    loader,
                    cache_dir,
                )
                .await?;
                Ok(vec![StagedEntry {
                    locked,
                    blob_path: Some(blob_path),
                }])
            }
        }
    }
}

struct DiffResult {
    added: Vec<LockedMod>,
    updated: Vec<(LockedMod, LockedMod)>,
    removed: Vec<LockedMod>,
    unchanged: Vec<LockedMod>,
}

fn compute_diff(previous: Option<&DemingLockfile>, next_mods: &[LockedMod]) -> DiffResult {
    let mut prev_by_key: BTreeMap<(String, String), LockedMod> = BTreeMap::new();
    if let Some(prev) = previous {
        for m in &prev.mods {
            prev_by_key.insert((m.source.clone(), m.id.clone()), m.clone());
        }
    }

    let mut added = Vec::new();
    let mut updated = Vec::new();
    let mut unchanged = Vec::new();

    for next in next_mods {
        let key = (next.source.clone(), next.id.clone());
        match prev_by_key.remove(&key) {
            None => added.push(next.clone()),
            Some(old) => {
                if old.version_id == next.version_id
                    && (next.sha256.is_empty() || old.sha256 == next.sha256)
                {
                    unchanged.push(next.clone());
                } else {
                    updated.push((old, next.clone()));
                }
            }
        }
    }

    let removed: Vec<LockedMod> = prev_by_key.into_values().collect();
    DiffResult {
        added,
        updated,
        removed,
        unchanged,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sync_instance_mods_dry_run_and_atomic_swap() {
        let temp = std::env::temp_dir().join(format!(
            "deming-sync-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let ws = temp.join("ws");
        let instance_dir = temp.join("instance");
        let src_dir = temp.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();

        let mod_a = src_dir.join("mod-a.jar");
        let mod_b = src_dir.join("mod-b.jar");
        std::fs::write(&mod_a, b"mod-a-v1").unwrap();
        std::fs::write(&mod_b, b"mod-b-v1").unwrap();

        let config = DemingConfig {
            workspace_dir: ws,
            ..DemingConfig::default()
        };

        let manifest1 = ModsManifest {
            mc_version: "1.21.4".to_string(),
            loader: ModLoader::Fabric,
            loader_version: Some("0.16.10".to_string()),
            mods: vec![
                ModSource::LocalPath(mod_a.clone()),
                ModSource::LocalPath(mod_b.clone()),
            ],
        };

        // 1. Dry-run must not write mods/ or deming.lock
        let dry_plan = sync_instance_mods(&config, &manifest1, &instance_dir, true, None)
            .await
            .unwrap();
        assert_eq!(dry_plan.added.len(), 2);
        assert!(dry_plan.removed.is_empty());
        assert!(!instance_dir.join("mods").exists());
        assert!(!instance_dir.join("deming.lock").exists());

        // 2. Real sync stages, verifies, writes deming.lock, and populates mods/
        let plan1 = sync_instance_mods(&config, &manifest1, &instance_dir, false, None)
            .await
            .unwrap();
        assert_eq!(plan1.added.len(), 2);
        assert!(instance_dir.join("mods").join("mod-a.jar").is_file());
        assert!(instance_dir.join("mods").join("mod-b.jar").is_file());
        assert!(instance_dir.join("deming.lock").is_file());
        assert!(!instance_dir.join("mods.staging").exists());

        // 3. Second sync removing mod-b and updating mod-a
        std::fs::write(&mod_a, b"mod-a-v2").unwrap();
        let manifest2 = ModsManifest {
            mc_version: "1.21.4".to_string(),
            loader: ModLoader::Fabric,
            loader_version: Some("0.16.10".to_string()),
            mods: vec![ModSource::LocalPath(mod_a.clone())],
        };
        let plan2 = sync_instance_mods(&config, &manifest2, &instance_dir, false, None)
            .await
            .unwrap();
        assert_eq!(plan2.updated.len(), 1);
        assert_eq!(plan2.removed.len(), 1);
        assert_eq!(plan2.removed[0].filename, "mod-b.jar");
        assert!(instance_dir.join("mods").join("mod-a.jar").is_file());
        assert!(!instance_dir.join("mods").join("mod-b.jar").exists());

        let _ = std::fs::remove_dir_all(&temp);
    }
}
