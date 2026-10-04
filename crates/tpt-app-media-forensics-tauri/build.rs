//! Build script for the Tauri shell.
//!
//! `tauri_build::build` generates the compile-time context the `#[tauri::command]`
//! and `tauri::Builder` macros expand against, and validates `tauri.conf.json`
//! during the build rather than at first window paint. A malformed config is a
//! compile error here instead of a blank window for whoever runs the app.
//!
//! # Why it also runs `node ui/check.mjs`
//!
//! The frontend has no build step by design - no bundler, no `npm install` - so
//! nothing else in this build looks at it. `tauri_build` embeds `ui/` as static
//! assets and never parses the JavaScript, which means a module that does not
//! parse produces a *working executable* that opens to a blank window: the
//! module fails to evaluate before any screen renders, and the failure is
//! invisible from Rust.
//!
//! That is not hypothetical. A `return` orphaned outside a function by a text
//! edit sat in `screens.js` through four builds and 188 passing tests. Nothing
//! caught it, because the Rust tests never load the frontend and the build never
//! parses it.
//!
//! The check is skipped rather than fatal when `node` is absent. A developer
//! without Node should still be able to build the desktop app - that is the
//! whole point of the no-bundler frontend - so its absence is a warning, not an
//! error. What is *not* skipped is running the check when Node is available:
//! anyone who has it gets the check for free.

use std::process::Command;

fn main() {
    tauri_build::build();

    println!("cargo:rerun-if-changed=ui");

    if !check_frontend() {
        // A warning, not a failure. See the module comment: the no-bundler
        // frontend is a deliberate property, and a machine without Node must
        // still be able to build.
        println!(
            "cargo:warning=node was not found, so ui/ was not checked. \
             Install Node and run `node ui/check.mjs` to verify the frontend parses."
        );
    }
}

/// Runs the frontend check, reporting whether Node was available at all.
///
/// Returns `true` when the check ran and passed, `false` when Node is missing.
/// A check that *fails* panics rather than returning: a frontend that does not
/// parse must not be embedded silently, because the resulting executable looks
/// fine right up until someone launches it.
fn check_frontend() -> bool {
    let output = match Command::new("node").arg("ui/check.mjs").output() {
        Ok(output) => output,
        Err(_) => return false,
    };

    if !output.status.success() {
        panic!(
            "the frontend does not pass its own check:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    true
}
