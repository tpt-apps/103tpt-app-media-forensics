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
- [~] Implement video analysis: structural, temporal, spatial, colour (§14)
      Structural, temporal and spatial are done (GOP, duplicates, near-duplicates,
      scene changes). **Colour is not:** `ColourInfo` and `is_hdr` exist in
      `-model` and serialise, but every reader assigns `Default::default()` and
      `false`, so the fields are empty for every file. A type that is present and
      never populated is the same silent gap as an unwired stage; the README
      listed colour/HDR as shipped until `readme_claims.rs` caught it.
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
- [ ] Implement spectral analysis (FFT) and loudness-range measurement
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
- [ ] Implement encoder fingerprinting (best-effort, confidence-labelled) (§27)
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
- [~] Implement error/anomaly timeline (§31)
      **Damage now lands on the timeline.** `SampleIndex` in `-container` maps a
      byte offset to the sample containing it, and `CONTAINER.TRUNCATED_MEDIA`
      sets `timeline_start` to that sample's presentation time. Verified on a real
      truncated file: the finding reports `00:00:00.280` and names the stream.
      The placement carries its provenance — offsets are *inferred* by accumulating
      sample sizes, not read from `stco`, and the finding says so. With no sample
      index (file above the sampling bound) the finding carries a byte offset and
      **no** timecode, because a fabricated `00:00:00` would read as a measured
      position.
      **Not yet done:** a unified timeline type merging damage with timestamp
      anomalies and codec errors; the `mdat` anchor walk stops at `moov`, so a file
      with `mdat` before `moov` gets no placement.
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
- [ ] Implement frame extraction as evidence
- [x] Implement analysis cache keyed on asset hash + analysis version +
      profile hash + rule-set hash (§54)
- [~] Implement large-file/streaming analysis with bounded memory,
      background workers, cancellation, progress reporting (§55–56)
      **Bounded memory is done** — acquisition and the container paths read
      through fixed-size buffers with no whole-file allocation, and acquisition
      detects a source changing size mid-read rather than recording a hash of a
      moving target. **Background workers, cancellation and progress reporting
      are not:** `-core` has no progress or cancellation type at all, so
      `docs/architecture.md` no longer claims them.
- [ ] Implement file-to-file comparison engine (duration, streams, codec,
      colour, audio, metadata, timestamps, scene structure) (§38–40)
- [ ] Implement search over case data (§41)
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
- [ ] Review workflow for findings (new/reviewed/accepted/rejected) (§66)
- [ ] Analyst notes on asset/stream/timestamp/frame/finding/evidence (§65)
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
      535 passing across 12 crates. Rule evaluation is checked in both
      directions — each rule trips on a fixture built to trigger it *and* stays
      silent on a clean one, so a rule that fires on everything is caught.
      **Known gap:** 15 of the 26 rules have no fixture that triggers them end
      to end; they are tested only against hand-built bundles in `new_rules.rs`.
      Recorded as `NO_END_TO_END_FIXTURE` in `stage_guard.rs`, which now fails if
      a *new* rule joins that set — or if a listed one starts firing. Closing the
      gap means roughly fifteen fixtures, one per rule.
- [ ] Property tests for timestamps, frame ordering, container parsing
      Not started. No `proptest`/`quickcheck` dependency is declared.
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
