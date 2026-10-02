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
- [ ] Implement SQLite persistence (§52) for cases/assets/analyses/streams/
      findings/evidence/rules/reports/notes
- [x] Implement case directory layout (§53) and manifest format (§58)
- [ ] Implement asset acquisition record (§11): path, size, timestamps,
      SHA-256, BLAKE3, filesystem info — read-only source guarantee
      — model types and integrity verification are done; hashing I/O and
      filesystem metadata capture remain

- [x] Integrate tpt-kinetix (container demux: `tpt-kinetix-core`, `-demux`)
- [x] Integrate tpt-kinetix-vp9 and -av1 for frame decoding (royalty-free; never
      implement them ourselves. H.264 is patent-encumbered and not decoded.
      Tier-2 only — see docs/decoding-tiers.md)
- [ ] Integrate tpt-cadence (audio codec/timing)
- [x] Implement container/stream inspection (§12–13)
- [ ] Implement video analysis: structural, temporal, spatial, colour (§14)
- [x] Implement GOP analysis (§15) — packet-layer only; no decoding required
- [x] Implement frame analysis & duplicate detection (§16–17) — exact duplication
      at the packet layer, no decoding
- [ ] Implement near-duplicate detection (needs decoded pixels, Tier 2)
- [ ] Implement scene-change analysis (§18)
- [ ] Implement audio analysis: codec, channels, loudness, spectral (§19–22)
- [x] Implement A/V synchronisation analysis (§23) — initial offset, final
      offset, and drift measured over the span
      **Note:** spec §23 names `tpt-av-sync` for this, but that crate is a CRDT
      collaboration engine with no A/V measurement capability (confirmed by
      source search). Implemented directly in `-timing`; see `docs/foundation.md`
- [x] Implement timestamp forensics (PTS/DTS, monotonicity, gaps) (§24)
- [ ] Implement metadata extraction + consistency cross-checks (§25–26)
- [ ] Implement encoder fingerprinting (best-effort, confidence-labelled) (§27)
- [ ] Implement compression/bitrate analysis + anomaly detection (§28–29)
- [ ] Implement corruption detection with graceful continuation (§30)
- [ ] Implement error/anomaly timeline (§31)
- [ ] Define Finding model with severity + evidence + confidence (§34)
- [ ] Implement rule engine (`ForensicRule` trait) + rule profiles (§35–37)
- [ ] Implement first ~20 forensic rules across container/video/audio/
      timing/metadata
- [ ] Implement Evidence model + integrity metadata (hashes, provenance) (§32–33)
- [ ] Implement frame extraction as evidence
- [ ] Implement analysis cache keyed on asset hash + analysis version +
      profile hash + rule-set hash (§54)
- [ ] Implement large-file/streaming analysis with bounded memory,
      background workers, cancellation, progress reporting (§55–56)
- [ ] Implement file-to-file comparison engine (duration, streams, codec,
      colour, audio, metadata, timestamps, scene structure) (§38–40)
- [ ] Implement search over case data (§41)
- [ ] Implement report generation: PDF, HTML, JSON, CSV (findings +
      measurements), with required disclaimers (§59–63)
- [ ] Implement CLI (`inspect`, `hash`, `analyze`, `report`, batch) sharing
      the core engine with the GUI (§51)
- [ ] Implement batch analysis engine + directory batch mode (§48–49)
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
- [ ] Verify: analysis is reproducible from recorded profile + software
      version (analysis fingerprint, §63)
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
- [ ] Unit tests for parsers, timing, hashing, rule evaluation, tolerances
- [ ] Property tests for timestamps, frame ordering, container parsing
- [ ] Fuzzing for container/codec/metadata/packet/timestamp parsers
- [ ] Golden tests against known fixtures (metadata, structure, findings)
- [ ] Build corrupt-media test corpus (§76): truncated, bad-header,
      invalid-timestamps, missing-index, bad-audio-packet, duplicate-frame,
      duration-mismatch, metadata-conflict
- [ ] Determinism checks (stable rule ordering, no unseeded randomness) (§77)
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

## Duplicate detection, and what its certainty means

Exact-duplicate detection runs at Tier 1 by hashing the **compressed** sample
bytes. What that proves depends on the sample type, and the distinction is
enforced in the type system rather than left to prose:

| Samples | `Soundness` | Claim |
|---|---|---|
| all keyframes | `PixelIdentical` | these frames decode to identical pictures |
| any predicted frame | `CompressedMatchOnly` | identical compressed data; reference state assumed |

An I-frame is self-contained, so identical I-frame bitstreams *must* decode to
the same picture. That is a sound conclusion and is reported as one.

A P- or B-frame is predicted from its references. Identical bytes therefore do
**not** prove identical output — the reference picture behind them could differ.
Those runs are still reported, because a long run of them is meaningful (a
freeze frame, an inserted still), but they carry the weaker soundness and the
finding states the assumption explicitly.

Reporting both at one confidence level would let a reviewer mistake an
assumption for a proof — which is the specific failure this product exists to
avoid.

## `stss` is 1-based

A detail worth recording because it was a real bug: the sync-sample table
stores **1-based** sample numbers, while every frame index in this engine is
0-based. Reading it unconverted placed every keyframe one frame late, shifting
every GOP boundary and making frame 0 look non-keyframe.

`track_frame_info` converts at the boundary, and a regression test asserts that
the packet path (which goes through the demuxer) and the box path agree on
keyframe positions.