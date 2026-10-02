//! Round-trip tests: a real encoded stream is decoded back to PCM.
//!
//! The fixtures are produced by the foundation's own encoders rather than
//! checked in as binaries. That keeps the repository free of opaque media, makes
//! every fixture reproducible on any machine (spec §77), and means the decode
//! path is tested against a genuinely compressed, genuinely Ogg-wrapped stream
//! rather than a hand-written guess at one.

use tpt_app_media_forensics_audio::{
    decode_audio, is_audio_decodable, level_stats, AudioDecode, AudioDecodeLimits,
};
use tpt_av_cadence_core::Encoder as _;

/// Builds a deterministic test tone: a 440 Hz sine, 48 kHz mono.
///
/// Fixed sample values make the assertions exact and the fixture byte-identical
/// on every machine.
fn tone(frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|i| (std::f64::consts::TAU * 440.0 * (i as f64) / 48_000.0).sin() as f32)
        .collect()
}

/// Reads an encoded stream back, asserting the encoder wrote something.
fn read_back(path: &std::path::Path) -> Vec<u8> {
    let bytes = std::fs::read(path).expect("reads the encoded stream");
    assert!(!bytes.is_empty(), "the encoder produced no bytes");
    bytes
}

/// Encodes `pcm` into an Ogg Opus stream using the foundation encoder.
///
/// The sink is a real temporary file rather than an in-memory buffer: the
/// foundation encoders take a `Write`, and a file is both a `Write` and
/// somewhere to read the finished stream back from.
fn encode_opus(pcm: &[f32]) -> Vec<u8> {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("out.opus");
    let file = std::fs::File::create(&path).expect("creates sink");

    let mut encoder =
        tpt_av_cadence_opus::OggOpusEncoder::new(file, 48_000, 1, 64_000).expect("opens encoder");
    encoder.encode(pcm).expect("encodes");
    encoder.finish().expect("finishes");

    read_back(&path)
}

/// Encodes `pcm` into an Ogg Vorbis stream using the foundation encoder.
fn encode_vorbis(pcm: &[f32]) -> Vec<u8> {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("out.ogg");
    let file = std::fs::File::create(&path).expect("creates sink");

    let mut encoder =
        tpt_av_cadence_vorbis::VorbisEncoder::new(file, 48_000, 1, 0.5).expect("opens encoder");
    encoder.encode(pcm).expect("encodes");
    encoder.finish().expect("finishes");

    read_back(&path)
}

/// Asserts that a decoded stream carries real audio, not silence.
fn assert_carries_the_tone(pcm: &[f32]) {
    // An all-zero decode would satisfy every structural assertion around it
    // while measuring nothing at all.
    let stats = level_stats(pcm);
    assert!(
        stats.rms > 0.1,
        "decoded RMS {} is too quiet to be the encoded tone",
        stats.rms
    );
}

#[test]
fn an_opus_stream_decodes_to_real_pcm() {
    let bytes = encode_opus(&tone(48_000));
    assert!(
        bytes.starts_with(b"OggS"),
        "the fixture must be a real Ogg stream"
    );

    let decoded = decode_audio(bytes, "Opus", AudioDecodeLimits::new(1)).expect("decodes");

    assert_eq!(decoded.channels, 1);
    assert_eq!(decoded.sample_rate, 48_000);
    assert!(
        !decoded.truncated,
        "a one-second file is well inside the cap"
    );
    assert!(
        decoded.frame_count() > 40_000,
        "expected roughly a second of audio, got {} frames",
        decoded.frame_count()
    );
    assert_carries_the_tone(&decoded.pcm);

    // A lossy codec legitimately overshoots full scale on decode: ringing
    // around a full-scale tone reconstructs to slightly more than 1.0. That is
    // a property of the codec, not a defect in the decode path, and it is why
    // `amplitude_to_dbfs` is defined over amplitudes above 1.0 too.
    //
    // This bound is deliberately loose: its job is to catch a decode that
    // produced the wrong stream or garbage — which would show peaks in the
    // hundreds — without asserting that lossless reconstruction happened.
    assert!(
        level_stats(&decoded.pcm).peak < 1.5,
        "decoded peak is implausible for the encoded tone"
    );
}

#[test]
fn a_vorbis_stream_decodes_to_real_pcm() {
    let bytes = encode_vorbis(&tone(48_000));
    assert!(
        bytes.starts_with(b"OggS"),
        "the fixture must be a real Ogg stream"
    );

    let decoded = decode_audio(bytes, "vorbis", AudioDecodeLimits::new(1)).expect("decodes");

    assert_eq!(decoded.channels, 1);
    assert_eq!(decoded.sample_rate, 48_000);
    assert!(!decoded.truncated);
    assert!(
        decoded.frame_count() > 40_000,
        "expected roughly a second of audio, got {} frames",
        decoded.frame_count()
    );
    assert_carries_the_tone(&decoded.pcm);
}

#[test]
fn decoding_is_deterministic() {
    // Spec §77: the same bytes must decode to the same PCM every time.
    let bytes = encode_vorbis(&tone(24_000));
    let first = decode_audio(bytes.clone(), "vorbis", AudioDecodeLimits::new(1)).expect("decodes");
    let second = decode_audio(bytes, "vorbis", AudioDecodeLimits::new(1)).expect("decodes");
    assert_eq!(first, second);
}

#[test]
fn a_frame_cap_is_reported_rather_than_hidden() {
    // A cap far below the stream's length must truncate *and say so*. Silently
    // returning a prefix would let a report describe a fragment as the track.
    let bytes = encode_vorbis(&tone(48_000));
    let capped = AudioDecodeLimits {
        max_frames: 4_000,
        channels: 1,
    };
    let decoded = decode_audio(bytes, "vorbis", capped).expect("decodes");

    assert!(decoded.truncated, "the cap must be reported");
    assert_eq!(decoded.frame_count(), 4_000, "the cap must be respected");
}

#[test]
fn a_truncated_opus_stream_does_not_panic() {
    // Spec §75: a damaged file is evidence, not a crash. Every prefix of a real
    // stream is a plausible damaged file.
    let bytes = encode_opus(&tone(24_000));
    for len in (0..bytes.len()).step_by(97) {
        // Either outcome is acceptable; not panicking is the point.
        let decoded = decode_audio(bytes[..len].to_vec(), "Opus", AudioDecodeLimits::new(1));
        if let Ok(decoded) = decoded {
            assert!(decoded.channels > 0);
        }
    }
}

#[test]
fn a_truncated_vorbis_stream_does_not_panic() {
    let bytes = encode_vorbis(&tone(24_000));
    for len in (0..bytes.len()).step_by(97) {
        let _ = decode_audio(bytes[..len].to_vec(), "vorbis", AudioDecodeLimits::new(1));
    }
}

#[test]
fn an_opus_stream_is_not_accepted_as_vorbis() {
    // Cross-codec misidentification must be refused rather than producing a
    // confident, meaningless measurement.
    let bytes = encode_opus(&tone(24_000));
    let result = decode_audio(bytes, "vorbis", AudioDecodeLimits::new(1));
    assert!(result.is_err(), "an Opus stream must not decode as Vorbis");
}

#[test]
fn the_decoded_channel_count_comes_from_the_stream() {
    // A caller that guesses 2 channels for a mono stream must not get a
    // two-channel result: the frame count is derived by dividing by channels,
    // so a wrong value silently rescales every duration downstream.
    let bytes = encode_vorbis(&tone(24_000));
    let decoded: AudioDecode =
        decode_audio(bytes, "vorbis", AudioDecodeLimits::new(2)).expect("decodes");
    assert_eq!(decoded.channels, 1, "the stream is mono");
    assert!(is_audio_decodable("vorbis"));
}
