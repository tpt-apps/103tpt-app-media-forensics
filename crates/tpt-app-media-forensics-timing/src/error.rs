//! Errors raised by the timing layer.

use thiserror::Error;

/// An error preventing timing analysis.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TimingError {
    /// Not enough correspondence points to estimate an offset or drift.
    ///
    /// Reported rather than substituted with a guess: a single offset sample
    /// supports a statement about alignment, but not about drift, and
    /// returning a fabricated rate would be worse than returning nothing.
    #[error("cannot estimate drift from {available} sample(s); at least 2 are required")]
    InsufficientSamples {
        /// Number of usable correspondence points found.
        available: usize,
    },

    /// A timebase of zero was supplied, which cannot convert timestamps.
    #[error("invalid timebase: {0}")]
    InvalidTimebase(String),
}
