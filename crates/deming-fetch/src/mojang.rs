//! Mojang Piston Meta manifest, version JSON, libraries, assets, and JAR fetcher.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use deming_core::{DemingError, Result};
use futures_util::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};

use crate::util::{
    build_http_client, current_arch, current_mojang_os, download_file, fetch_json,
    maven_coord_to_path, verify_sha1,
};

/// Official Mojang Piston Meta v2 version manifest URL.
pub const PISTON_META_MANIFEST_URL: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

/// Official Mojang asset objects base URL.
pub const RESOURCES_BASE_URL: &str = "https://resources.download.minecraft.net";

/// Top-level Mojang `version_manifest_v2.json` schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionManifest {
    pub latest: LatestVersions,
    pub versions: Vec<VersionEntry>,
}

/// Latest release and snapshot pointers in `version_manifest_v2.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatestVersions {
    pub release: String,
    pub snapshot: String,
}

/// A single Minecraft version entry in `version_manifest_v2.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub version_type: String,
    pub url: String,
    pub time: String,
    pub release_time: String,
    pub sha1: String,
}

/// Per-version metadata JSON (`<version>.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionJson {
    pub id: String,
    #[serde(rename = "type")]
    pub version_type: String,
    pub main_class: String,
    pub asset_index: AssetIndexInfo,
    pub assets: String,
    pub downloads: VersionDownloads,
    #[serde(default)]
    pub libraries: Vec<Library>,
    pub arguments: Option<VersionArguments>,
    pub minecraft_arguments: Option<String>,
    pub java_version: Option<JavaVersionInfo>,
}

/// Asset index descriptor inside [`VersionJson`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetIndexInfo {
    pub id: String,
    pub sha1: String,
    pub size: u64,
    pub total_size: Option<u64>,
    pub url: String,
}

/// Client and server JAR download descriptors inside [`VersionJson`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionDownloads {
    pub client: DownloadArtifact,
    pub server: Option<DownloadArtifact>,
    pub client_mappings: Option<DownloadArtifact>,
    pub server_mappings: Option<DownloadArtifact>,
}

/// A downloadable artifact with SHA-1 checksum and size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadArtifact {
    pub sha1: String,
    pub size: u64,
    pub url: String,
    pub path: Option<String>,
}

/// A Maven library dependency entry inside [`VersionJson`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Library {
    pub name: String,
    pub downloads: Option<LibraryDownloads>,
    pub rules: Option<Vec<Rule>>,
    pub natives: Option<BTreeMap<String, String>>,
    pub url: Option<String>,
}

/// Download artifacts for a [`Library`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryDownloads {
    pub artifact: Option<DownloadArtifact>,
    pub classifiers: Option<BTreeMap<String, DownloadArtifact>>,
}

/// OS/feature applicability rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub action: String,
    pub os: Option<OsRule>,
    pub features: Option<BTreeMap<String, bool>>,
}

/// OS matching criteria inside [`Rule`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OsRule {
    pub name: Option<String>,
    pub version: Option<String>,
    pub arch: Option<String>,
}

/// Modern structured JVM and game arguments inside [`VersionJson`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionArguments {
    #[serde(default)]
    pub game: Vec<ArgumentValue>,
    #[serde(default)]
    pub jvm: Vec<ArgumentValue>,
}

/// Either a plain string argument or a rule-gated argument list.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ArgumentValue {
    Simple(String),
    Conditional {
        rules: Vec<Rule>,
        value: StringOrVec,
    },
}

/// Single string or array of strings for conditional arguments.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StringOrVec {
    Single(String),
    Multiple(Vec<String>),
}

/// Required Java major version metadata inside [`VersionJson`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaVersionInfo {
    pub component: String,
    pub major_version: u32,
}

/// Asset index JSON mapping logical asset paths to object hashes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetIndexJson {
    pub objects: BTreeMap<String, AssetObject>,
}

/// Individual asset object hash and size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetObject {
    pub hash: String,
    pub size: u64,
}

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
        let version_json = self.fetch_version_json(mc_version, cache_dir).await?;

        // 1. Client JAR
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

        // 2. OS-filtered libraries
        let os_name = current_mojang_os();
        let os_arch = current_arch();
        let natives_dir = cache_dir.join("natives").join(mc_version);
        tokio::fs::create_dir_all(&natives_dir).await?;

        let mut library_paths = Vec::new();
        for lib in &version_json.libraries {
            if !is_rule_list_allowed(lib.rules.as_deref(), os_name, os_arch) {
                continue;
            }

            if let Some(downloads) = &lib.downloads {
                if let Some(artifact) = &downloads.artifact {
                    let rel_path = artifact
                        .path
                        .clone()
                        .or_else(|| maven_coord_to_path(&lib.name))
                        .ok_or_else(|| {
                            DemingError::InvalidSpec(format!(
                                "cannot determine path for library '{}'",
                                lib.name
                            ))
                        })?;
                    let dest = cache_dir.join("libraries").join(&rel_path);
                    download_file(
                        &self.http,
                        &artifact.url,
                        &dest,
                        Some(&artifact.sha1),
                        None,
                        false,
                    )
                    .await?;
                    library_paths.push(dest);
                }

                // Legacy natives classifier support
                if let (Some(natives_map), Some(classifiers)) =
                    (&lib.natives, &downloads.classifiers)
                {
                    if let Some(classifier_tpl) = natives_map.get(os_name) {
                        let arch_bits = if os_arch == "x86" { "32" } else { "64" };
                        let classifier_key = classifier_tpl.replace("${arch}", arch_bits);
                        if let Some(native_art) = classifiers.get(&classifier_key) {
                            let rel_path = native_art.path.clone().unwrap_or_else(|| {
                                format!("natives/{}-{classifier_key}.jar", lib.name.replace(':', "_"))
                            });
                            let dest = cache_dir.join("libraries").join(&rel_path);
                            download_file(
                                &self.http,
                                &native_art.url,
                                &dest,
                                Some(&native_art.sha1),
                                None,
                                false,
                            )
                            .await?;
                            extract_natives_jar(&dest, &natives_dir)?;
                        }
                    }
                }
            } else if let Some(base_url) = &lib.url {
                if let Some(rel_path) = maven_coord_to_path(&lib.name) {
                    let dest = cache_dir.join("libraries").join(&rel_path);
                    let url = format!("{}/{}", base_url.trim_end_matches('/'), rel_path);
                    download_file(&self.http, &url, &dest, None, None, true).await?;
                    library_paths.push(dest);
                }
            }
        }

        // 3. Asset index & objects
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
        self.ensure_asset_objects(&assets_dir, &index_json).await?;

        Ok(ResolvedVanilla {
            asset_index_id: version_json.asset_index.id.clone(),
            version_json,
            client_jar,
            library_paths,
            assets_dir,
            natives_dir,
        })
    }

    /// Downloads all missing or hash-mismatched asset objects concurrently.
    async fn ensure_asset_objects(
        &self,
        assets_dir: &Path,
        index_json: &AssetIndexJson,
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

        let http = self.http.clone();
        let results: Vec<Result<()>> = stream::iter(to_download)
            .map(|(url, dest, sha1)| {
                let http = http.clone();
                async move {
                    download_file(&http, &url, &dest, Some(&sha1), None, false).await?;
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
        let version_json = self.fetch_version_json(mc_version, cache_dir).await?;
        let server_dl = version_json.downloads.server.ok_or_else(|| {
            DemingError::InvalidSpec(format!(
                "Minecraft version '{mc_version}' has no server download in Mojang manifest"
            ))
        })?;
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
        Ok(dest)
    }
}

/// Returns `<cache_dir>/versions/<id>/<id>.json`.
pub fn version_json_path(cache_dir: &Path, mc_version: &str) -> PathBuf {
    cache_dir
        .join("versions")
        .join(mc_version)
        .join(format!("{mc_version}.json"))
}

/// Returns `<cache_dir>/versions/<id>/<id>-client.jar`.
pub fn client_jar_path(cache_dir: &Path, mc_version: &str) -> PathBuf {
    cache_dir
        .join("versions")
        .join(mc_version)
        .join(format!("{mc_version}-client.jar"))
}

/// Returns `<cache_dir>/versions/<id>/<id>-server.jar`.
pub fn server_jar_path(cache_dir: &Path, mc_version: &str) -> PathBuf {
    cache_dir
        .join("versions")
        .join(mc_version)
        .join(format!("{mc_version}-server.jar"))
}

/// Returns `<cache_dir>/assets/indexes/<asset_index_id>.json`.
pub fn asset_index_path(cache_dir: &Path, asset_index_id: &str) -> PathBuf {
    cache_dir
        .join("assets")
        .join("indexes")
        .join(format!("{asset_index_id}.json"))
}

/// Evaluates Mojang OS/feature rules for a library or argument entry.
///
/// If `rules` is `None` or empty, returns `true`.
/// Feature-gated rules (e.g. `is_demo_user`, `has_custom_resolution`) evaluate to `false`.
pub fn is_rule_list_allowed(rules: Option<&[Rule]>, os_name: &str, os_arch: &str) -> bool {
    let Some(rules) = rules else {
        return true;
    };
    if rules.is_empty() {
        return true;
    }

    let mut allowed = false;
    for rule in rules {
        if rule_matches(rule, os_name, os_arch) {
            allowed = rule.action == "allow";
        }
    }
    allowed
}

fn rule_matches(rule: &Rule, os_name: &str, os_arch: &str) -> bool {
    if let Some(features) = &rule.features {
        // Standard automated launches don't enable demo/quickplay/custom_resolution feature flags.
        if features.values().any(|&v| v) {
            return false;
        }
    }
    if let Some(os) = &rule.os {
        if let Some(name) = &os.name {
            if name != os_name {
                return false;
            }
        }
        if let Some(arch) = &os.arch {
            let normalized = if arch == "x86_64" { "x64" } else { arch.as_str() };
            if normalized != os_arch {
                return false;
            }
        }
    }
    true
}

/// Evaluates a list of [`ArgumentValue`] entries for the given OS/arch.
pub fn evaluate_arguments(args: &[ArgumentValue], os_name: &str, os_arch: &str) -> Vec<String> {
    let mut out = Vec::new();
    for item in args {
        match item {
            ArgumentValue::Simple(s) => out.push(s.clone()),
            ArgumentValue::Conditional { rules, value } => {
                if is_rule_list_allowed(Some(rules), os_name, os_arch) {
                    match value {
                        StringOrVec::Single(s) => out.push(s.clone()),
                        StringOrVec::Multiple(v) => out.extend(v.iter().cloned()),
                    }
                }
            }
        }
    }
    out
}

fn extract_natives_jar(jar_path: &Path, natives_dir: &Path) -> Result<()> {
    let file = std::fs::File::open(jar_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| DemingError::Other(format!("failed to open natives jar: {e}")))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| DemingError::Other(format!("zip entry error: {e}")))?;
        let name = entry.name().to_string();
        if entry.is_dir() || name.starts_with("META-INF/") {
            continue;
        }
        let Some(file_name) = Path::new(&name).file_name() else {
            continue;
        };
        let out_path = natives_dir.join(file_name);
        let mut out_file = std::fs::File::create(&out_path)?;
        std::io::copy(&mut entry, &mut out_file)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST_FIXTURE: &str = r#"{
      "latest": { "release": "1.21.4", "snapshot": "25w02a" },
      "versions": [
        {
          "id": "1.21.4",
          "type": "release",
          "url": "https://piston-meta.mojang.com/v1/packages/abc/1.21.4.json",
          "time": "2024-12-03T10:00:00+00:00",
          "releaseTime": "2024-12-03T10:00:00+00:00",
          "sha1": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
        }
      ]
    }"#;

    const VERSION_FIXTURE: &str = r#"{
      "id": "1.21.4",
      "type": "release",
      "mainClass": "net.minecraft.client.main.Main",
      "assets": "19",
      "assetIndex": {
        "id": "19",
        "sha1": "1111111111111111111111111111111111111111",
        "size": 450000,
        "totalSize": 600000000,
        "url": "https://piston-meta.mojang.com/v1/packages/19.json"
      },
      "downloads": {
        "client": {
          "sha1": "2222222222222222222222222222222222222222",
          "size": 25000000,
          "url": "https://piston-data.mojang.com/v1/objects/client.jar"
        },
        "server": {
          "sha1": "3333333333333333333333333333333333333333",
          "size": 45000000,
          "url": "https://piston-data.mojang.com/v1/objects/server.jar"
        }
      },
      "javaVersion": {
        "component": "java-runtime-delta",
        "majorVersion": 21
      },
      "arguments": {
        "game": ["--username", "${auth_player_name}"],
        "jvm": [
          {
            "rules": [{ "action": "allow", "os": { "name": "windows" } }],
            "value": "-XX:HeapDumpPath=MojangTricksIntelDriversForPerformance_javaw.exe_minecraft.exe.heapdump"
          },
          "-Djava.library.path=${natives_directory}"
        ]
      },
      "libraries": [
        {
          "name": "org.lwjgl:lwjgl:3.3.3",
          "downloads": {
            "artifact": {
              "path": "org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3.jar",
              "sha1": "4444444444444444444444444444444444444444",
              "size": 1000,
              "url": "https://libraries.minecraft.net/org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3.jar"
            }
          }
        },
        {
          "name": "org.lwjgl:lwjgl:3.3.3:natives-linux",
          "rules": [{ "action": "allow", "os": { "name": "linux" } }],
          "downloads": {
            "artifact": {
              "path": "org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-linux.jar",
              "sha1": "5555555555555555555555555555555555555555",
              "size": 2000,
              "url": "https://libraries.minecraft.net/org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-linux.jar"
            }
          }
        }
      ]
    }"#;

    #[test]
    fn parse_mojang_manifest_and_version_fixtures() {
        let manifest: VersionManifest = serde_json::from_str(MANIFEST_FIXTURE).unwrap();
        assert_eq!(manifest.latest.release, "1.21.4");
        assert_eq!(manifest.versions.len(), 1);
        assert_eq!(manifest.versions[0].id, "1.21.4");

        let ver: VersionJson = serde_json::from_str(VERSION_FIXTURE).unwrap();
        assert_eq!(ver.main_class, "net.minecraft.client.main.Main");
        assert_eq!(ver.java_version.as_ref().unwrap().major_version, 21);
        assert_eq!(ver.libraries.len(), 2);

        // OS rule filtering: on windows, linux-only library is excluded
        assert!(is_rule_list_allowed(
            ver.libraries[0].rules.as_deref(),
            "windows",
            "x64"
        ));
        assert!(!is_rule_list_allowed(
            ver.libraries[1].rules.as_deref(),
            "windows",
            "x64"
        ));
        assert!(is_rule_list_allowed(
            ver.libraries[1].rules.as_deref(),
            "linux",
            "x64"
        ));

        let jvm_args = evaluate_arguments(&ver.arguments.as_ref().unwrap().jvm, "windows", "x64");
        assert_eq!(jvm_args.len(), 2);
        let jvm_args_linux =
            evaluate_arguments(&ver.arguments.as_ref().unwrap().jvm, "linux", "x64");
        assert_eq!(jvm_args_linux.len(), 1);
    }

    #[test]
    fn cache_path_resolution() {
        let cache = Path::new("C:/deming/cache");
        assert_eq!(
            version_json_path(cache, "1.21.4"),
            PathBuf::from("C:/deming/cache/versions/1.21.4/1.21.4.json")
        );
        assert_eq!(
            client_jar_path(cache, "1.21.4"),
            PathBuf::from("C:/deming/cache/versions/1.21.4/1.21.4-client.jar")
        );
        assert_eq!(
            server_jar_path(cache, "1.21.4"),
            PathBuf::from("C:/deming/cache/versions/1.21.4/1.21.4-server.jar")
        );
        assert_eq!(
            asset_index_path(cache, "19"),
            PathBuf::from("C:/deming/cache/assets/indexes/19.json")
        );
    }
}
