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

## The built-in rule set

Twenty-seven rules ship in Phase 1. The engine evaluates them in sorted
rule-ID order regardless of registration order, so the set cannot drift.

```text
CONTAINER.PARSE_ANOMALY
CONTAINER.DECLARED_TRACK_MISMATCH
CONTAINER.STREAM_DURATION_MISSING
CONTAINER.STREAM_START_OFFSET
CONTAINER.MALFORMED_STRUCTURE
CONTAINER.NO_USABLE_STREAMS
CONTAINER.STRUCTURAL_DEFECT
CONTAINER.TRUNCATED_MEDIA
VIDEO.ALL_FRAMES_KEYFRAMES
VIDEO.SINGLE_KEYFRAME
VIDEO.FRAME_RATE_CHANGE
VIDEO.GOP_LENGTH_CHANGE
VIDEO.BITRATE_DROP
VIDEO.DUPLICATE_FRAME_RUN
VIDEO.HDR_METADATA_MISSING
VIDEO.SCENE_CHANGE
VIDEO.NEAR_DUPLICATE_FRAME
AUDIO.CLIPPING
AUDIO.DC_OFFSET
AUDIO.SILENCE_REGION
AUDIO.INAUDIBLE
TIMING.NON_MONOTONIC_PTS
TIMING.TIMESTAMP_GAP
TIMING.AV_SYNC_DRIFT
METADATA.TIMESTAMP_CONFLICT
METADATA.DECLARED_VS_MEASURED_MISMATCH
METADATA.MISSING_CREATION_TIME
```

A test asserts this list matches `builtin_rules()`, so the count in this
document cannot drift from the code the way a hand-maintained list would.

### Severity and confidence are not interchangeable

Severity says how much a reviewer should attend to a finding; confidence says
how much weight the evidence carries. A rule sets both, and the distinction is
load-bearing:

- A structural fact read directly from the box structure — a declared track
  count that disagrees with the recovered streams — is `High` confidence. The
  condition either holds or it does not.
- A single frame duration departing from the track's dominant duration is
  `Medium`. A timestamp rounding artefact would produce the same observation,
  so the rule does not claim more than the evidence supports.

An absence of measurement is never a finding. A file whose audio will not
decode has no loudness, so `AUDIO.INAUDIBLE` stays silent and the gap appears in
the report's limitations instead.

### Every rule is tested in both directions

Each rule is exercised against a fixture that should trip it *and* one that
should not. A rule that only ever fires is as useless as one that never does,
and the negative case is what catches a comparison that is too permissive.

That distinction matters for `VIDEO.ALL_FRAMES_KEYFRAMES` in particular:
`build_mp4` writes no `stss` box, so the default test fixture is itself an
all-keyframes case. Only a fixture built with an explicit `stss` exercises the
negative case.

### Every rule must be defensible

A rule states what it checks, why it matters, what was observed, and what the
observation does not establish. None of them asserts a cause: a GOP change is
reported as a GOP change, not as evidence of editing (spec §15).