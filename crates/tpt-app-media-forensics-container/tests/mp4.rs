//! Integration tests for MP4 container inspection.
//!
//! Exercised against `tpt-kinetix-demux` through synthetic but structurally
//! valid files built by [`tpt_app_media_forensics_container::fixture`].
use tpt_app_media_forensics_container::fixture::{
    build_mp4, build_mp4_empty_moov, build_mp4_without_moov, TrackSpec,
};
use tpt_app_media_forensics_container::mp4::{inspect_bytes, inspect_file};
use tpt_app_media_forensics_container::probe::{detect_file, ContainerFormat};
use tpt_app_media_forensics_model::StreamKind;

/// A 25 fps video track: timescale 25, every sample 1 tick long.
fn valid_25fps_mp4() -> Vec<u8> {
    build_mp4(&TrackSpec::video_25fps(1920, 1080, 50))
}

/// An audio track.
fn audio_mp4() -> Vec<u8> {
    build_mp4(&TrackSpec::audio_48khz(480))
}

#[test]
fn a_valid_file_is_detected_and_parsed() {
    let inspection = inspect_bytes(valid_25fps_mp4()).expect("parses");

    assert_eq!(inspection.streams.len(), 1, "one track should be recovered");
    assert_eq!(inspection.streams[0].kind, StreamKind::Video);
    assert!(
        inspection.anomalies.is_empty(),
        "unexpected anomalies: {:?}",
        inspection.anomalies
    );
}

#[test]
fn video_dimensions_are_read_from_the_track_header() {
    let inspection = inspect_bytes(valid_25fps_mp4()).expect("parses");
    let video = inspection.streams[0].video_format().expect("video");

    assert_eq!(video.coded_width, 1920);
    assert_eq!(video.coded_height, 1080);
}

#[test]
fn frame_rate_is_measured_from_the_timing_table() {
    let inspection = inspect_bytes(valid_25fps_mp4()).expect("parses");
    let video = inspection.streams[0].video_format().expect("video");

    let rate = video.frame_rate.expect("frame rate should be measurable");
    assert_eq!((rate.numerator(), rate.denominator()), (25, 1));
}

#[test]
fn ntsc_frame_rate_keeps_its_exact_rational_form() {
    // 30000/1001 must survive as a fraction, not collapse to 29.97.
    let spec = TrackSpec {
        timescale: 30_000,
        // 1001 samples, each lasting 1001 ticks => exactly 30000/1001 fps.
        timing: vec![(1001, 1001)],
        ..TrackSpec::video_25fps(640, 480, 1001)
    };
    let inspection = inspect_bytes(build_mp4(&spec)).expect("parses");
    let video = inspection.streams[0].video_format().expect("video");

    let rate = video.frame_rate.expect("frame rate should be measurable");
    assert_eq!((rate.numerator(), rate.denominator()), (30_000, 1_001));
}

#[test]
fn variable_cadence_yields_no_single_frame_rate() {
    // A track whose sample deltas change is reporting a cadence change. Smoothing
    // it into one number would hide exactly the condition we must surface.
    let spec = TrackSpec {
        timing: vec![(25, 1), (25, 2)],
        ..TrackSpec::video_25fps(640, 480, 50)
    };
    let inspection = inspect_bytes(build_mp4(&spec)).expect("parses");
    let video = inspection.streams[0].video_format().expect("video");

    assert_eq!(
        video.frame_rate, None,
        "a changing cadence must not be reported as one frame rate"
    );
}

#[test]
fn audio_tracks_are_recognised_as_audio() {
    let inspection = inspect_bytes(audio_mp4()).expect("parses");
    assert_eq!(inspection.streams[0].kind, StreamKind::Audio);
}

#[test]
fn declared_sample_count_is_recorded() {
    let inspection = inspect_bytes(valid_25fps_mp4()).expect("parses");
    assert_eq!(inspection.streams[0].packet_count, Some(50));
}

#[test]
fn garbage_input_is_an_error_not_a_panic() {
    // Malformed media must never crash the application (spec §75).
    assert!(inspect_bytes(b"this is not an mp4 file at all".to_vec()).is_err());
    assert!(inspect_bytes(Vec::new()).is_err());
}

#[test]
fn a_truncated_container_does_not_panic() {
    let full = valid_25fps_mp4();
    for cut in [1, 8, 16, 32, 64] {
        let truncated = full[..full.len().saturating_sub(cut)].to_vec();
        let _ = inspect_bytes(truncated);
    }
}

#[test]
fn an_empty_moov_reports_an_anomaly_instead_of_erroring() {
    let inspection = inspect_bytes(build_mp4_empty_moov()).expect("container should open");
    assert!(inspection.streams.is_empty());
    assert!(
        !inspection.anomalies.is_empty(),
        "an empty moov must be reported, not silently accepted"
    );
}

#[test]
fn a_missing_moov_yields_no_tracks_without_crashing() {
    let inspection = inspect_bytes(build_mp4_without_moov());
    // Either a parse error or a zero-track result; never a panic.
    if let Ok(inspection) = inspection {
        assert!(inspection.streams.is_empty());
    }
}

#[test]
fn inspect_file_detects_the_format_from_disk() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("sample.mp4");
    std::fs::write(&path, valid_25fps_mp4()).expect("writes");

    assert_eq!(detect_file(&path).expect("reads"), ContainerFormat::IsoBmff);

    let inspection = inspect_file(&path).expect("inspects");
    assert_eq!(inspection.streams.len(), 1);
}

#[test]
fn a_missing_file_reports_an_error_naming_the_path() {
    let result = inspect_file(std::path::Path::new("no-such-file.mp4"));
    let message = result.expect_err("should fail").to_string();
    assert!(message.contains("no-such-file.mp4"), "{message}");
}
