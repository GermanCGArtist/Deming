//! Mojang Piston Meta manifest, version JSON, libraries, assets, and JAR fetcher.

pub mod downloads;
pub mod libraries;
pub mod manifest;

pub use downloads::{MojangClient, ResolvedVanilla};
pub use libraries::{evaluate_arguments, is_rule_list_allowed};
pub use manifest::{
    asset_index_path, client_jar_path, server_jar_path, version_json_path, ArgumentValue,
    AssetIndexInfo, AssetIndexJson, AssetObject, DownloadArtifact, JavaVersionInfo, LatestVersions,
    Library, LibraryDownloads, OsRule, Rule, StringOrVec, VersionArguments, VersionDownloads,
    VersionEntry, VersionJson, VersionManifest, PISTON_META_MANIFEST_URL, RESOURCES_BASE_URL,
};
