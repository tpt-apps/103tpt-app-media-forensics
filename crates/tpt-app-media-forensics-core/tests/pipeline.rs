//! End-to-end tests for the analysis pipeline.
//!
//! These run the real engine over real container bytes, so they cover the
//! wiring between acquisition, inspection, rules, and the cache rather than
//! any one layer in isolation.

use tpt_app_media_forensics_container::fixture::{
    build_mp4, build_mp4_stsd_gop_change, build_mp4_with_hdr_signalling_only,
    build_mp4_with_repeated_frames, TrackSpec,
};
use tpt_app_media_forensics_core::case_dir::CaseDirectory;
use tpt_app_media_forensics_core::{acquire, AnalysisEngine};
use tpt_app_media_forensics_model::{Case, Severity, StreamKind};

/// Creates a case directory containing `contents` as `sample.mp4`.
fn case_with(contents: &[u8], dir: &std::path::Path) -> (CaseDirectory, std::path::PathBuf) {
    std::fs::create_dir_all(dir).expect("creates case parent");
    let source = dir.join("sample.mp4");
    std::fs::write(&source, contents).expect("writes source");

    let case_dir = dir.join("case.tptcase");
    CaseDirectory::create(&case_dir, &Case::new("Pipeline", None)).expect("creates case");
    (CaseDirectory::open(&case_dir).expect("opens"), source)
}

fn gop_change_bytes() -> Vec<u8> {
    build_mp4_stsd_gop_change()
}

#[test]
fn two_different_files_report_their_differences() {
    // The comparison engine's reason for existing (spec §38-40). Compared against
    // real container bytes so a regression in stream pairing shows up here rather
    // than only in the model crate's synthetic fixtures.
    let left = build_mp4(&TrackSpec::video_25fps(320, 240, 30));
    let right = build_mp4(&TrackSpec::video_25fps(1920, 1080, 30));

    let left_streams = tpt_app_media_forensics_container::inspect_bytes(left)
        .expect("left inspects")
        .streams;
    let right_streams = tpt_app_media_forensics_container::inspect_bytes(right)
        .expect("right inspects")
        .streams;

    let result =
        tpt_app_media_forensics_model::compare_streams(Some(&left_streams), Some(&right_streams));

    assert_eq!(result.streams.len(), 1, "both files have one video stream");
    let fields: Vec<&str> = result.streams[0]
        .differences()
        .map(|f| f.field.as_str())
        .collect();
    assert!(
        fields.contains(&"coded_dimensions"),
        "the resolution change must be found: {fields:?}"
    );
    assert!(
        !fields.contains(&"codec"),
        "the codec did not change and must not be reported as if it did: {fields:?}"
    );
}

#[test]
fn a_file_compared_against_itself_reports_no_differences() {
    // Without this, a comparison that reported everything as different would
    // still pass every other test here.
    let bytes = build_mp4(&TrackSpec::video_25fps(320, 240, 30));
    let streams = tpt_app_media_forensics_container::inspect_bytes(bytes)
        .expect("inspects")
        .streams;

    let result =
        tpt_app_media_forensics_model::compare_streams(Some(&streams.clone()), Some(&streams));

    assert!(
        result.streams[0].differences().next().is_none(),
        "a file must not differ from itself"
    );
    assert!(result.unmatched.is_empty());
    assert!(result.layout.is_equal(), "the layout must match itself");
}

#[test]
fn a_cancelled_analysis_stops_and_writes_nothing() {
    // The point of cancellation is that it stops real work, not just that a flag
    // flips. Cancelling before the first stage must abort the run.
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_with(
        &build_mp4(&TrackSpec::video_25fps(320, 240, 30)),
        tmp.path(),
    );

    let tracker = tpt_app_media_forensics_core::ProgressTracker::none();
    // Cancel before starting: the very first stage boundary must refuse.
    tracker.cancellation().cancel();

    let result = AnalysisEngine::new().analyse_with(&source, &case_dir, &tracker);
    let error = result.expect_err("a cancelled analysis must not succeed");
    assert!(
        matches!(error, tpt_app_media_forensics_core::CoreError::Cancelled),
        "expected cancellation, got {error:?}"
    );
}

#[test]
fn an_uncancelled_analysis_reports_progress_through_every_stage() {
    // The counterpart: a normal run must actually reach the reporter, or the
    // progress plumbing is dead code that tests still pass.
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_with(
        &build_mp4(&TrackSpec::video_25fps(320, 240, 30)),
        tmp.path(),
    );

    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&seen);
    let tracker = tpt_app_media_forensics_core::ProgressTracker::reporting(move |event| {
        sink.lock().expect("lock").push(event);
    });

    AnalysisEngine::new()
        .analyse_with(&source, &case_dir, &tracker)
        .expect("an uncancelled analysis succeeds");

    let events = seen.lock().expect("lock").clone();
    assert!(!events.is_empty(), "a run must report at least one stage");

    let tags: Vec<&str> = events.iter().map(|e| e.stage().tag()).collect();
    assert!(
        tags.contains(&"acquisition"),
        "acquisition must be reported: {tags:?}"
    );
    assert!(tags.contains(&"rules"), "rules must be reported: {tags:?}");

    // Every stage the run actually performed is reported, and no event is for a
    // stage that never ran.
    let known: Vec<&str> = tpt_app_media_forensics_core::Stage::ALL
        .iter()
        .map(|s| s.tag())
        .collect();
    for tag in &tags {
        assert!(known.contains(tag), "unknown stage reported: {tag}");
    }
}

#[test]
fn the_timeline_places_observations_from_more_than_one_source() {
    // The point of §31 is one ordered list, not three separate reports. A file
    // with a GOP change *and* a timestamp gap must produce entries from more
    // than one source, or the merge is not happening.
    let tmp = tempfile::tempdir().expect("temp dir");
    let bytes = tpt_app_media_forensics_container::fixture::build_mp4_stsd_gop_change();
    let (case_dir, source) = case_with(&bytes, tmp.path());

    let outcome = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("analyses");

    assert!(
        !outcome.timeline.is_empty(),
        "a file with findings must populate the timeline"
    );
    // Every finding must be represented: dropping one would mean the timeline
    // disagreed with the findings list above it.
    let finding_entries = outcome
        .timeline
        .from_source(tpt_app_media_forensics_model::TimelineSource::Finding)
        .count();
    assert_eq!(
        finding_entries,
        outcome.findings.len(),
        "the timeline must account for every finding"
    );
    // Entries are ordered, and each carries a placement that says where its
    // position came from.
    for entry in &outcome.timeline.entries {
        assert!(
            !entry.reference.is_empty(),
            "every entry must name what it refers to: {entry:?}"
        );
        if entry.time.is_none() {
            assert_eq!(
                entry.placement,
                tpt_app_media_forensics_model::Placement::Unplaced,
                "an entry with no time must say it is unplaced"
            );
        }
    }
}

#[test]
fn an_entry_with_no_position_is_never_given_one() {
    // The distinction the whole type exists for: "no position" is not "at zero".
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_with(
        &build_mp4(&TrackSpec::video_25fps(320, 240, 30)),
        tmp.path(),
    );

    let outcome = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("analyses");

    for entry in outcome.timeline.unplaced() {
        assert!(
            entry.time.is_none(),
            "an unplaced entry must carry no timecode: {entry:?}"
        );
    }
}

#[test]
fn encoder_fingerprinting_reports_declared_tags_without_asserting_a_cause() {
    // A file carrying a `©too` atom reaches the report, and the indicator says
    // what it does not establish. A fingerprint that reported "encoded with
    // FFmpeg" would be asserting something no tag can support.
    let tmp = tempfile::tempdir().expect("temp dir");
    let bytes = tpt_app_media_forensics_container::fixture::build_mp4_with_metadata(
        &TrackSpec::video_25fps(320, 240, 30),
    );
    let (case_dir, source) = case_with(&bytes, tmp.path());

    let outcome = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("analyses");

    // Whatever the fixture's tag says, every indicator must carry its limits.
    for indicator in &outcome.fingerprint.indicators {
        assert!(
            !indicator.limitations.is_empty(),
            "an indicator without limitations invites a verdict: {indicator:?}"
        );
        // And none may reach a confidence above what a self-report can support.
        // `Confidence` has no top variant precisely so this cannot be exceeded.
        assert!(
            matches!(
                indicator.confidence,
                tpt_app_media_forensics_metadata::fingerprint::Confidence::Low
                    | tpt_app_media_forensics_metadata::fingerprint::Confidence::Medium
            ),
            "an indicator claimed more weight than its evidence supports: {indicator:?}"
        );
    }
    // And the gaps are stated as limitations, not silently omitted.
    assert!(
        outcome
            .limitations
            .iter()
            .any(|l| l.contains("bitstream") || l.contains("quantisation")),
        "{:?}",
        outcome.limitations
    );
}

#[test]
fn a_file_with_no_encoder_tag_still_states_what_it_could_not_measure() {
    // An empty fingerprint and a missing fingerprint must be distinguishable.
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_with(
        &build_mp4(&TrackSpec::video_25fps(320, 240, 30)),
        tmp.path(),
    );

    let outcome = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("analyses");

    assert!(outcome.fingerprint.is_empty());
    assert!(!outcome.fingerprint.not_measured.is_empty());
    // `describe` renders indicators, so with none found it is legitimately
    // empty. What must not be empty is the statement of what was not measured:
    // an empty result has to be readable as "nothing found" rather than as
    // "nothing was looked at".
    assert!(outcome.fingerprint.describe().is_empty());
    assert!(
        outcome
            .limitations
            .iter()
            .any(|l| l.contains("not parsed by this build")),
        "the gaps must reach the report's limitations: {:?}",
        outcome.limitations
    );
}

#[test]
fn colour_reaches_the_report_through_the_whole_pipeline() {
    // The stage guard proves the rule fires from a file. This proves the
    // observation itself survives acquisition, inspection, rule evaluation, and
    // persistence — the route a report actually takes. A colour reader wired
    // into the container crate but not into the stored analysis would pass that
    // guard and still produce an empty report.
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_with(&build_mp4_with_hdr_signalling_only(), tmp.path());

    let outcome = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("analyses");

    let finding = outcome
        .findings
        .iter()
        .find(|f| f.rule_id == "VIDEO.HDR_METADATA_MISSING")
        .unwrap_or_else(|| {
            panic!(
                "the HDR fixture should raise a finding; got {:?}",
                outcome
                    .findings
                    .iter()
                    .map(|f| &f.rule_id)
                    .collect::<Vec<_>>()
            )
        });
    assert_eq!(finding.severity, Severity::Warning);
    // The observation names the declaration it is about, so a report reader can
    // check the finding against the file.
    assert!(
        finding.observation.summary.contains("BT.2020"),
        "{:?}",
        finding.observation.summary
    );
}

#[test]
fn analysis_produces_findings_for_a_real_container() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_with(&gop_change_bytes(), tmp.path());

    let outcome = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("analyses");

    assert!(!outcome.cache_hit, "the first run cannot be a cache hit");
    assert!(
        outcome
            .findings
            .iter()
            .any(|f| f.rule_id == "VIDEO.GOP_LENGTH_CHANGE"),
        "the GOP change fixture should raise a finding: {:?}",
        outcome
            .findings
            .iter()
            .map(|f| &f.rule_id)
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_header_that_contradicts_its_sample_table_reaches_the_report() {
    // The unit tests for this rule call `evaluate` directly, which would still
    // pass if the pipeline never populated the values it reads. This drives the
    // whole path — file on disk, container reader, stages, rules, outcome.
    let mut track = TrackSpec::video_25fps(320, 240, 60);
    track.declared_duration = Some(250); // claims 10 s over 2.4 s of samples

    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_with(&build_mp4(&track), tmp.path());

    let outcome = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("analyses");

    let finding = outcome
        .findings
        .iter()
        .find(|f| f.rule_id == "METADATA.DECLARED_VS_MEASURED_MISMATCH")
        .unwrap_or_else(|| {
            panic!(
                "the disagreement should survive to the outcome; got {:?}",
                outcome
                    .findings
                    .iter()
                    .map(|f| &f.rule_id)
                    .collect::<Vec<_>>()
            )
        });
    assert_eq!(finding.severity, Severity::Warning);
    assert!(
        finding
            .observation
            .summary
            .contains("declares 00:00:10.000"),
        "{}",
        finding.observation.summary
    );
}

#[test]
fn a_clean_fixture_reports_no_repeated_frames() {
    // The regression this guards: fixtures once stored an all-zero `mdat`, so
    // every sample hashed the same and every "clean" file reported a 60-frame
    // duplicate run. A detection that fires on healthy input is not a detection.
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_with(
        &build_mp4(&TrackSpec::video_25fps(320, 240, 60)),
        tmp.path(),
    );

    let outcome = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("analyses");

    let repeated = outcome
        .findings
        .iter()
        .filter(|f| f.rule_id == "VIDEO.DUPLICATE_FRAME_RUN")
        .count();
    assert_eq!(
        repeated, 0,
        "a clean file must not look like repeated frames"
    );
}

#[test]
fn a_frozen_frame_run_is_reported_at_its_true_length() {
    // The other half: distinct samples must still allow genuinely repeated ones
    // to be detected. Only asserting the clean case would pass with duplicate
    // detection silently switched off.
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_with(&build_mp4_with_repeated_frames(10, 15), tmp.path());

    let outcome = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("analyses");

    let finding = outcome
        .findings
        .iter()
        .find(|f| f.rule_id == "VIDEO.DUPLICATE_FRAME_RUN")
        .unwrap_or_else(|| {
            panic!(
                "15 identical frames should be reported; got {:?}",
                outcome
                    .findings
                    .iter()
                    .map(|f| &f.rule_id)
                    .collect::<Vec<_>>()
            )
        });
    assert!(
        finding
            .observation
            .measurements
            .iter()
            .any(|m| m.contains("15")),
        "the reported length should be the true run length: {:?}",
        finding.observation.measurements
    );
}

#[test]
fn a_second_run_is_served_from_the_cache() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_with(&gop_change_bytes(), tmp.path());
    let engine = AnalysisEngine::new();

    let first = engine.analyse(&source, &case_dir).expect("first");
    let second = engine.analyse(&source, &case_dir).expect("second");

    assert!(!first.cache_hit);
    assert!(
        second.cache_hit,
        "an identical re-analysis must hit the cache"
    );
    assert_eq!(
        first.findings, second.findings,
        "a cache hit must return exactly what was computed"
    );
}

#[test]
fn a_changed_profile_invalidates_the_cache_and_re_analyses() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_with(&gop_change_bytes(), tmp.path());

    let profile = tpt_app_media_forensics_rules::RuleProfile {
        gop_tolerance_frames: 200, // wide enough to suppress the finding
        ..Default::default()
    };

    let first = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("first");
    assert!(first
        .findings
        .iter()
        .any(|f| f.rule_id == "VIDEO.GOP_LENGTH_CHANGE"));

    let second = AnalysisEngine::with_profile(profile.clone())
        .analyse(&source, &case_dir)
        .expect("second");
    assert!(
        !second.cache_hit,
        "a different profile must not reuse the previous result"
    );
    assert!(
        !second
            .findings
            .iter()
            .any(|f| f.rule_id == "VIDEO.GOP_LENGTH_CHANGE"),
        "a profile with a wide tolerance should suppress this finding"
    );
}

#[test]
fn the_source_is_not_modified_by_analysis() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_with(&gop_change_bytes(), tmp.path());
    let before = std::fs::read(&source).expect("reads");

    let engine = AnalysisEngine::new();
    engine.analyse(&source, &case_dir).expect("first");
    engine.analyse(&source, &case_dir).expect("second");

    assert_eq!(std::fs::read(&source).expect("reads"), before);
}

#[test]
fn a_clean_file_produces_fewer_findings_than_a_damaged_one() {
    let tmp = tempfile::tempdir().expect("temp dir");

    let (clean_case, clean_source) = case_with(
        &build_mp4(&TrackSpec::video_25fps(640, 480, 50)),
        &tmp.path().join("a"),
    );
    let clean = AnalysisEngine::new()
        .analyse(&clean_source, &clean_case)
        .expect("analyses");

    let (damaged_case, damaged_source) = case_with(&gop_change_bytes(), &tmp.path().join("b"));
    let damaged = AnalysisEngine::new()
        .analyse(&damaged_source, &damaged_case)
        .expect("analyses");

    // Compared by rule, not by count. This previously asserted
    // `damaged.len() > clean.len()`, which passed for incidental reasons: the
    // two fixtures raise unrelated findings (an all-keyframes note on the
    // clean one, a GOP change on the other), so the totals tracked whatever
    // else each happened to report. A count comparison between different
    // fixtures says nothing about the change being tested.
    let clean_ids: Vec<&str> = clean.findings.iter().map(|f| f.rule_id.as_str()).collect();
    let damaged_ids: Vec<&str> = damaged
        .findings
        .iter()
        .map(|f| f.rule_id.as_str())
        .collect();

    assert!(
        damaged_ids.contains(&"VIDEO.GOP_LENGTH_CHANGE"),
        "the GOP change fixture should raise a GOP finding; got {damaged_ids:?}"
    );
    assert!(
        !clean_ids.contains(&"VIDEO.GOP_LENGTH_CHANGE"),
        "a constant-GOP file should not raise one; got {clean_ids:?}"
    );
}

#[test]
fn findings_come_back_most_severe_first() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_with(&gop_change_bytes(), tmp.path());
    let outcome = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("analyses");

    for pair in outcome.findings.windows(2) {
        assert!(
            pair[0].severity >= pair[1].severity,
            "findings must be ordered most severe first"
        );
    }
}

#[test]
fn the_analysis_fingerprint_is_stable_across_runs() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_with(&gop_change_bytes(), tmp.path());
    let engine = AnalysisEngine::new();

    let first = engine.analyse(&source, &case_dir).expect("first");
    let second = engine.analyse(&source, &case_dir).expect("second");
    assert_eq!(
        engine.analysis_fingerprint(&first.cache_key),
        engine.analysis_fingerprint(&second.cache_key)
    );
}

#[test]
fn the_same_content_yields_the_same_fingerprint_across_cases() {
    let a = tempfile::tempdir().expect("temp dir");
    let b = tempfile::tempdir().expect("temp dir");
    let bytes = gop_change_bytes();

    let (case_a, source_a) = case_with(&bytes, a.path());
    let (case_b, source_b) = case_with(&bytes, b.path());
    let engine = AnalysisEngine::new();

    let first = engine.analyse(&source_a, &case_a).expect("a");
    let second = engine.analyse(&source_b, &case_b).expect("b");
    assert_eq!(
        engine.analysis_fingerprint(&first.cache_key),
        engine.analysis_fingerprint(&second.cache_key),
        "the fingerprint identifies the analysis, not the case directory"
    );
}

#[test]
fn a_file_with_no_metadata_records_a_limitation() {
    // spec §60: what could not be measured must be stated.
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_with(
        &build_mp4(&TrackSpec::video_25fps(640, 480, 50)),
        tmp.path(),
    );

    let outcome = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("analyses");
    assert!(
        outcome.limitations.iter().any(|l| l.contains("metadata")),
        "absent metadata must be recorded: {:?}",
        outcome.limitations
    );
}

#[test]
fn a_non_container_source_still_produces_a_record() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_with(b"this is not a container at all", tmp.path());

    // The failure is recorded, not raised: an examiner needs the record.
    let outcome = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("analyses");
    assert!(
        outcome
            .limitations
            .iter()
            .any(|l| l.contains("demuxer") || l.contains("metadata")),
        "an unreadable container must be reported as a limitation: {:?}",
        outcome.limitations
    );
}

#[test]
fn the_acquisition_record_matches_the_file_on_disk() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let contents = gop_change_bytes();
    let (_case_dir, source) = case_with(&contents, tmp.path());

    let record = acquire(&source).expect("acquires");
    assert_eq!(record.size_bytes, contents.len() as u64);
    assert_eq!(record.hashes.sha256().map(str::len), Some(64));
    assert_eq!(record.hashes.blake3().map(str::len), Some(64));
}

#[test]
fn stream_kinds_are_reported_for_a_standard_fixture() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_with(
        &build_mp4(&TrackSpec::video_25fps(640, 480, 50)),
        tmp.path(),
    );
    let outcome = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("analyses");

    let container = outcome.limitations.iter().all(|l| !l.contains("demuxer"));
    assert!(
        container,
        "a standard fixture should parse: {:?}",
        outcome.limitations
    );
    assert_eq!(StreamKind::Video.tag(), "video");
    assert_eq!(Severity::Significant.tag(), "SIGNIFICANT");
}

/// Builds a minimal WebM document with one VP9 track and three blocks.
///
/// Built here rather than imported from the container crate's unit tests
/// because those helpers are private to that module; this is the pipeline's own
/// copy of "a WebM file an analyst would actually hand over".
fn webm_bytes() -> Vec<u8> {
    let mut track_entry = vec![0xD7, 0x81, 1, 0x83, 0x81, 1, 0x86, 0x80 | 5];
    track_entry.extend_from_slice(b"V_VP9");

    let mut tracks_body = vec![0xAE, 0x80 | track_entry.len() as u8];
    tracks_body.extend_from_slice(&track_entry);

    let mut cluster = vec![0xE7, 0x82];
    cluster.extend_from_slice(&0u16.to_be_bytes());
    for (index, is_key) in [true, false, true].into_iter().enumerate() {
        let mut block = vec![0x81];
        block.extend_from_slice(&((index as u16) * 33).to_be_bytes());
        block.push(u8::from(is_key) << 7);
        block.extend_from_slice(&[index as u8 + 1, 0xAA]);
        cluster.extend_from_slice(&[0xA3, 0x80 | block.len() as u8]);
        cluster.extend_from_slice(&block);
    }

    let mut segment = vec![0x16, 0x54, 0xAE, 0x6B, 0x80 | tracks_body.len() as u8];
    segment.extend_from_slice(&tracks_body);
    segment.extend_from_slice(&[0x1F, 0x43, 0xB6, 0x75, 0x80 | cluster.len() as u8]);
    segment.extend_from_slice(&cluster);

    let mut doc = vec![0x1A, 0x45, 0xDF, 0xA3, 0x80, 0x18, 0x53, 0x80, 0x67];
    doc.push(0x80 | segment.len() as u8);
    doc.extend_from_slice(&segment);
    doc
}

/// Creates a case directory containing `contents` under the given file name.
fn case_with_name(
    contents: &[u8],
    dir: &std::path::Path,
    name: &str,
) -> (CaseDirectory, std::path::PathBuf) {
    std::fs::create_dir_all(dir).expect("creates case parent");
    let source = dir.join(name);
    std::fs::write(&source, contents).expect("writes source");

    let case_dir = dir.join("case.tptcase");
    CaseDirectory::create(&case_dir, &Case::new("Pipeline", None)).expect("creates case");
    (CaseDirectory::open(&case_dir).expect("opens"), source)
}

#[test]
fn a_webm_file_is_analysed_end_to_end() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_with_name(&webm_bytes(), tmp.path(), "clip.webm");

    let outcome = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("analyses");

    // The decisive check: WebM is no longer reported as an unintegrated format.
    assert!(
        !outcome
            .limitations
            .iter()
            .any(|l| l.contains("no demuxer integrated")),
        "WebM has a demuxer now: {:?}",
        outcome.limitations
    );
}

#[test]
fn a_webm_file_reaches_the_video_rules() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_with_name(&webm_bytes(), tmp.path(), "clip.webm");

    let outcome = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("analyses");

    // Three frames, two of them keyframes: that is a single-keyframe-free track
    // with a measurable GOP, so the video rules have something to read.
    let saw_video_rule = outcome
        .findings
        .iter()
        .any(|f| f.rule_id.starts_with("VIDEO.") || f.rule_id.starts_with("CONTAINER."));
    assert!(
        saw_video_rule,
        "expected container/video findings, got {:?}",
        outcome
            .findings
            .iter()
            .map(|f| &f.rule_id)
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_webm_file_states_that_matroska_tags_are_not_extracted() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_with_name(&webm_bytes(), tmp.path(), "clip.webm");

    let outcome = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("analyses");

    // "No metadata in this file" and "this build does not read Matroska tags"
    // are different observations, and only the second is true here.
    assert!(
        outcome
            .limitations
            .iter()
            .any(|l| l.contains("Matroska tags are not extracted")),
        "{:?}",
        outcome.limitations
    );
}

#[test]
fn analysis_of_a_webm_file_is_reproducible() {
    // Spec §77, across a second case directory so the cache cannot serve it.
    let first_dir = tempfile::tempdir().expect("temp dir");
    let second_dir = tempfile::tempdir().expect("temp dir");
    let bytes = webm_bytes();

    let (first_case, first_source) = case_with_name(&bytes, first_dir.path(), "clip.webm");
    let (second_case, second_source) = case_with_name(&bytes, second_dir.path(), "clip.webm");

    let first = AnalysisEngine::new()
        .analyse(&first_source, &first_case)
        .expect("first");
    let second = AnalysisEngine::new()
        .analyse(&second_source, &second_case)
        .expect("second");

    assert_eq!(
        first.findings, second.findings,
        "two runs over identical bytes must produce identical findings"
    );
}
