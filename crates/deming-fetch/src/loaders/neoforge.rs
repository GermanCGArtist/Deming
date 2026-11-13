//! NeoForge mod loader installation.

use std::path::Path;

use deming_core::{DemingError, Result};
use serde::Deserialize;

use super::forge::run_forge_installer;
use super::{LaunchProfile, LoaderSide};
use crate::mojang::MojangClient;
use crate::util::{build_http_client, download_file, fetch_json};
use crate::{report_progress, ProgressCallback};

pub const NEOFORGE_MAVEN_API: &str =
    "https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/neoforge";
pub const NEOFORGE_MAVEN_REPO: &str = "https://maven.neoforged.net/releases";

/// NeoForge Maven versions API response.
#[derive(Debug, Clone, Deserialize)]
pub struct NeoForgeVersionsResponse {
    pub versions: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn install_neoforge(
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

pub(crate) fn neoforge_prefix_for_mc(mc_version: &str) -> Option<String> {
    let mut parts = mc_version.split('.');
    let _major = parts.next()?;
    let minor = parts.next()?;
    let patch = parts.next().unwrap_or("0");
    Some(format!("{minor}.{patch}."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neoforge_prefix_derivation() {
        assert_eq!(neoforge_prefix_for_mc("1.21.4"), Some("21.4.".to_string()));
        assert_eq!(neoforge_prefix_for_mc("1.21"), Some("21.0.".to_string()));
    }
}
