# Rules

Reference: spec §35, §36, §37, §69, §70, §71.

## What a rule is

A rule reads analysis results and emits findings. It does not read files, open
decoders, or write anything — all of that happens in the analyzers below it.

That constraint is what makes rules testable: a rule is a pure function from
analysis results to findings, so it can be unit-tested against synthetic
results with no media involved.

## The trait

```rust
pub trait ForensicRule {
    /// Stable identity, e.g. "VIDEO.FRAME_RATE_CHANGE".
    fn id(&self) -> &str;

    /// What this rule checks.
    fn what_it_checks(&self) -> &str;

    /// Why the condition matters.
    fn why_it_matters(&self) -> &str;

    /// Evaluate against a completed analysis.
    fn evaluate(&self, analysis: &AnalysisResults) -> Vec<Finding>;
}
```

`what_it_checks` and `why_it_matters` are not documentation conveniences —
they feed the explainability output every finding must carry (spec §71). A
rule that cannot state why it matters should not exist.

## Explainability

Every finding explains itself (spec §71):

```text
Rule:        VIDEO.FRAME_RATE_CHANGE
What:        Whether measured frame timing changes beyond the configured
             tolerance.
Why:         Unexpected changes may indicate cadence conversion, editing,
             encoding behaviour, or malformed timestamps.
Observed:    29.97 fps -> 30.00 fps at 00:37:21.120
Limitations: This finding alone does not establish manipulation.
```

The `Limitations` line is required. It is what keeps a mechanical observation
from reading as an accusation.

## Tolerances

Rules compare against tolerances rather than exact values, because media is
inexact. Tolerances come from the active profile, never from constants inlined
in a rule — otherwise a client-specific profile could not tighten them.

A tolerance change can alter results, so it changes the
`ProfileFingerprint` and therefore invalidates the cache (spec §54).

## Profiles

A profile selects which rules run and supplies their thresholds (spec §35-37).

Profiles are **versioned** (spec §70). A report identifies the exact profile
version used. An existing profile is never silently changed: a new requirement
means a new version, so a report produced last quarter still refers to a
profile that still means what it meant.

## Determinism

Rules are evaluated and emitted in sorted rule-ID order (spec §77). Two runs
over the same input must produce the same findings in the same order — no
unordered iteration, no reliance on registration or hash-map order.

Changing a rule's behaviour requires bumping `AnalysisVersion::CURRENT` so
cached results are invalidated.

## Layout

```text
rules/
  container/    boxes, streams, timebase, duration consistency
  video/        GOP, frames, duplicates, scenes
  audio/        channels, silence, clipping, loudness
  timing/       PTS/DTS, monotonicity, A/V sync
  metadata/     consistency, encoder signatures
```

The directories mirror the analyzer crates, so it is always clear which
analyzer a rule consumes.

## Initial rule set

Phase 1 targets roughly twenty rules:

```text
CONTAINER.INDEX_MISSING
CONTAINER.DURATION_MISMATCH
CONTAINER.UNEXPECTED_TRAILING_DATA
CONTAINER.MALFORMED_BOX
VIDEO.GOP_LENGTH_CHANGE
VIDEO.DUPLICATE_FRAME_RUN
VIDEO.FRAME_RATE_CHANGE
VIDEO.TIMESTAMP_DISCONTINUITY
AUDIO.CLIPPING
AUDIO.CHANNEL_COUNT_MISMATCH
AUDIO.SILENCE_REGION
AUDIO.DC_OFFSET
TIMING.NON_MONOTONIC_PTS
TIMING.TIMESTAMP_GAP
TIMING.TIMESTAMP_OVERLAP
TIMING.NEGATIVE_TIMESTAMP
TIMING.AV_SYNC_OFFSET
METADATA.TIMESTAMP_CONFLICT
METADATA.DECLARED_VS_MEASURED_MISMATCH
METADATA.ENCODER_SIGNATURE
```

Every rule must be defensible: it states what it checks, why it matters, what
was observed, and what the observation does not establish.