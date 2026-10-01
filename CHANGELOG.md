# Changelog

All notable changes to this project are docum
#### Phase 1 - report generation and case persistence (spec 59-63, 66)
- `-report::pdf`: PDF rendering written directly, with no new dependency
  - Emits PDF 1.4 with the base-14 Helvetica font, so nothing is embedded and
    the output stays byte-deterministic (spec 77) - a PDF library with its own
    metadata or timestamp defaults would break the evidence manifest hash
  - Text is drawn as positioned `Tj` operators: no shaping, no kerning, and no
    non-Latin-1 glyphs. A character outside Latin-1 becomes `?` rather than
    being dropped, because silently omitting text would misrepresent a finding
  - The disclaimer is appended inside `to_pdf` itself, so no invocation can
    produce a PDF without it (spec 59)
  - 14 tests covering xref byte offsets, `startxref`, stream lengths,
    pagination, string balancing under hostile metadata, and determinism
- CLI `report` renders JSON, HTML, CSV, PDF, and a self-verifying bundle,
  inferred from the output extension. It reads stored findings and never
  re-analyses, so a report states what was observed at the time.
- The evidence bundle now includes `case-report.pdf` alongside the JSON, HTML,
  and CSV deliverables (spec 62)
- Case persistence: schema v2
  - `findings.payload` stores each finding as canonical JSON. The typed columns
    cannot reconstruct a `Finding` - stream scope, evidence references, and
    reviewer state have no column - so the payload is what `report` reads back
  - `analyses` records the profile, profile fingerprint, rule-set fingerprint,
    and start time, which is what lets a rebuilt report state the same
    analysis fingerprint the original run did (spec 63)
  - **Findings are keyed by `(analysis_id, id)`, not `id` alone.** A `FindingId`
    is derived from the observation's content, so re-analysing the same file
    under a different profile produces the same id. Keying on id alone made a
    second analysis fail on a primary-key collision, which contradicts
    spec 66: re-analysing records new observations, it never edits old ones.
  - `finding_reviews` carries `analysis_id` so a review always resolves to one
    specific run's finding
- 5 CLI tests for `report`: each format, findings carried through, determinism
  across two renders, refusal of an unknown format, and the bundle manifestented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

#### Phase 0 — repository and foundation setup
- Cargo workspace with twelve crates per spec §8: `model`, `core`,
  `container`, `video`, `audio`, `timing`, `metadata`, `evidence`, `rules`,
  `report`, `cli`, `tauri`
- Dual MIT / Apache-2.0 licensing (copyright TPT Solutions)
- `-model`: the domain vocabulary the whole engine shares
  - `Case` (spec §9) with idempotent asset, finding, and report registration
  - `MediaAsset` and `AcquisitionRecord` (spec §10, §11) with content-derived
    IDs and read-only source semantics
  - `HashSet` covering SHA-256 and BLAKE3, with an explicit
    `AssetIntegrity::Unverifiable` outcome so an incomplete recheck is never
    reported as intact
  - `Finding` (spec §34) with severity, confidence, timeline placement, and
    review state kept separate from the observation (spec §66)
  - `Evidence` (spec §32, §33) with provenance, relative paths, and
    write-then-verify integrity semantics
  - `MediaTime`, `Timebase`, and `Rational` (spec §24) using exact integer and
    rational arithmetic with saturating conversion
  - Content-derived, deterministic identifiers (spec §77)
  - `CacheKey` and analysis fingerprints (spec §54, §63)
- CLI with `hash`, `acquire`, `audio`, `inspect`, `analyze`, `report`, and
  `batch` subcommands
- Tauri desktop shell crate as a separate workspace (spec §78)
- Documentation: architecture, evidence model, analysis model, findings,
  report format, rules, foundation integration, decoding tiers

#### Phase 1 — acquisition and case storage (spec §11, §53, §58)
- `-core::acquisition`: single-pass SHA-256 + BLAKE3 hashing with bounded
  memory (fixed 1 MiB buffer, so a 4 GB asset costs the same as a 4 KB one)
- Sources are opened read-only; the guarantee is asserted by an end-to-end CLI
  test that compares file bytes before and after
- Detects a source that changed size while being hashed and refuses to record a
  digest of a moving target
- Captures filesystem timestamps and platform metadata, leaving unavailable
  values `None` rather than substituting a guess
- `-core::case_dir`: case directory layout, versioned JSON manifest written via
  atomic temp-file rename, refusal to overwrite an existing case, and cache
  clearing scoped so it can never remove the record

#### Phase 1 — container inspection (spec §12, §13)
- TPT foundation crates wired as pinned-rev git dependencies; the crate names
  in the spec are repository names, and the real crates are sub-crates within
  them (documented in `docs/foundation.md`)
- `-container::probe`: format detection by file signature, not by extension,
  plus extension-mismatch reporting
- `-container::mp4`: MP4/ISO-BMFF inspection over `tpt-kinetix-demux`, mapping
  Kinetix tracks to `StreamAnalysis`. Frame rate is *measured* from the `stts`
  timing table, not copied from a declared value
- Variable-cadence tracks report no single frame rate rather than being
  smoothed into a misleading number
- Whole-file loading capped at 2 GiB; larger files are refused with an explicit
  error instead of risking an allocation failure
- `-container::fixture`: deterministic synthetic MP4 generator, which also
  serves the corrupt-media corpus (spec §76) without an external encoder

#### Phase 1 — timestamp forensics and A/V sync (spec §23, §24)
- `-timing::pts_dts`: PTS/DTS scanning for non-monotonic timestamps, gaps,
  overlaps, and negative (pre-roll) values. Expected frame duration is the
  *modal* inter-sample delta, so a track that is mostly regular with one long
  gap is not misread, and ordinary variable-frame-rate variation is absorbed by
  a profile-supplied tolerance rather than flagged as a defect
- `-timing::av_sync`: initial offset, final offset, and drift measured over the
  span it applies to
- Offset is the **median** of per-frame residuals, not the mean: when the true
  offset is not a whole frame interval the residuals alternate between roughly
  `-offset` and `frame - offset`, and the mean of that alternation is biased

#### Phase 1 — GOP structure (spec §15)
- `-video::gop`: GOP structure from the container's `stts` and `stss` boxes,
  with **no decoding required** — Tier-1 analysis that survives a decoder
  failure (see `docs/decoding-tiers.md`)
- GOP changes are reported as *transitions* against the preceding GOP, not as
  every GOP deviating from a global mode. One structural event produces one
  finding, in the direction it actually occurred
- `track_frame_info` expands the run-length `stts` table into per-frame
  timestamps, bounded by `MAX_EXPANDED_SAMPLES` so a hostile sample table
  cannot drive unbounded allocation

#### Phase 1 — duplicate detection (spec §16, §17)
- `-video::duplicate`: exact-duplicate detection at the packet layer by hashing
  compressed sample bytes — no decoding required
- Findings carry an explicit `Soundness`: `PixelIdentical` when every sample in
  the run is a keyframe (I-frames are self-contained, so identical bitstreams
  prove identical pictures), `CompressedMatchOnly` when a predicted frame is
  involved (identical bytes do not prove identical output, because reference
  state is unknown)
- `-container::read_samples`: per-sample digests, timing, and keyframe flags via
  the Kinetix demuxer, tolerating truncation rather than failing

#### Phase 1 — audio analysis (spec §19–22)
- `-audio::measurement`: every measurement is a `Measurement` carrying a
  `Methodology`, so spec §21's "never report a number without identifying the
  methodology" is enforced by the type system rather than by convention
- `level_stats` (peak, RMS, DC offset) uses Kahan compensated summation: a naive
  accumulator drifts enough over a long signal to shift the reported DC offset
- `find_silence` requires the *peak* within a region to stay under the
  threshold, so a fade is not misreported as a silent passage
- `-audio::loudness`: ITU-R BS.1770-4 integrated loudness with K-weighting and
  both gates, verified against the standard's -3.01 LUFS reference for a
  full-scale 1 kHz sine
- Loudness reports **no figure** at sample rates BS.1770-4 does not define (it
  publishes coefficients only for 48 kHz) rather than producing a
  plausible-looking number from the wrong filter
- New CLI `audio` command decoding WAV via `tpt-av-cadence`

#### Phase 1 — metadata extraction and consistency (spec §25, §26)
- `-metadata::tree`: every entry retains the scope and container element it
  came from, because a conflict is only visible when both competing values
  survive. Entries are sorted deterministically (spec §77)
- `-metadata::consistency`: reports keys whose values disagree *between
  scopes*, naming each value and its source, and asserting nothing about why
  they disagree (spec §26)
- Two tracks disagreeing within one scope is not a conflict: they are not
  competing versions of one fact
- New CLI `metadata` command
- Metadata atom names render as `u+a9cmt` rather than the raw copyright byte,
  which was illegible in a terminal

#### Phase 1 — SQLite persistence (spec §52)
- `-core::store`: case database holding cases, assets, analyses, streams,
  findings, evidence, rule results, reports, and analyst notes
- Versioned schema via `PRAGMA user_version`; migrations are ordered and
  never rewritten. A case written by a newer build is refused rather than
  silently misread
- Foreign keys are enforced per connection (`PRAGMA foreign_keys = ON`),
  which SQLite does not do by default. Without it a finding could reference
  an analysis that does not exist and the database would accept it, producing
  a report that cites evidence from nowhere
- Findings and evidence are insert-only. A reviewer's verdict lives in
  `finding_reviews`, keyed to the finding, so accepting or rejecting a finding
  never edits the observation it judges (spec §66)
- Observed values are stored verbatim. A digest the case "helpfully"
  upper-cased would no longer match what the file reports
- `UNIQUE (case_id, sha256)` makes a duplicate import of the same file into
  one case impossible, while permitting the same file across two cases where
  comparison is legitimate
- Write-ahead logging and `synchronous = FULL`: the record must survive a
  crash mid-write
- `rusqlite` with the `bundled` feature, so no system SQLite is required

#### Phase 1 — rule engine and the built-in rule set (spec §35–37)
- `-rules::engine`: a `ForensicRule` trait over an `AnalysisBundle`. Rules are
  pure functions over analysis results: they never open a file, run a decoder,
  or write anything, so they are testable with synthetic results and no media
- `what_it_checks` and `why_it_matters` are trait methods rather than doc
  comments, because spec §71 requires every finding to explain itself. A rule
  that cannot state why its condition matters should not exist
- Rules are registered and evaluated in sorted rule-ID order, and findings are
  sorted by severity, rule ID, then timeline position, so two runs produce
  identical output (spec §77)
- `-rules::profile`: every tolerance lives in a versioned `RuleProfile`, never
  in a rule. Changing a threshold changes the profile fingerprint, which
  invalidates the analysis cache (spec §54) and is named in the report (§63)
- Twelve built-in rules across container, video, audio, timing, and metadata
- Finding IDs are content-derived from rule, asset, and location, so re-analysing
  a file yields comparable IDs rather than renumbered ones
- Duplicate-run findings take their confidence from the soundness the detection
  layer established: a provable claim outranks an assumed one
- No rule asserts a cause. A test asserts rendered findings contain none of
  "tamper", "edit", "forged", or "manipulat" (spec §15, §26)

### Fixed

- Every stream was reported with index 0, which would mis-associate frame data
  in multi-track files
- `stss` stores 1-based sample numbers and was being read as 0-based, placing
  every keyframe one frame late and shifting every GOP boundary. Regression
  tests assert the packet path and the box path agree on keyframe positions
- `StreamKind` and `HashAlgorithm` serialised as Rust variant names
  (`"Audio"`) rather than stable lowercase tags in machine-readable output
- A report printed a peak amplitude of `1.0` as `"1.0 dBFS"`; full scale is
  0 dBFS, so peak level and peak amplitude are now distinct measurements
- The CLI reported "not implemented yet" for a file that was not a recognised
  container, conflating a gap in the tool with a finding about the evidence
- An unrecognised `fourcc` containing control bytes rendered as blank space in
  the terminal; now shown as `\xNN`
- Kahan summation double-counted each sample, inflating RMS and breaking
  DC-offset detection

### Notes

- TPT foundation crates are consumed as **pinned-rev git dependencies** (the
  ecosystem convention; none are on crates.io). Branches are never used: a
  moving dependency would break the reproducibility guarantee in spec §77.
  Updating a pin requires reviewing `AnalysisVersion::CURRENT`.
- `tpt-av-sync` is a CRDT collaboration engine with no A/V measurement
  capability, confirmed by source search across all six foundation
  repositories. Spec §23 is therefore implemented directly in `-timing`.
  See `docs/foundation.md`.
- `tpt-visual` requires a GPU, so it is not a dependency of the analysis path.
- H.264 decoding is **integrated, never implemented**: `tpt-kinetix-h264` is
  already bit-exact against ffmpeg. Its `pixel_exact` capability is honoured —
  Tier-2 measurements are withheld rather than computed on approximate frames.
- When a pinned `rev` moves, `AnalysisVersion::CURRENT` must be reviewed and
  bumped if any analysis result could change, so cached results are never
  served against a changed decoder.