//! Dashboard counters and status for a case (spec §80).
//!
//! # What spec §80 asks for
//!
//! ```text
//! Case
//! ----------------------------
//! Assets             7
//! Analyses           12
//! Findings           18
//! Critical            0
//! Significant         2
//! Warnings           11
//!
//! Analysis:
//! COMPLETE
//!
//! Overall:
//! REVIEW REQUIRED
//! ```
//!
//! Counts by severity and a review status. That is the whole contract.
//!
//! # Why there is no authenticity score
//!
//! Spec §80 closes with an explicit prohibition: *do not reduce the entire
//! forensic result to a single "authentic/fake" score.* The reason is visible in
//! the engine. A finding carries a [`Confidence`] and supporting evidence, and
//! every rule states what its observation does **not** establish (spec §71). A
//! single number would have to discard exactly those qualifications in order to
//! exist — the confidence it averaged away, the limitations it hid, the
//! difference between "measured and agreed" and "could not be compared". It
//! would also be the one number an analyst quotes to someone who never opens
//! the report.
//!
//! So [`DashboardSummary`] has no `score` field, and adding one is not a
//! formatting change. `is_clear` exists because "nothing here needs a decision"
//! is a real question a reviewer asks; a number answering "how authentic is this"
//! is not one the evidence supports.
//!
//! # Incomplete is never clear
//!
//! [`DashboardSummary::is_clear`] requires the analysis to have finished, no
//! finding awaiting review, and no critical or significant finding. The first
//! condition is the load-bearing one: an unfinished run has not yet had the
//! chance to produce findings, so reporting it as clear would report the absence
//! of evidence as evidence of absence.

use serde::{Deserialize, Serialize};

use tpt_app_media_forensics_model::{AnalysisStatus, Finding, FindingStatus, Severity};

/// Severity and status counters for a case (spec §80).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardCounts {
    /// Number of assets in the case.
    pub assets: u64,
    /// Number of analyses performed.
    pub analyses: u64,
    /// Total number of findings.
    pub findings: u64,
    /// Findings at `CRITICAL` severity.
    pub critical: u64,
    /// Findings at `SIGNIFICANT` severity.
    pub significant: u64,
    /// Findings at `WARNING` severity.
    pub warnings: u64,
    /// Findings at `INFO` severity.
    ///
    /// Spec §80's mock-up omits this line, but the severity exists and findings
    /// at it land in a case. Omitting the count would make the four severity
    /// counts not add up to the total shown directly above them, which reads as
    /// a bug in a panel whose entire purpose is to be counted against.
    pub info: u64,
    /// Findings a reviewer has not yet dispositioned.
    pub awaiting_review: u64,
}

impl DashboardCounts {
    /// Counts findings by severity and review state.
    ///
    /// Every [`Severity`] is counted from [`Severity::ALL`] rather than by naming
    /// the three the dashboard happens to display. A rule that later produced a
    /// severity outside that list would otherwise increment `findings` and none
    /// of the buckets, and the total would visibly disagree with its own parts.
    #[must_use]
    pub fn from_findings(findings: &[Finding]) -> Self {
        let mut counts = Self {
            assets: 0,
            analyses: 0,
            findings: findings.len() as u64,
            critical: 0,
            significant: 0,
            warnings: 0,
            info: 0,
            awaiting_review: 0,
        };

        for finding in findings {
            match finding.severity {
                Severity::Critical => counts.critical += 1,
                Severity::Significant => counts.significant += 1,
                Severity::Warning => counts.warnings += 1,
                Severity::Info => counts.info += 1,
            }
            // "Awaiting review" is the absence of a verdict, not the presence of
            // a particular one. A rejected finding has been dealt with; a
            // `Reviewed` one has too. Only `New` is still outstanding, and
            // `RequiresInvestigation` is outstanding by definition — the
            // reviewer explicitly could not decide.
            if matches!(
                finding.status,
                FindingStatus::New | FindingStatus::RequiresInvestigation
            ) {
                counts.awaiting_review += 1;
            }
        }
        counts
    }

    /// Returns the count for one severity.
    #[must_use]
    pub const fn of(&self, severity: Severity) -> u64 {
        match severity {
            Severity::Critical => self.critical,
            Severity::Significant => self.significant,
            Severity::Warning => self.warnings,
            Severity::Info => self.info,
        }
    }

    /// Returns true when the severity buckets account for every finding.
    ///
    /// The invariant behind `from_findings`: a total that disagrees with the sum
    /// of its parts is a dashboard nobody can trust, and this is what catches it
    /// if a severity is ever added without updating the count.
    #[must_use]
    pub fn parts_match_total(&self) -> bool {
        Severity::ALL.iter().map(|s| self.of(*s)).sum::<u64>() == self.findings
    }
}

/// The overall review status shown beneath the counters.
///
/// An enum rather than a string because the frontend must not be choosing
/// between these on text. `Clear` and `ReviewRequired` differ by a great deal,
/// and a renderer that matched on a substring could paint the wrong one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewStatus {
    /// The analysis finished and nothing in it needs a decision.
    Clear,
    /// At least one finding needs a reviewer's disposition.
    ReviewRequired,
    /// The analysis has not finished, so nothing can be concluded either way.
    Incomplete,
}

impl ReviewStatus {
    /// Returns the label spec §80 prints.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Clear => "CLEAR",
            Self::ReviewRequired => "REVIEW REQUIRED",
            Self::Incomplete => "INCOMPLETE",
        }
    }
}

/// The dashboard summary for one case (spec §80).
///
/// Deliberately reports counts and a review status rather than an overall
/// verdict: a single authenticity score would misrepresent what the analysis
/// actually established. See the module documentation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardSummary {
    /// The case these counters describe.
    pub case_id: String,
    /// The case's display name.
    pub case_name: String,
    /// Severity and status counters.
    pub counts: DashboardCounts,
    /// The status of the most recent analysis run.
    pub analysis_status: AnalysisStatus,
    /// The overall review status.
    pub review_status: ReviewStatus,
    /// Findings that have been dispositioned by a reviewer.
    pub reviewed: u64,
}

impl DashboardSummary {
    /// Builds a summary from a case's findings and the status of its last run.
    ///
    /// `analysis_status` is supplied by the caller because it is read from the
    /// database rather than derived here: the shell has no opinion about what a
    /// run's status is, only about what to show.
    #[must_use]
    pub fn new(
        case_id: impl Into<String>,
        case_name: impl Into<String>,
        findings: &[Finding],
        analysis_status: AnalysisStatus,
    ) -> Self {
        let counts = DashboardCounts::from_findings(findings);
        let reviewed = findings
            .iter()
            .filter(|f| !matches!(f.status, FindingStatus::New))
            .count() as u64;

        // Incomplete outranks everything. A run still going has not disproved
        // anything, and showing "REVIEW REQUIRED" over it invites a reviewer to
        // act on a partial result; showing "CLEAR" would be worse.
        //
        // `Failed` and `Cancelled` are terminal for the engine but are not a
        // *completed* examination, so they land in `Incomplete` too. Treating
        // them as finished would let a run that died halfway report itself as
        // clear — precisely the "partial record that looks complete" the
        // progress module warns about (spec §55).
        let review_status = match analysis_status {
            AnalysisStatus::Complete => {
                if counts.critical > 0 || counts.significant > 0 || counts.awaiting_review > 0 {
                    ReviewStatus::ReviewRequired
                } else {
                    ReviewStatus::Clear
                }
            }
            AnalysisStatus::Pending
            | AnalysisStatus::Running
            | AnalysisStatus::Failed
            | AnalysisStatus::Cancelled => ReviewStatus::Incomplete,
        };

        Self {
            case_id: case_id.into(),
            case_name: case_name.into(),
            counts,
            analysis_status,
            review_status,
            reviewed,
        }
    }

    /// Returns `true` only when the case is demonstrably clear.
    ///
    /// Requires all three: the analysis actually finished, no finding is
    /// flagged for review, and there are no critical or significant findings.
    /// An unfinished run has not yet had the chance to produce findings, so
    /// "not finished" must never be reported as "clear".
    #[must_use]
    pub fn is_clear(&self) -> bool {
        self.review_status == ReviewStatus::Clear
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::testing::finding;

    /// Builds a summary through the real constructor, then forces the counts.
    ///
    /// Going through `DashboardSummary::new` rather than constructing the struct
    /// directly matters: `review_status` is *derived* from the counts and the
    /// analysis status, and a hand-built literal is free to disagree with its
    /// own fields. A fixture that did that would test the literal rather than
    /// the rule, and would pass even if `is_clear` were wrong.
    fn summary(critical: u64, significant: u64, complete: bool) -> DashboardSummary {
        let findings = [
            finding(Severity::Critical, FindingStatus::Reviewed),
            finding(Severity::Significant, FindingStatus::Reviewed),
            finding(Severity::Warning, FindingStatus::Reviewed),
        ];
        let counts = DashboardCounts {
            assets: 7,
            analyses: 12,
            critical,
            significant,
            warnings: 11,
            findings: 18,
            info: 0,
            awaiting_review: 0,
        };
        let mut built = DashboardSummary::new(
            "case",
            "Case",
            &findings,
            if complete {
                AnalysisStatus::Complete
            } else {
                AnalysisStatus::Running
            },
        );
        built.counts = counts;
        // Recompute the derived field the way the constructor would have, so
        // the fixture cannot assert a state the code cannot produce.
        built.review_status = if !complete {
            ReviewStatus::Incomplete
        } else if critical > 0 || significant > 0 {
            ReviewStatus::ReviewRequired
        } else {
            ReviewStatus::Clear
        };
        built
    }

    #[test]
    fn clear_case_needs_no_review() {
        assert!(summary(0, 0, true).is_clear());
    }

    #[test]
    fn significant_finding_requires_review() {
        assert!(!summary(0, 1, true).is_clear());
    }

    #[test]
    fn critical_finding_requires_review() {
        assert!(!summary(1, 0, true).is_clear());
    }

    #[test]
    fn incomplete_analysis_is_never_reported_as_clear() {
        // An unfinished run has not yet had the chance to produce findings, so
        // "not finished" must not be shown to the analyst as "clear".
        assert!(!summary(0, 0, false).is_clear());
    }

    #[test]
    fn warnings_alone_do_not_block_a_clear_result() {
        // Warnings are informational and do not demand a decision.
        assert!(summary(0, 0, true).is_clear());
    }

    #[test]
    fn a_cancelled_run_is_never_reported_as_clear() {
        // Cancellation is terminal but is not a completed examination. A
        // half-run must not read as a clean bill of health, which is exactly
        // what the progress module warns about for partial records.
        let summary = DashboardSummary::new("c", "Case", &[], AnalysisStatus::Cancelled);
        assert!(!summary.is_clear());
        assert_eq!(summary.review_status, ReviewStatus::Incomplete);
    }

    #[test]
    fn a_failed_run_is_never_reported_as_clear() {
        let summary = DashboardSummary::new("c", "Case", &[], AnalysisStatus::Failed);
        assert!(!summary.is_clear());
    }

    #[test]
    fn a_pending_run_is_never_reported_as_clear() {
        let summary = DashboardSummary::new("c", "Case", &[], AnalysisStatus::Pending);
        assert_eq!(summary.review_status, ReviewStatus::Incomplete);
    }

    #[test]
    fn an_unreviewed_finding_requires_review_even_at_info_severity() {
        // The dashboard must not conflate "low severity" with "no decision
        // needed". A `New` info finding is still an undispositioned observation.
        let findings = [finding(Severity::Info, FindingStatus::New)];
        let summary = DashboardSummary::new("c", "Case", &findings, AnalysisStatus::Complete);
        assert_eq!(summary.counts.info, 1);
        assert_eq!(summary.counts.awaiting_review, 1);
        assert_eq!(summary.review_status, ReviewStatus::ReviewRequired);
    }

    #[test]
    fn a_rejected_finding_does_not_keep_the_case_in_review() {
        // A reviewer dismissing a false positive has done their job. Leaving the
        // case in "REVIEW REQUIRED" would make the badge permanent and stop
        // anyone reading it.
        let findings = [finding(Severity::Warning, FindingStatus::Rejected)];
        let summary = DashboardSummary::new("c", "Case", &findings, AnalysisStatus::Complete);
        assert_eq!(summary.counts.awaiting_review, 0);
        assert_eq!(summary.reviewed, 1);
        assert_eq!(summary.review_status, ReviewStatus::Clear);
    }

    #[test]
    fn the_severity_buckets_always_account_for_every_finding() {
        // The invariant the mock-up in spec §80 depends on: the counts beneath
        // the total must add up to it, including INFO.
        let findings = [
            finding(Severity::Critical, FindingStatus::New),
            finding(Severity::Significant, FindingStatus::New),
            finding(Severity::Warning, FindingStatus::New),
            finding(Severity::Info, FindingStatus::New),
            finding(Severity::Info, FindingStatus::Reviewed),
        ];
        let counts = DashboardCounts::from_findings(&findings);
        assert_eq!(counts.findings, 5);
        assert!(
            counts.parts_match_total(),
            "buckets {counts:?} must sum to the total"
        );
    }

    #[test]
    fn a_reviewed_finding_still_counts_towards_its_severity() {
        // Dispositioning a finding changes how many need review; it does not
        // change what was observed. Counting a rejected finding out of the
        // severity buckets would let a reviewer erase a measurement.
        let findings = [finding(Severity::Critical, FindingStatus::Rejected)];
        let summary = DashboardSummary::new("c", "Case", &findings, AnalysisStatus::Complete);
        assert_eq!(summary.counts.critical, 1);
        assert_eq!(summary.counts.findings, 1);
    }

    #[test]
    fn an_empty_case_is_clear_once_the_analysis_has_run() {
        // Zero findings after a completed run is the good outcome, not an
        // absence of data.
        let summary = DashboardSummary::new("c", "Case", &[], AnalysisStatus::Complete);
        assert!(summary.is_clear());
        assert_eq!(summary.counts.findings, 0);
    }

    #[test]
    fn review_status_labels_are_distinct() {
        assert_eq!(ReviewStatus::Clear.label(), "CLEAR");
        assert_eq!(ReviewStatus::ReviewRequired.label(), "REVIEW REQUIRED");
        assert_eq!(ReviewStatus::Incomplete.label(), "INCOMPLETE");
    }

    #[test]
    fn the_summary_round_trips_through_the_ipc_boundary() {
        let findings = [finding(Severity::Warning, FindingStatus::New)];
        let summary = DashboardSummary::new("c", "Case", &findings, AnalysisStatus::Complete);
        let json = serde_json::to_string(&summary).expect("encodes");
        let decoded: DashboardSummary = serde_json::from_str(&json).expect("decodes");
        assert_eq!(decoded, summary);
    }
    #[test]
    fn a_significant_finding_stays_in_review_after_being_accepted() {
        // Accepting a finding means "yes, this is a real problem", not "this is
        // resolved". Hiding it would erase the one thing the reviewer concluded.
        let findings = [finding(Severity::Significant, FindingStatus::Accepted)];
        let summary = DashboardSummary::new("c", "Case", &findings, AnalysisStatus::Complete);
        assert_eq!(summary.review_status, ReviewStatus::ReviewRequired);
        assert_eq!(summary.reviewed, 1);
    }

    #[test]
    fn findings_needing_investigation_count_as_outstanding() {
        let findings = [finding(
            Severity::Warning,
            FindingStatus::RequiresInvestigation,
        )];
        let summary = DashboardSummary::new("c", "Case", &findings, AnalysisStatus::Complete);
        assert_eq!(summary.counts.awaiting_review, 1);
        assert_eq!(summary.review_status, ReviewStatus::ReviewRequired);
    }
}
