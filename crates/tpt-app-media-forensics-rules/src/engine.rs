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

use std::collections::HashMap;

use tpt_app_media_forensics_model::{AssetId, Finding, FindingId, RuleRationale};

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
        scene: None,
        near_duplicates: None,
        damage: Vec::new(),
        sample_index: tpt_app_media_forensics_container::SampleIndex::default(),
        bitrate: None,
        packet_damage: Vec::new(),
        decode_damage: Vec::new(),
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
    pub container: Option<tpt_app_media_forensics_container::ContainerInspection>,
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
    /// Consecutive-frame differences, when Tier-2 decoding ran.
    pub scene: Option<tpt_app_media_forensics_video::scene::SceneReport>,
    /// Near-duplicate pairs, when Tier-2 decoding ran.
    pub near_duplicates: Option<tpt_app_media_forensics_video::near_duplicate::NearDuplicateReport>,
    /// Structural defects found by the container byte scan (spec §30).
    ///
    /// Always a `Vec`, never `Option`: an empty vector means the file was
    /// scanned and no structural damage was found, which is a measurement a
    /// report can rely on. `Option` would conflate "clean" with "not looked
    /// at", and those are exactly the two claims a forensic report must keep
    /// apart.
    pub damage: Vec<tpt_app_media_forensics_container::StructuralDamage>,
    /// Byte-offset to media-time lookup, used to place damage on a timeline
    /// (spec §31).
    ///
    /// Empty when samples were not read, which the caller cannot distinguish
    /// from "no damage to place" by looking at `damage` alone — so the timeline
    /// layer checks this rather than assuming a placement is available.
    pub sample_index: tpt_app_media_forensics_container::SampleIndex,
    /// Compression and bitrate analysis for video streams (spec §28-§29).
    ///
    /// `None` when samples were not read, or when fewer than two video samples
    /// were recoverable — a rate needs two points. That is different from an
    /// empty `anomalies` list, which means the file was measured and held steady.
    pub bitrate: Option<tpt_app_media_forensics_video::bitrate::BitrateReport>,
    /// Access units that are unusable, found without a decoder (spec §30).
    ///
    /// A `Vec` rather than `Option`, for the same reason as `damage`: empty means
    /// the packets were scanned and none was defective, which is a measurement a
    /// report can rely on. `Option` would conflate "clean" with "not looked at".
    pub packet_damage: Vec<tpt_app_media_forensics_container::PacketDamage>,
    /// Packets a decoder rejected or silently dropped (spec §30).
    ///
    /// Empty whenever Tier-2 did not run, which is the common case for the
    /// patent-encumbered codecs this engine deliberately never decodes. That is
    /// why the packet-layer check above exists: it covers those files, this one
    /// cannot.
    pub decode_damage: Vec<tpt_app_media_forensics_video::DecodeDamage>,
}

/// A piece of analysis a rule depends on.
///
/// Declared per rule so that a stage which was written but never called cannot
/// hide. Two such stages shipped: audio measurement and A/V synchronisation were
/// both fully implemented, documented, and unit-tested, and neither was ever
/// invoked by the pipeline — so six rules could not fire on any file, over any
/// number of green tests.
///
/// Declared rather than inferred, because Rust has no reflection and a field
/// that is read but not declared cannot be detected. That makes under-declaring
/// possible; it is a discipline cost paid once per rule, in exchange for
/// catching an entire class of silent failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BundleInput {
    /// Container structure and declared stream properties.
    Container,
    /// GOP structure and keyframe positions.
    Gop,
    /// Repeated compressed-sample runs.
    RepeatedRuns,
    /// Presentation-timestamp anomaly reports.
    Timestamps,
    /// Audio/video offset and drift.
    Sync,
    /// Decoded audio levels.
    AudioLevels,
    /// Detected silence regions.
    Silence,
    /// Integrated loudness.
    Loudness,
    /// Tier-2 scene-change analysis.
    Scene,
    /// Tier-2 perceptual near-duplicate detection.
    NearDuplicates,
    /// Extracted metadata tree.
    Metadata,
    /// Structural damage from the container byte scan.
    Damage,
    /// Byte-offset to media-time lookup, for the error timeline (spec §31).
    SampleIndex,
    /// Compression and bitrate analysis (spec §28-§29).
    Bitrate,
    /// Decoder-free access-unit defects (spec §30).
    PacketDamage,
    /// Decoder-reported packet damage (spec §30).
    DecodeDamage,
}

impl BundleInput {
    /// Every input a rule may declare, in a stable order.
    pub const ALL: &'static [BundleInput] = &[
        Self::Container,
        Self::Gop,
        Self::RepeatedRuns,
        Self::Timestamps,
        Self::Sync,
        Self::AudioLevels,
        Self::Silence,
        Self::Loudness,
        Self::Scene,
        Self::NearDuplicates,
        Self::Metadata,
        Self::Damage,
        Self::SampleIndex,
        Self::Bitrate,
        Self::PacketDamage,
        Self::DecodeDamage,
    ];

    /// The stable tag used in diagnostics and in the guard's messages.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Container => "container",
            Self::Gop => "gop",
            Self::RepeatedRuns => "repeated_runs",
            Self::Timestamps => "timestamps",
            Self::Sync => "sync",
            Self::AudioLevels => "audio_levels",
            Self::Silence => "silence",
            Self::Loudness => "loudness",
            Self::Scene => "scene",
            Self::NearDuplicates => "near_duplicates",
            Self::Metadata => "metadata",
            Self::Damage => "damage",
            Self::SampleIndex => "sample_index",
            Self::Bitrate => "bitrate",
            Self::PacketDamage => "packet_damage",
            Self::DecodeDamage => "decode_damage",
        }
    }

    /// Whether `bundle` carries a result for this input.
    ///
    /// "Populated" means the analyser ran and *found something*. An empty
    /// `Vec` therefore counts as **not** populated, because the guard's purpose
    /// is to prove a stage is reachable: a collection input nothing ever fills
    /// means no fixture exercises it, which is the condition being guarded
    /// against. (`Option` inputs answer the same question by `is_some`.)
    ///
    /// This is deliberately the opposite of "the analysis is complete" — an
    /// empty vector there means *scanned and clean*, which the bundle expresses
    /// by being present rather than by being non-empty.
    #[must_use]
    pub fn is_populated(self, bundle: &AnalysisBundle) -> bool {
        match self {
            Self::Container => bundle.container.is_some(),
            Self::Gop => bundle.gop.is_some(),
            Self::RepeatedRuns => !bundle.repeated_runs.is_empty(),
            Self::Timestamps => !bundle.timestamps.is_empty(),
            Self::Sync => bundle.sync.is_some(),
            Self::AudioLevels => bundle.audio_levels.is_some(),
            Self::Silence => !bundle.silence.is_empty(),
            Self::Loudness => bundle.loudness.is_some(),
            Self::Scene => bundle.scene.is_some(),
            Self::NearDuplicates => bundle.near_duplicates.is_some(),
            Self::Metadata => bundle.metadata.as_ref().is_some_and(|m| !m.is_empty()),
            Self::Damage => !bundle.damage.is_empty(),
            Self::SampleIndex => !bundle.sample_index.is_empty(),
            Self::Bitrate => bundle.bitrate.is_some(),
            Self::PacketDamage => !bundle.packet_damage.is_empty(),
            Self::DecodeDamage => !bundle.decode_damage.is_empty(),
        }
    }
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

    /// What a finding from this rule does *not* establish.
    ///
    /// Spec §71 asks every finding to state its own limitations, and a rule author
    /// is the only party who knows which apply. `VIDEO.GOP_LENGTH_CHANGE` can say
    /// precisely that a re-encode changes GOP length; a generic "this does not
    /// prove manipulation" would be true of every rule and so tells a reviewer
    /// nothing about this one.
    ///
    /// Defaults to [`RuleRationale::DEFAULT_LIMITATION`] rather than to nothing,
    /// because a missing caveat reads as *no* caveat — an absence a reader
    /// resolves in the finding's favour. Override it wherever the rule can say
    /// something a reviewer would act on differently.
    fn does_not_establish(&self) -> &'static str {
        RuleRationale::DEFAULT_LIMITATION
    }

    /// The analysis this rule reads, without which it cannot produce a finding.
    ///
    /// Mandatory, with no default. A rule that declares nothing it reads is
    /// indistinguishable from one that genuinely needs nothing, and the guard
    /// that catches unwired stages depends on the distinction being explicit.
    ///
    /// A rule with **no** required input is legitimate only if it can fire from
    /// the asset alone; `METADATA.DECLARED_VS_MEASURED_MISMATCH` is currently
    /// listed as an exception in the guard for the opposite reason.
    fn required_inputs(&self) -> &'static [BundleInput];

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
        let mut findings: Vec<Finding> = Vec::new();
        for rule in &self.rules {
            // Spec §71: every finding carries what its rule checks and why that
            // matters. Stamped here, while the rule is in hand, rather than looked
            // up at render time — a report is read away from this binary, and an
            // explanation that only existed inside the engine could not travel
            // with the finding.
            let rationale = RuleRationale {
                checks: rule.what_it_checks().to_owned(),
                why_it_matters: rule.why_it_matters().to_owned(),
                does_not_establish: rule.does_not_establish().to_owned(),
            };
            for mut finding in rule.evaluate(bundle, profile) {
                finding.rationale = Some(rationale.clone());
                findings.push(finding);
            }
        }

        findings.sort_by(|a, b| {
            // Most severe first (Severity orders Critical as greatest), then
            // rule ID, then timeline position.
            b.severity
                .cmp(&a.severity)
                .then_with(|| a.rule_id.cmp(&b.rule_id))
                .then_with(|| a.timeline_range().0.cmp(&b.timeline_range().0))
                .then_with(|| a.id.cmp(&b.id))
        });

        // Two findings from one rule at one position derive the same ID, and
        // `findings` has a primary key on `(analysis_id, id)` — so the second
        // insert aborts the entire analysis. Rather than requiring every rule to
        // remember to disambiguate, collisions are resolved here, in one place.
        //
        // Duplicates are suffixed by position in the sorted order, and their text
        // is mixed in so the ID still reflects what the finding says. The `#n`
        // component is what guarantees uniqueness: two findings from one rule at
        // one position are distinct rows and must stay separately addressable
        // even when they read identically.
        //
        // The findings are sorted before this loop and `seen` is only used to
        // count — never iterated — so re-running over the same input reproduces
        // the same IDs (spec §77).
        let mut seen: HashMap<FindingId, usize> = HashMap::new();
        for finding in &mut findings {
            let count = seen.entry(finding.id).or_insert(0);
            if *count > 0 {
                let text = format!(
                    "{}\u{1f}#{count}\u{1f}{}\u{1f}{}",
                    finding.timeline_range().0,
                    finding.observation.summary,
                    finding.observation.measurements.join("|"),
                );
                finding.id = finding_id(&finding.rule_id, &finding.asset_id, &text);
            }
            *count += 1;
        }

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
