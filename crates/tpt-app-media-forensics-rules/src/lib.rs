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
//! Forensic rule engine (spec §35-§37).
//!
//! Rules read analysis results and emit findings. They never open a file, run a
//! decoder, or write anything, which makes them testable against synthetic
//! results with no media involved.
//!
//! # Every rule explains itself
//!
//! `what_it_checks` and `why_it_matters` are trait methods rather than doc
//! comments, because spec §71 requires every finding to explain what was
//! checked and why it matters. A rule that cannot state why its condition
//! matters should not exist.
//!
//! # Nothing here asserts a cause
//!
//! Rules report what was observed. A GOP change is reported as a GOP change,
//! not as evidence of editing; the reasons a condition might arise are
//! described as possibilities in the finding's `why it matters`, never stated
//! as conclusions.

pub mod builtin;
pub mod engine;
pub mod profile;

pub use builtin::builtin_rules;
pub use engine::{AnalysisBundle, BundleInput, ForensicRule, RuleEngine, RuleError};
pub use profile::RuleProfile;
