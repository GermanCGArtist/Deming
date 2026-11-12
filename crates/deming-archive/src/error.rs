use thiserror::Error;

#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Zip error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("Regex error: {0}")]
    Regex(#[from] regex::Error),

    #[error("Deming core error: {0}")]
    Core(#[from] deming_core::DemingError),

    #[error("Run not found: {0}")]
    RunNotFound(String),

    #[error("Invalid archive: {0}")]
    InvalidArchive(String),
}

pub type Result<T> = std::result::Result<T, ArchiveError>;
