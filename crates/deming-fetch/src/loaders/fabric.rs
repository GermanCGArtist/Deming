//! Fabric (and shared Fabric/Quilt Meta) mod loader installation.

use std::path::{Path, PathBuf};

use deming_core::{DemingError, Result};
use serde::Deserialize;

use super::{build_vanilla_client_profile, LaunchProfile, LoaderSide};
use crate::java::required_java_version;
use crate::mojang::MojangClient;
use crate::util::{build_http_client, download_file, fetch_json, maven_coord_to_path};
use crate::{report_progress, ProgressCallback};

pub const FABRIC_META_BASE: &str = "https://meta.fabricmc.net/v2";
pub const FABRIC_MAVEN_REPO: &str = "https://maven.fabricmc.net";

/// Entry from Fabric/Quilt `/versions/loader/{mc_version}` endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct FabricLoaderListEntry {
    pub loader: FabricLoaderVersion,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FabricLoaderVersion {
    pub version: String,
    #[serde(default)]
    pub stable: bool,
}

/// Fabric/Quilt `/versions/loader/{mc}/{loader}/profile/json` or `/server/json` schema.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FabricProfileJson {
    pub id: String,
    pub main_class: String,
    #[serde(default)]
    pub arguments: Option<FabricArguments>,
    #[serde(default)]
    pub libraries: Vec<FabricLibrary>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FabricArguments {
    #[serde(default)]
    pub game: Vec<serde_json::Value>,
    #[serde(default)]
    pub jvm: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FabricLibrary {
    pub name: String,
    pub url: Option<String>,
    pub sha1: Option<String>,
    pub size: Option<u64>,
}

/// Fabric `/versions/installer` entry.
#[derive(Debug, Clone, Deserialize)]
pub struct FabricInstallerEntry {
    pub version: String,
    #[serde(default)]
    pub stable: bool,
}

pub(crate) async fn install_fabric(
    mojang: &MojangClient,
    mc_version: &str,
    loader_version: Option<&str>,
    side: LoaderSide,
    cache_dir: &Path,
    progress: Option<ProgressCallback>,
) -> Result<LaunchProfile> {
    install_fabric_or_quilt(
        mojang,
        FABRIC_META_BASE,
        FABRIC_MAVEN_REPO,
        mc_version,
        loader_version,
        side,
        cache_dir,
        true,
        progress,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn install_fabric_or_quilt(
    mojang: &MojangClient,
    meta_base: &str,
    default_maven: &str,
    mc_version: &str,
    loader_version: Option<&str>,
    side: LoaderSide,
    cache_dir: &Path,
    is_fabric: bool,
    progress: Option<ProgressCallback>,
) -> Result<LaunchProfile> {
    let http = build_http_client()?;
    let loader_name = if is_fabric { "Fabric" } else { "Quilt" };

    let resolved_loader = match loader_version {
        Some(v) => v.to_string(),
        None => {
            let url = format!("{meta_base}/versions/loader/{mc_version}");
            let entries: Vec<FabricLoaderListEntry> = fetch_json(&http, &url).await?;
            entries
                .iter()
                .find(|e| e.loader.stable)
                .or_else(|| entries.first())
                .map(|e| e.loader.version.clone())
                .ok_or_else(|| {
                    DemingError::InvalidSpec(format!(
                        "no loader version found for MC {mc_version} at {url}"
                    ))
                })?
        }
    };

    match side {
        LoaderSide::Client => {
            let vanilla = mojang
                .ensure_client_with_progress(mc_version, cache_dir, progress.clone())
                .await?;
            report_progress(
                progress.as_ref(),
                0,
                1,
                &format!("Resolving {loader_name} {resolved_loader} profile"),
            );
            let profile_url =
                format!("{meta_base}/versions/loader/{mc_version}/{resolved_loader}/profile/json");
            let profile: FabricProfileJson = fetch_json(&http, &profile_url).await?;
            let loader_libs = download_fabric_libraries(
                &http,
                &profile.libraries,
                default_maven,
                cache_dir,
                progress.as_ref(),
            )
            .await?;

            let mut base = build_vanilla_client_profile(&vanilla);
            let mut combined_cp = loader_libs;
            combined_cp.extend(base.classpath);
            base.classpath = combined_cp;
            base.main_class = profile.main_class;

            if let Some(args) = profile.arguments {
                for v in args.jvm {
                    if let Some(s) = v.as_str() {
                        base.jvm_args.push(s.to_string());
                    }
                }
                for v in args.game {
                    if let Some(s) = v.as_str() {
                        base.game_args.push(s.to_string());
                    }
                }
            }
            Ok(base)
        }
        LoaderSide::Server => {
            let vjson = mojang.fetch_version_json(mc_version, cache_dir).await?;
            let vanilla_server = mojang
                .ensure_server_with_progress(mc_version, cache_dir, progress.clone())
                .await?;
            let java_major = vjson
                .java_version
                .as_ref()
                .map(|j| j.major_version)
                .unwrap_or_else(|| required_java_version(mc_version));

            if is_fabric {
                report_progress(
                    progress.as_ref(),
                    0,
                    1,
                    &format!("Downloading {loader_name} {resolved_loader} server launcher"),
                );
                let installers_url = format!("{meta_base}/versions/installer");
                let installers: Vec<FabricInstallerEntry> =
                    fetch_json(&http, &installers_url).await?;
                let inst_ver = installers
                    .iter()
                    .find(|i| i.stable)
                    .or_else(|| installers.first())
                    .map(|i| i.version.as_str())
                    .ok_or_else(|| {
                        DemingError::Network("no Fabric installer version found".to_string())
                    })?;

                let server_jar_url = format!(
                    "{meta_base}/versions/loader/{mc_version}/{resolved_loader}/{inst_ver}/server/jar"
                );
                let dest = cache_dir
                    .join("loaders")
                    .join("fabric")
                    .join(format!(
                        "fabric-server-mc.{mc_version}-loader.{resolved_loader}-launcher.{inst_ver}.jar"
                    ));
                download_file(&http, &server_jar_url, &dest, None, None, true).await?;
                report_progress(
                    progress.as_ref(),
                    1,
                    1,
                    &format!("{loader_name} server launcher ready"),
                );

                Ok(LaunchProfile {
                    main_class: String::new(),
                    classpath: vec![dest.clone(), vanilla_server],
                    jvm_args: Vec::new(),
                    game_args: vec!["nogui".to_string()],
                    asset_index: vjson.asset_index.id,
                    assets_dir: cache_dir.join("assets"),
                    natives_dir: cache_dir.join("natives").join(mc_version),
                    server_jar: Some(dest),
                    java_major_version: java_major,
                })
            } else {
                let profile_url = format!(
                    "{meta_base}/versions/loader/{mc_version}/{resolved_loader}/server/json"
                );
                let profile: FabricProfileJson = fetch_json(&http, &profile_url).await?;
                let loader_libs = download_fabric_libraries(
                    &http,
                    &profile.libraries,
                    default_maven,
                    cache_dir,
                    progress.as_ref(),
                )
                .await?;
                let mut cp = loader_libs;
                cp.push(vanilla_server.clone());
                Ok(LaunchProfile {
                    main_class: profile.main_class,
                    classpath: cp,
                    jvm_args: Vec::new(),
                    game_args: vec!["nogui".to_string()],
                    asset_index: vjson.asset_index.id,
                    assets_dir: cache_dir.join("assets"),
                    natives_dir: cache_dir.join("natives").join(mc_version),
                    server_jar: Some(vanilla_server),
                    java_major_version: java_major,
                })
            }
        }
    }
}

async fn download_fabric_libraries(
    http: &reqwest::Client,
    libs: &[FabricLibrary],
    default_maven: &str,
    cache_dir: &Path,
    progress: Option<&ProgressCallback>,
) -> Result<Vec<PathBuf>> {
    let total = libs.len() as u64;
    let mut paths = Vec::new();
    for (idx, lib) in libs.iter().enumerate() {
        report_progress(
            progress,
            idx as u64,
            total.max(1),
            &format!("Downloading loader library {}", lib.name),
        );
        let rel = maven_coord_to_path(&lib.name).ok_or_else(|| {
            DemingError::InvalidSpec(format!("invalid Maven coordinate: {}", lib.name))
        })?;
        let base = lib.url.as_deref().unwrap_or(default_maven);
        let url = format!("{}/{}", base.trim_end_matches('/'), rel);
        let dest = cache_dir.join("libraries").join(&rel);
        download_file(http, &url, &dest, lib.sha1.as_deref(), None, true).await?;
        paths.push(dest);
    }
    report_progress(progress, total, total.max(1), "Loader libraries ready");
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fabric_meta_fixtures() {
        let list_json = r#"[
          { "loader": { "version": "0.16.10", "stable": true } },
          { "loader": { "version": "0.16.9", "stable": false } }
        ]"#;
        let list: Vec<FabricLoaderListEntry> = serde_json::from_str(list_json).unwrap();
        assert_eq!(list[0].loader.version, "0.16.10");
        assert!(list[0].loader.stable);

        let profile_json = r#"{
          "id": "fabric-loader-0.16.10-1.21.4",
          "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
          "arguments": {
            "game": [],
            "jvm": ["-DFabricMcEmu=net.minecraft.client.main.Main"]
          },
          "libraries": [
            {
              "name": "net.fabricmc:fabric-loader:0.16.10",
              "url": "https://maven.fabricmc.net/",
              "sha1": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
              "size": 123456
            }
          ]
        }"#;
        let profile: FabricProfileJson = serde_json::from_str(profile_json).unwrap();
        assert_eq!(
            profile.main_class,
            "net.fabricmc.loader.impl.launch.knot.KnotClient"
        );
        assert_eq!(profile.libraries.len(), 1);
        assert_eq!(
            profile.libraries[0].name,
            "net.fabricmc:fabric-loader:0.16.10"
        );
    }
}
