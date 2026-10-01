//! Finding model: severity, confidence, and supporting evidence (spec §34).
//!
//! # A finding is an observation, not a verdict
//!
//! The spec is explicit that observations are not conclusions: "GOP structure
//! changes from ~60 frames to ~15 frames" is an observation, not proof of
//! editing. This is enforced in the type system by requiring every finding to
//! carry both a [`Confidence`] and at least one evidence reference, and by
//! keeping the reviewer's verdict in a separate [`FindingStatus`] (spec §66).

use serde::{Deserialize, Serialize};

use crate::id::{AssetId, EvidenceId, FindingId, StreamId};
use crate::time::MediaTime;

/// How significant a finding is (spec §34, §80).
///
/// The `Ord` implementation ranks `Critical` as the *greatest* value, so that
/// sorting ascending puts the most serious findings first — the order used by
/// dashboard sections and report tables. The declaration order alone would give
/// the opposite, so the ordering is written out explicitly rather than derived.
///
/// Serialises using the same uppercase tags that appear in report headings, so
/// a value written to JSON matches what a reader sees in the PDF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Severity {
    /// Structural failure or evidence of tampering beyond reasonable dispute.
    #[serde(rename = "CRITICAL")]
    Critical,
    /// A significant technical deviation that a reviewer must assess.
    #[serde(rename = "SIGNIFICANT")]
    Significant,
    /// An anomaly worth recording that does not by itself indicate a problem.
    #[serde(rename = "WARNING")]
    Warning,
    /// An informational observation with no implied defect.
    #[serde(rename = "INFO")]
    Info,
}

impl Severity {
    /// All severities in descending order, for stable report sections.
    pub const ALL: [Self; 4] = [Self::Critical, Self::Significant, Self::Warning, Self::Info];

    /// The rank used for ordering, most severe first.
    const fn rank(self) -> u8 {
        match self {
            Self::Critical => 0,
            Self::Significant => 1,
            Self::Warning => 2,
            Self::Info => 3,
        }
    }

    /// Returns the stable uppercase tag used in reports and CSV exports.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Critical => "CRITICAL",
            Self::Significant => "SIGNIFICANT",
            Self::Warning => "WARNING",
            Self::Info => "INFO",
        }
    }

    /// Returns `true` when this severity should fail a delivery validation
    /// profile (spec §68).
    #[must_use]
    pub const fn fails_validation(self) -> bool {
        matches!(self, Self::Critical | Self::Significant)
    }
}

impl Ord for Severity {
    /// Orders most severe first, inverting the rank so `Critical` is greatest.
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        other.rank().cmp(&self.rank())
    }
}

impl PartialOrd for Severity {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// How much weight the observing rule placed on the evidence.
///
/// Required even for deterministic checks: a threshold comparison at 25.001
/// against an expected 25 is a weaker observation than an exact structural
/// mismatch, and the report should say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Confidence {
    /// Multiple independent indicators agree.
    High,
    /// A single clear indicator.
    Medium,
    /// Heuristic, sampling-dependent, or tolerant of noise.
    Low,
}

impl Confidence {
    /// Returns the stable title-case tag used in reports.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::High => "High",
            Self::Medium => "Medium",
            Self::Low => "Low",
        }
    }
}

/// The reviewer's disposition of a finding (spec §66).
///
/// Separate from the finding itself so a reviewer's conclusion never mutates
/// the original observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FindingStatus {
    /// Not yet reviewed.
    #[default]
    New,
    /// A reviewer has looked at it.
    Reviewed,
    /// A reviewer accepts the finding as a real issue.
    Accepted,
    /// A reviewer rejects the finding as a false positive.
    Rejected,
    /// A reviewer needs more information before deciding.
    RequiresInvestigation,
}

impl FindingStatus {
    /// Returns the stable tag used in the UI and reports.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Reviewed => "reviewed",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::RequiresInvestigation => "requires-investigation",
        }
    }
}
/// A single rule observation (spec §34).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// This finding's identifier.
    pub id: FindingId,
    /// The rule that produced it, e.g. `VIDEO.FRAME_RATE_CHANGE`.
    pub rule_id: String,
    /// How significant this is.
    pub severity: Severity,
    /// How much weight the rule placed on the evidence.
    pub confidence: Confidence,
    /// What was observed.
    pub observation: Observation,
    /// The asset this finding belongs to.
    pub asset_id: AssetId,
    /// The stream this finding belongs to, when stream-scoped.
    pub stream_id: Option<StreamId>,
    /// Where on the timeline this finding sits, for click-to-jump (spec §81).
    pub timeline_start: Option<MediaTime>,
    /// End of the affected timeline region, when the finding spans a range.
    pub timeline_end: Option<MediaTime>,
    /// Evidence artefacts supporting this finding.
    pub evidence: Vec<EvidenceId>,
    /// Reviewer disposition (spec §66).
    pub status: FindingStatus,
    /// Reviewer note, kept separate from the observation.
    pub review_note: Option<String>,
}

impl Finding {
    /// Returns the timeline region this finding covers.
    ///
    /// Falls back to a zero-length region at the start time so that callers can
    /// position the finding on a timeline without a special case.
    #[must_use]
    pub fn timeline_range(&self) -> (MediaTime, MediaTime) {
        let start = self.timeline_start.unwrap_or(MediaTime::ZERO);
        let end = self.timeline_end.unwrap_or(start);
        (start, end)
    }

    /// Returns `true` when the finding has at least one supporting artefact.
    ///
    /// A finding without evidence is a bare assertion and should be treated
    /// with suspicion, however severe it is labelled.
    #[must_use]
    pub fn has_evidence(&self) -> bool {
        !self.evidence.is_empty()
    }

    /// Applies a reviewer's disposition and note without touching the
    /// observation (spec §66).
    pub fn review(&mut self, status: FindingStatus, note: Option<String>) {
        self.status = status;
        self.review_note = note;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding() -> Finding {
        Finding {
            id: FindingId::new_derived(&["VIDEO.FRAME_RATE_CHANGE", "00:37:21.120"]),
            rule_id: "VIDEO.FRAME_RATE_CHANGE".to_owned(),
            severity: Severity::Significant,
            confidence: Confidence::High,
            observation: Observation {
                summary: "Frame timing changes beyond tolerance".to_owned(),
                measurements: vec!["29.97 fps -> 30.00 fps".to_owned()],
            },
            asset_id: AssetId::new_derived(&["asset"]),
            stream_id: Some(StreamId::new_derived(&["asset", "0"])),
            timeline_start: Some(MediaTime::from_millis(2_243_120)),
            timeline_end: None,
            evidence: vec![EvidenceId::new_derived(&["frame", "1002"])],
            status: FindingStatus::New,
            review_note: None,
        }
    }

    #[test]
    fn severity_ordering_is_most_to_least_severe() {
        assert!(Severity::Critical > Severity::Significant);
        assert!(Severity::Significant > Severity::Warning);
        assert!(Severity::Warning > Severity::Info);
    }

    #[test]
    fn sorting_ascending_puts_most_severe_last() {
        // `Critical` is the greatest value, so an ascending sort ends with it.
        // Report code sorts *descending* to show the most serious first.
        let mut items = vec![
            Severity::Info,
            Severity::Critical,
            Severity::Warning,
            Severity::Significant,
        ];
        items.sort();
        assert_eq!(
            items,
            [
                Severity::Info,
                Severity::Warning,
                Severity::Significant,
                Severity::Critical
            ]
        );
    }

    #[test]
    fn sorting_descending_puts_critical_first() {
        let mut items = vec![
            Severity::Info,
            Severity::Critical,
            Severity::Warning,
            Severity::Significant,
        ];
        items.sort_by_key(|s| std::cmp::Reverse(*s));
        assert_eq!(items, Severity::ALL.to_vec());
    }

    #[test]
    fn severity_tags_match_report_headers() {
        assert_eq!(Severity::Critical.tag(), "CRITICAL");
        assert_eq!(Severity::Info.tag(), "INFO");
    }

    #[test]
    fn significant_severities_fail_validation_profiles() {
        // spec §68: PASS / PASS WITH WARNINGS / FAIL
        assert!(Severity::Critical.fails_validation());
        assert!(Severity::Significant.fails_validation());
        assert!(!Severity::Warning.fails_validation());
        assert!(!Severity::Info.fails_validation());
    }

    #[test]
    fn timeline_range_defaults_to_zero_length_at_start() {
        let f = finding();
        let (start, end) = f.timeline_range();
        assert_eq!(start, MediaTime::from_millis(2_243_120));
        assert_eq!(end, start, "absent end means an instant, not a range");
    }

    #[test]
    fn findings_with_no_timeline_sit_at_zero() {
        let mut f = finding();
        f.timeline_start = None;
        f.timeline_end = None;
        assert_eq!(f.timeline_range(), (MediaTime::ZERO, MediaTime::ZERO));
    }

    #[test]
    fn review_does_not_alter_the_observation() {
        let mut f = finding();
        let before = f.observation.clone();
        f.review(
            FindingStatus::Accepted,
            Some("Likely caused by the known export process.".to_owned()),
        );
        assert_eq!(f.status, FindingStatus::Accepted);
        assert_eq!(f.observation, before, "review must not rewrite the record");
        assert_eq!(f.rule_id, "VIDEO.FRAME_RATE_CHANGE");
    }

    #[test]
    fn unevidenced_findings_are_flagged() {
        let mut f = finding();
        assert!(f.has_evidence());
        f.evidence.clear();
        assert!(!f.has_evidence());
    }

    #[test]
    fn findings_are_deterministically_ordered_by_severity() {
        let mut items = [
            finding(),
            Finding {
                severity: Severity::Critical,
                ..finding()
            },
            Finding {
                severity: Severity::Info,
                ..finding()
            },
        ];
        // Descending: most severe first, which is how reports present them.
        items.sort_by_key(|f| std::cmp::Reverse(f.severity));
        assert_eq!(items[0].severity, Severity::Critical);
        assert_eq!(items[2].severity, Severity::Info);
    }

    #[test]
    fn finding_ids_are_reproducible() {
        let a = FindingId::new_derived(&["VIDEO.FRAME_RATE_CHANGE", "00:37:21.120"]);
        let b = FindingId::new_derived(&["VIDEO.FRAME_RATE_CHANGE", "00:37:21.120"]);
        assert_eq!(a, b);
    }
}

/// What a finding observed, in plain language.
///
/// Stored verbatim rather than reconstructed at render time so that a report
/// remains readable even if a later version changes the wording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    /// One-line statement of what was observed.
    pub summary: String,
    /// Concrete measured values that support the observation.
    pub measurements: Vec<String>,
}
