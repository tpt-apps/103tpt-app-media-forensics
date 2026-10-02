# TPT Media Forensics

**Offline professional media inspection, forensic analysis, validation, and
evidence preservation.**

When a media file matters — a delivery is disputed, a file fails to ingest, a
clip glitches, audio drifts out of sync, or metadata contradicts itself — this
tool answers a specific question:

> What exactly is inside this file, how was it produced, and is there anything
> technically wrong, unusual, inconsistent, or suspicious about it?

It is not a media player, and it is not an AI deepfake detector.

## Status

**Phase 1 — MVP, in progress.** The engine is implemented and working end to
end: acquisition, container inspection, GOP/duplicate/timestamp/metadata
analysis, structural damage detection, 26 built-in rules, case persistence, and
PDF/HTML/JSON/CSV reports.
MP4 and Matroska/WebM containers are analysed. Opus is decoded from bare Ogg
streams and from demuxed packets inside a container; **Vorbis is decoded from
bare Ogg streams only**, so a Vorbis track in a `.webm` is identified and
reported but not measured.

See [`todo.md`](todo.md) for the full plan and
[`docs/architecture.md`](docs/architecture.md) for the design.

```text
build   passing
tests   618 passing
clippy  clean (workspace, all targets, -D warnings)
fmt     clean
```

### What this build will not do

Stated up front, because a tool that quietly omits a measurement is worse than
one that names the gap:

- **It does not decode H.264, HEVC, or AAC.** Those are patent-encumbered.
  Their tracks are identified and their declared properties reported; no sample
  is ever decoded. Tier-2 pixel analysis covers VP9 and AV1 only.
- **It does not invent a number it did not measure.** The Matroska reader
  exposes no picture geometry, so WebM video streams report no resolution
  rather than a guessed one.
- **It does not assert causes.** A GOP change is reported as a GOP change.
  A reviewer's conclusion is recorded separately and never overwrites the
  observation.

## What it does

- **Container inspection** — boxes, streams, timebase, duration consistency,
  malformed structures, truncation, trailing data
- **Error timeline** — structural damage located by byte offset and placed at a
  media time, so a finding says *where* the file stops being sound
- **Edit lists** — a track's declared start delay read from `elst`, so a stream
  that does not begin at zero is reported rather than assumed to
- **Video analysis** — structure, GOP layout, duplicate and near-duplicate
  detection, scene changes, bitrate and compression anomalies
- **Colour signalling** — primaries, transfer, matrix and range read from the
  container's own declarations, plus HDR static metadata (mastering display,
  content light level) where present
- **Audio analysis** — channels, silence, clipping, DC offset, dynamic range,
  loudness, and spectral content (peak frequency, centroid, flatness)
- **Timestamp forensics** — PTS/DTS monotonicity, gaps, overlaps, edit lists,
  A/V offset and drift
- **Metadata analysis** — structured extraction plus consistency cross-checks
- **Encoder fingerprinting** — declared encoder tags and encoding structure,
  each labelled with its confidence and what it does not establish
- **Evidence preservation** — SHA-256 and BLAKE3 at acquisition, computed in one
  pass; derived artefacts stored with hashes and provenance
- **Rule-driven findings** — severity, confidence, timeline placement, and an
  explanation of what was observed and what it does not establish
- **Reports** — PDF, HTML, JSON, CSV, all reproducible

### Planned, not built

Named here so the gap is visible rather than inferred from silence. None of these
is implemented, and `crates/...-core/tests/readme_claims.rs` fails if any of them
reappears in the list above.

| Capability | Spec | State |
|---|---|---|
| Comparing two or more assets against a reference master | §38–40 | not started |
| Colour for Matroska / WebM | §45 | not started; the Matroska reader exposes no picture geometry, so there is no video format to attach colour to |
| A corrupt-media corpus held on disk | §76 | directories are empty; every damaged file today is built in code by a fixture |

## Three principles

**The source is read-only.** Analysis never modifies the media under
examination. Everything derived is written to the case directory with its own
hash.

**Observations are not verdicts.** A finding records what was measured, with a
severity and a confidence level, and states its own limitations. It does not
claim a file is fake. A reviewer's conclusion is recorded separately and never
overwrites the observation.

**Reproducible or not at all.** Every report names the software version, the
analysis version, the profile, and the exact rule set that produced it. Two
runs over the same input produce identical output.

## Layout

```text
Cargo.toml          workspace root
docs/               architecture, evidence, analysis, findings, reports, rules
rules/              empty per-domain placeholders; the rules themselves live in
                    crates/...-rules/src/builtin.rs
fixtures/           empty placeholders; damaged files are built in code by
                    ...-container/src/fixture.rs, not held as files
tests/              integration tests
crates/
  ...-model/        domain types: cases, assets, hashes, findings, evidence
  ...-container/    container and stream inspection, and the file builders
  ...-video/        video analysis
  ...-audio/        audio analysis
  ...-timing/       timestamp forensics and A/V sync
  ...-metadata/     metadata extraction and consistency
  ...-evidence/     evidence storage
  ...-rules/        rule engine and rule set
  ...-core/         orchestration, caching, progress, cancellation
  ...-report/       PDF / HTML / JSON / CSV
  ...-cli/          command line
  ...-tauri/        desktop shell (separate workspace)
```

## Building

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

The desktop shell is a separate workspace because it pulls in the webview
toolchain:

```bash
cd crates/tpt-app-media-forensics-tauri && cargo check
```

## CLI

```bash
tpt-media-forensics inspect  <file>
tpt-media-forensics hash     <file>
tpt-media-forensics analyze  <file> --case-dir <case>
tpt-media-forensics report   --case-dir <case> --out report.pdf
tpt-media-forensics batch    <directory> --case-dir <case>
```

The CLI and the desktop app drive the same engine, so results are identical
whichever you use. No command requires a network connection.

## TPT foundation

This project is a commercial layer over the TPT media foundation
(`tpt-kinetix`, `tpt-cadence`, `tpt-visual`, `tpt-audio`, `tpt-voice`,
`tpt-av-asset`, `tpt-av-sync`, `tpt-av-test`, `tpt-dsp`).

None are published on crates.io. They are consumed the way the rest of the
ecosystem does — as git dependencies with a **pinned revision**, never a
branch, because a moving dependency would break the reproducibility guarantee
in spec §77.

`docs/foundation.md` documents how they are wired, the mapping from the spec's
repository names to the actual crate names, and three discrepancies between
`spec.txt` and the crates as they exist (most notably that `tpt-av-sync` is a
CRDT collaboration engine, not the A/V measurement tool spec §23 assumes).

## License

Dual-licensed, at your option:

- MIT ([LICENSE-MIT](LICENSE-MIT))
- Apache License 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

Copyright (c) 2026 TPT Solutions.