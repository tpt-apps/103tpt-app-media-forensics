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
- [~] Implement corruption detection with graceful continuation (§30)
      **Structural damage detection is in**, in `-container/src/damage.rs`. A
      shallow independent walk of the top-level box list, separate from the
      demuxer, because the demuxer reports success for everything before the
      bytes stopped making sense and so has by construction lost the boundary.
      Four typed defects: truncation, trailing data, impossible box size, and a
      non-printable box type (the signature of a reader that has lost sync).
      Two rules grade them, deliberately at different severities —
      `CONTAINER.TRUNCATED_MEDIA` is Critical/High because declared media is
      missing, `CONTAINER.STRUCTURAL_DEFECT` is Warning/Medium because appended
      data does not mean content is absent.
      **Not yet done:** decode-level and packet-level corruption (a valid
      container holding undecodable samples), and graceful continuation *within*
      Tier-2 decoding rather than around the container.
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
- [x] Implement analysis cache keyed on asset hash + analysis version +
      profile hash + rule-set hash (§54)
- [~] Implement large-file/streaming analysis with bounded memory,
      background workers, cancellation, progress reporting (§55–56)
      **Bounded memory is done** — acquisition and the container paths read
      through fixed-size buffers with no whole-file allocation, and acquisition
      detects a source changing size mid-read rather than recording a hash of a
      moving target.
      **Progress and cancellation are now done.** `-core/src/progress.rs`:
      `Stage` (8 stages, one ordered list so ordinals cannot drift),
      `Progress::Started`/`Finished` with an optional completed/total pair,
      `Cancellation` (an `Arc<AtomicBool>` that clones share rather than copy),
      and `ProgressTracker` which reports *and* checks at each boundary.
      `AnalysisEngine::analyse_with` is the new entry point; `analyse` delegates
      to it with a silent tracker, so the common path costs nothing and the two
      cannot diverge.
      **Not done: background workers.** The engine is synchronous by design and
      nothing spawns a runtime — adding async would be a large change for one
      feature. Callers wanting a UI can run `analyse_with` on their own thread.
      Cancellation is therefore *cooperative and coarse*: a cancel request during
      a 90-second decode is observed when that decode returns, because the
      decoder has no cancellation hook. That limitation is documented on the
      module and on `Stage`, and `CoreError::Cancelled` is its own variant so a
      caller can retry without treating it as a corrupt asset. A cancelled run
      writes nothing to the case database — a partial analysis record that looked
      complete would be worse than none.
      Progress deliberately reports no ETA: an estimate on a feature-length master
      would be wrong by minutes and reads as a promise the engine cannot make.
      Fractional progress appears only where a total is genuinely known.
- [ ] Implement file-to-file comparison engine (duration, streams, codec,
      colour, audio, metadata, timestamps, scene structure) (§38–40)
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
- [ ] Build Tauri desktop app shell wrapping the same core engine (§78)
- [ ] Implement core UI screens: Case, Assets, Overview, Streams, Timeline,
      Video, Audio, Metadata, Findings, Comparisons, Evidence, Reports (§79)
- [ ] Implement dashboard (asset/finding/severity counts, status) (§80)
- [ ] Implement timeline UI as central navigation (video/audio/scene/error/
      finding layers, click-to-jump) (§42, §81)
- [ ] Implement media viewer: frame stepping, PTS/DTS display, zoom, pixel
      inspector, histogram, waveform, A/B compare (§43–44)
- [ ] Implement side-by-side comparison view (§82)
- [ ] Implement batch results dashboard (§83)
- [ ] Verify: malformed/corrupt media cannot crash the app
- [x] Verify: analysis is reproducible from recorded profile + software
      version (analysis fingerprint, §63)
      `AnalysisEngine::analysis_fingerprint` derives from asset SHA-256 +
      analysis version + profile + rule set, and is embedded in every report
      format (`Methodology` has no `Default`, so it cannot be omitted).
- [ ] Complete a full real-world professional workflow end-to-end (§96)

## Phase 2 (spec §85)
- [ ] Advanced codec internals
- [ ] Richer container analysis
- [ ] MXF/broadcast workflow support
- [ ] Archive validation profile + batch validation (§48)
- [ ] Watch folder automation (§50)
- [ ] Delivery validation profiles (pass/fail/warn) (§68)
- [ ] Custom user-defined profiles (§69) with profile versioning (§70)
- [ ] Rule explainability output (what/why/observed/limitations) (§71)
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
      version moved to a `REPORT_SCHEMA_VERSION` constant, bumped to 2. Not done:
      no CLI command to write a note, and the CSV renderers omit notes (they are
      per-finding tables).
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
      Pending: bad-header, invalid-timestamps, missing-index,
      bad-audio-packet, duplicate-frame, duration-mismatch, metadata-conflict
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
