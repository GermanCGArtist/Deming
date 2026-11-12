//! Error types and result alias for Deming.

use thiserror::Error;

/// Unified error type used across Deming crates.
#[derive(Debug, Error)]
pub enum DemingError {
    /// Underlying I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization or deserialization failure.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Configuration loading, validation, or persistence error.
    #[error("configuration error: {0}")]
    Config(String),

    /// Invalid job, instance, or server specification.
    #[error("invalid specification: {0}")]
    InvalidSpec(String),

    /// Referenced instance could not be found.
    #[error("instance not found: {0}")]
    InstanceNotFound(String),

    /// Process spawn, management, or exit failure.
    #[error("process error: {0}")]
    Process(String),

    /// Network or remote API error.
    #[error("network error: {0}")]
    Network(String),

    /// Authentication or account resolution error.
    #[error("authentication error: {0}")]
    Auth(String),

    /// Operation exceeded its configured timeout.
    #[error("operation '{operation}' timed out after {seconds}s")]
    Timeout { operation: String, seconds: u64 },

    /// Catch-all error for unexpected conditions.
    #[error("{0}")]
    Other(String),
}

/// Convenience result alias using [`DemingError`].
pub type Result<T> = std::result::Result<T, DemingError>;
