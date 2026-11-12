//! IPC protocol message schema for Deming.

use deming_core::RunEvent;
use serde::{Deserialize, Serialize};

use crate::error::{IpcError, Result};

/// Message schema transmitted between runner/CLI/MCP clients and the GUI server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcMessage {
    /// Publish a live test run event to the GUI.
    PublishEvent {
        run_id: String,
        event: RunEvent,
    },
    /// Notify that a run has started.
    RunStarted {
        run_id: String,
        name: String,
    },
    /// Request cancellation/abort of a specific run.
    AbortRun {
        run_id: String,
    },
    /// Request cancellation/abort of all active runs.
    AbortAll,
    /// Request an immediate screenshot from a running instance.
    RequestScreenshot {
        run_id: String,
        instance_id: String,
    },
    /// Response containing screenshot capture path or error message.
    ScreenshotResult {
        run_id: String,
        instance_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Heartbeat ping message.
    Ping,
    /// Heartbeat pong response.
    Pong,
}

impl IpcMessage {
    /// Serializes this message to a JSON string terminated by a newline (`\n`).
    pub fn to_json_line(&self) -> Result<String> {
        let mut json = serde_json::to_string(self)?;
        json.push('\n');
        Ok(json)
    }

    /// Deserializes an [`IpcMessage`] from a single JSON line string.
    pub fn from_json_line(line: &str) -> Result<Self> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Err(IpcError::Other("empty message line".to_string()));
        }
        serde_json::from_str(trimmed).map_err(IpcError::from)
    }

    /// Returns the associated `run_id`, if this message variant contains one.
    pub fn run_id(&self) -> Option<&str> {
        match self {
            Self::PublishEvent { run_id, .. }
            | Self::RunStarted { run_id, .. }
            | Self::AbortRun { run_id }
            | Self::RequestScreenshot { run_id, .. }
            | Self::ScreenshotResult { run_id, .. } => Some(run_id.as_str()),
            Self::AbortAll | Self::Ping | Self::Pong => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use deming_core::{InstanceState, RunEvent};
    use uuid::Uuid;

    #[test]
    fn test_serialization_roundtrip_all_variants() {
        let run_uuid = Uuid::new_v4();
        let variants = vec![
            IpcMessage::Ping,
            IpcMessage::Pong,
            IpcMessage::AbortAll,
            IpcMessage::AbortRun {
                run_id: "run-42".into(),
            },
            IpcMessage::RunStarted {
                run_id: "run-42".into(),
                name: "smoke-test".into(),
            },
            IpcMessage::RequestScreenshot {
                run_id: "run-42".into(),
                instance_id: "inst-1".into(),
            },
            IpcMessage::ScreenshotResult {
                run_id: "run-42".into(),
                instance_id: "inst-1".into(),
                path: Some("/tmp/shot.png".into()),
                error: None,
            },
            IpcMessage::ScreenshotResult {
                run_id: "run-42".into(),
                instance_id: "inst-1".into(),
                path: None,
                error: Some("window minimized".into()),
            },
            IpcMessage::PublishEvent {
                run_id: "run-42".into(),
                event: RunEvent::InstanceStateChanged {
                    run_id: run_uuid,
                    instance_id: "inst-1".into(),
                    state: InstanceState::Running,
                    message: Some("started game".into()),
                    timestamp: Utc::now(),
                },
            },
            IpcMessage::PublishEvent {
                run_id: "run-42".into(),
                event: RunEvent::ServerReady {
                    run_id: run_uuid,
                    port: 25565,
                    timestamp: Utc::now(),
                },
            },
        ];

        for msg in variants {
            let line = msg.to_json_line().expect("failed to serialize message");
            assert!(line.ends_with('\n'), "json line must end with newline");
            let deserialized =
                IpcMessage::from_json_line(&line).expect("failed to deserialize message");
            assert_eq!(msg, deserialized);
        }
    }

    #[test]
    fn test_run_id_extraction() {
        assert_eq!(
            IpcMessage::AbortRun {
                run_id: "abc".into()
            }
            .run_id(),
            Some("abc")
        );
        assert_eq!(IpcMessage::AbortAll.run_id(), None);
        assert_eq!(IpcMessage::Ping.run_id(), None);
        assert_eq!(IpcMessage::Pong.run_id(), None);
    }
}

