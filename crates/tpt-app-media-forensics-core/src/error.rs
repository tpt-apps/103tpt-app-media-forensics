//! Errors raised by the analysis engine.
//!
//! # Errors are data here, not control flow
//!
//! Analyzers return partial results plus a list of anomalies rather than
//! propagating `Err` upward. That policy exists because "malformed media cannot
//! crash the application" is an acceptance criterion (spec §75, §96): an error
//! that unwinds out of an analyzer can take down a whole run, and the failure
//! is itself evidence that belongs in the report.
//!
//! This module holds the errors that legitimately stop work: the source cannot
//! be read, the case directory cannot be written. Those are operational
//! failures, not observations about the media.

use std::path::PathBuf;

/// An operational failure in the analysis engine.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// A filesystem operation failed.
    #[error("{operation} failed for {path}: {source}")]
    Io {
        /// What the engine was attempting, e.g. "open source".
        operation: &'static str,
        /// The path involved.
        path: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },

    /// The path is a directory, not a file that can be analysed.
    #[error("not a regular file: {path}")]
    NotAFile {
        /// The path that was rejected.
        path: String,
    },

    /// The source changed while it was being read.
    ///
    /// Distinct from a plain I/O error: the read succeeded, but the file was
    /// not stable, so any digest computed over it describes no coherent
    /// version. Recording one anyway would be misleading.
    #[error("source changed while being acquired: {path} ({reported_size} bytes reported, {bytes_read} read)")]
    SourceChangedDuringAcquisition {
        /// The path being acquired.
        path: String,
        /// Size the filesystem reported.
        reported_size: u64,
        /// Bytes actually read.
        bytes_read: u64,
    },

    /// The analysis was cancelled by the caller.
    ///
    /// Distinct from an I/O failure: nothing went wrong with the file, the caller
    /// asked the engine to stop. Carrying it as its own variant is what lets a
    /// caller retry a cancelled run without treating it as a corrupt asset, and
    /// lets the CLI report "cancelled" rather than "failed".
    ///
    /// Nothing is written to the case database when this is returned. A partial
    /// analysis record that looked complete would be worse than none, because a
    /// report built from it would assert measurements never taken.
    #[error("the analysis was cancelled")]
    Cancelled,

    /// A case directory is missing expected structure.
    #[error("case directory is not initialised: {0}")]
    CaseDirectoryNotInitialised(PathBuf),

    /// A case manifest could not be read or written.
    #[error("case manifest is invalid: {reason}")]
    InvalidManifest {
        /// What was wrong.
        reason: String,
    },

    /// A value could not be encoded or decoded for storage.
    ///
    /// Kept distinct from I/O and database errors: a serialisation failure is a
    /// bug in the model, and reading it as a disk problem would send an
    /// investigator looking in the wrong place.
    #[error("cannot {operation}: {reason}")]
    Serialise {
        /// What was being encoded or decoded.
        operation: &'static str,
        /// What went wrong.
        reason: String,
    },

    /// A case database operation failed.
    ///
    /// Kept distinct from an I/O error: a constraint violation (a finding
    /// referencing a missing analysis, a duplicate asset hash) is a different
    /// kind of problem from a disk failure, and the two must not be conflated
    /// when diagnosing a case.
    #[error("case database error during {operation}: {source}")]
    Database {
        /// What the database was being asked to do.
        operation: &'static str,
        /// The underlying error.
        #[source]
        source: rusqlite::Error,
    },
}

impl CoreError {
    /// Wraps an I/O error with the operation and path that produced it.
    ///
    /// Keeping the path on the error means a failure inside a batch run can be
    /// reported against the file that caused it, rather than aborting the whole
    /// batch with an unlabelled message.
    #[must_use]
    pub fn io(operation: &'static str, path: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }

    /// Wraps a database error with the operation that produced it.
    #[must_use]
    pub fn database(operation: &'static str, source: rusqlite::Error) -> Self {
        Self::Database { operation, source }
    }
}
