//! The open case and the running analysis.
//!
//! # What the application holds between commands
//!
//! Exactly two things: which case directory is open, and the token that can stop
//! work in progress. Both are behind a mutex because Tauri dispatches commands
//! on a thread pool and two of them can arrive at once — an analyst clicking
//! "analyse" twice, or a batch finishing while a screen is loading.
//!
//! # The engine is not in here
//!
//! `AnalysisEngine` is rebuilt per command rather than cached. It is a small
//! value — a rule set and a profile — and caching it would mean holding state
//! across commands that a cancelled run could leave half-configured. Rebuilding
//! costs microseconds and removes a class of bug entirely: there is no way for
//! two concurrent analyses to be using different rule sets, because each one
//! built its own.
//!
//! # Analysis runs on a worker thread, never on the UI thread
//!
//! Spec §57 requires the interface to stay responsive. `AnalysisEngine::analyse`
//! blocks for minutes on a long file, so the command layer hands it to
//! `AnalysisJob` and returns immediately. The job's own handle carries the
//! [`Cancellation`] token, so the cancel button is wired to the run by
//! construction rather than by a shared flag someone has to remember to set.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use tpt_app_media_forensics_core::progress::{Cancellation, Progress, ProgressTracker};
use tpt_app_media_forensics_core::{AnalysisEngine, AnalysisJob};

use crate::error::{ErrorKind, ShellError, ShellResult};
use tpt_app_media_forensics_core::CoreError;

/// The application's mutable state, held by Tauri.
#[derive(Debug, Default)]
pub struct AppState {
    inner: Arc<Mutex<Inner>>,
}

/// The state itself, guarded.
///
/// A manual `Debug` because [`AnalysisJob`] and [`AnalysisEngine`] do not
/// implement one — the former owns a thread handle, the latter a rule set — and
/// deriving would require printing both. What a log line needs is *whether*
/// something is running, not a dump of the worker's internals.
#[derive(Default)]
struct Inner {
    /// The case directory currently open, if any.
    case_dir: Option<PathBuf>,
    /// The tracker of the run in progress.
    ///
    /// Held whole rather than just its [`Cancellation`] because the tracker *is*
    /// the thing the analysis is given: `AnalysisJob` calls
    /// `progress.cancellation()` to obtain the token it hands back to the UI.
    /// Holding only a token here and a tracker elsewhere would be two objects
    /// that could disagree about whether a run is cancelled.
    ///
    /// Replaced whenever a new analysis starts, so a cancel can never reach a
    /// previous run that already finished.
    tracker: Option<ProgressTracker>,
    /// The worker job for the run in progress.
    job: Option<Box<AnalysisJob>>,
    /// The engine that run is using.
    ///
    /// Kept beside the job rather than rebuilt at collection time: the analysis
    /// fingerprint is computed by the engine that produced the findings, and
    /// rebuilding one would mean asking a *different* rule set to vouch for a
    /// result it did not produce (spec §63).
    engine: Option<Arc<AnalysisEngine>>,
    /// Monotonic counter for run identifiers.
    next_run: u64,
    /// The worker thread for a batch run in progress, if any.
    ///
    /// Separate from `job` because a batch is not an `AnalysisJob`: it is the
    /// engine's own directory walk, and it must not occupy the single analysis
    /// slot that cancel and poll reach. Sharing the slot would mean a batch run
    /// silently cancelling the analysis an analyst is watching.
    batch: Option<std::thread::JoinHandle<BatchResult>>,
}

/// What a batch run produced, once it has finished.
pub type BatchResult = Result<crate::view::batch::BatchView, CoreError>;

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inner")
            .field("case_dir", &self.case_dir)
            .field(
                "running",
                &self.job.as_ref().is_some_and(|j| !j.is_finished()),
            )
            .finish_non_exhaustive()
    }
}

impl AppState {
    /// State with no case open.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the open case directory.
    pub fn set_case_dir(&self, path: PathBuf) -> ShellResult<()> {
        self.lock()?.case_dir = Some(path);
        Ok(())
    }

    /// The open case directory.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::NoCaseOpen`] when nothing is open. A distinct error
    /// rather than `None`, because every caller would otherwise have to invent
    /// its own message for the most common thing an analyst does by accident:
    /// clicking through the screens before opening a case.
    pub fn case_dir(&self) -> ShellResult<PathBuf> {
        self.lock()?
            .case_dir
            .clone()
            .ok_or_else(|| ShellError::new(ErrorKind::NoCaseOpen, "no case is open"))
    }

    /// The open case directory, if any.
    #[must_use]
    pub fn maybe_case_dir(&self) -> Option<PathBuf> {
        self.lock().ok().and_then(|inner| inner.case_dir.clone())
    }

    /// Closes the current case.
    pub fn close_case(&self) -> ShellResult<()> {
        self.lock()?.case_dir = None;
        Ok(())
    }

    /// Starts a new run, returning the tracker to hand the analysis.
    ///
    /// `reporter` is invoked at each stage boundary with the engine's own
    /// progress event. The command layer supplies a closure that emits to the
    /// webview, which is how spec §55's progress reporting reaches the analyst
    /// without this module knowing anything about windows.
    ///
    /// The tracker returned is also the one [`AppState::cancel`] reaches, which
    /// is what makes the cancel button and the run wired together rather than
    /// merely associated.
    ///
    /// # Errors
    ///
    /// Returns an error if the state's lock is unusable, which would otherwise
    /// silently register a run the cancel button could not reach.
    pub fn begin_run(
        &self,
        reporter: impl Fn(Progress) + Send + Sync + 'static,
    ) -> ShellResult<ProgressTracker> {
        let tracker = ProgressTracker::reporting(reporter);
        self.lock()?.tracker = Some(tracker.clone());
        Ok(tracker)
    }

    /// A token that stops the run in progress.
    ///
    /// `None` when nothing is running, so the caller can decline rather than
    /// hold a token that will never be consulted.
    #[must_use]
    pub fn cancellation(&self) -> Option<Cancellation> {
        self.lock()
            .ok()
            .and_then(|inner| inner.tracker.as_ref().map(ProgressTracker::cancellation))
    }

    /// Cancels the run in progress, if any.
    ///
    /// Returns whether there was one. The frontend uses that to avoid showing a
    /// "cancelled" banner for a click that had nothing to cancel.
    pub fn cancel(&self) -> bool {
        match self.lock() {
            Ok(inner) => match inner.tracker.as_ref() {
                Some(tracker) => {
                    tracker.cancellation().cancel();
                    true
                }
                None => false,
            },
            // A poisoned lock means another thread panicked while holding it.
            // Reporting "nothing to cancel" would be a lie, but panicking here
            // would take down the window, which is the outcome this whole crate
            // exists to prevent (spec §75).
            Err(_) => false,
        }
    }

    /// Whether a run is currently registered.
    ///
    /// A poll, not a promise — spec §56's rule that progress is a measurement.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.lock().is_ok_and(|inner| inner.tracker.is_some())
    }

    /// Allocates the identifier for a run that is about to start.
    ///
    /// Taken *before* the reporter closure is built, because the closure has to
    /// stamp every event with the id and cannot read state it does not yet have.
    /// `register_job` allocates again and asserts the two agree; a mismatch
    /// would mean two runs shared an id and their progress bars overwrote each
    /// other.
    ///
    /// # Errors
    ///
    /// Returns an error if the state's lock is unusable.
    pub fn next_run_id(&self) -> ShellResult<u64> {
        let mut inner = self.lock()?;
        let id = inner.next_run;
        inner.next_run = inner.next_run.saturating_add(1);
        Ok(id)
    }

    /// Registers a freshly spawned job against the run already allocated by
    /// [`Self::next_run_id`].
    ///
    /// The job and the engine that is running it are stored together. They must
    /// be: the analysis fingerprint printed on every report is computed by that
    /// engine's rule set, and pairing a job with a different engine would have
    /// one vouch for a result it did not produce (spec §63).
    ///
    /// `run_id` is the id [`Self::next_run_id`] already handed out, and it is
    /// not allocated again here. Two analyses in flight would otherwise paint
    /// each other's progress onto the same bar, which is precisely what the id
    /// exists to prevent - so this method trusts the caller and never advances
    /// the counter.
    ///
    /// # Errors
    ///
    /// Returns an error if the state's lock is unusable, rather than dropping the
    /// job on the floor: a run nobody can cancel is worse than one that never
    /// started.
    pub fn register_job(
        &self,
        run_id: u64,
        job: AnalysisJob,
        engine: Arc<AnalysisEngine>,
    ) -> ShellResult<u64> {
        let mut inner = self.lock()?;
        inner.engine = Some(engine);
        inner.job = Some(Box::new(job));
        Ok(run_id)
    }

    /// Starts a batch run over `directory` and returns immediately.
    ///
    /// Runs on its own thread because a folder of masters takes minutes, and
    /// spec §57 requires the interface to stay responsive throughout.
    ///
    /// # Errors
    ///
    /// Returns an error if the state's lock is unusable.
    pub fn start_batch<F>(&self, work: F) -> ShellResult<()>
    where
        F: FnOnce() -> BatchResult + Send + 'static,
    {
        let mut inner = self.lock()?;
        inner.batch = Some(std::thread::spawn(work));
        Ok(())
    }

    /// Takes the finished batch result, if the run has finished.
    ///
    /// Returns `None` while the run is still going, so a poll is a question
    /// rather than a wait. A finished run is joined and cleared, so a second
    /// poll reports `None` rather than serving the same table twice.
    ///
    /// # Errors
    ///
    /// Returns an error only if the worker panicked; a panic inside the engine's
    /// walk is reported rather than being allowed to take the window with it.
    pub fn take_batch(&self) -> ShellResult<Option<BatchResult>> {
        let mut inner = self.lock()?;
        let Some(handle) = inner.batch.take() else {
            return Ok(None);
        };
        if !handle.is_finished() {
            // Put it back: the run is still going and the next poll must find it.
            inner.batch = Some(handle);
            return Ok(None);
        }
        match handle.join() {
            Ok(result) => Ok(Some(result)),
            Err(_) => Err(ShellError::new(
                ErrorKind::AnalysisFailed,
                "the batch run ended unexpectedly".to_owned(),
            )),
        }
    }

    /// Whether a batch run is in progress.
    #[must_use]
    pub fn batch_running(&self) -> bool {
        self.lock()
            .ok()
            .and_then(|inner| inner.batch.as_ref().map(|h| !h.is_finished()))
            .unwrap_or(false)
    }

    /// Takes the finished job and its engine, clearing the registry.
    ///
    /// Returns `None` when no run is registered or it has not finished, so a
    /// poll from the frontend is a question rather than a wait — which is what
    /// keeps the interface responsive while an analysis runs (spec §57).
    pub fn take_finished_job(
        &self,
    ) -> Option<(
        Result<tpt_app_media_forensics_core::AnalysisOutcome, CoreError>,
        Arc<AnalysisEngine>,
    )> {
        let mut inner = self.lock().ok()?;
        let job = inner.job.take()?;
        if !job.is_finished() {
            // Put it back: the run is still going and the next poll must find it.
            inner.job = Some(job);
            return None;
        }
        let engine = inner
            .engine
            .take()
            .unwrap_or_else(|| Arc::new(AnalysisEngine::new()));
        inner.tracker = None;
        Some((job.join(), engine))
    }

    /// Whether a run is registered and still in progress.
    #[must_use]
    pub fn job_is_running(&self) -> bool {
        self.lock()
            .is_ok_and(|inner| inner.job.as_ref().is_some_and(|job| !job.is_finished()))
    }

    /// Forgets the run in progress.
    ///
    /// Called once a job's result has been collected, so a later cancel does not
    /// report having stopped a run that had already finished.
    pub fn finish_run(&self) {
        if let Ok(mut inner) = self.lock() {
            inner.tracker = None;
        }
    }

    /// Takes the lock, recovering from poisoning rather than propagating it.
    ///
    /// # Poisoning is recovered from, deliberately
    ///
    /// A mutex is poisoned when a thread panicked while holding it. Propagating
    /// that would mean every later command fails, so one panic — which
    /// [`crate::error`] exists to prevent in the first place — would permanently
    /// break the application. The state being guarded is a path and a tracker;
    /// neither can be left inconsistent by a panic, so continuing is safe.
    fn lock(&self) -> ShellResult<MutexGuard<'_, Inner>> {
        self.inner.lock().map_err(|_| {
            ShellError::new(
                ErrorKind::Storage,
                "the application's case state was left inconsistent by an earlier \
                 fault; reopen the case to continue",
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_state_has_no_case_open() {
        let state = AppState::new();
        assert!(state.maybe_case_dir().is_none());
        assert_eq!(
            state.case_dir().expect_err("nothing is open").kind,
            ErrorKind::NoCaseOpen
        );
    }

    #[test]
    fn an_open_case_can_be_read_back() {
        let state = AppState::new();
        state
            .set_case_dir(PathBuf::from("C:/cases/alpine"))
            .expect("records");
        assert_eq!(
            state.case_dir().expect("reads"),
            PathBuf::from("C:/cases/alpine")
        );
    }

    #[test]
    fn closing_a_case_clears_it() {
        let state = AppState::new();
        state
            .set_case_dir(PathBuf::from("C:/cases/alpine"))
            .expect("records");
        state.close_case().expect("closes");
        assert!(state.maybe_case_dir().is_none());
    }

    #[test]
    fn cancelling_a_run_with_nothing_running_says_so() {
        // The frontend uses this to avoid showing "cancelled" over a click that
        // had nothing to cancel.
        let state = AppState::new();
        assert!(!state.cancel());
        assert!(state.cancellation().is_none());
    }

    #[test]
    fn cancelling_reaches_the_token_the_run_is_using() {
        // This is the wiring spec §55 asks for: the cancel button and the
        // analysis on the other side of it share one flag.
        let state = AppState::new();
        let tracker = state.begin_run(|_| {}).expect("registers");
        assert!(state.is_running());

        assert!(state.cancel());
        assert!(
            tracker.is_cancelled(),
            "the tracker handed to the analysis must be the one cancel() trips"
        );
    }

    #[test]
    fn a_new_run_gets_a_fresh_tracker() {
        // Otherwise cancelling the second run would also report the first as
        // cancelled, and a stale run would refuse to start.
        let state = AppState::new();
        let first = state.begin_run(|_| {}).expect("registers");
        let second = state.begin_run(|_| {}).expect("registers");
        assert!(!first.is_cancelled());
        assert!(!second.is_cancelled());

        state.cancel();
        assert!(!first.is_cancelled(), "the old run must be released");
        assert!(second.is_cancelled());
    }

    #[test]
    fn a_finished_run_is_no_longer_cancellable() {
        // Otherwise a cancel pressed after a run completed would report having
        // stopped work that had already finished, which is a false statement
        // about what the application did.
        let state = AppState::new();
        state.begin_run(|_| {}).expect("registers");
        state.finish_run();
        assert!(!state.is_running());
        assert!(!state.cancel());
    }

    #[test]
    fn progress_events_reach_the_supplied_reporter() {
        // The closure is how spec §55's stage reporting gets to the window. If
        // it were never invoked the UI would sit at zero forever while the
        // analysis ran, which looks exactly like a hang.
        use std::sync::{Arc, Mutex};

        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let state = AppState::new();
        let tracker = state
            .begin_run(move |event| sink.lock().expect("lock").push(event))
            .expect("registers");

        tracker
            .stage_started(
                tpt_app_media_forensics_core::progress::Stage::Acquisition,
                None,
                None,
            )
            .expect("runs");

        assert!(
            !seen.lock().expect("lock").is_empty(),
            "a stage boundary must reach the reporter"
        );
    }
}
