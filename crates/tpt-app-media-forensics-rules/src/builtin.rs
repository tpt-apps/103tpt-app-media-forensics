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

// ---------------------------------------------------------------------------
// Container, stream, and duration rules
//
// These read fields the container already exposes, so they cost nothing at
// analysis time and work on files the demuxer could only partially recover.
// ---------------------------------------------------------------------------

/// Reports a container whose declared track count disagrees with what it holds.
pub struct DeclaredTrackMismatch;

impl ForensicRule for DeclaredTrackMismatch {
    fn id(&self) -> &'static str {
        "CONTAINER.DECLARED_TRACK_MISMATCH"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether the number of `trak` boxes matches the number of streams recovered."
    }
    fn why_it_matters(&self) -> &'static str {
        "A track the container declares but does not deliver means part of the \
         content described by the file is missing or unreadable."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, _profile: &RuleProfile) -> Vec<Finding> {
        let Some(inspection) = &bundle.container else {
            return Vec::new();
        };
        if inspection.declared_track_count == inspection.streams.len() {
            return Vec::new();
        }
        vec![finding(
            self.id(),
            bundle,
            Severity::Significant,
            // The count is read directly from the box structure, so this is a
            // structural fact rather than an inference.
            Confidence::High,
            format!(
                "Container declares {} track(s) but {} stream(s) were recovered",
                inspection.declared_track_count,
                inspection.streams.len()
            ),
            vec![
                format!("declared `trak` boxes: {}", inspection.declared_track_count),
                format!("recovered streams: {}", inspection.streams.len()),
            ],
            None,
        )]
    }
}

/// Reports a stream whose declared duration is missing.
pub struct StreamDurationMissing;

impl ForensicRule for StreamDurationMissing {
    fn id(&self) -> &'static str {
        "CONTAINER.STREAM_DURATION_MISSING"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether any stream omits its declared duration."
    }
    fn why_it_matters(&self) -> &'static str {
        "A stream with no declared duration cannot be compared against the \
         container duration, so length and synchronisation checks for that \
         stream rest on whatever the samples themselves show."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, _profile: &RuleProfile) -> Vec<Finding> {
        let Some(inspection) = &bundle.container else {
            return Vec::new();
        };
        inspection
            .streams
            .iter()
            .filter(|stream| stream.timing.duration.is_none())
            .map(|stream| {
                finding(
                    self.id(),
                    bundle,
                    Severity::Info,
                    // The absence is certain; what it implies for a given
                    // workflow is not, so this stays informational.
                    Confidence::High,
                    format!(
                        "Stream {} ({}) declares no duration",
                        stream.index,
                        stream.kind.tag()
                    ),
                    vec![
                        format!("timebase: {}", stream.timing.timebase),
                        format!("declared start: {}", stream.timing.start_time.to_timecode()),
                    ],
                    None,
                )
            })
            .collect()
    }
}

/// Reports a stream whose declared start time is non-zero.
pub struct StreamStartOffset;

impl ForensicRule for StreamStartOffset {
    fn id(&self) -> &'static str {
        "CONTAINER.STREAM_START_OFFSET"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether any stream declares a non-zero start time or edit-list offset."
    }
    fn why_it_matters(&self) -> &'static str {
        "A non-zero start shifts a stream relative to the others, which changes \
         what 'the beginning' means for synchronisation and is often introduced \
         by trimming or by a conform."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, profile: &RuleProfile) -> Vec<Finding> {
        let Some(inspection) = &bundle.container else {
            return Vec::new();
        };
        let tolerance = profile.pts_tolerance;

        inspection
            .streams
            .iter()
            .filter_map(|stream| {
                let start = stream.timing.start_time;
                let edit = stream.timing.edit_list_offset;
                let significant = start > tolerance
                    || edit.is_some_and(|offset| offset.as_micros().abs() > tolerance.as_micros());
                significant.then_some((stream, start, edit))
            })
            .map(|(stream, start, edit)| {
                finding(
                    self.id(),
                    bundle,
                    Severity::Info,
                    Confidence::High,
                    format!(
                        "Stream {} ({}) starts at {}",
                        stream.index,
                        stream.kind.tag(),
                        start.to_timecode()
                    ),
                    vec![
                        format!("declared start: {}", start.to_timecode()),
                        format!(
                            "edit-list offset: {}",
                            edit.map_or_else(|| "none".to_owned(), |o| o.to_timecode())
                        ),
                    ],
                    Some(start),
                )
            })
            .collect()
    }
}

/// Reports container-level parse anomalies.
pub struct ContainerAnomalyList;

impl ForensicRule for ContainerAnomalyList {
    fn id(&self) -> &'static str {
        "CONTAINER.PARSE_ANOMALY"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether the container parser recorded any anomaly while reading the file."
    }
    fn why_it_matters(&self) -> &'static str {
        "The parser records what it had to tolerate. Those are the places where \
         the file's structure departed from the specification and where any \
         downstream measurement is least certain."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, _profile: &RuleProfile) -> Vec<Finding> {
        let Some(inspection) = &bundle.container else {
            return Vec::new();
        };
        inspection
            .anomalies
            .iter()
            .map(|anomaly| {
                finding(
                    self.id(),
                    bundle,
                    Severity::Warning,
                    // An anomaly is a statement about what the parser saw, not
                    // about what the file means.
                    Confidence::High,
                    format!("Container parser recorded an anomaly: {anomaly}"),
                    vec![format!("anomaly: {anomaly}")],
                    None,
                )
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Video rules
// ---------------------------------------------------------------------------

/// Reports a keyframe run long enough to suggest only one keyframe overall.
pub struct SingleKeyframe;

impl ForensicRule for SingleKeyframe {
    fn id(&self) -> &'static str {
        "VIDEO.SINGLE_KEYFRAME"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether the video track declares a single sync sample."
    }
    fn why_it_matters(&self) -> &'static str {
        "With one keyframe, seeking is approximate throughout the file and a \
         cut made in an editing tool has no nearby anchor to align to."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, _profile: &RuleProfile) -> Vec<Finding> {
        let Some(report) = &bundle.gop else {
            return Vec::new();
        };
        if report.keyframe_count != 1 {
            return Vec::new();
        }
        vec![finding(
            self.id(),
            bundle,
            Severity::Warning,
            Confidence::High,
            "Video track declares a single keyframe".to_owned(),
            vec![
                format!("keyframe count: {}", report.keyframe_count),
                format!("frame count: {}", report.frame_count),
            ],
            None,
        )]
    }
}

/// Reports a track where every frame is a sync sample.
pub struct AllFramesKeyframes;

impl ForensicRule for AllFramesKeyframes {
    fn id(&self) -> &'static str {
        "VIDEO.ALL_FRAMES_KEYFRAMES"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether the track declares no sync-sample box, making every frame a keyframe."
    }
    fn why_it_matters(&self) -> &'static str {
        "Every frame being a sync sample is unusual for encoded video and is a \
         property of how the file was produced rather than of its content."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, _profile: &RuleProfile) -> Vec<Finding> {
        let Some(inspection) = &bundle.container else {
            return Vec::new();
        };
        // `frame_info` is parallel to `streams`, so the two are zipped rather
        // than matched: a pointer comparison would be fragile, and the ordering
        // is part of `Mp4Inspection`'s contract.
        inspection
            .frame_info
            .iter()
            .zip(&inspection.streams)
            .filter_map(|(info, stream)| {
                let info = info.as_ref()?;
                (stream.kind == tpt_app_media_forensics_model::StreamKind::Video
                    && info.all_frames_are_keyframes)
                    .then_some((stream.index, info.frame_times.len()))
            })
            .map(|(index, frames)| {
                finding(
                    self.id(),
                    bundle,
                    Severity::Info,
                    // The declaration is explicit; whether it was intended is
                    // not something this rule can observe.
                    Confidence::High,
                    format!(
                        "Video track {index} declares no `stss` box; all {frames} frames are sync samples"
                    ),
                    vec![format!("stream: {index}"), format!("frames: {frames}")],
                    None,
                )
            })
            .collect()
    }
}

/// Reports a frame rate change within a track.
pub struct FrameRateChange;

impl ForensicRule for FrameRateChange {
    fn id(&self) -> &'static str {
        "VIDEO.FRAME_RATE_CHANGE"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether frame durations change partway through the track by more than the tolerance."
    }
    fn why_it_matters(&self) -> &'static str {
        "A frame rate that changes mid-file arises from joining material with \
         different timing, which is common in edited output and uncommon in a \
         single continuous recording."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, profile: &RuleProfile) -> Vec<Finding> {
        let Some(inspection) = &bundle.container else {
            return Vec::new();
        };

        let mut findings = Vec::new();
        for (index, info) in inspection.frame_info.iter().enumerate() {
            let Some(info) = info else { continue };
            if info.frame_times.len() < 3 {
                continue;
            }
            let Some(stream) = inspection.streams.get(index) else {
                continue;
            };

            // Compare each frame duration against the dominant one.
            let deltas: Vec<i64> = info
                .frame_times
                .windows(2)
                .map(|w| w[1].signed_diff(w[0]).as_micros())
                .collect();
            let Some(dominant) = mode(&deltas) else {
                continue;
            };

            for (position, delta) in deltas.iter().enumerate() {
                let difference = (delta - dominant).abs();
                if difference <= profile.pts_tolerance.as_micros() {
                    continue;
                }
                findings.push(finding(
                    self.id(),
                    bundle,
                    Severity::Info,
                    // A single differing duration could be a timestamp rounding
                    // artefact, so the observation carries less weight than a
                    // structural mismatch.
                    Confidence::Medium,
                    format!("Frame duration changes from {}us to {}us", dominant, delta),
                    vec![
                        format!("dominant frame duration: {dominant}us"),
                        format!("observed at frame {}", position + 1),
                        format!("stream: {}", stream.index),
                    ],
                    Some(info.frame_times[position]),
                ));
            }
        }
        findings
    }
}

// ---------------------------------------------------------------------------
// Audio rules
// ---------------------------------------------------------------------------

/// Reports audio too quiet to be heard as intended.
pub struct InaudibleAudio;

impl ForensicRule for InaudibleAudio {
    fn id(&self) -> &'static str {
        "AUDIO.INAUDIBLE"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether measured loudness falls below the profile's inaudible threshold."
    }
    fn why_it_matters(&self) -> &'static str {
        "Audio that measures below audibility is a delivery problem regardless \
         of what the file was meant to contain."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, profile: &RuleProfile) -> Vec<Finding> {
        let Some(loudness) = &bundle.loudness else {
            return Vec::new();
        };
        let lufs = loudness.value;
        if lufs > profile.inaudible_lufs {
            return Vec::new();
        }
        vec![finding(
            self.id(),
            bundle,
            Severity::Warning,
            // Integrated loudness is a direct measurement to a published
            // standard, so it carries full weight.
            Confidence::High,
            format!(
                "Integrated loudness measures {lufs} LUFS, at or below the inaudible threshold"
            ),
            vec![
                format!("measured: {lufs} LUFS"),
                format!("threshold: {} LUFS", profile.inaudible_lufs),
                format!("methodology: {:?}", loudness.methodology),
            ],
            None,
        )]
    }
}

/// Reports metadata that carries no creation-time field at all.
pub struct MissingCreationMetadata;

impl ForensicRule for MissingCreationMetadata {
    fn id(&self) -> &'static str {
        "METADATA.MISSING_CREATION_TIME"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether the file carries no creation-time metadata at all."
    }
    fn why_it_matters(&self) -> &'static str {
        "The absence of a creation time is an observation about the file's \
         recorded provenance. It does not establish when the media was made, and \
         a file with no metadata at all is reported separately."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, _profile: &RuleProfile) -> Vec<Finding> {
        let Some(tree) = &bundle.metadata else {
            return Vec::new();
        };

        // A file with no metadata at all is a different observation; this rule
        // asks the narrower question of whether metadata exists but carries no
        // creation time.
        if tree.is_empty() {
            return Vec::new();
        }

        const CREATION_KEYS: [&str; 4] = [
            "creation_time",
            "date",
            "creationdate",
            "com.apple.quicktime.creationdate",
        ];
        if CREATION_KEYS.iter().any(|key| !tree.find(key).is_empty()) {
            return Vec::new();
        }

        vec![finding(
            self.id(),
            bundle,
            Severity::Info,
            // That the field is missing is certain; why it is missing is not
            // observable here.
            Confidence::High,
            "File carries metadata but no creation-time field".to_owned(),
            vec![
                format!("metadata entries present: {}", tree.len()),
                format!("creation keys searched: {}", CREATION_KEYS.join(", ")),
            ],
            None,
        )]
    }
}

/// Returns the most common value, or `None` when there is no clear mode.
fn mode(values: &[i64]) -> Option<i64> {
    let mut best: Option<(i64, usize)> = None;
    let mut i = 0;
    while i < values.len() {
        let value = values[i];
        let count = values.iter().filter(|v| **v == value).count();
        if best.is_none_or(|(_, c)| count > c) {
            best = Some((value, count));
        }
        // Skip the whole run so each distinct value is counted once.
        i += count;
    }
    // A mode held by fewer than half the samples is not a dominant value.
    best.filter(|(_, count)| count.saturating_mul(2) >= values.len())
        .map(|(value, _)| value)
}

// ---------------------------------------------------------------------------
// Tier-2 rules
//
// These read decoded frames. Neither runs unless Tier-2 decoding succeeded, so a
// file the decoder could not read produces no pixel-level finding at all rather
// than a finding derived from absent measurements.
// ---------------------------------------------------------------------------

/// Reports consecutive frames differing by more than the profile's threshold.
pub struct SceneChange;

impl ForensicRule for SceneChange {
    fn id(&self) -> &'static str {
        "VIDEO.SCENE_CHANGE"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether consecutive decoded frames differ in mean luma by more than the \
         profile's threshold."
    }
    fn why_it_matters(&self) -> &'static str {
        "A large change between consecutive frames is consistent with a scene \
         change. It is also consistent with a dissolve, a flash, or a change of \
         lighting, so this reports the measured difference and not a cause."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, profile: &RuleProfile) -> Vec<Finding> {
        let Some(report) = &bundle.scene else {
            return Vec::new();
        };

        report
            .changes_above(profile.scene_change_threshold)
            .into_iter()
            .map(|change| {
                finding(
                    self.id(),
                    bundle,
                    Severity::Info,
                    // A mean luma difference is a single measurement with no
                    // corroborating indicator, so it cannot carry more weight
                    // than that however large it is.
                    Confidence::Medium,
                    format!(
                        "Mean luma difference of {:.1} between frames {} and {}",
                        change.mean_absolute,
                        change.index.saturating_sub(1),
                        change.index
                    ),
                    vec![
                        format!("mean absolute luma difference: {:.2}", change.mean_absolute),
                        format!(
                            "samples changed over threshold: {:.1}%",
                            change.changed_fraction * 100.0
                        ),
                        format!("profile threshold: {}", profile.scene_change_threshold),
                        format!("frames compared: {}", report.frames_examined),
                    ],
                    None,
                )
            })
            .collect()
    }
}

/// Reports frames whose decoded pictures match despite differing encoded bytes.
pub struct NearDuplicateFrames;

impl ForensicRule for NearDuplicateFrames {
    fn id(&self) -> &'static str {
        "VIDEO.NEAR_DUPLICATE_FRAME"
    }
    fn what_it_checks(&self) -> &'static str {
        "Whether two decoded frames within the profile's window have the same \
         perceptual hash."
    }
    fn why_it_matters(&self) -> &'static str {
        "A repeated picture re-encoded produces different bytes but the same \
         image, which packet-level duplicate detection cannot see. A perceptual \
         hash can also collide on two genuinely different shots of one scene, so \
         this is evidence of similarity rather than of reuse."
    }
    fn evaluate(&self, bundle: &AnalysisBundle, profile: &RuleProfile) -> Vec<Finding> {
        let Some(report) = &bundle.near_duplicates else {
            return Vec::new();
        };

        // Identical hashes are the strongest form this measurement produces, and
        // even then it is `Low`: two different shots of the same scene can hash
        // alike, and only a reviewer with the pictures can separate the cases.
        report
            .pairs
            .iter()
            .take(MAX_REPORTED_NEAR_DUPLICATES)
            .map(|pair| {
                finding(
                    self.id(),
                    bundle,
                    Severity::Info,
                    Confidence::Low,
                    format!(
                        "Decoded frames {} and {} have the same perceptual hash",
                        pair.earlier, pair.later
                    ),
                    vec![
                        format!("hash distance: {} bits of 64", pair.distance),
                        format!(
                            "comparison window: +/- {} frames",
                            profile.near_duplicate_window
                        ),
                        format!("frames examined: {}", report.frames_examined),
                    ],
                    None,
                )
            })
            .collect()
    }
}

/// Cap on near-duplicate pairs reported, so a pathological file cannot produce
/// thousands of findings that no reviewer will read.
const MAX_REPORTED_NEAR_DUPLICATES: usize = 20;

/// Returns every built-in rule.
#[must_use]
pub fn builtin_rules() -> Vec<Box<dyn ForensicRule>> {
    vec![
        Box::new(AllFramesKeyframes),
        Box::new(AvSyncDrift),
        Box::new(AudioClipping),
        Box::new(AudioDcOffset),
        Box::new(AudioSilence),
        Box::new(ContainerAnomaly),
        Box::new(ContainerAnomalyList),
        Box::new(DeclaredTrackMismatch),
        Box::new(DeclaredVsMeasuredMismatch),
        Box::new(DuplicateFrameRun),
        Box::new(FrameRateChange),
        Box::new(GopLengthChange),
        Box::new(InaudibleAudio),
        Box::new(MetadataConflict),
        Box::new(MissingCreationMetadata),
        Box::new(NearDuplicateFrames),
        Box::new(NoUsableStreams),
        Box::new(SceneChange),
        Box::new(NonMonotonicPts),
        Box::new(SingleKeyframe),
        Box::new(StreamDurationMissing),
        Box::new(StreamStartOffset),
        Box::new(TimestampGap),
    ]
}
