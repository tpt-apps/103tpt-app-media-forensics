//! Delivery-profile checking against real media (spec Â§68, Â§69, Â§95).
//!
//! # What is asserted here
//!
//! That a *declared specification* can be checked against a *file*, requirement
//! by requirement, with an expected and an observed value for each. Spec Â§95's
//! output is reproduced exactly:
//!
//! ```text
//! video.frame_rate
//!   Expected: 25 (+/- 0.5)
//!   Observed: 30
//! ```
//!
//! These drive the checker over fixtures built by `-container` rather than over
//! hand-written inspections. The unit tests beside the checker use synthetic
//! inspections to pin each branch; these confirm the two halves actually fit â€”
//! that the properties the engine reads from a real file are the ones the
//! requirements ask about.
//!
//! # The claim being defended
//!
//! `RequirementOutcome::Undetermined` blocks delivery. That is the property worth
//! testing hardest, because the failure it prevents is invisible: a profile
//! reporting `PASS` for a file it never checked looks exactly like a profile
//! reporting `PASS` for a file that is correct.

use tpt_app_media_forensics_container::fixture::{build_mp4, build_mp4_av, TrackSpec};
use tpt_app_media_forensics_container::{inspect_bytes, ContainerInspection};
use tpt_app_media_forensics_model::{
    DeliveryProfile, DeliveryReport, Requirement, RequirementOutcome,
};
use tpt_app_media_forensics_rules::delivery::{
    check_profile, check_profile_with_av_offset, codec_family,
};

/// A 1920x1080 25 fps stereo 48 kHz MP4 â€” a file that meets the profile below.
fn conforming() -> ContainerInspection {
    inspect_bytes(build_mp4_av(
        &TrackSpec::video_25fps(1920, 1080, 50),
        &TrackSpec::audio_48khz(48_000),
        0,
    ))
    .expect("the fixture parses")
}

/// The profile from spec Â§68, with the container requirement named the way a
/// customer writes it (`mov`) rather than the way this build reports it
/// (`isobmff`).
fn spec_68_profile() -> DeliveryProfile {
    DeliveryProfile {
        name: "Delivery".to_owned(),
        version: 1,
        requirements: vec![
            Requirement::VideoCodec {
                any_of: vec!["h264".to_owned()],
            },
            Requirement::VideoResolution {
                width: 1920,
                height: 1080,
            },
            Requirement::FrameRate {
                fps: 25.0,
                tolerance: 0.5,
            },
            Requirement::AudioChannels { channels: 2 },
            Requirement::AudioSampleRate {
                sample_rate: 48_000,
            },
            Requirement::ContainerFormat {
                any_of: vec!["mov".to_owned()],
            },
        ],
    }
}

/// The outcome of one named requirement in a report.
fn outcome_of(report: &DeliveryReport, id: &str) -> RequirementOutcome {
    report
        .checks
        .iter()
        .find(|c| c.requirement_id == id)
        .unwrap_or_else(|| panic!("no check for {id}"))
        .outcome
}

/// The check for one named requirement in a report.
fn check_of<'a>(
    report: &'a DeliveryReport,
    id: &str,
) -> &'a tpt_app_media_forensics_model::RequirementCheck {
    report
        .checks
        .iter()
        .find(|c| c.requirement_id == id)
        .unwrap_or_else(|| panic!("no check for {id}"))
}

#[test]
fn a_conforming_file_passes_every_requirement_in_the_spec_profile() {
    // The point of Â§68 and Â§95: a file that meets the specification must be
    // reported as meeting it, per requirement, with no finding severities
    // involved.
    let report = check_profile(&spec_68_profile(), &conforming());
    for check in &report.checks {
        assert_eq!(
            check.outcome,
            RequirementOutcome::Met,
            "{} should have been met: {}",
            check.requirement_id,
            check.detail
        );
        assert!(
            check.observed.is_some(),
            "a met requirement must state what was observed"
        );
    }
    assert!(report.is_fully_met());
}

#[test]
fn a_wrong_resolution_is_reported_with_both_the_expected_and_the_observed() {
    // Spec Â§95's exact output shape. A verdict that does not carry the observed
    // value is not actionable by the person holding the file.
    let inspection =
        inspect_bytes(build_mp4(&TrackSpec::video_25fps(1280, 720, 50))).expect("parses");

    let report = check_profile(&spec_68_profile(), &inspection);
    let check = check_of(&report, "video.resolution");

    assert_eq!(check.outcome, RequirementOutcome::NotMet);
    assert_eq!(check.expected, "1920x1080");
    assert_eq!(check.observed.as_deref(), Some("1280x720"));
    assert!(!report.is_fully_met());
}

#[test]
fn a_file_with_no_audio_track_leaves_the_audio_requirements_unmeasured() {
    // The requirement that could not be checked must not be reported as met.
    // Before `container/src/audio_sample_entry.rs` existed, `StreamAnalysis::audio`
    // was `None` on every MP4 and `channels: 2` had nothing to compare against.
    let video_only =
        inspect_bytes(build_mp4(&TrackSpec::video_25fps(1920, 1080, 50))).expect("parses");

    let report = check_profile(&spec_68_profile(), &video_only);
    assert_eq!(
        outcome_of(&report, "audio.channels"),
        RequirementOutcome::Undetermined
    );
    assert_eq!(
        outcome_of(&report, "audio.sample_rate"),
        RequirementOutcome::Undetermined
    );
    assert!(
        !report.is_fully_met(),
        "an unchecked requirement is not a pass"
    );

    // And the gap must be explained, so a reader does not go hunting for a
    // defect in the file that is really a gap in the analysis.
    let check = check_of(&report, "audio.channels");
    assert!(check.observed.is_none(), "no value was measured");
    assert!(!check.detail.is_empty(), "the gap must be explained");
}

#[test]
fn a_frame_rate_within_tolerance_is_met_and_one_outside_is_not() {
    // 25 fps against a 25 +/- 0.5 profile passes. 30 fps is 5 away, which is not
    // a tolerance question at all.
    let report = check_profile(&spec_68_profile(), &conforming());
    assert_eq!(
        outcome_of(&report, "video.frame_rate"),
        RequirementOutcome::Met
    );

    let inspection =
        inspect_bytes(build_mp4(&TrackSpec::video_30fps(1920, 1080, 60))).expect("parses");
    let report = check_profile(&spec_68_profile(), &inspection);
    assert_eq!(
        outcome_of(&report, "video.frame_rate"),
        RequirementOutcome::NotMet
    );
    assert_eq!(
        check_of(&report, "video.frame_rate").expected,
        "25 (+/- 0.5)"
    );
}

#[test]
fn a_zero_tolerance_profile_demands_an_exact_rate_and_says_so() {
    // The tolerance is reported, not merely applied: a reviewer reading
    // "Expected: 25" beside a check that allowed 24.5 could not tell what was
    // permitted.
    let strict = DeliveryProfile {
        requirements: vec![Requirement::FrameRate {
            fps: 25.0,
            tolerance: 0.0,
        }],
        ..spec_68_profile()
    };
    let report = check_profile(&strict, &conforming());
    assert_eq!(
        outcome_of(&report, "video.frame_rate"),
        RequirementOutcome::Met,
        "an exact 25.0 measurement must still pass an exact profile"
    );
    assert_eq!(check_of(&report, "video.frame_rate").expected, "25 (exact)");
}

#[test]
fn the_wrong_channel_count_is_a_mismatch_not_an_unmeasured_result() {
    // 6 channels against a 2-channel profile is a real failure. Reporting it as
    // unmeasured would hide a genuine delivery problem behind a tooling gap.
    let inspection = inspect_bytes(build_mp4_av(
        &TrackSpec::video_25fps(1920, 1080, 50),
        &TrackSpec::audio_48khz_channels(48_000, 6),
        0,
    ))
    .expect("parses");

    let report = check_profile(&spec_68_profile(), &inspection);
    let check = check_of(&report, "audio.channels");
    assert_eq!(check.outcome, RequirementOutcome::NotMet);
    assert_eq!(check.observed.as_deref(), Some("6"));
    assert_eq!(check.expected, "2");
}

#[test]
fn the_wrong_sample_rate_is_a_mismatch_too() {
    let inspection = inspect_bytes(build_mp4_av(
        &TrackSpec::video_25fps(1920, 1080, 50),
        &TrackSpec::audio_48khz(48_000),
        0,
    ))
    .expect("parses");

    let report = check_profile(&spec_68_profile(), &inspection);
    assert_eq!(
        outcome_of(&report, "audio.sample_rate"),
        RequirementOutcome::Met,
        "48 kHz matches the profile"
    );

    let strict = DeliveryProfile {
        requirements: vec![Requirement::AudioSampleRate {
            sample_rate: 44_100,
        }],
        ..spec_68_profile()
    };
    let report = check_profile(&strict, &inspection);
    let check = check_of(&report, "audio.sample_rate");
    assert_eq!(check.outcome, RequirementOutcome::NotMet);
    assert_eq!(check.observed.as_deref(), Some("48000"));
    assert_eq!(check.expected, "44100");
}

#[test]
fn a_declared_codec_is_matched_by_family_and_the_fourcc_survives() {
    // A profile says `h264`; the container declares `avc1`. Without the family
    // table every MP4 would report NOT MET against the spec's own example.
    let report = check_profile(&spec_68_profile(), &conforming());
    assert_eq!(outcome_of(&report, "video.codec"), RequirementOutcome::Met);

    let observed = check_of(&report, "video.codec")
        .observed
        .clone()
        .expect("observed");
    assert!(
        observed.contains("avc1"),
        "the declared fourcc is the evidence and must survive: {observed}"
    );
    assert!(
        observed.contains("h264"),
        "the family is the profile's view: {observed}"
    );
}

#[test]
fn a_codec_family_this_build_does_not_map_is_unmeasured_not_a_mismatch() {
    // Blaming the file for a gap in the engine's own table would be wrong.
    let exotic = DeliveryProfile {
        requirements: vec![Requirement::VideoCodec {
            any_of: vec!["dvhdr".to_owned()],
        }],
        ..spec_68_profile()
    };
    let report = check_profile(&exotic, &conforming());
    assert_eq!(
        outcome_of(&report, "video.codec"),
        RequirementOutcome::Undetermined
    );
    assert!(!check_of(&report, "video.codec").detail.is_empty());
}

#[test]
fn an_av_offset_is_checked_only_when_one_was_measured() {
    let profile = DeliveryProfile {
        requirements: vec![Requirement::MaxAvOffsetMs { limit_ms: 40.0 }],
        ..spec_68_profile()
    };

    // No offset supplied: the requirement was never measured, and a profile that
    // passed here would be passing on a number nobody measured.
    assert_eq!(
        outcome_of(
            &check_profile(&profile, &conforming()),
            "timing.max_av_offset_ms"
        ),
        RequirementOutcome::Undetermined
    );

    assert_eq!(
        outcome_of(
            &check_profile_with_av_offset(&profile, &conforming(), Some(12.0)),
            "timing.max_av_offset_ms"
        ),
        RequirementOutcome::Met
    );
    assert_eq!(
        outcome_of(
            &check_profile_with_av_offset(&profile, &conforming(), Some(120.0)),
            "timing.max_av_offset_ms"
        ),
        RequirementOutcome::NotMet
    );
}

#[test]
fn a_container_format_this_build_does_not_model_is_unmeasured() {
    // `avi` is a real container this build does not detect. Reporting NOT MET
    // would tell a user their file is wrong when the truth is that the tool
    // cannot see it.
    let profile = DeliveryProfile {
        requirements: vec![Requirement::ContainerFormat {
            any_of: vec!["avi".to_owned()],
        }],
        ..spec_68_profile()
    };
    assert_eq!(
        outcome_of(&check_profile(&profile, &conforming()), "container.format"),
        RequirementOutcome::Undetermined
    );
}

#[test]
fn a_container_format_this_build_does_model_but_that_the_file_is_not_is_a_mismatch() {
    let profile = DeliveryProfile {
        requirements: vec![Requirement::ContainerFormat {
            any_of: vec!["matroska".to_owned()],
        }],
        ..spec_68_profile()
    };
    let report = check_profile(&profile, &conforming());
    assert_eq!(
        outcome_of(&report, "container.format"),
        RequirementOutcome::NotMet,
        "an MP4 does not satisfy a matroska requirement"
    );
}

#[test]
fn a_profile_with_no_requirements_reports_nothing_and_invents_no_failure() {
    // An empty profile is a real state. It must not manufacture either a
    // failure or the appearance of having checked something.
    let empty = DeliveryProfile::new("Anything", 1);
    let report = check_profile(&empty, &conforming());
    assert!(report.checks.is_empty());
    assert!(report.is_fully_met());
}

#[test]
fn the_report_names_the_profile_version_and_its_fingerprint() {
    // Spec Â§70: a report must identify the exact profile version used.
    let report = check_profile(&spec_68_profile(), &conforming());
    assert_eq!(report.profile_identifier, "delivery v1");
    assert_eq!(report.profile_version, 1);
    assert_eq!(report.profile_fingerprint, spec_68_profile().fingerprint());
}

#[test]
fn the_same_profile_and_file_always_produce_the_same_report() {
    // Spec Â§77. A delivery verdict that changed between two runs over one
    // unchanged file could not be defended in a dispute, which is the only
    // situation this tool exists for.
    assert_eq!(
        check_profile(&spec_68_profile(), &conforming()),
        check_profile(&spec_68_profile(), &conforming())
    );
}

#[test]
fn codec_families_map_in_both_directions_and_never_guess() {
    assert_eq!(codec_family("avc1"), Some("h264"));
    assert_eq!(codec_family("h264"), Some("h264"));
    assert_eq!(codec_family("mp4a"), Some("aac"));
    assert_eq!(codec_family("sowt"), Some("pcm"));
    assert_eq!(codec_family("zzzz"), None, "unknown must not be guessed");
    // Case-insensitive, because a profile author will write `H264`.
    assert_eq!(codec_family("AVC1"), Some("h264"));
}

#[test]
fn every_requirement_kind_has_a_distinct_id_and_renders_expected_text() {
    // A requirement rendering as an empty string would put "Expected: " with
    // nothing after it into a report a client reads.
    let all = [
        Requirement::VideoCodec {
            any_of: vec!["h264".into()],
        },
        Requirement::VideoResolution {
            width: 1,
            height: 2,
        },
        Requirement::FrameRate {
            fps: 25.0,
            tolerance: 0.5,
        },
        Requirement::AudioCodec {
            any_of: vec!["pcm".into()],
        },
        Requirement::AudioChannels { channels: 2 },
        Requirement::AudioSampleRate {
            sample_rate: 48_000,
        },
        Requirement::ContainerFormat {
            any_of: vec!["mov".into()],
        },
        Requirement::MaxAvOffsetMs { limit_ms: 40.0 },
    ];

    let mut ids: Vec<&str> = Vec::new();
    for requirement in all {
        assert!(!requirement.id().is_empty());
        assert!(
            !requirement.expected_text().is_empty(),
            "{:?} renders empty",
            requirement.id()
        );
        ids.push(requirement.id());
    }

    ids.sort_unstable();
    let count = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), count, "two requirement kinds share an id");
}
