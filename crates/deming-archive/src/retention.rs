use std::fs::{self, File};
use std::path::{Path, PathBuf};

use deming_core::RetentionPolicy;
use walkdir::WalkDir;
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::CompressionMethod;

use crate::error::Result;
use crate::reader::RunArchiveReader;

/// Summary report returned after applying artifact retention rules.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct RetentionReport {
    /// List of run archives newly compressed to `.zip` files.
    pub compressed: Vec<PathBuf>,
    /// List of run archives pruned (permanently deleted) from disk.
    pub pruned: Vec<PathBuf>,
}

/// Applies the given [`RetentionPolicy`] to the runs in `runs_dir`.
///
/// 1. Finds all runs sorted descending by age (newest first).
/// 2. If `policy.auto_prune` is true, deletes all runs beyond `policy.max_runs`.
/// 3. Compresses any remaining uncompressed run folders older than `policy.compress_after_runs`
///    into `.zip` archives and deletes the original uncompressed directory.
pub fn apply_retention(
    runs_dir: impl AsRef<Path>,
    policy: &RetentionPolicy,
) -> Result<RetentionReport> {
    let runs_dir = runs_dir.as_ref();
    if !runs_dir.exists() {
        return Ok(RetentionReport::default());
    }

    let reader = RunArchiveReader::new(runs_dir);
    let mut runs = reader.list_runs()?;

    let mut report = RetentionReport::default();

    // 1. Prune runs beyond max_runs if auto_prune is true
    if policy.auto_prune && runs.len() > policy.max_runs {
        let runs_to_prune = runs.split_off(policy.max_runs);
        for run in runs_to_prune {
            if run.path.is_dir() {
                fs::remove_dir_all(&run.path)?;
            } else if run.path.is_file() {
                fs::remove_file(&run.path)?;
            }
            report.pruned.push(run.path);
        }
    }

    // 2. Compress runs older than compress_after_runs
    for (idx, run) in runs.iter().enumerate() {
        if idx >= policy.compress_after_runs && !run.is_compressed && run.path.is_dir() {
            let zip_path = compress_run_dir(&run.path)?;
            report.compressed.push(zip_path);
        }
    }

    Ok(report)
}

/// Compresses an uncompressed run directory to `<folder>.zip` and deletes the original folder.
pub fn compress_run_dir(run_dir: &Path) -> Result<PathBuf> {
    let parent = run_dir.parent().unwrap_or_else(|| Path::new("."));
    let folder_name = run_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("run");
    let zip_path = parent.join(format!("{}.zip", folder_name));
    let temp_zip_path = parent.join(format!("{}.zip.tmp", folder_name));

    {
        let file = File::create(&temp_zip_path)?;
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        for entry in WalkDir::new(run_dir).into_iter().filter_map(|e| e.ok()) {
            let entry_path = entry.path();
            if entry_path == run_dir {
                continue;
            }

            let rel_path = match entry_path.strip_prefix(run_dir) {
                Ok(r) => r,
                Err(_) => continue,
            };

            // Standardize path with forward slashes for zip format
            let rel_str = rel_path.to_string_lossy().replace('\\', "/");

            if entry.file_type().is_dir() {
                let dir_str = format!("{}/", rel_str.trim_end_matches('/'));
                writer.add_directory(&dir_str, options)?;
            } else if entry.file_type().is_file() {
                writer.start_file(&rel_str, options)?;
                let mut f = File::open(entry_path)?;
                std::io::copy(&mut f, &mut writer)?;
            }
        }

        writer.finish()?;
    }

    // Replace target zip if it already existed, rename tmp
    if zip_path.exists() {
        let _ = fs::remove_file(&zip_path);
    }
    fs::rename(&temp_zip_path, &zip_path)?;

    // Remove the original uncompressed directory
    fs::remove_dir_all(run_dir)?;

    Ok(zip_path)
}
