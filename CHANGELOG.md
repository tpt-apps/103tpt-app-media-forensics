# Changelog

All notable changes to this project are documented in this file, following
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
[Semantic Versioning](https://semver.org/).

#### Analyst notes reach the report — and the schema version says so
- Closes the gap noted when notes landed: `Report` now carries `notes`, and both
  the HTML and PDF renderers show them. A report that omitted a reviewer's
  conclusions would show the engine's observations with none of the human
  judgement layered on top — that misrepresents the record rather than
  abbreviating it
- **The schema version was a literal `1` at four construction sites.** It is now
  `REPORT_SCHEMA_VERSION`, bumped to `2`. Adding a field without bumping it lets a
  consumer read a report it cannot fully understand and treat the missing field as
  "nothing was recorded" rather than "this build did not know about it"
- `notes` is `#[serde(default)]`, pinned by a test that parses a hand-written
  version-1 report. Without the default, adding the field would have made every
  previously-generated report unreadable
- **A note body is escaped like any other analyst-supplied text** — pinned by a
  test feeding `<script>` through. The PDF pushes bodies line by line so the
  analyst's own paragraph breaks survive; a single string would be reflowed into
  one paragraph and lose where they intended a break. HTML uses `white-space:
  pre-wrap` for the same reason
- A note naming half its subject (only reachable from a hand-edited or older file,
  since `add_note` refuses those) is labelled "subject not identified" rather than
  silently presented as case-level
- `notes_for` in the CLI returns empty rather than erroring when the database cannot
  be read: `analyse` is mid-write to that database, and failing to produce a report
  because a note lookup failed is worse than producing one without notes
- `load_report` reads notes from the store, so re-running `analyse` on an annotated
  case produces a report that still carries the analyst's conclusions

#### Analyst notes: a note with half a subject names nothing
- §65 done. `Store::add_note`, `notes_on`, `notes_in_case`, and `StoredNote` in
  `-core/src/store/mod.rs`. The `notes` table and its index existed since the base
  schema and had **no API at all** — the feature was unreachable
- **A note names both the kind and the id of its subject, or neither.** A note with
  `subject_kind` but no `subject_id` names a subject ambiguously, and storing it
  would let a later reader attach the analyst's conclusion to the wrong thing. That
  pairing is rejected rather than stored. Case-level notes are legitimate ("the
  client disputes the timestamp"), so `None`/`None` is allowed
- **A note body is stored byte for byte.** A note is evidence of what the analyst
  concluded; reflowing their prose or trimming whitespace would alter the record.
  Pinned by a test using a body with CRLF, a blank line, a tab, and trailing
  spaces
- `notes_in_case` includes both attached and case-level notes, so a report showing
  "what the analyst said about this case" does not silently drop the observations
  that were not attached to one subject
- Ordered by row id rather than timestamp: two notes written in the same second
  still come back in write order
- `created_at` is supplied rather than read from the clock. My first draft of the
  doc comment claimed the engine recorded the time "because a caller-chosen time
  would let a note predate the analysis" — that contradicts the signature, and the
  argument was wrong: the store has no notion of "now", asserting one would make two
  identical runs differ, and the note body is the analyst's own text

#### Three more bugs, all found by writing tests for untested functions
- Last session's review bug made me check every store method for coverage. Seven had
  none. Writing tests for them turned up **three more real bugs** — so the pattern
  generalises: a `pub fn` with no test is an unknown, not an assumed-correct one
- **`latest_analysis` ordered by `id`, not time.** An analysis id is derived from
  asset content plus cache key, so it carries no chronological meaning — ordering by
  it returns whichever run hashes lowest, which for a re-analysis of the same file
  is the *older* run. Now `ORDER BY started_at DESC, id DESC`, with `id` breaking
  ties so same-second runs stay stable
- **`only_case_id` did not check that there was only one.** Named for exactly that
  guarantee, it returned the first case regardless of how many existed. A CLI using
  it to skip a `--case` argument would have silently analysed the wrong case. Now
  fetches two and returns `None` if a second exists — refusing beats guessing in a
  forensic tool
- **`count`'s table allowlist was missing `rule_results`.** The table exists and is
  written by `insert_rule_results`, but counting it returned
  `InvalidParameterName("unknown table")`. The allowlist was correct in spirit and
  had drifted from the schema
- Also pinned: report regeneration updates its digest rather than duplicating the
  row, a report with no analysis is recordable, re-recording a rule does not inflate
  the rule count, and both transaction outcomes (commit and rollback)

#### The review workflow could never have worked on any file
- **Bug: `insert_finding` omitted `analysis_id` from its review insert.** The
  column is `NOT NULL`, so *every* reviewed finding was rejected by the database
  with `NOT NULL constraint failed: finding_reviews.analysis_id`. The entire §66
  review workflow was non-functional, and no test caught it because
  `insert_finding` had **no test coverage at all** — a green suite around an
  unexercised function
- `analysis_id` is part of the finding's primary key, so it identifies *which run*
  is being reviewed. Passing it was the fix; the schema was right
- **A regression test now pins this**: storing a reviewed finding must succeed, and
  every verdict state (`Reviewed`, `Accepted`, `Rejected`,
  `RequiresInvestigation`) must round-trip individually. A state that fails only for
  one variant reads as a bug confined to a rare path
- Added `record_review` and `reviews_of`. The workflow's normal path is the engine
  writes `New` findings and a reviewer decides *later*, which previously had no API
  at all — `insert_finding` could only record a verdict that already existed
- **Repeated verdicts append rather than replace.** Who concluded what, and when, is
  part of the record; overwriting the earlier verdict would erase a reviewer's
  earlier reasoning
- Reviews are ordered by row id, not timestamp: two reviews in the same second must
  still come back in write order or the history reads as if it ran backwards
- A verdict against a finding that was never stored is refused, so a report cannot
  claim a reviewer examined something the engine never measured

#### Search: SQLite compares by storage class, which fails silently
- `-core/src/store/search.rs` implements search over case data (spec §41):
  `SearchQuery`, `SearchScope`, `SeverityFilter`, `SearchResult`. Scopes are
  findings, assets, and evidence, UNIONed in SQL and sorted once
- **Two bugs the tests caught, both silent rather than loud.** A severity rank
  bound as a `String` is compared against an integer expression *as text*, so
  `rank >= '0'` matches nothing and the search returns an empty list with no
  error. And `ArmParam` now distinguishes `Text` from `Int` so the mistake is a
  type error rather than a runtime surprise
- **"At least Critical" was inverted.** Rank 0 is the *most* severe, so the
  filter is `rank <= given`. Written as `>=` it admitted everything below the
  threshold — the exact opposite of what was asked
- **A blank term is not an empty term.** `SearchQuery::all()` lists everything;
  `SearchQuery::text("   ")` matches nothing. The first attempt used a NUL byte as
  an impossible LIKE pattern, but SQLite's `LIKE` ignores NUL in a bound string,
  so it matched every row. Now an unsatisfiable `AND 0` predicate, emitted as SQL
  so no bound parameter is expected and the positional numbering stays intact
- **A severity filter does not hide assets.** Assets carry no severity; applying
  a findings-only filter to them would silently remove every asset a reviewer
  asked to see alongside its findings
- **LIKE wildcards in a term are escaped.** The term is bound so it cannot inject,
  but an unescaped `%` would still match every row — which is not what someone
  searching for a hash expects. There is a test for both the quote and the `%`
- **Truncation is reported.** `SearchResult::truncated` and `total` come from a
  separate `COUNT`, because a page reporting its own length as the total would
  tell a reviewer there were 200 findings when the case holds thousands
- Searches are bounded by default (200 rows) — an unlimited search over a large
  case is a denial of service reachable by typing in a search box

#### Whole-file comparison: tolerances, and why unmeasured is not a value
- `-rules/src/comparison.rs` adds the whole-file aggregate — `Comparison`,
  `compare`, `compare_with`, `ComparisonInput` — feeding the metadata,
  scene-structure, silence, and loudness axes that the model crate's per-stream
  half left open. It lives in `-rules` because feeding it needs `MetadataTree`,
  `SceneReport`, and `SilenceRegion`, and the model crate must not depend on
  those; vocabulary stays in `-model`, aggregation stays above it
- **Whole-file numeric axes compare against a tolerance.** Loudness and scene
  counts are measurements: two runs of the same file differ in the last bits of
  a float. Comparing exactly would report a difference on every pair of related
  files, training a reviewer to ignore the axis. The tolerance used is recorded
  on every result so a reader can judge the claim rather than trust it
- **An unmeasured side is `NotComparable`, never a one-sided value.** The first
  draft returned `OnlyLeft { value: "not measured" }`, which reads as "only this
  file has silence" — a claim about the media that was never made. Caught by a
  test asserting silence never measured must not read as zero silence
- **Within-tolerance agreement reports `Equal`.** The first draft reported the
  0.2 LU gap as a `Different`, contradicting `WithinTolerance::agreed`, which had
  just told the caller the two agree
- **An LRA measurement is not compared against integrated loudness.** Loudness
  *range* is a different quantity over a different window; comparing them yields
  a plausible-looking number that means nothing, so a non-integrated
  methodology is treated as absent
- **Metadata pairs on `(scope, track, key)`, not position.** Metadata is a set of
  labelled values, not a sequence — pairing positionally compares whatever
  happened to sort into slot 3 of each file
- **Scene structure compares counts, not frame positions.** Two encodes of the
  same footage rarely agree on which frame a cut lands on while agreeing closely
  on how many cuts there are
- `compare_self` compares two analyses of the same file, so engine
  non-determinism is distinguishable from a genuine difference between two
  assets — otherwise a reviewer would be sent looking for a problem in the media
  that is not there
- `is_equivalent` is deliberately strict: unmeasured axes keep it false.
  `measured_differences` is the looser view for callers asking what actually
  disagreed

#### Comparing two files: streams are paired, not zipped
- `-model/src/comparison.rs` adds `Difference`, `ComparisonAxis`, and
  `compare_streams` for file-to-file comparison (spec §38–40)
- Streams pair by **kind and position within that kind**, not by raw index. Index
  `0` in one file is not necessarily index `0` in the other, so a file that
  dropped its first audio track reports *one unmatched stream* instead of
  reporting every subsequent track as changed — which would bury the one real
  difference in a wall of false ones
- `Difference::NotComparable` exists precisely so "we could not read it" is never
  rendered as "it matches". An unreadable container, a stream that carries video
  properties on one side only, and a property neither side declared all land
  here rather than in `Equal`
- `Left`/`Right` in `Difference` follow *argument position*, not asset identity.
  Documented on the enum because a caller that reorders its arguments gets
  correspondingly renamed variants — visibly in the report rather than silently
  reporting the wrong side
- **No similarity score.** A transcode to a lower bitrate and a re-mux with
  reordered atoms produce byte-different files, but only one of them changed
  anything a reviewer would care about. A single number would discard exactly the
  information the tool exists to surface
- `ChromaSubsampling::tag()` added to the model, rendering conventional notation
  (`4:2:0`, not `Cs420`) and showing undeclared values as such rather than
  normalising them to whatever this build happens to model

#### Progress and cancellation: what is and is not interruptible
- `-core/src/progress.rs` adds `Stage`, `Progress`, `Cancellation`, and
  `ProgressTracker`. `Cancellation` is an `Arc<AtomicBool>` that clones *share*
  rather than copy, so a UI holding a token can stop the worker holding the
  original. `AnalysisEngine::analyse_with` is the new entry point; `analyse`
  delegates to it with a silent tracker so the common path costs nothing and the
  two cannot diverge in what they compute
- Stage ordinals come from one ordered list rather than numbers hard-coded at each
  call site. Hard-coded numbers drift, and a progress bar that reaches 100% before
  the last stage starts is worse than no bar
- `CoreError::Cancelled` is its own variant, not an I/O failure. Nothing went wrong
  with the file — the caller asked the engine to stop — and a caller should be able
  to retry without concluding the asset is corrupt
- A cancelled analysis writes **nothing** to the case database. A partial
  analysis record that looked complete would be worse than no record, because a
  report built from it would assert measurements the engine never took
- The honest limitation, stated rather than papered over: cancellation is
  cooperative and coarse. A cancel during a 90-second decode is observed when that
  decode returns, because the decoder has no cancellation hook. Background workers
  are **not** implemented — the engine is synchronous by design and nothing spawns a
  runtime
- Progress reports no ETA. An estimate on a feature-length master would be wrong
  by minutes and would read as a commitment the engine cannot make. Fractional
  progress appears only where a total is genuinely known
- A cancelled stage is not announced as having started. A progress bar counting a
  stage that never ran is a small lie about what the engine did

#### Frame timestamps were off by a factor of a million
- The decoder's `Timestamp` carries a rational time base `(num, den)` meaning
  `num/den` seconds per tick. The first version of `media_time` passed
  `(1_000_000, 1)` as the microsecond base, which is the reciprocal: it asks for
  one million seconds per tick. A frame at a 48 kHz base landed at 2 microseconds
  instead of a millisecond
- Correct is `(1, 1_000_000)`. A test that builds a `(1, 48_000)` timestamp and
  checks the result in microseconds catches this immediately, and a second test
  pins the already-microsecond case so the fix cannot be over-applied
- This is the exact failure the project exists to prevent: a precise-looking
  timecode that is wrong by orders of magnitude, attached to evidence

#### Saturation, and the colour that wrapping would have produced
- YUV below the studio-swing floor of 16 makes the BT.601 conversion overshoot:
  with U=V=0, red works out to -223 and blue to -277. Cast to `u8` without
  clamping those become 33 and 179 — a dark red and a mid blue, which render as a
  plausible picture rather than an obvious fault
- The clamp is what makes an out-of-range frame look broken instead of wrong. A
  test asserts the values stay near black, with the specific wrapped value named
  in the comment so the failure mode is documented rather than merely prevented

#### Frame extraction as evidence (spec section 32, 33)
- Converts a decoded frame to interleaved RGB and writes a real PNG, so the
  artefact is openable with any image viewer rather than only by this engine.
  Evidence a reviewer cannot inspect with their own tools is an assertion
- Handles 4:2:0, 4:2:2, and 4:4:4 planar YUV plus RGB, BGR, and monochrome. The
  last three are reordered rather than converted, so the bytes remain the
  decoder's own
- 10- and 12-bit YUV are **refused**, not approximated. Converting them requires
  dithering or truncation, either of which alters pixel values, which would make
  the artefact a lossy transform rather than evidence of what was decoded
- No scaling anywhere. A thumbnail is a better artefact and worse evidence, so the
  extracted frame is the decoded frame at its own resolution
- The BT.601 matrix is recorded on every `FrameImage`. A YUV frame has no inherent
  RGB appearance — the decoder reports no colour space — and BT.601 versus BT.709
  differs visibly in skin tones, so a reviewer comparing against a reference
  decode needs to know which convention produced these bytes
- A truncated frame is refused rather than padded; padding would produce a
  plausible image with invented pixels along one edge
- Frame names derive from the frame's index and presentation time, so
  re-extracting the same frame twice yields the same name instead of a
  re-run-dependent sequence

#### The timeline sorted its own provenance, and ranked a weaker claim above a stronger one
- The unified error timeline (spec §31) merges structural damage, timestamp
  anomalies, and positioned findings into one ordered list. Every entry carries its
  source and a `Placement` of `Measured`, `Inferred`, or `Unplaced`, because the
  three disagree about how much their timecode can be trusted: a structural
  defect's time comes from accumulating sample sizes, while a timestamp anomaly's
  comes from the sample's own stamp
- The first sort key included `placement as u8`, which orders `Measured` before
  `Inferred`. At equal timestamps that put a weaker placement claim ahead of an
  exactly-measured one. Two tests caught it: an inferred entry at 100 ms displaced
  a measured entry, and a placed entry sorted ahead of an unplaced one that was
  supposed to come last
- Placement is now data on the entry, not a ranking of it. The key is time, source,
  reference, summary
- An observation with no position is `Unplaced` and sorts **last**, never at
  `00:00:00`. Findings without a position are kept rather than dropped: the
  timeline must account for every finding, or it disagrees with the findings list
  printed above it
- Two things are deliberately absent rather than approximated. Timestamp gaps and
  overlaps carry a *size*, not a time, and `TimestampReport` does not retain the
  timestamps it scanned — placing them would need a second pass over the file, so
  they stay off the timeline instead of landing at an invented instant. And a
  cache hit serves an empty timeline with a stated limitation, rather than
  pretending the cached findings carry positions they were not stored with

#### Loudness range, and why its unit is LU rather than LUFS
- `loudness_range` per EBU Tech 3342: the 10th to 95th percentile of 3-second
  short-term loudness. This completes §22
- It reuses the existing BS.1770-4 K-weighting rather than duplicating it:
  `block_loudness_series` was generalised over window length. Integrated loudness
  uses 400 ms blocks, LRA uses 3 s, and both step at 75 % overlap
- `Methodology::EbuR128Lra` is a separate variant from `ItuBs1770_4`, and renders
  in **LU**, not LUFS. LRA is a span between two loudness figures; printing it as
  LUFS presents a range as though it were an absolute level, and a reader would
  compare it against a delivery target it cannot be compared against
- The -70 LUFS absolute gate applies here too. Without it, silent blocks between
  passages drag the 10th percentile to the floor and a silent file reports an
  enormous range. A file that is silent throughout reports 0 LU, not 70
- `TooShortForShortTerm` is distinct from `TooShort` because a 2-second file is
  long enough for integrated loudness and too short for LRA. One shared message
  would hide that the two measurements need different amounts of audio
- LRA cannot see anything shorter than 3 seconds. A click or a sub-3-second edit
  is invisible to it, and the window length is stated with the figure so a reader
  knows what the measurement could not have observed

#### The dBFS floor was inventing energy, and digital silence reported a centroid
- Every bin below a threshold was floored at -200 dBFS rather than reporting
  `-inf`. That is right on its own: `-inf` propagates into any sum computed from
  the vector, and a report printing `-inf dBFS` looks like a tool defect
- But converting a floored bin *back* to a linear magnitude and summing it — the
  obvious way to compute a centroid — invented energy from nothing. 1024 floored
  bins sum to more than enough power to clear any "is this silent?" threshold, so
  a frame of pure digital silence reported a spectral centroid, and a peak
  frequency at the first bin holding the floor
- Fixed by treating a floored bin as the absence of a measurement rather than as a
  measurement of something very quiet. The same correction applies to flatness,
  where thousands of identical floored values in the geometric mean would pull
  every signal toward the same number and make a pure tone look flat
- Caught by a test written for the obvious behaviour: silence asserting that it has
  no peak frequency, and failing

#### Spectral analysis (spec section 22)
- 2048-point Hann-windowed FFT at 50% overlap. A rectangular window — no window at
  all — is the obvious thing to write and the wrong one: it leaks a strong tone
  across the whole spectrum, which would make a pure tone look like broadband noise
- The window's 1.5-bin equivalent noise bandwidth is reported, because that is the
  number saying how finely the analysis can distinguish two adjacent tones. Without
  it, "one bin" is an unstated claim
- Peak frequency, spectral centroid, Wiener flatness, and low/high energy ratios.
  DC (bin 0) is excluded from the peak search: a signal with an offset would
  otherwise report 0 Hz as its loudest frequency, which is a property of the
  waveform rather than anything audible
- Energy is accumulated in linear power, never in decibels. Summing dB values is
  not meaningful, because their total depends on how many bins were added
- Every figure carries `Methodology::HannWindowFft`. Spec section 21 forbids
  reporting a number without the method behind it, and a spectrum computed with
  different window parameters is a different measurement rather than a rougher one
- A signal shorter than one frame returns `TooShort` rather than an empty profile.
  A file this method cannot measure is not the same as a file with no content, and
  reporting one for the other is the failure this project exists to prevent

#### Encoder fingerprinting that cannot overstate itself
- Spec §27 asks for encoder signatures "where technically defensible". The
  defensible reading is that a signature is *not* an identification: `Lavf58.45.100`
  is a string in the file, and anyone can write any string into any file
- `fingerprint::Confidence` has **no top grade**. Declared tags are `Low`;
  all-intra structure, the one property that cannot be written as a string, is
  `Medium`. Nothing measured here can support a claim about which program ran, so
  the type makes that unrepresentable rather than relying on discipline
- Every indicator carries its own `limitations`, and a test fails the build if one
  is ever empty
- §27's bitstream and quantisation evidence is not collected: this build parses no
  coded slice data. That is reported as `not_measured` rather than omitted,
  because a fingerprint that hides its inputs reads as a complete answer
- `©nam` and `©cmt` are deliberately not matched. A file named after its camera is
  not evidence about what wrote it, and matching a title would fire on most files
  in existence

#### Property tests found a bug in the property test
- `-container/tests/properties.rs`: 6 properties, 512 cases each, over
  `next_box`, `parse_track_colour`, `parse_edit_lists`, and `scan_isobmff`
- The invariants are stronger than any example set can express: no panic;
  truncation is monotone, so a prefix never reveals more than the whole file; no
  reader emits more entries than there are `trak` boxes; damage offsets stay
  inside the input; absurd declared sizes are refused
- The box-walk property looped on `cursor` and never assigned it, so it only ever
  checked offset 0 — a passing property that examined nothing. Clippy caught the
  dead `mut`
- The harness was itself verified: an injected panic in a property body was caught,
  so the properties are genuinely executing rather than vacuously passing

#### Colour was a type that existed and nothing more
- `ColourInfo` had five fields and `VideoFormat::is_hdr` a sixth. All six were
  populated by `Default::default()` and `false` in every reader, on every file, so
  every report in every format carried empty colour
- `is_hdr` was not a measurement that came out false. It was a constant. A report
  could have said "not HDR" about an HDR master and been reporting a value no
  code path could ever produce otherwise
- `tpt-kinetix-demux` has no colour support of any kind, so this could not be
  obtained from the demuxer. It needed a reader, the same way `elst` and `ctts`
  did before it
- New `container/src/colr.rs` reads `colr` inside the visual sample entry, plus
  `mdcv` and `clli`. `nclx` and `nclc` are read separately, because treating one
  as the other would report a range for a box that declares none
- The 78-byte `VisualSampleEntry` offset is the whole difficulty. Searching the
  sample entry from offset 0 reads the width and height fields as a box header,
  walks into the middle of the entry, and reports no colour on every file —
  indistinguishable from a file that carries none
- Codes this build cannot name are reported as `unrecognised code N`. A file
  declaring an unknown code has still declared something, and the number is the
  evidence
- Matroska is unchanged and reports nothing. That reader exposes no picture
  geometry, so there is no `VideoFormat` to attach colour to

#### `VIDEO.HDR_METADATA_MISSING` — a new rule over a field that was always false
- Reports a stream that signals HDR (BT.2020 primaries, or PQ/HLG transfer)
  while carrying neither `mdcv` nor `clli`. Warning/High: nothing is damaged, and
  both halves are read directly from the box structure
- It asserts no cause. A re-mux that kept `colr` and dropped `mdcv` is the
  obvious story and is not named; a test fails the build if the finding's text
  ever does
- Its first version keyed on `is_hdr` alone, which fired on conformant HDR10
  masters as well — a rule reporting every correct HDR file as defective. The
  negative test caught it; it now checks `hdr_metadata` for the boxes it is
  actually about
- Its first version discriminated findings by primaries. Two HDR tracks declaring
  the same primaries would then derive one `FindingId`, and `findings` has a
  primary key on `(analysis_id, id)` — the second insert would abort the whole
  analysis. That is the collision already fixed once in `RuleEngine::evaluate`;
  this time the discriminator is the stream index

#### The fixture's `stsd` was not a valid visual sample entry
- It wrote 50 zero bytes where a `VisualSampleEntry` has a specified layout, so no
  child box could exist inside the sample entry and no `colr` could be written at
  all
- It now writes the real 78-byte field layout, with a `debug_assert` on the
  length. Every other fixture's bytes shift accordingly, which is why the whole
  suite was re-run rather than the colour tests alone

#### Three rendering bugs the CLI check caught that the unit tests did not
- The mastering display printed one coordinate per primary, so `R(0.6800)
  G(0.2650) B(0.1500)` looked like the y values had been dropped when the caller
  had only asked for one of each pair
- `mdcv` and `clli` were appended to one string, so a report read
  `...(raw 1)content light:...` with no boundary between the two boxes
- Content light levels were formatted to whole cd/m², and `clli` stores whole
  lumels, so 4,000 lumels rendered as `0 cd/m2` — indistinguishable from no value
  at all. Now four decimal places, with the raw lumel count kept beside it
- All three were visible only by running the real binary over a real file. The
  assertions that caught them are now in `colr.rs`'s tests

#### Two findings from one rule could abort the whole analysis
- `findings` has a primary key on `(analysis_id, id)`, and a finding's ID was
  derived from the rule ID and timeline locator alone. Nineteen of the 26 rules
  pass no locator, so any two findings from one of them collided
- The second insert failed with `UNIQUE constraint failed`, which surfaced as
  `case database error` — the analysis stored nothing and the operator was told
  the file could not be read. Not a degraded report; no report at all
- Every rule was affected. Verified directly: for all 26, two findings at one
  position derive an identical ID
- It could only be reached by a file producing two findings from a single rule,
  which no fixture in the corpus did. Chasing an unrelated fixture gap is what
  uncovered it: `build_mp4_with_frame_rate_change` fires
  `TIMING.TIMESTAMP_GAP` once per affected sample, and the second one crashed
  the analysis
- Fixed in `RuleEngine::evaluate`, not rule by rule, so a rule cannot
  reintroduce it by forgetting to disambiguate. Collisions are resolved from the
  finding's own content, after sorting, so the IDs are still reproducible across
  runs — confirmed by analysing the same file twice and comparing all 59 IDs
- Guarded by `a_file_yielding_several_findings_from_one_rule_stores_without_a_key_collision`,
  which runs through `analyse` rather than the rule engine, because persistence
  is where the failure was

#### `CONTAINER.DECLARED_TRACK_MISMATCH` now fires — it was unfireable, not just untested
- The rule compares what the container declared against what was recovered. Both
  container readers assigned `declared_track_count` from `tracks.len()`, so the
  two sides were the same number by construction and the condition could never be
  true — for a real file, a malformed one, or a synthetic one
- Fixed by reading `mvhd` independently of the demuxer, in a new `boxes` module
  holding the ISO-BMFF walkers that `elst.rs` already needed. Those helpers had
  been written twice by two readers; they now exist once
- `next_track_ID` is the declaration that matters, and it catches the case the
  `trak` count cannot: a file that claims a track it never carried leaves no box
  to count. Both are compared, so a track *lost in parsing* and a track *never
  present* are both reported
- Matroska is unchanged and stays silent. It has no `mvhd` equivalent, so the
  demuxer's track list is both the declaration and the recovery. Documented at the
  call site rather than silently reporting agreement
- Added `build_mp4_with_declared_track_mismatch`, the first file in the project
  that can trigger this rule

#### `VIDEO.FRAME_RATE_CHANGE` would have fired on nearly every real video file
- The rule took the spacing between consecutive entries of `frame_times`. Those are
  in *decode* order, so any file with B-frames has unevenly spaced presentation
  times even when the frame rate never changes: one IBBP group presents as
  0, 3, 1, 2, whose differences are +3, -2 and +1 ticks
- Essentially every real encoded video file uses B-frames, so the rule would have
  reported a frame-rate change on almost all real media. In a forensic tool that is
  the loudest way to be wrong: it trains an analyst to ignore the rule entirely
- Found only after `ctts` parsing made presentation times reachable. Before that,
  every timestamp was decode time and the sequence was monotonic by construction,
  so the bug could not express itself
- Fixed by sorting the presentation timeline before measuring. After sorting, a
  constant-rate track has identical intervals whatever its reordering, and a genuine
  rate change still stands out
- The finding's position also indexed the decode-ordered array while `position`
  counted the sorted one. For any file with B-frames those are different frames, so
  the reported time pointed somewhere else entirely
- Guarded by `b_frame_reordering_alone_is_not_a_frame_rate_change`, which also
  asserts the rule still fires on a real rate change — a fix that silenced the rule
  would not be a fix

#### The B-frame fixture was not a permutation of any real stream
- `build_mp4_with_reordered_frames` used composition offsets that produced
  presentation times of `3, 3, 3, 1, 7, 7, 7, 5`: three frames sharing a
  presentation time and two times with no frame at all. No encoder emits that, and it
  made the track look as if it had both duplicated and missing frames
- Now uses the closed-IBBP offsets a real encoder writes — `0, +2, -1, -1` — which
  is a genuine permutation: every frame presents once, at a distinct time, and sorted
  the presentation times recover `0, 1, 2, ...`
- `the_reordered_fixture_presents_every_frame_exactly_once` asserts that property
  directly, so a future change to the offsets cannot quietly reintroduce it
- With a real permutation the file also stops looking like a frame-rate change,
  which is what exposed the rule bug above

#### Twelve of sixteen video fixtures claimed every frame was a keyframe
- Omitting `stss` means *every* sample is a sync sample. The fixture builder
  omitted it for every track, so 12 of 16 video fixtures declared a stream of
  nothing but intra frames
- No real encoder produces that. `VIDEO.ALL_FRAMES_KEYFRAMES` fired on 12 of 20
  fixtures, which reads as a rule that fires on almost everything rather than as a
  property of the files — the same shape as the WebM duration problem, one layer
  down
- It had also been quietly excusing coverage. The attribution guard excluded this
  rule with a comment saying it "says nothing about attribution" — which was true,
  and was the problem: the guard had learned to expect a rule that could not be
  attributed, and would have kept excluding it
- `build_mp4` now writes a periodic sync-sample table (a keyframe every 12 frames)
  as a muxer does. Omitting it is now `SyncTable::AllSync` and must be asked for
- The rule fires on exactly one fixture, and the attribution guard no longer needs
  an exclusion for it. That is the check that the fix was real rather than cosmetic

#### A batch test that passed for the wrong reason
- `findings_are_collected_across_the_whole_batch` asserted findings span more than
  one asset. They did — because all three MP4 fixtures reported
  `ALL_FRAMES_KEYFRAMES`, a finding that said nothing about whether any file was
  damaged
- With the fixtures corrected, one asset had findings and the test failed, which is
  the honest result: only `damaged.mp4` was defective. `nested.mp4` now carries a
  real defect, and the test also asserts the clean file reports nothing
- Worth noting because it was passing, and had been for as long as it existed

#### Appending bytes to a valid file produced a finding about missing media
- The structural scanner kept walking boxes *after* `mdat`. Bytes a muxer appended
  for any reason — a signature, padding, a second `free` box — were parsed as box
  headers
- Appending the literal string `payload a real muxer never writes` produced `box
  'oad ' at offset 3677 declares 1885436268 bytes but only 33 remain`, reported as
  `CONTAINER.TRUNCATED_MEDIA`. The "box type" was the fourth character of the word
  "muxer"
- So an analyst would have been told media was missing from a file whose media was
  entirely present. In a forensic tool that is worse than silence: it names a defect
  that is not in the evidence
- `mdat` now ends the walk. Bytes after it are reported once, as trailing data,
  which is what they are — outside the container's structure by definition
- An existing test had *pinned the buggy behaviour*, asserting that appended bytes
  must surface as an over-read. Rewritten into two: a box appearing where a box is
  expected is still parsed and reported, and bytes after `mdat` never are. Both
  cases are legitimate and the distinction is the point

#### `CONTAINER.STREAM_DURATION_MISSING` fired on every WebM file, for a wrong reason
- The Matroska reader exposed no duration, so every WebM file reported "declares no
  duration". The finding was *correct about the fixture* — and the fixtures were
  unusual, not the files
- Compounding it, the fixture builder wrote no `Segment > Info > Duration` at all,
  so the corpus contained no WebM file resembling a real one. A reader would have
  learned to expect that rule on WebM, hiding it on files that genuinely omit the
  element
- `Segment > Info > Duration` is now parsed. `MkvTrack` carries only a number, a
  type and a codec id, so this had to be read from the bytes
- `build_webm` now writes the element, and `build_webm_without_duration` exists to
  produce the file the rule is actually for. The rule now fires on exactly one
  fixture

#### A guard for attribution, not just coverage
- The coverage guard asks "does every rule fire on *some* fixture", which a rule
  firing *everywhere* satisfies. Added `a_rule_fires_only_on_files_built_for_its_condition`,
  mapping each fixture to the rules it was built to test and asserting nothing else
  fires
- It found four things on first run, three of them real:
  - `no-duration.webm` also tripped `VIDEO.SINGLE_KEYFRAME` and
    `VIDEO.DUPLICATE_FRAME_RUN`. A duration fixture should reach no other
    condition; rebuilt with several distinct blocks
  - `bitrate-drop.mp4` trips `VIDEO.DUPLICATE_FRAME_RUN`, because its reduced
    frames are 8 bytes of `0x5A` and therefore identical. A real encoder produces
    small but *distinct* frames; recorded rather than fixed, since changing it
    would obscure the bitrate drop itself
  - Six further pairs are one condition with several true answers — an empty
    `moov` is malformed, anomalous, *and* streamless. Listed with a note each, so
    an extra finding reads as a known quantity rather than a mystery

#### The audio amplitude rules now fire from real encoded audio
- `AUDIO.CLIPPING`, `AUDIO.DC_OFFSET`, and `AUDIO.INAUDIBLE` had no fixture. The
  analysis was present, wired, and fed — the corpus's single audio fixture is a
  0.8-amplitude tone, which sits below the 0.999 clipping threshold, has a mean
  of zero, and is far above the -70 LUFS floor. It simply never reached any
  threshold
- Three signals added, each going through the same real Opus → Ogg → WebM path
  as the existing fixture, because these rules read *decoded* levels and a stub
  payload would parse as an audio track then decode to nothing
- A square wave for clipping rather than a full-scale sine: a sine spends most of
  its time well below its peak, so every sample has to be pushed over the
  threshold by decoder ringing, while a square wave sits at its extreme for half of
  every cycle. Each signal is pushed well past its threshold rather than to it,
  since a lossy codec will not reproduce a boundary value
- Added `each_audio_amplitude_fixture_triggers_only_its_own_rule`, which asserts
  each fixture trips its own rule *and not the other two*. The coverage guard only
  checks the set — one loud file with a DC offset would satisfy all three rules
  at once and pass, while proving nothing about any of them

#### `TIMING.NON_MONOTONIC_PTS` now fires — presentation order is now computed
- The second and last rule that could not fire on any file
- `scan_presentation` was always correct; nothing upstream could feed it. Frame
  timestamps came from `stts`, whose deltas are unsigned, so the sequence was
  monotonic by construction and no MP4 could violate it
- Root cause: **presentation order was never computed.** B-frames produce a
  non-monotonic presentation order through composition offsets, and
  `tpt-kinetix-demux` has no `ctts` support at all — the box was simply not being
  read. Now parsed in `boxes.rs` and added to decode time
- `TrackFrameInfo` gained `decode_times` alongside `frame_times`. Keeping both
  matters: a file whose decode order is correct but presentation order is not is
  *normal*, and reporting only presentation times would make that
  indistinguishable from a corrupt timestamp table
- Offsets are applied in ticks and converted once. Converting to microseconds,
  adding, and converting back would round twice, and the error would vary per
  sample — inventing jitter in files whose timestamps are exact
- `build_mp4_with_reordered_frames` writes a run-length `ctts` in the IBBP
  pattern a real muxer produces, not one entry per sample
- Guarded by `a_reordered_file_is_out_of_order_only_in_presentation_time`, which
  asserts decode time stays strictly increasing. A fixture going backwards in
  *both* would be a broken table rather than reordered frames, and would prove
  nothing about the reader
- `NO_END_TO_END_FIXTURE` is now empty. Every one of the 26 rules fires end to
  end from a file this project owns
- `METADATA.DECLARED_VS_MEASURED_MISMATCH`, `TIMING.TIMESTAMP_GAP`,
  `VIDEO.FRAME_RATE_CHANGE`, `CONTAINER.MALFORMED_STRUCTURE`,
  `CONTAINER.NO_USABLE_STREAMS`, `CONTAINER.PARSE_ANOMALY`,
  `CONTAINER.STREAM_START_OFFSET`, `CONTAINER.STRUCTURAL_DEFECT`,
  `VIDEO.GOP_LENGTH_CHANGE`, `VIDEO.SINGLE_KEYFRAME`
- Not one required new analysis. Every one was a builder that already existed —
  and `TrackSpec::declared_duration` was in use by the container's own unit
  tests — which had simply never been written to disk as part of the corpus
- Three added: `build_mp4_with_wrong_declared_duration`,
  `build_mp4_with_frame_rate_change`, and a trailing-data fixture for the
  structural rule, which needs a defect *other* than truncation
- The remaining five are documented individually in `stage_guard.rs` rather than
  left as a bare list, because "needs a builder" and "cannot fire" call for
  different work

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

#### Six fixture builders existed, were correct, and were never called
- `build_mp4_empty_moov`, `build_mp4_stsd_gop_change`, `build_mp4_with_keyframes`
  and `build_mp4_without_stss` were written, exported, and used by the rules
  crate's own tests. Nothing was wrong with any of them
- They were simply never written to disk by the corpus. So five rules could not
  fire end to end on any file the project owns, while every guard in the project
  reported the rule set as fully exercised
- This is the cheapest possible instance of the defect the guard was built for,
  and the most embarrassing: the capability existed, the test existed, and the
  wiring between them did not. Only asking "can this rule ever *say* anything",
  as opposed to "are its inputs reachable", surfaced it
- Six fixtures in the corpus closed it. `CONTAINER.STRUCTURAL_DEFECT` needed one
  piece of new construction — four appended bytes — because nothing built a file
  with trailing data

#### A trailing-data fixture tested the limitation instead of the rule
- The first version appended `b"trailing-bytes"`. Fourteen bytes is more than a
  box header, so `scan_isobmff` read the appended text as a further box and
  reported truncation rather than trailing data
- `CONTAINER.STRUCTURAL_DEFECT` therefore still did not fire, and the fixture was
  silently exercising the documented limitation that appended data long enough to
  resemble a box is read as one
- Reduced to four bytes, which cannot be a box header. The comment now says why:
  a fixture built on the fragile side of that boundary would be testing the
  limitation rather than the rule. Worth writing down because the fixture would
  otherwise have kept passing for the wrong reason

#### `edit_list_offset` was a modelled field no reader ever filled
- `StreamTiming::edit_list_offset` existed in the model, serialised into reports,
  and was hardcoded `None` in `mp4.rs` for every file ever analysed
- `tpt-kinetix-demux`'s `Mp4Track` carries no `elst` field at all, so the delay
  could not be populated through the demuxer. The fixture built a correct edit
  list; the reader discarded it
- This is the colour/HDR defect class exactly — a type that is present and never
  populated — and it had been hiding behind a field that looked implemented
- `elst.rs` parses `moov/trak/edts/elst` from the bytes the container reader
  already holds, so no extra I/O. `CONTAINER.STREAM_START_OFFSET` now fires end
  to end, verified on a real 120 ms-delayed file reporting `00:00:00.120`
- That rule had been sitting in `NO_END_TO_END_FIXTURE` from the day that list
  was written. It fired immediately once the field was populated, and the
  reverse assertion in the guard caught its own list entry going stale

#### The rule reported `00:00:00.000` for a track delayed by 120 ms
- `CONTAINER.STREAM_START_OFFSET` built its summary from `start_time`, which the
  container reader sets to zero unconditionally. The edit-list offset — the value
  that actually tripped the rule — appeared only in the measurements
- So the finding's headline said "starts at 00:00:00.000" while its own second
  line said "edit-list offset: 00:00:00.120". A precise, confident, wrong
  timecode, which is worse than the `None` it replaced
- The summary and the timeline placement now use whichever value triggered the
  rule. This is the declared-versus-measured distinction the project already
  applies to durations: a start time that was never read is not a measurement

#### The fixture wrote `media_time = 1` where an empty edit requires `-1`
- `build_mp4_av`'s edit list wrote `media_time: 1`. An empty edit — "hold nothing
  here for `segment_duration`" — is defined by `media_time == -1`
- `1` declares that presentation starts one tick into the media, a different edit
  entirely. It happened to work for the delay cases because only
  `segment_duration` is read, which is exactly why nothing caught it
- Found only because the parser distinguishes `Some(ZERO)` from `None` and a test
  asserted the difference. A parser that ignored `media_time` would never have
  noticed

#### A four-byte field was read through an eight-byte slice, silently
- `next_box` did `u32::from_be_bytes(data[offset..offset + 8].try_into().ok()?)`.
  The slice is eight bytes; `from_be_bytes` wants four; `try_into` returns `None`
  on a length mismatch and `.ok()?` turns that into an early return
- The walk therefore reported "end of input" at the first box it met, for every
  file, forever. No panic, no warning — the parser simply found nothing and
  returned an empty vector, which reads exactly like "this file has no edit
  lists"
- Found by printing the intermediate offsets. Four earlier revisions of the
  `moov` walk were each *read* as correct and each skipped every `trak`, because
  `body_end` and "one header in" are different numbers and both read plausibly
- The fix was to stop doing offset arithmetic entirely: `next_box` already returns
  the body as a borrow, so `moov_body` hands that straight to the child walk.
  There is no longer an offset to get wrong

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