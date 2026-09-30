//! Case model (spec §9).
//!
//! A case is the unit of forensic work, not a file. It groups an original, its
//! derivatives, a reference master, and the analyst's notes so that comparisons
//! between related media are possible (spec §67).

use serde::{Deserialize, Serialize};

use crate::asset::MediaAsset;
use crate::id::{AnalysisId, AssetId, CaseId, EvidenceId, FindingId, ReportId};

/// A forensic case (spec §9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Case {
    /// This case's identifier, derived from its name for reproducibility.
    pub id: CaseId,
    /// Human-readable case name.
    pub name: String,
    /// Optional description of the case.
    pub description: Option<String>,
    /// Assets under examination, in acquisition order.
    pub assets: Vec<AssetId>,
    /// Analyses performed against this case's assets.
    pub analyses: Vec<AnalysisId>,
    /// Findings raised by those analyses, in deterministic order.
    pub findings: Vec<FindingId>,
    /// Evidence artefacts retained for this case.
    pub evidence: Vec<EvidenceId>,
    /// Reports generated from this case.
    pub reports: Vec<ReportId>,
}

impl Case {
    /// Builds an empty case.
    ///
    /// The ID is derived from the name only, so creating a case with the same
    /// name twice yields the same ID. That is deliberate: re-running case
    /// creation must not fork a case's history (spec §77).
    #[must_use]
    pub fn new(name: impl Into<String>, description: Option<String>) -> Self {
        let name = name.into();
        let id = CaseId::new_derived(&[name.as_str()]);
        Self {
            id,
            name,
            description,
            assets: Vec::new(),
            analyses: Vec::new(),
            findings: Vec::new(),
            evidence: Vec::new(),
            reports: Vec::new(),
        }
    }

    /// Adds an asset, ignoring one that is already present.
    ///
    /// Returns `true` when the asset was added. Because asset IDs are derived
    /// from content (spec §10), importing the same file twice is detected here
    /// rather than producing a silently duplicated case.
    pub fn add_asset(&mut self, asset: &MediaAsset) -> bool {
        if self.assets.contains(&asset.id) {
            return false;
        }
        self.assets.push(asset.id);
        true
    }

    /// Records a finding produced by an analysis.
    pub fn add_finding(&mut self, id: FindingId) {
        if !self.findings.contains(&id) {
            self.findings.push(id);
        }
    }

    /// Records an analysis run.
    pub fn add_analysis(&mut self, id: AnalysisId) {
        if !self.analyses.contains(&id) {
            self.analyses.push(id);
        }
    }

    /// Records a retained evidence artefact.
    pub fn add_evidence(&mut self, id: EvidenceId) {
        if !self.evidence.contains(&id) {
            self.evidence.push(id);
        }
    }

    /// Records a generated report.
    pub fn add_report(&mut self, id: ReportId) {
        if !self.reports.contains(&id) {
            self.reports.push(id);
        }
    }

    /// Returns `true` when this case holds more than one asset.
    ///
    /// Comparison between assets is one of the most valuable workflows
    /// (spec §67), so the UI uses this to decide whether to show it.
    #[must_use]
    pub fn supports_comparison(&self) -> bool {
        self.assets.len() > 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset::{AcquisitionRecord, HashSet, MediaType};

    fn asset(name: &str, size: u64) -> MediaAsset {
        MediaAsset::new(
            name,
            MediaType::Container,
            AcquisitionRecord {
                source_path: name.to_owned(),
                size_bytes: size,
                hashes: HashSet::default(),
                timestamps: Default::default(),
                filesystem: Default::default(),
            },
        )
    }

    #[test]
    fn case_ids_are_reproducible_from_the_name() {
        let a = Case::new("Operation Alpha", None);
        let b = Case::new("Operation Alpha", None);
        assert_eq!(a.id, b.id);
    }

    #[test]
    fn differently_named_cases_get_different_ids() {
        assert_ne!(Case::new("Alpha", None).id, Case::new("Beta", None).id);
    }

    #[test]
    fn new_case_starts_empty() {
        let c = Case::new("Empty", None);
        assert!(c.assets.is_empty());
        assert!(c.findings.is_empty());
        assert!(!c.supports_comparison());
    }

    #[test]
    fn duplicate_assets_are_not_added_twice() {
        let mut c = Case::new("Dupes", None);
        let a = asset("same.mp4", 10);
        assert!(c.add_asset(&a));
        assert!(!c.add_asset(&a), "second import must be a no-op");
        assert_eq!(c.assets.len(), 1);
    }

    #[test]
    fn distinct_assets_are_all_added() {
        let mut c = Case::new("Related", None);
        c.add_asset(&asset("original.mp4", 10));
        c.add_asset(&asset("edited.mp4", 20));
        assert_eq!(c.assets.len(), 2);
        assert!(c.supports_comparison());
    }

    #[test]
    fn related_entities_are_idempotent() {
        let mut c = Case::new("Idempotent", None);
        let f = FindingId::new_derived(&["r", "a"]);
        c.add_finding(f);
        c.add_finding(f);
        assert_eq!(c.findings.len(), 1);
    }

    #[test]
    fn case_round_trips_through_json() {
        // The manifest format is JSON-based (spec §58), so this must not drift.
        let mut c = Case::new("Round Trip", Some("desc".to_owned()));
        c.add_asset(&asset("a.mp4", 1));
        let json = serde_json::to_string(&c).expect("serialises");
        let back: Case = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(c, back);
    }
}
