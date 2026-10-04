// The IPC bridge.
//
// `invoke` is the only place the frontend talks to Rust. Errors arrive as the
// `ShellError` the engine produced - a tag and a finished sentence - and are
// rethrown unchanged. The frontend does not translate, soften, or retry them,
// because the wording was authored where the failure happened and rewriting it
// here would mean the window and the log disagree.

import { invoke as tauriInvoke } from "./bridge.js";

export async function invoke(command, args = {}) {
  try {
    return await tauriInvoke(command, args);
  } catch (error) {
    // Tauri wraps a serialisable error payload in its own structure. Unwrap it
    // so the banner shows the sentence the engine wrote rather than a shape.
    throw new Error(unwrap(error));
  }
}

/** Digs the human-facing message out of whatever Tauri handed back. */
function unwrap(error) {
  if (typeof error === "string") return error;
  if (error && typeof error === "object") {
    if (typeof error.message === "string" && error.message) return error.message;
    if (error.payload && typeof error.payload.message === "string") return error.payload.message;
    if (error.payload && typeof error.payload === "string") return error.payload;
  }
  return String(error);
}

/** Formats a `MediaTime` for display. Serde writes it as whole microseconds. */
export function timecode(micros) {
  if (micros === null || micros === undefined) return "(no media position)";
  const negative = micros < 0;
  const total = Math.abs(micros);
  const ms = Math.floor(total / 1000);
  const seconds = Math.floor(ms / 1000) % 60;
  const minutes = Math.floor(ms / 60000) % 60;
  const hours = Math.floor(ms / 3600000);
  const pad = (n) => String(n).padStart(2, "0");
  return `${negative ? "-" : ""}${pad(hours)}:${pad(minutes)}:${pad(seconds)}.${String(ms % 1000).padStart(3, "0")}`;
}
