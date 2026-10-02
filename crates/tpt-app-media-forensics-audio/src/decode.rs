//! Royalty-free audio decoding (spec §19, §21).
//!
//! # Only royalty-free codecs are decoded
//!
//! Opus and Vorbis are decoded. AAC is **not**: it is covered by patent pools in
//! the same way H.264 is, so an AAC track is identified from the container and
//! its declared properties are reported, but no sample is ever decoded from it.
//! This mirrors the rule on the video side (`-video::decode`) — a patent
//! encumbrance is a reason not to ship the decoder, not a reason to pretend the
//! audio was measured.
//!
//! Decoding is **integrated, never implemented**. Both decoders already exist in
//! the foundation and are verified against reference vectors; re-deriving them
//! would be worse and slower.
//!
//! # Decode is bounded
//!
//! A forensic run must terminate on a hostile or simply enormous file, so
//! [`DecodeLimits`] caps the PCM retained. Hitting the cap is reported through
//! [`AudioDecode::truncated`], never silently presented as a complete
//! measurement.
//!
//! # Both codecs arrive in an Ogg container
//!
//! Opus (RFC 7845) and Vorbis I are Ogg logical bitstreams. [`decode`] therefore
//! takes bytes rather than a path, and the caller has already identified the
//! container. Keeping that boundary explicit means a raw Opus packet stream and
//! an Ogg-wrapped one cannot be silently confused.

use std::io::Read;

use tpt_av_cadence_core::{Decoder, FormatReader as _, StreamInfo};

/// Why audio could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioDecodeError {
    /// The codec is patent-encumbered or otherwise not decoded here.
    ///
    /// This is a deliberate policy, not a missing feature, so it is phrased to
    /// say so: "we do not decode this" is a different statement from "we could
    /// not decode this".
    #[error("codec `{0}` is not decoded: only royalty-free Opus and Vorbis are")]
    UnsupportedCodec(String),
    /// The bytes are not a readable Ogg bitstream for the requested codec.
    #[error("cannot open the audio stream: {0}")]
    Open(String),
    /// Decoding failed part-way through.
    #[error("decoding stopped after {frames_decoded} frame(s): {reason}")]
    Decode {
        /// Frames produced before the failure.
        frames_decoded: usize,
        /// What the decoder reported.
        reason: String,
    },
}

/// Bounds on how much audio one decode will produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeLimits {
    /// Maximum PCM frames to retain.
    pub max_frames: usize,
    /// Channel count the caller expects.
    ///
    /// Only used to size the scratch buffer. The channel count that reaches the
    /// caller always comes from the stream's own headers, because a container
    /// describing a different channel count than its bitstream carries is
    /// itself a finding rather than something to paper over.
    pub channels: u16,
}

impl DecodeLimits {
    /// Limits sized for one track of a QC examination.
    ///
    /// Ten minutes at 48 kHz is 28.8 million frames, far more than any level,
    /// silence, or loudness measurement needs and far less than a feature-length
    /// master would produce.
    #[must_use]
    pub fn new(channels: u16) -> Self {
        Self {
            max_frames: 48_000 * 600,
            channels,
        }
    }

    /// Interleaved sample slots one frame occupies.
    #[must_use]
    pub fn slots_per_frame(&self) -> usize {
        usize::from(self.channels.max(1))
    }
}

/// A decoded audio track.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioDecode {
    /// Interleaved PCM in `[-1.0, 1.0]`, exactly as the decoder produced it.
    ///
    /// Not normalised: the measurements downstream need the real relationship
    /// between samples, and a rescale here would destroy it.
    pub pcm: Vec<f32>,
    /// Channel count, as the stream's headers declared it.
    pub channels: u16,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Whether the frame cap was reached before the stream ended.
    ///
    /// `true` means these numbers describe a *prefix* of the track, which a
    /// report must say rather than presenting as the whole track.
    pub truncated: bool,
}

impl AudioDecode {
    /// Number of PCM frames retained.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        if self.channels == 0 {
            return 0;
        }
        self.pcm.len() / usize::from(self.channels)
    }
}

/// Reports whether a codec tag names a royalty-free codec this crate decodes.
///
/// Matching is case-insensitive because containers are inconsistent about the
/// case of their own tags, and `Opus` and `opus` name the same codec.
#[must_use]
pub fn is_decodable(codec: &str) -> bool {
    matches!(
        codec.to_ascii_lowercase().as_str(),
        "opus" | "vorbis" | "a_opus" | "a_vorbis"
    )
}

/// Decodes an Ogg-wrapped royalty-free audio stream to PCM.
///
/// # Errors
///
/// Returns an error when the codec is not one this crate decodes, when the
/// bytes are not a readable stream of that codec, or when decoding fails
/// part-way. A caller that gets an error still knows the file was not measured,
/// which is the point: a silent failure would read as a clean audio track.
pub fn decode(
    bytes: Vec<u8>,
    codec: &str,
    limits: DecodeLimits,
) -> Result<AudioDecode, AudioDecodeError> {
    if !is_decodable(codec) {
        return Err(AudioDecodeError::UnsupportedCodec(codec.to_owned()));
    }

    // The channel count and sample rate that reach the caller always come from
    // the stream's own headers, never from the caller's expectation.
    let source: Box<dyn Read + Send> = Box::new(std::io::Cursor::new(bytes));
    match codec.to_ascii_lowercase().as_str() {
        "vorbis" | "a_vorbis" => {
            let mut decoder = tpt_av_cadence_vorbis::VorbisDecoder::open(source)
                .map_err(|e| AudioDecodeError::Open(e.to_string()))?;
            pump(&mut decoder, limits)
        }
        _ => {
            let mut reader = tpt_av_cadence_opus::OggOpusReader::open(source)
                .map_err(|e| AudioDecodeError::Open(e.to_string()))?;
            pump(reader.decoder(), limits)
        }
    }
}
/// Decodes a sequence of raw Opus packets to PCM.
///
/// This is the path for Opus carried in a **container** rather than a bare Ogg
/// stream. A `.webm` file is Matroska, not Ogg: handing its bytes to an Ogg
/// reader fails with a capture-pattern error, because the two formats share a
/// lineage and nothing else. The packets must therefore be extracted by the
/// container layer first and handed over here.
///
/// Each payload is one access unit, exactly as a demuxer produces it.
///
/// # Errors
///
/// Returns an error when a packet cannot be parsed or decoded. Decoding stops
/// at the first failure and reports how many packets succeeded, so a truncated
/// tail yields the audio that *was* recoverable alongside the reason it ended.
pub fn decode_opus_packets(
    packets: &[Vec<u8>],
    channels: u16,
    limits: DecodeLimits,
) -> Result<AudioDecode, AudioDecodeError> {
    use tpt_av_cadence_opus::packet::parse_packet;
    use tpt_av_cadence_opus::OpusDecoder;

    if channels == 0 {
        return Err(AudioDecodeError::Open(
            "the stream declares zero channels".to_owned(),
        ));
    }

    let mut decoder = OpusDecoder::new(usize::from(channels))
        .map_err(|e| AudioDecodeError::Open(e.to_string()))?;

    let slots = usize::from(channels);
    let cap = limits.max_frames.saturating_mul(slots);
    let mut pcm: Vec<f32> = Vec::new();

    // Opus decodes at 48 kHz internally; every output sample rate is a
    // resampling of that, and the foundation decoders emit at the stream rate.
    const OPUS_RATE: u32 = 48_000;
    // Largest frame a single Opus packet can carry is 120 ms.
    const MAX_FRAME_SAMPLES: usize = (OPUS_RATE as usize * 120) / 1_000;
    let mut scratch = vec![0.0f32; MAX_FRAME_SAMPLES * slots];

    for (index, payload) in packets.iter().enumerate() {
        if pcm.len() >= cap {
            break;
        }
        let packet = parse_packet(payload).map_err(|e| AudioDecodeError::Decode {
            frames_decoded: index,
            reason: e.to_string(),
        })?;
        let written = decoder
            .decode_packet(&packet, payload, &mut scratch)
            .map_err(|e| AudioDecodeError::Decode {
                frames_decoded: index,
                reason: e.to_string(),
            })?;
        let available = cap.saturating_sub(pcm.len());
        let take = (written * slots).min(available);
        pcm.extend_from_slice(&scratch[..take]);
    }

    let truncated = pcm.len() >= cap;
    Ok(AudioDecode {
        pcm,
        channels,
        sample_rate: OPUS_RATE,
        truncated,
    })
}

/// Runs a decoder until the stream ends or the frame cap is reached.
fn pump(decoder: &mut dyn Decoder, limits: DecodeLimits) -> Result<AudioDecode, AudioDecodeError> {
    let info: StreamInfo = decoder.info().clone();
    let channels = info.channels;
    if channels == 0 {
        return Err(AudioDecodeError::Open(
            "the stream declares zero channels".to_owned(),
        ));
    }

    let slots = usize::from(channels);
    let cap = limits.max_frames.saturating_mul(slots);

    // Bounded chunks, so a long track never needs a scratch buffer the size of
    // the whole file.
    const CHUNK_FRAMES: usize = 4_096;
    let mut buffer = vec![0.0f32; CHUNK_FRAMES * slots];
    let mut pcm: Vec<f32> = Vec::new();
    let mut truncated = false;

    loop {
        if pcm.len() >= cap {
            truncated = true;
            break;
        }
        let written = decoder
            .decode(&mut buffer)
            .map_err(|e| AudioDecodeError::Decode {
                frames_decoded: pcm.len() / slots,
                reason: e.to_string(),
            })?;
        if written == 0 {
            break;
        }
        let available = cap - pcm.len();
        let take = written.saturating_mul(slots).min(available);
        pcm.extend_from_slice(&buffer[..take]);
        if take < written.saturating_mul(slots) {
            truncated = true;
            break;
        }
    }

    Ok(AudioDecode {
        pcm,
        channels,
        sample_rate: info.sample_rate,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_royalty_free_codecs_are_decodable() {
        for codec in ["Opus", "opus", "A_OPUS", "vorbis", "Vorbis", "A_VORBIS"] {
            assert!(is_decodable(codec), "{codec} should be decodable");
        }
        // AAC and MP3 are patent-encumbered and must never be decoded here.
        for codec in ["mp4a", "aac", "AAC", "mp3", ".mp3", "flac", ""] {
            assert!(!is_decodable(codec), "{codec} must not be decodable");
        }
    }

    #[test]
    fn a_patent_encumbered_codec_is_refused_with_a_policy_reason() {
        // The message must distinguish "we chose not to" from "we failed to".
        let error = decode(Vec::new(), "mp4a", DecodeLimits::new(2)).expect_err("refused");
        assert!(matches!(error, AudioDecodeError::UnsupportedCodec(_)));
        let text = error.to_string();
        assert!(text.contains("mp4a"), "{text}");
        assert!(text.contains("royalty-free"), "{text}");
    }

    #[test]
    fn garbage_bytes_are_an_open_error_not_a_panic() {
        // Spec §75: hostile input must be handled, never trusted.
        for bytes in [
            vec![],
            vec![0xFF; 64],
            b"OggS but not really".to_vec(),
            b"RIFF____WAVEfmt ".to_vec(),
        ] {
            for codec in ["opus", "vorbis"] {
                let result = decode(bytes.clone(), codec, DecodeLimits::new(2));
                assert!(
                    result.is_err(),
                    "{codec} should reject {} bytes of garbage",
                    bytes.len()
                );
            }
        }
    }

    #[test]
    fn errors_describe_themselves() {
        // These strings reach the report, so they must name the condition.
        assert!(AudioDecodeError::Open("short header".to_owned())
            .to_string()
            .contains("short header"));
        assert!(AudioDecodeError::Decode {
            frames_decoded: 12,
            reason: "bad packet".to_owned(),
        }
        .to_string()
        .contains("12"));
    }

    #[test]
    fn limits_are_sized_for_a_qc_run() {
        let limits = DecodeLimits::new(2);
        assert!(limits.max_frames > 0);
        assert_eq!(limits.slots_per_frame(), 2);
        // A zero-channel expectation must still yield a usable buffer size
        // rather than a zero-length one that silently decodes nothing.
        assert_eq!(DecodeLimits::new(0).slots_per_frame(), 1);
    }

    #[test]
    fn an_empty_decode_reports_no_frames() {
        let decoded = AudioDecode {
            pcm: Vec::new(),
            channels: 2,
            sample_rate: 48_000,
            truncated: false,
        };
        assert_eq!(decoded.frame_count(), 0);
        // A zero-channel result must report zero rather than divide by zero.
        let none = AudioDecode {
            pcm: vec![0.0; 100],
            channels: 0,
            sample_rate: 48_000,
            truncated: false,
        };
        assert_eq!(none.frame_count(), 0);
    }
}
