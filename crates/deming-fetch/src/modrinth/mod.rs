//! Mod resolution for `ModSource::LocalPath` and `ModSource::Modrinth`.

pub mod api;
pub mod install;

pub use api::{
    modrinth_loader_slug, ModSearchHit, ModrinthClient, ModrinthFile, ModrinthHashes,
    ModrinthVersion, MODRINTH_API_V2,
};
pub use install::{mod_cache_path, resolve_mods, resolve_mods_with_progress, ResolvedMod};

pub(crate) use api::resolve_modrinth_metadata;
pub(crate) use install::resolve_modrinth_to_blob;
