//! Runtime state, event stream, and run manifest types.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::spec::TestJobSpec;

/// Lifecycle state of an individual client instance during a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceState {
    Queued,
    Preparing,
    Starting,
    Running,
    Passed,
    Failed,
    Aborted,
    TimedOut,
}

/// Overall lifecycle status of a test run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Pending,
    Running,
    Passed,
    Failed,
    Aborted,
}

/// Timestamped event emitted during test run execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunEvent {
    /// Emitted when a run begins execution.
    RunStarted {
        run_id: Uuid,
        job_name: String,
        timestamp: DateTime<Utc>,
    },
    /// Emitted when the dedicated server matches its ready pattern.
    ServerReady {
        run_id: Uuid,
        port: u16,
        timestamp: DateTime<Utc>,
    },
    /// Emitted whenever an instance transitions between lifecycle states.
    InstanceStateChanged {
        run_id: Uuid,
        instance_id: String,
        state: InstanceState,
        message: Option<String>,
        timestamp: DateTime<Utc>,
    },
    /// Emitted for a line of stdout/stderr from an instance or server.
    LogLine {
        run_id: Uuid,
        source: String,
        line: String,
        timestamp: DateTime<Utc>,
    },
    /// Emitted when a screenshot is captured for an instance.
    ScreenshotCaptured {
        run_id: Uuid,
        instance_id: String,
        path: PathBuf,
        timestamp: DateTime<Utc>,
    },
    /// Emitted when an instance's [`crate::spec::TestCompletionRule`] is evaluated.
    TestEvaluated {
        run_id: Uuid,
        instance_id: String,
        passed: bool,
        reason: String,
        timestamp: DateTime<Utc>,
    },
    /// Emitted when the entire run reaches a terminal status.
    RunFinished {
        run_id: Uuid,
        status: RunStatus,
        timestamp: DateTime<Utc>,
    },
}

impl RunEvent {
    /// Returns the UTC timestamp associated with this event.
    pub fn timestamp(&self) -> DateTime<Utc> {
        match self {
            Self::RunStarted { timestamp, .. }
            | Self::ServerReady { timestamp, .. }
            | Self::InstanceStateChanged { timestamp, .. }
            | Self::LogLine { timestamp, .. }
            | Self::ScreenshotCaptured { timestamp, .. }
            | Self::TestEvaluated { timestamp, .. }
            | Self::RunFinished { timestamp, .. } => *timestamp,
        }
    }
}

/// Summary result for a single instance within a completed or active run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceResult {
    /// Identifier matching [`crate::spec::InstanceSpec::id`].
    pub instance_id: String,
    /// Final or current lifecycle state.
    pub state: InstanceState,
    /// Process exit code, if the process has exited.
    pub exit_code: Option<i32>,
    /// Whether the instance passed its completion rule.
    pub passed: bool,
    /// Human-readable explanation of the evaluation outcome.
    pub reason: Option<String>,
    /// Timestamp when the instance started running.
    pub started_at: Option<DateTime<Utc>>,
    /// Timestamp when the instance finished.
    pub finished_at: Option<DateTime<Utc>>,
    /// Path to the captured instance log file, if any.
    pub log_path: Option<PathBuf>,
    /// Paths to screenshots captured for this instance.
    #[serde(default)]
    pub screenshots: Vec<PathBuf>,
}

/// Persisted manifest describing a test run, its specification, and its results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunManifest {
    /// Unique identifier for this run.
    pub run_id: Uuid,
    /// Name of the executed job.
    pub job_name: String,
    /// Current or final run status.
    pub status: RunStatus,
    /// Timestamp when the run was created.
    pub created_at: DateTime<Utc>,
    /// Timestamp when execution started.
    pub started_at: Option<DateTime<Utc>>,
    /// Timestamp when execution finished.
    pub finished_at: Option<DateTime<Utc>>,
    /// Full job specification used for this run.
    pub spec: TestJobSpec,
    /// Per-instance execution results.
    #[serde(default)]
    pub instances: Vec<InstanceResult>,
    /// Path to the dedicated server log file, if a server was used.
    pub server_log_path: Option<PathBuf>,
    /// Path to the exported diagnostic bundle archive, if generated.
    pub bundle_path: Option<PathBuf>,
}
