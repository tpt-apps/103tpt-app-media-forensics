# Report Format

Reference: spec §59-§63, §68, §95, §96.

## Outputs

| Format | Use |
|---|---|
| PDF | The deliverable an analyst hands to a client, court, or broadcaster |
| HTML | Reviewable in a browser, embeds the evidence gallery |
| JSON | Machine-readable; the integration format for other tools |
| CSV | Findings and measurements for spreadsheets |

JSON is the canonical format. PDF, HTML, and CSV are renderings of it, so a
JSON report must be sufficient to reconstruct any other.

## Required content

Every report carries:

1. **Case identity** — name, ID, description
2. **Asset list** with acquisition records: size, timestamps, SHA-256, BLAKE3
3. **Software version** of the engine that produced the report
4. **Analysis version** (`AnalysisVersion::CURRENT`)
5. **Profile fingerprint** and **rule-set fingerprint**
6. **Findings**, most severe first, each with severity, confidence,
   observation, measurements, and evidence references
7. **Evidence manifest** — every artefact referenced, with hashes
8. **Disclaimers** (below)

Items 3-5 are what make the analysis reproducible (spec §63, §96). A report
that cannot state which rule set produced it cannot be re-run.

## Disclaimer

Required on every report, without exception:

```text
This report records technical observations about the structure, timing,
encoding, metadata, and integrity of the media files examined.

These observations are the output of automated analysis. They do not by
themselves establish that any file has been altered, that any content is
genuine, or that any person acted in any particular way.

No finding in this report, individually or collectively, constitutes proof of
manipulation, authenticity, or intent. A qualified reviewer must assess each
observation in context.

Findings describe technical conditions only. A finding is not a conclusion
about the content or history of the material.
```

The disclaimer is not legal decoration. It states the product's actual
position: it measures media, it does not adjudicate authenticity.

## Measurement methodology

Every measurement names the standard or method that produced it (spec §21).

```text
GOOD   Integrated loudness: -23.1 LUFS  (ITU-R BS.1770-4, 400 ms gating)

BAD    Loudness: -23.1
```

A loudness figure without its standard is not reproducible, and two figures
computed differently are not comparable. Both problems are avoided by always
attaching the method.

## Deterministic output

Given the same case, report output is byte-identical (spec §77):

- findings sorted by severity descending, then by rule ID, then by timeline
  position
- timestamps rendered from integer microseconds, never from locale-dependent
  formatting
- no wall-clock generation timestamp in the deterministic body, or it is
  emitted as an explicitly separate field that the reproducibility check
  excludes

## Validation reports

Delivery validation (spec §68, §95) uses the same machinery with a different
header:

```text
Result: FAIL

VIDEO.FRAME_RATE
  Expected: 25
  Observed: 24.0

AUDIO.CHANNELS
  Expected: 2
  Observed: 6
```

One of three results: `PASS`, `PASS WITH WARNINGS`, `FAIL`. Critical and
significant findings fail; warnings produce PASS WITH WARNINGS
(`Severity::fails_validation`).

This is what lets one engine serve both media QC and forensic analysis without
collapsing them into a single product.

## Case directory layout

A case is self-contained and portable:

```text
case.tptcase/
  manifest.json          case identity, assets, analyses, reports
  case.db                SQLite: findings, evidence, notes, review state
  assets/                references to sources (never copies)
  evidence/
    frames/              extracted frames
    audio/               waveform and spectrogram excerpts
    traces/              timestamp and packet traces
  reports/               generated PDF/HTML/JSON/CSV
  cache/                 analysis cache, safe to delete
```

Assets are referenced, not copied: the source file is never modified, and
copying a multi-gigabyte evidentiary file into a case directory by default
would be surprising and expensive. `cache/` is the only disposable
subdirectory — everything else is part of the record.