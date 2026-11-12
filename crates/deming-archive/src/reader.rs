use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDateTime, Utc};
use deming_core::{RunEvent, RunManifest, RunStatus};
use regex::Regex;
use uuid::Uuid;
use walkdir::WalkDir;
use zip::ZipArchive;

use crate::error::{ArchiveError, Result};

/// Summary information about a discovered run directory or `.zip` archive.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunInfo {
    /// Unique run ID if extracted from folder name or manifest.
    pub id: Option<Uuid>,
    /// Folder or archive base name without extension.
    pub name: String,
    /// Absolute or relative path to the directory or `.zip` file.
    pub path: PathBuf,
    /// Whether this run is archived as a compressed `.zip` file.
    pub is_compressed: bool,
    /// Run creation timestamp, if known.
    pub created_at: Option<DateTime<Utc>>,
    /// Status of the run, if known from manifest.
    pub status: Option<RunStatus>,
    /// Job name, if known from manifest.
    pub job_name: Option<String>,
}

/// Search filter parameters for inspecting archived log files.
#[derive(Debug, Clone, Default)]
pub struct LogSearchFilter {
    /// If specified, filters logs to only this instance (or "server").
    pub instance_id: Option<String>,
    /// If specified, filters logs to a specific log filename (e.g. "latest.log", "stdout.log").
    pub log_file: Option<String>,
    /// Regular expression pattern to match.
    pub regex: Option<String>,
    /// Whether regex matching is case sensitive (default false).
    pub case_sensitive: bool,
    /// Whether to include server logs when `instance_id` is not set (default true).
    pub include_server: bool,
    /// Maximum number of matching lines to return.
    pub limit: Option<usize>,
}

/// A single matched log line from a search query.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LogMatch {
    /// Source of the log: "server" or an `instance_id`.
    pub source: String,
    /// Log filename (e.g. "latest.log", "stdout.log", "stderr.log").
    pub log_file: String,
    /// 1-based line number within the log file.
    pub line_number: usize,
    /// The matching text line.
    pub line: String,
}

/// Information about a screenshot captured during a test run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScreenshotInfo {
    /// Identifier of the instance that produced the screenshot.
    pub instance_id: String,
    /// Base filename of the screenshot (e.g. `shot_1.png`).
    pub filename: String,
    /// Relative path inside the run archive.
    pub relative_path: String,
    /// File size in bytes.
    pub size_bytes: u64,
}

/// Reader and query engine for inspecting historical Deming test run archives.
#[derive(Debug, Clone)]
pub struct RunArchiveReader {
    runs_dir: PathBuf,
}

impl RunArchiveReader {
    /// Creates a new reader pointing to `runs_dir`.
    pub fn new(runs_dir: impl AsRef<Path>) -> Self {
        Self {
            runs_dir: runs_dir.as_ref().to_path_buf(),
        }
    }

    /// Returns the base runs directory.
    pub fn runs_dir(&self) -> &Path {
        &self.runs_dir
    }

    /// Lists all run archives (both uncompressed directories and `.zip` files) sorted newest first.
    pub fn list_runs(&self) -> Result<Vec<RunInfo>> {
        if !self.runs_dir.exists() {
            return Ok(Vec::new());
        }

        let mut runs = Vec::new();
        for entry in fs::read_dir(&self.runs_dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_name = match path.file_name().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };

            let is_dir = path.is_dir();
            let is_zip = path.is_file() && file_name.ends_with(".zip");

            if !is_dir && !is_zip {
                continue;
            }

            let base_name = if is_zip {
                file_name.trim_end_matches(".zip").to_string()
            } else {
                file_name.clone()
            };

            let (extracted_time, extracted_id) = parse_run_name(&base_name);

            // Attempt to read manifest for richer metadata if available
            let (manifest_status, manifest_job, manifest_time, manifest_id) =
                match self.load_manifest(&path) {
                    Ok(m) => (Some(m.status), Some(m.job_name), Some(m.created_at), Some(m.run_id)),
                    Err(_) => (None, None, None, None),
                };

            let id = manifest_id.or(extracted_id);
            let created_at = manifest_time.or(extracted_time).or_else(|| {
                entry.metadata().ok().and_then(|meta| {
                    meta.modified().ok().map(|sys_time| DateTime::<Utc>::from(sys_time))
                })
            });

            runs.push(RunInfo {
                id,
                name: base_name,
                path,
                is_compressed: is_zip,
                created_at,
                status: manifest_status,
                job_name: manifest_job,
            });
        }

        // Sort descending: newest runs first
        runs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(runs)
    }

    /// Finds a run archive matching a run ID (as UUID string) or folder/file name.
    pub fn find_run(&self, id_or_name: &str) -> Result<RunInfo> {
        let runs = self.list_runs()?;
        for run in runs {
            if let Some(id) = run.id {
                if id.to_string() == id_or_name {
                    return Ok(run);
                }
            }
            if run.name == id_or_name
                || run.path.file_name().and_then(|s| s.to_str()) == Some(id_or_name)
            {
                return Ok(run);
            }
        }
        Err(ArchiveError::RunNotFound(id_or_name.to_string()))
    }

    /// Loads and parses `manifest.json` from either a run folder or a `.zip` archive.
    pub fn load_manifest(&self, run_path: impl AsRef<Path>) -> Result<RunManifest> {
        let path = run_path.as_ref();
        if path.is_dir() {
            let manifest_file = path.join("manifest.json");
            if !manifest_file.exists() {
                return Err(ArchiveError::RunNotFound(format!(
                    "manifest.json missing in {}",
                    path.display()
                )));
            }
            let data = fs::read_to_string(manifest_file)?;
            let manifest: RunManifest = serde_json::from_str(&data)?;
            Ok(manifest)
        } else if path.is_file() {
            let file = File::open(path)?;
            let mut archive = ZipArchive::new(file)?;
            for i in 0..archive.len() {
                let mut zip_file = archive.by_index(i)?;
                let name = zip_file.name();
                if name == "manifest.json" || name.ends_with("/manifest.json") {
                    let mut content = String::new();
                    zip_file.read_to_string(&mut content)?;
                    let manifest: RunManifest = serde_json::from_str(&content)?;
                    return Ok(manifest);
                }
            }
            Err(ArchiveError::InvalidArchive(format!(
                "manifest.json not found in zip archive {}",
                path.display()
            )))
        } else {
            Err(ArchiveError::RunNotFound(path.display().to_string()))
        }
    }

    /// Loads all JSON lines from `events.jsonl` into a vector of [`RunEvent`].
    pub fn read_events(&self, run_path: impl AsRef<Path>) -> Result<Vec<RunEvent>> {
        let path = run_path.as_ref();
        let mut events = Vec::new();

        if path.is_dir() {
            let events_file = path.join("events.jsonl");
            if !events_file.exists() {
                return Ok(events);
            }
            let file = File::open(events_file)?;
            let reader = BufReader::new(file);
            for line_res in reader.lines() {
                let line = line_res?;
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let event: RunEvent = serde_json::from_str(trimmed)?;
                events.push(event);
            }
        } else if path.is_file() {
            let file = File::open(path)?;
            let mut archive = ZipArchive::new(file)?;
            for i in 0..archive.len() {
                let zip_file = archive.by_index(i)?;
                let name = zip_file.name();
                if name == "events.jsonl" || name.ends_with("/events.jsonl") {
                    let reader = BufReader::new(zip_file);
                    for line_res in reader.lines() {
                        let line = line_res?;
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        let event: RunEvent = serde_json::from_str(trimmed)?;
                        events.push(event);
                    }
                    break;
                }
            }
        }

        Ok(events)
    }

    /// Searches log files in the run archive matching the specified [`LogSearchFilter`].
    pub fn search_logs(
        &self,
        run_path: impl AsRef<Path>,
        filter: &LogSearchFilter,
    ) -> Result<Vec<LogMatch>> {
        let path = run_path.as_ref();
        let compiled_regex = if let Some(ref pattern) = filter.regex {
            if pattern.trim().is_empty() {
                None
            } else {
                let pat = if filter.case_sensitive {
                    pattern.clone()
                } else {
                    format!("(?i){}", pattern)
                };
                Some(Regex::new(&pat)?)
            }
        } else {
            None
        };

        let mut matches = Vec::new();

        if path.is_dir() {
            for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
                if !entry.file_type().is_file() {
                    continue;
                }
                let entry_path = entry.path();
                let ext = entry_path.extension().and_then(|s| s.to_str()).unwrap_or("");
                if ext != "log" {
                    continue;
                }

                let relative = match entry_path.strip_prefix(path) {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                let (source, log_file) = match parse_log_path(relative) {
                    Some(pair) => pair,
                    None => continue,
                };

                if !should_include_source(&source, filter) {
                    continue;
                }
                if let Some(ref target_file) = filter.log_file {
                    if &log_file != target_file {
                        continue;
                    }
                }

                let file = File::open(entry_path)?;
                let reader = BufReader::new(file);
                for (idx, line_res) in reader.lines().enumerate() {
                    let line = line_res?;
                    let is_match = match compiled_regex {
                        Some(ref re) => re.is_match(&line),
                        None => true,
                    };
                    if is_match {
                        matches.push(LogMatch {
                            source: source.clone(),
                            log_file: log_file.clone(),
                            line_number: idx + 1,
                            line,
                        });
                        if let Some(limit) = filter.limit {
                            if matches.len() >= limit {
                                return Ok(matches);
                            }
                        }
                    }
                }
            }
        } else if path.is_file() {
            let file = File::open(path)?;
            let mut archive = ZipArchive::new(file)?;
            for i in 0..archive.len() {
                let zip_file = archive.by_index(i)?;
                let zip_name = zip_file.name();
                if !zip_name.ends_with(".log") {
                    continue;
                }

                let rel_path = Path::new(zip_name);
                let (source, log_file) = match parse_log_path(rel_path) {
                    Some(pair) => pair,
                    None => continue,
                };

                if !should_include_source(&source, filter) {
                    continue;
                }
                if let Some(ref target_file) = filter.log_file {
                    if &log_file != target_file {
                        continue;
                    }
                }

                let reader = BufReader::new(zip_file);
                for (idx, line_res) in reader.lines().enumerate() {
                    let line = line_res?;
                    let is_match = match compiled_regex {
                        Some(ref re) => re.is_match(&line),
                        None => true,
                    };
                    if is_match {
                        matches.push(LogMatch {
                            source: source.clone(),
                            log_file: log_file.clone(),
                            line_number: idx + 1,
                            line,
                        });
                        if let Some(limit) = filter.limit {
                            if matches.len() >= limit {
                                return Ok(matches);
                            }
                        }
                    }
                }
            }
        }

        Ok(matches)
    }

    /// Tails the last `max_lines` from a log file of `source` ("server" or instance ID).
    pub fn tail_logs(
        &self,
        run_path: impl AsRef<Path>,
        source: &str,
        log_file: Option<&str>,
        max_lines: usize,
    ) -> Result<Vec<String>> {
        let target_file = log_file.unwrap_or("latest.log");
        let path = run_path.as_ref();
        let mut lines = Vec::new();

        if path.is_dir() {
            let log_path = if source == "server" {
                path.join("server").join(target_file)
            } else {
                path.join("instances").join(source).join(target_file)
            };

            if !log_path.exists() {
                return Ok(Vec::new());
            }

            let file = File::open(log_path)?;
            let reader = BufReader::new(file);
            for l in reader.lines() {
                lines.push(l?);
            }
        } else if path.is_file() {
            let file = File::open(path)?;
            let mut archive = ZipArchive::new(file)?;
            for i in 0..archive.len() {
                let zip_file = archive.by_index(i)?;
                let name = zip_file.name();
                let rel = Path::new(name);
                if let Some((s, f)) = parse_log_path(rel) {
                    if s == source && f == target_file {
                        let reader = BufReader::new(zip_file);
                        for l in reader.lines() {
                            lines.push(l?);
                        }
                        break;
                    }
                }
            }
        }

        if lines.len() > max_lines {
            let start = lines.len() - max_lines;
            Ok(lines[start..].to_vec())
        } else {
            Ok(lines)
        }
    }

    /// Lists screenshots captured during the run, optionally filtered by `instance_id`.
    pub fn list_screenshots(
        &self,
        run_path: impl AsRef<Path>,
        instance_id: Option<&str>,
    ) -> Result<Vec<ScreenshotInfo>> {
        let path = run_path.as_ref();
        let mut screenshots = Vec::new();

        if path.is_dir() {
            for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
                if !entry.file_type().is_file() {
                    continue;
                }
                let entry_path = entry.path();
                let relative = match entry_path.strip_prefix(path) {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                let components: Vec<&str> = relative
                    .iter()
                    .filter_map(|c| c.to_str())
                    .collect();

                // Expect pattern: ["instances", "<inst_id>", "screenshots", "<filename>"]
                // or with prefix components
                let inst_idx = components.iter().position(|&c| c == "instances");
                if let Some(i) = inst_idx {
                    if components.len() >= i + 4 && components[i + 2] == "screenshots" {
                        let inst = components[i + 1].to_string();
                        let filename = components[i + 3].to_string();
                        if let Some(target_inst) = instance_id {
                            if inst != target_inst {
                                continue;
                            }
                        }
                        let size_bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
                        screenshots.push(ScreenshotInfo {
                            instance_id: inst,
                            filename,
                            relative_path: relative.to_string_lossy().replace('\\', "/"),
                            size_bytes,
                        });
                    }
                }
            }
        } else if path.is_file() {
            let file = File::open(path)?;
            let mut archive = ZipArchive::new(file)?;
            for i in 0..archive.len() {
                let zip_file = archive.by_index(i)?;
                let name = zip_file.name();
                if zip_file.is_dir() {
                    continue;
                }

                let normalized = name.replace('\\', "/");
                let parts: Vec<&str> = normalized.split('/').collect();
                let inst_idx = parts.iter().position(|&c| c == "instances");
                if let Some(idx) = inst_idx {
                    if parts.len() >= idx + 4 && parts[idx + 2] == "screenshots" {
                        let inst = parts[idx + 1].to_string();
                        let filename = parts[idx + 3].to_string();
                        if let Some(target_inst) = instance_id {
                            if inst != target_inst {
                                continue;
                            }
                        }
                        screenshots.push(ScreenshotInfo {
                            instance_id: inst,
                            filename,
                            relative_path: normalized,
                            size_bytes: zip_file.size(),
                        });
                    }
                }
            }
        }

        Ok(screenshots)
    }
}

/// Helper parsing run timestamp and UUID from folder/stem name: `<timestamp>_<uuid>`.
fn parse_run_name(name: &str) -> (Option<DateTime<Utc>>, Option<Uuid>) {
    let parts: Vec<&str> = name.split('_').collect();
    if parts.len() >= 3 {
        // e.g. "20261112_223000_<uuid>"
        let ts_str = format!("{}_{}", parts[0], parts[1]);
        let dt = NaiveDateTime::parse_from_str(&ts_str, "%Y%m%d_%H%M%S")
            .ok()
            .map(|ndt| DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc));
        let uuid = Uuid::parse_str(parts[2]).ok();
        (dt, uuid)
    } else if parts.len() == 2 {
        let dt = NaiveDateTime::parse_from_str(parts[0], "%Y%m%d")
            .ok()
            .map(|ndt| DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc));
        let uuid = Uuid::parse_str(parts[1]).ok();
        (dt, uuid)
    } else {
        (None, Uuid::parse_str(name).ok())
    }
}

/// Parses source ("server" or instance_id) and log filename from a relative path.
fn parse_log_path(rel_path: &Path) -> Option<(String, String)> {
    let comps: Vec<&str> = rel_path.iter().filter_map(|c| c.to_str()).collect();
    let filename = comps.last()?.to_string();

    // Check if under server/
    if let Some(pos) = comps.iter().position(|&c| c == "server") {
        if pos + 1 < comps.len() {
            return Some(("server".to_string(), filename));
        }
    }

    // Check if under instances/<id>/...
    if let Some(pos) = comps.iter().position(|&c| c == "instances") {
        if pos + 2 < comps.len() {
            let instance_id = comps[pos + 1].to_string();
            return Some((instance_id, filename));
        }
    }

    None
}

/// Determines whether a source should be included based on filter settings.
fn should_include_source(source: &str, filter: &LogSearchFilter) -> bool {
    if let Some(ref target_inst) = filter.instance_id {
        source == target_inst
    } else if source == "server" {
        filter.include_server
    } else {
        true
    }
}
