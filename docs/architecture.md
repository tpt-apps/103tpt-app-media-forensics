# Architecture

TPT Media Forensics is a commercial workflow layer over the TPT media
foundation. This document describes how the pieces fit together and, just as
importantly, what the boundaries are *for*.

## Design commitments

Four properties shape every decision in this codebase.

### 1. The source media is read-only

The source file is evidence. Analysis never writes to it. Everything the engine
produces — extracted frames, waveform excerpts, structure dumps, reports — is
written into the case directory, alongside a hash of the artefact
(spec §11, §32).

### 2. Observations are not verdicts

The product deliberately does not claim to determine whether media is "fake".
A finding records *what was observed*, with a severity and a confidence level,
and stops there. A separate review workflow records the reviewer's conclusion
without altering the observation (spec §34, §66, §71).

This is why `Finding` carries `Observation`, `Confidence`, and `FindingStatus`
as distinct fields rather than one blended "verdict".

### 3. Determinism

Two runs over the same input must produce identical output (spec §77). In
practice this rules out:

- random identifiers — IDs are content-derived (`EntityId::new_derived`)
- unordered iteration where output order matters — `HashSet` is a `BTreeMap`
- floating-point accumulation in timing work — `MediaTime` and `Rational` are
  exact integer types
- rule ordering that depends on registration order — sorted by rule ID
- sampling without a recorded methodology and seed

Any change that can alter an analysis result must bump
`AnalysisVersion::CURRENT`. This is what keeps the cache (spec §54) and the
reproducibility claim (spec §63) honest.

### 4. Malformed media cannot crash the application

Container, codec, and packet data is fully attacker-controlled (spec §75).
Parsers are bounds-checked, arithmetic saturates rather than overflows, and
recursive structures are depth-capped. Analyzers return partial results plus
anomalies rather than propagating errors upward, so one broken track does not
abort a whole examination.

## Crate layout

```text
tpt-app-media-forensics/
  crates/
    ...-model/       domain types; no I/O, no workspace deps
    ...-container/   container boxes and stream enumeration
    ...-video/       GOP, frames, duplicates, scenes
    ...-audio/       channels, silence, clipping, loudness
    ...-timing/      PTS/DTS forensics and A/V sync
    ...-metadata/    metadata tree and consistency checks
    ...-evidence/    writes derived artefacts with hashes
    ...-rules/       ForensicRule trait, profiles, rule set
    ...-core/        orchestration and caching
    ...-report/      PDF / HTML / JSON / CSV rendering
    ...-cli/         command-line front end
    ...-tauri/       desktop shell (separate workspace)
```

Dependencies point strictly downward: `model` depends on nothing in the
workspace, and `core` sits above every analyzer. Nothing depends on `core`
except the front ends.

## One engine, many front ends

```text
      +-----------+        +------------+
      |    CLI    |        |   Tauri    |
      +-----+-----+        +------+-----+
            |                     |
            +----------+----------+
                       |
                 +-----v-----+
                 |   core    |   deterministic analysis engine
                 +-----+-----+
                       |
     +---------+-------+-------+----------+
     |         |       |       |          |
  container  video   audio  timing    metadata
                       |
                    evidence
                       |
                     rules
```

The CLI and the desktop app call the same engine (spec §51). The Tauri crate
is a separate workspace and depends *on* the engine, never the reverse — that
is what keeps the engine independent of Tauri and reusable by automation and
future services (spec §78).

## Data flow

```text
source file (read-only)
      |
      v
[acquisition]  size, timestamps, SHA-256, BLAKE3, filesystem info
      |                                              spec §11
      v
[container]  boxes, streams, timebase, duration consistency
      |
      +--> [video]    structure, GOP, duplicates, scenes
      +--> [audio]    channels, silence, clipping, loudness
      +--> [timing]   PTS/DTS monotonicity, gaps, A/V sync
      +--> [metadata] tree + consistency cross-checks
      |
      v
[rules]       read results, emit findings with severity + confidence
      |
      +--> [evidence]  extracted frames, traces, hashes
      v
[report]      PDF / HTML / JSON / CSV + reproducibility data
```

This is `-core::pipeline::AnalysisEngine`. Three details in that ordering are
load-bearing rather than incidental:

- **The cache is consulted before any analysis work**, not after. The key
  covers asset content, analysis version, profile, and rule set (spec 54), so
  a cache hit is only possible when the analysis would have been identical.
- **A stage that cannot run does not abort the examination.** A file whose audio
  track will not decode still produces a full container report; the gap is
  appended to `AnalysisOutcome::limitations` (spec 60). An examiner needs to
  know what was *not* looked at as much as what was.
- **The analysis fingerprint is computed once.** `AnalysisEngine::analysis_fingerprint`
  and the report methodology both call the same function, so a report and its
  cache validity cannot disagree about what was run.
```

## Reproducibility

Every report records, and every cache key incorporates:

- the asset SHA-256 from the acquisition record
- `AnalysisVersion::CURRENT` — engine analysis behaviour
- `ProfileFingerprint` — the exact profile configuration
- `RuleSetFingerprint` — the exact set of rules that ran

These form `CacheKey`. If any one differs, a cached result is invalid. That is
what allows a third party to re-run an analysis and get the same answer
(spec §63, §96).

## TPT foundation crates

The engine is intended to sit on the TPT media foundation: `tpt-kinetix`,
`tpt-cadence`, `tpt-visual`, `tpt-audio`, `tpt-voice`, `tpt-av-asset`,
`tpt-av-sync`, `tpt-av-test`, `tpt-dsp`.

**Current status:** none of these are published on crates.io, and only
`tpt-dsp` is public on GitHub. The rest are private TPT Solutions
repositories.

They are therefore declared in `[workspace.dependencies]` but deliberately
*not yet inherited* by any member crate — referencing an unresolvable
dependency would make the workspace impossible to build or test offline.

Until they are available, the video, audio, and timing crates depend on
abstraction traits rather than on concrete backends. The analysis logic — which
is what produces findings — is therefore decoupled from the decoder and stays
unit-testable without real media. Adapting a foundation crate later means
implementing a trait, not rewriting analysis code.

## Error handling

There is no `anyhow` in the engine crates. Analysis failures are data, not
exceptions: they are recorded as anomalies on the result, attached to the
finding they explain, and surfaced in the report. `anyhow` appears only at the
CLI boundary, where a human needs a readable message.

This is a deliberate consequence of "malformed media cannot crash the
application" — an error that unwinds out of an analyzer is an error that can
take down the run.