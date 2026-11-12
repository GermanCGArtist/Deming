//! Shared HTTP download, hash verification, and platform helpers.

use std::path::{Path, PathBuf};

use deming_core::{DemingError, Result};
use futures_util::StreamExt;
use sha1::{Digest as Sha1Digest, Sha1};
use sha2::Sha256;
use tokio::io::AsyncWriteExt;

/// Builds a shared `reqwest::Client` with a Deming user-agent.
pub fn build_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("deming-fetch/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| DemingError::Network(format!("failed to build HTTP client: {e}")))
}

/// Fetches and deserializes a JSON endpoint.
pub async fn fetch_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| DemingError::Network(format!("GET {url} failed: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(DemingError::Network(format!(
            "GET {url} returned HTTP {status}"
        )));
    }
    resp.json::<T>()
        .await
        .map_err(|e| DemingError::Network(format!("failed to decode JSON from {url}: {e}")))
}

/// Computes the lowercase hex SHA-1 digest of a file on disk.
pub fn compute_sha1(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)?;
    let mut hasher = Sha1::new();
    hasher.update(&bytes);
    Ok(hex::encode(hasher.finalize()))
}

/// Computes the lowercase hex SHA-256 digest of a file on disk.
pub fn compute_sha256(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hex::encode(hasher.finalize()))
}

/// Verifies whether `path` exists and matches `expected_sha1` (case-insensitive).
pub fn verify_sha1(path: &Path, expected_sha1: &str) -> Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }
    let actual = compute_sha1(path)?;
    Ok(actual.eq_ignore_ascii_case(expected_sha1))
}

/// Verifies whether `path` exists and matches `expected_sha256` (case-insensitive).
pub fn verify_sha256(path: &Path, expected_sha256: &str) -> Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }
    let actual = compute_sha256(path)?;
    Ok(actual.eq_ignore_ascii_case(expected_sha256))
}

/// Downloads `url` to `dest`, optionally verifying SHA-1 and/or SHA-256.
///
/// If `dest` already exists and matches all provided hashes (or if `skip_if_exists_without_hash`
/// is true and no hashes were given), the download is skipped.
/// Returns the computed `(sha1, sha256)` hex strings of the resulting file.
pub async fn download_file(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected_sha1: Option<&str>,
    expected_sha256: Option<&str>,
    skip_if_exists_without_hash: bool,
) -> Result<(String, String)> {
    if dest.is_file() {
        let sha1_ok = match expected_sha1 {
            Some(exp) => verify_sha1(dest, exp)?,
            None => true,
        };
        let sha256_ok = match expected_sha256 {
            Some(exp) => verify_sha256(dest, exp)?,
            None => true,
        };
        let has_hash = expected_sha1.is_some() || expected_sha256.is_some();
        if (has_hash && sha1_ok && sha256_ok) || (!has_hash && skip_if_exists_without_hash) {
            return Ok((compute_sha1(dest)?, compute_sha256(dest)?));
        }
    }

    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| DemingError::Network(format!("GET {url} failed: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(DemingError::Network(format!(
            "GET {url} returned HTTP {status}"
        )));
    }

    let tmp_path = temp_part_path(dest);
    let mut file = tokio::fs::File::create(&tmp_path).await?;
    let mut sha1_hasher = Sha1::new();
    let mut sha256_hasher = Sha256::new();
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            DemingError::Network(format!("stream error downloading {url}: {e}"))
        })?;
        sha1_hasher.update(&chunk);
        sha256_hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);

    let actual_sha1 = hex::encode(sha1_hasher.finalize());
    let actual_sha256 = hex::encode(sha256_hasher.finalize());

    if let Some(exp) = expected_sha1 {
        if !actual_sha1.eq_ignore_ascii_case(exp) {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(DemingError::Network(format!(
                "SHA-1 mismatch for {url}: expected {exp}, got {actual_sha1}"
            )));
        }
    }
    if let Some(exp) = expected_sha256 {
        if !actual_sha256.eq_ignore_ascii_case(exp) {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(DemingError::Network(format!(
                "SHA-256 mismatch for {url}: expected {exp}, got {actual_sha256}"
            )));
        }
    }

    if dest.exists() {
        let _ = tokio::fs::remove_file(dest).await;
    }
    tokio::fs::rename(&tmp_path, dest).await?;

    Ok((actual_sha1, actual_sha256))
}

fn temp_part_path(dest: &Path) -> PathBuf {
    let mut os = dest.as_os_str().to_os_string();
    os.push(".part");
    PathBuf::from(os)
}

/// Mojang OS identifier for the current platform (`windows`, `osx`, `linux`).
pub fn current_mojang_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

/// Mojang/Adoptium architecture identifier (`x64`, `x86`, `aarch64`).
pub fn current_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86") {
        "x86"
    } else {
        std::env::consts::ARCH
    }
}

/// Converts a Maven coordinate (`group:artifact:version[:classifier][@ext]`)
/// into a relative repository path (`group/path/artifact/version/artifact-version[-classifier].ext`).
pub fn maven_coord_to_path(coord: &str) -> Option<String> {
    let (coord_part, ext) = match coord.split_once('@') {
        Some((c, e)) => (c, e),
        None => (coord, "jar"),
    };
    let parts: Vec<&str> = coord_part.split(':').collect();
    if parts.len() < 3 || parts.len() > 4 {
        return None;
    }
    let group = parts[0].replace('.', "/");
    let artifact = parts[1];
    let version = parts[2];
    let classifier = parts.get(3).copied();

    let filename = match classifier {
        Some(c) if !c.is_empty() => format!("{artifact}-{version}-{c}.{ext}"),
        _ => format!("{artifact}-{version}.{ext}"),
    };
    Some(format!("{group}/{artifact}/{version}/{filename}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maven_coord_conversion() {
        assert_eq!(
            maven_coord_to_path("net.fabricmc:fabric-loader:0.16.10"),
            Some("net/fabricmc/fabric-loader/0.16.10/fabric-loader-0.16.10.jar".to_string())
        );
        assert_eq!(
            maven_coord_to_path("org.lwjgl:lwjgl:3.3.3:natives-windows"),
            Some("org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-windows.jar".to_string())
        );
        assert_eq!(
            maven_coord_to_path("de.oceanlabs.mcp:mcp_config:1.21.4@zip"),
            Some("de/oceanlabs/mcp/mcp_config/1.21.4/mcp_config-1.21.4.zip".to_string())
        );
        assert_eq!(maven_coord_to_path("invalid"), None);
    }
}
