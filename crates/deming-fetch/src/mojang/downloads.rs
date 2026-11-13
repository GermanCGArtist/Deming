//! Mojang client/server JAR, version JSON, and asset download orchestration.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use deming_core::{DemingError, Result};
use futures_util::stream::{self, StreamExt};

use super::libraries::download_allowed_libraries;
use super::manifest::{
    asset_index_path, client_jar_path, server_jar_path, version_json_path, AssetIndexJson,
    VersionJson, VersionManifest, PISTON_META_MANIFEST_URL, RESOURCES_BASE_URL,
};
use crate::util::{build_http_client, download_file, fetch_json, verify_sha1};
use crate::{report_progress, ProgressCallback};

/// Resolved vanilla client installation paths under `cache_dir`.
#[derive(Debug, Clone)]
pub struct ResolvedVanilla {
    pub version_json: VersionJson,
    pub client_jar: PathBuf,
    pub library_paths: Vec<PathBuf>,
    pub asset_index_id: String,
    pub assets_dir: PathBuf,
    pub natives_dir: PathBuf,
}

/// Client for fetching Mojang Piston Meta manifests, version JSONs, libraries, and assets.
#[derive(Debug, Clone)]
pub struct MojangClient {
    http: reqwest::Client,
    manifest_url: String,
    resources_base_url: String,
}

impl MojangClient {
    /// Creates a new [`MojangClient`] pointing at the official Mojang endpoints.
    pub fn new() -> Result<Self> {
        Ok(Self {
            http: build_http_client()?,
            manifest_url: PISTON_META_MANIFEST_URL.to_string(),
            resources_base_url: RESOURCES_BASE_URL.to_string(),
        })
    }

    /// Creates a [`MojangClient`] with custom manifest and resources URLs (for testing).
    pub fn with_urls(
        manifest_url: impl Into<String>,
        resources_base_url: impl Into<String>,
    ) -> Result<Self> {
        Ok(Self {
            http: build_http_client()?,
            manifest_url: manifest_url.into(),
            resources_base_url: resources_base_url.into().trim_end_matches('/').to_string(),
        })
    }

    /// Returns a reference to the underlying `reqwest::Client`.
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Fetches `version_manifest_v2.json`.
    pub async fn fetch_manifest(&self) -> Result<VersionManifest> {
        fetch_json(&self.http, &self.manifest_url).await
    }

    /// Resolves and caches `<cache_dir>/versions/<mc_version>/<mc_version>.json`.
    pub async fn fetch_version_json(
        &self,
        mc_version: &str,
        cache_dir: &Path,
    ) -> Result<VersionJson> {
        let json_path = version_json_path(cache_dir, mc_version);
        if json_path.is_file() {
            let raw = tokio::fs::read_to_string(&json_path).await?;
            if let Ok(parsed) = serde_json::from_str::<VersionJson>(&raw) {
                return Ok(parsed);
            }
        }

        let manifest = self.fetch_manifest().await?;
        let entry = manifest
            .versions
            .iter()
            .find(|v| v.id == mc_version)
            .ok_or_else(|| {
                DemingError::InvalidSpec(format!(
                    "Minecraft version '{mc_version}' not found in Mojang manifest"
                ))
            })?;

        download_file(
            &self.http,
            &entry.url,
            &json_path,
            Some(&entry.sha1),
            None,
            false,
        )
        .await?;

        let raw = tokio::fs::read_to_string(&json_path).await?;
        Ok(serde_json::from_str(&raw)?)
    }

    /// Ensures the vanilla client JAR, OS-filtered libraries, asset index, and asset objects
    /// are downloaded and SHA-1 verified into `cache_dir`.
    pub async fn ensure_client(
        &self,
        mc_version: &str,
        cache_dir: &Path,
    ) -> Result<ResolvedVanilla> {
        self.ensure_client_with_progress(mc_version, cache_dir, None)
            .await
    }

    /// Ensures the vanilla client JAR, OS-filtered libraries, asset index, and asset objects
    /// are downloaded and SHA-1 verified into `cache_dir`, emitting progress updates.
    pub async fn ensure_client_with_progress(
        &self,
        mc_version: &str,
        cache_dir: &Path,
        progress: Option<ProgressCallback>,
    ) -> Result<ResolvedVanilla> {
        report_progress(
            progress.as_ref(),
            0,
            1,
            &format!("Fetching version manifest for {mc_version}"),
        );
        let version_json = self.fetch_version_json(mc_version, cache_dir).await?;

        report_progress(
            progress.as_ref(),
            0,
            1,
            &format!("Downloading {mc_version} client JAR"),
        );
        let client_jar = client_jar_path(cache_dir, mc_version);
        download_file(
            &self.http,
            &version_json.downloads.client.url,
            &client_jar,
            Some(&version_json.downloads.client.sha1),
            None,
            false,
        )
        .await?;
        report_progress(
            progress.as_ref(),
            1,
            1,
            &format!("Client JAR {mc_version} ready"),
        );

        let natives_dir = cache_dir.join("natives").join(mc_version);
        let library_paths = download_allowed_libraries(
            &self.http,
            &version_json.libraries,
            cache_dir,
            &natives_dir,
            progress.as_ref(),
        )
        .await?;

        let assets_dir = cache_dir.join("assets");
        let index_path = asset_index_path(cache_dir, &version_json.asset_index.id);
        download_file(
            &self.http,
            &version_json.asset_index.url,
            &index_path,
            Some(&version_json.asset_index.sha1),
            None,
            false,
        )
        .await?;

        let index_raw = tokio::fs::read_to_string(&index_path).await?;
        let index_json: AssetIndexJson = serde_json::from_str(&index_raw)?;
        self.ensure_asset_objects(&assets_dir, &index_json, progress.as_ref())
            .await?;

        Ok(ResolvedVanilla {
            asset_index_id: version_json.asset_index.id.clone(),
            version_json,
            client_jar,
            library_paths,
            assets_dir,
            natives_dir,
        })
    }

    async fn ensure_asset_objects(
        &self,
        assets_dir: &Path,
        index_json: &AssetIndexJson,
        progress: Option<&ProgressCallback>,
    ) -> Result<()> {
        let mut to_download = Vec::new();
        for obj in index_json.objects.values() {
            if obj.hash.len() < 2 {
                continue;
            }
            let prefix = &obj.hash[..2];
            let dest = assets_dir.join("objects").join(prefix).join(&obj.hash);
            if !verify_sha1(&dest, &obj.hash)? {
                let url = format!("{}/{}/{}", self.resources_base_url, prefix, obj.hash);
                to_download.push((url, dest, obj.hash.clone()));
            }
        }

        let total = to_download.len() as u64;
        if total == 0 {
            report_progress(progress, 0, 0, "Assets up to date");
            return Ok(());
        }

        let completed = Arc::new(AtomicU64::new(0));
        let http = self.http.clone();
        let results: Vec<Result<()>> = stream::iter(to_download)
            .map(|(url, dest, sha1)| {
                let http = http.clone();
                let completed = Arc::clone(&completed);
                let progress = progress.cloned();
                async move {
                    download_file(&http, &url, &dest, Some(&sha1), None, false).await?;
                    let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                    if done == total || done.is_multiple_of(50) {
                        report_progress(
                            progress.as_ref(),
                            done,
                            total,
                            &format!("Downloading assets ({done}/{total})"),
                        );
                    }
                    Ok(())
                }
            })
            .buffer_unordered(16)
            .collect()
            .await;

        for res in results {
            res?;
        }
        Ok(())
    }

    /// Ensures the vanilla dedicated server JAR is downloaded and SHA-1 verified.
    pub async fn ensure_server(&self, mc_version: &str, cache_dir: &Path) -> Result<PathBuf> {
        self.ensure_server_with_progress(mc_version, cache_dir, None)
            .await
    }

    /// Ensures the vanilla dedicated server JAR is downloaded and SHA-1 verified,
    /// emitting progress updates.
    pub async fn ensure_server_with_progress(
        &self,
        mc_version: &str,
        cache_dir: &Path,
        progress: Option<ProgressCallback>,
    ) -> Result<PathBuf> {
        report_progress(
            progress.as_ref(),
            0,
            1,
            &format!("Fetching server metadata for {mc_version}"),
        );
        let version_json = self.fetch_version_json(mc_version, cache_dir).await?;
        let server_dl = version_json.downloads.server.ok_or_else(|| {
            DemingError::InvalidSpec(format!(
                "Minecraft version '{mc_version}' has no server download in Mojang manifest"
            ))
        })?;
        report_progress(
            progress.as_ref(),
            0,
            1,
            &format!("Downloading {mc_version} server JAR"),
        );
        let dest = server_jar_path(cache_dir, mc_version);
        download_file(
            &self.http,
            &server_dl.url,
            &dest,
            Some(&server_dl.sha1),
            None,
            false,
        )
        .await?;
        report_progress(
            progress.as_ref(),
            1,
            1,
            &format!("Server JAR {mc_version} ready"),
        );
        Ok(dest)
    }
}
