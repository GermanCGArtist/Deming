//! Mod manifest (`deming.toml` / `mods.json`) and lockfile (`deming.lock`) schemas.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{DemingError, Result};
use crate::spec::{ModLoader, ModSource};

/// Current schema version for [`DemingLockfile`].
pub const LOCKFILE_SCHEMA_VERSION: u32 = 1;

/// A single resolved and pinned mod entry in [`DemingLockfile`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedMod {
    /// Project identifier or slug.
    pub id: String,
    /// Human-readable mod name.
    pub name: String,
    /// Source provider (e.g., `"modrinth"`, `"curseforge"`, `"local"`).
    pub source: String,
    /// Resolved provider version identifier.
    pub version_id: String,
    /// Target `.jar` filename in `mods/`.
    pub filename: String,
    /// Download URL or `file://` URI.
    pub url: String,
    /// Hex-encoded SHA-256 digest of the artifact.
    pub sha256: String,
    /// Optional hex-encoded SHA-512 digest of the artifact.
    #[serde(default)]
    pub sha512: Option<String>,
}

/// Reproducible mod lockfile (`deming.lock`) pinning exact resolved mod versions and hashes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DemingLockfile {
    /// Lockfile format version.
    pub schema_version: u32,
    /// Target Minecraft version.
    pub mc_version: String,
    /// Target mod loader.
    pub loader: ModLoader,
    /// Optional pinned mod loader version.
    #[serde(default)]
    pub loader_version: Option<String>,
    /// Resolved and pinned mod entries.
    #[serde(default)]
    pub mods: Vec<LockedMod>,
}

impl DemingLockfile {
    /// Creates a new lockfile with [`LOCKFILE_SCHEMA_VERSION`].
    pub fn new(
        mc_version: impl Into<String>,
        loader: ModLoader,
        loader_version: Option<String>,
        mods: Vec<LockedMod>,
    ) -> Self {
        Self {
            schema_version: LOCKFILE_SCHEMA_VERSION,
            mc_version: mc_version.into(),
            loader,
            loader_version,
            mods,
        }
    }

    /// Loads a TOML-encoded lockfile from `path`.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let raw = fs::read_to_string(path.as_ref())?;
        toml::from_str(&raw).map_err(|e| DemingError::Config(format!("invalid lockfile TOML: {e}")))
    }

    /// Saves this lockfile as pretty-printed TOML to `path`, creating parent directories if needed.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let body = toml::to_string_pretty(self)
            .map_err(|e| DemingError::Config(format!("failed to serialize lockfile TOML: {e}")))?;
        fs::write(path, body)?;
        Ok(())
    }
}

/// Declarative mod manifest (`deming.toml` or `mods.json`) describing desired mods for a target MC + loader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModsManifest {
    /// Target Minecraft version.
    pub mc_version: String,
    /// Target mod loader.
    pub loader: ModLoader,
    /// Optional pinned mod loader version.
    #[serde(default)]
    pub loader_version: Option<String>,
    /// Mod sources to resolve.
    #[serde(default)]
    pub mods: Vec<ModSource>,
}

impl ModsManifest {
    /// Loads a [`ModsManifest`] from `path`, detecting JSON vs TOML by file extension (with fallback).
    pub fn load_auto(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = fs::read_to_string(path)?;
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase());

        match ext.as_deref() {
            Some("json") => Ok(serde_json::from_str(&raw)?),
            Some("toml") => toml::from_str(&raw)
                .map_err(|e| DemingError::Config(format!("invalid manifest TOML: {e}"))),
            _ => {
                if let Ok(manifest) = toml::from_str::<Self>(&raw) {
                    Ok(manifest)
                } else {
                    Ok(serde_json::from_str(&raw)?)
                }
            }
        }
    }

    /// Saves this manifest as pretty-printed TOML to `path`.
    pub fn save_toml(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let body = toml::to_string_pretty(self)
            .map_err(|e| DemingError::Config(format!("failed to serialize manifest TOML: {e}")))?;
        fs::write(path, body)?;
        Ok(())
    }

    /// Saves this manifest as pretty-printed JSON to `path`.
    pub fn save_json(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let body = serde_json::to_string_pretty(self)?;
        fs::write(path, body)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn lockfile_toml_round_trip() {
        let temp_dir =
            std::env::temp_dir().join(format!("deming-lockfile-test-{}", uuid::Uuid::new_v4()));
        let lock_path = temp_dir.join("deming.lock");

        let lockfile = DemingLockfile::new(
            "1.21.4",
            ModLoader::Fabric,
            Some("0.16.10".to_string()),
            vec![LockedMod {
                id: "fabric-api".to_string(),
                name: "Fabric API".to_string(),
                source: "modrinth".to_string(),
                version_id: "ver-123".to_string(),
                filename: "fabric-api-0.115.0+1.21.4.jar".to_string(),
                url: "https://cdn.modrinth.com/data/P7dR8mSH/versions/ver-123/fabric-api.jar"
                    .to_string(),
                sha256: "deadbeef".repeat(8),
                sha512: Some("cafebabe".repeat(16)),
            }],
        );

        lockfile.save(&lock_path).unwrap();
        let loaded = DemingLockfile::load(&lock_path).unwrap();
        assert_eq!(loaded, lockfile);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn mods_manifest_toml_and_json_round_trip() {
        let temp_dir =
            std::env::temp_dir().join(format!("deming-manifest-test-{}", uuid::Uuid::new_v4()));
        let toml_path = temp_dir.join("deming.toml");
        let json_path = temp_dir.join("mods.json");

        let manifest = ModsManifest {
            mc_version: "26.1.1".to_string(),
            loader: ModLoader::Fabric,
            loader_version: Some("0.20.1".to_string()),
            mods: vec![
                ModSource::LocalPath(PathBuf::from("mods/local-mod.jar")),
                ModSource::Modrinth {
                    project_id_or_slug: "fabric-api".to_string(),
                    version_id: None,
                },
            ],
        };

        manifest.save_toml(&toml_path).unwrap();
        manifest.save_json(&json_path).unwrap();

        let from_toml = ModsManifest::load_auto(&toml_path).unwrap();
        let from_json = ModsManifest::load_auto(&json_path).unwrap();
        assert_eq!(from_toml, manifest);
        assert_eq!(from_json, manifest);

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
