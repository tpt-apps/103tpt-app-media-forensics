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
