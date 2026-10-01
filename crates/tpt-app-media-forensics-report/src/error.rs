//! Errors raised while building a report.

use thiserror::Error;

/// An error preventing a report from being produced.
#[derive(Debug, Error)]
pub enum ReportError {
    /// A renderer failed.
    #[error("cannot render {format} report: {reason}")]
    Render {
        /// Which format failed.
        format: &'static str,
        /// What went wrong.
        reason: String,
    },

    /// The report is missing information required by spec §60.
    #[error("report is missing required methodology: {0}")]
    IncompleteMethodology(&'static str),

    /// The requested output format is not supported.
    #[error("unsupported report format: {0}")]
    UnsupportedFormat(String),
}
