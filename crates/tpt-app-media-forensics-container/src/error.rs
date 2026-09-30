//! Errors raised by the container layer.
//!
//! These stop work only when the container itself cannot be read. A malformed
//! track, an absent index, or an inconsistent duration is recorded as an
//! anomaly on the result instead — the engine must be able to describe a
//! damaged file rather than refusing to look at it (spec §30, §75).

use thiserror::Error;

/// An error preventing container inspection.
#[derive(Debug, Error)]
pub enum ContainerError {
    /// A filesystem operation failed.
    #[error("{operation} failed for {path}: {source}")]
    Io {
        /// What the layer was attempting.
        operation: &'static str,
        /// The path involved.
        path: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },

    /// The file exceeds the whole-file loading limit.
    ///
    /// MP4 parsing here loads the entire file; without this guard a
    /// multi-gigabyte asset could exhaust memory and crash the application,
    /// which spec §75 forbids.
    #[error("file is {size_bytes} bytes, above the {limit_bytes} byte inspection limit: {path}")]
    TooLarge {
        /// The path that was rejected.
        path: String,
        /// Actual file size.
        size_bytes: u64,
        /// The limit that was exceeded.
        limit_bytes: u64,
    },

    /// The container structure could not be parsed.
    #[error("cannot parse container: {0}")]
    Parse(String),
}

impl ContainerError {
    /// Wraps an I/O error with the operation and path that produced it.
    #[must_use]
    pub fn io(operation: &'static str, path: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }
}

impl From<tpt_kinetix_core::error::KinetixError> for ContainerError {
    fn from(error: tpt_kinetix_core::error::KinetixError) -> Self {
        Self::Parse(error.to_string())
    }
}
