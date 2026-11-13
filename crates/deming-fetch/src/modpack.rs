//! Modpack import and export for Modrinth (`.mrpack`) and CurseForge (`.zip`) formats.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use deming_core::{
    DemingError, DemingLockfile, LockedMod, ModLoader, ModSource, ModsManifest, Result,
};
use futures_util::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;

use crate::util::{
    build_http_client, download_file, link_or_copy, store_blob_sha256,
};
use crate::{report_progress, ProgressCallback};

/// `modrinth.index.json` schema inside a `.mrpack` archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MrpackIndex {
    pub format_version: u32,
    pub game: String,
    pub version_id: String,
    pub name: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub files: Vec<MrpackFile>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
}

/// A single mod/resource file entry inside [`MrpackIndex`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MrpackFile {
    pub path: String,
    pub hashes: MrpackHashes,
    #[serde(default)]
    pub downloads: Vec<String>,
    #[serde(default)]
    pub file_size: u64,
}

/// Hashes for a [`MrpackFile`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MrpackHashes {
    pub sha1: Option<String>,
    pub sha512: Option<String>,
}

/// CurseForge `manifest.json` schema inside a CurseForge modpack `.zip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeManifest {
    pub minecraft: CurseForgeManifestMinecraft,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub files: Vec<CurseForgeManifestFile>,
    #[serde(default = "default_overrides_dir")]
    pub overrides: String,
}

fn default_overrides_dir() -> String {
    "overrides".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeManifestMinecraft {
    pub version: String,
    #[serde(default)]
    pub mod_loaders: Vec<CurseForgeManifestLoader>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurseForgeManifestLoader {
    pub id: String,
    #[serde(default)]
    pub primary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurseForgeManifestFile {
    #[serde(rename = "projectID")]
    pub project_id: u64,
    #[serde(rename = "fileID")]
    pub file_id: u64,
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_true() -> bool {
    true
}

/// Parsed CurseForge modpack metadata returned by [`import_curseforge_zip`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedCurseForgePack {
    pub name: String,
    pub version: String,
    pub mc_version: String,
    pub loader: ModLoader,
    pub loader_version: Option<String>,
    pub files: Vec<CurseForgeManifestFile>,
    pub overrides_extracted: usize,
}

/// Imports a `.mrpack` archive into `dest_dir`, downloading referenced files into
/// `<cache_dir>/blobs/sha256/` (hardlinked to `dest_dir/<file.path>`) and extracting
/// `overrides/` + `client-overrides/`.
pub async fn import_mrpack(
    mrpack_path: &Path,
    dest_dir: &Path,
    cache_dir: &Path,
    progress: Option<ProgressCallback>,
) -> Result<(ModsManifest, DemingLockfile)> {
    let file = std::fs::File::open(mrpack_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| DemingError::Other(format!("failed to open mrpack zip: {e}")))?;

    let index: MrpackIndex = {
        let mut index_entry = archive
            .by_name("modrinth.index.json")
            .map_err(|e| DemingError::InvalidSpec(format!("missing modrinth.index.json: {e}")))?;
        let mut raw = String::new();
        index_entry.read_to_string(&mut raw)?;
        serde_json::from_str(&raw)?
    };

    let (mc_version, loader, loader_version) = parse_mrpack_dependencies(&index.dependencies)?;

    tokio::fs::create_dir_all(dest_dir).await?;
    extract_zip_prefix(&mut archive, "overrides/", dest_dir)?;
    extract_zip_prefix(&mut archive, "client-overrides/", dest_dir)?;

    let total = index.files.len() as u64;
    let completed = Arc::new(AtomicU64::new(0));
    let http = build_http_client()?;

    let results: Vec<Result<LockedMod>> = stream::iter(index.files.iter().cloned())
        .map(|mf| {
            let http = http.clone();
            let cache_dir = cache_dir.to_path_buf();
            let dest_dir = dest_dir.to_path_buf();
            let completed = Arc::clone(&completed);
            let progress = progress.clone();
            async move {
                let url = mf.downloads.first().cloned().ok_or_else(|| {
                    DemingError::InvalidSpec(format!(
                        "mrpack file '{}' has no download URL",
                        mf.path
                    ))
                })?;
                let filename = Path::new(&mf.path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&mf.path)
                    .to_string();
                let tmp_dest = cache_dir
                    .join("blobs")
                    .join("tmp")
                    .join(format!("mrpack-{filename}"));

                let (_sha1, sha256) = download_file(
                    &http,
                    &url,
                    &tmp_dest,
                    mf.hashes.sha1.as_deref(),
                    None,
                    false,
                )
                .await?;
                let blob_path = store_blob_sha256(&cache_dir, &tmp_dest, &sha256).await?;
                let target_path = dest_dir.join(&mf.path);
                link_or_copy(&blob_path, &target_path)?;

                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                report_progress(
                    progress.as_ref(),
                    done,
                    total.max(1),
                    &format!("Imported mrpack file {done}/{}", total.max(1)),
                );

                Ok(LockedMod {
                    id: filename.clone(),
                    name: filename.clone(),
                    source: "mrpack".to_string(),
                    version_id: sha256[..12.min(sha256.len())].to_string(),
                    filename,
                    url,
                    sha256,
                    sha512: mf.hashes.sha512,
                })
            }
        })
        .buffer_unordered(8)
        .collect()
        .await;

    let mut locked_mods = Vec::new();
    for res in results {
        locked_mods.push(res?);
    }

    let manifest_mods: Vec<ModSource> = locked_mods
        .iter()
        .map(|m| ModSource::LocalPath(dest_dir.join("mods").join(&m.filename)))
        .collect();
    let manifest = ModsManifest {
        mc_version: mc_version.clone(),
        loader,
        loader_version: loader_version.clone(),
        mods: manifest_mods,
    };
    let lockfile = DemingLockfile::new(mc_version, loader, loader_version, locked_mods);
    Ok((manifest, lockfile))
}

/// Exports a [`ModsManifest`] + [`DemingLockfile`] (and optional `overrides_dir`) as a `.mrpack` zip.
pub fn export_mrpack(
    manifest: &ModsManifest,
    lockfile: &DemingLockfile,
    overrides_dir: Option<&Path>,
    out_path: &Path,
) -> Result<()> {
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let mut dependencies = BTreeMap::new();
    dependencies.insert("minecraft".to_string(), manifest.mc_version.clone());
    if let Some(loader_ver) = manifest
        .loader_version
        .as_ref()
        .or(lockfile.loader_version.as_ref())
    {
        let key = match manifest.loader {
            ModLoader::Vanilla => None,
            ModLoader::Fabric => Some("fabric-loader"),
            ModLoader::Quilt => Some("quilt-loader"),
            ModLoader::NeoForge => Some("neoforge"),
            ModLoader::Forge => Some("forge"),
        };
        if let Some(k) = key {
            dependencies.insert(k.to_string(), loader_ver.clone());
        }
    }

    let files: Vec<MrpackFile> = lockfile
        .mods
        .iter()
        .map(|m| MrpackFile {
            path: format!("mods/{}", m.filename),
            hashes: MrpackHashes {
                sha1: None,
                sha512: m.sha512.clone(),
            },
            downloads: vec![m.url.clone()],
            file_size: 0,
        })
        .collect();

    let index = MrpackIndex {
        format_version: 1,
        game: "minecraft".to_string(),
        version_id: "1.0.0".to_string(),
        name: format!("deming-pack-{}", manifest.mc_version),
        summary: Some("Exported by Deming".to_string()),
        files,
        dependencies,
    };

    let out_file = std::fs::File::create(out_path)?;
    let mut zip = zip::ZipWriter::new(out_file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    zip.start_file("modrinth.index.json", options)
        .map_err(|e| DemingError::Other(format!("zip write error: {e}")))?;
    let index_json = serde_json::to_string_pretty(&index)?;
    zip.write_all(index_json.as_bytes())?;

    if let Some(ov_dir) = overrides_dir {
        if ov_dir.is_dir() {
            add_dir_to_zip(&mut zip, ov_dir, ov_dir, "overrides", options)?;
        }
    }

    zip.finish()
        .map_err(|e| DemingError::Other(format!("zip finish error: {e}")))?;
    Ok(())
}

/// Parses a CurseForge modpack `.zip` (`manifest.json` + optional `overrides/` extraction into `dest_dir`).
pub fn import_curseforge_zip(
    zip_path: &Path,
    dest_dir: Option<&Path>,
) -> Result<ImportedCurseForgePack> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| DemingError::Other(format!("failed to open CurseForge zip: {e}")))?;

    let manifest: CurseForgeManifest = {
        let mut entry = archive
            .by_name("manifest.json")
            .map_err(|e| DemingError::InvalidSpec(format!("missing manifest.json: {e}")))?;
        let mut raw = String::new();
        entry.read_to_string(&mut raw)?;
        serde_json::from_str(&raw)?
    };

    let (loader, loader_version) = parse_curseforge_loader(&manifest.minecraft.mod_loaders);

    let mut overrides_extracted = 0;
    if let Some(dest) = dest_dir {
        std::fs::create_dir_all(dest)?;
        let prefix = format!("{}/", manifest.overrides.trim_end_matches('/'));
        overrides_extracted = extract_zip_prefix(&mut archive, &prefix, dest)?;
    }

    Ok(ImportedCurseForgePack {
        name: manifest.name,
        version: manifest.version,
        mc_version: manifest.minecraft.version,
        loader,
        loader_version,
        files: manifest.files,
        overrides_extracted,
    })
}

fn parse_mrpack_dependencies(
    deps: &BTreeMap<String, String>,
) -> Result<(String, ModLoader, Option<String>)> {
    let mc_version = deps
        .get("minecraft")
        .cloned()
        .ok_or_else(|| DemingError::InvalidSpec("mrpack missing 'minecraft' dependency".into()))?;

    if let Some(v) = deps.get("fabric-loader") {
        Ok((mc_version, ModLoader::Fabric, Some(v.clone())))
    } else if let Some(v) = deps.get("quilt-loader") {
        Ok((mc_version, ModLoader::Quilt, Some(v.clone())))
    } else if let Some(v) = deps.get("neoforge") {
        Ok((mc_version, ModLoader::NeoForge, Some(v.clone())))
    } else if let Some(v) = deps.get("forge") {
        Ok((mc_version, ModLoader::Forge, Some(v.clone())))
    } else {
        Ok((mc_version, ModLoader::Vanilla, None))
    }
}

fn parse_curseforge_loader(loaders: &[CurseForgeManifestLoader]) -> (ModLoader, Option<String>) {
    let primary = loaders
        .iter()
        .find(|l| l.primary)
        .or_else(|| loaders.first());
    let Some(entry) = primary else {
        return (ModLoader::Vanilla, None);
    };
    if let Some(v) = entry.id.strip_prefix("fabric-") {
        (ModLoader::Fabric, Some(v.to_string()))
    } else if let Some(v) = entry.id.strip_prefix("quilt-") {
        (ModLoader::Quilt, Some(v.to_string()))
    } else if let Some(v) = entry.id.strip_prefix("neoforge-") {
        (ModLoader::NeoForge, Some(v.to_string()))
    } else if let Some(v) = entry.id.strip_prefix("forge-") {
        (ModLoader::Forge, Some(v.to_string()))
    } else {
        (ModLoader::Vanilla, Some(entry.id.clone()))
    }
}

fn extract_zip_prefix<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    prefix: &str,
    dest_dir: &Path,
) -> Result<usize> {
    let mut count = 0;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| DemingError::Other(format!("zip entry error: {e}")))?;
        let name = entry.name().to_string();
        let Some(rel) = name.strip_prefix(prefix) else {
            continue;
        };
        if rel.is_empty() || entry.is_dir() {
            continue;
        }
        let rel_path = Path::new(rel);
        if rel_path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::RootDir))
        {
            continue;
        }
        let out_path = dest_dir.join(rel_path);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out_file = std::fs::File::create(&out_path)?;
        std::io::copy(&mut entry, &mut out_file)?;
        count += 1;
    }
    Ok(count)
}

fn add_dir_to_zip<W: Write + std::io::Seek>(
    zip: &mut zip::ZipWriter<W>,
    base_dir: &Path,
    cur_dir: &Path,
    zip_prefix: &str,
    options: SimpleFileOptions,
) -> Result<()> {
    for entry in std::fs::read_dir(cur_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            add_dir_to_zip(zip, base_dir, &path, zip_prefix, options)?;
        } else if path.is_file() {
            let rel = path
                .strip_prefix(base_dir)
                .map_err(|e| DemingError::Other(format!("strip_prefix error: {e}")))?;
            let zip_name = format!(
                "{}/{}",
                zip_prefix.trim_end_matches('/'),
                rel.to_string_lossy().replace('\\', "/")
            );
            zip.start_file(&zip_name, options)
                .map_err(|e| DemingError::Other(format!("zip write error: {e}")))?;
            let bytes = std::fs::read(&path)?;
            zip.write_all(&bytes)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn mrpack_export_and_import_round_trip() {
        let payload = b"mrpack-mod-jar-bytes";
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = sock.read(&mut buf).await;
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        payload.len()
                    );
                    let _ = sock.write_all(header.as_bytes()).await;
                    let _ = sock.write_all(payload).await;
                    let _ = sock.shutdown().await;
                });
            }
        });

        let temp = std::env::temp_dir().join(format!(
            "deming-mrpack-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let overrides_src = temp.join("overrides_src");
        std::fs::create_dir_all(overrides_src.join("config")).unwrap();
        std::fs::write(overrides_src.join("config").join("test.toml"), "flag = true\n").unwrap();

        let mrpack_path = temp.join("pack.mrpack");
        let manifest = ModsManifest {
            mc_version: "1.21.4".to_string(),
            loader: ModLoader::Fabric,
            loader_version: Some("0.16.10".to_string()),
            mods: vec![],
        };
        let lockfile = DemingLockfile::new(
            "1.21.4",
            ModLoader::Fabric,
            Some("0.16.10".to_string()),
            vec![LockedMod {
                id: "example".to_string(),
                name: "Example".to_string(),
                source: "modrinth".to_string(),
                version_id: "v1".to_string(),
                filename: "example-1.0.jar".to_string(),
                url: format!("http://{addr}/example-1.0.jar"),
                sha256: "0".repeat(64),
                sha512: None,
            }],
        );

        export_mrpack(&manifest, &lockfile, Some(&overrides_src), &mrpack_path).unwrap();
        assert!(mrpack_path.is_file());

        let dest_dir = temp.join("imported");
        let cache_dir = temp.join("cache");
        let (imp_manifest, imp_lock) = import_mrpack(&mrpack_path, &dest_dir, &cache_dir, None)
            .await
            .unwrap();

        assert_eq!(imp_manifest.mc_version, "1.21.4");
        assert_eq!(imp_manifest.loader, ModLoader::Fabric);
        assert_eq!(imp_manifest.loader_version.as_deref(), Some("0.16.10"));
        assert_eq!(imp_lock.mods.len(), 1);
        assert!(dest_dir.join("mods").join("example-1.0.jar").is_file());
        assert_eq!(
            std::fs::read_to_string(dest_dir.join("config").join("test.toml")).unwrap(),
            "flag = true\n"
        );

        server.abort();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn curseforge_zip_manifest_parsing_and_overrides() {
        let temp = std::env::temp_dir().join(format!(
            "deming-cf-pack-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).unwrap();
        let zip_path = temp.join("cf-pack.zip");
        let manifest_json = r#"{
          "minecraft": {
            "version": "1.21.1",
            "modLoaders": [{ "id": "neoforge-21.1.72", "primary": true }]
          },
          "name": "Test CF Pack",
          "version": "1.2.0",
          "files": [{ "projectID": 238222, "fileID": 555555, "required": true }],
          "overrides": "overrides"
        }"#;

        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let opts = SimpleFileOptions::default();
            zip.start_file("manifest.json", opts).unwrap();
            zip.write_all(manifest_json.as_bytes()).unwrap();
            zip.start_file("overrides/options.txt", opts).unwrap();
            zip.write_all(b"fov:1.0\n").unwrap();
            zip.finish().unwrap();
        }

        let dest = temp.join("out");
        let imported = import_curseforge_zip(&zip_path, Some(&dest)).unwrap();
        assert_eq!(imported.name, "Test CF Pack");
        assert_eq!(imported.mc_version, "1.21.1");
        assert_eq!(imported.loader, ModLoader::NeoForge);
        assert_eq!(imported.loader_version.as_deref(), Some("21.1.72"));
        assert_eq!(imported.files.len(), 1);
        assert_eq!(imported.overrides_extracted, 1);
        assert_eq!(
            std::fs::read_to_string(dest.join("options.txt")).unwrap(),
            "fov:1.0\n"
        );

        let _ = std::fs::remove_dir_all(&temp);
    }
}
