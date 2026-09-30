//! Container inspection (spec §12).
//!
//! Identifies a file's container from its signature, enumerates its streams,
//! and reports the properties each stream declares about itself.
//!
//! Built on `tpt-kinetix-demux`, which does the box parsing; this crate owns
//! the mapping into the engine's forensic model and the decision about what
//! counts as an anomaly.
//!
//! # Declared, not measured
//!
//! What appears here is what the container *claims*: dimensions, timescales,
//! declared durations, and — where the demuxer exposes it — a frame rate
//! derived from the `stts` timing table. Nothing is asserted about whether
//! those claims are truthful; that is what the consistency rules are for
//! (spec §26).
//!
//! # Failure policy
//!
//! Container data is fully attacker-controlled (spec §75). Parsing is
//! delegated to a hardened demuxer, whole-file loading is capped, and a
//! damaged track is recorded as an anomaly rather than aborting the
//! inspection.

#![forbid(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod error;
pub mod fixture;
pub mod mp4;
pub mod probe;

pub use error::ContainerError;
pub use fixture::{TrackSpec, build_mp4, build_mp4_empty_moov, build_mp4_without_moov};
pub use mp4::{MAX_INSPECTED_BYTES, Mp4Inspection, inspect_bytes, inspect_file};
pub use probe::{ContainerFormat, detect, detect_file, extension_matches};