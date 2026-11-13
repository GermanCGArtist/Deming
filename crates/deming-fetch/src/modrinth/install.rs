//! Mod downloading, content-addressable blob caching, and instance `mods/` installation.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use deming_core::{DemingError, LockedMod, ModLoader, ModSource, Result};
use futures_util::stream::{self, StreamExt};

use super::api::{resolve_modrinth_metadata, ModrinthClient};
use crate::util::{compute_sha256, download_file, link_or_copy, store_blob_sha256};
use crate::{report_progress, ProgressCallback};

/// A resolved mod JAR placed in an instance's `mods/` directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMod {
    pub filename: String,
    pub destination_path: PathBuf,
    pub sha256: String,
}

/// Returns the cache path `<cache_dir>/mods/<sha256>/<filename>`.
pub fn mod_cache_path(cache_dir: &Path, sha256: &str, filename: &str) -> PathBuf {
    cache_dir.join("mods").join(sha256).join(filename)
}

/// Resolves all [`ModSource`] entries into `instance_mods_dir`, caching Modrinth downloads
/// by SHA-256 under `cache_dir/blobs/sha256/`.
pub async fn resolve_mods(
    client: &ModrinthClient,
    mods: &[ModSource],
    mc_version: &str,
    loader: ModLoader,
    cache_dir: &Path,
    instance_mods_dir: &Path,
) -> Result<Vec<ResolvedMod>> {
    resolve_mods_with_progress(
        client,
        mods,
        mc_version,
        loader,
        cache_dir,
        instance_mods_dir,
        None,
    )
    .await
}

/// Resolves all [`ModSource`] entries into `instance_mods_dir`, caching Modrinth downloads
/// by SHA-256 under `cache_dir/blobs/sha256/` (hardlinked into `instance_mods_dir`)
/// and emitting progress updates.
pub async fn resolve_mods_with_progress(
    client: &ModrinthClient,
    mods: &[ModSource],
    mc_version: &str,
    loader: ModLoader,
    cache_dir: &Path,
    instance_mods_dir: &Path,
    progress: Option<ProgressCallback>,
) -> Result<Vec<ResolvedMod>> {
    tokio::fs::create_dir_all(instance_mods_dir).await?;
    let total = mods.len() as u64;
    if total == 0 {
        return Ok(Vec::new());
    }
    report_progress(
        progress.as_ref(),
        0,
        total,
        &format!("Resolving {total} mod(s)"),
    );

    let completed = Arc::new(AtomicU64::new(0));
    let items: Vec<Result<Vec<ResolvedMod>>> = stream::iter(mods.iter().cloned())
        .map(|source| {
            let client = client.clone();
            let mc_version = mc_version.to_string();
            let cache_dir = cache_dir.to_path_buf();
            let instance_mods_dir = instance_mods_dir.to_path_buf();
            let completed = Arc::clone(&completed);
            let progress = progress.clone();
            async move {
                let batch = match source {
                    ModSource::LocalPath(path) => {
                        resolve_local_mod_path(&path, &instance_mods_dir).await?
                    }
                    ModSource::Modrinth {
                        project_id_or_slug,
                        version_id,
                    } => {
                        let item = resolve_modrinth_mod(
                            &client,
                            &project_id_or_slug,
                            version_id.as_deref(),
                            &mc_version,
                            loader,
                            &cache_dir,
                            &instance_mods_dir,
                        )
                        .await?;
                        vec![item]
                    }
                };
                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                report_progress(
                    progress.as_ref(),
                    done,
                    total,
                    &format!("Resolved mod {done}/{total}"),
                );
                Ok(batch)
            }
        })
        .buffer_unordered(8)
        .collect()
        .await;

    let mut resolved = Vec::new();
    for res in items {
        resolved.extend(res?);
    }
    Ok(resolved)
}

async fn resolve_local_mod_path(path: &Path, instance_mods_dir: &Path) -> Result<Vec<ResolvedMod>> {
    if !path.exists() {
        return Err(DemingError::InvalidSpec(format!(
            "local mod path does not exist: {}",
            path.display()
        )));
    }

    let mut out = Vec::new();
    if path.is_file() {
        if path.extension().and_then(|e| e.to_str()) != Some("jar") {
            return Err(DemingError::InvalidSpec(format!(
                "local mod file is not a .jar: {}",
                path.display()
            )));
        }
        out.push(copy_jar_to_mods_dir(path, instance_mods_dir).await?);
    } else if path.is_dir() {
        let mut entries = tokio::fs::read_dir(path).await?;
        while let Some(entry) = entries.next_entry().await? {
            let entry_path = entry.path();
            if entry_path.is_file()
                && entry_path.extension().and_then(|e| e.to_str()) == Some("jar")
            {
                out.push(copy_jar_to_mods_dir(&entry_path, instance_mods_dir).await?);
            }
        }
    }
    Ok(out)
}

async fn copy_jar_to_mods_dir(src: &Path, instance_mods_dir: &Path) -> Result<ResolvedMod> {
    let filename = src
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| DemingError::InvalidSpec(format!("invalid mod filename: {}", src.display())))?
        .to_string();
    let dest = instance_mods_dir.join(&filename);
    tokio::fs::copy(src, &dest).await?;
    let sha256 = compute_sha256(&dest)?;
    Ok(ResolvedMod {
        filename,
        destination_path: dest,
        sha256,
    })
}

/// Downloads a Modrinth mod into `<cache_dir>/blobs/sha256/<sha256>.jar` and returns `(LockedMod, blob_path)`.
pub(crate) async fn resolve_modrinth_to_blob(
    client: &ModrinthClient,
    project_id_or_slug: &str,
    version_id: Option<&str>,
    mc_version: &str,
    loader: ModLoader,
    cache_dir: &Path,
) -> Result<(LockedMod, PathBuf)> {
    let (version, file) =
        resolve_modrinth_metadata(client, project_id_or_slug, version_id, mc_version, loader)
            .await?;

    let tmp_dest = cache_dir
        .join("blobs")
        .join("tmp")
        .join(format!("{}-{}", version.id, file.filename));
    let (_sha1, sha256) = download_file(
        client.http(),
        &file.url,
        &tmp_dest,
        file.hashes.sha1.as_deref(),
        file.hashes.sha256.as_deref(),
        false,
    )
    .await?;

    let blob_path = store_blob_sha256(cache_dir, &tmp_dest, &sha256).await?;
    let locked = LockedMod {
        id: project_id_or_slug.to_string(),
        name: project_id_or_slug.to_string(),
        source: "modrinth".to_string(),
        version_id: version.id,
        filename: file.filename,
        url: file.url,
        sha256,
        sha512: file.hashes.sha512,
    };
    Ok((locked, blob_path))
}

async fn resolve_modrinth_mod(
    client: &ModrinthClient,
    project_id_or_slug: &str,
    version_id: Option<&str>,
    mc_version: &str,
    loader: ModLoader,
    cache_dir: &Path,
    instance_mods_dir: &Path,
) -> Result<ResolvedMod> {
    let (locked, blob_path) = resolve_modrinth_to_blob(
        client,
        project_id_or_slug,
        version_id,
        mc_version,
        loader,
        cache_dir,
    )
    .await?;

    let dest = instance_mods_dir.join(&locked.filename);
    link_or_copy(&blob_path, &dest)?;

    Ok(ResolvedMod {
        filename: locked.filename,
        destination_path: dest,
        sha256: locked.sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_mod_copying_file_and_dir() {
        let temp = std::env::temp_dir().join(format!(
            "deming-fetch-mod-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let src_dir = temp.join("src_mods");
        let cache_dir = temp.join("cache");
        let inst_mods = temp.join("instance").join("mods");
        std::fs::create_dir_all(&src_dir).unwrap();

        let single_jar = temp.join("standalone.jar");
        std::fs::write(&single_jar, b"fake-jar-1").unwrap();
        std::fs::write(src_dir.join("dir-mod.jar"), b"fake-jar-2").unwrap();
        std::fs::write(src_dir.join("readme.txt"), b"ignore-me").unwrap();

        let client = ModrinthClient::new().unwrap();
        let resolved = resolve_mods(
            &client,
            &[
                ModSource::LocalPath(single_jar),
                ModSource::LocalPath(src_dir),
            ],
            "1.21.4",
            ModLoader::Fabric,
            &cache_dir,
            &inst_mods,
        )
        .await
        .unwrap();

        assert_eq!(resolved.len(), 2);
        assert!(inst_mods.join("standalone.jar").is_file());
        assert!(inst_mods.join("dir-mod.jar").is_file());
        assert!(!inst_mods.join("readme.txt").exists());

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn mod_cache_path_format() {
        let cache = Path::new("C:/deming/cache");
        assert_eq!(
            mod_cache_path(cache, "deadbeef", "fabric-api.jar"),
            PathBuf::from("C:/deming/cache/mods/deadbeef/fabric-api.jar")
        );
    }

    #[tokio::test]
    async fn resolve_mods_with_progress_reports_updates() {
        use std::sync::{Arc, Mutex};

        let temp = std::env::temp_dir().join(format!(
            "deming-fetch-progress-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cache_dir = temp.join("cache");
        let inst_mods = temp.join("instance").join("mods");
        std::fs::create_dir_all(&temp).unwrap();
        let mod_jar = temp.join("test-mod.jar");
        std::fs::write(&mod_jar, b"fake-mod-jar").unwrap();

        let events: Arc<Mutex<Vec<(u64, u64, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);
        let cb: ProgressCallback = Arc::new(move |completed, total, msg| {
            events_clone
                .lock()
                .unwrap()
                .push((completed, total, msg.to_string()));
        });

        let client = ModrinthClient::new().unwrap();
        let resolved = resolve_mods_with_progress(
            &client,
            &[ModSource::LocalPath(mod_jar)],
            "1.21.4",
            ModLoader::Fabric,
            &cache_dir,
            &inst_mods,
            Some(cb),
        )
        .await
        .unwrap();

        assert_eq!(resolved.len(), 1);
        let recorded = events.lock().unwrap();
        assert!(recorded.len() >= 2);
        assert_eq!(recorded.last().unwrap().0, 1);
        assert_eq!(recorded.last().unwrap().1, 1);

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn blob_store_and_hardlink_populates_mods_dir() {
        let temp = std::env::temp_dir().join(format!(
            "deming-blob-hardlink-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cache_dir = temp.join("cache");
        let staging = temp.join("staging.jar");
        let dest = temp.join("instance").join("mods").join("fabric-api.jar");
        std::fs::create_dir_all(&temp).unwrap();
        std::fs::write(&staging, b"blob-content-bytes").unwrap();
        let sha256 = compute_sha256(&staging).unwrap();

        let blob = store_blob_sha256(&cache_dir, &staging, &sha256).await.unwrap();
        assert_eq!(
            blob,
            crate::util::blob_sha256_path(&cache_dir, &sha256)
        );
        assert!(blob.is_file());

        link_or_copy(&blob, &dest).unwrap();
        assert!(dest.is_file());
        assert_eq!(std::fs::read(&dest).unwrap(), b"blob-content-bytes");

        let _ = std::fs::remove_dir_all(&temp);
    }
}
