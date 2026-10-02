# Changelog

All notable changes to this project are documented in this file, following
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
[Semantic Versioning](https://semver.org/).

#### The README claimed colour and HDR analysis that no reader performs
- "Video analysis — structure, GOP layout, duplicate and near-duplicate
  detection, scene changes, **colour and HDR signalling**" was listed under
  "What it does". `ColourInfo` exists in `-model` and serialises, which is
  presumably why the claim survived review: the types are right there
- No reader populates it. `mp4.rs` assigns `colour: Default::default()` and
  `is_hdr: false` on every track, so all five fields are empty for every file
  analysed. The Matroska reader exposes no picture geometry at all
- This is the same shape as the two unwired stages already fixed below: a
  capability that is *modelled* but never *measured*. A report rendering a
  `ColourInfo` would show an empty struct and read as "no colour information
  found" rather than "never looked"
- Moved to "Planned, not built" with the reason stated. The rule set never fired
  on colour, so nothing downstream depended on the claim

#### The guard could only fail in one direction, and had
- `readme_claims.rs` maintained a denylist of capabilities that must not be
  claimed. Its BLAKE3 entry read "declared as a dependency but never called" —
  which stopped being true the moment `acquisition.rs` began hashing with it,
  and nobody noticed until the README was read against the source
- A denylist cannot detect a claim going stale in the other direction, which is
  precisely how this file's own subject matter rots. It asserts absences, and
  nothing verified the presences
- Added `every_claimed_capability_exists_in_the_source`: twelve claimed
  capabilities, each paired with the file that implements it, compared with
  `include_str!` at compile time. A path that stops resolving is a build error
  rather than a check that quietly matches nothing
- Verified by breaking it rather than by reading it. Reintroducing the colour
  claim failed two of the four tests by name; a deliberately wrong marker
  (`pub fn probe`, a function that does not exist) was caught before the test
  was trusted with anything real

#### Three more capabilities were claimed in docs rather than code
- `docs/architecture.md` listed `...-audio/ ... spectrum`, `...-video/ ... colour`,
  and `...-core/ ... progress, cancellation`. The audio crate has no FFT, the
  video crate has no colour reader, and `-core` contains no progress or
  cancellation type at all — the last one had been true since the crate was
  scaffolded
- The audio crate's own module docs claimed "spectral measurement". This is the
  sharpest of the four, because a crate-level doc comment is the thing a new
  contributor reads first and trusts most
- All corrected. The README's own gap table now carries colour/HDR, which is
  where a reader would look for it

#### Seven `todo.md` boxes were ticked for work already shipped
- Acquisition (both digests, timestamps, filesystem metadata), near-duplicate
  detection, scene-change analysis, report generation, the CLI, batch mode, the
  Finding model, and the reproducibility verification were all implemented and
  all still unchecked
- A checklist that under-reports is as misleading as a README that over-reports,
  and it costs more: it hides the work from anyone planning the next phase. Two
  entries are now `[~]` partial rather than falsely `[x]` or falsely `[ ]` —
  §14 video analysis is done except colour, and §55–56 is done for bounded
  memory but has no workers, cancellation or progress
- The reasons are recorded inline against each box rather than left to be
  rediscovered, so the next reader learns *why* something is partial

#### A rule that was wired, declared, documented — and unfireable
- `VIDEO.BITRATE_DROP` passed every guard this project has. `required_inputs` was
  declared, the stage was called, `BundleInput::Bitrate` was populated, and the
  stage guard confirmed it. It still could not fire on any file
- Cause: every fixture gave every sample the same 100 bytes, so the whole corpus
  held one flat bitrate. This is the same defect `repeated_frames` was added for,
  one analysis layer up
- The stage guard cannot see this class. It asks whether an input is *populated*,
  and a report describing a bitrate that never varies is perfectly populated.
  Populated is not the same as *interesting*
- Added `the_corpus_contains_a_file_the_bitrate_rule_can_fire_on`, which runs the
  pipeline over every fixture and asserts at least one makes the rule fire. It is
  the general shape of the missing check: not "is this input reachable" but "can
  this rule ever say anything"

#### `stsz` declared every sample the same size regardless of its contents
- The bitrate fixture was built by shrinking sample payloads, and the rule still
  found nothing. The `stsz` box wrote `SAMPLE_BYTES` for all entries — the size
  the payload *usually* had, not the size it *had*
- So the container declared a perfectly uniform bitrate while its `mdat` varied.
  The analysis is correct; it was reading a table that lied
- Invisible to every existing test, because nothing had read sample *sizes* before
  — duplicate detection reads digests, GOP analysis reads timestamps, and both are
  blind to a wrong `stsz`. A fixture defect that only a new analysis layer can
  surface
- `stsz` now derives each entry from `sample_payload(index).len()`. Worth noting
  what this class of defect is: the fixture corpus had been encoding a
  *contradiction* between two boxes, and only one consumer of those boxes could
  tell

#### `total_bytes` used `sum()` and panicked on hostile input
- `samples.iter().map(|s| s.size).sum()` overflows on four `u64::MAX` entries:
  panic in debug, silent wrap in release
- Caught by `absurd_sample_sizes_do_not_panic`. Sample sizes come from an
  attacker-controlled table, so this is the direction that matters — and a wrapped
  total would report a small, entirely plausible bitrate for an enormous file,
  which is the worst way for this particular number to be wrong
- Now `saturating_add`. The same test asserts only that the call survives, because
  pinning the exact saturated value would only encode the current implementation

#### Two types derived `Eq` over `f64`
- `BitrateReport` and `BitrateAnomaly` both derived `PartialEq, Eq` while holding
  bitrate measurements as `f64`. `Eq` on floats is not a relation to rely on, and
  a future `Hash` or `Ord` derive would have produced silently wrong ordering
- Caught by the compiler, not by review — the first sign that deriving `Eq` over a
  measurement is a category error rather than a style choice

#### Fourteen rules have no fixture that triggers them
- The bitrate guard I added was one rule. Generalised it to every rule, and the
  first run reported **19 of 26 could not fire**. Two of those four extra were my
  test's fault: it filtered to ISO-BMFF, so the audio and Tier-2 rules were judged
  against fixtures that cannot possibly trigger them. Fixed, and the real number
  is 15
- All 15 are tested — `new_rules.rs` exercises each against a hand-assembled
  `AnalysisBundle`. So my first version of the test, which said they were
  "untestable" and should be "removed", was simply wrong, and the message would
  have been quoted back at whoever read it
- The real gap is narrower and worth stating precisely: those rules are tested
  against state the test *builds*, never state the pipeline *produces*. A
  hand-built bundle can drift from what the pipeline actually fills in, and the
  existing guards catch only the extreme case (a stage never called)
- Recorded as `NO_END_TO_END_FIXTURE` rather than fixed. Closing it is roughly
  fifteen fixtures, each built to trip one rule. The list exists so that cost is
  visible and so a *newly* inert rule fails instead of joining the list quietly
- The test asserts both directions. Adding a firing rule to the list fails with
  "now fire end to end"; removing its fixture fails with "not recorded". A gap
  list that silently keeps solved entries is worse than no gap list at all

#### The demuxer cannot tell you where a file stopped being trustworthy
- Spec §30 asks for corruption detection with graceful continuation. What shipped
  was `ContainerInspection::anomalies: Vec<String>`, populated by one check: an
  empty track list. A file truncated mid-`mdat` produced **no** anomaly at all
- The reason is structural. `tpt-kinetix-demux` stops when the bytes stop making
  sense and returns the tracks it managed to read as a success. That is correct
  for a player. For an examination it is the wrong answer, because the demuxer
  has by construction lost the boundary — it cannot report where it gave up
- So the scan is a shallow, independent walk of the top-level box list
  (`-container/src/damage.rs`). Shallow deliberately: a damaged file's inner
  structure is exactly what cannot be trusted, and a recursive descent is how a
  malformed file turns an examination into a crash (spec §75)
- Typed rather than free text. `StructuralDamage` carries the byte offsets and
  the declared-vs-available numbers, so "truncated" is distinguishable from "has
  unaccounted bytes" — a distinction a string cannot express and an examiner
  needs, because only the first means content is missing
- Two rules at deliberately different severities. `CONTAINER.TRUNCATED_MEDIA` is
  Critical/High: the media the file describes is absent, so every other
  measurement from it is partial by construction. `CONTAINER.STRUCTURAL_DEFECT` is
  Warning/Medium: appended data is a legitimate technique and does not mean
  content is missing. Grading both Critical would have diluted the one finding
  that genuinely is

#### The damage scan reports a box-size of 0 as damage in a valid file
- ISO-BMFF defines a size field of 0 as "this box runs to the end of the file",
  and 1 as "a 64-bit size follows the type field". Both are legal
- The first version treated 0 as a literal size, so any file using the
  end-of-file form was reported as defective. That is the dangerous direction for
  this guard: a scanner that flags valid files trains an examiner to ignore it
- Caught by `a_size_of_zero_means_extent_to_end_of_file_not_a_defect`, written
  specifically because the case is easy to get wrong and invisible otherwise

#### One defect was being reported as two
- After finding truncation, the walk left `offset` pointing at the shortfall, and
  the trailing-data check then reported those same bytes again as "unaccounted
  for"
- One defect, two findings, two severities — and severity counts are what a
  dashboard leads with, so the duplication was not cosmetic
- Fixed by consuming the remainder before breaking, with a comment saying why.
  Both branches that `break` now do it

#### A limitation that cannot be fixed, pinned so it is not silently relied on
- Appended data of 8 bytes or more is indistinguishable from a further box: a box
  is exactly a size followed by a type, and appended payloads frequently have that
  shape. The scan parses it as a box header and reports what the bytes say
- The alternative — treating leftover bytes as opaque — would hide genuine
  trailing boxes, which are themselves a forensic signal. A reader that silently
  discarded structure is worse than one that reports structure it may have
  over-read, because the former cannot be detected from the report
- `appended_data_long_enough_to_mimic_a_box_is_read_as_one` pins the behaviour so
  a change to it has to be deliberate

#### A doc comment described the opposite of what the code did
- `BundleInput::is_populated` claimed "an empty `Vec` counts as populated ... the
  stage ran and found nothing, which is a measurement". Every collection arm
  returned `!is_empty()`, which is the opposite
- The code was right and the comment was wrong: the guard asks whether a stage is
  *reachable*, and a collection nothing ever fills means no fixture exercises it.
  But a reader taking the comment at face value would conclude the guard could
  not detect an unwired collection stage, which is precisely what it is for
- Corrected, and the distinction between "populated" (guard reachability) and
  "complete" (the bundle carrying an empty vector meaning *scanned and clean*) is
  now spelled out

#### A byte offset is not a timecode, and the engine now says which it has
- Structural damage reports *where in the file* it was found. Spec §31 wants it on
  an error timeline, which means a media time — and the two are only connected
  through the sample table
- `SampleIndex` walks the samples already read for duplicate detection (no extra
  I/O) and resolves an offset to the sample containing it. `CONTAINER.TRUNCATED_MEDIA`
  then sets `timeline_start` to that sample's presentation time
- The placement carries its provenance. Offsets are **inferred** — an anchor plus
  accumulated sample sizes — not read from `stco`, and `SampleOrigin` records which.
  The finding says "offset inferred, not read from the chunk offset table" in its
  own text, because a precise-looking timecode is exactly what a reader will trust
- Where no sample index exists (file above the sampling bound), the finding carries a
  byte offset and **no** timecode. Filling in `00:00:00` would read as a measured
  position; `damage_without_a_sample_index_is_reported_without_a_timecode` pins that
- The `mdat` anchor is read from the file's own box layout rather than assumed.
  MP4 may place `mdat` before or after `moov`, and a constant anchor would shift
  every sample position by whatever precedes it

#### A test asserted the placement was non-zero, and was wrong
- `damage_is_placed_on_the_timeline_when_samples_were_read` asserted the resolved
  time was greater than zero "so the placement is not a fabricated zero"
- It failed against correct code. The first sample of every file *is* at time zero,
  and this fixture's damage falls inside that first sample's byte range
- The assertion was the bug: it encoded an intuition about what a good placement
  looks like rather than what makes one correct. Rewritten to assert the placement
  equals the sample the index actually located — which checks the wiring rather than
  a property of the data, and would still pass if the fixture changed

#### The anchor walk stops at `moov`, so `mdat`-before-`moov` files get no placement
- ISO-BMFF permits media data before the sample tables. The walk returns `None` at
  `moov` to avoid descending into `mdat` payload looking for another `mdat`, which
  means a file ordered that way gets an empty index and therefore byte offsets with
  no timecode
- Stated in `todo.md` rather than papered over. The alternative — continuing the
  walk and hoping the bytes it finds are structure — is how a reader interprets
  sample payload as boxes, which is the failure `NonPrintableBoxType` exists to
  report

#### The declared-versus-measured rule was returning nothing, always
- `METADATA.DECLARED_VS_MEASURED_MISMATCH` was registered, documented, listed in
  the rule inventory, and returned an empty vector. Twenty-three of twenty-three
  rules were reachable on paper; one of them could not fire on any input
- The cause was a modelling gap, not a missing `if`. The stream timing stored a
  single duration, so after the reader had compared `mdhd` against the sample
  table there was nothing left to report: the losing value had been discarded
- Declared and measured are now separate fields on `StreamTiming`. Declared comes
  from `mdhd`; measured is summed from every `stts` delta, computed independently
  so the two can genuinely disagree
- Reading `mdhd` also fixed a latent bug: the duration was divided down to whole
  seconds before conversion, silently discarding sub-second precision — which
  would have hidden exactly the small disagreements this rule exists to surface
- The finding states both numbers and their difference, and asserts no cause. A
  test asserts the text contains no accusatory vocabulary, so "it was tampered
  with" cannot creep back in through a reworded summary
- 100 ms of slack is allowed, because muxers round durations and a clean file is
  not guaranteed an exact match. That trades some sensitivity for no false
  alarms, which is stated at the constant rather than left for someone to
  discover as a tuning question

#### The guard now catches rules that under-declare, not just rules that under-wired
- `required_inputs` was checked in one direction only: that every declared input
  was reachable. A rule reading an analysis it had not declared was invisible.
  That is the same silent failure as an unwired stage, one level removed, and it
  was the last known hole in the guard
- `stage_guard.rs` now reads `builtin.rs` as source, strips comments and string
  literals, and compares each rule's `bundle.<field>` accesses against its
  declaration. Calling `evaluate` cannot do this — it cannot know what the rule
  would have read had the bundle been fuller, and Rust has no reflection to ask
- A second direction: every optional field on `AnalysisBundle` must have a
  matching `BundleInput`, so a new analysis cannot be added with no way to
  declare it. `asset_id` is exempt by construction — it is always present and
  identifies the asset rather than reporting on it
- Both were verified by breaking them: three under-declarations, including one
  declaring nothing at all, were reported in a single run; and a new bundle field
  with no variant was named. Neither check had been run against a mutation
  before, which is the only reason to believe they work
- The scanner asserts it found exactly one block per registered rule. Without
  that, a scanner that silently matched nothing would pass forever — the trap this
  file exists to avoid, appearing inside the guard against it

#### The README was claiming six things the engine does not do
- It listed a comparison engine, BLAKE3 alongside SHA-256, spectral audio
  analysis, and a corrupt-media corpus on disk. None existed. BLAKE3 is
  declared as a dependency and never called; there is no comparison module
  anywhere; the only mention of a spectrum is a doc comment; `fixtures/` and
  `rules/` are empty placeholder directories the README described as populated
- It also said "Opus and Vorbis are decoded" in the same sentence that named
  Matroska as analysed. Vorbis is decoded from bare Ogg streams only — inside
  a `.webm` it is identified and not measured. A reader would reasonably have
  concluded otherwise
- This was the project's own failure mode pointed the other way. The README
  says "a tool that quietly omits a measurement is worse than one that names the
  gap", and then quietly invented measurements. In forensic work a false
  capability claim is worse than a missing feature: the report asserts a check
  that never ran
- Correcting the prose leaves it free to drift back, so
  `-core/tests/readme_claims.rs` checks the claims against the code: a capability
  the engine lacks may not appear under "What it does", the shipped and planned
  lists may not overlap, and every codec the status paragraph calls decoded must
  satisfy `is_decodable`. Verified by reintroducing three false claims, all
  named in one run

#### The A/V fixture was three separate defects wearing one coat
- `build_mp4_av` spliced a second track into a finished single-track file. The
  audio track's `mdat` was never copied, so its samples resolved to the *video's*
  bytes; and growing `moov` moved the data the video's offset pointed at. Both
  tracks also declared `track_id` 1
- Every fix for this went in wrong at first, which is the real lesson. Locating
  `stco` by walking the bytes failed three different ways: a flat scan found
  nothing and returned `false` that the caller ignored; a recursive scan read the
  entry count from the wrong word; and a rewrite briefly left the file with
  `tkhd` and `mdia` sitting directly in `moov`, which parses as a file declaring
  no tracks and looks entirely plausible in a hex dump
- The builder no longer searches for anything. `build_trak` returns the position
  of the chunk-offset value *as it assembles the boxes*, so the offset cannot be
  mislocated, and one `mdat` holds every track's samples
- `tests/av_fixture.rs` checks both tracks declare distinct in-range offsets
  directly against the bytes, so the invariant holds even while the reader below
  is broken. That test also had to stop searching for the text `mdat`: the
  fixture's sample pattern happens to spell it, so a text search finds a box
  that does not exist

#### A demuxer that never returns is now a reported limitation
- The upstream MP4 demuxer cannot read sample data from a valid two-track file.
  It yields packets indefinitely, each with a plausible non-zero size, so no
  simple check stops it — an analysis of such a file simply never finishes
- `read_samples` now stops when a packet cannot advance the reader, and returns
  an error when reported sample bytes exceed the file's own length. That second
  test needs no tuning: a legitimate file cannot contain more sample bytes than
  it has bytes. Reporting rather than returning is the point — the fabricated
  samples it was producing would have reached the rules as evidence
- A report that never arrives is indistinguishable from a clean file, which is
  the one failure mode a forensic tool must not have

#### The fixtures were lying about their own media data
- Every synthetic MP4 wrote its `mdat` as a run of zero bytes. Every sample then
  hashed identically, so duplicate detection reported **every** fixture as a
  single 60-frame repeated run. A real detection, rendered indistinguishable
  from noise on healthy input, and `VIDEO.DUPLICATE_FRAME_RUN` had never once
  been exercised against a file that was meant to trigger it
- Worse, the chunk offset table said `0`, so samples were read from the start of
  the file — the demuxer was digesting the `ftyp` and `moov` bytes *as if they
  were video frames*. Any test reasoning about sample content was reasoning
  about the container. The chunk offset now points at the real first sample
- Each sample gets its own payload, seeded so distinct samples cannot collide,
  and `TrackSpec::repeated_frames` builds a file with a genuinely frozen
  stretch. `build_mp4_with_repeated_frames(10, 15)` is that fixture
- A clean fixture reporting zero duplicate frames is now a regression test. So
  is a frozen run reporting its true length of 15 — the clean case alone would
  pass with duplicate detection switched off entirely
- `KNOWN_UNREACHABLE` is now empty. It last held `RepeatedRuns`, on the belief
  that no fixture produced a repeated run; the fixtures were not lacking, they
  were producing one each by accident. A documented-exceptions list can hide a
  defect as easily as it records a limitation
- One test was passing for the wrong reason and is now honest. It compared
  finding *counts* between two fixtures raising unrelated findings, so the
  totals tracked whatever else each happened to report. It asserts the GOP rule
  fires on one and not the other

#### A guard against analysis stages that are written but never called
- Two stages shipped fully implemented, documented, and unit-tested, and that no
  test could catch: `measure_audio` and `av_sync::analyse`. Nothing invoked
  either, so six rules could not fire on any file, over any number of green
  tests. The analyser was healthy and its caller was missing, which is exactly
  what a unit test cannot see
- `ForensicRule::required_inputs` is now mandatory, with no default. Every rule
  declares the analysis it reads, so "which stage does this rule depend on?" is
  answered in code rather than inferred from reading `evaluate`
- `BundleInput` carries `is_populated`, and `AnalysisEngine::observe_stages`
  exposes the bundle without the cache or persistence, so a test can ask *which
  analyses ran* — which findings cannot distinguish from *found nothing*
- `-core/tests/stage_guard.rs` runs the pipeline over a corpus that between them
  exercises every stage, and fails naming the affected rules when a declared
  input is never populated. Verified by unwiring the audio and A/V stages: it
  caught both, naming all five dependent rules

#### A/V synchronisation now runs, too (spec §23)
- **A second dead stage.** `av_sync::analyse` is fully implemented, documented,
  and tested — and nothing in the pipeline ever called it, so `bundle.sync` stayed
  `None` and `TIMING.AV_SYNC_DRIFT` could not fire on any file. Same shape as the
  audio gap: the unit under test was healthy and its caller was missing
- `-core::run_av_sync` locates both tracks **by kind**. Taking the first two
  streams would compare audio against video for most files and audio against
  audio for the rest, and the second case yields a confident, meaningless zero
  offset
- A single-stream file raises no limitation at all: sync was never applicable,
  which is a different statement from having been attempted and failed
- `build_mp4_av` is a new two-track fixture, composed by splicing a second `trak`
  into a single-track file rather than teaching the existing builder about
  multiple tracks. Its audio delay is expressed through a real `elst` edit list,
  because without one both tracks start at zero and there is nothing to measure —
  a fixture that would have passed vacuously

#### The pipeline now decodes audio, which it never did
- **Four rules were permanently dead.** `AUDIO.CLIPPING`, `AUDIO.DC_OFFSET`,
  `AUDIO.SILENCE_REGION`, and `AUDIO.INAUDIBLE` read `AnalysisBundle`'s
  `audio_levels`, `silence`, and `loudness`. A `measure_audio` helper existed
  and was **never called**, so those fields stayed `None` and every audio rule
  returned early on every file. `analyze` on any media produced zero audio
  findings, whatever the codec
- Each rule had tests — all of which built the `AnalysisBundle` by hand. Nothing
  exercised the missing step, so the suite was green over a feature that did
  not run. `-core/tests/audio_pipeline.rs` now drives the real engine over a
  real Opus-in-WebM file built with the foundation's own encoder
- `measure_audio` now also computes silence regions, from the same PCM buffer as
  the levels, and refuses a zero-channel stream rather than reporting zeroes

#### Fixed: three container-blind measurements
Writing the audio stage surfaced a family of bugs, all the same shape: a
**video** measurement applied to whichever stream happened to be first
- GOP structure and PTS scanning read `frame_info[0]` regardless of kind, so an
  audio-only file got GOP analysis and `VIDEO.SINGLE_KEYFRAME` fired on it
- Duplicate detection hashed samples from *every* stream into one flattened
  sequence, so a run of identical silence packets in an audio track produced
  `VIDEO.DUPLICATE_FRAME_RUN` — a video finding derived entirely from audio
- Both now select the video stream explicitly. An audio-only file makes no video
  claims at all, which `a_video_only_file_makes_no_audio_claims` and the silence
  test assert from opposite directions

#### Fixed: Opus inside a WebM container could not be decoded
- A `.webm` file is Matroska, not Ogg. The audio stage handed the whole
  container to an Ogg reader, which failed on the capture pattern — the two
  formats share a lineage and nothing else
- `-audio::decode_opus_packets` decodes demuxed access units directly, which is
  the only correct path for container-carried Opus. Vorbis in Matroska is still
  unimplemented and says so

#### Tier-2 verified end to end on a real AV1 file (spec 16-18)
- `-video/tests/tier2_end_to_end.rs`: 7 tests that encode genuine AV1 with the
  foundation's rav1e-backed encoder, wrap it in a real WebM container, read it
  back through `tpt-kinetix-demux`, and decode it to pixels. No ffmpeg required
  on the build machine
- This closed the gap the earlier work left open. Every previous Tier-2 test fed
  synthetic frames straight to the analysers, which proves the analysers work
  but proves nothing about whether a compressed stream survives the trip. The
  decoder integration had only ever been unit-tested at its own adapters
- `a_real_av1_webm_file_survives_the_whole_pipeline` covers the three seams no
  other test crossed: container writer against demuxer, demuxer sample bytes
  against decoder, and demuxer codec tag against Tier-2 dispatch
- **VP9 remains structurally tested only.** The foundation ships no VP9 encoder,
  so no genuine VP9 stream can be produced on this machine. That asymmetry is
  stated in the test file and in `todo.md` rather than left to look like parity

#### Fixed: the WebM fixture builder corrupted any real-sized payload
- `build_webm` wrote every element size as a single byte, `0x80 | len`. That is
  valid only below 127. At 128 it emits `0x80`, which a reader decodes as
  *unknown size* and therefore swallows the rest of the file
- Stub fixtures never reached the threshold, so only real encoded video exposed
  it — and when it did, the file parsed as an empty track with no error anywhere.
  A fixture that silently stops exercising the demuxer is worse than no fixture
- Replaced with a correct EBML variable-length integer encoder that picks the
  narrowest width and avoids the all-ones "unknown size" value at each width
- Two regression tests: payloads straddling every width boundary, and a direct
  round trip of the encoder across those boundaries

#### Fixed: `DecodedFrame` mixed `u32` and `usize` for its two dimensions
- `width` was `u32` and `height` was `usize`, while every other type in the
  codebase (`VideoFormat`, the Kinetix `VideoFrame`) uses `u32` for both. Every
  caller had to cast one of the two, and the analyser code was visibly
  juggling `width as usize` against a bare `height` — easy to mix up a width for
  a height in an index computation
- Both are now `u32`. `luma_at` computes its stride once instead of re-casting
  on every access

#### Matroska / WebM container support (spec 12, 13, 24)
- `-container::mkv`: a new inspection path beside `mp4.rs`, wrapping
  `tpt-kinetix-demux::mkv`. Wired into the pipeline and the CLI, so a WebM file
  is analysed rather than reported as an unintegrated format
- **The reader exposes keyframes, timestamps, and sample bytes — and nothing
  else.** No picture geometry, no frame rate, no sample rate. Those are reported
  as unmeasured (`video: None`, `sample_rate: 0`) rather than defaulted. A
  placeholder resolution in a forensic report is worse than an admitted gap
- Keyframe flags can be under-counted: plain `Block` elements are always
  reported as non-key by the reader, so a reference-block file shows fewer
  keyframes than it has. Recorded as an anomaly rather than as a finding about
  the file, because it is an artefact of this reader
- Timestamps are millisecond-resolution, coarser than an MP4 `mdhd` timescale.
  The timebase is recorded so a comparison against an MP4 of the same content
  does not mistake the resolution for a timing discrepancy
- `Mp4Inspection` became `ContainerInspection`. The old name became a lie the
  moment WebM landed: labelling a Matroska file's own inspection result "MP4"
  would assert a container format the file demonstrably is not
- Sample bytes are read once and reused. Tier-2 previously re-read the file
  that duplicate detection had just parsed; it now decodes from the same bytes
- 27 MKV tests plus 4 pipeline and 3 CLI tests, covering determinism, truncated
  prefixes, and a real end-to-end WebM analysis

#### Royalty-free audio decode: Opus and Vorbis (spec 19, 21)
- `-audio::decode`: Opus and Vorbis decode through the foundation's encoders and
  decoders. AAC is deliberately absent, mirroring the video-side rule — a
  patent encumbrance is a reason not to ship the decoder, not a reason to
  pretend the audio was measured
- The refusal is worded as a policy, not a failure: "we do not decode this" and
  "we could not decode this" are different statements and only one is true here
- Channel count and sample rate always come from the stream's own headers, never
  from the container's description or the caller's guess. A container that
  disagrees with its own bitstream is a finding, not something to paper over
- Decode is bounded, and hitting the bound sets `truncated`. Silently returning a
  prefix would let a report describe a fragment as the track
- The CLI `audio` command detects the codec from the file's bytes, not its
  extension
- 8 round-trip tests encode real streams with the foundation's own encoders and
  decode them back. Found a genuine property along the way: **lossy Opus decode
  overshoots full scale** (peak 1.0166 for a full-scale tone), which is why the
  peak assertion is bounded rather than `<= 1.0`

#### Documentation corrections
- `docs/rules.md` said twenty-one rules; `builtin_rules()` registers twenty-three.
  The list is now complete and a test asserts it matches the registered set, so
  the document cannot drift from the code again
- `docs/foundation.md` listed VP9, AV1, and the audio codecs as "not yet
  integrated". They are integrated; the section now records what the Matroska
  reader does and does not expose
#### Royalty-free decoders only
- Dropped the H.264 decoder (`tpt-kinetix-h264`, now `out-kinetix-h264` and unpublished upstream)
  because H.264 encode and decode are patent-encumbered. Tier-2 pixel analysis now decodes
  VP9 (`tpt-kinetix-vp9`) and AV1 (`tpt-kinetix-av1`), both reported `pixel_exact` upstream.
- `-video::decode`: `DecodeSession` dispatches by codec tag. `is_h264` became `is_decodable`,
  and `DecodeSession::capabilities` takes a codec. Only 8-bit planar frames are used.
- Removed the stderr-capture shim, which existed only for H.264's `PPS_PARSE_ERR` noise.
- H.264, HEVC and AAC tracks are still identified and get Tier-1 analysis. They are never decoded.
- Moved the `tpt-kinetix` pin to 28cefd8 and the `tpt-cadence` pin to 95ff6bf.
  `AnalysisVersion::CURRENT` is now 2, so results cached before the change are not reused.

#### Phase 1 - report generation and case persistence (spec 59-63, 66)
#### Phase 1 - batch analysis (spec 48-49)
#### Phase 1 - Tier-2 pixel analysis (spec 16-18)
- `tpt-kinetix-h264` integrated as the decoder. It is not reimplemented: it is
  already bit-exact against ffmpeg, and re-deriving it would be worse and slower
- `-video::decode`: the decoder adapter. The analysis that produces findings
  never sees a decoder, so both analysers are unit-testable on synthetic frames
  - Pixel-exactness gates everything. `DecodeSession` refuses to open unless the
    decoder reports `pixel_exact`, because Tier-2 measurements computed on
    approximate frames would describe the decoder rather than the media. Withheld
    is the correct outcome; a plausible wrong number is not
  - Decoding is bounded by `DecodeLimits`, and hitting a bound is reported
    rather than returning a partial result that reads as complete
- `-video::scene`: scene-change analysis from mean absolute luma difference
  between consecutive decoded frames. The threshold is a profile value, not a
  constant, because what counts as a cut depends on the content
- `-video::near_duplicate`: perceptual hashing on decoded frames, which finds
  the case packet-layer detection cannot: the same picture re-encoded, so the
  bytes differ and the image does not. Block means are compared against the
  image's own mean, so the hash measures structure rather than brightness
- Two rules: `VIDEO.SCENE_CHANGE` and `VIDEO.NEAR_DUPLICATE_FRAME`. Both carry
  `Medium` and `Low` confidence respectively and never above: a perceptual hash
  collides on two different shots of one scene, so only a reviewer with the
  pictures can separate that from reuse
- **A decoder abort is contained rather than propagated.** Some Kinetix parse
  paths attach an `anyhow::Context` to an error, which captures a backtrace, and
  the capture path aborts the process. A malformed PPS in a deliberately damaged
  file therefore ended the whole examination - the opposite of what spec 75
  requires of hostile input. The unwind is now caught, the frame is lost, the
  session continues, and the reason is recorded in the case's limitations
- Known cosmetic limitation: on Windows the decoder's own `PPS_PARSE_ERR`
  diagnostics still reach stderr. Redirecting the standard error handle needs
  `unsafe`, which this crate forbids, and a stray debug line is not worth an
  `unsafe` block. The examination completes with a success exit status either
  way, and the measurement outcome is recorded in the report
- 12 Tier-2 tests over synthetic frames, plus 5 decoder-adapter tests
#### Phase 1 - partial reads: inspection no longer loads whole files (spec 12)
- `-container::read_moov` walks the top-level box headers and loads only the
  `moov` box, leaving the media data on disk
  - Inspection needs the sample tables, codec descriptions, and timing, which
    all live in `moov`. `mdat` holds the encoded media, which on a long
    recording is nearly the whole file and which no structural check reads
  - Measured on a 600 MB file: 1 KB read, 0.0002% of the file, 168 microseconds
  - Handles the three size encodings the format allows: 32-bit, 64-bit extended
    (`size == 1`), and zero meaning "to end of file"
- `inspect_path` places no limit on file size, only on the `moov` box
  (`MAX_MOOV_BYTES`, 256 MiB). The previous 2 GiB whole-file ceiling no longer
  bounds what can be inspected, so a long master is now analysable
- The pipeline reads a bounded 64 KiB header for format detection rather than
  the whole file, and previously cloned the file buffer twice
- Sample-level duplicate detection genuinely needs every sample's encoded
  bytes, so it keeps its own bound (`MAX_SAMPLED_BYTES`). On a file above it,
  duplicate detection is skipped and the gap is recorded in the report's
  limitations - structural analysis still covers the file
- The `moov` size bound is checked before the file-extent check, so an
  oversized `moov` reports that it exceeds the limit rather than being
  misreported as a malformed file. The two are different findings
- 15 tests over partial reads: large-`mdat` files, missing `moov`, truncated
  final boxes, sizes past end-of-file, zero-size boxes, a `moov` that is not the
  second box, oversized `moov`, determinism, and bounded header reads
#### Phase 1 - the complete built-in rule set (spec 35-37)
- Nine rules added, completing the planned twenty. Each reads fields the
  analysis already produces, so none of them adds cost at analysis time
  - `CONTAINER.PARSE_ANOMALY` surfaces what the parser had to tolerate. Those
    are the places the file's structure departed from the specification, and
    where any downstream measurement is least certain
  - `CONTAINER.DECLARED_TRACK_MISMATCH` compares the declared `trak` count with
    the streams actually recovered
  - `CONTAINER.STREAM_DURATION_MISSING` and `CONTAINER.STREAM_START_OFFSET`
    report absent durations and non-zero starts, both of which change what
    "the beginning" means for a stream
  - `VIDEO.ALL_FRAMES_KEYFRAMES` reports a track that declares no `stss` box
  - `VIDEO.SINGLE_KEYFRAME` reports a track with a single sync sample, where
    seeking is approximate throughout
  - `VIDEO.FRAME_RATE_CHANGE` reports frame durations departing from the
    track's dominant duration
  - `AUDIO.INAUDIBLE` reports loudness at or below the profile threshold
  - `METADATA.MISSING_CREATION_TIME` reports metadata with no creation-time
    field. It deliberately stays silent when there is no metadata at all, which
    is a separate observation already reported elsewhere
- Severity and confidence are set per rule by how much the condition actually
  establishes. A structural fact read straight from the box structure is High;
  a single differing frame duration is Medium, because a timestamp rounding
  artefact would look the same
- 21 rule tests. Every rule is checked twice, once against a fixture that
  should trip it and once against a fixture that should not: a rule that only
  ever fires is as useless as one that never does, and the negative case is
  what catches a comparison that is too permissive
- `-core::batch`: analyses every media file beneath a directory into one case,
  which is the shape of real intake work, where a handover arrives as a folder
  - One failure does not stop the batch. Each file is analysed independently and
    a failure is recorded against that file with its path, so a single corrupt
    item cannot hide the results for the other thousand (spec 75)
  - The case directory is excluded from the scan, or a batch would read its own
    evidence and outputs on the next run
  - Files are visited in sorted path order, so two runs over an unchanged tree
    produce the same sequence of results (spec 77)
  - Files are selected by extension as a cheap filter, then confirmed by
    signature before analysis. An extension is a claim; the leading bytes are
    evidence (spec 12)
- CLI `batch` runs it, reports each file individually, writes the case-wide
  bundle, and exits 2 if any file could not be analysed so a scripted caller
  fails loudly rather than reading a short count as success
- 10 core tests and 5 CLI tests covering nested traversal, case-directory
  exclusion, cache reuse across a repeat batch, deterministic ordering,
  path-labelled failures, empty directories, and source immutability
- `-report::pdf`: PDF rendering written directly, with no new dependency
  - Emits PDF 1.4 with the base-14 Helvetica font, so nothing is embedded and
    the output stays byte-deterministic (spec 77) - a PDF library with its own
    metadata or timestamp defaults would break the evidence manifest hash
  - Text is drawn as positioned `Tj` operators: no shaping, no kerning, and no
    non-Latin-1 glyphs. A character outside Latin-1 becomes `?` rather than
    being dropped, because silently omitting text would misrepresent a finding
  - The disclaimer is appended inside `to_pdf` itself, so no invocation can
    produce a PDF without it (spec 59)
  - 14 tests covering xref byte offsets, `startxref`, stream lengths,
    pagination, string balancing under hostile metadata, and determinism
- CLI `report` renders JSON, HTML, CSV, PDF, and a self-verifying bundle,
  inferred from the output extension. It reads stored findings and never
  re-analyses, so a report states what was observed at the time.
- The evidence bundle now includes `case-report.pdf` alongside the JSON, HTML,
  and CSV deliverables (spec 62)
- Case persistence: schema v2
  - `findings.payload` stores each finding as canonical JSON. The typed columns
    cannot reconstruct a `Finding` - stream scope, evidence references, and
    reviewer state have no column - so the payload is what `report` reads back
  - `analyses` records the profile, profile fingerprint, rule-set fingerprint,
    and start time, which is what lets a rebuilt report state the same
    analysis fingerprint the original run did (spec 63)
  - **Findings are keyed by `(analysis_id, id)`, not `id` alone.** A `FindingId`
    is derived from the observation's content, so re-analysing the same file
    under a different profile produces the same id. Keying on id alone made a
    second analysis fail on a primary-key collision, which contradicts
    spec 66: re-analysing records new observations, it never edits old ones.
  - `finding_reviews` carries `analysis_id` so a review always resolves to one
    specific run's finding
- 5 CLI tests for `report`: each format, findings carried through, determinism
  across two renders, refusal of an unknown format, and the bundle manifestented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

#### Phase 0 — repository and foundation setup
- Cargo workspace with twelve crates per spec §8: `model`, `core`,
  `container`, `video`, `audio`, `timing`, `metadata`, `evidence`, `rules`,
  `report`, `cli`, `tauri`
- Dual MIT / Apache-2.0 licensing (copyright TPT Solutions)
- `-model`: the domain vocabulary the whole engine shares
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
  - `MediaTime`, `Timebase`, and `Rational` (spec §24) using exact integer and
    rational arithmetic with saturating conversion
  - Content-derived, deterministic identifiers (spec §77)
  - `CacheKey` and analysis fingerprints (spec §54, §63)
- CLI with `hash`, `acquire`, `audio`, `inspect`, `analyze`, `report`, and
  `batch` subcommands
- Tauri desktop shell crate as a separate workspace (spec §78)
- Documentation: architecture, evidence model, analysis model, findings,
  report format, rules, foundation integration, decoding tiers

#### Phase 1 — acquisition and case storage (spec §11, §53, §58)
- `-core::acquisition`: single-pass SHA-256 + BLAKE3 hashing with bounded
  memory (fixed 1 MiB buffer, so a 4 GB asset costs the same as a 4 KB one)
- Sources are opened read-only; the guarantee is asserted by an end-to-end CLI
  test that compares file bytes before and after
- Detects a source that changed size while being hashed and refuses to record a
  digest of a moving target
- Captures filesystem timestamps and platform metadata, leaving unavailable
  values `None` rather than substituting a guess
- `-core::case_dir`: case directory layout, versioned JSON manifest written via
  atomic temp-file rename, refusal to overwrite an existing case, and cache
  clearing scoped so it can never remove the record

#### Phase 1 — container inspection (spec §12, §13)
- TPT foundation crates wired as pinned-rev git dependencies; the crate names
  in the spec are repository names, and the real crates are sub-crates within
  them (documented in `docs/foundation.md`)
- `-container::probe`: format detection by file signature, not by extension,
  plus extension-mismatch reporting
- `-container::mp4`: MP4/ISO-BMFF inspection over `tpt-kinetix-demux`, mapping
  Kinetix tracks to `StreamAnalysis`. Frame rate is *measured* from the `stts`
  timing table, not copied from a declared value
- Variable-cadence tracks report no single frame rate rather than being
  smoothed into a misleading number
- Whole-file loading capped at 2 GiB; larger files are refused with an explicit
  error instead of risking an allocation failure
- `-container::fixture`: deterministic synthetic MP4 generator, which also
  serves the corrupt-media corpus (spec §76) without an external encoder

#### Phase 1 — timestamp forensics and A/V sync (spec §23, §24)
- `-timing::pts_dts`: PTS/DTS scanning for non-monotonic timestamps, gaps,
  overlaps, and negative (pre-roll) values. Expected frame duration is the
  *modal* inter-sample delta, so a track that is mostly regular with one long
  gap is not misread, and ordinary variable-frame-rate variation is absorbed by
  a profile-supplied tolerance rather than flagged as a defect
- `-timing::av_sync`: initial offset, final offset, and drift measured over the
  span it applies to
- Offset is the **median** of per-frame residuals, not the mean: when the true
  offset is not a whole frame interval the residuals alternate between roughly
  `-offset` and `frame - offset`, and the mean of that alternation is biased

#### Phase 1 — GOP structure (spec §15)
- `-video::gop`: GOP structure from the container's `stts` and `stss` boxes,
  with **no decoding required** — Tier-1 analysis that survives a decoder
  failure (see `docs/decoding-tiers.md`)
- GOP changes are reported as *transitions* against the preceding GOP, not as
  every GOP deviating from a global mode. One structural event produces one
  finding, in the direction it actually occurred
- `track_frame_info` expands the run-length `stts` table into per-frame
  timestamps, bounded by `MAX_EXPANDED_SAMPLES` so a hostile sample table
  cannot drive unbounded allocation

#### Phase 1 — duplicate detection (spec §16, §17)
- `-video::duplicate`: exact-duplicate detection at the packet layer by hashing
  compressed sample bytes — no decoding required
- Findings carry an explicit `Soundness`: `PixelIdentical` when every sample in
  the run is a keyframe (I-frames are self-contained, so identical bitstreams
  prove identical pictures), `CompressedMatchOnly` when a predicted frame is
  involved (identical bytes do not prove identical output, because reference
  state is unknown)
- `-container::read_samples`: per-sample digests, timing, and keyframe flags via
  the Kinetix demuxer, tolerating truncation rather than failing

#### Phase 1 — audio analysis (spec §19–22)
- `-audio::measurement`: every measurement is a `Measurement` carrying a
  `Methodology`, so spec §21's "never report a number without identifying the
  methodology" is enforced by the type system rather than by convention
- `level_stats` (peak, RMS, DC offset) uses Kahan compensated summation: a naive
  accumulator drifts enough over a long signal to shift the reported DC offset
- `find_silence` requires the *peak* within a region to stay under the
  threshold, so a fade is not misreported as a silent passage
- `-audio::loudness`: ITU-R BS.1770-4 integrated loudness with K-weighting and
  both gates, verified against the standard's -3.01 LUFS reference for a
  full-scale 1 kHz sine
- Loudness reports **no figure** at sample rates BS.1770-4 does not define (it
  publishes coefficients only for 48 kHz) rather than producing a
  plausible-looking number from the wrong filter
- New CLI `audio` command decoding WAV via `tpt-av-cadence`

#### Phase 1 — metadata extraction and consistency (spec §25, §26)
- `-metadata::tree`: every entry retains the scope and container element it
  came from, because a conflict is only visible when both competing values
  survive. Entries are sorted deterministically (spec §77)
- `-metadata::consistency`: reports keys whose values disagree *between
  scopes*, naming each value and its source, and asserting nothing about why
  they disagree (spec §26)
- Two tracks disagreeing within one scope is not a conflict: they are not
  competing versions of one fact
- New CLI `metadata` command
- Metadata atom names render as `u+a9cmt` rather than the raw copyright byte,
  which was illegible in a terminal

#### Phase 1 — SQLite persistence (spec §52)
- `-core::store`: case database holding cases, assets, analyses, streams,
  findings, evidence, rule results, reports, and analyst notes
- Versioned schema via `PRAGMA user_version`; migrations are ordered and
  never rewritten. A case written by a newer build is refused rather than
  silently misread
- Foreign keys are enforced per connection (`PRAGMA foreign_keys = ON`),
  which SQLite does not do by default. Without it a finding could reference
  an analysis that does not exist and the database would accept it, producing
  a report that cites evidence from nowhere
- Findings and evidence are insert-only. A reviewer's verdict lives in
  `finding_reviews`, keyed to the finding, so accepting or rejecting a finding
  never edits the observation it judges (spec §66)
- Observed values are stored verbatim. A digest the case "helpfully"
  upper-cased would no longer match what the file reports
- `UNIQUE (case_id, sha256)` makes a duplicate import of the same file into
  one case impossible, while permitting the same file across two cases where
  comparison is legitimate
- Write-ahead logging and `synchronous = FULL`: the record must survive a
  crash mid-write
- `rusqlite` with the `bundled` feature, so no system SQLite is required

#### Phase 1 — rule engine and the built-in rule set (spec §35–37)
- `-rules::engine`: a `ForensicRule` trait over an `AnalysisBundle`. Rules are
  pure functions over analysis results: they never open a file, run a decoder,
  or write anything, so they are testable with synthetic results and no media
- `what_it_checks` and `why_it_matters` are trait methods rather than doc
  comments, because spec §71 requires every finding to explain itself. A rule
  that cannot state why its condition matters should not exist
- Rules are registered and evaluated in sorted rule-ID order, and findings are
  sorted by severity, rule ID, then timeline position, so two runs produce
  identical output (spec §77)
- `-rules::profile`: every tolerance lives in a versioned `RuleProfile`, never
  in a rule. Changing a threshold changes the profile fingerprint, which
  invalidates the analysis cache (spec §54) and is named in the report (§63)
- Twelve built-in rules across container, video, audio, timing, and metadata
- Finding IDs are content-derived from rule, asset, and location, so re-analysing
  a file yields comparable IDs rather than renumbered ones
- Duplicate-run findings take their confidence from the soundness the detection
  layer established: a provable claim outranks an assumed one
- No rule asserts a cause. A test asserts rendered findings contain none of
  "tamper", "edit", "forged", or "manipulat" (spec §15, §26)

### Fixed

- Every stream was reported with index 0, which would mis-associate frame data
  in multi-track files
- `stss` stores 1-based sample numbers and was being read as 0-based, placing
  every keyframe one frame late and shifting every GOP boundary. Regression
  tests assert the packet path and the box path agree on keyframe positions
- `StreamKind` and `HashAlgorithm` serialised as Rust variant names
  (`"Audio"`) rather than stable lowercase tags in machine-readable output
- A report printed a peak amplitude of `1.0` as `"1.0 dBFS"`; full scale is
  0 dBFS, so peak level and peak amplitude are now distinct measurements
- The CLI reported "not implemented yet" for a file that was not a recognised
  container, conflating a gap in the tool with a finding about the evidence
- An unrecognised `fourcc` containing control bytes rendered as blank space in
  the terminal; now shown as `\xNN`
- Kahan summation double-counted each sample, inflating RMS and breaking
  DC-offset detection

### Notes

- TPT foundation crates are consumed as **pinned-rev git dependencies** (the
  ecosystem convention; none are on crates.io). Branches are never used: a
  moving dependency would break the reproducibility guarantee in spec §77.
  Updating a pin requires reviewing `AnalysisVersion::CURRENT`.
- `tpt-av-sync` is a CRDT collaboration engine with no A/V measurement
  capability, confirmed by source search across all six foundation
  repositories. Spec §23 is therefore implemented directly in `-timing`.
  See `docs/foundation.md`.
- `tpt-visual` requires a GPU, so it is not a dependency of the analysis path.
- Decoding is **integrated, never implemented**: the VP9 and AV1 decoders are
  already bit-exact against ffmpeg. (H.264 was dropped as patent-encumbered.)
  The next sentence applies to them: Its `pixel_exact` capability is honoured —
  Tier-2 measurements are withheld rather than computed on approximate frames.
- When a pinned `rev` moves, `AnalysisVersion::CURRENT` must be reviewed and
  bumped if any analysis result could change, so cached results are never
  served against a changed decoder.