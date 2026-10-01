//! Tests for batch analysis (spec §48-49).
//!
//! Batch is where the engine meets messy real input, so these cover the parts
//! that are easy to get wrong: what gets scanned, what gets skipped, and
//! whether one bad file can take down a whole run.

use std::path::Path;
use tpt_app_media_forensics_container::fixture::{build_mp4, build_mp4_stsd_gop_change, TrackSpec};
use tpt_app_media_forensics_core::batch::{self, FileOutcome};
use tpt_app_media_forensics_core::case_dir::CaseDirectory;
use tpt_app_media_forensics_core::AnalysisEngine;
use tpt_app_media_forensics_model::Case;

/// Builds a case directory next to `root`, returning it.
fn case_at(root: &Path) -> CaseDirectory {
    std::fs::create_dir_all(root).expect("creates case parent");
    CaseDirectory::create(root.join("case.tptcase"), &Case::new("Batch", None))
        .expect("creates case")
}

/// Builds an intake tree: a clean file, a damaged file, a nested clean file,
/// a text file, and an `.mp4` that is not media at all.
fn intake(root: &Path) {
    std::fs::create_dir_all(root.join("sub")).expect("creates subdir");
    std::fs::write(
        root.join("clean.mp4"),
        build_mp4(&TrackSpec::video_25fps(320, 240, 30)),
    )
    .expect("writes clean");
    std::fs::write(root.join("damaged.mp4"), build_mp4_stsd_gop_change()).expect("writes damaged");
    std::fs::write(
        root.join("sub/nested.mp4"),
        build_mp4(&TrackSpec::video_25fps(320, 240, 30)),
    )
    .expect("writes nested");
    std::fs::write(root.join("notes.txt"), b"not media at all").expect("writes text");
    std::fs::write(root.join("fake.mp4"), b"this is not an mp4 file").expect("writes fake");
}

#[test]
fn a_batch_analyses_every_real_media_file_including_nested_ones() {
    let tmp = tempfile::tempdir().expect("temp dir");
    intake(tmp.path());
    let case = case_at(tmp.path());

    let outcome = batch::run(&AnalysisEngine::new(), tmp.path(), &case).expect("runs");
    let (analysed, failed, skipped) = outcome.counts();

    assert_eq!(analysed, 3, "three real media files were staged");
    assert_eq!(failed, 0);
    assert_eq!(skipped, 0);

    let names: Vec<String> = outcome
        .results
        .iter()
        .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.contains(&"nested.mp4".to_owned()),
        "nested files are analysed"
    );
    assert!(
        !names.contains(&"notes.txt".to_owned()),
        "text is not media"
    );
    assert!(
        !names.contains(&"fake.mp4".to_owned()),
        "a fake .mp4 is skipped"
    );
}

#[test]
fn files_are_visited_in_a_deterministic_order() {
    // spec 77: two runs over an unchanged tree must visit files in the same
    // order, or the case record differs between them for no reason.
    let tmp = tempfile::tempdir().expect("temp dir");
    intake(tmp.path());
    let case = case_at(tmp.path());
    let engine = AnalysisEngine::new();

    let first = batch::run(&engine, tmp.path(), &case).expect("first");
    let second = batch::run(&engine, tmp.path(), &case).expect("second");

    let paths = |o: &batch::BatchOutcome| -> Vec<String> {
        o.results
            .iter()
            .map(|(p, _)| p.display().to_string())
            .collect()
    };
    assert_eq!(paths(&first), paths(&second));
    assert!(
        paths(&first).windows(2).all(|w| w[0] <= w[1]),
        "results must be sorted by path"
    );
}

#[test]
fn a_second_batch_over_the_same_files_hits_the_cache() {
    let tmp = tempfile::tempdir().expect("temp dir");
    intake(tmp.path());
    let case = case_at(tmp.path());
    let engine = AnalysisEngine::new();

    batch::run(&engine, tmp.path(), &case).expect("first");
    let second = batch::run(&engine, tmp.path(), &case).expect("second");

    let hits = second
        .results
        .iter()
        .filter(|(_, o)| matches!(o, FileOutcome::Analysed(a) if a.cache_hit))
        .count();
    assert_eq!(hits, 3, "every file should have been served from the cache");
}

#[test]
fn the_case_directory_is_never_scanned_into() {
    // Otherwise a batch would read its own evidence and outputs on the next run.
    let tmp = tempfile::tempdir().expect("temp dir");
    intake(tmp.path());
    let case = case_at(tmp.path());

    batch::run(&AnalysisEngine::new(), tmp.path(), &case).expect("first run");

    // Plant a media-looking file inside the case directory.
    let planted = case.root().join("planted.mp4");
    std::fs::write(&planted, build_mp4(&TrackSpec::video_25fps(320, 240, 30))).expect("plants");

    let outcome = batch::run(&AnalysisEngine::new(), tmp.path(), &case).expect("second run");
    assert!(
        !outcome
            .results
            .iter()
            .any(|(p, _)| p.starts_with(case.root())),
        "the case directory must be excluded from the scan"
    );
}

#[test]
fn a_damaged_file_still_contributes_findings_without_stopping_the_run() {
    let tmp = tempfile::tempdir().expect("temp dir");
    intake(tmp.path());
    let case = case_at(tmp.path());

    let outcome = batch::run(&AnalysisEngine::new(), tmp.path(), &case).expect("runs");
    let damaged = outcome
        .results
        .iter()
        .find(|(p, _)| p.ends_with("damaged.mp4"))
        .expect("damaged file was visited");

    assert!(
        damaged
            .1
            .findings()
            .iter()
            .any(|f| f.rule_id == "VIDEO.GOP_LENGTH_CHANGE"),
        "the damaged file should raise its finding"
    );
    // The point of the test: the clean files around it were still analysed.
    assert_eq!(outcome.counts().0, 3);
}

#[test]
fn every_failure_is_labelled_with_its_path() {
    // A batch failure that does not say which file failed is not actionable.
    let tmp = tempfile::tempdir().expect("temp dir");
    intake(tmp.path());
    let case = case_at(tmp.path());

    let outcome = batch::run(&AnalysisEngine::new(), tmp.path(), &case).expect("runs");
    for (path, result) in &outcome.results {
        if let FileOutcome::Failed { reason } = result {
            assert!(
                reason.contains(&path.display().to_string()),
                "the reason must name the file: {reason}"
            );
        }
    }
}

#[test]
fn an_empty_directory_produces_an_empty_batch_not_an_error() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let case = case_at(tmp.path());
    let empty = tmp.path().join("nothing");
    std::fs::create_dir_all(&empty).expect("creates");

    let outcome = batch::run(&AnalysisEngine::new(), &empty, &case).expect("runs");
    assert_eq!(outcome.counts(), (0, 0, 0));
    assert!(outcome.findings().is_empty());
}

#[test]
fn findings_are_collected_across_the_whole_batch() {
    let tmp = tempfile::tempdir().expect("temp dir");
    intake(tmp.path());
    let case = case_at(tmp.path());

    let outcome = batch::run(&AnalysisEngine::new(), tmp.path(), &case).expect("runs");
    let findings = outcome.findings();

    assert!(
        findings
            .iter()
            .any(|f| f.rule_id == "VIDEO.GOP_LENGTH_CHANGE"),
        "the batch's findings include the GOP change"
    );
    // Findings from different files carry different asset identifiers.
    let assets: std::collections::BTreeSet<String> =
        findings.iter().map(|f| f.asset_id.to_string()).collect();
    assert!(
        assets.len() >= 2,
        "findings should span more than one asset, got {}",
        assets.len()
    );
}

#[test]
fn a_source_file_is_not_modified_by_a_batch() {
    let tmp = tempfile::tempdir().expect("temp dir");
    intake(tmp.path());
    let case = case_at(tmp.path());
    let before = std::fs::read(tmp.path().join("damaged.mp4")).expect("reads");

    batch::run(&AnalysisEngine::new(), tmp.path(), &case).expect("runs");

    assert_eq!(
        std::fs::read(tmp.path().join("damaged.mp4")).expect("reads"),
        before
    );
}

#[test]
fn a_cache_directory_in_the_tree_is_skipped() {
    // An intake folder may contain a working cache; reading it would be waste.
    let tmp = tempfile::tempdir().expect("temp dir");
    intake(tmp.path());
    let cache = tmp.path().join("cache");
    std::fs::create_dir_all(&cache).expect("creates");
    std::fs::write(
        cache.join("stale.mp4"),
        build_mp4(&TrackSpec::video_25fps(320, 240, 30)),
    )
    .expect("writes");

    let case = case_at(tmp.path());
    let outcome = batch::run(&AnalysisEngine::new(), tmp.path(), &case).expect("runs");

    assert!(
        !outcome.results.iter().any(|(p, _)| p.starts_with(&cache)),
        "a directory named cache must not be descended into"
    );
    assert_eq!(outcome.counts().0, 3);
}
