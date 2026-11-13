//! Mod loader resolution and installation (`Vanilla`, `Fabric`, `Quilt`, `NeoForge`, `Forge`).

pub mod fabric;
pub mod forge;
pub mod neoforge;
pub mod quilt;

use std::path::{Path, PathBuf};

use deming_core::{DemingConfig, ModLoader, Result};
use serde::{Deserialize, Serialize};

pub use fabric::{
    FabricArguments, FabricInstallerEntry, FabricLibrary, FabricLoaderListEntry,
    FabricLoaderVersion, FabricProfileJson, FABRIC_MAVEN_REPO, FABRIC_META_BASE,
};
pub use forge::{ForgePromotions, FORGE_MAVEN_REPO, FORGE_PROMOTIONS_URL};
pub use neoforge::{NeoForgeVersionsResponse, NEOFORGE_MAVEN_API, NEOFORGE_MAVEN_REPO};
pub use quilt::{QUILT_MAVEN_REPO, QUILT_META_BASE};

use crate::java::required_java_version;
use crate::mojang::{evaluate_arguments, MojangClient, ResolvedVanilla};
use crate::util::{current_arch, current_mojang_os};
use crate::ProgressCallback;

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
        ModLoader::Vanilla => install_vanilla(&mojang, mc_version, side, &cache_dir, progress).await,
        ModLoader::Fabric => {
            fabric::install_fabric(&mojang, mc_version, loader_version, side, &cache_dir, progress)
                .await
        }
        ModLoader::Quilt => {
            quilt::install_quilt(&mojang, mc_version, loader_version, side, &cache_dir, progress)
                .await
        }
        ModLoader::NeoForge => {
            neoforge::install_neoforge(
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
            forge::install_forge(
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
