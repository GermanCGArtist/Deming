//! Minecraft artifact, mod loader, Java runtime, and Modrinth mod fetcher for Deming.

pub mod java;
pub mod loaders;
pub mod modrinth;
pub mod mojang;
mod util;

pub use java::{
    detect_installed_java, ensure_java_runtime, parse_java_version_output, required_java_version,
    JavaRuntime,
};
pub use loaders::{resolve_and_install_loader, LaunchProfile, LoaderSide};
pub use modrinth::{mod_cache_path, resolve_mods, ModrinthClient, ModrinthVersion, ResolvedMod};
pub use mojang::{
    asset_index_path, client_jar_path, server_jar_path, version_json_path, MojangClient,
    ResolvedVanilla, VersionJson, VersionManifest,
};
