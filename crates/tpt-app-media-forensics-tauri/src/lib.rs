#![forbid(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]

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
//!
//! # Layout
//!
//! * [`view`] — screen models. Pure data in, pure data out, fully unit-tested.
//! * [`commands`] — the IPC surface. Marshalling only.
//! * [`state`] — the open case and the cancellation token.
//! * [`error`] — failures as values, because a panic here is a crash.
//!
//! The split between `view` and `commands` is the load-bearing one. A screen
//! built inside a `#[tauri::command]` needs a webview to test; a screen built
//! as a struct here needs nothing. This repository tests its engine on 579
//! unit tests, and the screens are where an examiner's conclusions are formed.

pub mod commands;
pub mod error;
pub mod state;
pub mod view;

// The `#[tauri::command]` attribute expands into a `macro_rules!` pair marked
// `#[macro_export]`, which places it at the crate root rather than in
// `commands`. `tauri::generate_handler!` resolves those macros by path, so the
// binary needs them re-exported here for a command defined in one crate to be
// registered from another. This is a macro-system requirement, not a design
// choice; the functions themselves are also re-exported so the binary can pass
// them to the handler.
pub use commands::{
    analyse, audio_screen, cancel_analysis, close_case, compare_assets, dashboard, decode_frame,
    generate_report, inspect_asset, metadata_report, open_case, poll_analysis, poll_batch, screens,
    search, start_batch, startup_log, timeline, video_screen,
};

pub use error::{ErrorKind, ShellError, ShellResult};
pub use state::AppState;

/// The application name shown in the window title and installer.
pub const APP_NAME: &str = "TPT Media Forensics";

/// The identifier used by the Tauri bundler.
pub const APP_IDENTIFIER: &str = "co.tptsolutions.mediaforensics";
