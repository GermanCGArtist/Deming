//! Forge mod loader installation and shared Forge/NeoForge headless installer runner.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use deming_core::{DemingError, Result};
use serde::Deserialize;
use tokio::process::Command;

use super::{build_vanilla_client_profile, LaunchProfile, LoaderSide};
use crate::java::{ensure_java_runtime_with_progress, required_java_version};
use crate::mojang::{evaluate_arguments, MojangClient, VersionJson};
use crate::util::{
    build_http_client, current_arch, current_mojang_os, download_file, fetch_json,
    maven_coord_to_path,
};
use crate::{report_progress, ProgressCallback};

pub const FORGE_PROMOTIONS_URL: &str =
    "https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json";
pub const FORGE_MAVEN_REPO: &str = "https://maven.minecraftforge.net";

/// Forge `promotions_slim.json` response.
#[derive(Debug, Clone, Deserialize)]
pub struct ForgePromotions {
    pub promos: BTreeMap<String, String>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn install_forge(
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
pub(crate) async fn run_forge_installer(
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
    fn parse_forge_promotions() {
        let promos_json = r#"{
          "promos": {
            "1.21.4-latest": "54.1.0",
            "1.21.4-recommended": "54.0.26"
          }
        }"#;
        let promos: ForgePromotions = serde_json::from_str(promos_json).unwrap();
        assert_eq!(promos.promos.get("1.21.4-recommended").unwrap(), "54.0.26");
    }
}
