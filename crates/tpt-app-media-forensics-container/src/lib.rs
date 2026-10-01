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
pub use fixture::{
    build_mp4, build_mp4_empty_moov, build_mp4_stsd_gop_change, build_mp4_with_keyframes,
    build_mp4_with_metadata, build_mp4_without_moov, gop_change_keyframes, TrackSpec,
};
pub use mp4::{
    inspect_bytes, inspect_file, inspect_path, read_header, read_moov, read_samples,
    read_samples_file, track_frame_info, Mp4Inspection, SampleRecord, TrackFrameInfo,
    MAX_EXPANDED_SAMPLES, MAX_INSPECTED_BYTES, MAX_MOOV_BYTES, MAX_SAMPLED_BYTES,
};
pub use probe::{detect, detect_file, extension_matches, ContainerFormat};
