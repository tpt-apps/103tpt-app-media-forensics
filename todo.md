# TPT Media Forensics — Project Todo

Tracks all work for the whole project, organized in phases per `spec.txt`.
License: dual **MIT OR Apache-2.0**, copyright TPT Solutions.

## Phase 0 — Repository & Foundation Setup
- [x] Initialize git repository
- [x] Add dual license: `LICENSE-MIT` + `LICENSE-APACHE`, copyright TPT Solutions
- [x] Add `README.md`, `CHANGELOG.md`
- [x] Create Cargo workspace (`Cargo.toml`) and crate skeletons per spec §8:
      core, model, container, video, audio, timing, metadata, rules,
      evidence, report, cli, tauri
- [x] Add TPT foundation crates as workspace dependencies (tpt-kinetix,
      tpt-cadence, tpt-visual, tpt-audio, tpt-voice, tpt-av-asset,
      tpt-av-sync, tpt-av-test, tpt-dsp)
      — wired as **pinned-rev git dependencies** (the ecosystem convention;
      none are on crates.io). See `docs/foundation.md` for the crate-name
      mapping and three spec/ecosystem discrepancies found during integration.
- [x] Set up `docs/` skeleton (architecture, evidence-model, analysis-model,
      findings, report-format, rules)
- [x] Set up `rules/`, `fixtures/`, `tests/` directories

### Phase 0 — completed beyond the checklist
- [x] Define Case model (§9) and Asset model (§10) — implemented in `-model`
- [x] Implement asset acquisition record (§11): size, timestamps, SHA-256 /
      BLAKE3 hash *types* and integrity verification; hashing I/O lands with
      the acquisition step
- [x] Define Finding model with severity + evidence + confidence (§34)
- [x] Define Evidence model with integrity metadata and provenance (§32–33)
- [x] Implement analysis cache keying on asset hash + analysis version +
      profile hash + rule-set hash (§54)


## Phase 1 — MVP (spec §84, §96, §97)
- [x] Define Case model (§9) and Asset model (§10)
- [x] Implement SQLite persistence (§52) for cases/assets/analyses/streams/
      findings/evidence/rules/reports/notes — versioned schema, enforced
      foreign keys, append-only findings/evidence, transactional writes
- [x] Implement case directory layout (§53) and manifest format (§58)
- [x] Implement asset acquisition record (§11): path, size, timestamps,
      SHA-256, BLAKE3, filesystem info — read-only source guarantee
      Done in `-core/src/acquisition.rs`: both digests in one pass, timestamps
      via `SystemTime`, and filesystem metadata via `symlink_metadata` so a
      symlink in evidence is recorded rather than silently followed.
- [x] Integrate tpt-kinetix (container demux: `tpt-kinetix-core`, `-demux`)
- [x] Integrate tpt-kinetix-vp9 and -av1 for frame decoding (royalty-free; never
      implement them ourselves. H.264 is patent-encumbered and not decoded.
      Tier-2 only — see docs/decoding-tiers.md)
      Pins: tpt-kinetix 28cefd8, tpt-cadence 95ff6bf; `AnalysisVersion::CURRENT` = 2.
      Only 8-bit planar frames are used; H.264/HEVC/AAC are identify-only (Tier 1).
- [x] Verify Tier-2 end to end on a real VP9 and AV1 clip
      **AV1 is now verified end to end**, with no ffmpeg on the dev machine:
      `-video/tests/tier2_end_to_end.rs` encodes real AV1 with the foundation's
      rav1e-backed encoder, wraps it in a real WebM container, reads it back
      through the demuxer, and decodes it to pixels. 7 tests.
      **VP9 remains structurally tested only** — the foundation ships no VP9
      encoder, so there is no way to produce a genuine VP9 stream here. Stated
      rather than implied.
- [x] Open WebM/MKV via `tpt-kinetix-demux::mkv` (new `mkv.rs` beside `mp4.rs`)
      The reader exposes keyframes, timestamps and sample bytes, and nothing
      else: no picture geometry, no sample rate. Those are reported as
      unmeasured rather than defaulted. Wired into the pipeline and the CLI.
- [x] Wire royalty-free audio decode via tpt-cadence: Opus (`-opus`, `-ogg`) and
      Vorbis. Skip AAC (patent-encumbered)
      `-audio::decode`; the CLI `audio` command detects the codec from the file's
      own bytes. 8 round-trip tests encode real streams with the foundation's
      encoders and decode them back.
- [x] Reconcile rule count: `builtin.rs` has 23 rule IDs, not 21.
      `docs/rules.md` now says twenty-three, lists all of them, and a test
      asserts the list matches `builtin_rules()` so it cannot drift again.
- [x] Integrate tpt-cadence (	pt-av-cadence-core, -wav)
- [x] Implement container/stream inspection (§12–13)
- [x] Implement video analysis: structural, temporal, spatial, colour (§14)
      Structural, temporal and spatial are done (GOP, duplicates, near-duplicates,
      scene changes). **Colour is now done for ISO-BMFF**: `-container/src/colr.rs`
      reads `colr` (`nclx` and `nclc` separately, so range is reported only where
      the box declares it) plus `mdcv` and `clli`, walking
      `trak/mdia/minf/stbl/stsd/<entry>` from the spec's 78-byte
      `VisualSampleEntry` offset. `is_hdr` is now a measurement — BT.2020 primaries
      or a PQ/HLG transfer — rather than a hardcoded `false`. Unrecognised codes
      are reported as `unrecognised code N` rather than dropped, and an unparsable
      `mdcv` leaves `None` rather than a fabricated zero.
      **Not done: Matroska.** That reader exposes no picture geometry, so a WebM
      stream has no `VideoFormat` to attach colour to; inventing one to hold a
      primaries value would put a resolution in a report that no measurement
      produced. Listed in the README as a named gap instead.
      `VIDEO.HDR_METADATA_MISSING` consumes this (§45): HDR signalled with no
      `mdcv`/`clli` is Warning/High, reported as a comparison of the file's own
      declarations with no cause asserted.
- [x] Implement GOP analysis (§15) — packet-layer only; no decoding required
- [x] Implement frame analysis & duplicate detection (§16–17) — exact duplication
      at the packet layer, no decoding
- [x] Implement near-duplicate detection (needs decoded pixels, Tier 2)
      `video/src/near_duplicate.rs`; mean luma per 8x8 block reduced to a 64-bit
      perceptual hash, compared by Hamming distance. Wired at the Tier-2 stage.
- [x] Implement scene-change analysis (§18)
      `video/src/scene.rs`; consecutive-frame differences over decoded frames
- [x] Implement audio analysis: levels, silence, DC offset, loudness (§19–22 partial)
      **The pipeline now actually runs this stage.** `measure_audio` existed but
      was never called, so `AUDIO.CLIPPING`, `AUDIO.DC_OFFSET`,
      `AUDIO.SILENCE_REGION`, and `AUDIO.INAUDIBLE` were permanently dead on
      every file — each had tests, all of which hand-built the bundle. Verified
      now against real Opus decoded from a real WebM file.
- [x] Implement spectral analysis (FFT) and loudness-range measurement
      **Both halves are now done**, completing §22.
      **FFT:** `-audio/src/spectral.rs`, 2048-point Hann-windowed FFT at 50%
      overlap, reporting peak frequency, spectral centroid, Wiener flatness, and
      low/high energy ratios. Every figure carries `Methodology::HannWindowFft`,
      because spec §21 forbids reporting a number without the method behind it
      and a spectrum with different window parameters is a different measurement,
      not a rougher one. The window's 1.5-bin equivalent noise bandwidth is
      reported so a reader knows what one bin is worth.
      Two decisions worth recording: energy is accumulated in linear power rather
      than dB, because summing decibels depends on how many bins you added; and
      floored bins count as *no* energy rather than as a very quiet level, because
      converting them back to linear magnitude and summing invented enough power
      for digital silence to report a spectral centroid. Both were caught by tests
      written for the obvious behaviour.
      **Loudness range:** `loudness_range` in `-audio/src/loudness.rs`, per EBU
      Tech 3342 — the 10th to 95th percentile of 3-second short-term loudness.
      Reuses the existing BS.1770-4 K-weighting; `block_loudness_series` was
      generalised over window length rather than duplicated. The same -70 LUFS
      absolute gate applies, without which silent blocks would drag the 10th
      percentile to the floor and a silent file would report a huge range.
      `Methodology::EbuR128Lra` is a *separate* variant from `ItuBs1770_4` and
      renders in **LU, not LUFS** — a range printed as LUFS presents a span as an
      absolute level. A 2-second file gets `TooShortForShortTerm`, distinct from
      `TooShort`, because it is long enough for integrated loudness and too short
      for LRA and saying so is the point.
- [x] Implement A/V synchronisation analysis (§23) — initial offset, final
      offset, and drift measured over the span
      **Note:** spec §23 names `tpt-av-sync` for this, but that crate is a CRDT
      collaboration engine with no A/V measurement capability (confirmed by
      source search). Implemented directly in `-timing`; see `docs/foundation.md`
      **The pipeline now actually runs this.** `av_sync::analyse` was fully
      implemented and tested while nothing called it, so `bundle.sync` stayed
      `None` and `TIMING.AV_SYNC_DRIFT` could never fire — the same shape as the
      audio gap. Both tracks are now located by kind rather than by position.
- [x] Implement timestamp forensics (PTS/DTS, monotonicity, gaps) (§24)
- [x] Implement metadata extraction + consistency cross-checks (§25–26)
      — text atoms with scope/source provenance; conflicts are flagged
      without asserting a cause
- [x] Implement encoder fingerprinting (best-effort, confidence-labelled) (§27)
      `-metadata/src/fingerprint.rs`. Reads the `©too` encoder tag and the
      all-intra structure; each indicator carries a confidence grade and a
      statement of what it does not establish. **No grade above `Medium` exists**,
      deliberately: a declared tag is a string anyone can write, so nothing
      measured here can support a claim about *which program* produced a file.
      Bitstream and quantisation evidence from §27 is not collected — this build
      parses no coded slice data — and that is stated as `not_measured` rather
      than omitted, because a fingerprint that hides its inputs reads as complete.
      `©nam` and `©cmt` are deliberately not matched: a file named after its
      camera is not evidence about what wrote it.
- [x] Implement compression/bitrate analysis + anomaly detection (§28–29)
      `video/src/bitrate.rs`: sliding window over compressed sample sizes, no
      decoder needed. `VIDEO.BITRATE_DROP` reproduces spec §29's own worked
      example — average, segment, observed — and stops there, because the spec
      lists the possible causes (low-complexity content, a still shot) and the
      engine cannot choose between them from a size table.
      Two profile thresholds: `bitrate_window_frames` (12) and
      `bitrate_anomaly_ratio` (0.5). Windows slide with a one-sample stride
      because a step aligned to a window edge is the signature of a splice.
      Keyframe count rides with each finding — a keyframe-free window is a
      different observation from one where content stopped changing.
- [x] Implement corruption detection with graceful continuation (§30)
      **Three layers now**, because "corruption" is three different questions
      and answering only the first left the two a decode could see unanswered.
      **Structural** damage is in `-container/src/damage.rs`: a shallow
      independent walk of the top-level box list, separate from the demuxer,
      because the demuxer reports success for everything before the bytes
      stopped making sense and so has by construction lost the boundary. Four
      typed defects — truncation, trailing data, impossible box size, and a
      non-printable box type (the signature of a reader that has lost sync).
      **Packet** damage is in `-container/src/packets.rs`: what is wrong with the
      *access units* those boxes point at, which a well-formed box list says
      nothing about. Two typed defects — an access unit with no bytes, and an
      index promising more samples than can be read. It is deliberately
      **decoder-free**, so it also covers the H.264 and AAC tracks this engine
      never decodes; a decoder-based check would skip exactly the files a
      working professional most often hands over. **Decode** damage is in
      `-video/src/decode.rs`, and it is where spec §30's "a scan should continue
      after recoverable errors" is actually implemented — see below.
      Five rules grade them at three severities: `CONTAINER.TRUNCATED_MEDIA`
      (Critical/High — declared media is *absent*), `CONTAINER.UNREADABLE_PACKET`
      and `VIDEO.DECODE_FAILURE` (Significant/High — present but unusable, so a
      lesser and genuinely different problem), and `CONTAINER.STRUCTURAL_DEFECT`
      (Warning/Medium — appended data does not mean content is absent).

      **Graceful continuation means resynchronising, not persisting.** After a
      decode failure the session skips forward to the next keyframe rather than
      feeding the following predicted frames: their reference is gone, and
      decoding them anyway produces output that *looks* like a frame and is
      wrong. A plausible-looking wrong frame is the worst outcome this engine
      can produce, because a scene-change finding computed from it reads as a
      measurement. The skipped span is recorded as `LostReference` so the gap is
      visible rather than a silent hole in a frame count. `scene::analyse`
      refuses to compare across that gap — comparing frames 5 and 12 would
      measure seven frames of elapsed footage and report it as one step, which on
      any real cut is a large confident scene change that is an artefact of the
      recovery.

      **The decoder does not report undecodable packets.** Found by measurement,
      not by reading the source: flipping one packet's bytes in a nine-packet AV1
      stream yields eight frames and **no error**, and pure garbage yields zero
      frames and still no error. A scheme built on `Err` alone would have called
      both files clean while measuring nothing at all. So silence is read as
      damage — for a **keyframe** only, since a keyframe is self-contained and
      cannot be withheld for reordering — and a predicted frame dropped the same
      way is caught by reconciling the gaps in the recovered frame indices. Both
      paths dedupe against each other, so one lost packet is one defect and the
      count spec §30 asks the report to print is not inflated.

      **Still not done:** audio decode errors are not a typed defect yet — the
      audio decoder's failure is still recorded as a limitation string rather than
      routed through the same `DecodeDamage` type, and spec §30 lists audio decode
      errors alongside the video ones.
- [x] Implement error/anomaly timeline (§31)
      **The unified timeline now exists**: `-model/src/timeline.rs` plus
      `build_timeline` in the pipeline. It merges three sources into one ordered
      list — structural damage (placed through the sample index), timestamp
      anomalies (placed at the sample index the scanner knew), and rule findings
      (placed where their rule put them) — and every entry carries the *source* and
      a `Placement` of `Measured` / `Inferred` / `Unplaced`.
      That distinction is the whole point: a structural defect's timecode comes
      from accumulating sample sizes, while a timestamp anomaly's comes from the
      sample's own stamp, and rendering both as `00:00:10` would let an inference
      be quoted as a measurement. `describe()` prints `inferred` next to the
      timecode.
      An observation with no position becomes `Unplaced` and sorts **last**, never
      at `00:00:00`. Findings without a position are kept, not dropped — the
      timeline must account for every finding or it disagrees with the list above
      it.
      Ordering is total and deterministic (time, source, reference) so two runs
      produce identical reports per spec §77.
      **Two things deliberately not done.** Timestamp gaps and overlaps carry a
      *size*, not a time, and `TimestampReport` does not retain the timestamps it
      scanned — placing them would need a second scan, so they stay off the
      timeline rather than at a fabricated instant. And `mdat`-before-`moov` files
      still get no placement, because the anchor walk stops at `moov`.
- [x] Define Finding model with severity + evidence + confidence (§34)
      `-model/src/finding.rs`; duplicated in Phase 0 above, left ticked there
      too rather than removed, since both lines refer to the same type
- [x] Implement rule engine (`ForensicRule` trait) + rule profiles (§35–37)
- [x] Implement first ~20 forensic rules across container/video/audio/
      timing/metadata — 23 builtin rules, matching `builtin_rules()` exactly
      (a test asserts the documented list and the registered list agree)
- [x] Confirm all 25 rules are actually reachable. Five were not:
      `measure_audio` and `av_sync::analyse` were never called by the
      pipeline, and `DECLARED_VS_MEASURED_MISMATCH` returned an empty vector
      by construction. `required_inputs` is now mandatory on every rule and
      `-core/tests/stage_guard.rs` fails when a declared input is never
      populated, so the same class of gap cannot return silently
- [x] Implement Evidence model + integrity metadata (hashes, provenance) (§32–33)
- [x] Implement frame extraction as evidence
      `-video/src/frame.rs`. Converts a decoded `VideoFrame` to interleaved 8-bit
      RGB and writes it as a real PNG, ready for
      `EvidenceStore::write_verified` with `EvidenceKind::ExtractedFrame` and
      `Provenance::LosslessExtract`.
      **Why PNG and not an in-house format:** evidence a reviewer cannot open with
      their own tools is not evidence, it is an assertion. The `png` crate is
      vendored locally, so this costs no network access.
      Handles 4:2:0, 4:2:2 and 4:4:4 planar YUV, plus RGB, BGR and monochrome —
      the last three reordered rather than converted, so the bytes stay the
      decoder's own. **10- and 12-bit YUV are refused**, not approximated:
      converting them needs dithering or truncation, either of which changes pixel
      values and makes the result lossy rather than evidence.
      **No scaling.** A thumbnail is a better artefact and worse evidence, so it is
      not produced; the extracted frame is the decoded frame.
      The BT.601 matrix is named on every `FrameImage`, because a YUV frame has no
      inherent RGB appearance — the decoder does not report a colour space, and a
      reviewer comparing against a reference decode needs to know which convention
      produced the colours.
      A truncated frame is refused rather than padded: padding would produce a
      plausible image with invented pixels along one edge.
      **Wiring correction.** This item was marked done while the extraction
      library existed and nothing called it. Tier-2 decoded frames, measured
      scene changes and near-duplicates from them, and dropped the pixels;
      `Report.evidence` was hardcoded `Vec::new()` at both construction sites and
      the `evidence` table had no writer. The README's "decoded frames written as
      real PNGs" was checked by grepping for `pub fn to_png`, which passed while
      the capability did not exist.
      Now wired end to end: `run_stages` returns the frames alongside the
      bundle, `analyse` writes each as a PNG through `EvidenceStore::write_verified`
      (which re-reads and re-hashes from disk before marking it verified), attaches
      the matching id to the pixel findings that measured on that frame, and
      persists the records. Artefact count is bounded by
      `DecodeLimits::max_frames_in_memory`, so evidence cannot turn a long master
      into an unbounded number of PNGs. A cache hit writes none.
      **Frames are greyscale, and say so.** `DecodedFrame` retains the luma plane
      only — chroma is what the pixel analysers do not need, and keeping it would
      multiply the memory that bound exists to avoid. Assuming neutral chroma
      would render a saturated frame as grey and hide the colour shift a reviewer
      may be looking for, so `DecodedFrame::to_greyscale` passes luma through
      untouched and each caption states the frame is greyscale. An evidence frame
      that disagrees with the finding it supports is worse than none.
      Guarded by `-core/tests/evidence.rs`, which runs a real AV1 fixture end to
      end and asserts the PNGs are written, verified, persisted, and cited.
- [x] Implement analysis cache keyed on asset hash + analysis version +
      profile hash + rule-set hash (§54)
- [x] Implement large-file/streaming analysis with bounded memory,
      background workers, cancellation, progress reporting (§55–56)
      **Bounded memory is done** — acquisition and the container paths read
      through fixed-size buffers with no whole-file allocation, and acquisition
      detects a source changing size mid-read rather than recording a hash of a
      moving target.
      **Progress and cancellation are done.** `-core/src/progress.rs`:
      `Stage` (8 stages, one ordered list so ordinals cannot drift),
      `Progress::Started`/`Finished` with an optional completed/total pair,
      `Cancellation` (an `Arc<AtomicBool>` that clones share rather than copy),
      and `ProgressTracker` which reports *and* checks at each boundary.
      `AnalysisEngine::analyse_with` is the new entry point; `analyse` delegates
      to it with a silent tracker, so the common path costs nothing and the two
      cannot diverge.
      **Background workers are done.** `-core/src/worker.rs`: `AnalysisJob` runs
      an analysis on its own thread and hands back a handle with `cancel()`,
      `is_finished()`, and `join()`. The engine still spawns nothing on a
      caller's behalf — `spawn` is an ordinary call — and no async runtime was
      introduced, because a library that pulls in a runtime makes the choice for
      every consumer that wanted one. The CLI is a real caller: `analyze` runs
      through `AnalysisJob` and prints each stage to stderr, so `--json` on
      stdout stays a single parseable document.
      **Parallelism is done.** The four independent analysers §56 names —
      sample index/bitrate, audio, Tier-2, metadata — run concurrently through
      `std::thread::scope` over `Stage::CONCURRENT`. `WorkerBudget` sizes the fan-out
      from `available_parallelism()` so cores are not oversubscribed, and
      `WorkerBudget::serial()` gives the identical result on one thread for a
      caller whose binding constraint is peak memory.
      **Parallelism is invisible in the output, and that is tested.** Branches
      return their own measurements *and* their own limitations, then get sorted
      back into `Stage::CONCURRENT` order before anything is merged or reported.
      Merging in completion order would make a report's wording depend on how
      fast one decoder happened to be, breaking §77. `tests/concurrency.rs`
      asserts the serial and parallel paths agree on findings, limitations
      *and their order*, and on the progress event stream itself — removing that
      sort makes the progress test fail, which is what makes it worth having.
      Because the four stages now overlap, each reports through
      `Progress::BranchFinished` and `Stage::span()` places every event inside the
      group's range, so the bar advances smoothly and never moves backwards even
      when branches finish out of order.
      Cancellation remains *cooperative and coarse*: a cancel request during a
      90-second decode is observed when that decode returns, because the decoder
      has no cancellation hook. That limitation is documented on the module and
      on `Stage`, and `CoreError::Cancelled` is its own variant so a caller can
      retry without treating it as a corrupt asset. A cancelled run writes
      nothing to the case database — a partial analysis record that looked
      complete would be worse than none.
      `CoreError::WorkerFailed` is its own variant too: a worker thread that
      panics is *reported* with the path and the panic message rather than
      silently dying, which is the one way a background thread would otherwise
      turn a decoder defect into a hang. Parallelism trades peak memory for
      wall-clock — two branches hold decoded media at once — but each is still
      capped by its own `DecodeLimits`, so the growth is a bounded factor.
      Progress deliberately reports no ETA: an estimate on a feature-length master
      would be wrong by minutes and reads as a promise the engine cannot make.
      Fractional progress appears only where a total is genuinely known.
      Still not done: batch mode runs sequentially even though a folder of a
      thousand files would obviously benefit from threads, because the analyses
      would share one SQLite file and the interleaving of two writers' database
      work would become timing-dependent. `worker.rs` documents the reasoning.
- [x] Implement file-to-file comparison engine (duration, streams, codec,
      colour, audio, metadata, timestamps, scene structure) (§38–40)
      **The engine was already written. Nothing could reach it.** Two commits
      (`629ff65`, `91a28f9`) added `-model/src/comparison.rs` and
      `-rules/src/comparison.rs` — 66 KB of comparison code, split across the two
      crates because feeding the aggregate needs types the model crate must not
      depend on — documented in the CHANGELOG, and unit-tested. A grep for callers
      outside its own tests returned nothing but the two `pub use` re-export lines.
      This is the *same defect* `tests/stage_guard.rs` was written to prevent,
      in the one place the guard does not reach: the guard checks that a
      `BundleInput` was populated, and comparison produces no `BundleInput` at
      all, so two complete subsystems could ship unreachable and every test stay
      green. The engine was healthy and its caller was missing.
      **Fixed by wiring it.** A `compare <a> <b>` CLI subcommand analyses both
      files and renders the result. It takes no case directory on purpose: a
      comparison is a question *about* two files, not a finding *about* an asset,
      so writing an analysis record for each would put evidence in a case that
      nobody examined.
      The output labels every property `same`, `DIFFERS`, or `NOT COMPARED`
      rather than printing only the differences — a reader shown only differences
      would conclude the files match on everything else, which is the one
      inference this command must not invite. The summary line distinguishes
      "differences found" from "no differences but not every axis was
      comparable", because `is_equivalent()` is deliberately false whenever
      anything went uncomparable.
      `readme_claims.rs` no longer denies the capability; it now *checks* it
      against the CLI rather than against the engine, so the claim is anchored to
      the caller that makes it true.
      Not done: comparison against a designated reference master recorded in the
      case (§67). `compare` takes two arbitrary paths and has no notion of a
      stored reference.
- [x] Implement search over case data (§41)
      `-core/src/store/search.rs`: `SearchQuery` with text, scope
      (findings/assets/evidence), severity floor, asset filter, and row limit.
      Scopes UNION in SQL and sort once, most severe first. Every term is a bound
      parameter with LIKE wildcards escaped, so there is no expression syntax to
      learn and no way to inject one.
      Three distinctions the tests pin: a blank term matches nothing while
      `SearchQuery::all()` lists everything; a severity filter constrains findings
      only and does not hide assets; truncation is reported via `truncated` plus a
      separate `total`, never implied by a short page.
      Searches are bounded at 200 rows by default. Not done: full-text ranking
      (matching is a substring `LIKE`, so results are ordered by severity and id,
      not relevance), and no FTS index.
      **Then wired.** For all of the above, `Store::search` had no caller: the
      29 KB module was committed, documented, and unit-tested, and `analyze` wrote
      findings into the case database that nothing could read back out. An
      examiner who wanted one finding out of five thousand had no way to name it.
      A grep for `SearchQuery`/`SearchScope` outside `search.rs` returned a single
      hit — the `pub use` re-export.
      `search --case-dir <case> [term]` is the caller, with `--scope`,
      `--min-severity`, and `--limit`. The module's central distinction is carried
      into the text output rather than left implicit: a truncated page prints
      `N of M matches (truncated by --limit)` and says how many more exist, because
      "200 results" beside a 200-row list is a false statement about the case.
      JSON echoes the term and scope as well as the results, so a saved document
      is unambiguous about what was asked for.
- [x] Implement report generation: PDF, HTML, JSON, CSV (findings +
      measurements), with required disclaimers (§59–63)
      `-report`: `pdf.rs`, `html.rs`, `render.rs`, `bundle.rs`, plus a shared
      `Methodology` that cannot be defaulted, so a report cannot be built
      without naming software version, analysis version, profile and rule set.
- [x] Implement CLI (`inspect`, `hash`, `analyze`, `report`, batch) sharing
      the core engine with the GUI (§51)
      Subcommands: `Hash`, `Acquire`, `Audio`, `Metadata`, `Inspect`, `Analyze`,
      `Report`, `Batch`. No analysis logic of its own.
- [x] Implement batch analysis engine + directory batch mode (§48–49)
      `-core/src/batch.rs`; per-file outcomes are Analysed/Failed/Skipped, so a
      single unreadable file does not abort the directory
- [x] Build Tauri desktop app shell wrapping the same core engine (§78)
      Tauri 2.11 with a **no-bundler frontend** (plain ES modules under `ui/`,
      no Node dependency at all). The build is Rust-only, which is what keeps
      §76's reproducibility claim about the desktop app honest: a Node/pnpm
      toolchain in the build path is a second supply chain whose lockfile would
      need the same pinned-revision discipline as the Cargo one.
      `tauri.conf.json` is validated at compile time by `tauri-build`, so a
      malformed config is a build error rather than a blank window.
      Capabilities are **minimal by design**: `core:default` and the dialog
      picker, nothing else. Every path enters through a dialog rather than typed
      text (§11), and the application needs no general filesystem or shell
      permission because it writes only inside a case directory.
      Builds and produces a 15.9 MB executable; verified with
      `cargo build` in this directory.
- [x] Implement core UI screens: Case, Assets, Overview, Streams, Timeline,
      Video, Audio, Metadata, Findings, Comparisons, Evidence, Reports (§79)
      **The split that makes this testable.** Screens are ordinary Rust types
      in `src/view/`, not code inside `#[tauri::command]` functions. A screen
      built inside a command needs a webview, a serialisation boundary and an
      event loop to run; a screen built as a struct needs none. That is the
      difference between the screens having 132 unit tests and having none.
      The navigation list is sent from the backend (`commands::screens`) rather
      than hardcoded in the frontend, so a screen cannot exist without a command
      behind it and the spec §79 ordering cannot drift.
      All twelve screens have a renderer and a model.
      **Overview and Streams are now plumbed** through `commands::inspect_asset`,
      which reads the source read-only through the same `-container` entry points
      the rules used. They show the declared-versus-measured duration pair when
      they disagree (that disagreement *is* the finding, spec §26), the codec tag
      exactly as declared rather than normalised, and print the container's
      reason when it yields no streams — "could not be parsed" and "declared none"
      are different states and a blank table conflates them.
      **The analysis path is now complete**: `analyse` spawns an `AnalysisJob`,
      emits progress as events, `poll_analysis` collects the result, and
      `cancel_analysis` stops it. `tests/analysis_run.rs` drives that sequence
      end to end without a window.
      **Metadata, Comparisons and Reports are now plumbed** as well.
      `metadata_report` reads the tree and the cross-scope conflicts through the
      pipeline's own extraction, so the screen cannot disagree with the findings
      about which atoms were present; conflicts are surfaced rather than left for
      the analyst to spot by eye. `compare_assets` runs `observe_stages` on both
      sides — no case, no analysis record, nothing written — and carries the
      engine's own tolerances so a screen showing "within tolerance" also shows
      what it judged against. `generate_report` rebuilds from the case database
      rather than re-analysing, so it cannot describe a different run than the
      findings came from.
      **Video and Audio are now plumbed too.** All twelve screens display real
      engine data and all fifteen commands resolve in both directions — checked
      mechanically rather than by reading both sides.
      The Video screen shows the container's **frame table** with PTS *and* DTS
      per frame, because their disagreement is the observation worth making and a
      viewer showing only PTS renders a reordered stream as if it played in order.
      A file whose codec this build deliberately does not decode still gets a
      full screen: its timing is readable and is exactly what a reviewer needs
      when a file will not play. The refusal is worded as policy — "this build
      does not decode `avc1`... the file is not damaged" — because "we do not
      decode this" and "we could not decode this" are different statements and
      the second would send someone hunting for a corrupt file that is intact.
      The Audio screen decodes through the same `-audio` decoder the rules used, so
      a level or loudness figure on screen is the figure a finding was raised
      from. Every measurement carries its methodology (spec §21), and a decode
      that hit its frame cap says so prominently: those numbers describe a
      *prefix* of the track.
      **The viewer displays pixels.** `decode_frame` decodes one frame on demand
      and `tests/frame_decode.rs` proves it end to end on genuinely encoded AV1
      wrapped in a real WebM container — compressed bytes in, pixels out, through
      the same demuxer and decoder the pipeline uses.
      **I was wrong about this one, and it is worth recording.** I reported the
      pixel viewer as needing "an engine change: retention plus a bounded
      window". That was not checked, it was assumed. `DecodeSession::decode_prefix`,
      `read_samples_file` and `DecodedFrame::to_greyscale` are all already public
      in `-video` and `-container`; what the viewer needed was never an engine
      feature but a command calling what existed. Assuming a boundary and
      reporting it as a finding is the same error as duplicating a rule and
      calling it a feature — and this repository's own history has three
      instances of it.
      Decoding is **per click rather than retained**. A 4K master has tens of
      thousands of frames at ~25 MB of RGB each; retaining them would exhaust
      memory before anyone reached the end, and a viewer that loads everything
      before showing frame one looks like a hung application. Stepping backward
      re-decodes, which is the right trade: an analyst inspects the frames that
      matter.
- [x] Implement dashboard (asset/finding/severity counts, status) (§80)
      `view/dashboard.rs`. Counts by severity, plus `REVIEW REQUIRED` /
      `CLEAR` / `INCOMPLETE`.
      **There is no authenticity score, and adding one is not a formatting
      change.** Spec §80 forbids it because a single number would have to
      discard every `Confidence` and every rule's stated limitation (§71) to
      exist, and it would be the one figure an analyst quotes to someone who
      never opens the report. The spec's mock-up omits an INFO count; it is
      included here so the four severity buckets sum to the total shown above
      them, and a test asserts that invariant.
      `INCOMPLETE` outranks everything: a run that has not finished has not had
      the chance to produce findings. `Failed` and `Cancelled` are terminal for
      the engine but are not a *completed* examination, so they land there too —
      otherwise a run that died halfway would report itself clear.
- [x] Implement timeline UI as central navigation (video/audio/scene/error/
      finding layers, click-to-jump) (§42, §81)
      `view/timeline.rs`. Five layers, click-to-jump resolving to a `JumpTarget`
      naming the screen, time, frame, stream, finding and evidence.
      **An unplaced observation is not drawn.** Spec §31 established that some
      findings have no media position and that parking them at `00:00:00` would
      be a fabricated timecode; a drawn strip has the same temptation. Unplaced
      entries are listed separately with a count, and clicking one returns
      `JumpScreen::Nowhere` with an explanation rather than jumping somewhere
      invented.
      **Empty rows are omitted, not drawn blank.** A blank row means "looked,
      found nothing" and an absent one means "never asked" — the same
      conflation the engine refuses everywhere else.
      Placement (`measured` vs `inferred`) travels on every marker so the
      renderer can draw them differently, and `measured_marks` is reported
      separately from the total.
      **Known gap, stated:** reopening a case shows an empty ERRORS row. The
      engine's live timeline is not persisted — the database stores findings, and
      a finding's position is part of its canonical JSON — so structural-damage
      and timestamp anomalies from the original run cannot be recovered. The
      frontend prints both marker counts so an empty row is not mistaken for a
      clean one. Persisting the anomaly list is the fix and is not done.
- [x] Implement media viewer: frame stepping, PTS/DTS display, zoom, pixel
      inspector, histogram, waveform, A/B compare (§43–44)
      `view/viewer.rs`.
      **The pixel inspector names its conversions** (§44). `PixelSample`
      carries `RgbBasis` and the Y'CbCr note states which matrix produced the
      RGB and that the chroma figures are 128 by construction for a greyscale
      frame — a value that is arithmetically valid and completely
      uninformative, and which without that warning would read as "this content
      is colour-neutral".
      **PTS and DTS are separate fields** because they disagree, and that
      disagreement is often the finding; an absent DTS is `None`, never zero.
      Declared tick counts travel verbatim beside the converted timecodes so a
      reviewer can check one against the other.
      Frame stepping saturates rather than wrapping — wrapping would silently
      move an analyst's reference point while they looked away.
      **One bug this work found and fixed:** `next_second`'s threshold constant
      was named `SECOND_EPSILON_US` and held half a frame, so "step one second"
      advanced a single frame. A unit test asserting the elapsed *time* caught
      it. The constant is now `ONE_SECOND_US` and says what it is.
- [x] Implement side-by-side comparison view (§82)
      `view/comparison.rs`, built on the engine's `-model::comparison`.
      **`NotComparable` is carried all the way to the screen** and is never
      styled as agreement — spec §38's central distinction. A comparison
      showing "no differences" also prints how many properties were uncomparable,
      because those are very different claims.
      `agrees_on_everything_measured` requires that something *was* measured: an
      empty comparison has nothing to agree about.
      There is no similarity score, for the same reason the dashboard has no
      authenticity score.
- [x] Implement batch results dashboard (§83)
      `view/batch.rs`. PASS / WARN / FAIL with severity and finding-count sorts.
      **The status comes from the engine's own
      `ValidationResult::from_findings`**, the same function `validate` uses, so
      a delivery decision made in the GUI cannot contradict the printed report.
      A test asserts the two agree across six severity combinations.
      **A file that could not be read gets its own `UNREADABLE` status** rather
      than being folded into `FAIL`, and it blocks delivery: "we could not look
      at it" is not "it was fine".
      Every ordering breaks ties by name so two runs over an unchanged folder
      render identically (§77).
#### Second pass: making the shell actually run an analysis
- **The first version of this shell could not analyse anything.** Every screen
  was modelled and rendered, but there was no command that started an analysis, so
  the Overview, Streams, Timeline, Findings and Evidence screens had nothing to
  show no matter what an analyst did. That is the defect this pass fixes, and it
  is worth recording because *green tests did not catch it*: 132 passing unit
  tests covered every screen model and none of them required a run to exist
- **Progress is emitted, not polled, and the bar is never smoothed.** The engine's
  own `Progress::fraction` travels with each event rather than being recomputed in
  the frontend. A second derivation would be free to disagree about where the four
  concurrent stages (spec §56) begin and end, which is how a progress bar starts
  going backwards. The CSS carries no `transition` for the same reason: an eased
  bar implies a prediction the engine explicitly refuses to make
- **Every event carries its run id.** Two analyses in flight would otherwise
  paint each other's progress onto the same bar. The id is allocated *before* the
  reporter closure is built, because a closure cannot read state it does not yet
  have — and `register_job` asserts the two allocations agree
- **The fingerprint comes from the engine that ran the analysis.** The job and
  its engine are held together in the state for exactly this reason: the
  fingerprint is computed from the rule set that produced the findings, and
  having a *different* engine vouch for them would defeat the point of recording
  it (spec §63)
- **Two more shell-local wire types.** `StageTag` and the earlier `SearchScopeView`
  are copies of engine enums that have no `serde` derive. Adding one to the engine
  to satisfy a UI would put a presentation concern into the analysis engine; the
  mappings are total in both directions and a test asserts every stage round-trips
- **159 tests**, up from 145: 141 unit, 5 end-to-end analysis, 13 corrupt-media.
  The end-to-end file asserts what only a real run can — that progress arrives and
  never moves backwards, that the finding count the UI reports equals the count in
  the case database, and that a cancelled run writes nothing
#### Third pass: the last screens, and a bug the worst-case test found
- **Metadata, Comparisons and Reports are plumbed**, leaving only Video and Audio.
  All thirteen commands now exist and every one the frontend calls resolves —
  checked mechanically rather than by reading both sides, because a renamed
  command fails silently as a screen that never loads
- **The comparison screen prints the tolerance with every result.** Two runs of
  the same file differ in the last bits of a float, so loudness and scene counts
  are compared against a tolerance rather than for equality. "Within tolerance"
  with no tolerance shown is a claim the reader cannot check, so `ToleranceRow`
  carries it
- **A bug the test suite found, and it was the worst kind.** The first verdict
  logic checked `!difference.is_equal()`, which is true for `NotComparable` as
  well as `Different`. Two files that agreed on everything *and* declared nothing
  comparable — no colour, no scene data, no audio, no Tier-2 run — were therefore
  reported **DIFFERENT**. A reviewer told two identical files differ, with a table
  of axes behind it, and every row in that table actually said "not measured".
  The common case is what made it invisible: most codecs in a real intake never
  reach Tier-2, so `scene.changes` is `NotComparable` on nearly every comparison
- **The fix counts *every* source of "could not measure"** — per-field rows, the
  stream layout, metadata, scene, silence and the tolerance axes. The first
  version read `measured_differences()` alone, which is a list of field
  comparisons and excludes all of those, so a file whose container could not be
  read produced no rows at all, counted zero uncomparables, and read as
  EQUIVALENT. Two regression tests now pin both directions: an uncomparable axis
  must not read as a difference, and must not read as agreement either
- **A file compared against itself now never reports a difference**, asserted
  across the whole corrupt-media corpus. If the engine's own determinism could
  fail, every comparison in the GUI would be suspect and a reviewer would have no
  way to tell engine noise from a real finding
- **Two tests were corrected rather than fixed.** One asserted that axes nobody
  declared would be *absent* from the comparison; `compare_streams` emits a
  `NotComparable` row for each, which is correct — dropping them would hide that
  colour was never comparable, which is what a reviewer checking a suspicious
  grade needs to see. Another asserted two identical files were EQUIVALENT; they
  are INCOMPLETE, because nothing established agreement on the axes neither file
  declared
- **167 tests**: 147 unit, 5 end-to-end analysis, 15 corrupt-media
#### Fourth pass: the last two screens, and what is still missing
- **Video and Audio are plumbed.** All twelve screens now display real engine
  data. The Video screen's frame table carries PTS and DTS side by side, because
  reordering is invisible if you show only presentation time
- **The decoder refusal is worded as a policy, not a fault.** This build does not
  decode H.264, HEVC or AAC because they are patent-encumbered. Saying "could
  not decode" would send an analyst looking for damage in a file that is
  perfectly intact, so the screen says "this build does not decode `avc1` ...
  the file is not damaged"
- **A file with an undecodable codec still gets a full screen.** Its frame
  *table* is readable whatever the codec, and it is precisely what someone
  examines when a file will not play — refusing the screen would hide the timing
  that explains the problem
- **A small frame opens at 1:1 rather than "fit".** At 320x240, "fit" would
  upscale to fill a 1440-wide window and present interpolation as if it were
  detail, and reading individual pixels is the entire point of a pixel inspector
- **The waveform is scaled by the signal's own peak**, not by an assumed full
  scale. Normalising to 1.0 would render a track with 40 dB of dynamic range as
  uniformly full — the difference between "this file is quiet" and "this file is
  fine" to anyone QC-ing it
- **The frame table is bounded at 500 rows and says so.** A feature-length master
  would otherwise put 100,000 rows in the DOM. A truncated list that does not say
  it is truncated reads as the whole thing
- **The pixel inspector, histogram and frame A/B still have no pixels.** Their
  view models are complete and tested — including the colour-conversion labelling
  spec §44 demands — but displaying them needs decoded frames retained for the
  viewer's lifetime. That is an engine change (retention plus a bounded window),
  not a command, and it is the one real gap left in the UI
- **179 tests**: 159 unit, 5 end-to-end analysis, 15 corrupt-media
#### Fifth pass: the pixel viewer, and a claim I had not checked
- **The viewer decodes and displays frames.** The pixel inspector, luma histogram
  and frame A/B comparison are all live, with the colour-conversion labelling
  spec §44 requires
- **A correction, and the reason it is written down.** Two passes ago I reported
  this as requiring "an engine change: retention plus a bounded window". I had not
  checked. Every API it needs — `DecodeSession::decode_prefix`,
  `read_samples_file`, `DecodedFrame::to_greyscale` — was already public in
  `-video` and `-container`. It needed a command that called what existed, which
  is a day's work rather than an engine redesign. Assuming a boundary and
  reporting the assumption as a finding is the same failure mode as duplicating
  a rule and calling it a feature, and `todo.md` already records three instances
  of exactly that
- **The evidence is a real encoded stream.** `tests/frame_decode.rs` builds
  genuine AV1 with the foundation's rav1e-backed encoder, wraps it in a real WebM
  container, reads it back through the demuxer, decodes to pixels, and asserts the
  pixel length, the histogram's bin total, that two frames three apart differ,
  and that the same frame decoded twice is byte-identical. A test fed synthetic
  frames would have proved nothing about the path a file actually takes
- **Decoding is per click, not retained.** A 4K master is tens of thousands of
  frames at ~25 MB of RGB each. Retaining them exhausts memory before anyone
  reaches the end, and a viewer that loads everything first looks like a hung
  application. Stepping backward re-decodes
- **The canvas uses `image-rendering: pixelated`.** At 4x the browser would
  otherwise smoothly interpolate, and a reviewer would read the interpolation as
  detail in the frame rather than as an artefact of the display
- **The decoded frame is labelled greyscale.** `DecodedFrame` retains only the
  luma plane, so Cb and Cr are 128 by construction. The inspector says so rather
  than presenting neutral chroma as something that was measured
- **A corrupt AV1 stream loses frames without crashing**, asserted on the
  viewer's own path. The decoder resynchronises at the next keyframe, so a damaged
  frame is lost — and saying so is correct. Producing *a* frame from the wrong
  position would not be
- **193 tests**: 163 unit, 6 end-to-end analysis, 9 frame decode, 15 corrupt-media
#### Sixth pass: the file nobody had parsed
- **`ui/screens.js` did not parse.** Two orphaned lines from a line-range edit
  that removed a duplicated helper, surviving four builds and 188 passing
  Rust tests. The application would have opened to a blank window
- **Nothing could have caught it.** The frontend has no build step by design, so
  `tauri_build` embeds `ui/` without parsing it; the Rust tests never load it;
  and `cargo build` produced a working executable containing a file that does
  not parse. The only way to reach the failure was to launch the app and look
- **`ui/check.mjs` parses every module and cross-checks every `invoke` against
  the Rust command list**, and `build.rs` runs it. Both halves were verified by
  deliberately breaking the frontend
- Fatal when Node is present, a warning when it is not — the no-bundler frontend
  means a developer without Node must still be able to build
#### Seventh pass: the run id was allocated twice
- **`next_run_id()` incremented and returned the counter; `register_job()`
  incremented it again.** The id stamped on progress events and the id the run
  was registered under disagreed, so `debug_assert_eq!` panicked - meaning
  pressing "Analyse" crashed the app in a development build
- **Five integration tests passed throughout**, because `run_to_completion`
  called `register_job` directly and never performed the two-step sequence the
  real command uses. The tests covered a path no caller takes
- Fixed by having `register_job` accept the reserved id; the helper now mirrors
  `analyse` exactly. Reintroducing the bug fails five tests
#### Eighth pass: nothing had ever run the screens
- **Three screens threw a `ReferenceError` on first click.** `renderViewer`,
  `renderComparisons` and `renderReports` all used `helpers` / `el` / `guarded`
  without declaring the argument `renderScreen` passes. "Compare" and "Generate
  report bundle" were both broken, and every static check was green
- **`guarded` returns `null` for both "failed" and "answered null"**, and
  `close_case` (returns unit) and `poll_analysis` ("still running") are both the
  latter. The close button therefore did nothing at all, and a failing poll
  retried forever. `attempt` returns `{ ok, value }` for those two call sites
- **`ui/check.mjs` now renders all twelve screens against a stub DOM and presses
  every button** — found all three, verified by reintroducing each
- **The check was briefly vacuous**: it passed the Rust screen name where the
  serde key belongs, so all twelve rendered the "no renderer yet" placeholder and
  passed while testing nothing. Fixed, and view models are now a permissive
  stand-in rather than hand-written fixtures that drift from the Rust structs
- [x] Escape untrusted filenames and metadata before they reach `innerHTML` —
      `ui/dom.js` escapes by construction and `ui/check.mjs` fails on any
      untagged interpolation, including multi-line continuations
- [x] Verify in a real Tauri webview — `TPT_STARTUP_LOG` plus a page-load probe;
      it is what found the `close-case` startup crash and the unresolvable bare
      imports, neither of which any static check could see
- [x] Timeline click-to-jump, viewer zoom, and frame A/B pixel difference
- [x] Asset selection: rows are clickable, keyboard-reachable, and re-render
      immediately on selection
- [x] Build the batch workflow. `view/batch.rs` is modelled and unit-tested, and
      `start_batch`/`poll_batch` call the engine's own `core::batch::run` rather
      than re-walking the directory. It is a panel on the Case screen, not a
      thirteenth nav entry, because spec §79 fixes the screen list at twelve
- [x] Persist the engine's timeline. Schema v4 adds `timeline_entries`; the
      pipeline writes the strip with its run and `timeline` reads it back, so the
      video, audio, scene and error layers are no longer structurally empty.
      `Placement::Unplaced` is stored as itself, never as time zero
- [x] Decide whether `startup_log` should ship in release builds. It ships.
      The failure it detects is *most* likely in a release build — that is the
      one nobody is watching and the one with no other evidence — so removing it
      would remove the diagnostic from exactly the build that needs it. The real
      hazard is not the file, it is that this is a **write primitive reachable
      from the webview**, and the webview renders strings taken from the file
      under examination (asset names, container brands, codec strings, atom
      values — attacker-chosen by definition). Nothing routed case data into it
      and *nothing enforced that*; with no bundler the next edit is free to pass
      an error message through. So every control character is flattened and the
      line is bounded: one IPC call is one line, always. A crafted name with a
      newline can no longer write an entry the application never composed into
      the one file a build script treats as ground truth. Six tests, each
      verified to fail when the flattening is removed
- [x] The pre-existing `acquisition_is_deterministic` test is flaky because full
      records carry second-granular filesystem timestamps. Decided: **timestamps
      are not part of the determinism claim.** `acquire` *opens the file*, which
      is what updates the access time on any filesystem recording one, and the
      value is whole seconds — so two acquisitions either side of a second
      boundary disagree although nothing about the file did. The access time is
      an observation *about the act of looking*, not a property of the evidence.
      The claim now covers what the engine derived: digests, size, modification
      and creation times, filesystem context. A companion test
      (`the_access_time_is_the_one_field_acquisition_can_change_by_existing`)
      pins the carve-out so a later tidy-up cannot quietly reinstate it. 25
      consecutive runs, no flake
- [x] Decide what to do about cases created before schema v4. They now say so.
      The schema could not answer it: after migration a legacy case's
      `user_version` is current, so "predates timeline retention" and "this run
      found nothing" were indistinguishable inside the store. Comparing a run's
      date against v4's release date would *guess*, and the guess produces the
      more dangerous of the two answers — an empty strip reading as an absence of
      findings. Schema v5 adds `analyses.writer_schema_version`, written by the
      only party that knows the fact, `NULL` on rows a pre-v5 build wrote (the
      same reading a missing `findings.payload` already carries, spec §65).
      `TimelineRetention` = `Complete` / `Partial { unrecorded_runs,
      total_runs }` / `NoRuns`. The wording is produced in the view model, not
      the renderer: an empty strip and an unrecorded one draw identically, so a
      sentence assembled in a renderer is a sentence no Rust test can assert.
      Five tests, two verified to fail when the query stops detecting legacy rows
- [x] Verify: malformed/corrupt media cannot crash the app
      `tests/corrupt_media.rs`, 13 tests, and it found a real crash.
      **Three overflow bugs**, all the same shape: `checked_mul` guarded
      `width * height` and the following `* 3` for bytes-per-pixel was
      unchecked. On 64-bit, `u32::MAX * u32::MAX` passes the guard and the `* 3`
      wraps, so a frame declaring 65536-square dimensions was measured as three
      bytes long and the inspector went on to index a three-byte buffer. Fixed by
      `rgb_byte_len()`, which checks both steps, with a regression test.
      The corpus is 11 hostile fixtures (empty, one byte, oversized box, lying
      `moov`, truncated `ftyp`, pure noise, an EBML header with no body, a text
      file named `.mov`) pushed through the engine *and* every screen model. A
      panic anywhere fails the test.
      Also covered: decoded frames whose dimensions disagree with their buffer,
      a frame list with unsorted and non-monotonic indices, zero and absurd
      channel counts, NaN/infinite audio samples, and `i64::MIN`/`i64::MAX`
      timestamps.
      **One test was rewritten rather than fixed.** It originally asserted that a
      file with no recognisable structure produces *findings*. It does not, and
      should not: with no `ftyp` the engine does not know what the file claims to
      be, so no rule can decide whether anything about it is wrong. It produces
      limitations instead, which is what §75 actually asks for. A separate
      test pins the contrast — a file the engine *can* identify but cannot finish
      reading does produce findings about the damage.
- [x] Verify: analysis is reproducible from recorded profile + software
      version (analysis fingerprint, §63)
      `AnalysisEngine::analysis_fingerprint` derives from asset SHA-256 +
      analysis version + profile + rule set, and is embedded in every report
      format (`Methodology` has no `Default`, so it cannot be omitted).
- [x] Complete a full real-world professional workflow end-to-end (spec 96)
      **The last unclosed item of the definition of done, and the last thing
      nothing could reach.** Every other line in that list had a test
      somewhere. This one needs a whole case, taken all the way through, with
      each artefact checked as it is produced. The gap was not the absence of
      tests but the absence of a step: `validate` could only judge a case
      by finding severity, so a delivery could be judged but never checked
      against a specification. `crates/tpt-app-media-forensics-cli/tests/
      professional_workflow.rs` now runs the nine steps a QC engineer
      performs on an incoming delivery -- author the profile and check it
      loads; QC the file before intake; acquire into a case; analyse with the
      full engine; record an analyst note; validate the case against both the
      profile and the findings; render the evidence bundle; confirm the source
      is byte-identical afterwards; and confirm two renders are
      byte-identical (spec 77)
      **Driven through the real binary, not the library.** Argument parsing,
      exit codes and the read-only guarantee are all part of what
      'completed successfully' means, and none of them live in the library
      API. The test asserts the bundle manifest verifies against the files on
      disk, which is the property that makes it self-checking rather than
      merely complete, and that the analyst note and the profile version both
      reach the rendered HTML
      **The verdict is the worse of the two halves.** A delivery can meet its
      specification and still carry a significant finding, and a clean analysis
      does not make an under-specified delivery acceptable. The test derives the
      expected verdict from whichever half failed and asserts the command agrees,
      so a requirement table reading all-MET beside a severity FAIL cannot pass
## Phase 2 (spec §85)
- [ ] Advanced codec internals
- [ ] Richer container analysis
- [ ] MXF/broadcast workflow support
- [ ] Archive validation profile + batch validation (§48)
- [ ] Watch folder automation (§50)
- [x] Delivery validation profiles (pass/fail/warn) (spec 68)
      **The verdict machinery existed and nothing could produce a verdict.**
      `ValidationResult` with its PASS / PASS WITH WARNINGS / FAIL labels,
      `ValidationResult::from_findings`, and `Severity::fails_validation` were all
      implemented, documented, and unit-tested. Nothing called any of them:
      `Report.validation` was hardcoded `None` at both production construction
      sites, so the HTML header result banner could never render, and `pdf.rs`
      branched on a value that was always absent.
      **Two definitions of the same rule, free to drift.** `from_findings`
      inlined the severity list `matches!(Critical | Significant)` while
      `fails_validation` existed alongside it stating the same thing. A change to
      the predicate would have missed the function that actually produces
      verdicts, leaving the answer unchanged while the rule it names looked
      changed. `from_findings` now delegates to `fails_validation`.
      **The requirement half is now built.** §68's actual content is a *delivery
      profile*: declared requirements for codec, resolution, frame rate, channels,
      sample rate, and container format, checked against the file.
      `model::delivery` holds the domain types and `-rules::delivery` the checker,
      which takes a `ContainerInspection` rather than a path so it stays a pure
      function of measured properties and is testable with no media present
      **Three states, not two, and the third blocks delivery.** `MET | NOT MET |
      NOT MEASURED`. A WebM checked against `video.resolution` reports NOT
      MEASURED because this build's Matroska reader exposes no picture geometry;
      folding that into `Met` would let a profile print PASS for a file it never
      checked, which is the precise failure this product exists to prevent
      **A prerequisite nobody had recorded: MP4 declared no audio at all.**
      `convert_track` hardcoded `audio: None` on every MP4 track and the demuxer
      exposes no channel count, sample size, or sample rate, so the requirement
      checking half of §68's own example -- `audio.channels: 2`,
      `audio.sample_rate: 48000` -- was unbuildable against the format this engine
      reads most. `container/src/audio_sample_entry.rs` reads the `mp4a` entry
      from the specification's offsets, which differ from the `VisualSampleEntry`
      ones: read at the visual offsets it would take a channel count out of the
      middle of a reserved run and report a plausible wrong answer. The fixture
      wrote a `VisualSampleEntry` for audio tracks too, so it could not have
      tested the fix even once the reader existed
      **Tolerances are mandatory and printed.** A frame rate states its own
      tolerance, and a profile without one is rejected rather than defaulted -- an
      omitted field is exactly what a hand-written specification gets wrong.
      `Expected: 25 (+/- 0.5)`, because `Expected: 25` beside a check that allowed
      24.5 is a claim the report cannot support
      `validate <file> --profile <p>` is now spec §95's invocation, alongside
      `--case-dir`. `--json` carries the profile version, fingerprint, and every
      requirement's expected/observed/outcome. Exit 2 on FAIL
      **An honest limitation, stated rather than faked.** `container.format: mov`
      matches any ISO-BMFF file: this build detects the family from the `ftyp`
      signature and does not distinguish an Apple `qt  ` brand from `isom`. Closing
      that means reading the major brand in `-container`
- [x] Custom user-defined profiles (spec 69) with profile versioning (spec 70)
      **A profile is data, and nothing could hold any.** `RuleProfile` was a
      hardcoded struct of tolerances with no file format, no loader, and no way for
      a customer to express a delivery specification at all. `profile template |
      check | show` and a JSON format now carry one, and `validate --profile`
      reads it
      **The template must parse, and that is asserted rather than assumed.**
      `profile template` parses the string it is about to write, and a test runs
      the emitted file back through `profile check`. A template that does not load
      turns the most likely first use of `profile` into an error message
      **The template refuses to overwrite.** A profile is maintained across versions
      (spec 70); overwriting one because someone asked for a template would destroy
      the record of what the previous version actually required, and a delivery
      judged against it could no longer be explained
      **Version and fingerprint are both recorded, and they can disagree -- on
      purpose.** The fingerprint is derived from the requirements themselves, so a
      requirement edited without a version bump produces a different fingerprint
      beside the same version number. That disagreement is what makes "never
      silently change an existing profile" checkable rather than merely stated
      **An unknown requirement kind is an error.** A typo silently dropped would
      be a delivery that passes without ever having been checked against that
      requirement. Four tests in `-model` cover the format: round-trip, spec-shaped
      JSON, the rejected missing tolerance, and the rejected unknown kind

      JSON rather than the YAML the spec illustrates. `serde_json` is already a
      dependency of every crate here, and a parser added for one config file would
      be the only dependency in this project not pinned to a revision -- which is
      the trade spec §63 and §77 exist to prevent. The structure is identical
      either way. Stated here as a deliberate deviation from the spec's
      presentation
- [x] Rule explainability output (what/why/observed/limitations) (§71)
      **The prose existed; nothing read it.** Every rule implements
      `what_it_checks` and `why_it_matters` as mandatory trait methods, and
      `every_rule_explains_what_it_checks_and_why_it_matters` asserted they were
      non-empty. But those two methods were called from that one test file and
      nowhere else in the codebase. A report gave a reviewer the observation, the
      measurements, and a hardcoded one-line disclaimer — and no statement of what
      the rule checked or why the condition mattered. Three of spec §71's four
      required parts were rendered; the other two were defined and discarded.
      This is the *third* instance of the same defect: `measure_audio`, then
      `av_sync::analyse`, then the whole comparison engine. Each time the code
      was healthy and the caller was missing, and each time `stage_guard.rs` was
      structurally unable to see it — the guard checks `BundleInput` population,
      and a trait method that produces no bundle input is invisible to it.
      **Fixed by stamping the rationale onto the finding.** `Finding.rationale`
      carries `checks`, `why_it_matters`, and `does_not_establish`, populated in
      `RuleEngine::evaluate` where the rule is in hand. On the finding rather than
      looked up at render time, because a report is read away from this binary and
      an explanation that only existed inside the engine could not travel into the
      PDF a reviewer receives six months later.
      `ForensicRule::does_not_establish` is new and defaults to the report-wide
      disclaimer. A missing caveat reads as *no* caveat — an absence the reader
      resolves in the finding's favour — so the default is boilerplate rather than
      nothing, and `has_specific_limit()` lets a render say "the rule adds this"
      only when it actually does.
      `new_rules.rs` gains an `evaluate` helper that goes through
      `RuleEngine::evaluate` rather than calling `rule.evaluate` directly, because
      the direct path skips the stamping and could not detect its absence. Two new
      tests pin it: every finding carries non-empty rationale, and it is its *own*
      rule's text rather than a neighbour's. Both were verified to fail with the
      stamping removed.
- [x] Review workflow for findings (new/reviewed/accepted/rejected) (§66)
      `Store::record_review` and `Store::reviews_of`; `FindingStatus` covers
      `new`/`reviewed`/`accepted`/`rejected`/`requires-investigation`.
      Reviews append and never overwrite the observation, so the engine's own
      wording survives a reviewer's verdict, and repeated verdicts accumulate.
      **Fixed a bug that made this unusable**: `insert_finding` omitted
      `analysis_id` from its `finding_reviews` insert, so every reviewed finding
      was rejected by the database. `insert_finding` had no test coverage at all,
      which is how a fully broken path stayed green — now pinned by round-trip
      tests for each verdict state.
- [x] Analyst notes on asset/stream/timestamp/frame/finding/evidence (§65)
      `Store::add_note` / `notes_on` / `notes_in_case`. A note names both the kind
      and the id of its subject, or neither — half a subject is refused rather
      than stored, because it would let a later reader attach the analyst's
      conclusion to the wrong thing. Case-level notes (`None`/`None`) are
      allowed and included in the case listing.
      Bodies are stored byte for byte, including line endings and trailing
      whitespace: a note is evidence of what the analyst concluded, so reflowing
      their prose would alter the record.
      The `notes` table existed since the base schema with no API above it — the
      feature was unreachable. Notes now also reach the report: `Report` carries
      `notes`, and the HTML and PDF renderers show them. The report schema
      version moved to a `REPORT_SCHEMA_VERSION` constant, bumped to 2. The write
      path is closed too: a `note` CLI subcommand records a note from a `--body`
      argument or stdin, with optional `--subject-kind`/`--subject` (which must be
      given together). **Fixed along the way:** `CaseDirectory::create` never
      wrote a `cases` row — only `analyze` did — so an acquired-but-never-analysed
      case reported no case id and every read path treated it as empty. Not done:
      the CSV renderers omit notes (they are per-finding tables).
- [ ] Comparison against a defined reference/master asset (§67)
- [ ] More advanced audio measurements (loudness standards, spectral) (§21–22)
- [ ] More advanced visual anomaly detection
- [ ] Richer comparison tooling (blink/overlay/difference views) (§39)
- [ ] Colour/HDR deep inspection (HDR10, mastering/CLL metadata) (§45–46)
- [ ] Subtitle/data stream analysis (§47)

## Phase 3 (spec §86)
- [ ] Chain-of-custody audit mode (append-only event log) (§64)
- [ ] Hardware acceleration
- [ ] Large-scale batch processing
- [ ] Plugin rule system
- [ ] Enterprise profile management
- [ ] Automated ingestion
- [ ] Advanced media provenance analysis
- [ ] Media fingerprint for similarity comparison (§93)
- [ ] Media lineage tracking (camera master → delivery/streaming) (§94)
- [ ] Executable delivery specification (`validate` command with PASS/FAIL) (§95)

## Testing & Quality (spec §75–77, ongoing across phases)
- [x] Unit tests for parsers, timing, hashing, rule evaluation, tolerances
      579 passing across 12 crates. Rule evaluation is checked in both
      directions - each rule trips on a fixture built to trigger it *and* stays
      silent on a clean one, so a rule that fires on everything is caught.
      **No known gaps.** All 27 rules fire end to end from a file this project
      owns, so `NO_END_TO_END_FIXTURE` in `stage_guard.rs` is empty. The list and
      the mechanism that maintains it are kept: the guard fails if a new rule
      cannot fire, *and* if a listed one starts firing.

      The list began at fifteen. Reaching zero was not fifteen fixtures - it was
      three different problems wearing the same label, and telling them apart was
      the actual work:

      - **Ten** were builders that already existed and had never been written to
        disk, or signals the corpus's existing Opus path could already produce.
        No new analysis was needed.
      - **Two** (`CONTAINER.DECLARED_TRACK_MISMATCH`, `TIMING.NON_MONOTONIC_PTS`)
        could not fire on *any* file, which is a materially different claim from
        "not yet exercised". Both sides of each comparison came from the same
        source, or one side was never computed: `declared_track_count` came from
        the demuxer's own track list and so equalled `streams.len()` by
        construction, and frame timestamps were decode time, which `stts` builds
        from unsigned deltas and is therefore monotonic by construction. Both
        needed a *reader* - `mvhd`, then `ctts`.
      - **Three** needed fixtures reaching a *different* condition from the one
        already present, not a louder instance of it: `truncated.mp4` damages
        bytes, while `CONTAINER.STRUCTURAL_DEFECT` needs a defect that is not
        missing data.

      An entry added back to that list should name which of the three it is.
      Conflating "no fixture yet" with "no fixture could exist" is how a rule stays
      unfireable while looking merely untested.
- [~] Property tests for timestamps, frame ordering, container parsing
      **Container parsing now has real properties.**
      `-container/tests/properties.rs`: 6 properties, 512 cases each, on
      `next_box`, `parse_track_colour`, `parse_edit_lists`, and `scan_isobmff`.
      The invariants are stronger than any example set — no panic, truncation is
      monotone (a prefix never reveals more than the whole file), no reader
      exceeds the `trak` count, damage offsets stay inside the input, absurd box
      sizes are refused. Verified these actually execute: an injected panic in the
      property body was caught, so the harness is not vacuous.
      The harness caught a bug in the test itself — a loop that never advanced
      `cursor` and so only ever checked offset 0.
      **Still not started:** timestamps and frame ordering. Those need generators
      for presentation/decode sequences, which is a different piece of work from
      parsing bytes.
- [ ] Fuzzing for container/codec/metadata/packet/timestamp parsers
- [ ] Golden tests against known fixtures (metadata, structure, findings)
- [~] Build corrupt-media test corpus (§76): synthetic generator in place;
      fixtures generated on demand. **Truncated is now built and analysed** —
      `stage_guard.rs` truncates a real MP4 to half length, which is what
      populates `BundleInput::Damage`; without it the two damage rules are
      registered, documented, and unfireable.
      Pending: ~~bad-header~~, invalid-timestamps, missing-index,
      ~~bad-audio-packet~~, ~~duplicate-frame~~, ~~duration-mismatch~~,
      ~~metadata-conflict~~
      **Re-audited by measurement rather than by reading the list.** Three of
      these were already covered: `repeated-frames.mp4` is the duplicate-frame
      case, `wrong-duration.mp4` the duration mismatch, and `metadata.mp4`
      carries the metadata conflict. All 29 rules already fired end to end, and
      `NO_END_TO_END_FIXTURE` was empty — so the real gap was never
      unfireability.
      It was **variant coverage**. A rule can be reachable and green while one
      of the enum variants it matches on has never been produced by a real file,
      and nothing fails: `CONTAINER.STRUCTURAL_DEFECT` fired from
      `trailing-data.mp4` while two of its four variants had never run.
- [x] The four unreachable damage/timing variants (§76)
      `impossible_box_size`, `non_printable_box_type`, `empty_sample` and
      `NegativeTimestamp` — plus `Overlap`, which the new guard found once the
      first four were closed. Builders: `build_mp4_with_impossible_box_size`,
      `build_mp4_with_nonprintable_box_type`, `build_webm_with_empty_block`,
      `build_mp4_with_negative_presentation_times` and
      `build_mp4_with_overlapping_presentation_times`. Damage tags are now 4/4
      and packet tags 2/2.
      `empty_sample` needed a **Matroska** fixture, verified empirically rather
      than assumed: `read_samples` stops at the first zero-byte packet by design,
      so no ISO-BMFF file can express it, while a zero-length WebM block parses
      and is recovered normally. The fixture is the evidence for that asymmetry.
      `Overlap` looked impossible at first — `stts` builds decode times from
      *unsigned* deltas, so they can never repeat — and is only reachable
      because `ctts` composition offsets are signed.
- [x] A guard so the variant gap cannot silently return (§76)
      `every_damage_and_timing_variant_is_reached_by_some_fixture` in
      `stage_guard.rs`, the gap the rule-level guard above cannot see. `ALL_VARIANTS`
      is written out rather than derived from the enums, so adding a variant
      fails the guard on the day it is added rather than passing vacuously.
      Verified non-vacuous by inserting a fake tag and watching it fail.
      `UNREACHED_VARIANTS` holds one entry, `NonMonotonicDts`, recorded as **"no
      fixture could exist"** because the pipeline calls `scan_presentation` and
      never `scan_decode` — no file of any kind reaches it. That is an
      unwired *analysis*, not a corpus gap, and saying so is what distinguishes
      the two.
- [x] Determinism checks (stable rule ordering, no unseeded randomness) (§77)
      Checked at three levels: `results_are_ordered_deterministically` (rule
      output order), `hash_is_deterministic_across_invocations` and
      `report_is_deterministic_for_the_same_case` ("two renders of one case
      must be byte-identical"), plus reproducible-findings tests for audio,
      A/V and WebM. Identifiers are content-derived rather than random, which
      is what makes byte-identical output possible across machines.
- [ ] Benchmark against representative large media files (§57)

## Commercial & Release (spec §87–89, §96–97)
- [ ] Finalize dual MIT/Apache-2.0 licensing (LICENSE-MIT, LICENSE-APACHE,
      TPT Solutions copyright) across all crates
- [ ] Define product tiers (Core / Professional / Enterprise) (§87)
- [ ] Define pricing (§88)
- [ ] Package Windows installer
- [ ] Prepare Gumroad package: installer, license key, quick-start guide,
      example media corpus, example case, example reports, docs,
      changelog (§89)
- [ ] Produce example forensic cases and reports
- [ ] Beta test with media professionals
- [ ] Refine feature set from customer feedback
