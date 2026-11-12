//! Mod resolution for `ModSource::LocalPath` and `ModSource::Modrinth`.

use std::path::{Path, PathBuf};

use deming_core::{DemingError, ModLoader, ModSource, Result};
use serde::{Deserialize, Serialize};

use crate::util::{build_http_client, compute_sha256, download_file, fetch_json};

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
    tokio::fs::create_dir_all(instance_mods_dir).await?;
    let mut resolved = Vec::new();

    for source in mods {
        match source {
            ModSource::LocalPath(path) => {
                resolved.extend(resolve_local_mod_path(path, instance_mods_dir).await?);
            }
            ModSource::Modrinth {
                project_id_or_slug,
                version_id,
            } => {
                let item = resolve_modrinth_mod(
                    client,
                    project_id_or_slug,
                    version_id.as_deref(),
                    mc_version,
                    loader,
                    cache_dir,
                    instance_mods_dir,
                )
                .await?;
                resolved.push(item);
            }
        }
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

async fn resolve_modrinth_mod(
    client: &ModrinthClient,
    project_id_or_slug: &str,
    version_id: Option<&str>,
    mc_version: &str,
    loader: ModLoader,
    cache_dir: &Path,
    instance_mods_dir: &Path,
) -> Result<ResolvedMod> {
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
        .ok_or_else(|| {
            DemingError::Network(format!(
                "Modrinth version '{}' has no downloadable files",
                version.id
            ))
        })?;

    // Download to a temporary staging path if SHA-256 isn't known upfront, then move into
    // `<cache_dir>/mods/<sha256>/<filename>`.
    let tmp_dest = cache_dir
        .join("mods")
        .join("tmp")
        .join(&file.filename);
    let (_sha1, sha256) = download_file(
        client.http(),
        &file.url,
        &tmp_dest,
        file.hashes.sha1.as_deref(),
        file.hashes.sha256.as_deref(),
        false,
    )
    .await?;

    let cached_path = mod_cache_path(cache_dir, &sha256, &file.filename);
    if !cached_path.is_file() {
        if let Some(parent) = cached_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::rename(&tmp_dest, &cached_path).await?;
    } else {
        let _ = tokio::fs::remove_file(&tmp_dest).await;
    }

    let dest = instance_mods_dir.join(&file.filename);
    tokio::fs::copy(&cached_path, &dest).await?;

    Ok(ResolvedMod {
        filename: file.filename.clone(),
        destination_path: dest,
        sha256,
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
}
