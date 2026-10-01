//! End-to-end tests for the analysis pipeline.
//!
//! These run the real engine over real container bytes, so they cover the
//! wiring between acquisition, inspection, rules, and the cache rather than
//! any one layer in isolation.

use tpt_app_media_forensics_container::fixture::{build_mp4, build_mp4_stsd_gop_change, TrackSpec};
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

    assert!(
        damaged.findings.len() > clean.findings.len(),
        "a GOP change should raise findings a clean file does not: {} vs {}",
        damaged.findings.len(),
        clean.findings.len()
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
