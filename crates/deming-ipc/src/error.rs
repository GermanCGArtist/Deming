//! Error types for the Deming IPC module.

use thiserror::Error;

/// Errors that can occur during IPC server/client operations.
#[derive(Debug, Error)]
pub enum IpcError {
    /// Underlying I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization or deserialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Port file contained an invalid port string.
    #[error("invalid port: {0}")]
    InvalidPort(String),

    /// Server is not running or unreachable.
    #[error("server not running")]
    ServerNotRunning,

    /// Connection was closed by the remote peer.
    #[error("connection closed")]
    ConnectionClosed,

    /// Operation timed out.
    #[error("operation timed out: {0}")]
    Timeout(String),

    /// Generic IPC error.
    #[error("{0}")]
    Other(String),
}

/// Convenience result alias for IPC operations.
pub type Result<T> = std::result::Result<T, IpcError>;
