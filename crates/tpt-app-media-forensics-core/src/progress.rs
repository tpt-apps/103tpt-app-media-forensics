//! Progress reporting and cooperative cancellation (spec §55, §56).
//!
//! # Why cancellation is cooperative, and where it is checked
//!
//! The engine is synchronous: `analyse` runs to completion on the calling thread
//! and returns a result. Nothing here spawns a runtime or introduces async. What it
//! provides is a flag the pipeline *checks at stage boundaries* and a progress
//! callback it *invokes at the same points*.
//!
//! That is a real limitation and it is stated rather than papered over: a stage
//! that takes ninety seconds cannot be interrupted part-way through, because the
//! decoder and the byte scanner have no cancellation hook of their own. A cancel
//! request during a long decode is observed when that decode returns. The
//! granularity is documented on [`Stage`] so a caller knows what it is agreeing
//! to.
//!
//! # Cancellation leaves no partial result behind
//!
//! A cancelled analysis returns [`crate::error::CoreError::Cancelled`] and writes
//! nothing to the case database. This matters forensically: a half-written
//! analysis record that looks complete is worse than no record, because a report
//! built from it would assert measurements the engine never finished taking.
//!
//! # Progress is a measurement, not a promise
//!
//! [`Progress`] reports a stage's ordinal and an optional completion fraction. It
//! deliberately does *not* estimate remaining time: an estimate on a 90-minute
//! feature-length master would be wrong by minutes and would read as a commitment
//! the engine cannot make. Fractional progress is reported only where a total is
//! genuinely known, such as bytes read during acquisition.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A stage of one analysis, in execution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stage {
    /// Reading the file and computing its hashes.
    Acquisition,
    /// Probing the container and enumerating streams.
    ContainerInspection,
    /// Reading and indexing samples.
    SampleIndex,
    /// Decoding audio and measuring levels.
    Audio,
    /// Running Tier-2 pixel analysis.
    TierTwo,
    /// Extracting container and timing metadata.
    Metadata,
    /// Evaluating the rule set.
    Rules,
    /// Writing results to the case database.
    Persist,
}

impl Stage {
    /// Every stage, in the order `run_stages` performs them.
    ///
    /// A single ordered list so the ordinal a progress report carries is derived
    /// from the same sequence the pipeline runs. Hard-coding numbers at each call
    /// site would let them drift, and a progress bar reaching 100% before the last
    /// stage starts is worse than none.
    pub const ALL: [Stage; 8] = [
        Stage::Acquisition,
        Stage::ContainerInspection,
        Stage::SampleIndex,
        Stage::Audio,
        Stage::TierTwo,
        Stage::Metadata,
        Stage::Rules,
        Stage::Persist,
    ];

    /// A stable lowercase tag for logs and reports.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Acquisition => "acquisition",
            Self::ContainerInspection => "container-inspection",
            Self::SampleIndex => "sample-index",
            Self::Audio => "audio",
            Self::TierTwo => "tier-two",
            Self::Metadata => "metadata",
            Self::Rules => "rules",
            Self::Persist => "persist",
        }
    }

    /// This stage's zero-based position in [`Stage::ALL`].
    #[must_use]
    pub fn ordinal(self) -> usize {
        // Found rather than hardcoded: a new variant listed in one place but not
        // the other would otherwise silently report a wrong ordinal.
        Self::ALL
            .iter()
            .position(|stage| *stage == self)
            .unwrap_or(0)
    }
}
/// How far along one stage is, where a total is genuinely known.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Progress {
    /// A stage has begun, with no measurable internal progress.
    ///
    /// The `completed`/`total` pair is optional because most stages genuinely have
    /// no denominator. Reporting a fake one — say, a nominal frame count — would
    /// produce a bar that moves smoothly and means nothing.
    Started {
        /// The stage.
        stage: Stage,
        /// Items completed, where known.
        completed: Option<u64>,
        /// Total items, where known.
        total: Option<u64>,
    },
    /// A stage has finished successfully.
    Finished {
        /// The stage.
        stage: Stage,
    },
}

impl Progress {
    /// This event's stage.
    #[must_use]
    pub const fn stage(&self) -> Stage {
        match self {
            Self::Started { stage, .. } | Self::Finished { stage } => *stage,
        }
    }

    /// Completion as a fraction of the whole analysis, 0.0 to 1.0.
    ///
    /// Stage-granular: each stage counts as an equal share. Reporting finer
    /// fractions for some stages and coarser for others would make the bar lurch,
    /// and a fraction that jumps backwards mid-run is worse than none.
    #[must_use]
    pub fn fraction(&self) -> f64 {
        let total = Stage::ALL.len() as f64;
        let done = match self {
            Self::Finished { stage } => stage.ordinal() as f64 + 1.0,
            Self::Started { stage, .. } => stage.ordinal() as f64,
        };
        (done / total).clamp(0.0, 1.0)
    }
}

/// A shared cancellation flag.
///
/// Cheap to clone and safe to hold across threads, so a UI can own one and a
/// worker can observe it. Cloning shares the flag; it does not copy it.
#[derive(Debug, Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    /// A new, uncancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation.
    ///
    /// Idempotent: cancelling twice is not an error, because a user pressing a
    /// cancel button twice should not produce a failure.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}
/// Progress and cancellation for one analysis.
///
/// Bundled because they are always needed together: a caller that wants progress
/// also wants to be able to stop, and threading two separate parameters through
/// every stage would be easy to get wrong.
#[derive(Clone, Default)]
pub struct ProgressTracker {
    cancellation: Cancellation,
    reporter: Option<Arc<dyn Fn(Progress) + Send + Sync>>,
}

impl std::fmt::Debug for ProgressTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgressTracker")
            .field("cancelled", &self.cancellation.is_cancelled())
            .field("reporting", &self.reporter.is_some())
            .finish()
    }
}

impl ProgressTracker {
    /// A tracker that reports nowhere and never cancels.
    ///
    /// The default for library callers: analysis must not require a caller to set
    /// anything up, and a CLI with no progress display must not pay for one.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// A tracker that invokes `reporter` at each stage boundary.
    #[must_use]
    pub fn reporting(reporter: impl Fn(Progress) + Send + Sync + 'static) -> Self {
        Self {
            cancellation: Cancellation::new(),
            reporter: Some(Arc::new(reporter)),
        }
    }

    /// A token for cancelling this analysis.
    #[must_use]
    pub fn cancellation(&self) -> Cancellation {
        self.cancellation.clone()
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Announces that `stage` has begun.
    ///
    /// Returns an error when cancellation has been requested, so a stage boundary
    /// is a single call site that both reports progress and checks for a stop.
    ///
    /// # Errors
    ///
    /// [`CoreError::Cancelled`](crate::error::CoreError::Cancelled) when the token
    /// is set. Nothing is emitted in that case: a stage that did not start must
    /// not announce itself as having started.
    pub fn stage_started(
        &self,
        stage: Stage,
        completed: Option<u64>,
        total: Option<u64>,
    ) -> Result<(), crate::error::CoreError> {
        self.check_cancelled()?;
        self.emit(Progress::Started {
            stage,
            completed,
            total,
        });
        Ok(())
    }

    /// Announces that `stage` has finished.
    ///
    /// # Errors
    ///
    /// [`CoreError::Cancelled`](crate::error::CoreError::Cancelled) when the token
    /// is set.
    pub fn stage_finished(&self, stage: Stage) -> Result<(), crate::error::CoreError> {
        self.check_cancelled()?;
        self.emit(Progress::Finished { stage });
        Ok(())
    }

    /// Returns an error if cancellation has been requested.
    ///
    /// # Errors
    ///
    /// [`CoreError::Cancelled`](crate::error::CoreError::Cancelled) when the token
    /// is set.
    pub fn check_cancelled(&self) -> Result<(), crate::error::CoreError> {
        if self.is_cancelled() {
            return Err(crate::error::CoreError::Cancelled);
        }
        Ok(())
    }

    fn emit(&self, event: Progress) {
        if let Some(reporter) = &self.reporter {
            reporter(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Cancellation, Progress, ProgressTracker, Stage};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    #[test]
    fn a_fresh_token_is_not_cancelled() {
        assert!(!Cancellation::new().is_cancelled());
    }

    #[test]
    fn cancelling_twice_is_not_an_error() {
        // A user pressing cancel twice should not produce a failure.
        let token = Cancellation::new();
        token.cancel();
        token.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn a_cloned_token_shares_one_flag() {
        // Cloning must share, not copy: a UI holding a clone has to be able to
        // stop the worker that owns the original.
        let original = Cancellation::new();
        let clone = original.clone();
        assert!(!clone.is_cancelled());

        clone.cancel();
        assert!(
            original.is_cancelled(),
            "a cancelled clone must cancel the original"
        );
    }

    #[test]
    fn a_stage_boundary_refuses_once_cancelled() {
        let tracker = ProgressTracker::none();
        assert!(tracker
            .stage_started(Stage::Acquisition, None, None)
            .is_ok());

        tracker.cancellation().cancel();
        let error = tracker
            .stage_started(Stage::ContainerInspection, None, None)
            .expect_err("a cancelled stage must not start");
        assert_eq!(error.to_string(), "the analysis was cancelled");

        // Cancellation is sticky: it stays refused.
        assert!(tracker.stage_finished(Stage::ContainerInspection).is_err());
    }

    #[test]
    fn progress_is_reported_at_every_boundary() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let tracker = ProgressTracker::reporting(move |event| {
            sink.lock().expect("lock").push(event);
        });

        for stage in Stage::ALL {
            tracker.stage_started(stage, None, None).expect("runs");
            tracker.stage_finished(stage).expect("runs");
        }

        let events = seen.lock().expect("lock").clone();
        assert_eq!(
            events.len(),
            Stage::ALL.len() * 2,
            "one start and one finish per stage"
        );
    }

    #[test]
    fn nothing_is_reported_after_cancellation() {
        // A stage that did not start must not announce itself as having started:
        // a progress bar counting a stage that never ran is a small lie about
        // what the engine did.
        let count = Arc::new(AtomicUsize::new(0));
        let sink = Arc::clone(&count);
        let tracker = ProgressTracker::reporting(move |_| {
            sink.fetch_add(1, Ordering::SeqCst);
        });

        tracker
            .stage_started(Stage::Acquisition, None, None)
            .expect("runs");
        tracker.cancellation().cancel();
        let _ = tracker.stage_started(Stage::ContainerInspection, None, None);

        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "a cancelled stage must not also announce itself"
        );
    }

    #[test]
    fn stage_ordinals_are_contiguous_and_ordered() {
        // The ordinals are what a progress fraction is built from, so a gap or a
        // duplicate would make the bar skip or stall.
        let ordinals: Vec<usize> = Stage::ALL.iter().map(|s| s.ordinal()).collect();
        assert_eq!(ordinals, (0..Stage::ALL.len()).collect::<Vec<_>>());
    }

    #[test]
    fn the_fraction_runs_from_zero_to_one() {
        let first = Progress::Started {
            stage: Stage::Acquisition,
            completed: None,
            total: None,
        };
        let last = Progress::Finished {
            stage: *Stage::ALL.last().expect("stages"),
        };

        assert_eq!(first.fraction(), 0.0);
        assert_eq!(last.fraction(), 1.0);
    }

    #[test]
    fn the_fraction_never_leaves_the_unit_range() {
        for stage in Stage::ALL {
            for fraction in [
                Progress::Started {
                    stage,
                    completed: None,
                    total: None,
                }
                .fraction(),
                Progress::Finished { stage }.fraction(),
            ] {
                assert!(
                    (0.0..=1.0).contains(&fraction),
                    "{stage:?} reported {fraction}"
                );
            }
        }
    }

    #[test]
    fn every_stage_has_a_distinct_tag() {
        let mut tags: Vec<&str> = Stage::ALL.iter().map(|s| s.tag()).collect();
        tags.sort_unstable();
        let before = tags.len();
        tags.dedup();
        assert_eq!(tags.len(), before, "stage tags must be unique");
    }

    #[test]
    fn a_tracker_with_no_reporter_still_runs() {
        // Analysis must not require a caller to set anything up.
        let tracker = ProgressTracker::none();
        assert!(!tracker.is_cancelled());
        assert!(tracker.stage_started(Stage::Rules, None, None).is_ok());
        assert!(tracker.stage_finished(Stage::Rules).is_ok());
    }

    #[test]
    fn completed_and_total_are_carried_through() {
        // Where a total is genuinely known — bytes read, samples read — it must
        // reach the reporter rather than being discarded.
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let tracker = ProgressTracker::reporting(move |event| {
            sink.lock().expect("lock").push(event);
        });

        tracker
            .stage_started(Stage::Acquisition, Some(512), Some(1024))
            .expect("runs");

        let events = seen.lock().expect("lock").clone();
        match &events[0] {
            Progress::Started {
                stage,
                completed,
                total,
            } => {
                assert_eq!(*stage, Stage::Acquisition);
                assert_eq!(*completed, Some(512));
                assert_eq!(*total, Some(1024));
            }
            other => panic!("expected a start event, got {other:?}"),
        }
    }

    #[test]
    fn a_cancelled_check_reports_the_cancelled_error() {
        let tracker = ProgressTracker::none();
        assert!(tracker.check_cancelled().is_ok());

        tracker.cancellation().cancel();
        let error = tracker.check_cancelled().expect_err("must refuse");
        assert!(
            matches!(error, crate::error::CoreError::Cancelled),
            "a cancelled check must report cancellation, got {error:?}"
        );
    }
}
