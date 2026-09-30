# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **Phase 0 — repository and foundation setup**
  - Cargo workspace with twelve crates per spec §8: `model`, `core`,
    `container`, `video`, `audio`, `timing`, `metadata`, `evidence`, `rules`,
    `report`, `cli`, `tauri`
  - Dual MIT / Apache-2.0 licensing (copyright TPT Solutions)
  - Domain model in `tpt-app-media-forensics-model`:
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
    - `MediaTime`, `Timebase`, and `Rational` (spec §24) using exact integer
      and rational arithmetic with saturating conversion
    - Content-derived, deterministic identifiers (spec §77)
    - `CacheKey` and analysis fingerprints (spec §54, §63)
  - CLI with `inspect`, `hash`, `analyze`, `report`, and `batch` subcommands
  - Tauri desktop shell crate as a separate workspace (spec §78)
  - Documentation skeleton: architecture, evidence model, analysis model,
    findings, report format, rules
  - `rules/`, `fixtures/`, and `tests/` directory skeleton
  - **Phase 1 — acquisition and case storage** (spec §11, §53, §58)
    - `-core::acquisition`: single-pass SHA-256 + BLAKE3 hashing with bounded
      memory (fixed 1 MiB buffer, so a 4 GB asset costs the same as a 4 KB one)
    - Sources are opened read-only; the read-only guarantee is asserted by an
      end-to-end CLI test that compares file bytes before and after
    - Detects a source that changed size while being hashed and refuses to
      record a digest of a moving target
    - Captures filesystem timestamps and platform metadata, leaving unavailable
      values `None` rather than substituting a guess
    - `-core::case_dir`: case directory layout, versioned JSON manifest written
      via atomic temp-file rename, refusal to overwrite an existing case, and
      cache clearing scoped so it can never remove the record
    - CLI `hash` and `acquire` commands fully implemented, in text and `--json`
    - `-model`: `EntityId::from_canonical_str` for reading manifests written by
      earlier runs; `FilesystemInfo` extended with metadata size, link count,
      and permission/attribute bits
    - `-cli`: 10 end-to-end tests driving the real binary

### Notes

- TPT foundation crates (`tpt-kinetix`, `tpt-cadence`, `tpt-visual`,
  `tpt-audio`, `tpt-voice`, `tpt-av-asset`, `tpt-av-sync`, `tpt-av-test`,
  `tpt-dsp`) are declared in `[workspace.dependencies]` but not yet inherited:
  they are not on crates.io and most are private repositories, so referencing
  them would break offline builds. The analyzer crates depend on abstraction
  traits in the meantime.
- The analysis engine is not yet implemented; CLI commands report this
  explicitly rather than returning misleading empty results.