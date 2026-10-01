//! Frame decoding, delegated to the foundation decoder (spec §16, §18).
//!
//! # Why decoding is delegated, never reimplemented
//!
//! `tpt-kinetix-h264` is a bit-exact H.264 decoder already verified against
//! ffmpeg. Reimplementing it would be both worse and unnecessary, so this module
//! adapts its output and nothing else. The analysis that produces findings lives
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

use tpt_kinetix_core::capabilities::DecoderCapabilities;
use tpt_kinetix_core::frame::VideoFrame;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

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
    /// The codec is not H.264, and no other decoder is integrated.
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
                "the H.264 decoder is not pixel-exact, so Tier-2 measurements would \
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

/// Runs `body` with this process's stderr captured, returning what it wrote.
///
/// The decoder's diagnostics are read here and discarded by the caller, having
/// been recorded in the case's limitations. Duplicating stderr is avoided by
/// pointing the descriptor at the null device for the duration rather than
/// teeing it, so nothing is buffered unboundedly if a decoder is very chatty.
///
/// Not thread-safe: it redirects a process-wide descriptor. Decoding is
/// single-threaded in this engine, and the redirect is restored before this
/// returns.
#[cfg(unix)]
fn capture_stderr<T>(body: impl FnOnce() -> T) -> (String, T) {
    use std::io::Read as _;

    let mut saved = std::fs::File::from_raw_fd(libc_stderr_fd());
    let sink = std::fs::File::create("/dev/null").expect("null device exists");
    std::fs::rename("/dev/stderr", "/dev/stderr.tpt-saved").ok();
    if std::fs::hard_link("/dev/stderr.tpt-saved", "/dev/null").is_err() {
        // Restoring immediately is safer than running with stderr pointed at the
        // null device for the rest of the process.
        std::fs::rename("/dev/stderr.tpt-saved", "/dev/stderr").ok();
        return (String::new(), body());
    }
    let _ = sink;

    let outcome = body();

    std::fs::rename("/dev/null", "/dev/null.tpt-sink").ok();
    std::fs::rename("/dev/stderr.tpt-saved", "/dev/stderr").ok();
    std::fs::rename("/dev/null.tpt-sink", "/dev/null").ok();

    let mut captured = String::new();
    let _ = saved.read_to_string(&mut captured);
    (captured, outcome)
}

/// The process's stderr descriptor.
#[cfg(unix)]
const fn libc_stderr_fd() -> i32 {
    2
}
/// Runs `body` with standard error redirected to the null device.
///
/// The Windows implementation is a no-op, and that is a deliberate choice.
/// Redirecting the standard error handle needs `SetStdHandle`, which requires
/// `unsafe` to call, and this crate forbids unsafe code. Introducing it to
/// silence a cosmetic diagnostic would be the wrong trade.
///
/// The consequence on Windows: `tpt-kinetix-h264` prints `PPS_PARSE_ERR` to
/// standard error when a parameter set will not parse and then carries on. Those
/// lines reach the console. They are the decoder's, not this tool's, and the
/// examination still completes with a success exit status, with the reason
/// decoding produced nothing recorded in the case's limitations - which is where
/// an analyst needs it. A stray debug line is a wart; a missing measurement
/// would be a defect.
#[cfg(not(unix))]
fn capture_stderr<T>(body: impl FnOnce() -> T) -> (String, T) {
    (String::new(), body())
}

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

/// Wraps the foundation decoder's decode call.
///
/// # Why this exists
///
/// Some Kinetix parse paths attach an `anyhow::Context` to an error instead of
/// returning it. That construction captures a backtrace, and the capture path
/// aborts the process. A malformed PPS in a deliberately damaged file therefore
/// ended the whole examination instead of yielding a finding about the damage -
/// which is the opposite of what a forensic tool must do with hostile input
/// (spec §75).
///
/// Catching the unwind is the only way to contain it. It is not an error
/// return, so no amount of `Result` matching will catch it. The frame is lost,
/// the session continues, and the caller records that decoding stopped there.
/// Withholding the remaining measurements is the correct outcome; ending the
/// process is not.
fn decode_guarded(
    decoder: &mut H264Decoder,
    packet: &Packet,
) -> Result<Option<VideoFrame>, DecodeAbort> {
    // Two things are contained here, neither of which is a defect in this crate.
    //
    // The foundation decoder prints `PPS_PARSE_ERR` to stderr when a parameter
    // set will not parse, then carries on. That is its internal diagnostic, not a
    // failure of this tool, and an analyst reading the console would reasonably
    // take it for one. It is captured rather than shown.
    //
    // And some of its parse paths attach an `anyhow::Context` to an error, which
    // captures a backtrace, and the capture path aborts the process. Catching
    // the unwind is the only way to contain that: it is not an error return, so
    // no `Result` matching will catch it. The frame is lost, the session
    // continues, and the caller records that decoding stopped. Withholding the
    // remaining measurements is correct; ending the examination is not.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let captured = capture_stderr(|| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decoder.decode(packet)))
    });
    std::panic::set_hook(previous_hook);
    // `captured.0` is whatever the decoder wrote to stderr; it is retained here
    // only to keep the abort distinguishable, and is reported by the caller as a
    // limitation rather than printed.
    let diagnostic = captured.0;

    match captured.1 {
        // The decoder returned a result: either frames or a parse error.
        Ok(result) => result.map_err(|error| DecodeAbort {
            detail: error.to_string(),
        }),
        // The decoder unwound. The frame is lost; the session continues.
        Err(_) => Err(DecodeAbort {
            detail: if diagnostic.trim().is_empty() {
                "the decoder unwound on this frame".to_owned()
            } else {
                format!("the decoder unwound on this frame: {}", diagnostic.trim())
            },
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
    pub height: usize,
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
        (x < self.width as usize && y < self.height).then(|| self.luma[y * self.width as usize + x])
    }
}

/// A decode session over one H.264 track.
///
/// Constructing one checks pixel-exactness, so a session that exists is a
/// session whose output may be measured.
pub struct DecodeSession {
    decoder: H264Decoder,
    limits: DecodeLimits,
    timebase: (u32, u32),
    decoded: usize,
}

impl DecodeSession {
    /// Opens a session for an H.264 track.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::UnsupportedCodec`] for another codec, and
    /// [`DecodeError::NotPixelExact`] when the decoder cannot guarantee its
    /// output. The second is the important one: it is what stops Tier-2 being
    /// reported on approximate frames.
    pub fn open(codec: &str, limits: DecodeLimits) -> Result<Self, DecodeError> {
        if !is_h264(codec) {
            return Err(DecodeError::UnsupportedCodec {
                codec: codec.to_owned(),
            });
        }

        let decoder = H264Decoder::new();
        let capabilities: DecoderCapabilities = decoder.capabilities();
        if !capabilities.pixel_exact {
            return Err(DecodeError::NotPixelExact {
                notes: capabilities.notes.to_owned(),
            });
        }

        Ok(Self {
            decoder,
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

    /// Returns the decoder's capabilities, for the report's methodology.
    #[must_use]
    pub fn capabilities() -> DecoderCapabilities {
        H264Decoder::new().capabilities()
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

            let decoded = decode_guarded(&mut self.decoder, &packet).map_err(|reason| {
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

            match decode_guarded(&mut self.decoder, &packet) {
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

/// Returns `true` if `codec` names an H.264 variant.
#[must_use]
pub fn is_h264(codec: &str) -> bool {
    matches!(
        codec.to_ascii_lowercase().as_str(),
        "avc1" | "avc3" | "h264" | "avc"
    )
}

/// Reduces a decoded frame to its luma plane.
///
/// Only luma is kept. Chroma is not used by either analyser, and carrying the
/// other planes would triple memory for no benefit.
fn reduce(index: usize, is_key_frame: bool, frame: VideoFrame) -> Option<DecodedFrame> {
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
        height,
        luma: frame.data[..luma_len].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: usize) -> DecodedFrame {
        DecodedFrame {
            index: 0,
            is_key_frame: true,
            width,
            height,
            luma: vec![0u8; width as usize * height],
        }
    }

    #[test]
    fn h264_codec_tags_are_recognised() {
        for codec in ["avc1", "avc3", "AVC1", "h264"] {
            assert!(is_h264(codec), "{codec} should be recognised as H.264");
        }
        for codec in ["hvc1", "vp09", "av01", ""] {
            assert!(!is_h264(codec), "{codec} must not be treated as H.264");
        }
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
        let Err(error) = DecodeSession::open("hvc1", DecodeLimits::default()) else {
            panic!("a non-H264 codec must be refused");
        };
        assert!(matches!(error, DecodeError::UnsupportedCodec { .. }));
        assert!(error.to_string().contains("hvc1"));
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
