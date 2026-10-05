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

pub mod audio_sample_entry;
pub mod boxes;
pub mod colr;
pub mod damage;
pub mod elst;
pub mod error;
pub mod fixture;
pub mod mkv;
pub mod mp4;
pub mod packets;
pub mod probe;

pub use audio_sample_entry::{parse_track_audio, TrackAudio};
pub use colr::{parse_track_colour, TrackColour};
pub use damage::{scan_isobmff, SampleIndex, SampleOrigin, SamplePosition, StructuralDamage};
pub use elst::{parse_edit_lists, EditList};
pub use error::ContainerError;
pub use fixture::{
    build_mp4, build_mp4_av, build_mp4_empty_moov, build_mp4_stsd_gop_change,
    build_mp4_with_bitrate_drop, build_mp4_with_colour, build_mp4_with_declared_track_mismatch,
    build_mp4_with_frame_rate_change, build_mp4_with_hdr_colour,
    build_mp4_with_hdr_signalling_only, build_mp4_with_keyframes, build_mp4_with_metadata,
    build_mp4_with_reordered_frames, build_mp4_with_repeated_frames,
    build_mp4_with_wrong_declared_duration, build_mp4_without_moov, build_webm,
    build_webm_without_duration, gop_change_keyframes, TrackSpec,
};
pub use mkv::{
    inspect_bytes as inspect_matroska_bytes, inspect_file as inspect_matroska_file,
    read_samples as read_matroska_samples, read_samples_file as read_matroska_samples_file,
    MAX_MKV_SAMPLES,
};
pub use mp4::{
    inspect_bytes, inspect_file, inspect_path, read_header, read_moov, read_samples,
    read_samples_file, track_frame_info, ContainerInspection, SampleRecord, TrackFrameInfo,
    MAX_EXPANDED_SAMPLES, MAX_INSPECTED_BYTES, MAX_MOOV_BYTES, MAX_SAMPLED_BYTES,
};
pub use packets::{scan_packets, PacketDamage};
pub use probe::{detect, detect_file, extension_matches, ContainerFormat, PROBE_BYTES};
