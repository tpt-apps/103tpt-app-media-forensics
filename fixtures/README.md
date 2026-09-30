# Test fixtures

Media fixtures used by golden tests, fuzzing, and the corrupt-media corpus.

## Corrupt-media corpus (spec §76)

Each fixture must document the anomaly it is meant to provoke. A fixture
without a documented intent is a liability: when a test starts passing for the
wrong reason, nobody can tell which behaviour regressed.

| Fixture | Intended anomaly |
|---|---|
| `valid.mp4` | Baseline; no anomalies |
| `truncated.mp4` | File ends mid-stream; partial results must still be produced |
| `bad-header.mp4` | Unreadable container header |
| `invalid-timestamps.mp4` | PTS/DTS out of order or non-monotonic |
| `missing-index.mp4` | No sample index; packets must be walked directly |
| `bad-audio-packet.mp4` | Corrupt audio packet data |
| `duplicate-frame.mp4` | Identical repeated frames |
| `duration-mismatch.mp4` | Declared duration disagrees with measured |
| `metadata-conflict.mp4` | Contradictory timestamps or encoder tags across boxes |

## Requirements for every fixture

- **Must never crash the engine.** Each one is a regression test for the
  "malformed media cannot crash the application" requirement (spec §75, §96).
- **Deterministic.** Same bytes in, same findings out (spec §77).
- **Small.** Keep fixtures minimal — a few seconds or a few KB is enough to
  provoke the condition. A corpus of multi-gigabyte files is impractical to
  keep in a repository.
- **Documented.** State the intended anomaly in a sidecar note or the test.

Locally generated fixtures are gitignored (`fixtures/local/`).