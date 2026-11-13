//! Minecraft artifact, mod loader, Java runtime, and Modrinth mod fetcher for Deming.

use std::sync::Arc;

pub mod java;
pub mod loaders;
pub mod modrinth;
pub mod mojang;
mod util;

/// Optional callback invoked as downloads/extractions progress: `(completed, total, message)`.
pub type ProgressCallback = Arc<dyn Fn(u64, u64, &str) + Send + Sync>;

pub(crate) fn report_progress(
    progress: Option<&ProgressCallback>,
    completed: u64,
    total: u64,
    message: &str,
) {
    if let Some(cb) = progress {
        cb(completed, total, message);
    }
}

pub use java::{
    detect_installed_java, ensure_java_runtime, ensure_java_runtime_with_progress,
    parse_java_version_output, required_java_version, JavaRuntime,
};
pub use loaders::{
    resolve_and_install_loader, resolve_and_install_loader_with_progress, LaunchProfile,
    LoaderSide,
};
pub use modrinth::{
    mod_cache_path, resolve_mods, resolve_mods_with_progress, ModrinthClient, ModrinthVersion,
    ResolvedMod,
};
pub use mojang::{
    asset_index_path, client_jar_path, server_jar_path, version_json_path, MojangClient,
    ResolvedVanilla, VersionJson, VersionManifest,
};
