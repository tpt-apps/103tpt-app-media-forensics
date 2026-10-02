//! Audio analysis (spec §19-§22).
//!
//! Covers codec and channel description, silence, clipping, DC offset, dynamic
//! range, and loudness. Spectral measurement is not implemented; `measurement`
//! and `loudness` are amplitude-domain only.
//!
//! # Methodology must be reported
//!
//! "Never report a number without identifying the methodology" (spec §21).
//! Measurement results therefore carry the standard and method that produced
//! them, so a report cannot present a bare figure.

#![forbid(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
pub mod decode;
pub mod loudness;
pub mod measurement;

pub use decode::{
    decode as decode_audio, decode_opus_packets, is_decodable as is_audio_decodable, AudioDecode,
    AudioDecodeError, DecodeLimits as AudioDecodeLimits,
};
pub use loudness::{integrated_loudness, LoudnessError, SPECIFIED_SAMPLE_RATE};
pub use measurement::{
    amplitude_to_dbfs, find_silence, level_stats, LevelStats, Measurement, Methodology,
    SilenceRegion,
};
