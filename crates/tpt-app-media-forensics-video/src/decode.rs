//! Frame decoding, delegated to the foundation decoder (spec §16, §18).
//!
//! # Why decoding is delegated, never reimplemented
//!
//! `tpt-kinetix-vp9` and `tpt-kinetix-av1` are bit-exact decoders already
//! verified against ffmpeg. Reimplementing them would be both worse and
//! unnecessary, so this module adapts their output and nothing else.
//!
//! # Only royalty-free codecs are decoded
//!
//! VP9 and AV1 are the only codecs decoded here. H.264, HEVC and AAC are
//! covered by patent pools, so those tracks are identified from the container
//! and analysed at Tier 1, but never decoded. The analysis that produces findings lives
//! in [`crate::near_duplicate`] and [`crate::scene`] and never sees a decoder.
//!
//! # Pixel-exactness gates everything
//!
//! The decoder reports whether its output is bit-exact with a reference. Tier-2
//! measurements - near-duplicate detection and scene changes - are computed from
//! pixel values, so on a decoder that is not pixel-exact they would be
//! measurements of the decoder rather than of the media. [`DecodeSession`] refuses
//! to produce frames in that case, and the caller records a limitation instead.
//! Withholding a measurement is the correct outcome; a plausible wrong number is
//! not.
//!
//! # Decoding is expensive and therefore bounded
//!
//! Pixel analysis is quadratic in the worst case and linear in memory, so
//! [`DecodeLimits`] caps how many frames a session will decode. Exceeding a limit
//! stops decoding and is reported, rather than silently producing a partial
//! result that reads as complete.

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::capabilities::DecoderCapabilities;
use tpt_kinetix_core::frame::VideoFrame;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::pixel_format::PixelFormat;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_vp9::Vp9Decoder;

/// Bounds on how much work a decode session will do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeLimits {
    /// Maximum frames to decode.
    pub max_frames: usize,
    /// Maximum frames to keep resident at once.
    pub max_frames_in_memory: usize,
}

impl Default for DecodeLimits {
    /// Limits sized for a QC examination on a workstation.
    ///
    /// Full-rate decoding of a feature-length master would take far longer than
    /// an analyst will wait, so Tier-2 runs over a bounded window and says so.
    fn default() -> Self {
        Self {
            max_frames: 2_000,
            max_frames_in_memory: 64,
        }
    }
}

/// Why decoding could not proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The decoder is not pixel-exact, so Tier-2 cannot be measured.
    NotPixelExact {
        /// What the decoder reported.
        notes: String,
    },
    /// The codec has no integrated decoder.
    UnsupportedCodec {
        /// The codec tag as the container recorded it.
        codec: String,
    },
    /// A decode error, described.
    Decode {
        /// Frame index at which it occurred.
        frame: usize,
        /// What went wrong.
        reason: String,
    },
    /// A limit was reached before the stream ended.
    LimitReached {
        /// Which limit.
        limit: &'static str,
        /// Frames decoded before stopping.
        decoded: usize,
    },
    /// The decoder produced a frame with an unusable pixel layout.
    UnusableFrame {
        /// Frame index.
        frame: usize,
        /// Why it could not be used.
        reason: String,
    },
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotPixelExact { notes } => write!(
                f,
                "the decoder is not pixel-exact, so Tier-2 measurements would \
                 describe the decoder rather than the media: {notes}"
            ),
            Self::UnsupportedCodec { codec } => {
                write!(f, "no decoder is integrated for codec `{codec}`")
            }
            Self::Decode { frame, reason } => {
                write!(f, "decoding failed at frame {frame}: {reason}")
            }
            Self::LimitReached { limit, decoded } => {
                write!(f, "{limit} reached after {decoded} frames")
            }
            Self::UnusableFrame { frame, reason } => {
                write!(f, "frame {frame} could not be used: {reason}")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// The abort reason, carried so a caller can report it like any other failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeAbort {
    /// Why the decode did not complete.
    pub detail: String,
}

impl std::fmt::Display for DecodeAbort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the decoder aborted on this frame: {}", self.detail)
    }
}

impl std::error::Error for DecodeAbort {}

/// A decoder for one of the supported royalty-free codecs.
enum Backend {
    Vp9(Box<Vp9Decoder>),
    Av1(Box<Av1Decoder>),
}

impl Backend {
    /// Builds the decoder for `codec`, or `None` if it is not decodable here.
    fn for_codec(codec: &str) -> Option<Self> {
        match codec.to_ascii_lowercase().as_str() {
            "vp09" | "vp9" => Some(Self::Vp9(Box::new(Vp9Decoder::new().with_strict(true)))),
            "av01" | "av1" => Some(Self::Av1(Box::new(Av1Decoder::new().with_strict(true)))),
            _ => None,
        }
    }

    fn capabilities(&self) -> DecoderCapabilities {
        match self {
            Self::Vp9(decoder) => decoder.capabilities(),
            Self::Av1(decoder) => decoder.capabilities(),
        }
    }

    fn decode(&mut self, packet: &Packet) -> Result<Option<VideoFrame>, String> {
        match self {
            Self::Vp9(decoder) => decoder.decode(packet),
            Self::Av1(decoder) => decoder.decode(packet),
        }
        .map_err(|error| error.to_string())
    }
}

/// Wraps the decoder's decode call.
///
/// # Why this exists
///
/// A decoder fed deliberately damaged input may panic rather than return an
/// error. That would end the whole examination instead of yielding a finding
/// about the damage, which is the opposite of what a forensic tool must do with
/// hostile input (spec §75).
///
/// Catching the unwind is the only way to contain it. It is not an error
/// return, so no amount of `Result` matching will catch it. The frame is lost,
/// the session continues, and the caller records that decoding stopped there.
/// Withholding the remaining measurements is the correct outcome; ending the
/// process is not.
fn decode_guarded(
    backend: &mut Backend,
    packet: &Packet,
) -> Result<Option<VideoFrame>, DecodeAbort> {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| backend.decode(packet)));
    std::panic::set_hook(previous_hook);

    match outcome {
        // The decoder returned a result: either a frame or a parse error.
        Ok(result) => result.map_err(|detail| DecodeAbort { detail }),
        // The decoder unwound. The frame is lost; the session continues.
        Err(_) => Err(DecodeAbort {
            detail: "the decoder unwound on this frame".to_owned(),
        }),
    }
}

/// A decoded frame, reduced to what the analysers need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    /// Index within the stream, zero-based.
    pub index: usize,
    /// Whether this frame was a random-access point.
    pub is_key_frame: bool,
    /// Luma plane width in pixels.
    pub width: u32,
    /// Luma plane height in pixels.
    ///
    /// Both dimensions are `u32` to match `VideoFormat` and the Kinetix
    /// `VideoFrame`. They were previously `u32` and `usize` respectively, which
    /// forced every caller to cast one of the two and made it easy to compare a
    /// width against a height.
    pub height: u32,
    /// Luma samples, `width * height` bytes in raster order.
    pub luma: Vec<u8>,
}

impl DecodedFrame {
    /// Returns the luma sample at `(x, y)`, or `None` when out of bounds.
    ///
    /// Bounds-checked because callers index by computed coordinates, and a
    /// decoded frame's dimensions are attacker-controlled.
    #[must_use]
    pub fn luma_at(&self, x: usize, y: usize) -> Option<u8> {
        let w = self.width as usize;
        (x < w && y < self.height as usize).then(|| self.luma[y * w + x])
    }
}

/// A decode session over one VP9 or AV1 track.
///
/// Constructing one checks pixel-exactness, so a session that exists is a
/// session whose output may be measured.
pub struct DecodeSession {
    backend: Backend,
    limits: DecodeLimits,
    timebase: (u32, u32),
    decoded: usize,
}

impl DecodeSession {
    /// Opens a session for a VP9 or AV1 track.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::UnsupportedCodec`] for another codec, and
    /// [`DecodeError::NotPixelExact`] when the decoder cannot guarantee its
    /// output. The second is the important one: it is what stops Tier-2 being
    /// reported on approximate frames.
    pub fn open(codec: &str, limits: DecodeLimits) -> Result<Self, DecodeError> {
        let Some(backend) = Backend::for_codec(codec) else {
            return Err(DecodeError::UnsupportedCodec {
                codec: codec.to_owned(),
            });
        };

        let capabilities: DecoderCapabilities = backend.capabilities();
        if !capabilities.pixel_exact {
            return Err(DecodeError::NotPixelExact {
                notes: capabilities.notes.to_owned(),
            });
        }

        Ok(Self {
            backend,
            limits,
            timebase: (1, 30),
            decoded: 0,
        })
    }

    /// Sets the timebase used for synthetic timestamps.
    #[must_use]
    pub const fn with_timebase(mut self, numerator: u32, denominator: u32) -> Self {
        self.timebase = (numerator, denominator);
        self
    }

    /// Returns the capabilities of the decoder for `codec`, for the report's
    /// methodology, or `None` if the codec is not decoded here.
    #[must_use]
    pub fn capabilities(codec: &str) -> Option<DecoderCapabilities> {
        Backend::for_codec(codec).map(|backend| backend.capabilities())
    }

    /// Decodes every packet in `packets`, returning the usable frames.
    ///
    /// Stops at [`DecodeLimits::max_frames`] and reports that as an error rather
    /// than returning a partial set that reads as complete.
    ///
    /// # Errors
    ///
    /// Returns a [`DecodeError`] if a frame cannot be decoded or is unusable, or
    /// if a limit stops decoding before the stream ends.
    pub fn decode_all(
        &mut self,
        packets: &[(Vec<u8>, bool)],
    ) -> Result<Vec<DecodedFrame>, DecodeError> {
        let mut frames = Vec::new();

        for (index, (data, is_key_frame)) in packets.iter().enumerate() {
            if self.decoded >= self.limits.max_frames {
                return Err(DecodeError::LimitReached {
                    limit: "the frame limit",
                    decoded: self.decoded,
                });
            }

            let packet = Packet {
                pts: Timestamp::new(index as i64, self.timebase),
                dts: Timestamp::new(index as i64, self.timebase),
                data: data.clone(),
                stream_index: 0,
                is_key_frame: *is_key_frame,
            };

            let decoded = decode_guarded(&mut self.backend, &packet).map_err(|reason| {
                DecodeError::Decode {
                    frame: index,
                    reason: reason.detail,
                }
            })?;

            let Some(frame) = decoded else {
                // The decoder buffers frames for reordering, so `None` between
                // packets is normal rather than an error.
                continue;
            };

            self.decoded += 1;
            if let Some(reduced) = reduce(index, *is_key_frame, frame) {
                frames.push(reduced);
                if frames.len() > self.limits.max_frames_in_memory {
                    return Err(DecodeError::LimitReached {
                        limit: "the in-memory frame limit",
                        decoded: frames.len(),
                    });
                }
            }
        }

        Ok(frames)
    }

    /// Decodes the packets and returns whatever succeeded, plus why it stopped.
    ///
    /// Unlike [`Self::decode_all`], a limit or a decode failure ends the session
    /// without discarding the frames already recovered. This is the entry point
    /// an examination uses: a partially decoded track still yields findings about
    /// the frames that were read, as long as the report says how many there were.
    ///
    /// Returns the frames and, if the run was cut short, the reason.
    pub fn decode_prefix(
        &mut self,
        packets: &[(Vec<u8>, bool)],
    ) -> (Vec<DecodedFrame>, Option<DecodeError>) {
        let mut frames = Vec::new();

        for (index, (data, is_key_frame)) in packets.iter().enumerate() {
            if self.decoded >= self.limits.max_frames {
                return (
                    frames,
                    Some(DecodeError::LimitReached {
                        limit: "the frame limit",
                        decoded: self.decoded,
                    }),
                );
            }

            let packet = Packet {
                pts: Timestamp::new(index as i64, self.timebase),
                dts: Timestamp::new(index as i64, self.timebase),
                data: data.clone(),
                stream_index: 0,
                is_key_frame: *is_key_frame,
            };

            match decode_guarded(&mut self.backend, &packet) {
                Ok(Some(frame)) => {
                    self.decoded += 1;
                    if let Some(reduced) = reduce(index, *is_key_frame, frame) {
                        frames.push(reduced);
                        if frames.len() >= self.limits.max_frames_in_memory {
                            let decoded = frames.len();
                            return (
                                frames,
                                Some(DecodeError::LimitReached {
                                    limit: "the in-memory frame limit",
                                    decoded,
                                }),
                            );
                        }
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    return (
                        frames,
                        Some(DecodeError::Decode {
                            frame: index,
                            reason: error.to_string(),
                        }),
                    );
                }
            }
        }

        (frames, None)
    }
}

/// Returns `true` if `codec` names a codec this engine can decode (VP9, AV1).
///
/// Patent-encumbered codecs such as H.264 and HEVC are deliberately absent.
#[must_use]
pub fn is_decodable(codec: &str) -> bool {
    Backend::for_codec(codec).is_some()
}

/// Reduces a decoded frame to its luma plane.
///
/// Only luma is kept. Chroma is not used by either analyser, and carrying the
/// other planes would triple memory for no benefit.
fn reduce(index: usize, is_key_frame: bool, frame: VideoFrame) -> Option<DecodedFrame> {
    // The luma plane leads the buffer only in 8-bit planar layouts. Anything
    // else (10-bit words, packed RGB) would be misread as 8-bit luma.
    if !matches!(
        frame.pixel_format,
        PixelFormat::Yuv420p | PixelFormat::Yuv422p | PixelFormat::Yuv444p | PixelFormat::Gray
    ) {
        return None;
    }

    let width = frame.width as usize;
    let height = frame.height as usize;
    let luma_len = width.checked_mul(height)?;

    // A frame whose buffer is shorter than its own dimensions is not usable,
    // and indexing it would panic.
    if frame.data.len() < luma_len {
        return None;
    }

    Some(DecodedFrame {
        index,
        is_key_frame,
        width: frame.width,
        height: frame.height,
        luma: frame.data[..luma_len].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32) -> DecodedFrame {
        DecodedFrame {
            index: 0,
            is_key_frame: true,
            width,
            height,
            luma: vec![0u8; width as usize * height as usize],
        }
    }

    #[test]
    fn only_royalty_free_codecs_are_decodable() {
        for codec in ["vp09", "VP09", "vp9", "av01", "av1"] {
            assert!(is_decodable(codec), "{codec} should be decodable");
        }
        for codec in ["avc1", "avc3", "h264", "hvc1", "mp4a", ""] {
            assert!(!is_decodable(codec), "{codec} must not be decodable");
        }
    }

    #[test]
    fn decoders_are_pixel_exact() {
        for codec in ["vp09", "av01"] {
            let caps = DecodeSession::capabilities(codec).expect("decodable");
            assert!(caps.pixel_exact, "{codec} must be pixel-exact");
        }
        assert!(DecodeSession::capabilities("avc1").is_none());
    }

    #[test]
    fn luma_at_is_bounds_checked() {
        let f = frame(4, 4);
        assert_eq!(f.luma_at(0, 0), Some(0));
        assert_eq!(f.luma_at(3, 3), Some(0));
        assert_eq!(f.luma_at(4, 0), None);
        assert_eq!(f.luma_at(0, 4), None);
    }

    #[test]
    fn an_unsupported_codec_is_refused() {
        // Matched rather than `expect_err`, because `DecodeSession` holds a
        // decoder that is not `Debug` and has no reason to be.
        let Err(error) = DecodeSession::open("avc1", DecodeLimits::default()) else {
            panic!("a patent-encumbered codec must be refused");
        };
        assert!(matches!(error, DecodeError::UnsupportedCodec { .. }));
        assert!(error.to_string().contains("avc1"));
    }

    #[test]
    fn errors_describe_themselves() {
        // These strings reach the report, so they must name the condition.
        assert!(DecodeError::NotPixelExact {
            notes: "x".to_owned()
        }
        .to_string()
        .contains("not pixel-exact"));
        assert!(DecodeError::LimitReached {
            limit: "the frame limit",
            decoded: 7
        }
        .to_string()
        .contains("7"));
    }

    #[test]
    fn default_limits_bound_both_dimensions() {
        let limits = DecodeLimits::default();
        assert!(limits.max_frames > 0);
        assert!(limits.max_frames_in_memory <= limits.max_frames);
    }
}
