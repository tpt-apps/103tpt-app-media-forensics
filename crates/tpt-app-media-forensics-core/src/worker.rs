//! Background analysis workers (spec §55, §57).
//!
//! # The engine stays synchronous; this is how you move it off your thread
//!
//! `AnalysisEngine::analyse` blocks until the analysis is done. That is the right
//! shape for a library: it composes with any executor, and it means the analysis
//! cannot be abandoned halfway by dropping a future. Spec §55 asks for background
//! workers and spec §57 asks that the UI stay responsive, and the honest way to
//! satisfy both without dragging in an async runtime is to hand the caller an
//! explicit handle to a thread it started itself.
//!
//! Nothing here is started implicitly. [`AnalysisJob::spawn`] is an ordinary
//! function call with an ordinary handle back; there is no global executor, no
//! work-stealing pool, and no task that outlives the handle that created it.
//!
//! # A dead worker is reported, never propagated
//!
//! Spec §75 and §96 require that malformed media cannot crash the application. A
//! panic inside a decoder on hostile bytes is exactly that case, and a background
//! thread is where such a panic would otherwise disappear without trace: the
//! thread dies, nobody notices, and the joiner is left waiting on a handle that
//! will never produce anything. [`AnalysisJob::join`] therefore converts a dead
//! worker into [`CoreError::WorkerFailed`] carrying the path and the panic
//! message, so a crash is recorded the way every other failure is.
//!
//! # Cancellation is the same token, observed on the worker
//!
//! The handle owns the [`Cancellation`] token the analysis was given. Cancelling
//! from the caller's thread sets the shared flag the worker checks at its stage
//! boundaries, so a UI's cancel button and the analysis running on the other side
//! of it are wired together by construction rather than by convention.
//!
//! # One analysis per job, deliberately
//!
//! Two jobs may run concurrently only against *different* case directories. They
//! would share nothing else, but they would write to one SQLite file, and this
//! crate opens a connection per write rather than pooling one. SQLite serialises
//! the writers itself, which is correct, but it makes the interleaving of two
//! analyses' database work depend on timing in a way that is hard to reason about
//! and harder to reproduce. Sequential batch runs stay sequential for that reason;
//! see [`crate::batch`].

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::case_dir::CaseDirectory;
use crate::error::CoreError;
use crate::pipeline::{AnalysisEngine, AnalysisOutcome};
use crate::progress::{Cancellation, ProgressTracker};

/// A handle to an analysis running on its own thread.
///
/// Dropping the handle detaches the thread; it does not cancel the analysis. That
/// asymmetry is deliberate — a caller that wants to stop should say so through
/// [`AnalysisJob::cancel`], and a handle dropped by an error path mid-teardown
/// should not kill work that is still writing a valid record. Call
/// [`AnalysisJob::join`] to wait for the result.
#[derive(Debug)]
pub struct AnalysisJob {
    path: PathBuf,
    cancellation: Cancellation,
    handle: Option<JoinHandle<Result<AnalysisOutcome, CoreError>>>,
}

impl AnalysisJob {
    /// Starts an analysis of `source` on a new thread.
    ///
    /// The engine is taken as an `Arc` so one configured engine can serve many
    /// files without rebuilding its rule set per job, and so a caller can keep a
    /// handle on the exact engine a job is using while it is in flight.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::WorkerFailed`] with `retryable: false` if the thread
    /// cannot be started at all — an exhausted thread limit, typically. That is
    /// distinguished from a worker that later dies because the two call for
    /// different responses: one is worth retrying, the other needs the process to
    /// shed work first.
    pub fn spawn(
        engine: Arc<AnalysisEngine>,
        source: impl Into<PathBuf>,
        case_dir: CaseDirectory,
        progress: ProgressTracker,
    ) -> Result<Self, CoreError> {
        let source = source.into();
        let path = source.display().to_string();
        let cancellation = progress.cancellation();

        let handle = std::thread::Builder::new()
            .name("tpt-analysis".to_owned())
            .spawn(move || engine.analyse_with(&source, &case_dir, &progress))
            .map_err(|error| CoreError::WorkerFailed {
                path: path.clone(),
                reason: format!("could not be started: {error}"),
                retryable: false,
            })?;

        Ok(Self {
            path: PathBuf::from(path),
            cancellation,
            handle: Some(handle),
        })
    }

    /// The source being analysed, for a caller's own bookkeeping.
    ///
    /// A caller driving several jobs needs to tell them apart in whatever UI it
    /// is driving, and the job has no identifier to offer beyond the path.
    #[must_use]
    pub fn source(&self) -> &std::path::Path {
        &self.path
    }

    /// A token that stops this analysis.
    ///
    /// Shares the flag the worker checks, so cancelling here is observed at the
    /// worker's next stage boundary. Returns a clone rather than a reference
    /// because a caller usually hands the token to a cancel button, which outlives
    /// the closure it was created in.
    #[must_use]
    pub fn cancellation(&self) -> Cancellation {
        self.cancellation.clone()
    }

    /// Requests cancellation.
    ///
    /// Idempotent, and safe to call after the job has finished: the flag is
    /// consulted by the worker, so setting it late changes nothing that has
    /// already been written.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// Whether the analysis has finished, without blocking.
    ///
    /// A poll, not a promise. A UI drawing a live indicator needs to know when to
    /// stop drawing; it must not read "not finished" as anything stronger than
    /// that.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Waits for the analysis and returns its outcome.
    ///
    /// Cancelling through [`AnalysisJob::cancel`] before or during the wait makes
    /// this return [`CoreError::Cancelled`], and the run writes nothing to the
    /// case database — the same guarantee the synchronous path gives, because it
    /// is the same code.
    ///
    /// # Errors
    ///
    /// Everything [`AnalysisEngine::analyse`] returns, plus
    /// [`CoreError::WorkerFailed`] if the worker thread panicked rather than
    /// returning. The panic is captured, not resumed: a defect in a decoder must
    /// not take down the application that is trying to report on it.
    pub fn join(mut self) -> Result<AnalysisOutcome, CoreError> {
        let Some(handle) = self.handle.take() else {
            return Err(CoreError::WorkerFailed {
                path: self.path.display().to_string(),
                reason: "was already joined".to_owned(),
                retryable: false,
            });
        };

        match handle.join() {
            Ok(result) => result,
            Err(payload) => Err(CoreError::WorkerFailed {
                path: self.path.display().to_string(),
                reason: format!("panicked: {}", panic_message(payload.as_ref())),
                // A panic is a defect in this program. It may well not recur on
                // another file, so the flag says "worth one more try" rather than
                // promising anything.
                retryable: true,
            }),
        }
    }
}

/// Extracts a readable message from a panic payload.
///
/// Takes `&dyn Any + Send` rather than `&Box<dyn Any + Send>` deliberately.
/// Passing a `&Box<_>` where a `&dyn Any` is expected does not deref to the
/// box's contents: the coercion produces a fat pointer to the *box*, the
/// `TypeId` it carries is the box's, and every downcast silently misses. The
/// result is a function that appears to work and reports "no message" for every
/// panic it is given. Callers must pass `payload.as_ref()`.
///
/// A panic payload is `Box<dyn Any + Send>`, which in practice is a `String` from
/// `panic!` or a `&'static str` from `assert!`. Both are read; anything else is
/// described rather than discarded, because "the worker died" without saying what
/// it died on is not something an examiner can act on.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&'static str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "the panic carried no message".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::panic_message;

    #[test]
    fn a_str_payload_is_read() {
        // `assert!` panics with a `&'static str`.
        let payload: Box<dyn std::any::Any + Send> = Box::new("decoder produced no frame");
        assert_eq!(panic_message(payload.as_ref()), "decoder produced no frame");
    }

    #[test]
    fn an_owned_payload_is_read() {
        // `panic!("{}", x)` and the default hook panic with a `String`.
        let payload: Box<dyn std::any::Any + Send> = Box::new(String::from("index 41 of 3"));
        assert_eq!(panic_message(payload.as_ref()), "index 41 of 3");
    }

    #[test]
    fn an_unrecognised_payload_is_described_not_dropped() {
        // Described rather than replaced with a placeholder: a message that says
        // nothing is indistinguishable from a message that was lost, and those
        // call for different responses.
        let payload: Box<dyn std::any::Any + Send> = Box::new(7u32);
        assert!(panic_message(payload.as_ref()).contains("no message"));
    }

    /// A real panic round-trips through the same path `join` takes.
    ///
    /// Worth a test because the downcast it depends on fails *silently*: passing
    /// the box by reference instead of by `as_ref` compiles, runs, and returns
    /// "no message" for every payload rather than erroring. A test that constructs
    /// the payload by hand would not have caught that; this one lets the standard
    /// library produce it.
    #[test]
    fn a_real_panic_yields_its_own_message() {
        let payload = std::panic::catch_unwind(|| -> () { panic!("decode produced {} frames", 0) })
            .expect_err("the closure must panic");

        let message = panic_message(payload.as_ref());
        assert!(
            message.contains("decode produced 0 frames"),
            "the panic's own text must survive: {message}"
        );
    }
}
