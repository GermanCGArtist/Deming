//! Mod resolution for `ModSource::LocalPath` and `ModSource::Modrinth`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use deming_core::{DemingError, LockedMod, ModLoader, ModSource, Result};
use futures_util::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};

use crate::util::{
    build_http_client, compute_sha256, download_file, fetch_json, link_or_copy, store_blob_sha256,
};
use crate::{report_progress, ProgressCallback};

/// Official Modrinth API v2 base URL.
pub const MODRINTH_API_V2: &str = "https://api.modrinth.com/v2";

/// A Modrinth version entry returned by `/project/{id}/version` or `/version/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModrinthVersion {
    pub id: String,
    pub project_id: String,
    pub version_number: String,
    #[serde(default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub loaders: Vec<String>,
    #[serde(default)]
    pub files: Vec<ModrinthFile>,
}

/// A downloadable mod file inside a [`ModrinthVersion`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModrinthFile {
    pub hashes: ModrinthHashes,
    pub url: String,
    pub filename: String,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub size: u64,
}

/// File hashes provided by Modrinth.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModrinthHashes {
    pub sha512: Option<String>,
    pub sha1: Option<String>,
    pub sha256: Option<String>,
}

/// A resolved mod JAR placed in an instance's `mods/` directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMod {
    pub filename: String,
    pub destination_path: PathBuf,
    pub sha256: String,
}

/// Unified mod search hit returned by [`ModrinthClient::search_projects`] and [`crate::CurseForgeClient::search_mods`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModSearchHit {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub source: String,
    pub downloads: u64,
    #[serde(default)]
    pub categories: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ModrinthSearchResponse {
    #[serde(default)]
    hits: Vec<ModrinthSearchHitRaw>,
}

#[derive(Debug, Clone, Deserialize)]
struct ModrinthSearchHitRaw {
    project_id: String,
    slug: String,
    title: String,
    description: String,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    categories: Vec<String>,
}

/// Client for querying Modrinth API v2.
#[derive(Debug, Clone)]
pub struct ModrinthClient {
    http: reqwest::Client,
    base_url: String,
}

impl ModrinthClient {
    /// Creates a new [`ModrinthClient`] pointing at [`MODRINTH_API_V2`].
    pub fn new() -> Result<Self> {
        Ok(Self {
            http: build_http_client()?,
            base_url: MODRINTH_API_V2.to_string(),
        })
    }

    /// Creates a [`ModrinthClient`] with a custom base URL (for testing).
    pub fn with_base_url(base_url: impl Into<String>) -> Result<Self> {
        Ok(Self {
            http: build_http_client()?,
            base_url: base_url.into().trim_end_matches('/').to_string(),
        })
    }

    /// Queries Modrinth for versions of `project_id_or_slug` matching `loaders` and `game_versions`.
    pub async fn get_project_versions(
        &self,
        project_id_or_slug: &str,
        loaders: &[&str],
        game_versions: &[&str],
    ) -> Result<Vec<ModrinthVersion>> {
        let loaders_json = serde_json::to_string(loaders)?;
        let versions_json = serde_json::to_string(game_versions)?;
        let url = format!(
            "{}/project/{}/version?loaders={}&game_versions={}",
            self.base_url, project_id_or_slug, loaders_json, versions_json
        );
        fetch_json(&self.http, &url).await
    }

    /// Fetches a specific Modrinth version by its ID.
    pub async fn get_version(&self, version_id: &str) -> Result<ModrinthVersion> {
        let url = format!("{}/version/{}", self.base_url, version_id);
        fetch_json(&self.http, &url).await
    }

    /// Searches Modrinth projects matching `query` and optional `mc_version` / `loader` facets.
    pub async fn search_projects(
        &self,
        query: &str,
        mc_version: Option<&str>,
        loader: Option<ModLoader>,
        limit: usize,
    ) -> Result<Vec<ModSearchHit>> {
        let mut facets: Vec<Vec<String>> = vec![vec!["project_type:mod".to_string()]];
        if let Some(ver) = mc_version {
            facets.push(vec![format!("versions:{ver}")]);
        }
        if let Some(ldr) = loader.and_then(modrinth_loader_slug) {
            facets.push(vec![format!("categories:{ldr}")]);
        }
        let facets_json = serde_json::to_string(&facets)?;
        let resp = self
            .http
            .get(format!("{}/search", self.base_url))
            .query(&[
                ("query", query),
                ("limit", &limit.max(1).to_string()),
                ("facets", &facets_json),
            ])
            .send()
            .await
            .map_err(|e| DemingError::Network(format!("Modrinth search failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(DemingError::Network(format!(
                "Modrinth search returned HTTP {status}"
            )));
        }
        let body: ModrinthSearchResponse = resp
            .json()
            .await
            .map_err(|e| DemingError::Network(format!("failed to decode Modrinth search: {e}")))?;

        Ok(body
            .hits
            .into_iter()
            .map(|h| ModSearchHit {
                id: h.project_id,
                slug: h.slug,
                title: h.title,
                description: h.description,
                source: "modrinth".to_string(),
                downloads: h.downloads,
                categories: h.categories,
            })
            .collect())
    }

    /// Returns a reference to the underlying `reqwest::Client`.
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }
}

/// Returns the Modrinth loader slug for a [`ModLoader`].
pub fn modrinth_loader_slug(loader: ModLoader) -> Option<&'static str> {
    match loader {
        ModLoader::Vanilla => None,
        ModLoader::Fabric => Some("fabric"),
        ModLoader::Quilt => Some("quilt"),
        ModLoader::NeoForge => Some("neoforge"),
        ModLoader::Forge => Some("forge"),
    }
}

/// Returns the cache path `<cache_dir>/mods/<sha256>/<filename>`.
pub fn mod_cache_path(cache_dir: &Path, sha256: &str, filename: &str) -> PathBuf {
    cache_dir.join("mods").join(sha256).join(filename)
}

/// Resolves all [`ModSource`] entries into `instance_mods_dir`, caching Modrinth downloads
/// by SHA-256 under `cache_dir/mods/`.
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

/// Resolves Modrinth version and primary file metadata without downloading the JAR.
pub(crate) async fn resolve_modrinth_metadata(
    client: &ModrinthClient,
    project_id_or_slug: &str,
    version_id: Option<&str>,
    mc_version: &str,
    loader: ModLoader,
) -> Result<(ModrinthVersion, ModrinthFile)> {
    let version = match version_id {
        Some(vid) => client.get_version(vid).await?,
        None => {
            let loader_slug = modrinth_loader_slug(loader).ok_or_else(|| {
                DemingError::InvalidSpec(format!(
                    "cannot resolve Modrinth mod '{project_id_or_slug}' for Vanilla loader"
                ))
            })?;
            let versions = client
                .get_project_versions(project_id_or_slug, &[loader_slug], &[mc_version])
                .await?;
            versions.into_iter().next().ok_or_else(|| {
                DemingError::InvalidSpec(format!(
                    "no Modrinth version found for '{project_id_or_slug}' ({loader_slug} {mc_version})"
                ))
            })?
        }
    };

    let file = version
        .files
        .iter()
        .find(|f| f.primary)
        .or_else(|| version.files.iter().find(|f| f.filename.ends_with(".jar")))
        .or_else(|| version.files.first())
        .cloned()
        .ok_or_else(|| {
            DemingError::Network(format!(
                "Modrinth version '{}' has no downloadable files",
                version.id
            ))
        })?;

    Ok((version, file))
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
        .join(format!("{}-{}", version.id, &file.filename));
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

    #[test]
    fn parse_modrinth_version_fixture() {
        let fixture = r#"[
          {
            "id": "v123",
            "project_id": "P7dR8mSH",
            "version_number": "0.115.0+1.21.4",
            "game_versions": ["1.21.4"],
            "loaders": ["fabric"],
            "files": [
              {
                "hashes": {
                  "sha512": "abcd",
                  "sha1": "ef01"
                },
                "url": "https://cdn.modrinth.com/data/P7dR8mSH/versions/v123/fabric-api-0.115.0.jar",
                "filename": "fabric-api-0.115.0.jar",
                "primary": true,
                "size": 2048000
              }
            ]
          }
        ]"#;
        let versions: Vec<ModrinthVersion> = serde_json::from_str(fixture).unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].id, "v123");
        assert_eq!(versions[0].files[0].filename, "fabric-api-0.115.0.jar");
        assert!(versions[0].files[0].primary);
    }

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

    #[test]
    fn parse_modrinth_search_response_fixture() {
        let fixture = r#"{
          "hits": [
            {
              "project_id": "P7dR8mSH",
              "slug": "fabric-api",
              "title": "Fabric API",
              "description": "Lightweight and modular API",
              "downloads": 95000000,
              "categories": ["fabric", "library"]
            }
          ]
        }"#;
        let parsed: ModrinthSearchResponse = serde_json::from_str(fixture).unwrap();
        assert_eq!(parsed.hits.len(), 1);
        assert_eq!(parsed.hits[0].slug, "fabric-api");
        assert_eq!(parsed.hits[0].downloads, 95_000_000);
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
