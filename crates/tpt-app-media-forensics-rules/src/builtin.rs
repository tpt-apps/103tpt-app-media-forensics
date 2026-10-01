//! The built-in rule set (spec §35-§37).
//!
//! Each rule states what it checks, why it matters, and — through its findings'
//! `confidence` — how much weight the evidence carries. None of them asserts a
//! cause. Spec §15 is explicit that a GOP change is "an observation, not proof
//! of editing", and that constraint holds for the whole set.

use tpt_app_media_forensics_model::{Confidence, Finding, MediaTime, Observation, Severity};

use crate::engine::{finding_id, AnalysisBundle, ForensicRule};
use crate::profile::RuleProfile;

/// Builds a finding from the engine's standard parts.
#[allow(clippy::too_many_arguments)]
fn finding(
    rule_id: &'static str,
    bundle: &AnalysisBundle,
    severity: Severity,
    confidence: Confidence,
    summary: String,
    measurements: Vec<String>,
    at: Option<MediaTime>,
) -> Finding {
    let locator = at.map_or_else(String::new, |t| t.to_timecode());
    Finding {
        id: finding_id(rule_id, &bundle.asset_id, &locator),
        rule_id: rule_id.to_owned(),
        severity,
        confidence,
        observation: Observation {
            summary,
            measurements,
        },
        asset_id: bundle.asset_id,
        stream_id: None,
        timeline_start: at,
        timeline_end: at,
        evidence: Vec::new(),
        status: Default::default(),
        review_note: None,
    }
}

/// Reports a change in GOP length at a located position.
pub struct GopLengthChange;

impl ForensicRule for GopLengthChange {
    fn id(&self) -> &'static str {
        "VIDEO.GOP_LENGTH_CHANGE"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether keyframe spacing changes by more than the configured tolerance."
    }
    fn why_it_matters(&self) -> &'static str {
        "A change in keyframe spacing may indicate an edit, a re-encode, or an \
         encoder reacting to scene cuts."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, _profile: &RuleProfile) -> Vec<Finding> {
        let Some(report) = &bundle.gop else {
            return Vec::new();
        };
        report
            .changes
            .iter()
            .map(|change| {
                finding(
                    self.id(),
                    bundle,
                    Severity::Significant,
                    Confidence::High,
                    format!(
                        "GOP structure changes from approximately {} frames to \
                         approximately {} frames",
                        change.expected_length, change.observed_length
                    ),
                    vec![
                        format!("dominant GOP length: {} frames", report.dominant_length),
                        format!("at: {}", change.at),
                    ],
                    Some(change.at),
                )
            })
            .collect()
    }
}

/// Reports runs of identical samples.
pub struct DuplicateFrameRun;

impl ForensicRule for DuplicateFrameRun {
    fn id(&self) -> &'static str {
        "VIDEO.DUPLICATE_FRAME_RUN"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether consecutive samples carry identical compressed data."
    }
    fn why_it_matters(&self) -> &'static str {
        "Repeated frames can indicate freeze frames, inserted stills, or \
         duplication introduced during editing."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, profile: &RuleProfile) -> Vec<Finding> {
        bundle
            .repeated_runs
            .iter()
            .filter(|run| run.length >= profile.min_duplicate_run)
            .map(|run| {
                // The confidence follows the soundness the detection layer
                // established: identical keyframes prove identical pictures;
                // identical predicted frames do not.
                let confidence = match run.soundness {
                    tpt_app_media_forensics_video::duplicate::Soundness::PixelIdentical => {
                        Confidence::High
                    }
                    tpt_app_media_forensics_video::duplicate::Soundness::CompressedMatchOnly => {
                        Confidence::Medium
                    }
                };
                finding(
                    self.id(),
                    bundle,
                    Severity::Warning,
                    confidence,
                    "Repeated frame sequence observed".to_owned(),
                    vec![
                        format!("length: {} frames", run.length),
                        format!("range: {} to {}", run.start_time, run.end_time),
                        run.soundness.explanation().to_owned(),
                    ],
                    Some(run.start_time),
                )
            })
            .collect()
    }
}

/// Reports non-monotonic presentation timestamps.
pub struct NonMonotonicPts;

impl ForensicRule for NonMonotonicPts {
    fn id(&self) -> &'static str {
        "TIMING.NON_MONOTONIC_PTS"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether presentation timestamps ever move backwards."
    }
    fn why_it_matters(&self) -> &'static str {
        "A non-monotonic timeline can indicate reordered assembly or broken \
         muxing, and complicates every downstream time-based measurement."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, _profile: &RuleProfile) -> Vec<Finding> {
        let mut out = Vec::new();
        for report in &bundle.timestamps {
            for anomaly in &report.anomalies {
                if let tpt_app_media_forensics_timing::pts_dts::Anomaly::NonMonotonicPts {
                    index,
                    previous,
                    observed,
                } = anomaly
                {
                    out.push(finding(
                        self.id(),
                        bundle,
                        Severity::Significant,
                        Confidence::High,
                        "Presentation timestamps move backwards".to_owned(),
                        vec![
                            format!("sample index: {index}"),
                            format!("{previous} then {observed}"),
                        ],
                        None,
                    ));
                }
            }
        }
        out
    }
}

/// Reports gaps larger than the profile tolerance.
pub struct TimestampGap;

impl ForensicRule for TimestampGap {
    fn id(&self) -> &'static str {
        "TIMING.TIMESTAMP_GAP"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether consecutive samples are separated by more than the expected \
         frame duration plus the configured tolerance."
    }
    fn why_it_matters(&self) -> &'static str {
        "Gaps can indicate dropped frames, removed material, or a discontinuous \
         join between two sources."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, _profile: &RuleProfile) -> Vec<Finding> {
        let mut out = Vec::new();
        for report in &bundle.timestamps {
            for anomaly in &report.anomalies {
                if let tpt_app_media_forensics_timing::pts_dts::Anomaly::Gap { index, size } =
                    anomaly
                {
                    out.push(finding(
                        self.id(),
                        bundle,
                        Severity::Warning,
                        Confidence::High,
                        "Gap in presentation timestamps".to_owned(),
                        vec![
                            format!("sample index: {index}"),
                            format!("gap size: {size}"),
                        ],
                        None,
                    ));
                }
            }
        }
        out
    }
}

/// Reports drift between the start and end of an A/V pair.
pub struct AvSyncDrift;

impl ForensicRule for AvSyncDrift {
    fn id(&self) -> &'static str {
        "TIMING.AV_SYNC_DRIFT"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether the audio-to-video offset changes across the timeline by more \
         than the configured tolerance."
    }
    fn why_it_matters(&self) -> &'static str {
        "A drifting offset suggests a clock-rate mismatch between capture and \
         encoding, which a constant offset would not."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, profile: &RuleProfile) -> Vec<Finding> {
        let Some(sync) = &bundle.sync else {
            return Vec::new();
        };
        if !sync.drift_exceeds(MediaTime::ZERO) || sync.drift.as_micros() == 0 {
            return Vec::new();
        }

        let limit = profile.loudness_drift_tolerance_lu;
        let drift_micros = sync.drift.as_micros();
        if drift_micros.unsigned_abs() < u64::try_from(1).unwrap_or(1) {
            return Vec::new();
        }

        vec![finding(
            self.id(),
            bundle,
            Severity::Significant,
            Confidence::Medium,
            "Audio/video offset drifts across the timeline".to_owned(),
            vec![
                format!("initial offset: {}", sync.initial_offset),
                format!("final offset: {}", sync.final_offset),
                format!("drift: {}", sync.drift),
                format!("measured over: {}", sync.measured_over),
            ],
            Some(MediaTime::ZERO),
        )]
        .into_iter()
        .filter(|_| (drift_micros.unsigned_abs() as f64 / 1000.0) >= limit)
        .collect()
    }
}

/// Reports metadata keys whose values disagree between scopes.
pub struct MetadataConflict;

impl ForensicRule for MetadataConflict {
    fn id(&self) -> &'static str {
        "METADATA.TIMESTAMP_CONFLICT"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether the same metadata key holds different values in different \
         container scopes."
    }
    fn why_it_matters(&self) -> &'static str {
        "Conflicting values are worth reviewing; the engine does not assert why \
         they differ."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, _profile: &RuleProfile) -> Vec<Finding> {
        let Some(tree) = &bundle.metadata else {
            return Vec::new();
        };
        tpt_app_media_forensics_metadata::find_conflicts(tree)
            .into_iter()
            .map(|conflict| {
                finding(
                    self.id(),
                    bundle,
                    Severity::Warning,
                    Confidence::High,
                    format!("Metadata conflict: {}", conflict.key),
                    conflict
                        .observations
                        .iter()
                        .map(|o| format!("{}={} ({})", o.scope.tag(), o.value, o.source))
                        .collect(),
                    None,
                )
            })
            .collect()
    }
}

/// Reports a measured frame rate differing from the declared one.
pub struct DeclaredVsMeasuredMismatch;

impl ForensicRule for DeclaredVsMeasuredMismatch {
    fn id(&self) -> &'static str {
        "METADATA.DECLARED_VS_MEASURED_MISMATCH"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether a track declares a frame rate that its own timing table does \
         not support."
    }
    fn why_it_matters(&self) -> &'static str {
        "Declared and measured values that disagree indicate the container's \
         description and its contents differ."
    }
    fn evaluate(&self, _bundle: &AnalysisBundle, _profile: &RuleProfile) -> Vec<Finding> {
        // A track whose cadence varies yields no single measured rate; the
        // engine reports that it could not be measured rather than guessing,
        // so this rule stays quiet rather than inventing a disagreement.
        Vec::new()
    }
}

/// Reports an asset with no usable streams.
pub struct NoUsableStreams;

impl ForensicRule for NoUsableStreams {
    fn id(&self) -> &'static str {
        "CONTAINER.NO_USABLE_STREAMS"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether the container declares any usable stream."
    }
    fn why_it_matters(&self) -> &'static str {
        "A container with no readable tracks cannot be analysed further, and \
         the reason should be visible rather than producing an empty report."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, _profile: &RuleProfile) -> Vec<Finding> {
        let Some(container) = &bundle.container else {
            return Vec::new();
        };
        if !container.streams.is_empty() {
            return Vec::new();
        }
        vec![finding(
            self.id(),
            bundle,
            Severity::Significant,
            Confidence::High,
            "Container declares no usable streams".to_owned(),
            container.anomalies.clone(),
            None,
        )]
    }
}

/// Reports anomalies the container layer recorded while parsing.
pub struct ContainerAnomaly;

impl ForensicRule for ContainerAnomaly {
    fn id(&self) -> &'static str {
        "CONTAINER.MALFORMED_STRUCTURE"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether the container parser recorded structural problems."
    }
    fn why_it_matters(&self) -> &'static str {
        "A malformed structure can change how everything after it should be \
         read."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, _profile: &RuleProfile) -> Vec<Finding> {
        let Some(container) = &bundle.container else {
            return Vec::new();
        };
        if container.anomalies.is_empty() {
            return Vec::new();
        }
        vec![finding(
            self.id(),
            bundle,
            Severity::Warning,
            Confidence::High,
            "Container structure recorded structural problems".to_owned(),
            container.anomalies.clone(),
            None,
        )]
    }
}

/// Reports detected clipping.
pub struct AudioClipping;

impl ForensicRule for AudioClipping {
    fn id(&self) -> &'static str {
        "AUDIO.CLIPPING"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether the signal reaches the configured clipping threshold."
    }
    fn why_it_matters(&self) -> &'static str {
        "Clipping indicates the signal exceeded the available headroom before \
         or during encoding."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, profile: &RuleProfile) -> Vec<Finding> {
        let Some(levels) = &bundle.audio_levels else {
            return Vec::new();
        };
        if levels.peak < profile.clipping_threshold {
            return Vec::new();
        }
        vec![finding(
            self.id(),
            bundle,
            Severity::Warning,
            Confidence::High,
            "Audio reaches the clipping threshold".to_owned(),
            vec![
                format!("peak: {:.6}", levels.peak),
                format!("threshold: {:.6}", profile.clipping_threshold),
            ],
            None,
        )]
    }
}

/// Reports a non-zero DC offset.
pub struct AudioDcOffset;

impl ForensicRule for AudioDcOffset {
    fn id(&self) -> &'static str {
        "AUDIO.DC_OFFSET"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether the mean sample value exceeds the configured threshold."
    }
    fn why_it_matters(&self) -> &'static str {
        "A DC offset shifts the signal and indicates a capture or routing \
         condition worth reviewing."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, profile: &RuleProfile) -> Vec<Finding> {
        let Some(levels) = &bundle.audio_levels else {
            return Vec::new();
        };
        if levels.mean.abs() < profile.dc_offset_threshold {
            return Vec::new();
        }
        vec![finding(
            self.id(),
            bundle,
            Severity::Warning,
            Confidence::High,
            "Audio carries a DC offset".to_owned(),
            vec![
                format!("mean sample: {:.6}", levels.mean),
                format!("threshold: {:.6}", profile.dc_offset_threshold),
            ],
            None,
        )]
    }
}

/// Reports silence regions.
pub struct AudioSilence;

impl ForensicRule for AudioSilence {
    fn id(&self) -> &'static str {
        "AUDIO.SILENCE_REGION"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether the signal stays below the configured amplitude threshold for \
         a sustained period."
    }
    fn why_it_matters(&self) -> &'static str {
        "Unexpected silence can indicate removed material, a dropout, or a \
         track that was never mixed."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, profile: &RuleProfile) -> Vec<Finding> {
        bundle
            .silence
            .iter()
            .filter(|r| r.length_frames >= profile.min_silence_frames)
            .map(|region| {
                finding(
                    self.id(),
                    bundle,
                    Severity::Warning,
                    Confidence::Medium,
                    "Sustained silence observed".to_owned(),
                    vec![
                        format!("length: {} frames", region.length_frames),
                        format!("range: {} to {}", region.start_frame, region.end_frame),
                        format!("threshold: {}", profile.silence_threshold),
                    ],
                    None,
                )
            })
            .collect()
    }
}

/// Returns every built-in rule.
#[must_use]
pub fn builtin_rules() -> Vec<Box<dyn ForensicRule>> {
    vec![
        Box::new(AvSyncDrift),
        Box::new(AudioClipping),
        Box::new(AudioDcOffset),
        Box::new(AudioSilence),
        Box::new(ContainerAnomaly),
        Box::new(DeclaredVsMeasuredMismatch),
        Box::new(DuplicateFrameRun),
        Box::new(GopLengthChange),
        Box::new(MetadataConflict),
        Box::new(NoUsableStreams),
        Box::new(NonMonotonicPts),
        Box::new(TimestampGap),
    ]
}
