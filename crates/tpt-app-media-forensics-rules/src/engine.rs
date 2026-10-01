//! The forensic rule trait and engine (spec §35, §36).
//!
//! # Rules are pure functions
//!
//! A rule reads analysis results and returns findings. It never opens a file,
//! invokes a decoder, or writes anything. That constraint is what makes rules
//! testable against synthetic results with no media involved, and it is why
//! every rule takes an [`AnalysisBundle`] rather than a path.
//!
//! # A rule must explain itself
//!
//! Spec §71 requires every finding to explain what was checked, why it
//! matters, and what the observation does not establish. `what_it_checks` and
//! `why_it_matters` are therefore trait methods, not documentation comments: a
//! rule that cannot state why its condition matters should not exist.
//!
//! # Deterministic ordering
//!
//! The engine evaluates rules in sorted rule-ID order, and findings are sorted
//! by (severity, rule ID, timeline position). Two runs over the same input
//! therefore produce identical output (spec §77) regardless of registration
//! order.

use tpt_app_media_forensics_model::{AssetId, Finding, FindingId};

use crate::profile::RuleProfile;

/// Builds an empty bundle for a given asset.
///
/// Present because `AnalysisBundle` cannot derive `Default`: a bundle with no
/// asset is meaningless, and silently defaulting the asset would attach
/// findings to the wrong evidence.
#[must_use]
pub fn empty_bundle(asset_id: AssetId) -> AnalysisBundle {
    AnalysisBundle {
        asset_id,
        container: None,
        gop: None,
        repeated_runs: Vec::new(),
        timestamps: Vec::new(),
        sync: None,
        metadata: None,
        audio_levels: None,
        silence: Vec::new(),
        loudness: None,
    }
}

/// Everything a rule may look at.
///
/// Rules see analysis results only. Nothing here holds a file handle, so a
/// rule can never modify a source file even by accident (spec §11).
#[derive(Debug, Clone)]
pub struct AnalysisBundle {
    /// The asset under examination.
    pub asset_id: AssetId,
    /// Container structure, when the container could be read.
    pub container: Option<tpt_app_media_forensics_container::Mp4Inspection>,
    /// GOP structure for the first video stream, when measured.
    pub gop: Option<tpt_app_media_forensics_video::gop::GopReport>,
    /// Repeated-sample runs across the file.
    pub repeated_runs: Vec<tpt_app_media_forensics_video::duplicate::RepeatedRun>,
    /// Presentation-timestamp anomalies for each stream, parallel to
    /// `container.streams`.
    pub timestamps: Vec<tpt_app_media_forensics_timing::pts_dts::TimestampReport>,
    /// A/V synchronisation, when both streams were measurable.
    pub sync: Option<tpt_app_media_forensics_timing::av_sync::SyncReport>,
    /// Extracted metadata tree.
    pub metadata: Option<tpt_app_media_forensics_metadata::MetadataTree>,
    /// Sample levels, when audio was decoded.
    pub audio_levels: Option<tpt_app_media_forensics_audio::LevelStats>,
    /// Silence regions, when audio was decoded.
    pub silence: Vec<tpt_app_media_forensics_audio::SilenceRegion>,
    /// Integrated loudness, when it could be measured correctly.
    pub loudness: Option<tpt_app_media_forensics_audio::Measurement>,
}

/// A single forensic rule.
pub trait ForensicRule: Send + Sync {
    /// Stable identifier, `DOMAIN.TECHNIQUE`, e.g. `VIDEO.GOP_LENGTH_CHANGE`.
    ///
    /// Part of the report contract: it appears in reports, CSV exports, and
    /// profile configuration, and must stay stable across releases.
    fn id(&self) -> &'static str;

    /// What this rule checks, in plain language.
    fn what_it_checks(&self) -> &'static str;

    /// Why the condition matters.
    fn why_it_matters(&self) -> &'static str;

    /// Evaluates the rule, returning findings.
    ///
    /// Returning an empty `Vec` means the condition was not observed; that is
    /// the common case and is not an error.
    fn evaluate(&self, bundle: &AnalysisBundle, profile: &RuleProfile) -> Vec<Finding>;
}

/// Evaluates a set of rules over one bundle.
pub struct RuleEngine {
    rules: Vec<Box<dyn ForensicRule>>,
}

impl RuleEngine {
    /// Builds an engine from the given rules.
    #[must_use]
    pub fn new(mut rules: Vec<Box<dyn ForensicRule>>) -> Self {
        // Sorted by ID so evaluation order never depends on registration order.
        rules.sort_by_key(|r| r.id());
        Self { rules }
    }

    /// Returns the rule IDs that will run, in evaluation order.
    #[must_use]
    pub fn rule_ids(&self) -> Vec<&'static str> {
        self.rules.iter().map(|r| r.id()).collect()
    }

    /// Returns the number of rules registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Returns `true` when no rules are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Runs every rule and returns the findings in a deterministic order.
    ///
    /// # Errors
    ///
    /// Currently infallible. Rules analyse data, not I/O, so there is nothing
    /// here that can fail; the signature exists so that a rule needing external
    /// input can be added without changing every caller.
    pub fn evaluate(
        &self,
        bundle: &AnalysisBundle,
        profile: &RuleProfile,
    ) -> Result<Vec<Finding>, RuleError> {
        let mut findings: Vec<Finding> = self
            .rules
            .iter()
            .flat_map(|rule| rule.evaluate(bundle, profile))
            .collect();

        findings.sort_by(|a, b| {
            // Most severe first (Severity orders Critical as greatest), then
            // rule ID, then timeline position.
            b.severity
                .cmp(&a.severity)
                .then_with(|| a.rule_id.cmp(&b.rule_id))
                .then_with(|| a.timeline_range().0.cmp(&b.timeline_range().0))
                .then_with(|| a.id.cmp(&b.id))
        });

        // Identifiers are content-derived from rule ID plus location, so two
        // runs over the same input produce identical IDs (spec §77).
        Ok(findings)
    }

    /// Builds a stable fingerprint of the enabled rule set (spec §54).
    #[must_use]
    pub fn fingerprint(&self) -> tpt_app_media_forensics_model::RuleSetFingerprint {
        let ids: Vec<String> = self.rule_ids().iter().map(|id| (*id).to_owned()).collect();
        tpt_app_media_forensics_model::RuleSetFingerprint::from_rule_ids(&ids)
    }
}

/// An error raised while evaluating rules.
#[derive(Debug, thiserror::Error)]
pub enum RuleError {
    /// A rule failed to evaluate.
    #[error("rule {rule_id} failed: {reason}")]
    RuleFailed {
        /// The rule that failed.
        rule_id: String,
        /// What went wrong.
        reason: String,
    },
}

/// Builds a content-derived finding identifier.
///
/// Deriving the ID from the rule and location rather than a counter means
/// re-analysing a file yields the same finding IDs, so a re-run can be compared
/// against the previous result without a renumbering step.
#[must_use]
pub fn finding_id(rule_id: &str, asset: &AssetId, locator: &str) -> FindingId {
    FindingId::new_derived(&[rule_id, &asset.to_string(), locator])
}
