use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BakeError {
    #[error("io error at {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },

    #[error("bundle serialization failed: {0}")]
    Serialize(String),

    #[error("corrupt bundle at {path}: {reason}")]
    Corrupt { path: PathBuf, reason: String },

    #[error("bundle checksum mismatch at {path} (expected {expected}, got {actual})")]
    ChecksumMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },

    #[error("bundle field decode failed ({field}): {reason}")]
    Decode { field: &'static str, reason: String },

    #[error("watcher error: {0}")]
    Watch(String),
}

impl BakeError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        BakeError::Io {
            path: path.into(),
            source,
        }
    }
}
