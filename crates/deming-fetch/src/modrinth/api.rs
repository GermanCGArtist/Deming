//! Modrinth API v2 client, schemas, and search/version metadata resolution.

use deming_core::{DemingError, ModLoader, Result};
use serde::{Deserialize, Serialize};

use crate::util::{build_http_client, fetch_json};

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
}
