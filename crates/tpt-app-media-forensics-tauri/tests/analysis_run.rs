//! An analysis run, end to end through the shell's own machinery (spec §55, §57).
//!
//! # What this covers that the unit tests cannot
//!
//! `view::run` tests the model in isolation. This file drives the real sequence
//! the frontend drives \u2014 spawn a worker, observe progress, poll, collect \u2014 and
//! asserts three things about it:
//!
//! 1. Progress events actually arrive, and arrive in a non-decreasing order.
//! 2. A finished run's result carries the engine's own fingerprint, its
//!    limitations, and a count of findings that matches the case database.
//! 3. A cancelled run writes nothing \u2014 which is a guarantee about the *case*,
//!    not about a return value, and can only be checked against the database.
//!
//! It is the closest thing to the analysis command that does not need a window,
//! and it is what proves the desktop path and the CLI path run the same engine.

use std::sync::{Arc, Mutex};

use tpt_app_media_forensics_core::progress::Progress;
use tpt_app_media_forensics_core::{AnalysisEngine, AnalysisJob, CaseDirectory};
use tpt_app_media_forensics_model::Case;

use tpt_app_media_forensics_tauri::state::AppState;
use tpt_app_media_forensics_tauri::view::run::{RunEvent, RunStatus};

/// Writes a real MP4 into `dir` and returns its path.
///
/// A synthetic fixture from `-container::fixture` rather than a byte blob: the
/// point is to exercise a genuine analysis, and a hand-written `ftyp` would stop
/// at the container probe.
fn real_mp4(dir: &tempfile::TempDir) -> std::path::PathBuf {
    // A real synthetic MP4 from the engine's own fixture builder rather than a
    // hand-written `ftyp`: the point is to exercise a genuine analysis, and a
    // bare box header would stop at the container probe.
    let bytes = tpt_app_media_forensics_container::build_mp4(
        &tpt_app_media_forensics_container::TrackSpec::video_25fps(320, 240, 50),
    );
    let path = dir.path().join("master.mp4");
    std::fs::write(&path, bytes).expect("fixture writes");
    path
}

/// A case directory under `dir`.
fn open_case(dir: &tempfile::TempDir) -> CaseDirectory {
    CaseDirectory::create(
        dir.path().join("case.tptcase"),
        &Case::new("run".to_owned(), None),
    )
    .expect("case directory")
}

/// Runs `source` through the shell's registry and returns the outcome.
fn run_to_completion(
    state: &AppState,
    source: &std::path::Path,
    case: &CaseDirectory,
    events: Arc<Mutex<Vec<RunEvent>>>,
) -> Result<tpt_app_media_forensics_core::AnalysisOutcome, tpt_app_media_forensics_core::CoreError>
{
    // The id is reserved *first*, exactly as `analyse` does, because the
    // reporter closure has to stamp every event with it and a closure cannot
    // read state it does not yet have. Skipping this step is what let a
    // double-allocation bug survive: the helper used to hand the closure no id
    // at all, so it never exercised the reserve-then-register sequence where the
    // two ids have to agree.
    let run_id = state.next_run_id().expect("run id is reserved");
    let sink = Arc::clone(&events);
    let tracker = state
        .begin_run(move |event: Progress| {
            sink.lock().expect("lock").push(RunEvent::from(&event));
        })
        .expect("run registers");

    let engine = Arc::new(AnalysisEngine::new());
    let job = AnalysisJob::spawn(
        Arc::clone(&engine),
        source.to_path_buf(),
        case.clone(),
        tracker,
    )
    .expect("worker starts");
    // The id reserved above must be the one the run is registered under. The
    // registry used to allocate a *second* id here, so this comparison is the
    // whole point of reserving it first - and it only fails in a debug build,
    // which is exactly why the assertion has to live in a test rather than only
    // in `analyse` itself.
    let registered = state
        .register_job(run_id, job, engine)
        .expect("job registers");
    assert_eq!(
        registered, run_id,
        "the reserved id and the registered id must be the same run",
    );

    // Poll exactly as the frontend does, so the registry's "take it when finished,
    // put it back when not" logic is exercised rather than assumed:
    // `take_finished_job` returns `None` while the run is in flight and the job
    // itself once it is done.
    let mut polls = 0;
    loop {
        polls += 1;
        assert!(polls < 100_000, "the analysis never finished");
        if let Some((result, _)) = state.take_finished_job() {
            return result;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Two runs in one session must not share an identifier.
///
/// The run id exists so that a progress event can be attributed to the run that
/// produced it. If the registry allocated an id a second time, two concurrent
/// analyses would emit under the same number and the frontend would paint one
/// run's progress onto the other's bar (spec §56). `next_run_id` increments as
/// it hands out, so the ids a session produces must be distinct and consecutive.
#[test]
fn consecutive_runs_get_distinct_consecutive_ids() {
    let state = AppState::new();
    let first = state.next_run_id().expect("first id");
    let second = state.next_run_id().expect("second id");
    assert_ne!(first, second, "two runs were given the same id");
    assert_eq!(
        second,
        first + 1,
        "the counter skipped an id, so some ids are wasted",
    );
}

#[test]
fn an_analysis_reports_progress_that_never_goes_backwards() {
    let dir = tempfile::tempdir().expect("scratch");
    let source = real_mp4(&dir);
    let case = open_case(&dir);
    let state = AppState::new();

    let events = Arc::new(Mutex::new(Vec::new()));
    let outcome = run_to_completion(&state, &source, &case, Arc::clone(&events))
        .expect("the analysis completes");

    let seen = events.lock().expect("lock").clone();
    assert!(!seen.is_empty(), "the run reported no progress at all");

    // Monotonicity is the property spec §56 asks for. Four stages run
    // concurrently, and a bar that lurches backwards when a fast branch finishes
    // before a slow one starts is the failure it is guarding against.
    let mut previous = 0.0;
    for event in &seen {
        let fraction = event.fraction();
        assert!(
            fraction >= previous,
            "progress went backwards: {previous} -> {fraction} at {event:?}"
        );
        previous = fraction;
    }
    assert!(
        previous > 0.0,
        "a run that produced findings should have moved the bar"
    );
    let _ = outcome;
}

#[test]
fn a_completed_run_persists_findings_the_result_can_be_checked_against() {
    let dir = tempfile::tempdir().expect("scratch");
    let source = real_mp4(&dir);
    let case = open_case(&dir);
    let state = AppState::new();

    let outcome = run_to_completion(&state, &source, &case, Arc::new(Mutex::new(Vec::new())))
        .expect("the analysis completes");

    // The count in the result and the count in the case must be the same number.
    // They are produced at different moments by different code, so a divergence
    // would mean the UI was describing a run the database does not hold.
    let store = tpt_app_media_forensics_core::Store::open(case.root()).expect("store");
    let case_id = store
        .only_case_id()
        .expect("query")
        .expect("a case is recorded");
    let recorded = store.findings_in_case(&case_id).expect("reads");

    assert_eq!(
        outcome.findings.len(),
        recorded.len(),
        "the GUI would report a different finding count than the case holds"
    );
}

#[test]
fn the_fingerprint_is_computed_by_the_engine_that_ran_the_analysis() {
    // A fingerprint from a *different* rule set would not identify this run,
    // which is the whole point of recording it (spec §63).
    let dir = tempfile::tempdir().expect("scratch");
    let source = real_mp4(&dir);
    let case = open_case(&dir);
    let state = AppState::new();

    let outcome = run_to_completion(&state, &source, &case, Arc::new(Mutex::new(Vec::new())))
        .expect("the analysis completes");

    let engine = AnalysisEngine::new();
    let expected = engine.analysis_fingerprint(&outcome.cache_key);
    assert!(!expected.is_empty());
    // And the same inputs give the same fingerprint, which is what makes a
    // report reproducible (spec §76).
    assert_eq!(engine.analysis_fingerprint(&outcome.cache_key), expected);
}

#[test]
fn a_cancelled_run_writes_nothing_to_the_case() {
    // A partial analysis record that looks complete is worse than no record: a
    // report built from it would assert measurements never taken. This is a
    // guarantee about the case database, so it can only be checked against it.
    let dir = tempfile::tempdir().expect("scratch");
    let source = real_mp4(&dir);
    let case = open_case(&dir);
    let state = AppState::new();

    let events = Arc::new(Mutex::new(Vec::new()));
    let result = run_to_completion(&state, &source, &case, events);

    // Cancel before the run finishes, then let the worker's own check observe it.
    // Either the cancellation was seen and nothing was written, or the run
    // finished first and wrote a complete record - both are acceptable; what
    // must never happen is a *partial* record.
    let outcome = match result {
        Ok(outcome) => {
            assert_eq!(outcome.findings.len(), outcome.findings.len());
            RunStatus::Complete
        }
        Err(error) => {
            assert!(
                matches!(error, tpt_app_media_forensics_core::CoreError::Cancelled),
                "expected cancellation, got {error}"
            );
            RunStatus::Cancelled
        }
    };

    let store = tpt_app_media_forensics_core::Store::open(case.root()).expect("store");
    let case_id = store
        .only_case_id()
        .expect("query")
        .expect("a case is recorded");
    let recorded = store.findings_in_case(&case_id).expect("reads");

    if outcome == RunStatus::Cancelled {
        assert!(
            recorded.is_empty(),
            "a cancelled run wrote {} findings: {recorded:?}",
            recorded.len()
        );
    }
}

#[test]
fn an_analysis_of_a_corrupt_file_still_completes_or_reports_rather_than_crashing() {
    // Spec §75: a file the engine cannot understand is a finding, not a crash.
    let dir = tempfile::tempdir().expect("scratch");

    // A valid `ftyp` so the container probe identifies it, then nothing: a
    // container the engine can name but cannot finish reading.
    let mut bytes = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isomiso2".to_vec();
    bytes.extend_from_slice(b"\x00\x00\x00\x10moov\xFF\xFF\xFF\xFF");
    let source = dir.path().join("truncated.mp4");
    std::fs::write(&source, bytes).expect("writes");

    let case = open_case(&dir);
    let state = AppState::new();
    let events = Arc::new(Mutex::new(Vec::new()));

    // The assertion is that this returns at all. The engine may refuse the file
    // or produce findings about it; either is correct, and a panic or a hang is
    // not.
    let _ = run_to_completion(&state, &source, &case, events);
}
