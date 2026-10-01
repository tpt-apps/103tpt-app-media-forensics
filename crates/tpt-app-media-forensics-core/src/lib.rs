//! Deterministic analysis engine (spec §51, §96).
//!
//! Orchestrates the per-layer analyzers (container, video, audio, timing,
//! metadata), runs the forensic rules against their output, records findings
//! and evidence, and caches results.
//!
//! # One engine, many front ends
//!
//! The CLI and the Tauri desktop app both call into this crate (spec §51).
//! Nothing here may depend on Tauri or on any CLI framework, so that
//! automation and future services reuse the identical analysis path.
//!
//! # Failure policy
//!
//! Malformed media must never crash the application (spec §75, §96). Every
//! analyzer therefore returns a partial result plus a list of anomalies rather
//! than propagating an error upward, and the engine records the failure as
//! evidence instead of aborting the run.

#![forbid(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod acquisition;
pub mod case_dir;
pub mod error;

pub use acquisition::{acquire, acquire_asset};
pub use case_dir::{CaseDirectory, CaseManifest};
pub use error::CoreError;

pub mod store;

pub use store::Store;

pub mod cache;

pub use cache::{AnalysisCache, CacheEntry};

pub mod pipeline;

pub use pipeline::{AnalysisEngine, AnalysisOutcome};
