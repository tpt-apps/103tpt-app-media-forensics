# Evidence Model

Reference: spec §32, §33, §64.

## The three layers of trust

It is easy to conflate three different claims. They are separate, and the
product must never blur them.

| Claim | Rests on | Proves |
|---|---|---|
| The source is unchanged since acquisition | `AcquisitionRecord` hashes | Integrity from acquisition onward |
| The evidence artefact is unchanged since it was written | `EvidenceIntegrity` hashes | Integrity of the derived artefact |
| The source was unaltered before the case was opened | Chain of custody (§64, Phase 3) | Nothing the app can do alone |

`Evidence` hashes prove the middle claim only. A report that implies otherwise
is overstating its evidence, and the wording of every report template is
checked against this.

## Acquisition

Before any analysis, the source is recorded (spec §11):

- absolute source path
- file size in bytes
- modification, creation, and access timestamps where available
- SHA-256 (interoperability with legal/evidentiary workflows)
- BLAKE3 (fast local checking and cache keying)
- filesystem type, volume, and native file identifier

The source is opened read-only. Nothing in the engine writes to it.

### Re-verification

`MediaAsset::verify` re-checks a source against its acquisition record and
returns `AssetIntegrity`:

| Result | Meaning |
|---|---|
| `Intact` | Every recorded hash still matches and the size is unchanged |
| `HashMismatch` | Size matches but content differs |
| `Modified` | Size differs — replaced or truncated |
| `Unverifiable` | Not enough information to decide |

`Unverifiable` exists so that a partial hash set can never be reported as
`Intact`. "We could not check" and "it is fine" are different answers, and
conflating them would be a serious defect in a forensic tool.

## Evidence artefacts

Derived artefacts are written into the case directory and recorded with:

- a content-derived `EvidenceId`
- the owning `AssetId`
- `EvidenceKind` — frame, frame sequence, audio excerpt, structure dump,
  metadata export, timing trace
- `Provenance` — direct copy, lossless extract, lossy transform, derived
- a **relative** path within the case directory
- optional caption and source time range
- `EvidenceIntegrity` — size, hashes, and a verified flag

### Relative paths only

`relative_path` is always relative to the case root. An absolute path would
leak the analyst's directory layout into a report that may be produced for
another party, and would break as soon as the case is moved or archived.

### Written is not verified

`Evidence::new` forces `verified: false`. A freshly written artefact has not
been re-read, so claiming verification at construction time would be asserting
something unchecked. `mark_verified` is called only after the engine re-reads
the file and recomputes its hashes.

`Evidence::verify` returns `false` — never `true` — when the recorded hash set
is incomplete, for the same reason `AssetIntegrity::Unverifiable` exists.

## Provenance and evidence strength

Provenance is recorded because it changes how much an artefact is worth:

- `DirectCopy` and `LosslessExtract` preserve the source signal
- `LossyTransform` does not — a re-encoded frame is weaker evidence
- `Derived` is engine-computed (a histogram, a difference image) and is an
  interpretation, not a capture

## Retention

Evidence is stored inside the case directory so a case remains self-contained
and can be archived or handed over as a unit. Deleting evidence must be an
explicit, recorded analyst action — never a side effect of cache eviction.