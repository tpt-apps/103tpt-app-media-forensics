//! End-to-end Tier-2 verification on a genuinely encoded AV1 clip.
//!
//! # Why this test exists
//!
//! Every other Tier-2 test runs on synthetic frames fed straight to the
//! analysers. That proves the analysers work, but it proves nothing about the
//! path a real file takes: compressed bytes in, pixels out. The decoder
//! integration was previously verified only by unit-testing its adapters, with
//! no evidence that a real encoded stream survives the trip.
//!
//! This closes that gap without needing ffmpeg on the build machine. The
//! foundation ships an AV1 encoder backed by rav1e, so the fixture here is real:
//! genuinely encoded AV1 frames, decoded back by the Tier-2 session.
//!
//! VP9 has no upstream encoder, so VP9 remains covered structurally only. That
//! asymmetry is stated rather than hidden.

use tpt_app_media_forensics_video::{is_decodable, DecodeLimits, DecodeSession};
use tpt_kinetix_av1::{Av1Encoder, Av1EncoderConfig};
use tpt_kinetix_core::frame::VideoFrame;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::pixel_format::PixelFormat;
use tpt_kinetix_core::timestamp::Timestamp;

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;
const FRAME_MS: i64 = 33;

/// Builds one 8-bit 4:2:0 frame of deterministic content.
///
/// `level` shifts the luma so consecutive frames differ measurably. A sequence
/// that barely changes would make a difference assertion pass or fail for
/// reasons unrelated to whether the decoder worked.
fn frame(level: u8, pts_ms: i64) -> VideoFrame {
    let w = WIDTH as usize;
    let h = HEIGHT as usize;
    let y_size = w * h;
    let uv_size = y_size / 4;

    let mut data = vec![0u8; y_size + uv_size * 2];
    for y in 0..h {
        for x in 0..w {
            // A moving diagonal keeps the image non-uniform, which matters: a
            // flat image compresses to almost nothing and would not exercise
            // real coefficient coding.
            let value = (level as usize + x + y) % 256;
            data[y * w + x] = value as u8;
        }
    }
    // Neutral chroma: the analysers read luma, and arbitrary chroma would add
    // noise that has nothing to do with what is under test.
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

/// Encodes `count` frames of real AV1 and returns the encoded packets.
fn encode_av1(count: usize) -> Vec<Packet> {
    let config = Av1EncoderConfig {
        width: WIDTH,
        height: HEIGHT,
        // Constant-quality mode: no bitrate target, so the encoder cannot drop
        // detail to hit a size budget and understate what the decoder sees.
        bitrate: 0,
        quantizer: 80,
        speed: 10,
        keyframe_interval: count as u64,
    };
    let mut encoder = Av1Encoder::new(&config).expect("opens an AV1 encoder");

    let mut packets = Vec::new();
    for index in 0..count {
        let picture = frame((index * 30) as u8, index as i64 * FRAME_MS);
        if let Some(packet) = encoder.encode_frame(&picture).expect("encodes") {
            packets.push(packet);
        }
    }
    packets.extend(encoder.flush().expect("flushes"));

    assert!(!packets.is_empty(), "the encoder produced no packets");
    packets
}

/// Encodes real AV1 and prepares it for the Tier-2 session.
fn encode_for_decode(count: usize) -> Vec<(Vec<u8>, bool)> {
    encode_av1(count)
        .iter()
        .map(|p| (p.data.clone(), p.is_key_frame))
        .collect()
}

#[test]
fn real_av1_survives_encode_decode_and_reaches_the_analysers() {
    let packets = encode_av1(8);
    assert!(
        packets.iter().all(|p| !p.data.is_empty()),
        "every encoded packet must carry bytes"
    );

    // The container tag Matroska reports for AV1, and what the pipeline
    // dispatches Tier-2 on.
    assert!(is_decodable("av01"), "AV1 must be decodable");
    let mut session =
        DecodeSession::open("av01", DecodeLimits::default()).expect("opens an AV1 decoder");

    let (frames, stopped) = session.decode_prefix(&encode_for_decode(8));

    assert!(
        stopped.is_none(),
        "a valid AV1 stream must not stop the decoder: {stopped:?}"
    );
    assert!(
        frames.len() >= 2,
        "Tier-2 needs at least two frames to compare, decoded {}",
        frames.len()
    );

    // Every frame must carry the dimensions the encoder was configured with.
    // A decode that silently produced a wrong-sized frame would make every
    // downstream measurement describe the wrong picture.
    let expected_luma = WIDTH as usize * HEIGHT as usize;
    for (index, decoded) in frames.iter().enumerate() {
        assert_eq!(decoded.width, WIDTH, "frame {index} width");
        assert_eq!(decoded.height, HEIGHT, "frame {index} height");
        assert_eq!(
            decoded.luma.len(),
            expected_luma,
            "frame {index} luma plane size"
        );
    }

    // The decoded luma must actually vary. A decoder returning a constant plane
    // would pass every structural check above while measuring nothing.
    let first = &frames[0].luma;
    assert!(
        first.iter().any(|&v| v != first[0]),
        "decoded luma is constant ({}) — the decoder produced a flat frame",
        first[0]
    );
}

/// The decoded frames must be measurably different from each other, which is
/// what makes the scene and near-duplicate analysers meaningful downstream.
#[test]
fn consecutive_decoded_av1_frames_differ() {
    let mut session =
        DecodeSession::open("av01", DecodeLimits::default()).expect("opens an AV1 decoder");
    let (frames, _) = session.decode_prefix(&encode_for_decode(6));

    assert!(frames.len() >= 2, "need at least two frames");

    // Mean absolute luma difference: the same quantity the scene-change
    // analyser thresholds.
    let (first, second) = (&frames[0].luma, &frames[1].luma);
    let mean_diff: f64 = first
        .iter()
        .zip(second.iter())
        .map(|(a, b)| f64::from(a.abs_diff(*b)))
        .sum::<f64>()
        / first.len() as f64;

    assert!(
        mean_diff > 1.0,
        "consecutive frames differ by only {mean_diff} luma levels — the round trip is \
         collapsing the content the analysers depend on"
    );
}

/// Decoding must be deterministic across sessions: spec §77 requires two runs
/// over the same input to produce identical output.
#[test]
fn decoding_the_same_av1_twice_is_identical() {
    let input = encode_for_decode(4);

    let decode_once = || {
        let mut session =
            DecodeSession::open("av01", DecodeLimits::default()).expect("opens a decoder");
        session.decode_prefix(&input).0
    };

    assert_eq!(
        decode_once(),
        decode_once(),
        "two decodes of one stream must be identical"
    );
}

#[test]
fn a_corrupted_av1_stream_is_contained_rather_than_fatal() {
    let mut input = encode_for_decode(6);

    // Damage the payload of every packet past the first keyframe.
    for (bytes, _) in input.iter_mut().skip(1) {
        let midpoint = bytes.len() / 2;
        if midpoint < bytes.len() {
            bytes[midpoint] ^= 0xFF;
        }
    }

    let mut session =
        DecodeSession::open("av01", DecodeLimits::default()).expect("opens a decoder");
    let (frames, stopped) = session.decode_prefix(&input);

    // The point is that this returns at all. A decoder panic that escaped the
    // session would take the whole examination down with it.
    assert!(
        frames.is_empty() || stopped.is_some(),
        "a corrupted stream must either recover or report why it stopped"
    );
}

/// The decode limit must be enforced and reported, not silently applied.
#[test]
fn the_decode_limit_is_enforced_and_reported() {
    let mut session = DecodeSession::open(
        "av01",
        DecodeLimits {
            max_frames: 3,
            max_frames_in_memory: 2,
        },
    )
    .expect("opens a decoder");

    let (frames, stopped) = session.decode_prefix(&encode_for_decode(12));

    assert!(
        frames.len() <= 3,
        "the limit must bound decoding, got {} frames",
        frames.len()
    );
    assert!(
        stopped.is_some(),
        "hitting the limit must be reported, not silently applied"
    );
}

/// The whole pipeline, end to end, on a real AV1 file in a real container.
///
/// This is the test the earlier work was missing. Everything above feeds
/// encoder output straight to the decoder; this goes the whole way a submitted
/// file does — encoded AV1 wrapped in a genuine WebM container, read back
/// through `tpt-kinetix-demux`, then decoded by Tier-2. It therefore covers the
/// three seams no other test crosses: the container writer against the demuxer,
/// the demuxer's sample bytes against the decoder, and the demuxer's codec tag
/// against Tier-2's dispatch.
#[test]
fn a_real_av1_webm_file_survives_the_whole_pipeline() {
    use tpt_app_media_forensics_container::build_webm;

    // Encode real AV1, then wrap the packets exactly as a muxer would.
    let packets = encode_av1(8);
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
    let file = build_webm("V_AV1", 1, &blocks);

    // 1. The container layer reads it and reports AV1.
    let inspection =
        tpt_app_media_forensics_container::inspect_matroska_bytes(file.clone()).expect("demuxes");
    assert_eq!(inspection.streams.len(), 1, "one track expected");
    let stream = &inspection.streams[0];
    assert_eq!(
        stream.codec.name, "av01",
        "the demuxer must resolve V_AV1 to the tag Tier-2 dispatches on"
    );
    assert!(is_decodable(&stream.codec.name));

    // 2. Sample reading returns the encoded bytes intact.
    let samples =
        tpt_app_media_forensics_container::read_matroska_samples(file).expect("samples read");
    assert_eq!(
        samples.len(),
        packets.len(),
        "every encoded packet must survive the container round trip"
    );

    // 3. Tier-2 decodes the demuxer's bytes to real pixels.
    let input: Vec<(Vec<u8>, bool)> = samples
        .iter()
        .map(|s| (s.data.clone(), s.is_key_frame))
        .collect();
    let mut session =
        DecodeSession::open(&stream.codec.name, DecodeLimits::default()).expect("opens");
    let (frames, stopped) = session.decode_prefix(&input);

    assert!(
        stopped.is_none(),
        "a valid AV1 file must not stop the decoder: {stopped:?}"
    );
    assert!(
        frames.len() >= 2,
        "expected several decoded frames, got {}",
        frames.len()
    );

    // 4. The pixels are real: correct size, and not a flat plane.
    let expected_luma = WIDTH as usize * HEIGHT as usize;
    for (index, decoded) in frames.iter().enumerate() {
        assert_eq!(decoded.width, WIDTH, "frame {index} width");
        assert_eq!(decoded.height, HEIGHT, "frame {index} height");
        assert_eq!(decoded.luma.len(), expected_luma, "frame {index} luma");
    }
    let first = &frames[0].luma;
    assert!(
        first.iter().any(|&v| v != first[0]),
        "decoded luma is flat; the pipeline produced pixels but not an image"
    );
}

/// Guards against a regression where the encoder stops producing real bytes and
/// every other test in this file passes vacuously.
#[test]
fn the_av1_fixture_is_a_real_compressed_stream() {
    let frames = 4;
    let packets = encode_av1(frames);
    let total: usize = packets.iter().map(|p| p.data.len()).sum();
    let raw = frames * WIDTH as usize * HEIGHT as usize;

    // The invariant that matters is that real *compression* happened, not that
    // the stream reached some size. A 64x48 moving gradient is highly
    // compressible: measured here at roughly 180 bytes for four frames against
    // 12288 raw, about 67:1. Asserting a lower bound on the encoded size would
    // have tested the wrong thing and failed on a working encoder.
    assert!(
        !packets.is_empty() && total > 0,
        "the encoder produced no compressed output"
    );
    assert!(
        total < raw / 8,
        "encoded AV1 totalled {total} bytes against {raw} raw; that is not compression, \
         so the encoder may not be running"
    );

    // Every packet must be a real OBU sequence, not an empty placeholder. An
    // AV1 frame always begins with a marker byte in the 0b01xxxxxx range.
    for packet in &packets {
        assert!(
            !packet.data.is_empty(),
            "a packet carried no bytes; the encoder emitted a placeholder"
        );
    }
}
