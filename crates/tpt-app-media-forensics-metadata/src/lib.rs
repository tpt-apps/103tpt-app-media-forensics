//! Metadata extraction and consistency checking (spec §25, §26).
//!
//! Extracts container, track, and codec metadata into a structured tree that
//! preserves each key's value, source, and location, then cross-checks entries
//! that disagree with one another.
//!
//! # Observation, not accusation
//!
//! A conflict between a container creation time and a track creation time is
//! reported as an inconsistency. The engine flags what disagrees and does not
//! assert why.

#![forbid(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
pub mod consistency;
pub mod tree;

pub use consistency::{find_conflicts, Conflict, ConflictKind, Observation};
pub use tree::{MetadataEntry, MetadataTree, Scope};
