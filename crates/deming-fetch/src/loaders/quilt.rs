//! Quilt mod loader installation (delegates to shared Fabric/Quilt Meta resolver).

use std::path::Path;

use deming_core::Result;

use super::fabric::install_fabric_or_quilt;
use super::{LaunchProfile, LoaderSide};
use crate::mojang::MojangClient;
use crate::ProgressCallback;

pub const QUILT_META_BASE: &str = "https://meta.quiltmc.org/v3";
pub const QUILT_MAVEN_REPO: &str = "https://maven.quiltmc.org/repository/release";

pub(crate) async fn install_quilt(
    mojang: &MojangClient,
    mc_version: &str,
    loader_version: Option<&str>,
    side: LoaderSide,
    cache_dir: &Path,
    progress: Option<ProgressCallback>,
) -> Result<LaunchProfile> {
    install_fabric_or_quilt(
        mojang,
        QUILT_META_BASE,
        QUILT_MAVEN_REPO,
        mc_version,
        loader_version,
        side,
        cache_dir,
        false,
        progress,
    )
    .await
}
