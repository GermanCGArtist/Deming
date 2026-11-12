use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use deming_core::{RunEvent, RunManifest};
use uuid::Uuid;

use crate::error::Result;

/// Writer for persisting and organizing artifacts from a single test run.
#[derive(Debug, Clone)]
pub struct RunArchiveWriter {
    runs_dir: PathBuf,
    run_dir: PathBuf,
    run_id: Uuid,
    timestamp: DateTime<Utc>,
}

impl RunArchiveWriter {
    /// Creates a new run archive in `runs_dir` with the current UTC timestamp: `<runs_dir>/<timestamp>_<run_id>/`.
    pub fn new(runs_dir: impl AsRef<Path>, run_id: Uuid) -> Result<Self> {
        Self::with_timestamp(runs_dir, run_id, Utc::now())
    }

    /// Creates a new run archive directory with an explicit UTC timestamp.
    pub fn with_timestamp(
        runs_dir: impl AsRef<Path>,
        run_id: Uuid,
        timestamp: DateTime<Utc>,
    ) -> Result<Self> {
        let runs_dir = runs_dir.as_ref().to_path_buf();
        let folder_name = format!("{}_{}", timestamp.format("%Y%m%d_%H%M%S"), run_id);
        let run_dir = runs_dir.join(folder_name);

        fs::create_dir_all(&run_dir)?;
        fs::create_dir_all(run_dir.join("server"))?;
        fs::create_dir_all(run_dir.join("instances"))?;

        Ok(Self {
            runs_dir,
            run_dir,
            run_id,
            timestamp,
        })
    }

    /// Wraps an existing run directory without re-creating directories.
    pub fn from_existing(
        runs_dir: impl AsRef<Path>,
        run_dir: impl AsRef<Path>,
        run_id: Uuid,
        timestamp: DateTime<Utc>,
    ) -> Self {
        Self {
            runs_dir: runs_dir.as_ref().to_path_buf(),
            run_dir: run_dir.as_ref().to_path_buf(),
            run_id,
            timestamp,
        }
    }

    /// Base runs directory containing all run archives.
    pub fn runs_dir(&self) -> &Path {
        &self.runs_dir
    }

    /// Directory for this specific run.
    pub fn run_dir(&self) -> &Path {
        &self.run_dir
    }

    /// Identifier of the run.
    pub fn run_id(&self) -> Uuid {
        self.run_id
    }

    /// Creation timestamp of the run.
    pub fn timestamp(&self) -> DateTime<Utc> {
        self.timestamp
    }

    /// Path to `manifest.json`.
    pub fn manifest_path(&self) -> PathBuf {
        self.run_dir.join("manifest.json")
    }

    /// Path to `events.jsonl`.
    pub fn events_path(&self) -> PathBuf {
        self.run_dir.join("events.jsonl")
    }

    /// Directory containing server-related logs.
    pub fn server_dir(&self) -> PathBuf {
        self.run_dir.join("server")
    }

    /// Directory containing all instances.
    pub fn instances_dir(&self) -> PathBuf {
        self.run_dir.join("instances")
    }

    /// Directory for a specific instance.
    pub fn instance_dir(&self, instance_id: &str) -> PathBuf {
        self.instances_dir().join(instance_id)
    }

    /// Ensures the instance directory and its subdirectories (`crash-reports/`, `screenshots/`) exist.
    pub fn ensure_instance_dir(&self, instance_id: &str) -> Result<PathBuf> {
        let inst_dir = self.instance_dir(instance_id);
        fs::create_dir_all(&inst_dir)?;
        fs::create_dir_all(inst_dir.join("crash-reports"))?;
        fs::create_dir_all(inst_dir.join("screenshots"))?;
        Ok(inst_dir)
    }

    /// Writes `manifest.json` as formatted JSON.
    pub fn write_manifest(&self, manifest: &RunManifest) -> Result<()> {
        let path = self.manifest_path();
        let json = serde_json::to_string_pretty(manifest)?;
        fs::write(path, json)?;
        Ok(())
    }

    /// Appends a single event as a JSON line to `events.jsonl`.
    pub fn append_event(&mut self, event: &RunEvent) -> Result<()> {
        let path = self.events_path();
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        let line = serde_json::to_string(event)?;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    /// Appends multiple events in sequence to `events.jsonl`.
    pub fn append_events(&mut self, events: &[RunEvent]) -> Result<()> {
        for event in events {
            self.append_event(event)?;
        }
        Ok(())
    }

    /// Overwrites or creates a log file under `server/<filename>`.
    pub fn write_server_log(&mut self, filename: &str, content: &str) -> Result<PathBuf> {
        let dir = self.server_dir();
        fs::create_dir_all(&dir)?;
        let path = dir.join(filename);
        fs::write(&path, content)?;
        Ok(path)
    }

    /// Appends content to a log file under `server/<filename>`.
    pub fn append_server_log(&mut self, filename: &str, content: &str) -> Result<PathBuf> {
        let dir = self.server_dir();
        fs::create_dir_all(&dir)?;
        let path = dir.join(filename);
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        write!(file, "{}", content)?;
        Ok(path)
    }

    /// Appends a single line to `server/latest.log`.
    pub fn append_server_line(&mut self, line: &str) -> Result<()> {
        let dir = self.server_dir();
        fs::create_dir_all(&dir)?;
        let path = dir.join("latest.log");
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    /// Writes or overwrites a log file under `instances/<id>/<log_name>`.
    pub fn write_instance_log(
        &mut self,
        instance_id: &str,
        log_name: &str,
        content: &str,
    ) -> Result<PathBuf> {
        let inst_dir = self.ensure_instance_dir(instance_id)?;
        let path = inst_dir.join(log_name);
        fs::write(&path, content)?;
        Ok(path)
    }

    /// Appends text to a log file under `instances/<id>/<log_name>`.
    pub fn append_instance_log(
        &mut self,
        instance_id: &str,
        log_name: &str,
        content: &str,
    ) -> Result<PathBuf> {
        let inst_dir = self.ensure_instance_dir(instance_id)?;
        let path = inst_dir.join(log_name);
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        write!(file, "{}", content)?;
        Ok(path)
    }

    /// Appends a line to `instances/<id>/stdout.log`.
    pub fn append_instance_stdout(&mut self, instance_id: &str, line: &str) -> Result<()> {
        let inst_dir = self.ensure_instance_dir(instance_id)?;
        let path = inst_dir.join("stdout.log");
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    /// Appends a line to `instances/<id>/stderr.log`.
    pub fn append_instance_stderr(&mut self, instance_id: &str, line: &str) -> Result<()> {
        let inst_dir = self.ensure_instance_dir(instance_id)?;
        let path = inst_dir.join("stderr.log");
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    /// Appends a line to `instances/<id>/latest.log`.
    pub fn append_instance_latest_log(&mut self, instance_id: &str, line: &str) -> Result<()> {
        let inst_dir = self.ensure_instance_dir(instance_id)?;
        let path = inst_dir.join("latest.log");
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    /// Saves a screenshot file under `instances/<id>/screenshots/<filename>`.
    pub fn save_screenshot(
        &mut self,
        instance_id: &str,
        filename: &str,
        image_bytes: &[u8],
    ) -> Result<PathBuf> {
        let inst_dir = self.ensure_instance_dir(instance_id)?;
        let path = inst_dir.join("screenshots").join(filename);
        fs::write(&path, image_bytes)?;
        Ok(path)
    }

    /// Saves a crash report under `instances/<id>/crash-reports/<filename>`.
    pub fn save_crash_report(
        &mut self,
        instance_id: &str,
        filename: &str,
        content: &str,
    ) -> Result<PathBuf> {
        let inst_dir = self.ensure_instance_dir(instance_id)?;
        let path = inst_dir.join("crash-reports").join(filename);
        fs::write(&path, content)?;
        Ok(path)
    }
}
