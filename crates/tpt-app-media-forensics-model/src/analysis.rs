//! Analysis record and reproducibility fingerprints (spec §54, §63, §77).
//!
//! # Reproducibility is a first-class requirement
//!
//! "Analysis is reproducible from recorded profile + software version" is an
//! explicit acceptance criterion (spec §96). That means a report must be able
//! to state exactly which engine build, rule set, and profile produced it, and
//! a re-run must reproduce the result.
//!
//! The [`AnalysisVersion`] / [`ProfileFingerprint`] / [`RuleSetFingerprint`]
//! triple below feeds the cache key (spec §54) and is printed on every report.
//! Any change to analysis behaviour must bump [`AnalysisVersion`] or the cache
//! would serve stale results — this is the single most important correctness
//! invariant in the engine.

use serde::{Deserialize, Serialize};

use crate::id::{AnalysisId, AssetId, EntityId, EntityKind};

/// Derives a fingerprint string from a namespaced input.
fn fingerprint_of(kind: EntityKind, input: &str) -> String {
    crate::asset::to_hex(
        &EntityId::new_derived(kind, &[input.as_bytes()])
            .value()
            .to_be_bytes(),
    )
}

/// The engine's analysis-behaviour version.
///
/// Bump this whenever a change could alter an analysis result, including
/// changes to thresholds, heuristics, or default tolerances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AnalysisVersion(u32);

impl AnalysisVersion {
    /// The current analysis behaviour version.
    pub const CURRENT: Self = Self(2);

    /// Builds a version from a raw number.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the raw version number.
    #[must_use]
    pub const fn value(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for AnalysisVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A digest identifying the exact profile configuration used (spec §37, §70).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileFingerprint(String);

impl ProfileFingerprint {
    /// Builds a fingerprint from a profile's stable serialised form.
    #[must_use]
    pub fn from_serialized(profile: &str) -> Self {
        Self(fingerprint_of(EntityKind::Report, profile))
    }

    /// Returns the fingerprint's hexadecimal representation.
    #[must_use]
    pub fn as_hex(&self) -> &str {
        &self.0
    }
}

/// A digest identifying the exact set of rules that ran (spec §35, §37).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuleSetFingerprint(String);

impl RuleSetFingerprint {
    /// Builds a fingerprint from the rule IDs that were enabled.
    ///
    /// The IDs are sorted internally so the fingerprint does not depend on
    /// registration order, which must not vary between runs (spec §77).
    #[must_use]
    pub fn from_rule_ids(rule_ids: &[String]) -> Self {
        let mut sorted = rule_ids.to_vec();
        sorted.sort();
        Self(fingerprint_of(EntityKind::Case, &sorted.join("\n")))
    }

    /// Returns the fingerprint's hexadecimal representation.
    #[must_use]
    pub fn as_hex(&self) -> &str {
        &self.0
    }
}

/// The combined cache key for an analysis (spec §54).
///
/// An asset's cached result is valid only when the asset content, the analysis
/// version, the profile, and the rule set are all unchanged. Any one of these
/// differing invalidates the cache.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CacheKey {
    /// SHA-256 of the asset's content, from the acquisition record.
    pub asset_sha256: String,
    /// The analysis behaviour version.
    pub analysis_version: AnalysisVersion,
    /// The profile fingerprint.
    pub profile: ProfileFingerprint,
    /// The rule-set fingerprint.
    pub rules: RuleSetFingerprint,
}

impl CacheKey {
    /// Returns a single-line key suitable for a filename or a database column.
    ///
    /// Uses `:` as a separator because it cannot occur in a hex digest, so the
    /// components cannot be confused with one another.
    #[must_use]
    pub fn to_key_string(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.analysis_version,
            self.asset_sha256,
            self.profile.as_hex(),
            self.rules.as_hex()
        )
    }
}

/// How an analysis run terminated (spec §56, §80).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AnalysisStatus {
    /// The run has not started.
    #[default]
    Pending,
    /// The run is in progress.
    Running,
    /// The run completed; findings may still require review.
    Complete,
    /// The run failed partway; any partial results are retained.
    Failed,
    /// The analyst cancelled the run.
    Cancelled,
}

impl AnalysisStatus {
    /// Returns the stable uppercase tag shown in the dashboard (spec §80).
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Running => "RUNNING",
            Self::Complete => "COMPLETE",
            Self::Failed => "FAILED",
            Self::Cancelled => "CANCELLED",
        }
    }

    /// Returns `true` when the run finished and will not change further.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Failed | Self::Cancelled)
    }
}

/// A record of one run of the analysis engine (spec §63).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisRecord {
    /// This run's identifier.
    pub id: AnalysisId,
    /// The asset that was analysed.
    pub asset_id: AssetId,
    /// The cache key this run used (spec §54).
    pub cache_key: CacheKey,
    /// The software version string of the engine that produced the result.
    pub software_version: String,
    /// How the run terminated.
    pub status: AnalysisStatus,
    /// Number of findings produced by this run.
    pub finding_count: u32,
    /// Number of rules evaluated, for coverage reporting.
    pub rule_count: u32,
}

impl AnalysisRecord {
    /// Builds a completed analysis record.
    #[must_use]
    pub fn new(
        asset_id: AssetId,
        cache_key: CacheKey,
        software_version: impl Into<String>,
        finding_count: u32,
        rule_count: u32,
    ) -> Self {
        let id = AnalysisId::new_derived(&[
            asset_id.raw().to_string().as_str(),
            cache_key.to_key_string().as_str(),
        ]);
        Self {
            id,
            asset_id,
            cache_key,
            software_version: software_version.into(),
            status: AnalysisStatus::Complete,
            finding_count,
            rule_count,
        }
    }

    /// Marks the run as failed, retaining any partial result counts.
    pub fn mark_failed(&mut self) {
        self.status = AnalysisStatus::Failed;
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn cache_key(asset_sha256: &str, version: u32, rules: &[&str]) -> CacheKey {
        let ids: Vec<String> = rules.iter().map(|r| (*r).to_owned()).collect();
        CacheKey {
            asset_sha256: asset_sha256.to_owned(),
            analysis_version: AnalysisVersion::new(version),
            profile: ProfileFingerprint::from_serialized("default"),
            rules: RuleSetFingerprint::from_rule_ids(&ids),
        }
    }

    #[test]
    fn cache_key_changes_with_every_component() {
        let base = cache_key("aa", 1, &["A"]);
        assert_ne!(base, cache_key("bb", 1, &["A"]), "asset content");
        assert_ne!(base, cache_key("aa", 2, &["A"]), "analysis version");
        assert_ne!(base, cache_key("aa", 1, &["A", "B"]), "rule set");
    }

    #[test]
    fn bumping_analysis_version_invalidates_the_cache() {
        // The invariant that keeps stale results out of reports (spec §54).
        let v1 = cache_key("aa", AnalysisVersion::CURRENT.value(), &["A"]);
        let v2 = cache_key("aa", AnalysisVersion::CURRENT.value() + 1, &["A"]);
        assert_ne!(v1.to_key_string(), v2.to_key_string());
    }

    #[test]
    fn rule_set_fingerprint_ignores_registration_order() {
        let a = vec!["VIDEO.A".to_owned(), "AUDIO.B".to_owned()];
        let b = vec!["AUDIO.B".to_owned(), "VIDEO.A".to_owned()];
        assert_eq!(
            RuleSetFingerprint::from_rule_ids(&a),
            RuleSetFingerprint::from_rule_ids(&b),
            "rule order must not affect the fingerprint (spec §77)"
        );
    }

    #[test]
    fn rule_set_fingerprint_distinguishes_different_sets() {
        assert_ne!(
            RuleSetFingerprint::from_rule_ids(&["A".to_owned()]),
            RuleSetFingerprint::from_rule_ids(&["B".to_owned()])
        );
    }

    #[test]
    fn profile_fingerprint_distinguishes_configurations() {
        assert_ne!(
            ProfileFingerprint::from_serialized("client-x-v3"),
            ProfileFingerprint::from_serialized("client-x-v4"),
            "profiles must be versioned, never silently changed (spec §70)"
        );
    }

    #[test]
    fn key_string_components_cannot_be_confused() {
        // ':' cannot appear in a hex digest, so splitting on it is unambiguous.
        assert_eq!(
            cache_key("deadbeef", 1, &["A"])
                .to_key_string()
                .split(':')
                .count(),
            4
        );
    }

    #[test]
    fn analysis_ids_are_derived_from_asset_and_key() {
        let make = || {
            AnalysisRecord::new(
                AssetId::new_derived(&["x"]),
                cache_key("aa", 1, &["A"]),
                "1.0.0",
                3,
                20,
            )
        };
        assert_eq!(make().id, make().id, "re-running must not fork the record");
    }

    #[test]
    fn terminal_states_are_exhaustive() {
        assert!(AnalysisStatus::Complete.is_terminal());
        assert!(AnalysisStatus::Failed.is_terminal());
        assert!(AnalysisStatus::Cancelled.is_terminal());
        assert!(!AnalysisStatus::Running.is_terminal());
        assert!(!AnalysisStatus::Pending.is_terminal());
    }

    #[test]
    fn status_tags_match_dashboard_labels() {
        assert_eq!(AnalysisStatus::Complete.tag(), "COMPLETE");
        assert_eq!(AnalysisStatus::Cancelled.tag(), "CANCELLED");
    }

    #[test]
    fn failed_analysis_retains_partial_counts() {
        let mut record = AnalysisRecord::new(
            AssetId::new_derived(&["x"]),
            cache_key("aa", 1, &["A"]),
            "1.0.0",
            5,
            20,
        );
        record.mark_failed();
        assert_eq!(record.status, AnalysisStatus::Failed);
        assert_eq!(record.finding_count, 5);
    }
}
