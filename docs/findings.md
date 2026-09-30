# Findings

Reference: spec §34, §66, §71, §80, §81.

## A finding is an observation

A finding records something the engine measured. It does not assert what that
measurement means.

```text
GOOD   "GOP structure changes from approximately 60 frames to
        approximately 15 frames at 00:37:21.120."
       confidence: High

BAD    "File was edited at 00:37:21."
```

The first is checkable by anyone re-running the analysis. The second is a
conclusion the data does not support, and stating it as a finding would
discredit the entire report.

The reviewer's conclusion lives in `FindingStatus` (spec §66) and
`review_note`, which are separate fields precisely so a conclusion can never
overwrite the observation.

## Structure

```rust
struct Finding {
    id: FindingId,
    rule_id: String,           // e.g. "VIDEO.FRAME_RATE_CHANGE"
    severity: Severity,        // Critical | Significant | Warning | Info
    confidence: Confidence,    // High | Medium | Low
    observation: Observation,  // summary + measured values
    asset_id: AssetId,
    stream_id: Option<StreamId>,
    timeline_start: Option<MediaTime>,
    timeline_end: Option<MediaTime>,
    evidence: Vec<EvidenceId>,
    status: FindingStatus,     // New | Reviewed | Accepted | Rejected | ...
    review_note: Option<String>,
}
```

### Severity

How significant, not how certain.

| Severity | Meaning |
|---|---|
| `Critical` | Structural failure, or evidence of tampering beyond reasonable dispute |
| `Significant` | A deviation a reviewer must assess |
| `Warning` | Worth recording; not by itself a problem |
| `Info` | Informational, no implied defect |

`Severity::fails_validation()` marks `Critical` and `Significant` as failing a
delivery profile (spec §68); warnings produce PASS WITH WARNINGS rather than
FAIL.

### Confidence

How much weight the *rule* placed on the evidence. Separate from severity.

A perfectly executed threshold comparison can still be a weak observation, and
the report should say so. Encoder fingerprinting, being heuristic, is
typically `Low` — which is exactly why spec §27 says never to claim exact
provenance from weak evidence.

### Evidence

`Finding::has_evidence()` exists because a finding with no supporting artefact
is a bare assertion. Severity does not excuse that; a `Critical` finding
without evidence should be treated with suspicion by any reviewer.

## Rule identity

Rule IDs are `DOMAIN.TECHNIQUE`, e.g.:

```text
CONTAINER.INDEX_MISSING
CONTAINER.DURATION_MISMATCH
VIDEO.GOP_LENGTH_CHANGE
VIDEO.DUPLICATE_FRAME_RUN
VIDEO.FRAME_RATE_CHANGE
AUDIO.CLIPPING
AUDIO.CHANNEL_MISMATCH
TIMING.NON_MONOTONIC_PTS
TIMING.AV_SYNC_DRIFT
METADATA.TIMESTAMP_CONFLICT
METADATA.ENCODER_SIGNATURE
```

The ID is part of the report contract: it appears in reports, in CSV exports,
and in the profile configuration, and must stay stable across releases.

## Timeline placement

`timeline_start` / `timeline_end` place the finding on the timeline, which is
the central navigation mechanism (spec §81). Selecting a finding jumps the
viewer to that moment.

A finding with no start is placed at zero with a zero-length range
(`Finding::timeline_range`), so the UI never has to special-case a missing
value.

## Review workflow

```text
New -> Reviewed -> Accepted
                -> Rejected
                -> RequiresInvestigation
```

Transitions are recorded separately from the finding's content. Re-analysing
the asset produces a new `AnalysisRecord`; it does not rewrite what a previous
run observed, and it does not silently discard review decisions.

## Dashboard

The dashboard counts findings by severity and reports an overall review status
(spec §80). It must **not** collapse the case to a single "authentic/fake"
score — that would misrepresent a set of independent observations as a single
verdict, which is precisely the overclaiming this product exists to avoid.