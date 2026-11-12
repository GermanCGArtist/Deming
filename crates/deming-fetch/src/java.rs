//! Java runtime detection and Temurin/Adoptium JRE provisioning.

use std::path::{Path, PathBuf};
use std::process::Command;

use deming_core::{DemingError, Result};
use serde::{Deserialize, Serialize};

use crate::util::{build_http_client, current_arch, download_file, fetch_json};

/// Adoptium API v3 base URL for Eclipse Temurin JRE releases.
pub const ADOPTIUM_API_BASE: &str = "https://api.adoptium.net/v3";

/// A detected or provisioned Java runtime on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JavaRuntime {
    pub major_version: u32,
    pub version_string: String,
    pub executable: PathBuf,
}

/// Subset of the Adoptium v3 `/assets/latest/{feature_version}/hotspot` response.
#[derive(Debug, Clone, Deserialize)]
pub struct AdoptiumAsset {
    pub binary: AdoptiumBinary,
    pub release_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdoptiumBinary {
    pub package: AdoptiumPackage,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdoptiumPackage {
    pub checksum: String,
    pub link: String,
    pub name: String,
}

/// Returns the required Java major version (8, 17, 21, or 25) for a given Minecraft version.
pub fn required_java_version(mc_version: &str) -> u32 {
    let parts: Vec<u32> = mc_version
        .split('.')
        .filter_map(|p| p.parse::<u32>().ok())
        .collect();
    let major = parts.first().copied().unwrap_or(1);
    if major >= 26 {
        return 25;
    }

    let minor = parts.get(1).copied().unwrap_or(21);
    let patch = parts.get(2).copied().unwrap_or(0);

    if minor < 17 {
        8
    } else if minor < 20 || (minor == 20 && patch <= 4) {
        17
    } else {
        21
    }
}

/// Parses `java -version` stderr/stdout output into `(major_version, full_version_string)`.
///
/// Supports both legacy (`"1.8.0_392"`) and modern (`"17.0.12"`, `"21.0.5"`) version formats.
pub fn parse_java_version_output(output: &str) -> Option<(u32, String)> {
    let start = output.find('"')? + 1;
    let rest = &output[start..];
    let end = rest.find('"')?;
    let ver_str = rest[..end].trim().to_string();

    let mut nums = ver_str.split(['.', '_', '-', '+']);
    let first: u32 = nums.next()?.parse().ok()?;
    let major = if first == 1 {
        nums.next().and_then(|s| s.parse().ok()).unwrap_or(8)
    } else {
        first
    };
    Some((major, ver_str))
}

/// Probes a `java` executable by running `java -version`.
pub fn probe_java_executable(executable: &Path) -> Option<JavaRuntime> {
    if !executable.is_file() && executable != Path::new("java") {
        return None;
    }
    let out = Command::new(executable).arg("-version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let combined = if stderr.trim().is_empty() {
        stdout.as_ref()
    } else {
        stderr.as_ref()
    };
    let (major_version, version_string) = parse_java_version_output(combined)?;
    Some(JavaRuntime {
        major_version,
        version_string,
        executable: executable.to_path_buf(),
    })
}

/// Detects installed Java runtimes from `cache_dir/runtimes/`, `JAVA_HOME`, `PATH`,
/// and standard system installation directories.
pub fn detect_installed_java(cache_dir: Option<&Path>) -> Vec<JavaRuntime> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let exe_name = java_exe_name();

    if let Some(cache) = cache_dir {
        let runtimes_dir = cache.join("runtimes");
        if let Ok(entries) = std::fs::read_dir(&runtimes_dir) {
            for entry in entries.flatten() {
                if let Some(exe) = find_java_exe_under(&entry.path()) {
                    candidates.push(exe);
                }
            }
        }
    }

    if let Ok(java_home) = std::env::var("JAVA_HOME") {
        candidates.push(PathBuf::from(java_home).join("bin").join(exe_name));
    }

    candidates.push(PathBuf::from(exe_name));

    #[cfg(target_os = "windows")]
    {
        let mut bases: Vec<PathBuf> = vec![
            PathBuf::from(r"C:\Program Files\Eclipse Adoptium"),
            PathBuf::from(r"C:\Program Files\Java"),
            PathBuf::from(r"C:\Program Files\Microsoft"),
            PathBuf::from(r"C:\Program Files\Zulu"),
        ];
        if let Ok(appdata) = std::env::var("APPDATA") {
            bases.push(
                PathBuf::from(appdata)
                    .join("ModrinthApp")
                    .join("meta")
                    .join("java_versions"),
            );
        }
        for base in bases {
            collect_java_candidates_in_dir(&base, &mut candidates);
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        for base in ["/usr/lib/jvm", "/Library/Java/JavaVirtualMachines"] {
            if let Ok(entries) = std::fs::read_dir(base) {
                for entry in entries.flatten() {
                    candidates.push(entry.path().join("bin").join(exe_name));
                    candidates.push(entry.path().join("Contents/Home/bin").join(exe_name));
                }
            }
        }
    }

    let mut runtimes = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for cand in candidates {
        let key = std::fs::canonicalize(&cand).unwrap_or_else(|_| cand.clone());
        if !seen.insert(key) {
            continue;
        }
        if let Some(rt) = probe_java_executable(&cand) {
            runtimes.push(rt);
        }
    }
    runtimes
}

/// Ensures a Java runtime matching `required_major` is available, downloading a Temurin JRE
/// into `<cache_dir>/runtimes/java-<required_major>/` if no matching runtime is installed.
pub async fn ensure_java_runtime(cache_dir: &Path, required_major: u32) -> Result<JavaRuntime> {
    let installed = detect_installed_java(Some(cache_dir));
    if let Some(found) = installed
        .into_iter()
        .find(|rt| rt.major_version == required_major)
    {
        return Ok(found);
    }

    let os = adoptium_os();
    let arch = current_arch();
    let api_url = format!(
        "{ADOPTIUM_API_BASE}/assets/latest/{required_major}/hotspot?architecture={arch}&image_type=jre&os={os}&vendor=eclipse"
    );

    let client = build_http_client()?;
    let assets: Vec<AdoptiumAsset> = fetch_json(&client, &api_url).await?;
    let asset = assets.into_iter().next().ok_or_else(|| {
        DemingError::Network(format!(
            "no Adoptium JRE {required_major} asset found for {os}/{arch}"
        ))
    })?;

    let runtime_dir = runtime_install_dir(cache_dir, required_major);
    let archive_path = cache_dir
        .join("runtimes")
        .join(&asset.binary.package.name);

    download_file(
        &client,
        &asset.binary.package.link,
        &archive_path,
        None,
        Some(&asset.binary.package.checksum),
        false,
    )
    .await?;

    extract_jre_archive(&archive_path, &runtime_dir)?;

    let exe = find_java_exe_under(&runtime_dir).ok_or_else(|| {
        DemingError::Process(format!(
            "extracted JRE {required_major} in {} but could not locate {}",
            runtime_dir.display(),
            java_exe_name()
        ))
    })?;

    probe_java_executable(&exe).ok_or_else(|| {
        DemingError::Process(format!(
            "downloaded Java executable {} failed `java -version` check",
            exe.display()
        ))
    })
}

/// Returns `<cache_dir>/runtimes/java-<major>`.
pub fn runtime_install_dir(cache_dir: &Path, major_version: u32) -> PathBuf {
    cache_dir
        .join("runtimes")
        .join(format!("java-{major_version}"))
}

fn java_exe_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "java.exe"
    } else {
        "java"
    }
}

fn adoptium_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    }
}

fn collect_java_candidates_in_dir(base: &Path, candidates: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(base) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                if let Some(exe) = find_java_exe_under(&p) {
                    candidates.push(exe);
                }
            }
        }
    }
}

fn find_java_exe_under(dir: &Path) -> Option<PathBuf> {
    let exe_name = java_exe_name();
    let direct = dir.join("bin").join(exe_name);
    if direct.is_file() {
        return Some(direct);
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                let nested = p.join("bin").join(exe_name);
                if nested.is_file() {
                    return Some(nested);
                }
                let mac_nested = p.join("Contents/Home/bin").join(exe_name);
                if mac_nested.is_file() {
                    return Some(mac_nested);
                }
            }
        }
    }
    None
}

fn extract_jre_archive(archive_path: &Path, dest_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dest_dir)?;
    let name = archive_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();

    if name.ends_with(".zip") {
        let file = std::fs::File::open(archive_path)?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| DemingError::Other(format!("failed to open JRE zip: {e}")))?;
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| DemingError::Other(format!("zip entry error: {e}")))?;
            let Some(enclosed) = entry.enclosed_name() else {
                continue;
            };
            let out_path = dest_dir.join(enclosed);
            if entry.is_dir() {
                std::fs::create_dir_all(&out_path)?;
            } else {
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut out_file = std::fs::File::create(&out_path)?;
                std::io::copy(&mut entry, &mut out_file)?;
            }
        }
        return Ok(());
    }

    // Fallback to system `tar` for `.tar.gz` on Linux/macOS
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(archive_path)
        .arg("-C")
        .arg(dest_dir)
        .status()?;
    if !status.success() {
        return Err(DemingError::Process(format!(
            "failed to extract JRE archive {}",
            archive_path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_version_mapping_for_mc_versions() {
        assert_eq!(required_java_version("1.12.2"), 8);
        assert_eq!(required_java_version("1.16.5"), 8);
        assert_eq!(required_java_version("1.18.2"), 17);
        assert_eq!(required_java_version("1.20.1"), 17);
        assert_eq!(required_java_version("1.20.4"), 17);
        assert_eq!(required_java_version("1.20.5"), 21);
        assert_eq!(required_java_version("1.21.4"), 21);
        assert_eq!(required_java_version("26.1.1"), 25);
    }

    #[test]
    fn parse_java_version_strings() {
        let java8 = "java version \"1.8.0_392\"\nJava(TM) SE Runtime Environment";
        assert_eq!(
            parse_java_version_output(java8),
            Some((8, "1.8.0_392".to_string()))
        );

        let java17 = "openjdk version \"17.0.12\" 2024-07-16\nOpenJDK Runtime Environment";
        assert_eq!(
            parse_java_version_output(java17),
            Some((17, "17.0.12".to_string()))
        );

        let java21 = "openjdk version \"21.0.5\" 2024-10-15 LTS\nOpenJDK Runtime Environment Temurin-21.0.5+11";
        assert_eq!(
            parse_java_version_output(java21),
            Some((21, "21.0.5".to_string()))
        );
    }

    #[test]
    fn parse_adoptium_asset_fixture() {
        let fixture = r#"[{
          "release_name": "jdk-21.0.5+11",
          "binary": {
            "package": {
              "checksum": "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
              "link": "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.5/OpenJDK21U-jre_x64_windows_hotspot_21.0.5_11.zip",
              "name": "OpenJDK21U-jre_x64_windows_hotspot_21.0.5_11.zip"
            }
          }
        }]"#;
        let parsed: Vec<AdoptiumAsset> = serde_json::from_str(fixture).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].release_name, "jdk-21.0.5+11");
        assert!(parsed[0].binary.package.name.ends_with(".zip"));
    }

    #[test]
    fn runtime_cache_path() {
        let cache = Path::new("C:/deming/cache");
        assert_eq!(
            runtime_install_dir(cache, 21),
            PathBuf::from("C:/deming/cache/runtimes/java-21")
        );
    }

    #[test]
    fn collects_modrinth_style_java_versions_dir() {
        let temp = std::env::temp_dir().join(format!(
            "deming-java-scan-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let bin_dir = temp
            .join("zulu25.32.21-ca-jre25.0.2-win_x64")
            .join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let fake_java = bin_dir.join(java_exe_name());
        std::fs::write(&fake_java, b"").unwrap();

        let mut found = Vec::new();
        collect_java_candidates_in_dir(&temp, &mut found);
        assert_eq!(found, vec![fake_java]);

        let _ = std::fs::remove_dir_all(&temp);
    }
}
