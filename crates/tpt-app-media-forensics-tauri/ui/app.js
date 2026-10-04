// TPT Media Forensics - frontend entry point.
//
// # What lives here and what does not
//
// This file orchestrates and renders. Every decision it could get wrong - which
// counts a case has, whether a timeline entry is jumpable, whether a file
// passed, how far an analysis has got - is made in Rust and arrives as data.
// The frontend never derives a verdict, never infers a timecode, and never
// smooths over a value the engine could not measure.
//
// # Analysis runs on a worker thread, and the UI stays live
//
// The engine blocks for minutes on a long file, so `analyse` returns
// immediately and progress arrives as events. The poll is a timer rather than an
// awaited promise for the same reason: an awaited call would hold the window and
// make the cancel button unreachable for the duration of the run (spec \u00a757).
//
// # Why no framework
//
// Plain ES modules, no bundler, no dependency tree. The build stays a Rust
// toolchain plus `cargo`, which is what keeps the desktop app reproducible and
// keeps Node out of the path that produces an evidentiary tool.

import { invoke } from "./api.js";
import { listen, openDialog } from "./bridge.js";
import { renderScreen } from "./screens.js";
import { PROGRESS_EVENT } from "./constants.js";

const state = {
  case: null,
  screens: [],
  current: null,
  asset: null,
  running: null,
  lastResult: null,
  // Set by a timeline click: the presentation time to seek to when the video
  // screen next renders. `null` means "no pending seek".
  seekMicros: null,
};

const el = {
  nav: document.getElementById("nav"),
  screen: document.getElementById("screen"),
  banner: document.getElementById("banner"),
  caseName: document.getElementById("case-name"),
  search: document.getElementById("search"),
  toolbar: document.getElementById("toolbar"),
};

/** Shows a message in the banner, or clears it when given null. */
export function banner(kind, message) {
  if (!message) {
    el.banner.hidden = true;
    el.banner.textContent = "";
    return;
  }
  el.banner.hidden = false;
  el.banner.className = `banner ${kind}`;
  el.banner.textContent = message;
}

/**
 * Renders one call without losing the screen behind it.
 *
 * A failure is shown in the banner rather than replacing the view, because the
 * analyst was looking at something when it happened and replacing it with a
 * message loses their place in the case.
 */
async function attempt(fn) {
  try {
    const value = await fn();
    banner(null);
    return { ok: true, value, error: null };
  } catch (error) {
    const message = error?.message ?? String(error);
    banner("error", message);
    // The message travels with the result. A caller that only checks `ok` and
    // then reaches for a value has nothing to show the analyst, which is how a
    // failure turns into "something went wrong" with no explanation.
    return { ok: false, value: null, error: message };
  }
}

/**
 * Renders one call without losing the screen behind it.
 *
 * # Why there is also an `attempt`
 *
 * This returns `null` for *both* a failed call and a command that legitimately
 * answered `null`. For most screens those are the same outcome - nothing to
 * show, and the banner already said why - and collapsing them keeps the call
 * sites short.
 *
 * They are not the same outcome where a command answers `null` on success.
 * `poll_analysis` answers `null` for "still running", `close_case` answers
 * `null` because it returns unit. Reading those as failures produced two real
 * bugs: the close button silently did nothing, because a successful close looks
 * exactly like a rejected promise; and the poll loop treated a failed poll as
 * "still running" and retried every 750ms for the life of the window, since the
 * only thing that stopped it was a run that never ends.
 *
 * Those call sites use `attempt`, which says which happened.
 */
async function guarded(fn) {
  return (await attempt(fn)).value;
}

/** Builds the navigation from the backend's own screen list (spec \u00a779). */
function renderNav() {
  el.nav.replaceChildren();
  for (const screen of state.screens) {
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = screen.label;
    button.dataset.screen = screen.screen;
    button.setAttribute("aria-current", String(screen.screen === state.current));
    // An asset-scoped screen is disabled rather than shown empty, so landing on
    // "no video stream" before choosing a file does not read as a problem with
    // the case.
    button.disabled = !state.case || screen.needs_asset;
    button.addEventListener("click", () => navigate(screen.screen));
    el.nav.append(button);
  }
}

/** Switches to a screen and re-renders it. */
export async function navigate(name) {
  state.current = name;
  renderNav();
  el.screen.replaceChildren();
  // `guarded` is passed in because `renderReports` generates a report bundle from
  // inside its own click handler. It used to call the name directly, which is
  // not in scope there - the ReferenceError only surfaced when an analyst
  // pressed the button, on the one screen that generates reports.
  const content = await guarded(() =>
    renderScreen(name, state, { banner, setAsset, guarded, jumpTo, attempt, openFolder }),
  );
  if (content) el.screen.append(content);
}

/**
 * Selects the asset the asset-scoped screens describe.
 *
 * Re-renders the current screen so the change is visible immediately. Selecting
 * an asset and seeing nothing happen is how an analyst concludes the selection
 * did not register, and then goes on reading the wrong file's findings.
 */
export function setAsset(asset) {
  state.asset = asset;
  void navigate(state.current);
}

/**
 * Shows the native folder picker and hands the chosen path to `accept`.
 *
 * The dialog is the only way a path enters the batch workflow, for the same
 * reason it is the only way into a case: an analyst never types one, so what is
 * analysed is a file they are looking at rather than text from a clipboard.
 */
async function openFolder(accept) {
  const selected = await guarded(() =>
    openDialog({
      directory: true,
      multiple: false,
      title: "Choose a folder to batch-analyse",
    }),
  );
  if (selected) accept(selected);
}

/**
 * Moves the viewer to a position on the timeline.
 *
 * Clicking a timeline track used to do nothing at all: the handler called
 * `helpers.jumpTo?.()` and nothing had ever supplied one. A control that answers
 * a click with no consequence is worse than one that is absent, because it
 * teaches the analyst the timeline is not clickable.
 *
 * The target is recorded rather than acted on here. The video screen owns the
 * frame table and knows which frame index is nearest a presentation time;
 * deciding that from the timeline would mean picking a frame using a table it
 * has not loaded.
 */
function jumpTo(micros) {
  state.seekMicros = micros;
  void navigate("VIDEO");
}

/** Draws the analyse / cancel controls and the progress bar. */
function renderToolbar() {
  el.toolbar.replaceChildren();

  const pick = document.createElement("button");
  pick.type = "button";
  pick.textContent = "Analyse media\u2026";
  pick.disabled = !state.case || Boolean(state.running);
  pick.addEventListener("click", chooseAndAnalyse);
  el.toolbar.append(pick);

  if (state.running) {
    const bar = document.createElement("div");
    bar.className = "progress";
    const fill = document.createElement("div");
    fill.className = "progress-fill";
    fill.style.width = `${Math.round(state.running.fraction * 100)}%`;
    bar.append(fill);

    const label = document.createElement("span");
    label.className = "progress-label";
    label.textContent = state.running.stage;
    bar.append(label);

    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.textContent = "Cancel";
    cancel.addEventListener("click", async () => {
      const cancelled = await guarded(() => invoke("cancel_analysis"));
      // Only say so when there was something to cancel; otherwise the banner
      // would report an analyst's click as the engine's decision.
      if (cancelled) banner("notice", "Cancelling\u2026 a cancelled run writes nothing to the case.");
    });
    el.toolbar.append(bar, cancel);
  }

  if (state.lastResult) {
    const summary = document.createElement("span");
    summary.className = "run-summary";
    const r = state.lastResult;
    summary.textContent =
      `${r.status.toLowerCase()} - ${r.finding_count} finding(s), ` +
      `${r.evidence_count} artefact(s)` +
      // The cache and limitation markers are shown beside the counts, not hidden
      // behind a disclosure: a run served from cache measured nothing this time,
      // and a run with limitations measured less than it looks.
      (r.cache_hit ? " (from cache)" : "") +
      (r.limitations.length ? `, ${r.limitations.length} limitation(s)` : "");
    el.toolbar.append(summary);
  }
}

/** Picks a media file through the dialog and starts an analysis. */
async function chooseAndAnalyse() {
  const started = await guarded(async () => {
    const selected = await openDialog({
      multiple: false,
      title: "Choose media to analyse",
      filters: [
        { name: "Media", extensions: ["mp4", "m4v", "mov", "mkv", "webm", "wav", "aiff", "flac", "ogg"] },
        { name: "All files", extensions: ["*"] },
      ],
    });
    if (!selected) return null;
    return invoke("analyse", { source: selected });
  });
  if (!started) return;

  state.running = { id: started.id, fraction: 0, stage: "Starting\u2026" };
  state.lastResult = null;
  renderToolbar();
  poll();
}

/**
 * Polls for the finished result.
 *
 * A timer rather than an awaited promise, so the cancel button stays reachable
 * for the whole run. The interval is generous: polling faster gains nothing when
 * the analysis itself takes minutes.
 */
async function poll() {
  // `attempt`, not `guarded`: a `null` here means "still running", and a failed
  // call also yields `null`. Reading them the same way made every poll error
  // look like progress, and the loop then retried every 750ms for as long as
  // the window stayed open. A poll that fails has to stop the loop - the banner
  // already said why, and cancelling is still one click away.
  const { ok, value: result } = await attempt(() => invoke("poll_analysis"));
  if (!ok) {
    state.running = null;
    renderToolbar();
    return;
  }
  if (result === null) {
    window.setTimeout(poll, 750);
    return;
  }

  state.running = null;
  state.lastResult = result;
  renderToolbar();

  if (result.status === "COMPLETE") {
    banner(
      "notice",
      `Analysis complete. Fingerprint ${result.analysis_fingerprint}` +
        (result.limitations.length ? ` - ${result.limitations.length} limitation(s) recorded.` : ".")
    );
    await refresh();
  } else if (result.status === "CANCELLED") {
    // A deliberate action, reported as one. Showing it as an error would train
    // an analyst to ignore the banner.
    banner("notice", "Analysis cancelled. Nothing was written to the case.");
  } else {
    banner("error", result.limitations[0] ?? "The analysis did not complete.");
  }
}

/** Reloads the open case and returns to its dashboard. */
export async function refresh() {
  el.caseName.textContent = state.case ? state.case.name : "No case open";
  if (state.case) {
    state.case = await guarded(() => invoke("open_case", { path: state.case.root })) ?? state.case;
  }
  renderNav();
  renderToolbar();
  await navigate("CASE");
}

const closeButton = document.getElementById("close-case");

// Closing forgets the case in this process only. Nothing on disk is touched: a
// case directory is the record of an examination, and discarding it from the
// window is not the same as discarding it. The manifest, database and evidence
// are exactly as they were.
closeButton.addEventListener("click", async () => {
  // `attempt`, not `guarded`: `close_case` returns unit, so a *successful* close
  // arrives as `null`. The old `if (!closed) return;` therefore discarded every
  // success and the button did nothing at all - it could not even be told apart
  // from a failed close.
  const { ok } = await attempt(() => invoke("close_case"));
  if (!ok) return;
  state.case = null;
  state.asset = null;
  state.lastResult = null;
  closeButton.disabled = true;
  await refresh();
});

document.getElementById("open-case").addEventListener("click", async () => {
  const opened = await guarded(async () => {
    // The dialog plugin is the only way a path enters this application, so a
    // case directory always comes from disk rather than from typed text.
    const selected = await openDialog({ directory: true, multiple: false, title: "Open a case directory" });
    if (!selected) return null;
    return invoke("open_case", { path: selected });
  });
  if (!opened) return;
  state.case = opened;
  state.asset = opened.assets?.[0] ?? null;
  closeButton.disabled = false;
  await refresh();
});

el.search.addEventListener("keydown", async (event) => {
  if (event.key !== "Enter") return;
  const term = el.search.value.trim();
  const result = await guarded(() => invoke("search", { text: term || null }));
  if (result) {
    const { renderSearch } = await import("./search.js");
    el.screen.replaceChildren(renderSearch(result));
  }
});

// Progress events drive the bar between polls. The listener is registered once at
// startup and filters on the run id, so a second run cannot have its progress
// drawn onto the first.
await listen(PROGRESS_EVENT, (event) => {
  if (!state.running || event.payload?.runId !== state.running.id) return;
  state.running.fraction = event.payload.fraction ?? state.running.fraction;
  state.running.stage = event.payload.label ?? state.running.stage;
  renderToolbar();
});

state.screens = await invoke("screens");
renderNav();
renderToolbar();

// Reports what actually loaded, to `TPT_STARTUP_LOG` when it is set.
//
// This is the only check that runs in a real webview. `ui/check.mjs` uses a
// stub DOM and cannot tell whether the shipped page resolved its modules, found
// the Tauri bridge, or rendered anything — a green check and a blank window are
// indistinguishable from outside. Here the answer is written where a build
// script can read it, which is how "the executable builds" and "the executable
// works" stop being the same claim.
try {
  await invoke("startup_log", {
    line: `ready screens=${state.screens.length} bridge=${globalThis.__TAURI__ ? "yes" : "no"}`,
  });
} catch {
  // Diagnostics must never be the thing that breaks startup.
}
