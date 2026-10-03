//! Video analysis (spec §14-§18).
//!
//! Covers structural, temporal, spatial, and colour properties, plus GOP
//! structure, frame statistics, duplicate and near-duplicate detection, and
//! scene-change analysis.
//!
//! # Backend abstraction
//!
//! Frame decoding is delegated through traits rather than called directly, so
//! that the analysis logic — which is what produces findings — stays
//! independent of which decoder is in use and remains unit-testable without
//! decoding real frames.

#![forbid(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod bitrate;

pub use bitrate::{analyse as analyse_bitrate, BitrateAnomaly, BitrateReport, BitrateSample};

pub mod decode;
pub mod frame;

pub use decode::{is_decodable, DecodeError, DecodeLimits, DecodeSession, DecodedFrame};
pub use frame::{extract as extract_frame, FrameError, FrameImage, COLOUR_MATRIX};

pub mod near_duplicate;

pub use near_duplicate::{NearDuplicate, NearDuplicateReport, PerceptualHash};

pub mod scene;

pub use scene::{FrameDifference, SceneReport};

pub mod gop;

pub use gop::{Gop, GopChange, GopReport};

pub mod duplicate;

pub use duplicate::{RepeatedRun, SampleDigest, Soundness};
