// Screen renderers (spec §79).
//
// Each function returns a DOM node. None of them computes anything the model
// already decided: the dashboard prints the counts it was handed, the timeline
// draws the markers it was given, and the comparison view prints the verdict
// word the engine chose. Where a screen needs to say "not measured", it says
// exactly that rather than showing an empty cell.

import { invoke, timecode } from "./api.js";
import { html } from "./dom.js";

/** Dispatches to the renderer for `name`. */
export async function renderScreen(name, state, helpers) {
  const el = document.getElementById("screen");
  const renderers = {
    CASE: renderCase,
    ASSETS: renderAssets,
    OVERVIEW: renderOverview,
    STREAMS: renderStreams,
    TIMELINE: renderTimeline,
    VIDEO: renderViewer,
    AUDIO: renderAudio,
    METADATA: renderMetadata,
    FINDINGS: renderFindings,
    COMPARISONS: renderComparisons,
    EVIDENCE: renderEvidence,
    REPORTS: renderReports,
  };
  const render = renderers[name];
  if (!render) return text("This screen has no renderer yet.");
  return render(state, { ...helpers, el, sourcePath: () => state.asset?.source_path });
}

/** Builds an `h2`. */
function heading(text) {
  const node = document.createElement("h2");
  node.textContent = text;
  return node;
}

/** Builds a `p` of dim explanatory text. */
function text(value, className = "placeholder") {
  const node = document.createElement("p");
  node.className = className;
  node.textContent = value;
  return node;
}

/** Builds a fragment without a wrapper element. */
function fragment(...children) {
  const frag = document.createDocumentFragment();
  for (const child of children) if (child) frag.append(child);
  return frag;
}

/** Formats a byte count for display. */
function bytes(value) {
  if (value === null || value === undefined) return "-";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let n = value;
  let i = 0;
  while (n >= 1024 && i < units.length - 1) { n /= 1024; i += 1; }
  return `${i === 0 ? n : n.toFixed(1)} ${units[i]}`;
}

// ---------------------------------------------------------------- case/dash

/**
 * The dashboard (spec §80).
 *
 * Counts and a review status, and no authenticity score. The `review_status`
 * the model computed is printed verbatim, including `INCOMPLETE` for a run that
 * has not finished - which is the state that must never be shown as clean.
 */
async function renderCase(state, helpers) {
  const summary = await invoke("dashboard");
  const wrap = document.createDocumentFragment();
  wrap.append(heading(summary.case_name || summary.case_id));

  const counters = document.createElement("div");
  counters.className = "counters";
  const rows = [
    ["Assets", summary.counts.assets],
    ["Analyses", summary.counts.analyses],
    ["Findings", summary.counts.findings],
    ["Critical", summary.counts.critical],
    ["Significant", summary.counts.significant],
    ["Warnings", summary.counts.warnings],
    ["Info", summary.counts.info],
    ["Awaiting review", summary.counts.awaiting_review],
  ];
  for (const [label, value] of rows) {
    const card = document.createElement("div");
    card.className = "counter";
    const v = document.createElement("div");
    v.className = `value sev-${label.toUpperCase().split(" ")[0]}`;
    v.textContent = String(value);
    const l = document.createElement("div");
    l.className = "label";
    l.textContent = label;
    card.append(v, l);
    counters.append(card);
  }
  wrap.append(counters);

  const verdict = document.createElement("div");
  verdict.className = "verdict";
  const analysis = document.createElement("div");
  analysis.innerHTML = html`<div class="k">Analysis</div><div class="v">${summary.analysis_status}</div>`;
  const review = document.createElement("div");
  review.innerHTML = html`<div class="k">Overall</div><div class="v">${summary.review_status}</div>`;
  verdict.append(analysis, review);
  wrap.append(verdict);

  if (summary.review_status === "INCOMPLETE") {
    wrap.append(
      text(
        "This analysis has not completed. Nothing below is a finished result, and no " +
          "absence of findings here means the file is clean.",
        "panel dim"
      )
    );
  }
  wrap.append(buildBatchPanel(helpers));
  return wrap;
}

/**
 * The batch intake dashboard (spec §83).
 *
 * A panel on the Case screen rather than a thirteenth nav entry: spec §79 fixes
 * the screen list at twelve, and batch is a workflow *within* the case rather
 * than a different kind of thing to look at.
 *
 * The statuses are the engine's own verdicts, carried through `BatchRow` without
 * being recomputed here. `UNREADABLE` and `SKIPPED` are shown as themselves
 * rather than folded into `FAIL`: a corrupt intake folder and a file that failed
 * delivery are different problems, and treating "we could not read it" as
 * "FAIL" is how an incomplete intake passes QC.
 */
function buildBatchPanel(helpers) {
  const panel = document.createElement("section");
  panel.className = "panel batch";

  const title = document.createElement("h3");
  title.textContent = "Batch intake";
  panel.append(title);

  const note = document.createElement("p");
  note.className = "dim";
  note.textContent =
    "Analyse every media file in a folder into this case. The source files are " +
    "opened read-only; everything written goes under the case directory.";
  panel.append(note);

  const picker = document.createElement("input");
  picker.type = "text";
  picker.className = "path";
  picker.placeholder = "Folder to batch-analyse";

  const choose = document.createElement("button");
  choose.type = "button";
  choose.textContent = "Choose folder…";
  choose.addEventListener("click", () => {
    void helpers.openFolder((folder) => {
      picker.value = folder;
    });
  });

  // The holder is created before the click handler is attached and passed into
  // `runBatch`, rather than being found again through `panel.lastChild`: a
  // positional lookup couples the two functions to the panel's current shape.
  const holder = document.createElement("div");
  const run = document.createElement("button");
  run.type = "button";
  run.textContent = "Run batch";
  run.addEventListener("click", () => {
    if (!picker.value) {
      helpers.banner("error", "Choose a folder to batch-analyse first.");
      return;
    }
    void runBatch(picker.value, holder, run, helpers);
  });

  const controls = document.createElement("div");
  controls.className = "viewer-controls";
  controls.append(picker, choose, run);
  panel.append(controls);
  panel.append(holder);
  return panel;
}

/** Runs a batch and polls until it finishes, then draws the table. */
async function runBatch(directory, holder, runButton, helpers) {
  runButton.disabled = true;
  holder.replaceChildren(text(`Analysing ${directory}…`, "dim"));

  const started = await helpers.attempt(() =>
    invoke("start_batch", { directory }),
  );
  if (!started.ok) {
    runButton.disabled = false;
    holder.replaceChildren(
      text(
        String(started.error ?? "The batch could not start."),
        "panel sev sev-SIGNIFICANT",
      ),
    );
    return;
  }

  // Poll on a timer rather than awaiting, so the window stays usable and the
  // other screens stay reachable while a large folder is analysed.
  const poll = async () => {
    const { ok, value, error } = await helpers.attempt(() =>
      invoke("poll_batch"),
    );
    if (!ok) {
      runButton.disabled = false;
      holder.replaceChildren(
        text(
          String(error ?? "The batch run failed."),
          "panel sev sev-SIGNIFICANT",
        ),
      );
      return;
    }
    if (value === null) {
      window.setTimeout(poll, 750);
      return;
    }
    runButton.disabled = false;
    holder.replaceChildren(renderBatch(value));
  };
  await poll();
}

/** Draws the batch table, with the blocking count stated rather than implied. */
function renderBatch(view) {
  const wrap = document.createDocumentFragment();
  const rows = view.rows ?? [];
  if (!rows.length) {
    wrap.append(text("No media files were found in that folder.", "dim"));
    return wrap;
  }

  const blocking = rows.filter((row) =>
    ["FAIL", "UNREADABLE", "SKIPPED"].includes(row.status),
  ).length;
  const summary = document.createElement("p");
  summary.className = blocking > 0 ? "sev sev-SIGNIFICANT" : "sev sev-INFO";
  summary.textContent =
    `${rows.length} file${rows.length === 1 ? "" : "s"} examined, ` +
    `${blocking} blocking delivery. ` +
    (blocking > 0
      ? "An unreadable or skipped file is not a verdict on the media - it is a " +
        "file this tool could not examine, and it cannot be certified either way."
      : "Nothing here blocks delivery.");
  wrap.append(summary);

  const table = document.createElement("table");
  table.innerHTML =
    "<thead><tr><th>File</th><th>Status</th><th class='num'>Findings</th>" +
    "<th>Worst</th><th>Note</th></tr></thead>";
  const body = document.createElement("tbody");
  for (const row of rows) {
    const tr = document.createElement("tr");
    tr.innerHTML =
      html`<td title="${row.path}">${row.name}</td>` +
      html`<td>${row.status}</td>` +
      html`<td class="num">${row.finding_count}</td>` +
      html`<td>${row.worst_severity ?? "-"}</td>` +
      html`<td class="dim">${row.reason ?? ""}</td>`;
    body.append(tr);
  }
  table.append(body);
  wrap.append(table);
  return wrap;
}

async function renderAssets(state, helpers) {
  const wrap = document.createDocumentFragment();
  wrap.append(heading("Assets"));
  const assets = state.case?.assets ?? [];
  if (!assets.length) return fragment(wrap, text("This case holds no assets."));

  const table = document.createElement("table");
  table.innerHTML =
    "<thead><tr><th>Name</th><th>Source path</th><th class='num'>Size</th>" +
    "<th>SHA-256</th></tr></thead>";
  const body = document.createElement("tbody");
  for (const asset of assets) {
    const row = document.createElement("tr");
    row.innerHTML =
      html`<td>${asset.name}</td>` +
      html`<td class="dim mono">${asset.source_path}</td>` +
      html`<td class="num">${bytes(asset.size_bytes)}</td>` +
      html`<td class="mono dim">${asset.sha256 ? asset.sha256.slice(0, 16) + "\u2026" : "-"}</td>`;
// A row is the only place an analyst can say *which* file the rest of the
    // screens describe. Without it every asset-scoped screen silently showed the
    // first asset in the case, which is a way of being wrong rather than a way
    // of being empty.
    row.tabIndex = 0;
    row.setAttribute("role", "button");
    const select = () => helpers.setAsset(asset);
    row.addEventListener("click", select);
    row.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        select();
      }
    });
    if (state.asset && state.asset.id === asset.id) {
      row.classList.add("selected");
      row.setAttribute("aria-current", "true");
    }
    body.append(row);
  }
  table.append(body);
  wrap.append(table);

  // Hashes are the integrity record (spec §11), so the full digest is
  // selectable text rather than a truncated display - a reviewer needs to copy
  // it into a chain-of-custody document.
  const full = document.createElement("div");
  full.className = "panel";
  const list = document.createElement("dl");
  list.className = "kv";
  for (const asset of assets) {
    const dt = document.createElement("dt");
    dt.textContent = asset.name;
    const dd = document.createElement("dd");
    dd.textContent = asset.sha256 ?? "(no digest recorded)";
    list.append(dt, dd);
  }
  full.append(heading("Recorded digests"), list);
  wrap.append(full);
  return wrap;
}

// ------------------------------------------------------------------ overview

async function renderOverview(state) {
  const wrap = document.createDocumentFragment();
  wrap.append(heading("Overview"));
  if (!state.asset) return fragment(wrap, text("Select an asset."));

  const inspection = await invoke("inspect_asset", { source: state.asset.source_path });
  const o = inspection.overview;

  const panel = document.createElement("div");
  panel.className = "panel";
  const kv = document.createElement("dl");
  kv.className = "kv";
  const pairs = [
    ["Container", o.format],
    ["Dimensions", o.dimensions ?? "not declared"],
    ["Frame rate", o.frame_rate ?? "not declared"],
    ["Declared tracks", String(o.declared_track_count)],
    ["Streams recovered", String(o.stream_count)],
  ];
  for (const [k, v] of pairs) {
    const dt = document.createElement("dt");
    dt.textContent = k;
    const dd = document.createElement("dd");
    dd.textContent = v;
    kv.append(dt, dd);
  }
  panel.append(kv);
  wrap.append(panel);

  // A declared track count higher than the number recovered is a property of
  // the file, and it is the raw material of
  // `CONTAINER.DECLARED_TRACK_MISMATCH`. Stated as an observation here; the rule
  // decides whether it is significant.
  if (o.declared_track_count > o.stream_count) {
    wrap.append(
      text(
        `The container declares ${o.declared_track_count} tracks but ${o.stream_count} ` +
          "could be recovered. A skipped track is still in the file.",
        "panel dim"
      )
    );
  }

  // "Parsed cleanly and declared nothing" is a different state from "could not
  // be parsed", so the reason is printed rather than leaving an empty panel.
  if (o.anomalies.length) {
    const notes = document.createElement("div");
    notes.className = "panel";
    const title = document.createElement("strong");
    title.textContent = "Observations";
    const list = document.createElement("ul");
    list.className = "limits";
    for (const note of o.anomalies) {
      const item = document.createElement("li");
      item.textContent = note;
      list.append(item);
    }
    notes.append(title, list);
    wrap.append(notes);
  }
  return wrap;
}

// ------------------------------------------------------------------- streams

async function renderStreams(state) {
  const wrap = document.createDocumentFragment();
  wrap.append(heading("Streams"));
  if (!state.asset) return fragment(wrap, text("Select an asset."));

  const inspection = await invoke("inspect_asset", { source: state.asset.source_path });
  if (!inspection.streams.length) {
    // Not an empty table: an empty table reads as "this file has no streams",
    // when the truth may be that the container could not be read.
    return fragment(
      wrap,
      text(
        inspection.overview.anomalies[0] ??
          "No streams were recovered from this container.",
        "panel dim"
      )
    );
  }

  const table = document.createElement("table");
  table.innerHTML =
    "<thead><tr><th class='num'>#</th><th>Kind</th><th>Codec</th><th>Format</th>" +
    "<th>Colour</th><th class='num'>Duration</th></tr></thead>";
  const body = document.createElement("tbody");

  for (const stream of inspection.streams) {
    const row = document.createElement("tr");

    const details = [];
    if (stream.dimensions) details.push(stream.dimensions);
    if (stream.frame_rate) details.push(`${stream.frame_rate} fps`);
    if (stream.sample_rate) details.push(`${stream.sample_rate} Hz`);
    if (stream.channels) details.push(`${stream.channels} ch`);
    if (stream.bit_depth) details.push(`${stream.bit_depth}-bit`);

    const colour = [stream.primaries, stream.transfer, stream.matrix].filter(Boolean).join(" / ");
    // The declared and measured durations are shown as a pair when they differ,
    // because their disagreement is the observation (spec §26). Reconciling
    // them here would hide exactly what the rule reports.
    let duration = "-";
    if (stream.declared_duration_micros !== null) {
      duration = timecode(stream.declared_duration_micros);
      if (stream.declared_duration_micros !== stream.measured_duration_micros) {
        duration += ` declared / ${timecode(stream.measured_duration_micros)} measured`;
      }
    }

    row.innerHTML =
      html`<td class="num dim">${stream.index}</td>` +
      html`<td>${stream.kind}</td>` +
      // The codec tag is shown exactly as declared, never normalised: a stream
      // tagged `avc1` in a file with no `avcC` box is itself the observation.
      `<td><code>${stream.codec}</code>${
        stream.codec_long_name ? ` <span class="dim">${stream.codec_long_name}</span>` : ""
      }</td>` +
      html`<td>${details.join(", ") || "-"}</td>` +
      html`<td>${colour || '<span class="dim">not declared</span>'}${
        stream.is_hdr ? ' <span class="sev sev-CRITICAL">HDR</span>' : ""
      }</td>` +
      html`<td class="num">${duration}</td>`;
    body.append(row);
  }

  table.append(body);
  wrap.append(table);
  return wrap;
}

// ------------------------------------------------------------------ timeline

/**
 * The timeline (spec §42, §81).
 *
 * Markers are positioned by the model's own time and coloured by the severity
 * the engine assigned. A marker whose position was `inferred` is drawn dashed
 * and semi-transparent, because spec §31's whole point is that an inferred
 * position must not read like a measured one.
 *
 * Observations with no media position are listed below the strip, never drawn
 * on it. Placing one at the left edge would claim it happened at the start of
 * the file, which is precisely the fabrication spec §31 forbids.
 */
async function renderTimeline(state, helpers) {
  const wrap = document.createDocumentFragment();
  wrap.append(heading("Timeline"));
  const view = await invoke("timeline");

  // Stated before anything else, and by the model rather than here.
  //
  // An empty strip and an unrecorded one draw identically, so without this
  // sentence a case created before timeline retention reads as "this run found
  // nothing on this track" — which is the opposite of what happened, and the
  // more dangerous of the two errors in a forensic tool. The wording lives in
  // `TimelineView::retention_notice` so a test can reach it.
  if (view.retention_notice) {
    const notice = document.createElement("div");
    notice.className = "panel";
    notice.textContent = view.retention_notice;
    wrap.append(notice);
  }

  if (!view.layers.length && !view.unplaced.length) {
    return fragment(wrap, text("No positioned observations were recorded for this asset."));
  }

  const duration = view.duration ?? 0;
  const strip = document.createElement("div");
  strip.className = "timeline";

  for (const row of view.layers) {
    const line = document.createElement("div");
    line.className = "tl-row";

    const label = document.createElement("div");
    label.className = "tl-label";
    label.textContent = row.layer;

    const track = document.createElement("div");
    track.className = "tl-track";
    if (duration > 0) {
      track.addEventListener("click", (event) => {
        const bounds = track.getBoundingClientRect();
        const fraction = (event.clientX - bounds.left) / bounds.width;
        helpers.jumpTo?.(Math.round(fraction * duration));
      });
    }

    for (const mark of row.events) {
      const bar = document.createElement("div");
      const inferred = mark.placement === "inferred";
      bar.className = `tl-mark${inferred ? " inferred" : ""}`;
      if (mark.severity) bar.classList.add(`sev-${mark.severity}`);
      else bar.style.background = "var(--accent)";
      if (duration > 0) bar.style.left = `${(mark.time / duration) * 100}%`;
      bar.title = `${timecode(mark.time)}${inferred ? " (inferred)" : ""} - ${mark.summary}`;
      track.append(bar);
    }

    line.append(label, track);
    strip.append(line);
  }
  wrap.append(strip);

  const measured = document.createElement("p");
  measured.className = "dim";
  // The measured count is stated rather than implied: "12 events" beside
  // markers of which two were inferred overstates what the analysis established.
  measured.textContent =
    `${view.layers.reduce((n, r) => n + r.events.length, 0)} markers, ` +
    `${view.measured_marks} with a measured position`;
  wrap.append(measured);

  if (view.unplaced.length) {
    const unplaced = document.createElement("div");
    unplaced.className = "tl-unplaced";
    const head = document.createElement("strong");
    // "No media position" is stated in the heading, not left for the reader to
    // infer from the absence of a marker.
    head.textContent = `${view.unplaced.length} observation(s) with no media position - not drawn above:`;
    const list = document.createElement("ul");
    for (const entry of view.unplaced) {
      const item = document.createElement("li");
      item.innerHTML = html`<code>${entry.reference}</code> - ${entry.summary}`;
      list.append(item);
    }
    unplaced.append(head, list);
    wrap.append(unplaced);
  }
  return wrap;
}

// ---------------------------------------------------------------- video/audio

/**
 * The Video screen (spec §43, §44).
 *
 * The frame *table* is always available for a readable file and is what a
 * reviewer examines when a file will not play, so the screen shows it even when
 * no pixels can be decoded. PTS and DTS are both printed: their disagreement is
 * the observation worth making, and a viewer showing only PTS renders a
 * reordered stream as if it played in order.
 */
async function renderViewer(state, helpers) {
  const wrap = document.createDocumentFragment();
  wrap.append(heading("Video"));
  if (!state.asset) return fragment(wrap, text("Select an asset."));

  const view = await invoke("video_screen", { source: state.asset.source_path });

  const panel = document.createElement("div");
  panel.className = "panel";
  const kv = document.createElement("dl");
  kv.className = "kv";
  for (const [k, v] of [
    ["Frames", String(view.frame_count)],
    ["Dimensions", view.dimensions ?? "not declared"],
    ["Frame rate", view.frame_rate ?? "not declared"],
    [
      "Every frame a keyframe",
      view.all_frames_are_keyframes ? "yes (no stss declared)" : "no",
    ],
  ]) {
    const dt = document.createElement("dt");
    dt.textContent = k;
    const dd = document.createElement("dd");
    dd.textContent = v;
    kv.append(dt, dd);
  }
  panel.append(kv);
  wrap.append(panel);

  // The decoder refusal is printed as the policy it is. An analyst who read
  // "could not decode" would go looking for a corrupt file that is perfectly
  // intact.
  if (view.pixels_unavailable) {
    const note = document.createElement("div");
    note.className = "panel dim";
    note.textContent = view.pixels_unavailable;
    wrap.append(note);
  }

  if (!view.frames.length) {
    return fragment(
      wrap,
      text("There are no frames to show for this file.", "panel dim")
    );
  }

  // The frame table, with both timestamps, plus a viewer.
  //
  // Both timestamps on every row because their disagreement is the observation
  // worth making; a viewer showing only PTS renders a reordered stream as if it
  // played in order.
  const table = document.createElement("table");
  table.innerHTML =
    "<thead><tr><th class='num'>Frame</th><th>PTS</th><th>DTS</th>" +
    "<th class='num'>Reorder</th><th>Type</th></tr></thead>";
  const body = document.createElement("tbody");

  // Bounded so a feature-length master does not put 100,000 rows in the DOM. The
  // note below says so, rather than letting a truncated list read as the whole.
  const limit = 500;
  const shown = view.frames.slice(0, limit);
  for (const stamp of shown) {
    const ts = view.timestamps[stamp.index];
    const row = document.createElement("tr");
    row.dataset.frame = String(stamp.index);
    row.style.cursor = "pointer";
    const reorder = ts?.dts !== null && ts?.dts !== undefined
      ? Math.round((ts.pts - ts.dts) / 1000)
      : null;
    row.innerHTML =
      html`<td class="num">${stamp.index}</td>` +
      html`<td class="mono">${timecode(ts?.pts ?? stamp.time)}</td>` +
      html`<td class="mono">${ts?.dts === null || ts?.dts === undefined ? '<span class="dim">not declared</span>' : timecode(ts.dts)}</td>` +
      html`<td class="num">${reorder === null ? "-" : `${reorder} ms`}</td>` +
      html`<td>${stamp.is_key_frame ? '<span class="sev sev-INFO">KEYFRAME</span>' : '<span class="dim">predicted</span>'}</td>`;
    row.addEventListener("click", () => loadFrame(stamp.index, helpers));
    body.append(row);
  }
  table.append(body);
  wrap.append(table);

  if (view.frames.length > limit) {
    wrap.append(
      text(
        `Showing the first ${limit} of ${view.frames.length} frames. ` +
          "The full table is in the case database.",
        "panel dim"
      )
    );
  }

  // The viewer itself, populated on demand. Decoding is per click rather than
  // up front: a 4K master is tens of thousands of frames, and a viewer that
  // loaded them all before showing frame one would look like a hung application.
  const viewer = document.createElement("div");
  viewer.className = "viewer";
  viewerState.source = state.asset.source_path;
  viewerState.current = null;
  viewer.append(
    buildViewerControls(),
    text("Select a frame to decode and display it.", "dim"),
  );
  wrap.append(viewer);

  // A timeline click arrives here as a seek target. The nearest frame is chosen
  // from the table this screen just loaded, rather than by the timeline, so the
  // frame that gets decoded is one this screen can actually vouch for.
  if (state.seekMicros !== null && state.seekMicros !== undefined) {
    const target = state.seekMicros;
    state.seekMicros = null;
    let nearest = null;
    let bestDelta = Infinity;
    for (const stamp of view.frames) {
      const delta = Math.abs((stamp.time ?? 0) - target);
      if (delta < bestDelta) {
        bestDelta = delta;
        nearest = stamp;
      }
    }
    if (nearest) {
      const row = body.querySelector(`tr[data-frame="${nearest.index}"]`);
      if (row) row.classList.add("selected");
      await loadFrame(nearest.index, helpers);
    }
  }
  return wrap;
}

/**
 * The zoom and A/B controls shown above the decoded frame.
 *
 * Zoom is a set of fixed steps rather than a free scale: a fixed step is
 * reproducible, and "8x nearest-neighbour" is something an analyst can state in
 * a report. A continuous zoom would be easier to use and harder to describe.
 */
function buildViewerControls() {
  const bar = document.createElement("div");
  bar.className = "viewer-controls";

  for (const step of ZOOMS) {
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = `${step}x`;
    button.disabled = viewerState.zoom === step;
    button.setAttribute("aria-pressed", String(viewerState.zoom === step));
    button.addEventListener("click", () => {
      viewerState.zoom = step;
      // Repaint the frame already decoded rather than decoding it again: the
      // pixels have not changed, only the magnification.
      const canvas = document.querySelector(".frame-canvas");
      if (canvas && canvas.__image) drawFrame(canvas, canvas.__image, step);
      bar.replaceWith(buildViewerControls());
    });
    bar.append(button);
  }

  for (const side of ["a", "b"]) {
    const pin = document.createElement("button");
    pin.type = "button";
    const label = side.toUpperCase();
    pin.textContent =
      viewerState[side] === null
        ? `Pin current as ${label}`
        : `${label} = frame ${viewerState[side]}`;
    // Pinning needs a decoded frame to pin. Without one the button explains
    // itself rather than silently recording `undefined`.
    pin.disabled = viewerState.current === null;
    pin.addEventListener("click", () => {
      viewerState[side] = viewerState.current;
      bar.replaceWith(buildViewerControls());
    });
    bar.append(pin);
  }

  const compare = document.createElement("button");
  compare.type = "button";
  compare.textContent = "Compare A and B";
  compare.disabled = viewerState.a === null || viewerState.b === null;
  compare.addEventListener("click", () => {
    void compareFrames(viewerState.a, viewerState.b);
  });
  bar.append(compare);
  return bar;
}

/**
 * Decodes two pinned frames and reports how they differ.
 *
 * The figure is a count of pixels whose luma differs, not a similarity score.
 * Spec \u00a731 forbids collapsing a comparison to one number that reads as a verdict,
 * and this is a measurement an analyst can check: it says how many of the pixels
 * moved, and by how much, and names the largest single difference.
 */
async function compareFrames(frameA, frameB) {
  const screen = document.getElementById("screen");
  const viewer = screen?.querySelector(".viewer");
  if (!viewer || frameA === null || frameB === null) return;
  const source = viewerState.source;
  if (!source) return;
  viewer.replaceChildren(text(`Decoding frames ${frameA} and ${frameB}\u2026`, "dim"));
  try {
    const [left, right] = await Promise.all([
      invoke("decode_frame", { source, frameIndex: frameA }),
      invoke("decode_frame", { source, frameIndex: frameB }),
    ]);
    const report = document.createElement("div");
    report.className = "panel";
    if (
      left.image.width !== right.image.width ||
      left.image.height !== right.image.height
    ) {
      report.textContent =
        `Frames ${frameA} and ${frameB} differ in dimensions ` +
        `(${left.image.width}x${left.image.height} against ` +
        `${right.image.width}x${right.image.height}), so their pixels cannot be ` +
        `compared position by position. No similarity is reported.`;
    } else {
      const { differing, worst, total } = comparePixels(
        left.image.rgb,
        right.image.rgb,
      );
      report.textContent =
        `${differing} of ${total} pixels differ between frames ${frameA} and ` +
        `${frameB}. Largest single-channel difference: ${worst}. This is a ` +
        `count of changed pixels, not a similarity score.`;
    }
    viewer.append(report);
  } catch (error) {
    viewer.replaceChildren(
      text(error?.message ?? String(error), "panel sev sev-SIGNIFICANT"),
    );
  }
}

/** Counts differing pixels and the largest single-channel delta between them. */
function comparePixels(a, b) {
  const total = Math.floor(Math.min(a.length, b.length) / 3);
  let differing = 0;
  let worst = 0;
  for (let i = 0; i < total * 3; i += 3) {
    const dr = Math.abs(a[i] - b[i]);
    const dg = Math.abs(a[i + 1] - b[i + 1]);
    const db = Math.abs(a[i + 2] - b[i + 2]);
    const delta = Math.max(dr, dg, db);
    if (delta > 0) differing += 1;
    if (delta > worst) worst = delta;
  }
  return { differing, worst, total };
}

/**
 * Decodes one frame and renders it with the pixel inspector and histogram.
 *
 * The decode runs on demand and the button is disabled while it is in flight: a
 * double click would start two decodes of the same frame, and the slower one
 * would overwrite the faster one's result - leaving the analyst looking at a
 * frame number that no longer matches the pixels.
 */
async function loadFrame(frameIndex, helpers) {
  const el = document.getElementById("screen");
  const viewer = el.querySelector(".viewer");
  if (!viewer) return;

  viewer.replaceChildren(text(`Decoding frame ${frameIndex}\u2026`, "dim"));

  let payload;
  try {
    payload = await invoke("decode_frame", {
      source: helpers.sourcePath(),
      frameIndex,
    });
  } catch (error) {
    viewer.replaceChildren(
      text(error?.message ?? String(error), "panel sev sev-SIGNIFICANT")
    );
    return;
  }

  viewer.replaceChildren();
  const image = payload.image;
  viewerState.current = frameIndex;
  viewerState.source = helpers.sourcePath();
  const canvas = document.createElement("canvas");
  canvas.className = "frame-canvas";
  drawFrame(canvas, image, viewerState.zoom);
  // Kept on the node so changing zoom repaints from the decoded pixels instead
  // of decoding the same frame again.
  canvas.__image = image;

  const readout = document.createElement("div");
  readout.className = "readout";
  const header = document.createElement("div");
  header.className = "panel";
  const ts = payload.timestamps;
  header.innerHTML =
    html`<div class="k">Frame</div><div class="v">${frameIndex}</div>` +
    html`<div class="k">PTS</div><div class="v mono">${timecode(ts.pts)}</div>` +
    html`<div class="k">DTS</div><div class="v mono">${ts.dts === null ? "not declared" : timecode(ts.dts)}</div>` +
    html`<div class="k">Type</div><div class="v">${ts.is_key_frame ? "KEYFRAME" : "predicted"}</div>` +
    html`<div class="k">Size</div><div class="v">${image.width}x${image.height}</div>`;

  const histogram = buildHistogram(image.rgb, image.width, image.height);
  viewer.append(header, buildViewerControls(), canvas, histogram, buildInspector(canvas, image, helpers));

  // The conversion note travels with the inspector. Spec §44 forbids a silent
  // conversion, and for a greyscale frame the chroma figures are 128 by
  // construction - arithmetically valid and completely uninformative.
  const note = document.createElement("p");
  note.className = "dim";
  note.textContent =
    `Decoded greyscale (luma only; chroma was not retained by the analysers' decode). ` +
    `Matrix: ${payload.basis === "luma_only" ? "none" : "ITU-R BT.601"}.`;
  viewer.append(note);
}

// ------------------------------------------------------------------- viewer

/**
 * What the viewer is showing, and what has been pinned.
 *
 * Module scope because the frame table, the canvas and the A/B controls are
 * built by different functions that must agree. Deliberately *not* called
 * `view`: every renderer already has a local `view` holding the command's
 * result, and shadowing it is how `view.zoom` silently became `undefined`.
 */
const viewerState = {
  zoom: 1,
  a: null,
  b: null,
  current: null,
  source: null,
};

/** The zoom steps, in order, with the two ends labelled as what they are. */
const ZOOMS = [1, 2, 4, 8];

/**
 * Paints a decoded frame into a canvas at a zoom level.
 *
 * Two things this gets right that the obvious loop does not:
 *
 * 1. **Stride.** The decode buffer is three bytes per pixel and the canvas is
 *    four. Stepping the canvas index by four while guarding on the *RGB* length
 *    stops after three quarters of the frame, and the remaining quarter is left
 *    at alpha zero - so a frame renders with a transparent corner and nobody can
 *    say why. The two strides are kept separate here.
 * 2. **Zoom is real, not CSS.** Magnifying with a stylesheet would resample with
 *    the compositor's filter and blur the evidence. At 8x an analyst is looking
 *    for a single wrong pixel, so the upscale is nearest-neighbour and the
 *    picture stays exactly as decoded.
 */
function drawFrame(canvas, image, zoom) {
  const width = image.width || 0;
  const height = image.height || 0;
  if (!width || !height) return canvas;
  const scale = Math.max(1, Math.round(zoom));
  canvas.width = width * scale;
  canvas.height = height * scale;
  // Display width is capped so a large frame does not push the inspector and
  // histogram off the screen; the backing store is not capped, so the pixels
  // remain exact.
  canvas.style.width = `${Math.min(canvas.width, 900)}px`;

  const ctx = canvas.getContext("2d");
  if (!ctx || !image.rgb) return canvas;
  const out = ctx.createImageData(canvas.width, canvas.height);
  for (let y = 0; y < canvas.height; y += 1) {
    const sy = Math.min(height - 1, (y / scale) | 0);
    for (let x = 0; x < canvas.width; x += 1) {
      const sx = Math.min(width - 1, (x / scale) | 0);
      const src = (sy * width + sx) * 3;
      const dst = (y * canvas.width + x) * 4;
      out.data[dst] = image.rgb[src];
      out.data[dst + 1] = image.rgb[src + 1];
      out.data[dst + 2] = image.rgb[src + 2];
      // Explicit, because `createImageData` returns a zero-filled buffer and an
      // alpha left at zero is an invisible pixel rather than a black one.
      out.data[dst + 3] = 255;
    }
  }
  ctx.putImageData(out, 0, 0);
  return canvas;
}
function buildHistogram(rgb, width, height) {
  if (!rgb) return document.createElement("div");
  const canvas = document.createElement("canvas");
  canvas.className = "histogram";
  canvas.width = 320;
  canvas.height = 100;

  const ctx = canvas.getContext("2d");
  if (!ctx) return canvas;

  const bins = new Uint32Array(256);
  const pixels = width * height;
  for (let i = 0; i < pixels * 3 && i < rgb.length; i += 3) {
    // Rec. 601 luma, matching the histogram the Rust model computes, so the two
    // agree about what "luma" means.
    const y = ((77 * rgb[i] + 150 * rgb[i + 1] + 29 * rgb[i + 2]) >> 8) & 0xff;
    bins[y] += 1;
  }

  const peak = Math.max(1, ...bins);
  ctx.fillStyle = "#0e1216";
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  ctx.fillStyle = "#e8c547";
  for (let i = 0; i < 256; i += 1) {
    const h = (bins[i] / peak) * (canvas.height - 2);
    ctx.fillRect((i / 256) * canvas.width, canvas.height - h, canvas.width / 256, h);
  }

  const wrap = document.createElement("div");
  const label = document.createElement("div");
  label.className = "dim";
  label.textContent = `Luma histogram (256 bins, ${pixels} pixels)`;
  wrap.append(label, canvas);
  return wrap;
}

/** Wires hover over the frame to the pixel inspector (spec §44). */
function buildInspector(canvas, image, helpers) {
  const panel = document.createElement("div");
  panel.className = "panel inspector";

  if (!image.rgb) {
    panel.append(text("No pixels are available for this frame.", "dim"));
    return panel;
  }

  const output = document.createElement("div");
  output.className = "mono";
  output.textContent = "Move the pointer over the frame to read a pixel.";

  canvas.addEventListener("mousemove", (event) => {
    const bounds = canvas.getBoundingClientRect();
    // The canvas is displayed at a CSS width, so the scale has to come from the
    // rendered size rather than the pixel width - otherwise every reading would
    // be offset by the zoom.
    const scaleX = image.width / bounds.width;
    const scaleY = image.height / bounds.height;
    const x = Math.floor((event.clientX - bounds.left) * scaleX);
    const y = Math.floor((event.clientY - bounds.top) * scaleY);

    if (x < 0 || y < 0 || x >= image.width || y >= image.height) return;
    const offset = (y * image.width + x) * 3;
    if (offset + 2 >= image.rgb.length) return;

    // The Y'CbCr conversion is the engine's, labelled, and it is BT.709 - the
    // convention a pixel inspector shows - which is not the matrix that produced
    // the RGB. The note below says so, because spec §44 forbids a silent
    // conversion.
    const [yc, cb, cr] = rgbToYcbcr(image.rgb[offset], image.rgb[offset + 1], image.rgb[offset + 2]);
    output.innerHTML =
      html`X: ${x} &nbsp; Y: ${y}<br />` +
      html`RGB: ${image.rgb[offset]} / ${image.rgb[offset + 1]} / ${image.rgb[offset + 2]}<br />` +
      html`Y'CbCr: ${yc} / ${cb} / ${cr}<br />` +
      html`<span class="dim">RGB as decoded (greyscale, luma only). ` +
      html`Y'CbCr computed from those values using BT.709; for a greyscale frame ` +
      html`Cb and Cr are 128 by construction and describe nothing about the ` +
      html`content's colour.</span>`;
  });

  panel.append(output);
  return panel;
}

/** Fixed-point BT.709 Y'CbCr, matching the Rust model's conversion. */
function rgbToYcbcr(r, g, b) {
  const clamp = (v) => Math.max(0, Math.min(255, v));
  return [
    clamp((19595 * r + 38470 * g + 7471 * b + 32768) >> 16),
    clamp((-11056 * r - 21712 * g + 32768 * b + 8388608) >> 16),
    clamp((32768 * r - 27440 * g - 5328 * b + 8388608) >> 16),
  ];
}

/**
 * The Audio screen (spec §19-§22, §43).
 *
 * Every figure carries the methodology that produced it (spec §21), and a
 * truncated decode says so prominently: those numbers describe a *prefix* of the
 * track, and presenting that as the whole thing is the quiet misrepresentation
 * this project exists to avoid.
 */
async function renderAudio(state) {
  const wrap = document.createDocumentFragment();
  wrap.append(heading("Audio"));
  if (!state.asset) return fragment(wrap, text("Select an asset."));

  const view = await invoke("audio_screen", { source: state.asset.source_path });

  if (view.truncated) {
    const warn = document.createElement("div");
    warn.className = "panel sev sev-SIGNIFICANT";
    warn.textContent =
      "The decode hit its frame cap. These figures describe a PREFIX of the track, " +
      "not the whole thing.";
    wrap.append(warn);
  }

  const panel = document.createElement("div");
  panel.className = "panel";
  const kv = document.createElement("dl");
  kv.className = "kv";
  const rows = [
    ["Sample rate", view.sample_rate ? `${view.sample_rate} Hz` : "not declared"],
    ["Channels", view.channels ?? "not declared"],
    ["Bit depth", view.bit_depth ? `${view.bit_depth}-bit` : "not declared"],
    ["Peak", view.peak === null ? "not measured" : view.peak.toFixed(6)],
    ["RMS", view.rms === null ? "not measured" : view.rms.toFixed(6)],
    [
      "DC offset (mean)",
      view.mean === null
        ? "not measured"
        : `${view.mean.toFixed(8)}${Math.abs(view.mean) > 1e-6 ? "  \u2014 non-zero" : ""}`,
    ],
  ];
  if (view.loudness) {
    // The methodology travels with the figure. A loudness reading without
    // "ITU-R BS.1770-4" beside it is not reproducible by a later reader.
    rows.push(["Integrated loudness", `${view.loudness.value.toFixed(1)} ${view.loudness.unit}`]);
    rows.push(["Method", view.loudness.methodology]);
  }
  if (view.loudness_range) {
    rows.push([
      "Loudness range",
      `${view.loudness_range.value.toFixed(1)} ${view.loudness_range.unit}`,
    ]);
  }
  for (const [k, v] of rows) {
    const dt = document.createElement("dt");
    dt.textContent = k;
    const dd = document.createElement("dd");
    dd.textContent = v;
    kv.append(dt, dd);
  }
  panel.append(kv);
  wrap.append(panel);

  if (view.unavailable) {
    // Zero peak would be indistinguishable from silence, and a file whose audio
    // could not be read is not a silent file.
    const note = document.createElement("div");
    note.className = "panel dim";
    note.textContent = `${view.unavailable} The file is not necessarily damaged.`;
    wrap.append(note);
    return wrap;
  }

  if (view.waveform) {
    wrap.append(heading("Waveform"));
    wrap.append(drawWaveform(view.waveform));
    const note = document.createElement("p");
    note.className = "dim";
    // The envelope is a downsampled picture of the signal, not the signal. A
    // reviewer has to know that before reading a spike as a transient.
    note.textContent =
      `Peak envelope: ${view.waveform.columns.length} columns drawn from ` +
      `${view.waveform.pcm_length} samples per channel ` +
      `(${view.waveform.samples_per_column} samples per column). ` +
      `Signal peak ${view.waveform.peak.toFixed(6)}.`;
    wrap.append(note);
  }

  if (view.silence.length) {
    const table = document.createElement("table");
    table.innerHTML =
      "<thead><tr><th>Start</th><th>Duration</th><th class='num'>Samples</th></tr></thead>";
    const body = document.createElement("tbody");
    for (const region of view.silence) {
      const row = document.createElement("tr");
      row.innerHTML =
        html`<td class="mono">${timecode(region.start ?? 0)}</td>` +
        html`<td class="mono">${timecode(region.duration ?? 0)}</td>` +
        html`<td class="num">${region.length_frames}</td>`;
      body.append(row);
    }
    table.append(body);
    wrap.append(heading("Silent regions"), table);
  }
  return wrap;
}

/**
 * Draws a waveform envelope on a canvas.
 *
 * The column's min and max are drawn from its own extremes, so a quiet passage
 * is visibly quiet. Normalising the whole display to the loudest peak would
 * render a track with a 40 dB dynamic range as uniformly full - which is the
 * difference between "this file is quiet" and "this file is fine" to anyone
 * using it for QC.
 */
function drawWaveform(waveform) {
  const canvas = document.createElement("canvas");
  canvas.className = "waveform";
  canvas.width = 1200;
  canvas.height = 160;

  const ctx = canvas.getContext("2d");
  if (!ctx) return canvas;

  ctx.fillStyle = "#0e1216";
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  ctx.strokeStyle = "#3c465a";
  ctx.beginPath();
  ctx.moveTo(0, canvas.height / 2);
  ctx.lineTo(canvas.width, canvas.height / 2);
  ctx.stroke();

  const scale = (waveform.peak > 0 ? waveform.peak : 1) * (canvas.height / 2 - 4);
  ctx.fillStyle = "#5aaaff";
  waveform.columns.forEach((column, i) => {
    const x = (i / waveform.columns.length) * canvas.width;
    const width = Math.max(1, canvas.width / waveform.columns.length - 0.5);
    const top = canvas.height / 2 - (column.max * scale);
    const bottom = canvas.height / 2 - (column.min * scale);
    ctx.fillRect(x, top, width, Math.max(1, bottom - top));
  });
  return canvas;
}

// ------------------------------------------------------------------ metadata

async function renderMetadata(state) {
  const wrap = document.createDocumentFragment();
  wrap.append(heading("Metadata"));
  if (!state.asset) return fragment(wrap, text("Select an asset."));

  const view = await invoke("metadata_report", { source: state.asset.source_path });

  if (!view.entries.length) {
    return fragment(
      wrap,
      text(
        "No metadata entries were found in this container. That is a measurement - " +
          "the file was read and declared nothing - rather than a failure to read it.",
        "panel dim"
      )
    );
  }

  const table = document.createElement("table");
  table.innerHTML =
    "<thead><tr><th>Scope</th><th>Key</th><th>Value</th><th>Read from</th></tr></thead>";
  const body = document.createElement("tbody");
  for (const entry of view.entries) {
    const row = document.createElement("tr");
    // Values are shown exactly as read. A metadata value is the evidence, and a
    // "helpfully" trimmed timestamp would no longer match the file.
    row.innerHTML =
      html`<td class="dim">${entry.scope}</td>` +
      html`<td><code>${entry.key}</code></td>` +
      html`<td class="mono">${entry.value}</td>` +
      html`<td class="dim mono">${entry.source}</td>`;
    body.append(row);
  }
  table.append(body);
  wrap.append(table);

  // Conflicts are surfaced rather than left for the analyst to spot by eye: a
  // key holding two different values across scopes is the whole point of the
  // cross-check, and it is the observation METADATA.INCONSISTENT_VALUE consumes.
  const conflicts = document.createElement("div");
  conflicts.className = "panel";
  const title = document.createElement("strong");
  title.textContent = view.conflicts.length
    ? `${view.conflicts.length} cross-scope inconsistency(ies)`
    : "No cross-scope inconsistencies found";
  conflicts.append(title);
  if (view.conflicts.length) {
    const list = document.createElement("ul");
    list.className = "limits";
    for (const conflict of view.conflicts) {
      const item = document.createElement("li");
      item.textContent = conflict;
      list.append(item);
    }
    conflicts.append(list);
  }
  wrap.append(conflicts);

  // Encoder indicators carry what they do *not* establish. A declared `Lavf58`
  // tag shown on its own invites a reader to treat it as proof of FFmpeg, which
  // is precisely the inference spec §27 rules out.
  if (view.indicators.length) {
    const indicators = document.createElement("div");
    indicators.className = "panel";
    const headingNode = document.createElement("strong");
    headingNode.textContent = "Encoder indicators";
    indicators.append(headingNode);
    const list = document.createElement("ul");
    list.className = "limits";
    for (const indicator of view.indicators) {
      const item = document.createElement("li");
      item.innerHTML =
        html`<strong>${indicator.name}</strong> [${indicator.confidence}] observed: ${indicator.observation}` +
        html`<br /><span class="dim">does not establish: ${indicator.limitations}</span>`;
      list.append(item);
    }
    indicators.append(list);
    wrap.append(indicators);
  }
  return wrap;
}

// ------------------------------------------------------------------- findings

async function renderFindings(state) {
  const wrap = document.createDocumentFragment();
  wrap.append(heading("Findings"));
  const result = await invoke("search", { text: null, scope: "findings" });
  if (!result.hits.length) return fragment(wrap, text("No findings were recorded for this case."));

  const table = document.createElement("table");
  table.innerHTML =
    "<thead><tr><th>Severity</th><th>Rule</th><th>Observation</th></tr></thead>";
  const body = document.createElement("tbody");
  for (const hit of result.hits) {
    const row = document.createElement("tr");
    row.innerHTML =
      html`<td class="sev sev-${hit.severity ?? "INFO"}">${hit.severity ?? "-"}</td>` +
      html`<td><code>${hit.reference}</code></td>` +
      html`<td>${hit.summary}</td>`;
    body.append(row);
  }
  table.append(body);
  wrap.append(table);
  return wrap;
}

// --------------------------------------------------------------- comparisons

// `helpers` is the second argument `renderScreen` passes - the screen element,
  // `banner`, `guarded` and `sourcePath`. It was not named as a parameter here,
  // so `helpers.banner` below was a free identifier: the screen rendered
  // perfectly and only threw when someone pressed Compare with an empty field.
  async function renderComparisons(state, helpers) {
  const wrap = document.createDocumentFragment();
  wrap.append(heading("Comparisons"));

  const leftInput = document.createElement("input");
  leftInput.type = "text";
  leftInput.placeholder = "Left: path to the original";
  leftInput.className = "path";
  const rightInput = document.createElement("input");
  rightInput.type = "text";
  rightInput.placeholder = "Right: path to the comparison";
  rightInput.className = "path";

  const go = document.createElement("button");
  go.type = "button";
  go.textContent = "Compare";
  go.addEventListener("click", async () => {
    if (!leftInput.value || !rightInput.value) {
      helpers.banner("error", "Both paths are required. A comparison needs two files.");
      return;
    }
    helpers.el.replaceChildren();
    helpers.el.append(heading("Comparisons"));
    helpers.el.append(await renderPair(leftInput.value, rightInput.value));
  });

  const form = document.createElement("div");
  form.className = "panel";
  form.append(leftInput, rightInput, go);
  wrap.append(form);

  return fragment(
    wrap,
    text(
      "A comparison reports each axis separately and never collapses to a single " +
        "similarity score: a re-mux and a transcode are both 'different files', and " +
        "only the axes say which difference matters. Two files that agree on " +
        "everything they declared, and declare nothing comparable elsewhere, are " +
        "reported INCOMPLETE rather than equivalent.",
      "panel dim"
    )
  );
}

/** Renders one comparison between two paths. */
async function renderPair(left, right) {
  const view = await invoke("compare_assets", { left, right });
  const wrap = document.createDocumentFragment();

  const names = document.createElement("div");
  names.className = "panel";
  names.innerHTML =
    html`<div class="k">Left</div><div class="v">${view.left_name}</div>` +
    html`<div class="k">Right</div><div class="v">${view.right_name}</div>` +
    html`<div class="k">Result</div><div class="v sev-${
      view.verdict === "EQUIVALENT" ? "INFO" : "WARNING"
    }">${view.verdict}</div>`;
  wrap.append(names);

  const table = document.createElement("table");
  table.innerHTML =
    "<thead><tr><th>Axis</th><th>Property</th><th>Verdict</th><th>Left</th><th>Right</th></tr></thead>";
  const body = document.createElement("tbody");
  for (const axis of view.axes) {
    for (const row of axis.rows) {
      const tr = document.createElement("tr");
      // Every row is printed, including the ones that agree. A reader shown only
      // the differences would conclude the files match on everything else -
      // which is the one inference this screen must not invite.
      const tone = row.verdict === "SAME" ? "dim" : "sev sev-SIGNIFICANT";
      tr.innerHTML =
        html`<td class="dim">${axis.axis}</td>` +
        html`<td>${row.field}</td>` +
        html`<td class="${tone}">${row.verdict}${row.reason ? ` \u2014 ${row.reason}` : ""}</td>` +
        html`<td class="mono">${row.left ?? "-"}</td>` +
        html`<td class="mono">${row.right ?? "-"}</td>`;
      body.append(tr);
    }
  }
  table.append(body);
  wrap.append(table);

  // The tolerance travels with the result it judged. "Within tolerance" with no
  // tolerance shown is a claim the reader cannot check.
  const tolerance = document.createElement("div");
  tolerance.className = "panel";
  const list = document.createElement("dl");
  list.className = "kv";
  for (const [name, row] of [
    ["Scene changes", view.scene_changes],
    ["Total silence", view.silence_totals],
    ["Integrated loudness", view.loudness],
  ]) {
    const dt = document.createElement("dt");
    dt.textContent = name;
    const dd = document.createElement("dd");
    dd.textContent =
      `${row.verdict}` +
      (row.delta !== null ? ` (gap ${row.delta.toFixed(2)}, tolerance ${row.tolerance})` : "") +
      (row.unmeasured_side ? ` \u2014 ${row.unmeasured_side} not measured` : "");
    list.append(dt, dd);
  }
  tolerance.append(heading("Tolerance-based axes"), list);
  wrap.append(tolerance);

  if (view.unmatched.length) {
    const unmatched = document.createElement("div");
    unmatched.className = "panel dim";
    unmatched.textContent =
      `${view.unmatched.length} stream(s) had no counterpart: ` +
      view.unmatched.map((u) => `${u.kind} on the ${u.side} side`).join(", ") +
      ". Streams pair by kind and position, so one dropped audio track reports one " +
      "unmatched stream rather than every later track as changed.";
    wrap.append(unmatched);
  }
  return wrap;
}

// ------------------------------------------------------- evidence and reports

async function renderEvidence(state) {
  const wrap = document.createDocumentFragment();
  wrap.append(heading("Evidence"));
  const result = await invoke("search", { text: null, scope: "evidence" });
  if (!result.hits.length) return fragment(wrap, text("No artefacts have been retained for this case."));

  const table = document.createElement("table");
  table.innerHTML = "<thead><tr><th>Reference</th><th>Summary</th></tr></thead>";
  const body = document.createElement("tbody");
  for (const hit of result.hits) {
    const row = document.createElement("tr");
    row.innerHTML = html`<td><code>${hit.reference}</code></td><td>${hit.summary}</td>`;
    body.append(row);
  }
  table.append(body);
  wrap.append(table);
  return wrap;
}

// `guarded` arrives through the same second argument; it was not named as a
  // parameter, so generating a report threw a ReferenceError on the one screen
  // whose entire purpose is generating reports.
  async function renderReports(state, helpers) {
  const wrap = document.createDocumentFragment();
  wrap.append(heading("Reports"));
  if (!state.case) return fragment(wrap, text("Open a case first."));

  const go = document.createElement("button");
  go.type = "button";
  go.textContent = "Generate report bundle";
  const holder = document.createElement("div");
  go.addEventListener("click", async () => {
    go.disabled = true;
    go.textContent = "Generating\u2026";
    const view = await helpers.guarded(() => invoke("generate_report"));
    go.disabled = false;
    go.textContent = "Generate report bundle";
    if (view) holder.replaceChildren(renderReport(view));
  });

  wrap.append(go, holder);
  return wrap;
}

/**
 * Renders a generated report bundle.
 *
 * The disclaimer is printed verbatim and in full. Spec §59 requires it on every
 * report, and it is the sentence that stops a reader treating the findings as a
 * verdict - so it is rendered here rather than left to a renderer that might
 * truncate it.
 */
function renderReport(view) {
  const wrap = document.createDocumentFragment();

  const summary = document.createElement("div");
  summary.className = "panel";
  summary.innerHTML =
    html`<div class="k">Case</div><div class="v">${view.case_name}</div>` +
    html`<div class="k">Findings</div><div class="v">${view.finding_count}</div>` +
    html`<div class="k">Fingerprint</div><div class="v mono">${view.analysis_fingerprint}</div>` +
    html`<div class="k">Written to</div><div class="v mono dim">${view.directory}</div>`;
  wrap.append(summary);

  const table = document.createElement("table");
  table.innerHTML =
    "<thead><tr><th>File</th><th class='num'>Size</th><th>SHA-256</th></tr></thead>";
  const body = document.createElement("tbody");
  for (const file of view.files) {
    const row = document.createElement("tr");
    row.innerHTML =
      html`<td>${file.name}</td>` +
      html`<td class="num">${bytes(file.size_bytes)}</td>` +
      html`<td class="mono dim">${file.sha256.slice(0, 16)}\u2026</td>`;
    body.append(row);
  }
  table.append(body);
  wrap.append(table);

  const disclaimer = document.createElement("pre");
  disclaimer.className = "panel mono dim";
  disclaimer.style.whiteSpace = "pre-wrap";
  disclaimer.textContent = view.disclaimer;
  wrap.append(disclaimer);
  return wrap;
}
