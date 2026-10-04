//! Desktop entry point.
//!
//! # Why the binary is this thin
//!
//! Everything a command does lives in the library: [`AppState`] holds the open
//! case, [`commands`] holds the IPC surface, and [`view`] holds the screens. All
//! that is left here is wiring — register the plugin, hand over the state, list
//! the commands — and then hand control to the runtime.
//!
//! That split is what lets `cargo test` in this directory exercise every screen
//! without a window. See `crates/tpt-app-media-forensics-tauri/src/view/mod.rs`.
//!
//! # Panics here end the application, so there are none
//!
//! `run` is the one function whose panic is unrecoverable. It therefore does not
//! do anything that can fail — no filesystem work, no analysis, no parsing. A
//! failure anywhere else comes back as a [`ShellError`] and is rendered as a
//! message (spec §75, §96).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use tpt_app_media_forensics_tauri::AppState;

/// Builds and runs the desktop application.
///
/// Commands are referenced by full path inside `generate_handler!`. The
/// `#[tauri::command]` attribute emits its macro pair with `#[macro_export]`,
/// which places them at the crate root; `generate_handler!` resolves them by
/// that path, so a bare name would not find them from the binary crate.
///
/// The error type is Tauri's own rather than `Box<dyn Error>`, because a
/// `tauri::Error` from a windowing failure carries an `Any` payload that
/// boxing would discard — and the distinction between "the window system
/// refused" and "something went wrong inside" is worth keeping.
///
/// # Errors
///
/// Returns an error only if the windowing system refuses to start. Every other
/// failure — an unreadable case, a corrupt file, a cancelled analysis — is
/// reported to the user as a message and leaves the application running.
pub fn run() -> Result<(), tauri::Error> {
    // Marks the log the moment the process starts, so a missing frontend line
    // can be read as "the webview never ran the script" rather than as an
    // environment problem.
    tpt_app_media_forensics_tauri::commands::startup_log("rust: process started".to_owned());
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::new())
        .on_page_load(|window, _payload| {
            // Asks the webview what it actually has, and writes the answer to
            // `TPT_STARTUP_LOG`.
            //
            // Nothing else can distinguish "the frontend loaded and rendered"
            // from "the frontend failed and the window is blank": both look like
            // a running process. `__TAURI_INTERNALS__` is used rather than
            // `__TAURI__` because the internals bridge is injected
            // unconditionally, so this probe still works when the global bridge
            // is the thing that is missing.
            let probe = r#"
              (function () {
                var out = 'probe bridge=' + (typeof window.__TAURI__)
                  + ' internals=' + (typeof window.__TAURI_INTERNALS__)
                  + ' scripts=' + document.querySelectorAll('script').length
                  + ' navButtons=' + (document.getElementById('nav')
                      ? document.getElementById('nav').children.length : -1)
                  + ' fatal=' + (document.querySelector('pre.fatal')
                      ? document.querySelector('pre.fatal').textContent.slice(0, 200) : 'none');
                try {
                  if (window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke) {
                    window.__TAURI_INTERNALS__.invoke('startup_log', { line: out });
                  }
                } catch (e) { /* the probe must never throw */ }
              })();
            "#;
            let _ = window.eval(probe);
        })
        .invoke_handler(tauri::generate_handler![
            tpt_app_media_forensics_tauri::open_case,
            tpt_app_media_forensics_tauri::close_case,
            tpt_app_media_forensics_tauri::screens,
            tpt_app_media_forensics_tauri::dashboard,
            tpt_app_media_forensics_tauri::search,
            tpt_app_media_forensics_tauri::timeline,
            tpt_app_media_forensics_tauri::analyse,
            tpt_app_media_forensics_tauri::poll_analysis,
            tpt_app_media_forensics_tauri::cancel_analysis,
            tpt_app_media_forensics_tauri::inspect_asset,
            tpt_app_media_forensics_tauri::metadata_report,
            tpt_app_media_forensics_tauri::compare_assets,
            tpt_app_media_forensics_tauri::generate_report,
            tpt_app_media_forensics_tauri::video_screen,
            tpt_app_media_forensics_tauri::audio_screen,
            tpt_app_media_forensics_tauri::decode_frame,
            tpt_app_media_forensics_tauri::startup_log,
            tpt_app_media_forensics_tauri::start_batch,
            tpt_app_media_forensics_tauri::poll_batch,
        ])
        .run(tauri::generate_context!())
}

/// Starts the application, exiting the process on a startup failure.
///
/// A `Result` return would let a caller — a test, an installer — decide what a
/// windowing failure means. There is no such caller here, so the process exits
/// with the runtime's own message rather than a custom one that says less.
fn main() {
    if let Err(error) = run() {
        eprintln!(
            "TPT Media Forensics could not start: {error}\n\
             A failure here means the windowing system refused to open a window; \
             it is not a problem with any case."
        );
        std::process::exit(1);
    }
}
