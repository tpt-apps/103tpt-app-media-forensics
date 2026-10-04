//! Progress reporting, cooperative cancellation, and the stage model that
//! describes how much of an analysis is done (spec §55, §56).
//!
//! # Why cancellation is cooperative, and where it is checked
//!
//! The analysis itself is synchronous and pulls no async runtime: `analyse` runs
//! to completion on whichever thread called it and returns a result. What this
//! module provides is a flag the pipeline *checks at stage boundaries* and a
//! progress callback it *invokes at the same points*.
//!
//! That is a real limitation and it is stated rather than papered over: a stage
//! that takes ninety seconds cannot be interrupted part-way through, because the
//! decoder and the byte scanner have no cancellation hook of their own. A cancel
//! request during a long decode is observed when that decode returns. The
//! granularity is documented on [`Stage`] so a caller knows what it is agreeing
//! to.
//!
//! Moving a run *off* the calling thread is a separate concern and lives in
//! [`crate::worker`]: the engine itself never spawns a thread on a caller's
//! behalf, so a library caller decides for itself where the work happens.
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
//!
//! # Concurrent branches still report one forward-moving bar
//!
//! Four stages are independent and may run at the same time (spec §56). Emitting
//! their boundaries naively would let the bar lurch: a fast branch finishing
//! before a slower one starts would push the fraction past work that has not
//! happened yet. So a concurrent stage reports through [`Progress::BranchFinished`]
//! instead, and [`Progress::fraction`] places every event inside its *group's*
//! span. The fraction is then monotonic by construction, and identical whether the
//! branches ran on separate threads or one after another — the parallelism is an
//! implementation detail a report can never observe.

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
    ///
    /// The middle four are a *group*, not a queue: they are independent and run
    /// concurrently (spec §56). See [`Stage::CONCURRENT`] and [`Stage::span`].
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

    /// The stages that run at the same time as one another (spec §56).
    ///
    /// Declared as a list rather than inferred, so that adding a stage to
    /// [`Stage::ALL`] does not silently change which work is concurrent — and,
    /// more importantly, so that a stage nobody puts in here is visibly
    /// sequential rather than accidentally sharing a thread with something that
    /// writes the same bundle fields.
    ///
    /// These four read from the inspection and the sample table and write to
    /// disjoint bundle fields, which is what makes them safe to overlap. Each
    /// reports through [`Progress::BranchFinished`] rather than through
    /// [`Stage::ordinal()`], because their individual positions in [`Stage::ALL`]
    /// are an ordering convention and not a claim about when they finish.
    pub const CONCURRENT: [Stage; 4] = [
        Stage::SampleIndex,
        Stage::Audio,
        Stage::TierTwo,
        Stage::Metadata,
    ];

    /// The half-open ordinal range `[start, end)` this stage's events occupy.
    ///
    /// A concurrent stage spans its whole group, so every branch of the group
    /// reports against the same range and the combined fraction only ever moves
    /// forward. A sequential stage spans itself, which is the width-1 case.
    ///
    /// This is what keeps a progress bar monotonic without the pipeline having to
    /// know whether it is running on one core or sixteen.
    #[must_use]
    pub fn span(self) -> std::ops::Range<usize> {
        if Self::CONCURRENT.contains(&self) {
            let first = Self::CONCURRENT[0].ordinal().min(self.ordinal());
            let last = Self::CONCURRENT[Self::CONCURRENT.len() - 1]
                .ordinal()
                .max(self.ordinal());
            first..last + 1
        } else {
            self.ordinal()..self.ordinal() + 1
        }
    }

    /// Whether this stage runs concurrently with the others in
    /// [`Stage::CONCURRENT`].
    #[must_use]
    pub fn is_concurrent(self) -> bool {
        Self::CONCURRENT.contains(&self)
    }

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
    /// One branch of a concurrent stage group has finished.
    ///
    /// The four independent analysers in [`Stage::CONCURRENT`] report this rather
    /// than [`Progress::Finished`], because a branch that finished first did not
    /// finish the group: reporting it as a completed stage would let the fraction
    /// jump past work that has not started, and a bar that jumps backwards is
    /// worse than no bar at all.
    ///
    /// `completed` and `total` count *branches*, not media items — the group size
    /// is genuinely known, so the fraction it yields is real rather than nominal.
    BranchFinished {
        /// The branch's stage.
        stage: Stage,
        /// Branches finished so far, including this one.
        completed: usize,
        /// Branches in the group.
        total: usize,
    },
}

impl Progress {
    /// This event's stage.
    #[must_use]
    pub const fn stage(&self) -> Stage {
        match self {
            Self::Started { stage, .. }
            | Self::Finished { stage }
            | Self::BranchFinished { stage, .. } => *stage,
        }
    }

    /// Completion as a fraction of the whole analysis, 0.0 to 1.0.
    ///
    /// Stage-granular: each stage counts as an equal share. Reporting finer
    /// fractions for some stages and coarser for others would make the bar lurch,
    /// and a fraction that jumps backwards mid-run is worse than none.
    ///
    /// A concurrent stage reports against its whole group's span, so a finishing
    /// branch advances the bar by a fraction of the group rather than by an entire
    /// stage. The emitted sequence is therefore monotonic — and byte-identical —
    /// whether the branches ran on separate threads or one after another, so
    /// nothing about the parallelism can be observed from a report.
    #[must_use]
    pub fn fraction(&self) -> f64 {
        let stages = Stage::ALL.len() as f64;
        let span = self.stage().span();
        let width = span.end.saturating_sub(span.start) as f64;
        let position = match self {
            Self::Started { .. } => 0.0,
            Self::Finished { .. } => width,
            Self::BranchFinished {
                completed, total, ..
            } => {
                // A total of zero would divide by zero; a tracker that reports no
                // total has not counted the group, so the bar stays where it is
                // rather than jumping to a fabricated position.
                if *total == 0 {
                    0.0
                } else {
                    width * (*completed as f64 / *total as f64).clamp(0.0, 1.0)
                }
            }
        };
        ((span.start as f64 + position) / stages).clamp(0.0, 1.0)
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

    /// Announces that one branch of a concurrent stage group has finished.
    ///
    /// `completed` counts branches, not media items: it is the number of
    /// [`Stage::CONCURRENT`] branches done so far, including this one, out of
    /// `total` — the size of the group, which is known before the work starts.
    ///
    /// Reports are emitted in *stage order* rather than completion order by the
    /// pipeline, so the sequence a caller sees does not depend on which branch
    /// happened to finish first.
    ///
    /// # Errors
    ///
    /// [`CoreError::Cancelled`](crate::error::CoreError::Cancelled) when the token
    /// is set.
    pub fn branch_finished(
        &self,
        stage: Stage,
        completed: usize,
        total: usize,
    ) -> Result<(), crate::error::CoreError> {
        self.check_cancelled()?;
        self.emit(Progress::BranchFinished {
            stage,
            completed,
            total,
        });
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
    fn the_concurrent_stages_are_one_contiguous_span() {
        // A group whose members do not share a span would let a branch report
        // against a range its neighbours are not in, which is exactly the
        // backwards jump the span exists to prevent.
        let first = Stage::CONCURRENT[0];
        let last = Stage::CONCURRENT[Stage::CONCURRENT.len() - 1];
        for stage in Stage::CONCURRENT {
            assert_eq!(stage.span(), first.span(), "{stage:?} is outside the group");
        }
        assert_eq!(first.span().start, first.ordinal());
        assert_eq!(last.span().end, last.ordinal() + 1);

        // Contiguous in the ordered list too, so the span really is the whole
        // middle of the run rather than an arbitrary range.
        let ordinals: Vec<usize> = Stage::CONCURRENT.iter().map(|s| s.ordinal()).collect();
        let expected: Vec<usize> = (first.ordinal()..=last.ordinal()).collect();
        assert_eq!(
            ordinals, expected,
            "the group must be contiguous in Stage::ALL"
        );
    }

    #[test]
    fn a_sequential_stage_spans_only_itself() {
        let stage = Stage::Acquisition;
        assert_eq!(stage.span().len(), 1);
        assert!(!stage.is_concurrent());
    }

    #[test]
    fn a_concurrent_branch_never_reports_outside_the_group() {
        // Whatever the branch counts say, the fraction has to stay inside the
        // span the whole group occupies.
        let span = Stage::Audio.span();
        for completed in 0..=Stage::CONCURRENT.len() {
            let event = Progress::BranchFinished {
                stage: Stage::Audio,
                completed,
                total: Stage::CONCURRENT.len(),
            };
            assert!(
                event.fraction() >= span.start as f64 / Stage::ALL.len() as f64,
                "{completed} branches reported before the group began"
            );
            assert!(
                event.fraction() <= span.end as f64 / Stage::ALL.len() as f64,
                "{completed} branches reported past the end of the group"
            );
        }
    }

    #[test]
    fn the_fraction_only_moves_forwards_across_a_whole_run() {
        // The property the concurrency depends on. Driven in the worst possible
        // order — a group reporting in reverse, which is what a scheduler is free
        // to do — the bar must still never go backwards.
        let mut events = vec![
            Progress::Started {
                stage: Stage::Acquisition,
                completed: None,
                total: None,
            },
            Progress::Finished {
                stage: Stage::Acquisition,
            },
            Progress::Started {
                stage: Stage::ContainerInspection,
                completed: None,
                total: None,
            },
            Progress::Finished {
                stage: Stage::ContainerInspection,
            },
            Progress::Started {
                stage: Stage::SampleIndex,
                completed: Some(0),
                total: Some(4),
            },
        ];

        let total = Stage::CONCURRENT.len();
        // The realistic worst case: the branch *count* advances correctly, but the
        // stages arrive scrambled because they were reported in completion order.
        // The named stage must not be what drives the fraction, or a fast
        // metadata read finishing before a slow decode would drag the bar back.
        for completed in 1..=total {
            events.push(Progress::BranchFinished {
                stage: Stage::CONCURRENT[total - completed],
                completed,
                total,
            });
        }
        events.push(Progress::Started {
            stage: Stage::Rules,
            completed: None,
            total: None,
        });
        events.push(Progress::Finished {
            stage: Stage::Rules,
        });
        events.push(Progress::Started {
            stage: Stage::Persist,
            completed: None,
            total: None,
        });
        events.push(Progress::Finished {
            stage: Stage::Persist,
        });

        let mut previous = 0.0;
        for event in &events {
            let fraction = event.fraction();
            assert!(
                fraction >= previous,
                "the bar went backwards at {event:?}: {fraction} after {previous}"
            );
            previous = fraction;
        }
        assert_eq!(previous, 1.0, "a finished run must end at 1.0");
    }

    #[test]
    fn an_empty_group_does_not_divide_by_zero() {
        // A reporter that has not counted the group must not send the bar to a
        // fabricated position.
        let event = Progress::BranchFinished {
            stage: Stage::Audio,
            completed: 1,
            total: 0,
        };
        assert!(event.fraction().is_finite());
        assert_eq!(
            event.fraction(),
            Stage::Audio.span().start as f64 / Stage::ALL.len() as f64
        );
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
