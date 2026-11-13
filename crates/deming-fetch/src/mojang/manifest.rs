//! Mojang Piston Meta manifest and version JSON schemas + cache path helpers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mojang::libraries::{evaluate_arguments, is_rule_list_allowed};

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
