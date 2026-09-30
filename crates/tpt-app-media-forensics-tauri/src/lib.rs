//! Tauri desktop shell for TPT Media Forensics (spec §78).
//!
//! # A shell, not an engine
//!
//! This crate wires a desktop UI to [`tpt_app_media_forensics_core`]. It
//! contains no analysis logic of its own: every result the UI displays is
//! produced by the same engine the CLI drives, so a finding means the same
//! thing however the analyst arrived at it (spec §51).
//!
//! Keeping the boundary this way is what lets the CLI, the GUI, automation,
//! and future services share one analysis path (spec §78). If analysis logic
//! ever appears here, that guarantee is broken.
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
//!
//! The dashboard must never reduce a case to a single "authentic/fake" score
//! (spec §80); it reports counts by severity and an overall review status.

#![forbid(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod commands;

/// The application name shown in the window title and installer.
pub const APP_NAME: &str = "TPT Media Forensics";

/// The identifier used by the Tauri bundler.
pub const APP_IDENTIFIER: &str = "co.tptsolutions.mediaforensics";