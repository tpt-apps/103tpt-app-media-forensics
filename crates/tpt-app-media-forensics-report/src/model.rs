//! Report structure and the required disclaimer (spec §59, §60).
//!
//! # The disclaimer is not decoration
//!
//! Spec §59 requires, verbatim: *"Findings are technical observations and
//! should not be interpreted as proof of intent, authorship, manipulation, or
//! authenticity unless independently established. This protects the product
//! from overclaiming."*
//!
//! So the disclaimer is a constant on the report type rather than a template
//! variable. A renderer cannot omit it, and a caller cannot supply a weaker
//! one, because [`DISCLAIMER`] is not a `&str` parameter.
//!
//! # Everything needed to reproduce the analysis travels with the report
//!
//! Spec §60 lists what every report must state: software version, analysis
//! engine version, enabled rules, profile, input hash, analysis timestamp,
//! and applicable standards. Spec §63 adds the analysis fingerprint. All of it
//! lives in [`Methodology`], which has no `Default` — a report cannot be
//! constructed without them.

use serde::{Deserialize, Serialize};
use tpt_app_media_forensics_model::{AssetId, Evidence, Finding};

/// The disclaimer required on every report (spec §59).
pub const DISCLAIMER: &str = "\
This report records technical observations about the structure, timing, encoding, \
metadata, and integrity of the media files examined.

Findings are technical observations and must not be interpreted as proof of \
intent, authorship, manipulation, or authenticity unless independently \
established.

No finding in this report, individually or collectively, constitutes proof of \
manipulation or authenticity. A qualified reviewer must assess each observation \
in context before drawing any conclusion from it.";

/// Everything needed to reproduce an analysis (spec §60, §63).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Methodology {
    /// Version of the application that produced the report.
    pub application_version: String,
    /// Version of the analysis engine's behaviour.
    pub analysis_version: String,
    /// Identifier of the rule profile, e.g. `default-forensic v1`.
    pub profile: String,
    /// Fingerprint of the profile's thresholds.
    pub profile_fingerprint: String,
    /// The rules that ran, sorted.
    pub enabled_rules: Vec<String>,
    /// Fingerprint of the rule set.
    pub rule_set_fingerprint: String,
    /// SHA-256 of each analysed asset.
    pub input_hashes: Vec<(String, String)>,
    /// When the analysis ran, in Unix seconds.
    pub analysis_timestamp_unix: i64,
    /// Standards or methodologies the measurements follow.
    pub applicable_standards: Vec<String>,
    /// The combined fingerprint that identifies this analysis uniquely.
    pub analysis_fingerprint: String,
}

impl Methodology {
    /// Computes the combined analysis fingerprint (spec §63).
    ///
    /// Derived from the asset hash, analysis version, profile fingerprint, and
    /// rule-set fingerprint — the same four inputs that key the cache (§54), so
    /// a report's fingerprint and its cache validity cannot disagree.
    #[must_use]
    pub fn compute_fingerprint(
        input_hash: &str,
        analysis_version: &str,
        profile_fingerprint: &str,
        rule_set_fingerprint: &str,
    ) -> String {
        let joined =
            format!("{input_hash}:{analysis_version}:{profile_fingerprint}:{rule_set_fingerprint}");
        let digest = tpt_app_media_forensics_model::EntityId::new_derived(
            tpt_app_media_forensics_model::EntityKind::Report,
            &[joined.as_bytes()],
        );
        tpt_app_media_forensics_model::asset::to_hex(&digest.value().to_be_bytes())
    }
}

/// The outcome of a delivery validation run (spec §68).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ValidationResult {
    /// Every requirement met.
    Pass,
    /// Requirements met, with findings that do not block delivery.
    PassWithWarnings,
    /// At least one requirement not met.
    Fail,
}

impl ValidationResult {
    /// Returns the label printed in a validation report.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::PassWithWarnings => "PASS WITH WARNINGS",
            Self::Fail => "FAIL",
        }
    }

    /// Derives the result from the findings' severities (spec §68).
    ///
    /// `Critical` and `Significant` fail delivery; `Warning` and `Info` do not.
    #[must_use]
    pub fn from_findings(findings: &[Finding]) -> Self {
        let has_blocking = findings.iter().any(|f| {
            matches!(
                f.severity,
                tpt_app_media_forensics_model::Severity::Critical
                    | tpt_app_media_forensics_model::Severity::Significant
            )
        });
        let has_warnings = findings
            .iter()
            .any(|f| f.severity == tpt_app_media_forensics_model::Severity::Warning);

        match (has_blocking, has_warnings) {
            (true, _) => Self::Fail,
            (false, true) => Self::PassWithWarnings,
            (false, false) => Self::Pass,
        }
    }
}

/// One analysed asset as recorded in a report (spec §59).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetSummary {
    /// Display name of the file.
    pub name: String,
    /// Path recorded at acquisition.
    pub source_path: String,
    /// SHA-256 recorded at acquisition.
    pub sha256: Option<String>,
    /// BLAKE3 recorded at acquisition.
    pub blake3: Option<String>,
    /// Size in bytes.
    pub size_bytes: u64,
    /// Streams found in the container.
    pub stream_count: usize,
}

/// A complete forensic report (spec §59).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    /// Schema version of the report format itself.
    pub schema_version: u32,
    /// Case name.
    pub case_name: String,
    /// Case identifier.
    pub case_id: String,
    /// Optional case description.
    pub case_description: Option<String>,
    /// The assets examined.
    pub assets: Vec<AssetSummary>,
    /// Findings, ordered most severe first.
    pub findings: Vec<Finding>,
    /// Evidence artefacts referenced by those findings.
    pub evidence: Vec<Evidence>,
    /// Everything needed to reproduce the analysis.
    pub methodology: Methodology,
    /// Stated limitations, e.g. measurements that could not be taken.
    pub limitations: Vec<String>,
    /// Present for a validation report (spec §68), absent for a forensic one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation: Option<ValidationResult>,
}

impl Report {
    /// Counts findings by severity, for the dashboard and report summary.
    ///
    /// Returned as a fixed-size array in the order
    /// `[Critical, Significant, Warning, Info]` so renderers cannot reorder it.
    #[must_use]
    pub fn severity_counts(&self) -> [u32; 4] {
        use tpt_app_media_forensics_model::Severity;
        let mut counts = [0u32; 4];
        for finding in &self.findings {
            let index = match finding.severity {
                Severity::Critical => 0,
                Severity::Significant => 1,
                Severity::Warning => 2,
                Severity::Info => 3,
            };
            counts[index] = counts[index].saturating_add(1);
        }
        counts
    }

    /// Returns the total number of findings.
    #[must_use]
    pub fn finding_count(&self) -> usize {
        self.findings.len()
    }

    /// Returns evidence referenced by at least one finding.
    #[must_use]
    pub fn referenced_evidence(&self) -> Vec<&Evidence> {
        self.evidence
            .iter()
            .filter(|e| self.findings.iter().any(|f| f.evidence.contains(&e.id)))
            .collect()
    }
}

/// Builds the standard limitations list from what could not be measured.
///
/// Spec §60 requires that an approximate measurement be stated as such. Rather
/// than letting each caller invent wording, limitations are derived from the
/// conditions the engine actually encountered.
#[must_use]
pub fn standard_limitations(
    unmeasured_loudness: bool,
    undecodable_streams: bool,
    decoded_samples: Option<usize>,
) -> Vec<String> {
    let mut out = Vec::new();
    if unmeasured_loudness {
        out.push(
            "Integrated loudness was not measured: ITU-R BS.1770-4 defines K-weighting \
             only for 48 kHz, and no figure is reported for other rates."
                .to_owned(),
        );
    }
    if undecodable_streams {
        out.push(
            "Some streams could not be decoded, so measurements that depend on \
             decoded samples were not taken for them. Structural observations \
             remain valid."
                .to_owned(),
        );
    }
    match decoded_samples {
        None => out.push("No audio samples were decoded.".to_owned()),
        Some(count) => {
            out.push(format!(
                "Audio measurements cover {count} decoded samples; sampling \
                 methodology is recorded alongside each measurement."
            ));
        }
    }
    out
}

/// Convenience for building an asset summary from model types.
#[must_use]
pub fn asset_summary(
    name: impl Into<String>,
    source_path: impl Into<String>,
    sha256: Option<String>,
    blake3: Option<String>,
    size_bytes: u64,
    stream_count: usize,
) -> AssetSummary {
    AssetSummary {
        name: name.into(),
        source_path: source_path.into(),
        sha256,
        blake3,
        size_bytes,
        stream_count,
    }
}

/// The identifier of an asset, for reporting.
#[must_use]
pub fn asset_id_string(id: AssetId) -> String {
    id.to_string()
}
