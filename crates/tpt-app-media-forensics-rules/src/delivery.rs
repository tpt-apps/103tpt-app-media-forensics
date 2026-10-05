//! Delivery-profile checking (spec §68, §69, §95).
//!
//! # What this answers, and what it does not
//!
//! [`crate::profile::RuleProfile`] decides how strict the *forensic* rules are.
//! This module answers a different question: does this file meet a *declared
//! specification*? Spec §68's example is `codec: h264`, `width: 1920`,
//! `frame_rate: 25`, `channels: 2`, `sample_rate: 48000`, `format: mov`, and
//! spec §95 asks for `PASS` or a list of expected-versus-observed failures.
//!
//! Before this module, `validate` could only derive a verdict from finding
//! severities, which answers "do the findings permit delivery" and not "does
//! this file meet the specification". Both halves matter, and they are combined
//! by [`tpt_app_media_forensics_report::ValidationResult::from_delivery`].
//!
//! # Unmeasured is never met
//!
//! Every requirement that cannot be measured yields
//! [`RequirementOutcome::Undetermined`], which blocks delivery. A WebM file
//! checked against `video.resolution` is *not* a passing file: this build's
//! Matroska reader exposes no picture geometry, so nothing established the
//! resolution either way, and reporting `PASS` would be a claim the run does not
//! support.
//!
//! # The codec table is explicit
//!
//! A profile says `h264`; a file declares `avc1`. [`codec_family`] is the one
//! place that translation happens, and an unrecognised tag returns `None` rather
//! than a guess — so a delivery against an exotic codec is reported as
//! unmeasured instead of being matched against whichever family sorts nearby.

use tpt_app_media_forensics_container::{ContainerFormat, ContainerInspection};
use tpt_app_media_forensics_model::{
    trim_float, AudioFormat, DeliveryReport, Requirement, RequirementCheck, RequirementOutcome,
    StreamAnalysis, StreamKind,
};

/// The measured properties a requirement can be checked against.
///
/// Gathered once so every requirement reads the same container, and so "there is
/// no video stream" and "the video stream declares no resolution" stay
/// distinguishable states rather than both collapsing into a `None`.
struct Facts<'a> {
    format: ContainerFormat,
    video: Option<&'a StreamAnalysis>,
    audio: Option<&'a StreamAnalysis>,
    /// Largest absolute A/V offset in milliseconds, when one was measured.
    av_offset_ms: Option<f64>,
}

/// Checks a delivery profile against an inspected container (spec §68, §95).
///
/// A free function rather than a method on [`tpt_app_media_forensics_model::DeliveryProfile`]
/// because the profile type lives in `-model`, which must not depend on
/// `-container` — and an inherent `impl` has to live in the defining crate. The
/// checker belongs here for the same reason every rule does: it needs the
/// container model, and `-model` is below both.
///
/// Takes an inspection rather than a path so the check is a pure function of
/// measured properties. That is the same discipline
/// [`crate::engine::ForensicRule`] follows, and it is the reason the requirement
/// logic is testable with no media present.
#[must_use]
pub fn check_profile(
    profile: &tpt_app_media_forensics_model::DeliveryProfile,
    inspection: &ContainerInspection,
) -> DeliveryReport {
    check_profile_with_av_offset(profile, inspection, None)
}

/// Checks a profile, supplying a measured A/V offset in milliseconds.
///
/// The offset is a separate argument because it comes from the timing layer
/// rather than the container. A caller that has not run that analysis passes
/// `None`, and `timing.max_av_offset_ms` then reports
/// [`RequirementOutcome::Undetermined`] rather than passing on a measurement
/// nobody took.
#[must_use]
pub fn check_profile_with_av_offset(
    profile: &tpt_app_media_forensics_model::DeliveryProfile,
    inspection: &ContainerInspection,
    av_offset_ms: Option<f64>,
) -> DeliveryReport {
    let facts = Facts {
        format: inspection.format,
        // The *first* stream of each kind, matching how every other consumer of
        // an inspection picks one. A multi-track file is something this build
        // identifies but does not fully model, and taking the first track
        // consistently beats taking whichever happened to sort last.
        video: inspection
            .streams
            .iter()
            .find(|s| s.kind == StreamKind::Video),
        audio: inspection
            .streams
            .iter()
            .find(|s| s.kind == StreamKind::Audio),
        av_offset_ms,
    };

    DeliveryReport {
        profile_name: profile.name.clone(),
        profile_version: profile.version,
        profile_identifier: profile.identifier(),
        profile_fingerprint: profile.fingerprint(),
        checks: profile
            .requirements
            .iter()
            .map(|requirement| check_one(requirement, &facts))
            .collect(),
    }
}

/// Checks one requirement against the measured facts.
fn check_one(requirement: &Requirement, facts: &Facts<'_>) -> RequirementCheck {
    let expected = requirement.expected_text();

    let (outcome, observed, detail) = match requirement {
        Requirement::VideoCodec { any_of } => {
            codec_check(any_of, facts.video.map(|s| &s.codec), "video")
        }
        Requirement::AudioCodec { any_of } => {
            codec_check(any_of, facts.audio.map(|s| &s.codec), "audio")
        }
        Requirement::VideoResolution { width, height } => {
            resolution_check(*width, *height, facts.video)
        }
        Requirement::FrameRate { fps, tolerance } => {
            frame_rate_check(*fps, *tolerance, facts.video)
        }
        Requirement::AudioChannels { channels } => {
            channels_check(*channels, facts.audio.and_then(|s| s.audio.as_ref()))
        }
        Requirement::AudioSampleRate { sample_rate } => {
            sample_rate_check(*sample_rate, facts.audio.and_then(|s| s.audio.as_ref()))
        }
        Requirement::ContainerFormat { any_of } => container_check(any_of, facts.format),
        Requirement::MaxAvOffsetMs { limit_ms } => av_offset_check(*limit_ms, facts.av_offset_ms),
    };

    RequirementCheck {
        requirement_id: requirement.id().to_owned(),
        expected,
        observed,
        outcome,
        detail,
    }
}

/// Checks a codec requirement against a declared tag.
fn codec_check(
    any_of: &[String],
    codec: Option<&tpt_app_media_forensics_model::CodecInfo>,
    which: &str,
) -> (RequirementOutcome, Option<String>, String) {
    let Some(codec) = codec else {
        return (
            RequirementOutcome::Undetermined,
            None,
            format!("the file declares no {which} stream"),
        );
    };

    // The declared tag is printed verbatim beside the family it resolved to. The
    // tag is the evidence; the family is only the vocabulary the profile is
    // written in.
    let observed = format!(
        "{} ({})",
        codec_family(&codec.name).unwrap_or("unknown"),
        codec.name
    );

    let Some(family) = codec_family(&codec.name) else {
        return (
            RequirementOutcome::Undetermined,
            Some(observed),
            format!(
                "the container declares codec `{}`, which this build does not map to a \
                 codec family, so it cannot be compared against the profile",
                codec.name
            ),
        );
    };

    let accepted: Vec<&str> = any_of.iter().filter_map(|n| codec_family(n)).collect();

    if accepted.is_empty() {
        // A profile naming a family this build does not know is a profile that
        // cannot be satisfied. Reporting it as `NOT MET` would blame the file for
        // a mistake in the specification.
        return (
            RequirementOutcome::Undetermined,
            Some(observed),
            format!(
                "the profile names no codec family this build recognises (asked for: {})",
                any_of.join(", ")
            ),
        );
    }

    if accepted.contains(&family) {
        (
            RequirementOutcome::Met,
            Some(observed),
            format!("the declared codec is {family}, which the profile accepts"),
        )
    } else {
        (
            RequirementOutcome::NotMet,
            Some(observed),
            format!(
                "the declared codec is {family}; the profile accepts {}",
                accepted.join(" or ")
            ),
        )
    }
}

/// Checks a frame-size requirement.
fn resolution_check(
    width: u32,
    height: u32,
    video: Option<&StreamAnalysis>,
) -> (RequirementOutcome, Option<String>, String) {
    let Some(video) = video else {
        return (
            RequirementOutcome::Undetermined,
            None,
            "the file declares no video stream".to_owned(),
        );
    };
    let Some((actual_width, actual_height)) = video.dimensions() else {
        return (
            RequirementOutcome::Undetermined,
            None,
            "the video stream declares no frame size: this build's Matroska reader \
             exposes no picture geometry"
                .to_owned(),
        );
    };

    let observed = format!("{actual_width}x{actual_height}");
    if actual_width == width && actual_height == height {
        (
            RequirementOutcome::Met,
            Some(observed),
            "the declared frame size matches the profile".to_owned(),
        )
    } else {
        (
            RequirementOutcome::NotMet,
            Some(observed),
            "the declared frame size does not match the profile".to_owned(),
        )
    }
}

/// Checks a frame-rate requirement, within its tolerance.
fn frame_rate_check(
    fps: f64,
    tolerance: f64,
    video: Option<&StreamAnalysis>,
) -> (RequirementOutcome, Option<String>, String) {
    let Some(video) = video else {
        return (
            RequirementOutcome::Undetermined,
            None,
            "the file declares no video stream".to_owned(),
        );
    };
    let Some(rate) = frame_rate_of(video) else {
        return (
            RequirementOutcome::Undetermined,
            None,
            "the video stream declares no frame rate: the container carried no \
             timing table to derive one from"
                .to_owned(),
        );
    };

    let observed = trim_float(rate);
    // Compared as a difference, not by rounding the rate to an integer first.
    // 24000/1001 is 23.976 fps, and a check that rounded before comparing would
    // misjudge exactly the NTSC rates this tolerance exists to admit.
    let difference = (rate - fps).abs();
    if difference <= tolerance {
        (
            RequirementOutcome::Met,
            Some(observed),
            format!(
                "the measured rate is within {} fps of the profile",
                trim_float(tolerance)
            ),
        )
    } else {
        (
            RequirementOutcome::NotMet,
            Some(observed),
            format!(
                "the measured rate differs from the profile by {} fps, past the {} \
                 fps tolerance",
                trim_float(difference),
                trim_float(tolerance)
            ),
        )
    }
}

/// Checks an audio channel-count requirement.
fn channels_check(
    channels: u16,
    audio: Option<&AudioFormat>,
) -> (RequirementOutcome, Option<String>, String) {
    let Some(audio) = audio else {
        return (
            RequirementOutcome::Undetermined,
            None,
            "the file declares no audio stream parameters".to_owned(),
        );
    };
    let actual = audio.channel_count();
    if actual == 0 {
        // `channel_count()` returns 0 for an absent layout, which is this model's
        // honest "not measured" value. Treating it as a measured zero would fail
        // every profile for entirely the wrong reason.
        return (
            RequirementOutcome::Undetermined,
            None,
            "the audio sample entry declares no channel layout".to_owned(),
        );
    }

    let observed = actual.to_string();
    if actual == channels {
        (
            RequirementOutcome::Met,
            Some(observed),
            "the declared channel count matches the profile".to_owned(),
        )
    } else {
        (
            RequirementOutcome::NotMet,
            Some(observed),
            "the declared channel count does not match the profile".to_owned(),
        )
    }
}

/// Checks an audio sample-rate requirement.
fn sample_rate_check(
    sample_rate: u32,
    audio: Option<&AudioFormat>,
) -> (RequirementOutcome, Option<String>, String) {
    let Some(audio) = audio else {
        return (
            RequirementOutcome::Undetermined,
            None,
            "the file declares no audio stream parameters".to_owned(),
        );
    };
    if audio.sample_rate == 0 {
        // Same reasoning as the channel count: zero here means the sample entry
        // declared none, not that the file is a 0 Hz stream.
        return (
            RequirementOutcome::Undetermined,
            None,
            "the audio sample entry declares no sample rate".to_owned(),
        );
    }

    let observed = audio.sample_rate.to_string();
    if audio.sample_rate == sample_rate {
        (
            RequirementOutcome::Met,
            Some(observed),
            "the declared sample rate matches the profile".to_owned(),
        )
    } else {
        (
            RequirementOutcome::NotMet,
            Some(observed),
            "the declared sample rate does not match the profile".to_owned(),
        )
    }
}

/// Checks a measured A/V offset against a limit.
fn av_offset_check(
    limit_ms: f64,
    offset_ms: Option<f64>,
) -> (RequirementOutcome, Option<String>, String) {
    let Some(offset) = offset_ms else {
        return (
            RequirementOutcome::Undetermined,
            None,
            "no A/V offset was measured: the check needs both a video and an audio \
             track with measurable timing"
                .to_owned(),
        );
    };

    let observed = format!("{} ms", trim_float(offset));
    if offset <= limit_ms {
        (
            RequirementOutcome::Met,
            Some(observed),
            format!(
                "the A/V offset is within the {} ms limit",
                trim_float(limit_ms)
            ),
        )
    } else {
        (
            RequirementOutcome::NotMet,
            Some(observed),
            format!(
                "the A/V offset exceeds the {} ms limit",
                trim_float(limit_ms)
            ),
        )
    }
}

/// Checks a container-format requirement.
fn container_check(
    any_of: &[String],
    format: ContainerFormat,
) -> (RequirementOutcome, Option<String>, String) {
    let observed = format.tag().to_owned();

    if any_of.iter().any(|name| format_matches(name, format)) {
        return (
            RequirementOutcome::Met,
            Some(observed),
            "the detected container format matches the profile".to_owned(),
        );
    }

    // A profile may name a format this build does not model at all. That is a
    // specification this engine cannot check, not a file that failed — and only
    // one of those two is actionable by the person holding the file.
    if any_of.iter().all(|name| !is_known_format_name(name)) {
        return (
            RequirementOutcome::Undetermined,
            Some(observed.clone()),
            format!(
                "the profile names container format(s) this build does not model: {}",
                any_of.join(", ")
            ),
        );
    }

    (
        RequirementOutcome::NotMet,
        Some(observed.clone()),
        format!(
            "the file is a {observed} container; the profile requires {}",
            any_of.join(" or ")
        ),
    )
}

/// Whether a profile names a container format this build has a tag for.
///
/// Extensions are *not* included: `mov` and `webm` are accepted when they match
/// a detected family, but a profile naming only `avi` is naming something this
/// build cannot detect at all, which is a different situation from naming a
/// family that simply is not present.
fn is_known_format_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        ContainerFormat::IsoBmff.tag(),
        ContainerFormat::Matroska.tag(),
        ContainerFormat::Wav.tag(),
        ContainerFormat::Aiff.tag(),
        ContainerFormat::Ogg.tag(),
        ContainerFormat::Flac.tag(),
    ]
    .contains(&lower.as_str())
}

/// Whether a profile's format name matches what was detected.
///
/// A profile may name either the tag this build reports (`isobmff`) or the
/// extension a customer writes in a specification (`mp4`, `mov`, `mkv`, `webm`).
/// Both are accepted, because spec §68's own example says `format: mov` while
/// this build reports `isobmff` for every ISO-BMFF file it sees.
///
/// **That coarseness is a stated limitation, not a fact.** This build detects the
/// family from the `ftyp` signature and does not distinguish an Apple `qt  `
/// brand from `isom`, so an `isom`-branded file with a `.mov` name satisfies a
/// `mov` requirement. Closing that gap means reading the major brand in
/// `-container`, not faking a distinction here.
fn format_matches(name: &str, format: ContainerFormat) -> bool {
    let name = name.to_ascii_lowercase();
    if name == format.tag() {
        return true;
    }
    match format {
        ContainerFormat::IsoBmff => matches!(name.as_str(), "mp4" | "m4v" | "mov" | "3gp" | "3g2"),
        ContainerFormat::Matroska => matches!(name.as_str(), "mkv" | "webm"),
        _ => false,
    }
}

/// Maps a codec family name or a declared fourcc onto a canonical family.
///
/// The one place a profile's vocabulary and a container's vocabulary meet. It is
/// a table rather than a rule because the mapping is genuinely irregular: `mp4a`
/// is AAC and `sowt` is PCM, and both sit in files this engine reads.
///
/// Returns `None` for anything not listed, which callers treat as *unmeasured*
/// rather than as a mismatch.
#[must_use]
pub fn codec_family(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    let family = match lower.as_str() {
        // Video.
        "h264" | "avc" | "avc1" | "avc3" | "x264" => "h264",
        "h265" | "hevc" | "hvc1" | "hev1" => "h265",
        "av1" | "av01" => "av1",
        "vp9" | "vp09" => "vp9",
        "mpeg2video" | "mpeg-2" | "mpv2" => "mpeg2video",
        "prores" | "apch" | "apcn" | "apcs" | "apco" | "ap4h" | "ap4x" => "prores",
        "dnxhd" | "avdn" => "dnxhd",
        // Audio.
        "aac" | "mp4a" | "aacp" => "aac",
        "pcm" | "sowt" | "twos" | "lpcm" | "in24" | "in32" | "fl32" | "fl64" | "ulaw" => "pcm",
        "opus" => "opus",
        "vorbis" => "vorbis",
        "flac" => "flac",
        "mp3" | ".mp3" => "mp3",
        "alac" => "alac",
        _ => return None,
    };
    Some(family)
}

/// The declared frame rate of a video stream, if it declares one.
fn frame_rate_of(video: &StreamAnalysis) -> Option<f64> {
    video
        .video_format()
        .and_then(|format| format.frame_rate)
        .map(|rate| rate.to_f64())
}
