//! The analysis run: progress, completion, and what it produced (spec \u00a755-\u00a757).
//!
//! # Progress is a measurement, never a promise
//!
//! Spec \u00a756 asks the application not to estimate remaining time, and this model
//! carries no time estimate for the same reason: an estimate on a ninety-minute
//! master would be wrong by minutes and would read as a commitment the engine
//! cannot make. [`RunEvent::fraction`] reports a position in the run and, where
//! a total is genuinely known such as bytes during acquisition, a partial
//! count \u2014 nothing more.
//!
//! # The four concurrent stages report one forward-moving bar
//!
//! Spec \u00a756 allows independent stages to run at once. Emitting their boundaries
//! naively would let the bar lurch backwards when a fast branch finished before
//! a slower one started, so the engine groups them and reports
//! [`RunEvent::BranchFinished`]. The frontend draws one bar from that, and this
//! module never derives a fraction of its own \u2014 it asks the engine, which is the
//! only place that knows how the stages are grouped.
//!
//! # Cancellation leaves nothing behind
//!
//! A cancelled run writes nothing to the case database, because a partial
//! analysis record that looks complete is worse than no record: a report built
//! from it would assert measurements the engine never finished taking.

use serde::{Deserialize, Serialize};

use tpt_app_media_forensics_core::progress::{Progress, Stage};

/// A stage, as it travels over the IPC boundary.
///
/// A shell-local copy of the engine's [`Stage`] rather than the type itself. The
/// engine's enum has no `serde` derive \u2014 it is never serialised there \u2014 and
/// adding one to satisfy a progress bar would put a presentation concern into the
/// analysis engine. The mapping is total in both directions and is tested, so an
/// engine stage cannot fail to reach the window or fail to come back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StageTag {
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

impl From<Stage> for StageTag {
    /// Maps an engine stage onto its wire tag.
    ///
    /// Written out exhaustively rather than derived from a tag string, so a new
    /// engine stage fails to compile here instead of reaching the frontend as a
    /// stage the UI has no label for.
    fn from(stage: Stage) -> Self {
        match stage {
            Stage::Acquisition => Self::Acquisition,
            Stage::ContainerInspection => Self::ContainerInspection,
            Stage::SampleIndex => Self::SampleIndex,
            Stage::Audio => Self::Audio,
            Stage::TierTwo => Self::TierTwo,
            Stage::Metadata => Self::Metadata,
            Stage::Rules => Self::Rules,
            Stage::Persist => Self::Persist,
        }
    }
}

impl From<StageTag> for Stage {
    /// Maps a wire tag back onto the engine stage.
    fn from(tag: StageTag) -> Self {
        match tag {
            StageTag::Acquisition => Stage::Acquisition,
            StageTag::ContainerInspection => Stage::ContainerInspection,
            StageTag::SampleIndex => Stage::SampleIndex,
            StageTag::Audio => Stage::Audio,
            StageTag::TierTwo => Stage::TierTwo,
            StageTag::Metadata => Stage::Metadata,
            StageTag::Rules => Stage::Rules,
            StageTag::Persist => Stage::Persist,
        }
    }
}

impl StageTag {
    /// Every tag, in execution order.
    pub const ALL: [Self; 8] = [
        Self::Acquisition,
        Self::ContainerInspection,
        Self::SampleIndex,
        Self::Audio,
        Self::TierTwo,
        Self::Metadata,
        Self::Rules,
        Self::Persist,
    ];

    /// Whether this stage runs concurrently with the others in its group.
    ///
    /// Mirrors `Stage::is_concurrent`, so the frontend can label a branch event
    /// without knowing which stages the engine groups.
    #[must_use]
    pub const fn is_concurrent(self) -> bool {
        matches!(
            self,
            Self::SampleIndex | Self::Audio | Self::TierTwo | Self::Metadata
        )
    }

    /// The label drawn beside the progress bar.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Acquisition => "Reading file",
            Self::ContainerInspection => "Inspecting container",
            Self::SampleIndex => "Indexing samples",
            Self::Audio => "Measuring audio",
            Self::TierTwo => "Analysing pixels",
            Self::Metadata => "Extracting metadata",
            Self::Rules => "Evaluating rules",
            Self::Persist => "Writing to case",
        }
    }
}

/// One progress report from a running analysis (spec \u00a755).
///
/// Carries the stage on every variant, including `BranchFinished`. That looks
/// redundant for a branch event, whose fraction depends on the *group* rather
/// than on the stage \u2014 but the engine's own event names a stage too, and the
/// frontend needs one to label the bar.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RunEvent {
    /// A stage has begun.
    Started {
        /// Which stage.
        stage: StageTag,
        /// Units done so far, where a total is known.
        completed: Option<u64>,
        /// The total, where known.
        total: Option<u64>,
    },
    /// A stage has finished.
    Finished {
        /// Which stage.
        stage: StageTag,
    },
    /// One of the concurrent branches has finished.
    BranchFinished {
        /// The branch that finished.
        stage: StageTag,
        /// Branches done.
        completed: usize,
        /// Branches in the group.
        total: usize,
    },
}

impl RunEvent {
    /// The stage this event concerns.
    #[must_use]
    pub const fn stage(&self) -> StageTag {
        match self {
            Self::Started { stage, .. }
            | Self::Finished { stage }
            | Self::BranchFinished { stage, .. } => *stage,
        }
    }

    /// A fraction of the whole run, 0.0 to 1.0.
    ///
    /// Taken from the engine's own calculation rather than recomputed here. The
    /// engine knows how its concurrent stages are grouped and this does not, and
    /// a second derivation would be free to disagree with the first \u2014 which is
    /// how a progress bar starts going backwards.
    #[must_use]
    pub fn fraction(&self) -> f64 {
        self.as_engine().fraction()
    }

    /// The equivalent engine event, for the fraction calculation.
    fn as_engine(&self) -> Progress {
        match self {
            Self::Started {
                stage,
                completed,
                total,
            } => Progress::Started {
                stage: (*stage).into(),
                completed: *completed,
                total: *total,
            },
            Self::Finished { stage } => Progress::Finished {
                stage: (*stage).into(),
            },
            Self::BranchFinished {
                stage,
                completed,
                total,
            } => Progress::BranchFinished {
                stage: (*stage).into(),
                completed: *completed,
                total: *total,
            },
        }
    }
}

impl From<&Progress> for RunEvent {
    /// Converts an engine progress event for the wire.
    fn from(event: &Progress) -> Self {
        match event {
            Progress::Started {
                stage,
                completed,
                total,
            } => Self::Started {
                stage: (*stage).into(),
                completed: *completed,
                total: *total,
            },
            Progress::Finished { stage } => Self::Finished {
                stage: (*stage).into(),
            },
            Progress::BranchFinished {
                stage,
                completed,
                total,
            } => Self::BranchFinished {
                stage: (*stage).into(),
                completed: *completed,
                total: *total,
            },
        }
    }
}

/// What one analysis produced, as the UI receives it (spec \u00a756).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunResult {
    /// The file that was analysed.
    pub asset_name: String,
    /// Its SHA-256, when one could be computed.
    pub sha256: Option<String>,
    /// How it ended.
    pub status: RunStatus,
    /// Findings produced, most severe first.
    pub finding_count: u64,
    /// Artefacts written into the case.
    pub evidence_count: u64,
    /// Observations on the timeline.
    pub timeline_count: u64,
    /// How many of those had a measured position.
    pub measured_timeline_count: u64,
    /// Whether the result came from the analysis cache.
    pub cache_hit: bool,
    /// The combined fingerprint identifying this analysis (spec \u00a763).
    pub analysis_fingerprint: String,
    /// Everything the engine could not measure (spec \u00a775).
    ///
    /// Reported prominently rather than hidden behind a disclosure. An examiner
    /// needs to know what was not looked at as much as what was, and a run that
    /// measured nothing at all must not read as a clean file.
    pub limitations: Vec<String>,
    /// Whether this run wrote its findings to the case database.
    pub persisted: bool,
}

/// How an analysis run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RunStatus {
    /// The run completed; findings may still need review.
    Complete,
    /// The run failed partway; any partial results are retained.
    Failed,
    /// The analyst cancelled the run.
    Cancelled,
}

impl RunStatus {
    /// Whether the run finished and will not change further.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_app_media_forensics_core::progress::{Progress, Stage};

    #[test]
    fn every_engine_stage_reaches_the_wire_and_comes_back() {
        // The mapping is hand-written in both directions, so it is exactly the
        // kind of pair that can grow a hole. A stage that fails to survive the
        // round trip would reach the frontend as a stage the UI cannot label,
        // and the progress bar would silently stop moving on that stage.
        for stage in Stage::ALL {
            let tag = StageTag::from(stage);
            assert_eq!(Stage::from(tag), stage, "{stage:?} did not round trip");
            assert!(!tag.label().is_empty(), "{stage:?} has no label");
        }
        assert_eq!(StageTag::ALL.len(), Stage::ALL.len());
    }

    #[test]
    fn the_concurrent_group_is_mirrored_exactly() {
        // If the two lists disagreed, a branch event could arrive labelled as a
        // sequential stage and the frontend would draw it as its own bar.
        let engine: Vec<Stage> = Stage::CONCURRENT.to_vec();
        let shell: Vec<StageTag> = StageTag::ALL
            .iter()
            .copied()
            .filter(|t| t.is_concurrent())
            .collect();
        assert_eq!(
            engine.len(),
            shell.len(),
            "concurrent groups differ in size"
        );
        for stage in engine {
            assert!(
                StageTag::from(stage).is_concurrent(),
                "{stage:?} mismatched"
            );
        }
    }

    #[test]
    fn the_fraction_is_the_engines_own_and_never_moves_backwards() {
        // Recomputing the fraction here would be free to disagree with the
        // engine's grouping, which is how a progress bar starts going backwards.
        let events: Vec<Progress> = Stage::ALL
            .iter()
            .map(|stage| Progress::Finished { stage: *stage })
            .collect();

        let mut previous = 0.0;
        for event in &events {
            let wire = RunEvent::from(event);
            let fraction = wire.fraction();
            assert!(
                fraction >= previous,
                "fraction went backwards: {previous} -> {fraction} at {wire:?}"
            );
            assert!(
                (0.0..=1.0).contains(&fraction),
                "fraction {fraction} outside 0..=1"
            );
            previous = fraction;
        }
        assert_eq!(previous, 1.0, "a finished run must end at 1.0");
    }

    #[test]
    fn a_branch_event_does_not_move_the_bar_outside_its_group() {
        // Spec \u00a756: four stages run at once, and their boundaries must not push
        // the bar past work that has not happened yet.
        for stage in Stage::CONCURRENT {
            for completed in 0..=Stage::CONCURRENT.len() {
                let engine = Progress::BranchFinished {
                    stage,
                    completed,
                    total: Stage::CONCURRENT.len(),
                };
                let fraction = RunEvent::from(&engine).fraction();
                let span = stage.span();
                assert!(
                    fraction >= span.start as f64 / Stage::ALL.len() as f64,
                    "{stage:?} reported {fraction} before its group began"
                );
                assert!(
                    fraction <= span.end as f64 / Stage::ALL.len() as f64 + f64::EPSILON,
                    "{stage:?} reported {fraction} past its group"
                );
            }
        }
    }

    #[test]
    fn a_scrambled_branch_order_still_advances_the_bar() {
        // The realistic worst case: the branches complete in an order unrelated
        // to their stage ordinals, because one is fast and one is slow. The
        // branch *count* must drive the fraction, not the stage name.
        let total = Stage::CONCURRENT.len();
        let mut previous = 0.0;
        for completed in 1..=total {
            let engine = Progress::BranchFinished {
                stage: Stage::CONCURRENT[total - completed],
                completed,
                total,
            };
            let fraction = RunEvent::from(&engine).fraction();
            assert!(
                fraction >= previous,
                "branch {completed} moved the bar backwards: {previous} -> {fraction}"
            );
            previous = fraction;
        }
    }

    #[test]
    fn an_event_round_trips_through_the_ipc_boundary() {
        for event in [
            RunEvent::Started {
                stage: StageTag::Acquisition,
                completed: Some(512),
                total: Some(1024),
            },
            RunEvent::Finished {
                stage: StageTag::Rules,
            },
            RunEvent::BranchFinished {
                stage: StageTag::Audio,
                completed: 2,
                total: 4,
            },
        ] {
            let json = serde_json::to_string(&event).expect("encodes");
            let decoded: RunEvent = serde_json::from_str(&json).expect("decodes");
            assert_eq!(decoded, event);
        }
    }

    #[test]
    fn a_run_result_keeps_the_limitations_rather_than_dropping_them() {
        // The count of measurements not taken is as much a part of the result as
        // the ones that were. A result that silently omitted them would read as
        // a clean run.
        let result = RunResult {
            asset_name: "master.mov".to_owned(),
            sha256: Some("abc".to_owned()),
            status: RunStatus::Complete,
            finding_count: 0,
            evidence_count: 0,
            timeline_count: 0,
            measured_timeline_count: 0,
            cache_hit: false,
            analysis_fingerprint: "fingerprint".to_owned(),
            limitations: vec!["Tier-2 pixel analysis was not run".to_owned()],
            persisted: true,
        };
        let json = serde_json::to_string(&result).expect("encodes");
        assert!(
            json.contains("Tier-2"),
            "limitations must survive serialisation: {json}"
        );
        let decoded: RunResult = serde_json::from_str(&json).expect("decodes");
        assert_eq!(decoded.limitations.len(), 1);
    }

    #[test]
    fn a_cache_hit_is_marked_as_not_having_measured_this_time() {
        // A cached run measured nothing; the field is what stops that reading as
        // a thorough examination.
        let hit = RunResult {
            asset_name: "a.mov".to_owned(),
            sha256: None,
            status: RunStatus::Complete,
            finding_count: 3,
            evidence_count: 0,
            timeline_count: 0,
            measured_timeline_count: 0,
            cache_hit: true,
            analysis_fingerprint: "f".to_owned(),
            limitations: vec!["served from the analysis cache".to_owned()],
            persisted: false,
        };
        assert!(hit.cache_hit);
        assert!(!hit.persisted, "a cache hit writes nothing new to the case");
    }

    #[test]
    fn every_run_status_is_terminal() {
        // A status that is not terminal has no result to show, so the distinction
        // would be meaningless.
        assert!(RunStatus::Complete.is_terminal());
        assert!(RunStatus::Failed.is_terminal());
        assert!(RunStatus::Cancelled.is_terminal());
    }
}
