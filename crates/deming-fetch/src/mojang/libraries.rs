//! OS/feature rule evaluation, library downloading, and native extraction for Mojang versions.

use std::path::{Path, PathBuf};

use deming_core::{DemingError, Result};

use super::manifest::{ArgumentValue, Library, Rule, StringOrVec};
use crate::util::{current_arch, current_mojang_os, download_file, maven_coord_to_path};
use crate::{report_progress, ProgressCallback};

/// Evaluates Mojang OS/feature rules for a library or argument entry.
///
/// If `rules` is `None` or empty, returns `true`.
/// Feature-gated rules (e.g. `is_demo_user`, `has_custom_resolution`) evaluate to `false`.
pub fn is_rule_list_allowed(rules: Option<&[Rule]>, os_name: &str, os_arch: &str) -> bool {
    let Some(rules) = rules else {
        return true;
    };
    if rules.is_empty() {
        return true;
    }

    let mut allowed = false;
    for rule in rules {
        if rule_matches(rule, os_name, os_arch) {
            allowed = rule.action == "allow";
        }
    }
    allowed
}

fn rule_matches(rule: &Rule, os_name: &str, os_arch: &str) -> bool {
    if let Some(features) = &rule.features {
        // Standard automated launches don't enable demo/quickplay/custom_resolution feature flags.
        if features.values().any(|&v| v) {
            return false;
        }
    }
    if let Some(os) = &rule.os {
        if let Some(name) = &os.name {
            if name != os_name {
                return false;
            }
        }
        if let Some(arch) = &os.arch {
            let normalized = if arch == "x86_64" { "x64" } else { arch.as_str() };
            if normalized != os_arch {
                return false;
            }
        }
    }
    true
}

/// Evaluates a list of [`ArgumentValue`] entries for the given OS/arch.
pub fn evaluate_arguments(args: &[ArgumentValue], os_name: &str, os_arch: &str) -> Vec<String> {
    let mut out = Vec::new();
    for item in args {
        match item {
            ArgumentValue::Simple(s) => out.push(s.clone()),
            ArgumentValue::Conditional { rules, value } => {
                if is_rule_list_allowed(Some(rules), os_name, os_arch) {
                    match value {
                        StringOrVec::Single(s) => out.push(s.clone()),
                        StringOrVec::Multiple(v) => out.extend(v.iter().cloned()),
                    }
                }
            }
        }
    }
    out
}

/// Downloads OS-filtered libraries and extracts legacy native classifiers into `natives_dir`.
pub(crate) async fn download_allowed_libraries(
    http: &reqwest::Client,
    libraries: &[Library],
    cache_dir: &Path,
    natives_dir: &Path,
    progress: Option<&ProgressCallback>,
) -> Result<Vec<PathBuf>> {
    let os_name = current_mojang_os();
    let os_arch = current_arch();
    tokio::fs::create_dir_all(natives_dir).await?;

    let allowed_libs: Vec<&Library> = libraries
        .iter()
        .filter(|lib| is_rule_list_allowed(lib.rules.as_deref(), os_name, os_arch))
        .collect();
    let total_libs = allowed_libs.len() as u64;

    let mut library_paths = Vec::new();
    for (idx, lib) in allowed_libs.into_iter().enumerate() {
        report_progress(
            progress,
            idx as u64,
            total_libs.max(1),
            &format!("Downloading library {}", lib.name),
        );

        if let Some(downloads) = &lib.downloads {
            if let Some(artifact) = &downloads.artifact {
                let rel_path = artifact
                    .path
                    .clone()
                    .or_else(|| maven_coord_to_path(&lib.name))
                    .ok_or_else(|| {
                        DemingError::InvalidSpec(format!(
                            "cannot determine path for library '{}'",
                            lib.name
                        ))
                    })?;
                let dest = cache_dir.join("libraries").join(&rel_path);
                download_file(http, &artifact.url, &dest, Some(&artifact.sha1), None, false)
                    .await?;
                library_paths.push(dest);
            }

            if let (Some(natives_map), Some(classifiers)) = (&lib.natives, &downloads.classifiers) {
                if let Some(classifier_tpl) = natives_map.get(os_name) {
                    let arch_bits = if os_arch == "x86" { "32" } else { "64" };
                    let classifier_key = classifier_tpl.replace("${arch}", arch_bits);
                    if let Some(native_art) = classifiers.get(&classifier_key) {
                        let rel_path = native_art.path.clone().unwrap_or_else(|| {
                            format!("natives/{}-{classifier_key}.jar", lib.name.replace(':', "_"))
                        });
                        let dest = cache_dir.join("libraries").join(&rel_path);
                        download_file(
                            http,
                            &native_art.url,
                            &dest,
                            Some(&native_art.sha1),
                            None,
                            false,
                        )
                        .await?;
                        extract_natives_jar(&dest, natives_dir)?;
                    }
                }
            }
        } else if let Some(base_url) = &lib.url {
            if let Some(rel_path) = maven_coord_to_path(&lib.name) {
                let dest = cache_dir.join("libraries").join(&rel_path);
                let url = format!("{}/{}", base_url.trim_end_matches('/'), rel_path);
                download_file(http, &url, &dest, None, None, true).await?;
                library_paths.push(dest);
            }
        }
    }

    report_progress(progress, total_libs, total_libs.max(1), "Libraries ready");
    Ok(library_paths)
}

fn extract_natives_jar(jar_path: &Path, natives_dir: &Path) -> Result<()> {
    let file = std::fs::File::open(jar_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| DemingError::Other(format!("failed to open natives jar: {e}")))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| DemingError::Other(format!("zip entry error: {e}")))?;
        let name = entry.name().to_string();
        if entry.is_dir() || name.starts_with("META-INF/") {
            continue;
        }
        let Some(file_name) = Path::new(&name).file_name() else {
            continue;
        };
        let out_path = natives_dir.join(file_name);
        let mut out_file = std::fs::File::create(&out_path)?;
        std::io::copy(&mut entry, &mut out_file)?;
    }
    Ok(())
}
