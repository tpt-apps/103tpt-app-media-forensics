# Analysis Model

Reference: spec §12-§31, §54-§57, §77.

## Pipeline

```text
acquisition
   |
container analysis        spec §12  boxes, streams, timebase, duration
   |
   +--> video analysis     spec §14-§18
   +--> audio analysis     spec §19-§22
   +--> timing analysis    spec §23-§24
   +--> metadata analysis  spec §25-§26
   |
   +--> encoder fingerprinting   spec §27   best-effort, confidence-labelled
   +--> compression analysis     spec §28-§29
   +--> corruption detection     spec §30   graceful continuation
   +--> error timeline           spec §31
   |
rules  --> findings --> evidence --> report
```

## Declared vs measured

The single most important distinction in the model.

A container *declares* a frame rate in a box. Analysis *measures* the frame
timing from the packets. These can disagree, and the disagreement is often the
most interesting thing in the file — so they are modelled separately
(`VideoFormat::frame_rate` vs measured timing) and reconciled by an explicit
metadata-consistency rule (spec §26).

The same applies to duration (declared in `moov`, derived from timestamps),
channel count, and sample rate.

Never silently overwrite a declared value with a measured one. Both are
observations; the report shows both and, where they conflict, says so.

## Failure policy

Analyzers return partial results plus a list of anomalies. They do not return
`Err` for bad input.

Reasons:

1. A malformed track should not abort a whole examination.
2. "Malformed media cannot crash the application" is an acceptance criterion
   (spec §96).
3. The failure is itself evidence, and belongs in the report.

A truncated MP4 should yield a container description, the streams that did
parse, a corruption finding, and an error-timeline entry — not an error
dialog.

## Large files

Analysis streams rather than loading whole files (spec §55). Bounded memory
means:

- fixed-size read buffers
- no whole-file allocation anywhere in the container, packet, or frame paths
- constant-memory statistics where possible (running sums, histogram bins)

Work runs on background workers with progress reporting and cancellation
(spec §56). Progress is reported per-asset and per-stage so the UI can show
meaningful state for a 40 GB file.

## Caching

`CacheKey` = asset SHA-256 + `AnalysisVersion` + `ProfileFingerprint` +
`RuleSetFingerprint`.

All four are required. A cached result is valid only when the asset is
unchanged *and* the engine's analysis behaviour *and* the profile *and* the
rule set are unchanged. Omitting any one silently serves stale results — for
example, after a threshold is tightened, the old findings would still be
reported as current.

`AnalysisVersion::CURRENT` is the lever: bump it whenever a change could alter
a result, including threshold or heuristic changes that leave the API
untouched.

## Determinism

Spec §77 requires identical results across runs. Concretely:

- content-derived IDs, never random UUIDs
- `BTreeMap` where serialised order is observable
- exact integer/rational timing arithmetic, no float accumulation
- rules evaluated and emitted in sorted rule-ID order
- any sampling records its method and seed, and the report states it

Run the test suite twice on the same input to confirm; the model crate's
ordering tests exist to catch regressions here.