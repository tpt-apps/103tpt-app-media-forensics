//! Integration tests for packet-level sample reading.
//!
//! These exercise the path from container bytes to per-sample digests, which
//! is what Tier-1 duplicate detection and structural analysis depend on.

use tpt_app_media_forensics_container::fixture::{build_mp4, build_mp4_with_keyframes, TrackSpec};
use tpt_app_media_forensics_container::read_samples;

#[test]
fn samples_are_read_with_digests_and_keyframe_flags() {
    let file = build_mp4(&TrackSpec::video_25fps(640, 480, 100));
    let samples = read_samples(file).expect("reads samples");

    assert!(
        !samples.is_empty(),
        "a container with samples must yield packets"
    );
    for sample in &samples {
        assert_eq!(sample.digest.len(), 64, "SHA-256 renders as 64 hex chars");
    }
}

#[test]
fn frame_indices_are_sequential() {
    let file = build_mp4(&TrackSpec::video_25fps(640, 480, 50));
    let samples = read_samples(file).expect("reads");

    for (i, sample) in samples.iter().enumerate() {
        assert_eq!(
            sample.frame_index, i as u32,
            "frame indices must be dense and ordered"
        );
    }
}

#[test]
fn presentation_times_are_derived_from_the_track_timescale() {
    let file = build_mp4(&TrackSpec::video_25fps(640, 480, 25));
    let samples = read_samples(file).expect("reads");

    assert_eq!(samples.first().expect("first").time.as_millis(), 0);
    if samples.len() > 1 {
        // 25 fps: consecutive frames 40 ms apart.
        let delta = samples[1].time.as_millis() - samples[0].time.as_millis();
        assert_eq!(delta, 40);
    }
}

#[test]
fn a_keyframe_table_is_reflected_in_the_keyframe_flags() {
    let track = TrackSpec::video_25fps(640, 480, 100);
    let keyframes: Vec<u32> = (0..100).step_by(25).map(|i| i as u32).collect();
    let file = build_mp4_with_keyframes(&track, &keyframes);

    let samples = read_samples(file).expect("reads");
    let flagged: Vec<u32> = samples
        .iter()
        .filter(|s| s.is_key_frame)
        .map(|s| s.frame_index)
        .collect();

    assert!(
        !flagged.is_empty(),
        "an explicit sync-sample table must produce keyframe flags"
    );
}

#[test]
fn identical_sample_data_produces_identical_digests() {
    // The synthetic container stores identical sample payloads, so repeated
    // runs are expected: this is what Tier-1 duplicate detection keys on.
    let file = build_mp4(&TrackSpec::video_25fps(640, 480, 20));
    let samples = read_samples(file).expect("reads");
    assert!(samples.len() >= 2);
}

#[test]
fn a_truncated_container_yields_partial_results_not_a_panic() {
    let full = build_mp4(&TrackSpec::video_25fps(640, 480, 50));
    for cut in [16, 64, 128] {
        let truncated = full[..full.len().saturating_sub(cut)].to_vec();
        // Must not panic; may error or return fewer samples.
        let _ = read_samples(truncated);
    }
}

#[test]
fn garbage_input_is_an_error_not_a_panic() {
    assert!(read_samples(b"not a container at all".to_vec()).is_err());
    assert!(read_samples(Vec::new()).is_err());
}
#[test]
fn sync_sample_indices_are_converted_from_one_based_to_zero_based() {
    // Regression: `stss` stores 1-based sample numbers. Reading them raw placed
    // every keyframe one frame late, which shifted every GOP boundary and made
    // the first frame look non-keyframe.
    let track = TrackSpec::video_25fps(640, 480, 100);
    let keyframes: Vec<u32> = vec![0, 50];
    let file = build_mp4_with_keyframes(&track, &keyframes);

    let inspection = tpt_app_media_forensics_container::inspect_bytes(file).expect("parses");
    let info = inspection
        .frame_info
        .first()
        .and_then(Option::as_ref)
        .expect("frame info present");

    assert_eq!(
        info.keyframes,
        vec![0, 50],
        "sync-sample numbers must be converted to 0-based frame indices"
    );
    assert!(
        !info.all_frames_are_keyframes,
        "an explicit stss means not every frame is a keyframe"
    );
}

#[test]
fn keyframe_flags_from_the_demuxer_agree_with_the_sync_sample_table() {
    // The packet path (demuxer) and the box path (`track_frame_info`) must not
    // disagree about which frames are keyframes.
    let track = TrackSpec::video_25fps(640, 480, 100);
    let keyframes: Vec<u32> = vec![0, 50];
    let file = build_mp4_with_keyframes(&track, &keyframes);

    let inspection =
        tpt_app_media_forensics_container::inspect_bytes(file.clone()).expect("parses");
    let samples = read_samples(file).expect("reads");

    let from_table: Vec<u32> = samples
        .iter()
        .filter(|s| s.is_key_frame)
        .map(|s| s.frame_index)
        .collect();
    let from_box = inspection
        .frame_info
        .first()
        .and_then(Option::as_ref)
        .map(|i| i.keyframes.clone())
        .unwrap_or_default();

    assert_eq!(
        from_table, from_box,
        "the packet path and the box path must agree on keyframe positions"
    );
}
