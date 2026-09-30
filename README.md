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

**Phase 0 — foundation.** The workspace, domain model, and documentation
skeleton are in place and building. The analysis engine is not yet implemented;
see [`todo.md`](todo.md) for the full plan and
[`docs/architecture.md`](docs/architecture.md) for the design.

```text
build   passing
tests   passing
clippy  clean (workspace, all targets, -D warnings)
fmt     clean
```

## What it does

- **Container inspection** — boxes, streams, timebase, duration consistency,
  malformed structures, trailing data
- **Video analysis** — structure, GOP layout, frame statistics, duplicate and
  near-duplicate detection, scene changes, colour and HDR signalling
- **Audio analysis** — channels, silence, clipping, DC offset, dynamic range,
  loudness, spectrum
- **Timestamp forensics** — PTS/DTS monotonicity, gaps, overlaps, edit lists,
  A/V offset and drift
- **Metadata analysis** — structured extraction plus consistency cross-checks
- **Evidence preservation** — SHA-256 and BLAKE3 at acquisition; derived
  artefacts stored with hashes and provenance
- **Rule-driven findings** — severity, confidence, timeline placement, and an
  explanation of what was observed and what it does not establish
- **Comparison** — two or more assets in one case, against a reference master
- **Reports** — PDF, HTML, JSON, CSV, all reproducible

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
rules/              rule definitions by domain
fixtures/           test media, including the corrupt-media corpus
tests/              integration tests
crates/
  ...-model/        domain types: cases, assets, hashes, findings, evidence
  ...-container/    container and stream inspection
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