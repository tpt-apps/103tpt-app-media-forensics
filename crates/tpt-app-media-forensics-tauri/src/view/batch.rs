//! The batch results dashboard (spec §83).
//!
//! ```text
//! File                  Status      Findings
//! ------------------------------------------------
//! master01.mov          PASS        0
//! master02.mov          WARN        4
//! master03.mov          FAIL        2
//! master04.mov          PASS        0
//! ```
//!
//! # The status comes from the engine, not from a second rule here
//!
//! Spec §83's three labels map onto the engine's own
//! [`ValidationResult::from_findings`], which is what `validate` uses. The batch
//! screen calls that same function rather than re-deriving a verdict from
//! severity counts. Two implementations of "does this file pass" in one
//! repository is precisely how the GUI and the CLI come to disagree, which is
//! the one thing spec §78 exists to prevent — and a delivery decision made from
//! the GUI would then contradict the report printed from the terminal.
//!
//! # A file that could not be analysed is neither pass nor fail
//!
//! `Failed` here means the engine could not read the file, which is an
//! operational outcome, not a measurement of the media. It gets its own status
//! rather than being folded into `Fail`, because a corrupted intake folder and
//! a file that failed delivery are different problems with different remedies.

use serde::{Deserialize, Serialize};

use tpt_app_media_forensics_report::ValidationResult;

use tpt_app_media_forensics_model::{Finding, Severity};

/// How one file fared in a batch run (spec §83).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BatchStatus {
    /// Analysed; nothing blocks delivery.
    Pass,
    /// Analysed; findings that do not block delivery.
    Warn,
    /// Analysed; at least one blocking finding.
    Fail,
    /// The file could not be analysed at all.
    ///
    /// Not a verdict on the media: the engine never got far enough to have one.
    Unreadable,
    /// The file was skipped, with a reason.
    Skipped,
}

impl BatchStatus {
    /// The label drawn in the status column.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
            Self::Unreadable => "UNREADABLE",
            Self::Skipped => "SKIPPED",
        }
    }

    /// The engine's own verdict, where there is one.
    ///
    /// `None` for the operational outcomes, which have no validation result
    /// because no analysis produced findings to derive one from.
    #[must_use]
    pub const fn validation(self) -> Option<ValidationResult> {
        match self {
            Self::Pass => Some(ValidationResult::Pass),
            Self::Warn => Some(ValidationResult::PassWithWarnings),
            Self::Fail => Some(ValidationResult::Fail),
            Self::Unreadable | Self::Skipped => None,
        }
    }

    /// Whether this status blocks delivery.
    ///
    /// `Unreadable` and `Skipped` do block, and this is the one place the
    /// dashboard asserts a judgement. An unexamined file cannot be certified as
    /// deliverable; treating "we could not look at it" as equivalent to "it was
    /// fine" is how an incomplete intake passes QC.
    #[must_use]
    pub const fn blocks_delivery(self) -> bool {
        matches!(self, Self::Fail | Self::Unreadable | Self::Skipped)
    }
}

/// One file's row in the batch table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchRow {
    /// The file's path, as visited.
    pub path: String,
    /// The file's display name.
    pub name: String,
    /// How it fared.
    pub status: BatchStatus,
    /// Findings the analysis produced.
    pub finding_count: u64,
    /// The most severe finding's severity, when there is one.
    pub worst_severity: Option<Severity>,
    /// Why it could not be analysed, or was skipped.
    ///
    /// Present for the operational outcomes and absent otherwise. Carrying the
    /// reason beside the row is what stops an analyst seeing "UNREADABLE" with
    /// no indication of which of a thousand files it was or why.
    pub reason: Option<String>,
    /// Whether the result was served from the analysis cache.
    pub cache_hit: bool,
}

impl BatchRow {
    /// A row for a file the engine could not analyse.
    #[must_use]
    pub fn unreadable(path: impl Into<String>, reason: impl Into<String>) -> Self {
        let path = path.into();
        Self {
            name: file_name(&path),
            path,
            status: BatchStatus::Unreadable,
            finding_count: 0,
            worst_severity: None,
            reason: Some(reason.into()),
            cache_hit: false,
        }
    }

    /// A row for a file that was not examined.
    #[must_use]
    pub fn skipped(path: impl Into<String>, reason: impl Into<String>) -> Self {
        let path = path.into();
        Self {
            name: file_name(&path),
            path,
            status: BatchStatus::Skipped,
            finding_count: 0,
            worst_severity: None,
            reason: Some(reason.into()),
            cache_hit: false,
        }
    }
}

/// Extracts the display name from a path.
fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
        .to_owned()
}

/// How the batch table is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchSort {
    /// File name, ascending.
    Name,
    /// Status, most serious first.
    Status,
    /// Finding count, most first.
    Findings,
    /// Most severe finding first.
    Severity,
}

impl BatchSort {
    /// Every ordering, in the order the sort control lists them.
    pub const ALL: [Self; 4] = [Self::Status, Self::Severity, Self::Findings, Self::Name];
}

/// The batch results table (spec §83).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchView {
    /// One row per file visited, in the order they were scanned.
    pub rows: Vec<BatchRow>,
}

impl BatchView {
    /// Builds the table from per-file outcomes.
    #[must_use]
    pub fn from_rows(rows: Vec<BatchRow>) -> Self {
        Self { rows }
    }

    /// Builds a row for a file the engine analysed.
    ///
    /// The status is derived by calling the engine's own
    /// [`ValidationResult::from_findings`], so this screen and the `validate`
    /// command cannot reach different verdicts on the same findings.
    #[must_use]
    pub fn analysed(path: impl Into<String>, findings: &[Finding], cache_hit: bool) -> BatchRow {
        let path = path.into();
        let verdict = ValidationResult::from_findings(findings);
        // `Severity::ALL` is descending, so the first severity present is the
        // worst one. Sorting a three-element constant rather than comparing
        // severities by hand means a new severity cannot be missed here.
        let worst = tpt_app_media_forensics_model::Severity::ALL
            .iter()
            .find(|s| findings.iter().any(|f| f.severity == **s))
            .copied();

        BatchRow {
            name: file_name(&path),
            path,
            status: match verdict {
                ValidationResult::Pass => BatchStatus::Pass,
                ValidationResult::PassWithWarnings => BatchStatus::Warn,
                ValidationResult::Fail => BatchStatus::Fail,
            },
            finding_count: findings.len() as u64,
            worst_severity: worst,
            reason: None,
            cache_hit,
        }
    }

    /// Returns the rows in the requested order.
    ///
    /// Every ordering breaks ties by name, so two runs over an unchanged folder
    /// produce the same table (spec §77) — without it, a scan that happened to
    /// complete two equal rows in the other order would look like a change.
    #[must_use]
    pub fn sorted(&self, sort: BatchSort) -> Vec<&BatchRow> {
        let mut rows: Vec<&BatchRow> = self.rows.iter().collect();
        // Ranks are written so that a *lower* value is more serious, and
        // `sort_by` puts lower values first — so the comparison is `a.cmp(b)`:
        // the more serious of the two pairs sorts ahead.
        //
        // The explicit ranks rather than `Severity`'s own `Ord` are deliberate:
        // `Severity` inverts its ordering so that ascending puts Critical first,
        // which is right for report sections and confusing anywhere else.
        // Deriving it from `severity_rank` keeps the two conventions from being
        // mixed up at a glance.
        rows.sort_by(|a, b| {
            let primary = match sort {
                BatchSort::Name => a.name.cmp(&b.name),
                BatchSort::Status => status_rank(a.status).cmp(&status_rank(b.status)),
                BatchSort::Findings => b.finding_count.cmp(&a.finding_count),
                BatchSort::Severity => {
                    severity_rank(a.worst_severity).cmp(&severity_rank(b.worst_severity))
                }
            };
            // Name last, always ascending, so equal rows have a total order and
            // two runs over an unchanged folder render identically (spec §77).
            primary.then_with(|| a.name.cmp(&b.name))
        });
        rows
    }

    /// Counts of each status, for the summary line above the table.
    #[must_use]
    pub fn counts(&self) -> BatchCounts {
        let mut counts = BatchCounts::default();
        for row in &self.rows {
            match row.status {
                BatchStatus::Pass => counts.passed += 1,
                BatchStatus::Warn => counts.warned += 1,
                BatchStatus::Fail => counts.failed += 1,
                BatchStatus::Unreadable => counts.unreadable += 1,
                BatchStatus::Skipped => counts.skipped += 1,
            }
            counts.findings += row.finding_count;
        }
        counts.total = self.rows.len() as u64;
        counts
    }

    /// Whether every file passed.
    ///
    /// False when the batch was empty, and false when anything could not be
    /// examined. An empty run has certified nothing.
    #[must_use]
    pub fn all_passed(&self) -> bool {
        !self.rows.is_empty() && self.rows.iter().all(|r| r.status == BatchStatus::Pass)
    }
}

/// Ranks a severity so that a *lower* rank is more serious.
///
/// Rows with no findings rank last (`u8::MAX`), not first: an unreadable file
/// has no severity, and sorting it to the top would put the file the engine
/// could not read above one it measured and found critical.
fn severity_rank(severity: Option<Severity>) -> u8 {
    severity.map_or(u8::MAX, |s| match s {
        Severity::Critical => 0,
        Severity::Significant => 1,
        Severity::Warning => 2,
        Severity::Info => 3,
    })
}

/// Ranks a batch status so that a *lower* rank is more serious.
fn status_rank(status: BatchStatus) -> u8 {
    match status {
        BatchStatus::Fail => 0,
        BatchStatus::Unreadable => 1,
        BatchStatus::Skipped => 2,
        BatchStatus::Warn => 3,
        BatchStatus::Pass => 4,
    }
}

/// Status totals for a batch run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BatchCounts {
    /// Files visited.
    pub total: u64,
    /// Files that passed.
    pub passed: u64,
    /// Files that passed with findings that do not block.
    pub warned: u64,
    /// Files with blocking findings.
    pub failed: u64,
    /// Files that could not be analysed.
    pub unreadable: u64,
    /// Files that were skipped.
    pub skipped: u64,
    /// Findings across every analysed file.
    pub findings: u64,
}

impl BatchCounts {
    /// Whether the run examined every file it visited.
    ///
    /// Distinct from `all_passed`: a run can have passed everything it managed
    /// to read while leaving a fifth of the folder unread, and an intake summary
    /// that said "all passed" would be claiming otherwise.
    #[must_use]
    pub fn examined_everything(&self) -> bool {
        self.total > 0 && self.unreadable == 0 && self.skipped == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::testing::finding;
    use tpt_app_media_forensics_model::FindingStatus;

    fn findings(severities: &[Severity]) -> Vec<Finding> {
        severities
            .iter()
            .map(|s| finding(*s, FindingStatus::New))
            .collect()
    }

    #[test]
    fn a_clean_file_passes() {
        let row = BatchView::analysed("C:/intake/master01.mov", &[], false);
        assert_eq!(row.status, BatchStatus::Pass);
        assert_eq!(row.finding_count, 0);
        assert_eq!(row.name, "master01.mov");
    }

    #[test]
    fn warnings_warn_rather_than_fail() {
        // `ValidationResult::from_findings` is the engine's own predicate; this
        // pins that the screen defers to it rather than re-deciding.
        let row = BatchView::analysed("C:/intake/m.mov", &findings(&[Severity::Warning]), false);
        assert_eq!(row.status, BatchStatus::Warn);
        assert_eq!(
            row.status.validation(),
            Some(ValidationResult::PassWithWarnings)
        );
    }

    #[test]
    fn a_blocking_severity_fails() {
        let row = BatchView::analysed("C:/intake/m.mov", &findings(&[Severity::Critical]), false);
        assert_eq!(row.status, BatchStatus::Fail);
        assert!(row.status.blocks_delivery());
    }

    #[test]
    fn the_status_agrees_with_the_validate_command_by_construction() {
        // Both call `ValidationResult::from_findings`. If this ever fails, a
        // delivery decision made in the GUI would contradict the printed report.
        for severities in [
            vec![],
            vec![Severity::Info],
            vec![Severity::Warning],
            vec![Severity::Significant],
            vec![Severity::Critical],
            vec![Severity::Warning, Severity::Critical],
        ] {
            let observed = findings(&severities);
            let expected = ValidationResult::from_findings(&observed);
            let row = BatchView::analysed("C:/intake/m.mov", &observed, false);
            assert_eq!(
                row.status.validation(),
                Some(expected),
                "for {severities:?}"
            );
        }
    }

    #[test]
    fn a_file_that_could_not_be_read_is_not_a_pass() {
        // Treating "we could not look at it" as "it was fine" is how an
        // incomplete intake passes QC.
        let row = BatchRow::unreadable("C:/intake/broken.mp4", "not a recognised container");
        assert_eq!(row.status, BatchStatus::Unreadable);
        assert!(row.status.blocks_delivery());
        assert_eq!(row.status.validation(), None);
        assert!(row.reason.is_some());
        assert_eq!(row.finding_count, 0);
    }

    #[test]
    fn an_unreadable_file_carries_no_worst_severity() {
        // Nothing was measured, so there is no severity to report. Zero
        // findings beside a fabricated severity would read as "clean".
        let row = BatchRow::unreadable("C:/intake/broken.mp4", "truncated");
        assert_eq!(row.worst_severity, None);
    }

    #[test]
    fn the_worst_severity_is_the_most_serious_present() {
        let row = BatchView::analysed(
            "C:/intake/m.mov",
            &findings(&[Severity::Info, Severity::Warning, Severity::Critical]),
            false,
        );
        assert_eq!(row.worst_severity, Some(Severity::Critical));
        assert_eq!(row.finding_count, 3);
    }

    #[test]
    fn sorting_by_severity_puts_the_worst_first_and_clean_files_last() {
        let view = BatchView::from_rows(vec![
            BatchView::analysed("C:/intake/a.mov", &[], false),
            BatchView::analysed("C:/intake/b.mov", &findings(&[Severity::Warning]), false),
            BatchView::analysed("C:/intake/c.mov", &findings(&[Severity::Critical]), false),
        ]);

        let order: Vec<&str> = view
            .sorted(BatchSort::Severity)
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(order, vec!["c.mov", "b.mov", "a.mov"]);
    }

    #[test]
    fn sorting_by_status_puts_the_most_serious_outcome_first() {
        let view = BatchView::from_rows(vec![
            BatchView::analysed("C:/intake/a.mov", &[], false),
            BatchRow::unreadable("C:/intake/z.mp4", "truncated"),
            BatchView::analysed("C:/intake/b.mov", &findings(&[Severity::Critical]), false),
        ]);

        let order: Vec<BatchStatus> = view
            .sorted(BatchSort::Status)
            .iter()
            .map(|r| r.status)
            .collect();
        assert_eq!(
            order,
            vec![
                BatchStatus::Fail,
                BatchStatus::Unreadable,
                BatchStatus::Pass
            ]
        );
    }

    #[test]
    fn ties_are_broken_by_name_so_two_runs_agree() {
        // Spec §77: without a total order, two runs over an unchanged folder
        // could produce different tables for identical content.
        let view = BatchView::from_rows(vec![
            BatchView::analysed("C:/intake/z.mov", &findings(&[Severity::Warning]), false),
            BatchView::analysed("C:/intake/a.mov", &findings(&[Severity::Warning]), false),
        ]);

        let order: Vec<&str> = view
            .sorted(BatchSort::Status)
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(order, vec!["a.mov", "z.mov"]);
    }

    #[test]
    fn the_counts_account_for_every_file() {
        let view = BatchView::from_rows(vec![
            BatchView::analysed("C:/intake/a.mov", &[], false),
            BatchView::analysed("C:/intake/b.mov", &findings(&[Severity::Warning]), false),
            BatchView::analysed("C:/intake/c.mov", &findings(&[Severity::Critical]), false),
            BatchRow::unreadable("C:/intake/d.mp4", "truncated"),
            BatchRow::skipped("C:/intake/e.txt", "not a media extension"),
        ]);

        let counts = view.counts();
        assert_eq!(counts.total, 5);
        assert_eq!(counts.passed, 1);
        assert_eq!(counts.warned, 1);
        assert_eq!(counts.failed, 1);
        assert_eq!(counts.unreadable, 1);
        assert_eq!(counts.skipped, 1);
        assert_eq!(counts.findings, 2);
    }

    #[test]
    fn a_run_with_an_unreadable_file_did_not_examine_everything() {
        // Distinct from `all_passed`: everything readable can pass while a
        // fifth of the folder was never looked at.
        let view = BatchView::from_rows(vec![
            BatchView::analysed("C:/intake/a.mov", &[], false),
            BatchRow::unreadable("C:/intake/b.mp4", "truncated"),
        ]);

        assert!(!view.all_passed());
        assert!(!view.counts().examined_everything());
    }

    #[test]
    fn a_run_where_everything_passed_reports_it() {
        let view = BatchView::from_rows(vec![
            BatchView::analysed("C:/intake/a.mov", &[], false),
            BatchView::analysed("C:/intake/b.mov", &[], false),
        ]);
        assert!(view.all_passed());
        assert!(view.counts().examined_everything());
    }

    #[test]
    fn an_empty_run_certifies_nothing() {
        // "All passed" over zero files would be a vacuous claim.
        let view = BatchView::from_rows(Vec::new());
        assert!(!view.all_passed());
        assert!(!view.counts().examined_everything());
    }

    #[test]
    fn a_cache_hit_is_marked_so_a_fast_run_is_not_mistaken_for_a_thorough_one() {
        let row = BatchView::analysed("C:/intake/a.mov", &[], true);
        assert!(row.cache_hit);
    }

    #[test]
    fn the_status_labels_match_the_specification() {
        // Spec §83's table prints PASS, WARN and FAIL.
        assert_eq!(BatchStatus::Pass.label(), "PASS");
        assert_eq!(BatchStatus::Warn.label(), "WARN");
        assert_eq!(BatchStatus::Fail.label(), "FAIL");
    }

    #[test]
    fn the_table_round_trips_through_the_ipc_boundary() {
        let view = BatchView::from_rows(vec![
            BatchView::analysed("C:/intake/a.mov", &findings(&[Severity::Warning]), false),
            BatchRow::unreadable("C:/intake/b.mp4", "truncated"),
        ]);
        let json = serde_json::to_string(&view).expect("encodes");
        let decoded: BatchView = serde_json::from_str(&json).expect("decodes");
        assert_eq!(decoded, view);
    }
}
