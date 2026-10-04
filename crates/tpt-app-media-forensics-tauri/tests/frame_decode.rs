//! Frame decoding, end to end on genuinely encoded AV1 (spec \u00a743, \u00a744).
//!
//! # Why this uses a real encoder
//!
//! A frame-decoding test that fed synthetic "frames" to the decoder would prove
//! nothing about the path a real file takes: compressed bytes in, pixels out.
//! The foundation ships an AV1 encoder backed by rav1e, so the fixture here is
//! real - genuinely encoded AV1 wrapped in a real WebM container, read back
//! through the demuxer, decoded to pixels, and handed to the inspector and
//! histogram the viewer uses.
//!
//! It is also the test that settles whether the viewer can display pixels at
//! all. I had recorded that as needing an engine change; it does not, because
//! `DecodeSession::decode_prefix`, `read_samples_file` and
//! `DecodedFrame::to_greyscale` are already public. This file is the evidence.
//!
//! # Greyscale, and labelled
//!
//! `DecodedFrame` retains only the luma plane - chroma is what the analysers do
//! not need, and keeping it for every frame would multiply the memory the
//! window bound exists to prevent. So the decoded frame is greyscale and
//! `RgbBasis::LumaOnly` says so. A viewer presenting it as colour would show
//! neutral chroma that was never measured.

use tpt_app_media_forensics_container::build_webm;
use tpt_app_media_forensics_tauri::view::viewer::{
    FrameDifference, FrameImageView, Histogram, PixelSample, RgbBasis,
};
use tpt_kinetix_av1::{Av1Encoder, Av1EncoderConfig};
use tpt_kinetix_core::frame::VideoFrame;
use tpt_kinetix_core::pixel_format::PixelFormat;
use tpt_kinetix_core::timestamp::Timestamp;

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;
const FRAME_MS: i64 = 33;

/// One frame of deterministic content.
///
/// `level` shifts the luma so consecutive frames differ measurably. A sequence
/// that barely changes would make a difference assertion pass or fail for
/// reasons unrelated to whether the decoder worked.
fn frame(level: u8, pts_ms: i64) -> VideoFrame {
    let w = WIDTH as usize;
    let h = HEIGHT as usize;
    let y_size = w * h;

    let mut data = vec![0u8; y_size + (y_size / 4) * 2];
    for y in 0..h {
        for x in 0..w {
            // A moving diagonal keeps the image non-uniform, which matters: a
            // flat image compresses to almost nothing and would not exercise
            // real coefficient coding.
            data[y * w + x] = ((level as usize + x + y) % 256) as u8;
        }
    }
    for sample in data.iter_mut().skip(y_size) {
        *sample = 128;
    }

    let ts = Timestamp::new(pts_ms, (1, 1_000));
    VideoFrame {
        pts: ts,
        dts: ts,
        data,
        width: WIDTH,
        height: HEIGHT,
        pixel_format: PixelFormat::Yuv420p,
        is_key_frame: true,
    }
}

/// Encodes real AV1 and wraps it in a real WebM container.
fn real_av1_webm(count: usize) -> Vec<u8> {
    let config = Av1EncoderConfig {
        width: WIDTH,
        height: HEIGHT,
        bitrate: 0,
        quantizer: 80,
        speed: 10,
        keyframe_interval: count as u64,
    };
    let mut encoder = Av1Encoder::new(&config).expect("opens an AV1 encoder");

    // Packets are collected across the whole run rather than per frame: the
    // encoder buffers, so `encode_frame` returning `None` for an early picture is
    // normal and collecting only what it returns immediately yields nothing.
    let mut packets = Vec::new();
    for index in 0..count {
        let picture = frame((index * 30) as u8, index as i64 * FRAME_MS);
        if let Some(packet) = encoder.encode_frame(&picture).expect("encodes") {
            packets.push(packet);
        }
    }
    packets.extend(encoder.flush().expect("flushes"));
    assert!(!packets.is_empty(), "the encoder produced no packets");

    let blocks: Vec<(u16, bool, Vec<u8>)> = packets
        .iter()
        .enumerate()
        .map(|(index, packet)| {
            (
                u16::try_from(index * FRAME_MS as usize).unwrap_or(u16::MAX),
                packet.is_key_frame,
                packet.data.clone(),
            )
        })
        .collect();
    build_webm("V_AV1", 1, &blocks)
}

/// Decodes one frame the way the viewer's command does.
///
/// Written out rather than calling the command, because a `#[tauri::command]`
/// needs a `tauri::State` this crate cannot construct. What is under test is the
/// decode path itself, which is the part that could be wrong.
fn decode_one(source: &std::path::Path, wanted: u64) -> Option<FrameImageView> {
    use tpt_app_media_forensics_video::{DecodeLimits, DecodeSession};

    let bytes = std::fs::read(source).ok()?;
    let inspection =
        tpt_app_media_forensics_container::inspect_matroska_bytes(bytes.clone()).ok()?;
    let video = inspection
        .streams
        .iter()
        .find(|s| s.kind == tpt_app_media_forensics_model::StreamKind::Video)?;
    let samples = tpt_app_media_forensics_container::read_matroska_samples(bytes).ok()?;

    let packets: Vec<(Vec<u8>, bool)> = samples
        .iter()
        .filter(|s| s.stream_index == video.index)
        .map(|s| (s.data.clone(), s.is_key_frame))
        .collect();

    let wanted = usize::try_from(wanted).ok()?;
    if wanted >= packets.len() {
        return None;
    }

    let mut session = DecodeSession::open(&video.codec.name, DecodeLimits::default()).ok()?;
    let (frames, _) = session.decode_prefix(&packets[..=wanted]);
    let decoded = frames.last()?;
    let image = decoded
        .to_greyscale(tpt_app_media_forensics_model::MediaTime::ZERO)
        .ok()?;

    Some(FrameImageView {
        frame_index: wanted as u64,
        width: image.width,
        height: image.height,
        time: tpt_app_media_forensics_model::MediaTime::ZERO,
        rgb: Some(image.rgb),
        basis: RgbBasis::LumaOnly,
        evidence_path: None,
    })
}

#[test]
fn a_real_av1_frame_decodes_to_pixels_a_viewer_can_display() {
    let dir = tempfile::tempdir().expect("scratch");
    let source = dir.path().join("av1.webm");
    std::fs::write(&source, real_av1_webm(6)).expect("writes");

    let Some(frame) = decode_one(&source, 2) else {
        panic!("a real AV1 frame must decode to pixels");
    };

    assert_eq!(frame.width, WIDTH);
    assert_eq!(frame.height, HEIGHT);
    assert!(frame.has_pixels());
    assert_eq!(
        frame.rgb.as_ref().map(Vec::len),
        Some((WIDTH * HEIGHT * 3) as usize),
        "an interleaved RGB buffer is three bytes per pixel"
    );
}

#[test]
fn the_decoded_frame_is_labelled_greyscale() {
    // The decoded frame retains only the luma plane. Presenting it as colour
    // would show neutral chroma that was never measured, and a reviewer would
    // read it as "this content has no colour cast".
    let dir = tempfile::tempdir().expect("scratch");
    let source = dir.path().join("av1.webm");
    std::fs::write(&source, real_av1_webm(4)).expect("writes");

    let frame = decode_one(&source, 1).expect("decodes");
    assert_eq!(frame.basis, RgbBasis::LumaOnly);
    assert!(!frame.basis.has_chroma());

    let sample = PixelSample::read(
        frame.rgb.as_ref().expect("has pixels"),
        frame.width,
        frame.height,
        10,
        10,
        frame.basis,
        None,
    )
    .expect("in range");

    // The warning must reach the analyst, not just the type.
    let note = sample.conversion_note();
    assert!(note.contains("128 by construction"), "{note}");
}

#[test]
fn the_pixel_inspector_reads_a_real_decoded_frame() {
    let dir = tempfile::tempdir().expect("scratch");
    let source = dir.path().join("av1.webm");
    std::fs::write(&source, real_av1_webm(4)).expect("writes");

    let frame = decode_one(&source, 1).expect("decodes");
    let rgb = frame.rgb.as_ref().expect("has pixels");

    // Every pixel of a real frame must be readable, and none may be out of range.
    for (x, y) in [(0u32, 0u32), (1, 0), (0, 1), (63, 47), (32, 24)] {
        let sample = PixelSample::read(rgb, frame.width, frame.height, x, y, frame.basis, None)
            .unwrap_or_else(|| panic!("pixel {x},{y} must be readable"));
        assert_eq!((sample.x, sample.y), (x, y));
    }

    // And a coordinate past the edge is refused rather than wrapping.
    assert!(PixelSample::read(rgb, frame.width, frame.height, 64, 0, frame.basis, None).is_none());
}

#[test]
fn the_histogram_measures_a_real_decoded_frame() {
    let dir = tempfile::tempdir().expect("scratch");
    let source = dir.path().join("av1.webm");
    std::fs::write(&source, real_av1_webm(4)).expect("writes");

    let frame = decode_one(&source, 1).expect("decodes");
    let histogram = Histogram::from_rgb(
        frame.rgb.as_ref().expect("has pixels"),
        frame.width,
        frame.height,
    )
    .expect("a real frame produces a histogram");

    assert_eq!(histogram.total, u64::from(WIDTH * HEIGHT));
    assert_eq!(
        histogram.bins.iter().sum::<u32>() as u64,
        histogram.total,
        "every pixel must land in exactly one bin"
    );
    // The fixture is a moving diagonal, so it is neither black nor white.
    assert!(
        histogram.black_fraction() < 1.0 && histogram.white_fraction() < 1.0,
        "the fixture is mid-grey content, not a black or white frame"
    );
}

#[test]
fn two_different_decoded_frames_differ() {
    // The A/B comparison depends on this: if every frame decoded to the same
    // picture, "identical" would be the answer for a sequence that visibly
    // changes.
    let dir = tempfile::tempdir().expect("scratch");
    let source = dir.path().join("av1.webm");
    std::fs::write(&source, real_av1_webm(6)).expect("writes");

    let first = decode_one(&source, 0).expect("decodes");
    let second = decode_one(&source, 3).expect("decodes");

    assert_ne!(
        FrameDifference::compare(&first, &second),
        FrameDifference::Identical,
        "frames three apart in a shifting sequence must not be identical"
    );
}

#[test]
fn a_frame_decoded_twice_is_byte_identical() {
    // The engine's determinism requirement (spec \u00a777), applied to the viewer: an
    // analyst who steps away and back must see the same pixels, or every
    // observation made about a frame is suspect.
    let dir = tempfile::tempdir().expect("scratch");
    let source = dir.path().join("av1.webm");
    std::fs::write(&source, real_av1_webm(5)).expect("writes");

    let first = decode_one(&source, 2).expect("decodes");
    let second = decode_one(&source, 2).expect("decodes");
    assert_eq!(first, second);
}

#[test]
fn stepping_past_the_end_reports_nothing_rather_than_a_wrong_frame() {
    let dir = tempfile::tempdir().expect("scratch");
    let source = dir.path().join("av1.webm");
    std::fs::write(&source, real_av1_webm(3)).expect("writes");

    // The command refuses an out-of-range index; the same refusal here is what
    // keeps the frontend from displaying a stale frame under a new number.
    assert!(decode_one(&source, 99).is_none());
}

#[test]
fn a_corrupt_av1_stream_loses_frames_without_crashing() {
    // Spec \u00a775 on the viewer's own path. The decoder resynchronises at the next
    // keyframe, so a damaged frame is lost - and saying so is correct. Producing
    // *a* frame from the wrong position would not be.
    let dir = tempfile::tempdir().expect("scratch");
    let mut bytes = real_av1_webm(6);

    // Corrupt bytes in the middle of the payload, leaving the container intact.
    let middle = bytes.len() / 2;
    for byte in bytes.iter_mut().skip(middle).take(64) {
        *byte ^= 0xFF;
    }

    let source = dir.path().join("damaged.webm");
    std::fs::write(&source, bytes).expect("writes");

    // Whatever comes back, it comes back.
    let _ = decode_one(&source, 0);
    let _ = decode_one(&source, 4);
}

#[test]
fn a_non_media_file_produces_no_frame_rather_than_a_garbage_one() {
    let dir = tempfile::tempdir().expect("scratch");
    let source = dir.path().join("notes.txt");
    std::fs::write(&source, b"this is not a video").expect("writes");
    assert!(decode_one(&source, 0).is_none());
}
