//! Forensic rule engine (spec §35-§37).
//!
//! Defines the rule trait, rule profiles, tolerance handling, and
//! the built-in rule set. Rules read analysis results and emit findings; they
//! never touch the filesystem.
//!
//! # Ordering
//!
//! Rules are evaluated and emitted in a fixed order (sorted by rule ID) so that
//! two runs over the same input produce identical output (spec §77).

#![forbid(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
