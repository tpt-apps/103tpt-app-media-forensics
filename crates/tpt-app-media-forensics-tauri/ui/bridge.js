// The Tauri bridge.
//
// # Why there is no `import ... from "@tauri-apps/api"`
//
// This frontend has no bundler, and that is deliberate (spec §76): a Node
// toolchain in the build path is a second supply chain. But `frontendDist` is a
// directory of static files served to the webview, and a browser cannot resolve
// a *bare specifier*. There is no import map and no bundler to rewrite one, so
//
//     import { invoke } from "@tauri-apps/api/core";
//
// throws `Failed to resolve module specifier` before a single line of this
// application runs. The module graph never loads, the window opens empty, and
// nothing anywhere reports an error - the same class of silent failure as the
// orphaned `return` in `screens.js`, one level further out.
//
// Tauri already injects its API as `window.__TAURI__` when `withGlobalTauri`
// is set, which is exactly the shape a no-bundler frontend needs. This module
// is the single place that reads it, so the rest of the frontend never touches
// a global and never names a package it cannot load.
//
// `ui/check.mjs` fails the build if a bare specifier reappears, because this is
// the one mistake that cannot be caught by parsing.

/** Reads `window.__TAURI__`, failing with something an analyst can act on. */
function tauri() {
  const api = globalThis.__TAURI__;
  if (!api) {
    // Thrown rather than defaulted. A missing bridge means every command would
    // silently do nothing, and a blank window with no explanation is worse.
    throw new Error(
      "The Tauri bridge (window.__TAURI__) is missing. This window is not " +
        "running inside the desktop shell, or 'withGlobalTauri' is not set in " +
        "tauri.conf.json.",
    );
  }
  return api;
}

/** Invokes a Rust command. */
export function invoke(command, args = {}) {
  return tauri().core.invoke(command, args);
}

/** Subscribes to a backend event. Returns an unlisten function. */
export function listen(event, handler) {
  return tauri().event.listen(event, handler);
}

/**
 * Shows the native open dialog and returns the chosen path, or null.
 *
 * The dialog is the only way a path enters this application: an analyst never
 * types one, so a case directory or a media file always comes from the
 * filesystem they are looking at rather than from text.
 */
export async function openDialog(options) {
  const dialog = tauri().dialog;
  if (!dialog || typeof dialog.open !== "function") {
    throw new Error(
      "The file-open dialog is unavailable. The 'dialog' plugin is not " +
        "registered with the Rust side, so no path can be chosen.",
    );
  }
  return dialog.open(options);
}