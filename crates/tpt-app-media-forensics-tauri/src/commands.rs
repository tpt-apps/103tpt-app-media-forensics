//! IPC command surface exposed to the webview frontend (spec §79).
//!
//! Every command here is a thin adapter: it marshals arguments across the IPC
//! boundary and delegates to the core engine. No analysis decision is made in
//! this layer, so the GUI and the CLI cannot drift apart in their results.

use serde::{Deserialize, Serialize};

/// Dashboard counters for a case (spec §80).
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
}

/// The dashboard summary for one case (spec §80).
///
/// Deliberately reports counts and a review status rather than an overall
/// verdict: a single authenticity score would misrepresent what the analysis
/// actually established.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardSummary {
    /// The case these counters describe.
    pub case_id: String,
    /// Severity and status counters.
    pub counts: DashboardCounts,
    /// Whether every analysis has finished.
    pub analysis_complete: bool,
    /// True when at least one finding still needs a reviewer's attention.
    pub review_required: bool,
}

impl DashboardSummary {
    /// Returns `true` only when the case is demonstrably clear.
    ///
    /// Requires all three: the analysis actually finished, no finding is
    /// flagged for review, and there are no critical or significant findings.
    /// An unfinished run has not yet had the chance to produce findings, so
    /// "not finished" must never be reported as "clear".
    #[must_use]
    pub fn is_clear(&self) -> bool {
        self.analysis_complete
            && !self.review_required
            && self.counts.critical == 0
            && self.counts.significant == 0
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn counts(critical: u64, significant: u64) -> DashboardCounts {
        DashboardCounts {
            assets: 7,
            analyses: 12,
            findings: 18,
            critical,
            significant,
            warnings: 11,
        }
    }

    fn summary(critical: u64, significant: u64, complete: bool) -> DashboardSummary {
        DashboardSummary {
            case_id: "case".to_owned(),
            counts: counts(critical, significant),
            analysis_complete: complete,
            review_required: critical > 0 || significant > 0,
        }
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
        // An unfinished run has not had the chance to produce findings yet, so
        // "not finished" must not be shown to the analyst as "clear".
        assert!(!summary(0, 0, false).is_clear());
    }

    #[test]
    fn warnings_alone_do_not_block_a_clear_result() {
        // Warnings are informational and do not demand a decision.
        assert!(summary(0, 0, true).is_clear());
    }
}