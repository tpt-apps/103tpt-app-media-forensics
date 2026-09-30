//! Timestamp forensics and synchronisation (spec §23, §24).
//!
//! Inspects PTS/DTS monotonicity, discontinuities, gaps, overlaps, negative
//! timestamps, duration mismatches, edit lists, and A/V offset and drift.
//!
//! All arithmetic is exact integer or rational (see
//! [`tpt_app_media_forensics_model::time`]); float accumulation would
//! manufacture artefacts that look like evidence.

#![forbid(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod av_sync;
pub mod error;
pub mod pts_dts;

pub use av_sync::{analyse, AudioSamples, SyncReport, VideoSamples};
pub use error::TimingError;
pub use pts_dts::{scan_decode, scan_presentation, Anomaly, TimestampReport};
