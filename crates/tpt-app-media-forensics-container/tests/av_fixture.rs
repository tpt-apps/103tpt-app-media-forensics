//! The two-track MP4 fixture, and the demuxer defect it exposed.
//!
//! # What is asserted here
//!
//! `build_mp4_av` produces a file that parses correctly — two streams, no
//! anomalies — but whose *sample* data the upstream MP4 demuxer cannot read. It
//! yields packets forever instead of ending, and each one carries a plausible
//! non-zero size, so nothing short of a bound stops it.
//!
//! That matters beyond this fixture: a forensic tool handed a malformed file
//! must still produce a report. A reader that never returns produces no report
//! at all, which a reader of the results cannot distinguish from a clean file.
//!
//! These tests pin both halves. The file is well formed, and the sample reader
//! says so rather than looping.

use tpt_app_media_forensics_container::fixture::{build_mp4_av, TrackSpec};
use tpt_app_media_forensics_container::{inspect_bytes, read_samples};

fn av_file() -> Vec<u8> {
    build_mp4_av(
        &TrackSpec::video_25fps(320, 240, 50),
        &TrackSpec::audio_48khz(2_400),
        0,
    )
}

#[test]
fn the_av_fixture_declares_both_tracks_with_no_anomalies() {
    let inspection = inspect_bytes(av_file()).expect("the fixture parses");
    assert_eq!(inspection.streams.len(), 2, "a video and an audio track");
    assert!(
        inspection.anomalies.is_empty(),
        "a well-formed fixture should report nothing: {:?}",
        inspection.anomalies
    );
}

/// The first chunk offset each track declares, in document order.
fn declared_chunk_offsets(bytes: &[u8]) -> Vec<u32> {
    let mut found = Vec::new();
    let mut i = 0;
    while let Some(rel) = bytes[i..].windows(4).position(|w| w == b"stco") {
        let at = i + rel;
        // 8-byte box header, then 4 bytes of version/flags, then the offset.
        found.push(u32::from_be_bytes([
            bytes[at + 12],
            bytes[at + 13],
            bytes[at + 14],
            bytes[at + 15],
        ]));
        i = at + 4;
    }
    found
}

/// Where the top-level `mdat` box begins.
///
/// Walked structurally rather than found by searching for the bytes `mdat`:
/// sample payloads are arbitrary, and this fixture's pattern happens to spell
/// `mdat`, so a text search finds a box that does not exist. That is the same
/// trap the builder stopped falling into.
fn top_level_mdat(bytes: &[u8]) -> Option<usize> {
    let mut offset = 0;
    while offset + 8 <= bytes.len() {
        let size = u32::from_be_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]) as usize;
        if size < 8 || offset + size > bytes.len() {
            return None;
        }
        if &bytes[offset + 4..offset + 8] == b"mdat" {
            return Some(offset);
        }
        offset += size;
    }
    None
}

#[test]
fn each_track_declares_its_own_distinct_chunk_offset() {
    // Checked against the bytes rather than through the demuxer, because the
    // demuxer cannot currently traverse this file. An earlier version of this
    // fixture never copied the audio track's `mdat`, so its samples resolved to
    // the video's bytes; a byte-level check catches that even while the reader
    // is broken.
    let bytes = av_file();
    let offsets = declared_chunk_offsets(&bytes);
    assert_eq!(offsets.len(), 2, "one chunk table per track: {offsets:?}");

    let first_sample = top_level_mdat(&bytes).expect("the file carries sample data") + 8;

    assert_ne!(
        offsets[0], offsets[1],
        "both tracks point at the same bytes, so one of them is reading the other's data"
    );
    for offset in &offsets {
        let at = *offset as usize;
        assert!(
            at >= first_sample,
            "a chunk offset of {at} lands before the sample data at {first_sample}, so the \
             demuxer would read the file header as frames"
        );
        assert!(
            at < bytes.len(),
            "a chunk offset of {at} points past the end of this {} byte file",
            bytes.len()
        );
    }
}

#[test]
fn reading_samples_from_the_av_fixture_reports_rather_than_looping() {
    // The regression this guards. Without a bound, this call never returns.
    match read_samples(av_file()) {
        Ok(samples) => {
            // If the upstream demuxer is ever fixed, the samples must be the
            // ones the fixture actually wrote.
            assert_eq!(samples.len(), 50 + 2_400, "every declared sample, once");
        }
        Err(e) => {
            let message = e.to_string();
            assert!(
                message.contains("not advancing"),
                "a failure here must say the reader was not advancing, got: {message}"
            );
        }
    }
}

#[test]
fn a_single_track_fixture_still_yields_every_sample() {
    // The bound must not fire on a file the demuxer handles: it exists to catch
    // a malfunction, not to truncate ordinary work.
    let bytes = tpt_app_media_forensics_container::fixture::build_mp4(&TrackSpec::video_25fps(
        320, 240, 50,
    ));
    let samples = read_samples(bytes).expect("a single-track file is readable");
    assert_eq!(samples.len(), 50);
}
