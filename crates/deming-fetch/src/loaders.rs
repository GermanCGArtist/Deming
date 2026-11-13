//! Mod loader resolution and installation (`Vanilla`, `Fabric`, `Quilt`, `NeoForge`, `Forge`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use deming_core::{DemingConfig, DemingError, ModLoader, Result};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::java::{
    ensure_java_runtime_with_progress, required_java_version,
};
use crate::mojang::{
    evaluate_arguments, MojangClient, ResolvedVanilla, VersionJson,
};
use crate::util::{
    build_http_client, current_arch, current_mojang_os, download_file, fetch_json,
    maven_coord_to_path,
};
use crate::{report_progress, ProgressCallback};

pub const FABRIC_META_BASE: &str = "https://meta.fabricmc.net/v2";
pub const QUILT_META_BASE: &str = "https://meta.quiltmc.org/v3";
pub const NEOFORGE_MAVEN_API: &str =
    "https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/neoforge";
pub const NEOFORGE_MAVEN_REPO: &str = "https://maven.neoforged.net/releases";
pub const FORGE_PROMOTIONS_URL: &str =
    "https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json";
pub const FORGE_MAVEN_REPO: &str = "https://maven.minecraftforge.net";

/// Target side for mod loader installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoaderSide {
    Client,
    Server,
}

/// Resolved launch configuration returned by [`resolve_and_install_loader`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchProfile {
    pub main_class: String,
    pub classpath: Vec<PathBuf>,
    pub jvm_args: Vec<String>,
    pub game_args: Vec<String>,
    pub asset_index: String,
    pub assets_dir: PathBuf,
    pub natives_dir: PathBuf,
    pub server_jar: Option<PathBuf>,
    pub java_major_version: u32,
}

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

/// NeoForge Maven versions API response.
#[derive(Debug, Clone, Deserialize)]
pub struct NeoForgeVersionsResponse {
    pub versions: Vec<String>,
}

/// Forge `promotions_slim.json` response.
#[derive(Debug, Clone, Deserialize)]
pub struct ForgePromotions {
    pub promos: BTreeMap<String, String>,
}

/// Resolves and installs the requested [`ModLoader`] for either client or server,
/// returning a ready-to-launch [`LaunchProfile`].
pub async fn resolve_and_install_loader(
    config: &DemingConfig,
    mc_version: &str,
    loader: ModLoader,
    loader_version: Option<&str>,
    side: LoaderSide,
    work_dir: &Path,
    java_bin: Option<&Path>,
) -> Result<LaunchProfile> {
    resolve_and_install_loader_with_progress(
        config,
        mc_version,
        loader,
        loader_version,
        side,
        work_dir,
        java_bin,
        None,
    )
    .await
}

/// Resolves and installs the requested [`ModLoader`] for either client or server,
/// returning a ready-to-launch [`LaunchProfile`] and emitting progress updates.
#[allow(clippy::too_many_arguments)]
pub async fn resolve_and_install_loader_with_progress(
    config: &DemingConfig,
    mc_version: &str,
    loader: ModLoader,
    loader_version: Option<&str>,
    side: LoaderSide,
    work_dir: &Path,
    java_bin: Option<&Path>,
    progress: Option<ProgressCallback>,
) -> Result<LaunchProfile> {
    let cache_dir = config.cache_dir();
    let mojang = MojangClient::new()?;

    match loader {
        ModLoader::Vanilla => {
            install_vanilla(&mojang, mc_version, side, &cache_dir, progress).await
        }
        ModLoader::Fabric => {
            install_fabric_or_quilt(
                &mojang,
                FABRIC_META_BASE,
                "https://maven.fabricmc.net",
                mc_version,
                loader_version,
                side,
                &cache_dir,
                true,
                progress,
            )
            .await
        }
        ModLoader::Quilt => {
            install_fabric_or_quilt(
                &mojang,
                QUILT_META_BASE,
                "https://maven.quiltmc.org/repository/release",
                mc_version,
                loader_version,
                side,
                &cache_dir,
                false,
                progress,
            )
            .await
        }
        ModLoader::NeoForge => {
            install_neoforge(
                &mojang,
                mc_version,
                loader_version,
                side,
                &cache_dir,
                work_dir,
                java_bin,
                progress,
            )
            .await
        }
        ModLoader::Forge => {
            install_forge(
                &mojang,
                mc_version,
                loader_version,
                side,
                &cache_dir,
                work_dir,
                java_bin,
                progress,
            )
            .await
        }
    }
}

async fn install_vanilla(
    mojang: &MojangClient,
    mc_version: &str,
    side: LoaderSide,
    cache_dir: &Path,
    progress: Option<ProgressCallback>,
) -> Result<LaunchProfile> {
    match side {
        LoaderSide::Client => {
            let resolved = mojang
                .ensure_client_with_progress(mc_version, cache_dir, progress)
                .await?;
            Ok(build_vanilla_client_profile(&resolved))
        }
        LoaderSide::Server => {
            let vjson = mojang.fetch_version_json(mc_version, cache_dir).await?;
            let server_jar = mojang
                .ensure_server_with_progress(mc_version, cache_dir, progress)
                .await?;
            let java_major = vjson
                .java_version
                .as_ref()
                .map(|j| j.major_version)
                .unwrap_or_else(|| required_java_version(mc_version));
            Ok(LaunchProfile {
                main_class: String::new(),
                classpath: vec![server_jar.clone()],
                jvm_args: Vec::new(),
                game_args: vec!["nogui".to_string()],
                asset_index: vjson.asset_index.id,
                assets_dir: cache_dir.join("assets"),
                natives_dir: cache_dir.join("natives").join(mc_version),
                server_jar: Some(server_jar),
                java_major_version: java_major,
            })
        }
    }
}

/// Constructs a [`LaunchProfile`] from a [`ResolvedVanilla`] client installation.
pub fn build_vanilla_client_profile(resolved: &ResolvedVanilla) -> LaunchProfile {
    let os_name = current_mojang_os();
    let os_arch = current_arch();

    let (jvm_args, game_args) = if let Some(args) = &resolved.version_json.arguments {
        (
            evaluate_arguments(&args.jvm, os_name, os_arch),
            evaluate_arguments(&args.game, os_name, os_arch),
        )
    } else if let Some(legacy) = &resolved.version_json.minecraft_arguments {
        (
            vec!["-Djava.library.path=${natives_directory}".to_string()],
            legacy.split_whitespace().map(str::to_string).collect(),
        )
    } else {
        (Vec::new(), Vec::new())
    };

    let mut classpath = resolved.library_paths.clone();
    classpath.push(resolved.client_jar.clone());

    let java_major = resolved
        .version_json
        .java_version
        .as_ref()
        .map(|j| j.major_version)
        .unwrap_or_else(|| required_java_version(&resolved.version_json.id));

    LaunchProfile {
        main_class: resolved.version_json.main_class.clone(),
        classpath,
        jvm_args,
        game_args,
        asset_index: resolved.asset_index_id.clone(),
        assets_dir: resolved.assets_dir.clone(),
        natives_dir: resolved.natives_dir.clone(),
        server_jar: None,
        java_major_version: java_major,
    }
}

#[allow(clippy::too_many_arguments)]
async fn install_fabric_or_quilt(
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

#[allow(clippy::too_many_arguments)]
async fn install_neoforge(
    mojang: &MojangClient,
    mc_version: &str,
    loader_version: Option<&str>,
    side: LoaderSide,
    cache_dir: &Path,
    work_dir: &Path,
    java_bin: Option<&Path>,
    progress: Option<ProgressCallback>,
) -> Result<LaunchProfile> {
    let http = build_http_client()?;

    let nf_version = match loader_version {
        Some(v) => v.to_string(),
        None => {
            let resp: NeoForgeVersionsResponse = fetch_json(&http, NEOFORGE_MAVEN_API).await?;
            let prefix = neoforge_prefix_for_mc(mc_version).ok_or_else(|| {
                DemingError::InvalidSpec(format!(
                    "cannot derive NeoForge version prefix from MC '{mc_version}'"
                ))
            })?;
            resp.versions
                .into_iter()
                .rev()
                .find(|v| v.starts_with(&prefix) && !v.contains("beta"))
                .ok_or_else(|| {
                    DemingError::InvalidSpec(format!(
                        "no NeoForge version found for MC {mc_version} (prefix {prefix})"
                    ))
                })?
        }
    };

    report_progress(
        progress.as_ref(),
        0,
        1,
        &format!("Downloading NeoForge {nf_version} installer"),
    );
    let installer_url = format!(
        "{NEOFORGE_MAVEN_REPO}/net/neoforged/neoforge/{nf_version}/neoforge-{nf_version}-installer.jar"
    );
    let installer_jar = cache_dir
        .join("loaders")
        .join("neoforge")
        .join(format!("neoforge-{nf_version}-installer.jar"));
    download_file(&http, &installer_url, &installer_jar, None, None, true).await?;

    run_forge_installer(
        mojang,
        mc_version,
        &installer_jar,
        side,
        cache_dir,
        work_dir,
        java_bin,
        progress,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn install_forge(
    mojang: &MojangClient,
    mc_version: &str,
    loader_version: Option<&str>,
    side: LoaderSide,
    cache_dir: &Path,
    work_dir: &Path,
    java_bin: Option<&Path>,
    progress: Option<ProgressCallback>,
) -> Result<LaunchProfile> {
    let http = build_http_client()?;

    let forge_ver = match loader_version {
        Some(v) => v.to_string(),
        None => {
            let promos: ForgePromotions = fetch_json(&http, FORGE_PROMOTIONS_URL).await?;
            let rec_key = format!("{mc_version}-recommended");
            let lat_key = format!("{mc_version}-latest");
            promos
                .promos
                .get(&rec_key)
                .or_else(|| promos.promos.get(&lat_key))
                .cloned()
                .ok_or_else(|| {
                    DemingError::InvalidSpec(format!(
                        "no Forge promotion found for MC {mc_version}"
                    ))
                })?
        }
    };

    let full_ver = if forge_ver.starts_with(mc_version) {
        forge_ver
    } else {
        format!("{mc_version}-{forge_ver}")
    };

    report_progress(
        progress.as_ref(),
        0,
        1,
        &format!("Downloading Forge {full_ver} installer"),
    );
    let installer_url = format!(
        "{FORGE_MAVEN_REPO}/net/minecraftforge/forge/{full_ver}/forge-{full_ver}-installer.jar"
    );
    let installer_jar = cache_dir
        .join("loaders")
        .join("forge")
        .join(format!("forge-{full_ver}-installer.jar"));
    download_file(&http, &installer_url, &installer_jar, None, None, true).await?;

    run_forge_installer(
        mojang,
        mc_version,
        &installer_jar,
        side,
        cache_dir,
        work_dir,
        java_bin,
        progress,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_forge_installer(
    mojang: &MojangClient,
    mc_version: &str,
    installer_jar: &Path,
    side: LoaderSide,
    cache_dir: &Path,
    work_dir: &Path,
    java_bin: Option<&Path>,
    progress: Option<ProgressCallback>,
) -> Result<LaunchProfile> {
    tokio::fs::create_dir_all(work_dir).await?;

    let java_major = required_java_version(mc_version);
    let java_exe = match java_bin {
        Some(p) => p.to_path_buf(),
        None => {
            ensure_java_runtime_with_progress(cache_dir, java_major, progress.clone())
                .await?
                .executable
        }
    };

    report_progress(progress.as_ref(), 0, 1, "Running headless mod loader installer");

    let flag = match side {
        LoaderSide::Client => {
            let profiles = work_dir.join("launcher_profiles.json");
            if !profiles.exists() {
                tokio::fs::write(&profiles, r#"{"profiles":{}}"#).await?;
            }
            "--installClient"
        }
        LoaderSide::Server => "--installServer",
    };

    let output = Command::new(&java_exe)
        .arg("-jar")
        .arg(installer_jar)
        .arg(flag)
        .arg(work_dir)
        .current_dir(work_dir)
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(DemingError::Process(format!(
            "installer {} {flag} failed: {stderr}\n{stdout}",
            installer_jar.display()
        )));
    }

    match side {
        LoaderSide::Client => {
            let vanilla = mojang
                .ensure_client_with_progress(mc_version, cache_dir, progress)
                .await?;
            let mut profile = build_vanilla_client_profile(&vanilla);
            if let Some(installed_json) = find_installed_version_json(work_dir)? {
                profile.main_class = installed_json.main_class;
                if let Some(args) = installed_json.arguments {
                    let os_name = current_mojang_os();
                    let os_arch = current_arch();
                    profile
                        .jvm_args
                        .extend(evaluate_arguments(&args.jvm, os_name, os_arch));
                    profile
                        .game_args
                        .extend(evaluate_arguments(&args.game, os_name, os_arch));
                }
                for lib in installed_json.libraries {
                    if let Some(rel) = maven_coord_to_path(&lib.name) {
                        let candidate = work_dir.join("libraries").join(&rel);
                        if candidate.is_file() {
                            profile.classpath.push(candidate);
                        }
                    }
                }
            }
            Ok(profile)
        }
        LoaderSide::Server => {
            let vjson = mojang.fetch_version_json(mc_version, cache_dir).await?;
            let args_file = find_server_args_file(work_dir);
            let mut jvm_args = Vec::new();
            if let Some(ref af) = args_file {
                jvm_args.push(format!("@{}", af.display()));
            }
            Ok(LaunchProfile {
                main_class: String::new(),
                classpath: Vec::new(),
                jvm_args,
                game_args: vec!["nogui".to_string()],
                asset_index: vjson.asset_index.id,
                assets_dir: cache_dir.join("assets"),
                natives_dir: cache_dir.join("natives").join(mc_version),
                server_jar: None,
                java_major_version: java_major,
            })
        }
    }
}

fn neoforge_prefix_for_mc(mc_version: &str) -> Option<String> {
    let mut parts = mc_version.split('.');
    let _major = parts.next()?;
    let minor = parts.next()?;
    let patch = parts.next().unwrap_or("0");
    Some(format!("{minor}.{patch}."))
}

fn find_installed_version_json(work_dir: &Path) -> Result<Option<VersionJson>> {
    let versions_dir = work_dir.join("versions");
    if !versions_dir.is_dir() {
        return Ok(None);
    }
    for entry in std::fs::read_dir(&versions_dir)?.flatten() {
        let dir = entry.path();
        if dir.is_dir() {
            if let Some(name) = dir.file_name().and_then(|s| s.to_str()) {
                let candidate = dir.join(format!("{name}.json"));
                if candidate.is_file() {
                    let raw = std::fs::read_to_string(&candidate)?;
                    if let Ok(parsed) = serde_json::from_str::<VersionJson>(&raw) {
                        return Ok(Some(parsed));
                    }
                }
            }
        }
    }
    Ok(None)
}

fn find_server_args_file(work_dir: &Path) -> Option<PathBuf> {
    let target_name = if cfg!(target_os = "windows") {
        "win_args.txt"
    } else {
        "unix_args.txt"
    };
    let libs = work_dir.join("libraries");
    find_file_recursive(&libs, target_name)
}

fn find_file_recursive(dir: &Path, target_name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(found) = find_file_recursive(&p, target_name) {
                return Some(found);
            }
        } else if p.file_name().and_then(|s| s.to_str()) == Some(target_name) {
            return Some(p);
        }
    }
    None
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

    #[test]
    fn parse_forge_promotions_and_neoforge_prefix() {
        let promos_json = r#"{
          "promos": {
            "1.21.4-latest": "54.1.0",
            "1.21.4-recommended": "54.0.26"
          }
        }"#;
        let promos: ForgePromotions = serde_json::from_str(promos_json).unwrap();
        assert_eq!(promos.promos.get("1.21.4-recommended").unwrap(), "54.0.26");

        assert_eq!(neoforge_prefix_for_mc("1.21.4"), Some("21.4.".to_string()));
        assert_eq!(neoforge_prefix_for_mc("1.21"), Some("21.0.".to_string()));
    }
}
