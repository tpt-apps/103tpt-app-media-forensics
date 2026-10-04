//! Screen view models (spec §79).
//!
//! # Why this layer exists separately from the commands
//!
//! Spec §79 asks for a dozen screens. Implementing them directly inside
//! `#[tauri::command]` functions would make every one of them untestable: a
//! command needs a webview, a serialisation boundary, and an event loop to
//! run, none of which exist in `cargo test`. The screens would then be the only
//! part of this project with no tests, in a repository that gates its
//! deterministic engine on 579 of them.
//!
//! So the screens are ordinary Rust types here. They take engine values and
//! produce serializable structs; the command layer only marshals. Everything
//! that could be wrong with a screen — a count that does not add up, a jump
//! that lands on a fabricated timecode, a pixel reading whose colour conversion
//! is unlabelled — is a plain unit test here rather than something a person has
//! to click through a webview to find.
//!
//! # Nothing in this layer measures anything
//!
//! The rule the crate exists to keep (spec §78) is that the GUI and the CLI
//! cannot disagree. Every number shown here comes from a type the rules also
//! consumed. Where a screen needs a *choice* — which screen a jump lands on,
//! which row a marker belongs on, whether a case is clear — that choice lives in
//! this layer and is tested. Where it needs a *fact*, the fact is read from the
//! engine and never recomputed, because a second implementation of a threshold
//! or a colour matrix is exactly how two front ends start telling different
//! stories about one file.
//!
//! # Screens (spec §79)
//!
//! ```text
//! Case
//!  +-- Assets
//!  +-- Overview
//!  +-- Streams
//!  +-- Timeline
//!  +-- Video
//!  +-- Audio
//!  +-- Metadata
//!  +-- Findings
//!  +-- Comparisons
//!  +-- Evidence
//!  +-- Reports
//! ```

pub mod assets;
pub mod batch;
pub mod comparison;
pub mod dashboard;
pub mod media;
pub mod run;
pub mod timeline;
pub mod viewer;

#[cfg(test)]
pub(crate) mod testing;

/// How a timeline position was arrived at (spec §31).
///
/// Re-exported under a shell-local name so screens and the command layer can
/// refer to one type without repeating the fully-qualified path. It is the
/// engine's [`Placement`](tpt_app_media_forensics_model::Placement) unchanged —
/// the shell does not get to soften the distinction between a measured position
/// and an inferred one.
pub use tpt_app_media_forensics_model::Placement as TimelinePlacement;

/// How much of a case's timeline is a record of what its runs observed.
///
/// A shell-local re-export of the store's [`TimelineRetention`], for the same
/// reason [`TimelinePlacement`] is re-exported: the screens and the command layer
/// name one type rather than repeating the fully-qualified path. The engine's
/// verdict is passed through unchanged — the shell does not get to decide that a
/// partial record reads as a complete one.
pub type Retention = tpt_app_media_forensics_core::store::TimelineRetention;
