//! Core domain types for TPT Media Forensics.
//!
//! This crate is the dependency root of the workspace: it must not depend on
//! any other crate in the workspace, and must stay free of I/O so that every
//! layer above it remains deterministic and testable (spec §77).
//!
//! It defines the forensic vocabulary shared by the analysis engine, the
//! persistence layer, the CLI, and the desktop UI:
//!
//! * [`Case`] and [`MediaAsset`] — the unit of work and the read-only source
//!   under examination (spec §9, §10, §11).
//! * [`Finding`] — a rule observation carrying severity, confidence, and
//!   supporting evidence (spec §34).
//! * [`Evidence`] — a derived artefact with integrity metadata and provenance
//!   (spec §32, §33).
//! * [`AnalysisRecord`] — a reproducible record of one run of the engine,
//!   including the analysis fingerprint (spec §63, §77).

#![forbid(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod analysis;
pub mod asset;
pub mod case;
pub mod comparison;
pub mod evidence;
pub mod finding;
pub mod id;
pub mod media;
pub mod time;
pub mod timeline;

pub use analysis::{
    AnalysisRecord, AnalysisStatus, AnalysisVersion, CacheKey, ProfileFingerprint,
    RuleSetFingerprint,
};
pub use asset::{
    AcquisitionRecord, AssetIntegrity, FileHash, FileTimestamps, FilesystemInfo, HashAlgorithm,
    HashSet, MediaAsset, MediaType,
};
pub use case::Case;
pub use comparison::{
    compare_pair, compare_streams, compare_timing, ComparisonAxis, ComparisonSide, Difference,
    FieldComparison, StreamComparison, StreamComparisonResult, UnmatchedStream,
};
pub use evidence::{Evidence, EvidenceIntegrity, EvidenceKind, Provenance};
pub use finding::{Confidence, Finding, FindingStatus, Observation, RuleRationale, Severity};
pub use id::{
    AnalysisId, AssetId, CaseId, EntityId, EntityKind, EvidenceId, FindingId, ReportId, StreamId,
};
pub use media::{
    AudioFormat, ChannelLayout, ChromaSubsampling, CodecInfo, ColourInfo, PixelFormat,
    StreamAnalysis, StreamKind, StreamTiming, VideoFormat,
};
pub use time::{MediaTime, Rational, Timebase};
pub use timeline::{Placement, Timeline, TimelineEntry, TimelineSource};
