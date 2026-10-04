//! Shared test fixtures for the view-model tests.
//!
//! # These builders exist because the alternative was worse
//!
//! Every screen test needs a `Finding`, and the real type has nine fields of
//! which one — `rationale` — is `Option` precisely so a test can leave it out.
//! Copy-pasting a nine-field literal into a dozen test modules produced the
//! risk this repository has already been bitten by three times (see
//! `todo.md`): a fixture quietly missing a field, so the test asserted against
//! a finding the engine could never produce.
//!
//! One builder means a change to `Finding` breaks compilation in exactly one
//! place, and every screen test is working from a finding that is structurally
//! identical to a real one.

#![cfg(test)]

use tpt_app_media_forensics_model::{
    AnalysisStatus, AssetId, Confidence, Evidence, EvidenceIntegrity, EvidenceKind, Finding,
    FindingId, FindingStatus, MediaTime, Observation, Provenance, RuleRationale, Severity,
    StreamId,
};

/// A finding at a given severity and review status.
pub fn finding(severity: Severity, status: FindingStatus) -> Finding {
    Finding {
        id: FindingId::new_derived(&[severity.tag(), status.tag()]),
        rule_id: "TEST.RULE".to_owned(),
        severity,
        confidence: Confidence::High,
        observation: Observation {
            summary: "observed".to_owned(),
            measurements: Vec::new(),
        },
        rationale: Some(RuleRationale::default()),
        asset_id: AssetId::new_derived(&["asset"]),
        stream_id: None::<StreamId>,
        timeline_start: None,
        timeline_end: None,
        evidence: Vec::new(),
        frame_index: None,
        status,
        review_note: None,
    }
}

/// A finding that also sits at a position on the timeline.
pub fn finding_at(severity: Severity, start_ms: i64, end_ms: Option<i64>) -> Finding {
    let mut f = finding(severity, FindingStatus::New);
    f.id = FindingId::new_derived(&[severity.tag(), &start_ms.to_string()]);
    f.timeline_start = Some(MediaTime::from_millis(start_ms));
    f.timeline_end = end_ms.map(MediaTime::from_millis);
    f
}

/// A positioned finding carrying one evidence artefact.
///
/// Positioned as well as carrying evidence: a jump is only meaningful for a
/// finding that has somewhere to jump *to*, and a fixture that omitted the
/// position would test the unplaced path while appearing to test the other.
pub fn finding_with_evidence() -> Finding {
    let mut f = finding_at(Severity::Significant, 4_000, None);
    f.evidence = vec![evidence().id];
    f
}

/// A stored evidence artefact with complete hashes.
pub fn evidence() -> Evidence {
    Evidence::new(
        AssetId::new_derived(&["asset"]),
        EvidenceKind::ExtractedFrame,
        Provenance::LosslessExtract,
        "evidence/frames/frame-00000001.png",
        EvidenceIntegrity {
            size_bytes: 3,
            hashes: tpt_app_media_forensics_model::asset::HashSet::new([
                tpt_app_media_forensics_model::asset::FileHash::from_bytes(
                    tpt_app_media_forensics_model::asset::HashAlgorithm::Sha256,
                    &[1u8; 32],
                ),
                tpt_app_media_forensics_model::asset::FileHash::from_bytes(
                    tpt_app_media_forensics_model::asset::HashAlgorithm::Blake3,
                    &[2u8; 32],
                ),
            ]),
            verified: true,
        },
    )
}

/// A completed analysis status, the common case in tests.
#[allow(dead_code, reason = "used by the viewer and comparison fixtures")]
pub const COMPLETE: AnalysisStatus = AnalysisStatus::Complete;
