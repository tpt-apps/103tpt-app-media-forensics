//! Tests for §55 background workers and §56 parallelism.
//!
//! # The property everything else rests on
//!
//! Parallelism must be invisible. Two runs of the same file have to produce the
//! same findings, the same limitations *in the same order*, and the same progress
//! events, whether the work ran on one thread or four. If that is not true then
//! spec §77 is broken: a report would depend on how fast one decoder happened to
//! be, and two examiners comparing notes could disagree about a file that never
//! changed.
//!
//! Those tests come first, because they are the ones that would catch a
//! regression in the merge order — and a merge order is exactly the thing that
//! looks correct in review and breaks under a scheduler.

use std::sync::{Arc, Mutex};

use tpt_app_media_forensics_container::fixture::{build_mp4_with_metadata, TrackSpec};
use tpt_app_media_forensics_core::case_dir::CaseDirectory;
use tpt_app_media_forensics_core::pipeline::WorkerBudget;
use tpt_app_media_forensics_core::progress::Stage;
use tpt_app_media_forensics_core::{
    AnalysisEngine, AnalysisJob, AnalysisOutcome, CoreError, Progress, ProgressTracker,
};
use tpt_app_media_forensics_model::Case;

/// Writes `contents` as `sample.mp4` inside a fresh case, returning both.
fn case_with(contents: &[u8], dir: &std::path::Path) -> (CaseDirectory, std::path::PathBuf) {
    std::fs::create_dir_all(dir).expect("creates case parent");
    let source = dir.join("sample.mp4");
    std::fs::write(&source, contents).expect("writes source");

    let case_dir = dir.join("case.tptcase");
    CaseDirectory::create(&case_dir, &Case::new("Concurrency", None)).expect("creates case");
    (CaseDirectory::open(&case_dir).expect("opens"), source)
}

/// A fixture that reaches three of the four concurrent branches.
///
/// Metadata, the sample index, and the bitrate report all produce measurements;
/// audio and Tier-2 return limitations, which is the point — the branches have to
/// agree on *where those limitations land in the list* as much as on the
/// measurements themselves.
fn fixture_bytes() -> Vec<u8> {
    build_mp4_with_metadata(&TrackSpec::video_25fps(320, 240, 30))
}

/// The identifiers of an outcome's findings, in order.
fn finding_ids(outcome: &AnalysisOutcome) -> Vec<String> {
    outcome.findings.iter().map(|f| f.id.to_string()).collect()
}

/// Runs one analysis at `budget` and returns the progress it reported.
fn events_at(bytes: &[u8], dir: &std::path::Path, budget: WorkerBudget) -> Vec<Progress> {
    let (case_dir, source) = case_with(bytes, dir);
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let tracker = ProgressTracker::reporting(move |event| {
        sink.lock().expect("lock").push(event);
    });

    AnalysisEngine::new()
        .with_worker_budget(budget)
        .analyse_with(&source, &case_dir, &tracker)
        .expect("analysis runs");

    let events = events.lock().expect("lock").clone();
    events
}

#[test]
fn the_parallel_path_produces_exactly_what_the_serial_path_does() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let bytes = fixture_bytes();

    let (serial_case, source) = case_with(&bytes, &tmp.path().join("serial"));
    let serial = AnalysisEngine::new()
        .with_worker_budget(WorkerBudget::serial())
        .analyse(&source, &serial_case)
        .expect("serial analysis");

    let (parallel_case, source) = case_with(&bytes, &tmp.path().join("parallel"));
    let parallel = AnalysisEngine::new()
        .with_worker_budget(WorkerBudget::new(8))
        .analyse(&source, &parallel_case)
        .expect("parallel analysis");

    assert_eq!(
        finding_ids(&serial),
        finding_ids(&parallel),
        "the findings must not depend on how the branches were scheduled"
    );
    assert_eq!(
        serial.limitations, parallel.limitations,
        "limitations must match in value *and* order: a report reads them in the order \
         they were recorded, so a scheduling-dependent sequence is a scheduling-dependent \
         report"
    );
    assert_eq!(
        serial.asset.sha256(),
        parallel.asset.sha256(),
        "both paths must examine the same bytes"
    );

    // Guard against the assertion above being vacuous. This fixture is audio-less
    // and H.264, so two of the four branches report that they could not measure
    // something — which is exactly what makes the *order* of `limitations` a real
    // thing to compare rather than an empty list.
    assert!(
        !serial.limitations.is_empty(),
        "the fixture must produce limitations, or this test compares two empty lists"
    );
    assert!(
        serial.limitations.len() >= 2,
        "at least two branches must decline to measure, or their relative order cannot \
         be observed: {:?}",
        serial.limitations
    );
}

#[test]
fn a_serial_budget_still_reports_the_same_progress_as_a_parallel_one() {
    // The progress stream is part of what a UI renders, so it has to be stable
    // too — otherwise the same examination shows a different bar depending on the
    // machine it ran on.
    let tmp = tempfile::tempdir().expect("temp dir");
    let bytes = fixture_bytes();

    let serial = events_at(&bytes, &tmp.path().join("serial"), WorkerBudget::serial());
    let parallel = events_at(&bytes, &tmp.path().join("parallel"), WorkerBudget::new(8));

    assert_eq!(
        serial, parallel,
        "the progress a caller sees must not reveal whether the work was threaded"
    );
    assert!(!serial.is_empty(), "a run must report something");
}

#[test]
fn a_run_reports_every_concurrent_branch_exactly_once() {
    // The guard against a stage that reports progress it never performed, and
    // against one that reports twice.
    let events = events_at(
        &fixture_bytes(),
        &tempfile::tempdir().expect("temp dir").path().join("run"),
        WorkerBudget::new(8),
    );
    let reported: Vec<Stage> = events
        .iter()
        .filter_map(|event| match event {
            Progress::BranchFinished { stage, .. } => Some(*stage),
            _ => None,
        })
        .collect();

    assert_eq!(
        reported.len(),
        Stage::CONCURRENT.len(),
        "each concurrent stage must report exactly once: {reported:?}"
    );
    for stage in Stage::CONCURRENT {
        assert!(
            reported.contains(&stage),
            "{stage:?} never reported; a stage listed as concurrent that never runs would \
             otherwise be invisible"
        );
    }
}

#[test]
fn the_reported_fraction_never_goes_backwards() {
    let events = events_at(
        &fixture_bytes(),
        &tempfile::tempdir().expect("temp dir").path().join("run"),
        WorkerBudget::new(8),
    );

    let mut previous = 0.0;
    for event in &events {
        let fraction = event.fraction();
        assert!(
            fraction >= previous,
            "the bar went backwards at {event:?}: {fraction} after {previous}"
        );
        previous = fraction;
    }
    assert_eq!(previous, 1.0, "a completed analysis must end at 1.0");
}

/// Starts a job over `bytes` in a fresh case under `name`.
fn job_over(bytes: &[u8], dir: &std::path::Path) -> AnalysisJob {
    let (case_dir, source) = case_with(bytes, dir);
    AnalysisJob::spawn(
        Arc::new(AnalysisEngine::new()),
        source,
        case_dir,
        ProgressTracker::none(),
    )
    .expect("the worker starts")
}

#[test]
fn a_worker_analyses_the_file_to_the_same_result_as_the_caller_would() {
    // The point of a background worker: the same engine, the same findings, run
    // somewhere else. If a worker's output could differ from a direct call, then
    // "run it in the background to keep the UI responsive" would change what the
    // application concludes — which is the one thing it must never do.
    let tmp = tempfile::tempdir().expect("temp dir");
    let bytes = fixture_bytes();

    let (direct_case, source) = case_with(&bytes, &tmp.path().join("direct"));
    let direct = AnalysisEngine::new()
        .analyse(&source, &direct_case)
        .expect("direct analysis");

    let on_worker = job_over(&bytes, &tmp.path().join("worker"))
        .join()
        .expect("the worker finishes");

    assert_eq!(finding_ids(&direct), finding_ids(&on_worker));
    assert_eq!(direct.limitations, on_worker.limitations);
}

#[test]
fn a_worker_reports_itself_finished_without_being_asked_to_stop() {
    // `is_finished` is what a UI polls instead of blocking, so it has to become
    // true — and the caller has to be able to keep asking.
    let tmp = tempfile::tempdir().expect("temp dir");
    let job = job_over(&fixture_bytes(), tmp.path());

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !job.is_finished() {
        assert!(
            std::time::Instant::now() < deadline,
            "the worker never reported itself finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(job.is_finished(), "finished must stay finished");
    job.join().expect("the worker finishes");
}

#[test]
fn cancelling_a_worker_stops_it_and_is_reported_as_cancellation() {
    // Spec §55 lists cancellation alongside background workers, and the two only
    // compose if the token the caller holds is the token the worker observes. A
    // cancel button on another thread has to reach the analysis.
    let tmp = tempfile::tempdir().expect("temp dir");
    let job = job_over(&fixture_bytes(), tmp.path());

    job.cancel();
    let error = job
        .join()
        .expect_err("a cancelled analysis must not succeed");

    assert!(
        matches!(error, CoreError::Cancelled),
        "a cancelled worker must report cancellation, not failure: {error:?}"
    );
}

#[test]
fn a_token_held_before_the_work_starts_still_cancels_it() {
    // The UI's shape: build the token, wire it to the button, then start. If the
    // token had to be taken *from* a running job, a click during setup would be
    // dropped on the floor.
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_with(&fixture_bytes(), tmp.path());

    let tracker = ProgressTracker::none();
    tracker.cancellation().cancel();

    let job = AnalysisJob::spawn(Arc::new(AnalysisEngine::new()), source, case_dir, tracker)
        .expect("the worker starts");

    let outcome = job.join();
    assert!(
        matches!(outcome, Err(CoreError::Cancelled)),
        "a job cancelled before its first stage must not do any work, got {outcome:?}"
    );
}

#[test]
fn cancelling_twice_is_not_an_error() {
    // A user pressing cancel twice, or a teardown path cancelling after the
    // button already did, must not produce a failure.
    let tmp = tempfile::tempdir().expect("temp dir");
    let job = job_over(&fixture_bytes(), tmp.path());

    let token = job.cancellation();
    token.cancel();
    job.cancel();

    assert!(job.cancellation().is_cancelled());
    assert!(matches!(job.join(), Err(CoreError::Cancelled)));
}

#[test]
fn a_worker_knows_which_file_it_is_analysing() {
    // A caller driving several jobs needs to tell them apart in whatever UI it
    // is driving, and the job has no identifier to offer beyond the path.
    let tmp = tempfile::tempdir().expect("temp dir");
    let job = job_over(&fixture_bytes(), tmp.path());

    assert!(job.source().ends_with("sample.mp4"), "{:?}", job.source());
    job.join().expect("the worker finishes");
}
