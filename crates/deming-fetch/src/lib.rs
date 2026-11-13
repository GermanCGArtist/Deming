//! Minecraft artifact, mod loader, Java runtime, and Modrinth mod fetcher for Deming.

use std::sync::Arc;

pub mod curseforge;
pub mod java;
pub mod loaders;
pub mod modpack;
pub mod modrinth;
pub mod mojang;
pub mod sync;
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

pub use curseforge::CurseForgeClient;
pub use java::{
    detect_installed_java, ensure_java_runtime, ensure_java_runtime_with_progress,
    parse_java_version_output, required_java_version, JavaRuntime,
};
pub use loaders::{
    resolve_and_install_loader, resolve_and_install_loader_with_progress, LaunchProfile,
    LoaderSide,
};
pub use modpack::{
    export_mrpack, import_curseforge_zip, import_mrpack, CurseForgeManifest,
    ImportedCurseForgePack, MrpackFile, MrpackIndex,
};
pub use modrinth::{
    mod_cache_path, resolve_mods, resolve_mods_with_progress, ModSearchHit, ModrinthClient,
    ModrinthVersion, ResolvedMod,
};
pub use mojang::{
    asset_index_path, client_jar_path, server_jar_path, version_json_path, MojangClient,
    ResolvedVanilla, VersionJson, VersionManifest,
};
pub use sync::{sync_instance_mods, SyncDiff, SyncPlan};
pub use util::{blob_sha256_path, link_or_copy};
