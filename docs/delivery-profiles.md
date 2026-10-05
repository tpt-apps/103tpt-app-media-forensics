# Delivery profiles

A delivery profile is a declared specification: the codec, resolution, frame
rate, channels, sample rate, and container format a client requires. Checking a
file against one is spec §68 and §95 — the "executable media specification" that
is this product's long-term differentiator.

```text
tpt-media-forensics profile template --out client-x.json
tpt-media-forensics validate delivery.mp4 --profile client-x.json
```

## Writing a profile

Start from the template and edit it. JSON, not the YAML the spec illustrates:
`serde_json` is already a dependency of every crate here, and a parser added for
one config file would be the only dependency in this project not pinned to a
revision.

```json
{
  "name": "Client X Delivery",
  "version": 3,
  "requirements": [
    { "kind": "video_codec", "any_of": ["h264"] },
    { "kind": "video_resolution", "width": 1920, "height": 1080 },
    { "kind": "frame_rate", "fps": 25.0, "tolerance": 0.5 },
    { "kind": "audio_channels", "channels": 2 },
    { "kind": "audio_sample_rate", "sample_rate": 48000 },
    { "kind": "container_format", "any_of": ["mov"] },
    { "kind": "max_av_offset_ms", "limit_ms": 40.0 }
  ]
}
```

Check it loads before you rely on it:

```text
$ tpt-media-forensics profile check client-x.json
Profile       client-x-delivery v3
Name          Client X Delivery
Version       3
Fingerprint   1def861167c40d1bb93214551b220932
Requirements  4
  video.codec              h264
  video.resolution         1920x1080
  video.frame_rate         25 (+/- 0.5)
  container.format         mov
```

### Three rules the format enforces

**Every numeric requirement states its own tolerance.** There is no default. A
profile that silently supplied one would change its verdict because a field was
omitted — which is exactly what happens to a hand-written specification.

**Every requirement carries an id**, the same names spec §69 lists under `rules`:
`video.codec`, `video.resolution`, `video.frame_rate`, `audio.codec`,
`audio.channels`, `audio.sample_rate`, `container.format`,
`timing.max_av_offset_ms`. A reviewer matches a line of the specification to a
line of the result without translating between two vocabularies.

**An unknown `kind` is an error**, not a line to ignore. A typo that were
silently dropped would be a delivery that passes without ever having been checked
against that requirement.

## Versioning (spec §70)

A profile carries `version`, and both it and a fingerprint of the requirements
travel into every report and every `--json` result.

- `identifier()` — `client-x-delivery v3`
- `fingerprint()` — derived from the name, version, and every requirement

The fingerprint is derived from the requirements themselves, so editing a
requirement without bumping the version produces a **different fingerprint** from
the same **version number**. That disagreement is what makes "never silently
change an existing profile" checkable rather than merely stated.

`profile template` refuses to overwrite an existing file for the same reason: a
profile is maintained across versions, and overwriting one would destroy the
record of what the previous version actually required.
```text
$ tpt-media-forensics validate delivery.mp4 --profile client-x.json
Result        PASS
Source        /deliveries/A001_master.mp4
Profile       client-x-delivery v3
Fingerprint   1def861167c40d1bb93214551b220932

video.codec
  Expected: h264
  Observed: h264 (avc1)
  Result:    MET
  the declared codec is h264, which the profile accepts
video.resolution
  Expected: 1920x1080
  Observed: 1920x1080
  Result:    MET
  the declared frame size matches the profile
video.frame_rate
  Expected: 25 (+/- 0.5)
  Observed: 25
  Result:    MET
  the measured rate is within 0.5 fps of the profile
container.format
  Expected: mov
  Observed: isobmff
  Result:    MET
  the detected container format matches the profile

No forensic analysis was run for this check.
```

And the rejection a client will dispute:

```text
$ tpt-media-forensics validate A002_master.mp4 --profile client-x.json
Result        FAIL
...
video.resolution
  Expected: 1920x1080
  Observed: 1280x720
  Result:    NOT MET
  the declared frame size does not match the profile
video.frame_rate
  Expected: 25 (+/- 0.5)
  Observed: 25
  Result:    MET
```

The passing requirements are still listed. A report showing only the failure
reads as though nothing else was checked.

Exit code is **2** on `FAIL`, so a pipeline can gate on it. A delivery gate that
always exits 0 is not a gate.

Note the closing sentence: `No forensic analysis was run for this check.` That
distinction is the difference between "the analysis found nothing" and "no
analysis ran", and a reader of the output is entitled to know which.

## "Not measured" is not "passed"

A requirement that could not be checked reports `NOT MEASURED`, prints no
observed value, and **blocks delivery**. Three states rather than two is the
load-bearing decision in this subsystem.

- A WebM checked against `video.resolution` cannot pass: this build's Matroska
  reader exposes no picture geometry, so nothing established the resolution
  either way.
- An MP4 with no audio track checked against `audio.channels` cannot pass.
- `timing.max_av_offset_ms` reports `NOT MEASURED` when `validate` was given a
  file rather than a case, because that path inspects the container and does not
  run the timing analysis.

Folding any of these into `Met` would let a profile print `PASS` for a file it
never checked, which is the precise failure this product exists to prevent.

## Validating a case instead

With `--case-dir`, the specification is checked against the media the case
recorded, and the findings recorded for it are folded in:

```text
tpt-media-forensics validate --case-dir case.tptcase --profile client-x.json --write
```

The verdict is the **worse** of the two halves. A file can meet its
specification and still carry a significant finding, and a clean analysis does
not make an under-specified delivery acceptable. Both halves are always printed.

`--write` is off by default and renders the verdict and the requirement table
into a report bundle under `reports/validated/`. A verdict is a claim about
delivery, and silently adding one to an existing bundle would change a record the
analyst did not ask to change.

## Known limitations

- **`container.format: mov` matches any ISO-BMFF file.** This build detects the
  family from the `ftyp` signature and does not distinguish an Apple `qt  ` brand
  from `isom`, so an `isom`-branded file with a `.mov` name satisfies a `mov`
  requirement. Closing that means reading the major brand in `-container`; it is
  stated here rather than faked.
- **Codec families come from an explicit table.** An unrecognised tag, or a
  profile naming a family this build does not know, reports `NOT MEASURED` rather
  than a mismatch — a specification this engine cannot check is not a file that
  failed.
- **`timing.max_av_offset_ms` needs a case.** See above.
- **Audio parameters require an `AudioSampleEntry`.** The MP4 reader gained one in
  `container/src/audio_sample_entry.rs`; before it, `StreamAnalysis::audio` was
  `None` on every MP4 and §68's own `audio.channels` example was uncheckable.

## See also

- `docs/report-format.md` — how the verdict and table are rendered
- `docs/findings.md` — the severity half of the verdict
- `crates/tpt-app-media-forensics-rules/tests/delivery.rs` — the checker's tests
- `crates/tpt-app-media-forensics-cli/tests/professional_workflow.rs` — §96