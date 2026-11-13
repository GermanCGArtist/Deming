//! CurseForge API v1 client for searching and resolving mods.

use std::path::{Path, PathBuf};

use deming_core::{DemingConfig, DemingError, LockedMod, ModLoader, Result};
use serde::Deserialize;

use crate::modrinth::ModSearchHit;
use crate::util::{build_http_client, download_file, store_blob_sha256};

/// Official CurseForge API v1 base URL.
pub const CURSEFORGE_API_BASE: &str = "https://api.curseforge.com/v1";

const MINECRAFT_GAME_ID: u32 = 432;
const MODS_CLASS_ID: u32 = 6;

/// Client for querying CurseForge API v1 using an `x-api-key`.
#[derive(Debug, Clone)]
pub struct CurseForgeClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
}

#[derive(Debug, Deserialize)]
struct CurseForgeSearchResponse {
    #[serde(default)]
    data: Vec<CurseForgeMod>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurseForgeMod {
    id: u64,
    name: String,
    slug: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    download_count: u64,
    #[serde(default)]
    categories: Vec<CurseForgeCategory>,
}

#[derive(Debug, Deserialize)]
struct CurseForgeCategory {
    name: String,
}

#[derive(Debug, Deserialize)]
struct CurseForgeFilesResponse {
    #[serde(default)]
    data: Vec<CurseForgeFile>,
}

#[derive(Debug, Deserialize)]
struct CurseForgeSingleFileResponse {
    data: CurseForgeFile,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurseForgeFile {
    id: u64,
    file_name: String,
    download_url: Option<String>,
    #[serde(default)]
    hashes: Vec<CurseForgeHash>,
    #[serde(default)]
    game_versions: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CurseForgeHash {
    value: String,
    algo: u32, // 1 = SHA1, 2 = MD5
}

impl CurseForgeClient {
    /// Builds a [`CurseForgeClient`] from [`DemingConfig::curseforge_api_key`],
    /// returning [`DemingError::Config`] if the key is missing or empty.
    pub fn from_config(config: &DemingConfig) -> Result<Self> {
        let key = config
            .curseforge_api_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                DemingError::Config(
                    "curseforge_api_key is not configured in DemingConfig".to_string(),
                )
            })?;
        Self::new(key)
    }

    /// Creates a new [`CurseForgeClient`] with the given API key.
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        Self::with_base_url(api_key, CURSEFORGE_API_BASE)
    }

    /// Creates a [`CurseForgeClient`] with a custom base URL (for testing).
    pub fn with_base_url(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Result<Self> {
        let key = api_key.into();
        if key.trim().is_empty() {
            return Err(DemingError::Config(
                "CurseForge API key cannot be empty".to_string(),
            ));
        }
        Ok(Self {
            http: build_http_client()?,
            api_key: key,
            base_url: base_url.into().trim_end_matches('/').to_string(),
        })
    }

    /// Searches CurseForge Minecraft mods matching `query` and optional `mc_version` / `loader`.
    pub async fn search_mods(
        &self,
        query: &str,
        mc_version: Option<&str>,
        loader: Option<ModLoader>,
        limit: usize,
    ) -> Result<Vec<ModSearchHit>> {
        let game_id = MINECRAFT_GAME_ID.to_string();
        let class_id = MODS_CLASS_ID.to_string();
        let page_size = limit.clamp(1, 50).to_string();
        let mut params: Vec<(&str, String)> = vec![
            ("gameId", game_id),
            ("classId", class_id),
            ("searchFilter", query.to_string()),
            ("pageSize", page_size),
        ];
        if let Some(ver) = mc_version {
            params.push(("gameVersion", ver.to_string()));
        }
        if let Some(ldr_type) = loader.and_then(curseforge_mod_loader_type) {
            params.push(("modLoaderType", ldr_type.to_string()));
        }

        let resp = self
            .http
            .get(format!("{}/mods/search", self.base_url))
            .header("x-api-key", &self.api_key)
            .query(&params)
            .send()
            .await
            .map_err(|e| DemingError::Network(format!("CurseForge search failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(DemingError::Network(format!(
                "CurseForge search returned HTTP {status}"
            )));
        }

        let parsed: CurseForgeSearchResponse = resp
            .json()
            .await
            .map_err(|e| DemingError::Network(format!("failed to decode CurseForge search: {e}")))?;

        Ok(parsed
            .data
            .into_iter()
            .map(|m| ModSearchHit {
                id: m.id.to_string(),
                slug: m.slug,
                title: m.name,
                description: m.summary,
                source: "curseforge".to_string(),
                downloads: m.download_count,
                categories: m.categories.into_iter().map(|c| c.name).collect(),
            })
            .collect())
    }

    /// Resolves and downloads a CurseForge mod file into `<cache_dir>/blobs/sha256/<sha256>.jar`.
    pub async fn resolve_mod(
        &self,
        mod_id: u64,
        file_id: Option<u64>,
        mc_version: &str,
        loader: ModLoader,
        cache_dir: &Path,
    ) -> Result<(LockedMod, PathBuf)> {
        let file = match file_id {
            Some(fid) => {
                let url = format!("{}/mods/{mod_id}/files/{fid}", self.base_url);
                let resp = self
                    .http
                    .get(&url)
                    .header("x-api-key", &self.api_key)
                    .send()
                    .await
                    .map_err(|e| DemingError::Network(format!("GET {url} failed: {e}")))?;
                if !resp.status().is_success() {
                    return Err(DemingError::Network(format!(
                        "GET {url} returned HTTP {}",
                        resp.status()
                    )));
                }
                let parsed: CurseForgeSingleFileResponse = resp.json().await.map_err(|e| {
                    DemingError::Network(format!("failed to decode CurseForge file: {e}"))
                })?;
                parsed.data
            }
            None => {
                let mut params: Vec<(&str, String)> =
                    vec![("gameVersion", mc_version.to_string()), ("pageSize", "20".to_string())];
                if let Some(ldr_type) = curseforge_mod_loader_type(loader) {
                    params.push(("modLoaderType", ldr_type.to_string()));
                }
                let url = format!("{}/mods/{mod_id}/files", self.base_url);
                let resp = self
                    .http
                    .get(&url)
                    .header("x-api-key", &self.api_key)
                    .query(&params)
                    .send()
                    .await
                    .map_err(|e| DemingError::Network(format!("GET {url} failed: {e}")))?;
                if !resp.status().is_success() {
                    return Err(DemingError::Network(format!(
                        "GET {url} returned HTTP {}",
                        resp.status()
                    )));
                }
                let parsed: CurseForgeFilesResponse = resp.json().await.map_err(|e| {
                    DemingError::Network(format!("failed to decode CurseForge files: {e}"))
                })?;
                parsed
                    .data
                    .into_iter()
                    .find(|f| {
                        f.game_versions.is_empty()
                            || f.game_versions.iter().any(|v| v == mc_version)
                    })
                    .ok_or_else(|| {
                        DemingError::InvalidSpec(format!(
                            "no CurseForge file found for mod {mod_id} ({mc_version})"
                        ))
                    })?
            }
        };

        let download_url = file.download_url.ok_or_else(|| {
            DemingError::Network(format!(
                "CurseForge mod {mod_id} file {} has no downloadUrl (third-party distribution disabled)",
                file.id
            ))
        })?;
        let sha1 = file
            .hashes
            .iter()
            .find(|h| h.algo == 1)
            .map(|h| h.value.as_str());

        let tmp_dest = cache_dir
            .join("blobs")
            .join("tmp")
            .join(format!("cf-{mod_id}-{}-{}", file.id, file.file_name));
        let (_actual_sha1, sha256) =
            download_file(&self.http, &download_url, &tmp_dest, sha1, None, false).await?;

        let blob_path = store_blob_sha256(cache_dir, &tmp_dest, &sha256).await?;
        let locked = LockedMod {
            id: mod_id.to_string(),
            name: file.file_name.clone(),
            source: "curseforge".to_string(),
            version_id: file.id.to_string(),
            filename: file.file_name,
            url: download_url,
            sha256,
            sha512: None,
        };
        Ok((locked, blob_path))
    }
}

/// Maps [`ModLoader`] to CurseForge's numeric `modLoaderType`.
pub fn curseforge_mod_loader_type(loader: ModLoader) -> Option<u32> {
    match loader {
        ModLoader::Vanilla => None,
        ModLoader::Forge => Some(1),
        ModLoader::Fabric => Some(4),
        ModLoader::Quilt => Some(5),
        ModLoader::NeoForge => Some(6),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_config_errors_when_api_key_missing() {
        let cfg = DemingConfig::default();
        let err = CurseForgeClient::from_config(&cfg).unwrap_err();
        assert!(matches!(err, DemingError::Config(_)));
    }

    #[test]
    fn parse_curseforge_search_fixture() {
        let fixture = r#"{
          "data": [
            {
              "id": 238222,
              "name": "Just Enough Items (JEI)",
              "slug": "jei",
              "summary": "View Items and Recipes",
              "downloadCount": 300000000,
              "categories": [{ "name": "API and Library" }]
            }
          ]
        }"#;
        let parsed: CurseForgeSearchResponse = serde_json::from_str(fixture).unwrap();
        assert_eq!(parsed.data.len(), 1);
        assert_eq!(parsed.data[0].id, 238222);
        assert_eq!(parsed.data[0].slug, "jei");
    }
}
