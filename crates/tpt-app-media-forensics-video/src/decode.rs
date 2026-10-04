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

use tpt_app_media_forensics_model::MediaTime;

use crate::frame::{FrameError, FrameImage};

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

/// A recoverable fault encountered while decoding, recorded rather than fatal.
///
/// # Why this is a type and not a string
///
/// Spec §30 asks the report to say "Analysis completed with 17 recoverable
/// decode errors". Counting is the easy half. What makes the statement worth
/// anything is that the errors can be told apart: a packet the decoder *rejected*
/// is different from a frame whose pixel layout was unusable, and both are
/// different from a run of frames skipped because their references were lost.
/// A free-text string collapses all three into one number an analyst cannot act
/// on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeDamage {
    /// The decoder rejected this packet.
    ///
    /// The strongest statement in this type: a verified, pixel-exact decoder
    /// could not parse bytes a container presents as a valid access unit.
    ///
    /// Still not a statement about *why*. A rejected packet is consistent with
    /// corruption, with a truncated tail, and with an encoding the decoder does
    /// not implement, and nothing here distinguishes them.
    PacketFailed {
        /// Index of the packet within the stream, zero-based.
        packet: usize,
        /// Whether the failed packet was a random-access point.
        ///
        /// Load-bearing for everything that follows: a keyframe is independent,
        /// so the decoder resynchronises at the next one. A *predicted* frame
        /// that fails destroys the reference the following frames are built on.
        is_key_frame: bool,
        /// What the decoder reported.
        reason: String,
    },

    /// Frames skipped while waiting to resynchronise.
    ///
    /// Produced when a predicted frame fails. Its reference is gone, so every
    /// later predicted frame would decode against a picture that no longer
    /// exists — and would produce something that *looks* like a frame.
    ///
    /// Recording the span rather than silently dropping the packets is the whole
    /// point: an analyst reading "decoded 40 frames" must be able to find out
    /// that 12 more were present and could not be used.
    LostReference {
        /// Index of the first packet skipped.
        from_packet: usize,
        /// How many packets were skipped before the next keyframe.
        skipped: usize,
    },

    /// A frame decoded but could not be used.
    ///
    /// The decoder succeeded and the result was still unusable — an unsupported
    /// pixel format, or a buffer shorter than the frame's own dimensions.
    UnusableFrame {
        /// Index of the packet that produced it.
        packet: usize,
        /// Why it could not be used.
        reason: String,
    },
}

impl DecodeDamage {
    /// Returns the stable tag used in rule IDs and report output.
    #[must_use]
    pub fn tag(&self) -> &'static str {
        match self {
            Self::PacketFailed { .. } => "packet_failed",
            Self::LostReference { .. } => "lost_reference",
            Self::UnusableFrame { .. } => "unusable_frame",
        }
    }

    /// The packet index this defect sits at.
    #[must_use]
    pub fn packet(&self) -> usize {
        match self {
            Self::PacketFailed { packet, .. } | Self::UnusableFrame { packet, .. } => *packet,
            Self::LostReference { from_packet, .. } => *from_packet,
        }
    }

    /// Whether this defect means a frame the file describes could not be read.
    ///
    /// Always true, and that is the difference from structural damage. There,
    /// appended bytes are present-but-unexplained. Here every variant is a case
    /// where an access unit the container declares could not be turned into an
    /// image.
    #[must_use]
    pub fn is_missing_data(&self) -> bool {
        true
    }

    /// Renders the damage as a single line for an anomaly list.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::PacketFailed {
                packet,
                is_key_frame,
                reason,
            } => format!(
                "packet {packet} ({}) was rejected by the decoder: {reason}",
                if *is_key_frame {
                    "keyframe"
                } else {
                    "predicted"
                }
            ),
            Self::LostReference {
                from_packet,
                skipped,
            } => format!(
                "{skipped} packet(s) from index {from_packet} were skipped: they are predicted \
                 frames whose reference was lost, and decoding them would have produced pictures \
                 built on a frame that does not exist"
            ),
            Self::UnusableFrame { packet, reason } => {
                format!("packet {packet} decoded to a frame that could not be used: {reason}")
            }
        }
    }
}

/// The outcome of a resilient decode: what was recovered, what was not, and why.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DecodeRun {
    /// Frames that decoded cleanly and could be used.
    ///
    /// Each frame's `index` is the **packet** index it came from, so a gap in
    /// those values marks frames that were lost. Consumers comparing a frame to
    /// its predecessor must honour that gap rather than treating this vector as
    /// contiguous — see `-video::scene`.
    pub frames: Vec<DecodedFrame>,
    /// Every recoverable fault, in the order encountered.
    pub damage: Vec<DecodeDamage>,
    /// Why the run ended, when it ended early.
    ///
    /// `None` means the stream was fully processed. A run that *stopped* still
    /// returns every frame and every fault it recovered, because a partial decode
    /// of a damaged file is still evidence about it.
    pub stopped: Option<DecodeError>,
}

impl DecodeRun {
    /// Number of recoverable faults, for the spec §30 summary line.
    #[must_use]
    pub fn recoverable_error_count(&self) -> usize {
        self.damage.len()
    }

    /// Whether the run completed with no fault of any kind.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.damage.is_empty() && self.stopped.is_none()
    }
}

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

    /// Converts the frame to a greyscale image for extraction as evidence (§32).
    ///
    /// # Greyscale, and deliberately not colour
    ///
    /// A `DecodedFrame` keeps only the luma plane — chroma is what `scene` and
    /// `near_duplicate` do not need, and keeping it for every frame in a bounded
    /// window would multiply the memory that bound exists to avoid. There is
    /// therefore no chroma here to convert.
    ///
    /// Two ways to produce colour were rejected, and both would have been lies:
    /// assuming neutral chroma (which renders a saturated frame as grey and hides
    /// exactly the colour shift a reviewer may be looking for) and re-decoding
    /// through the full YUV path (which would make evidence extraction cost a
    /// second decode of the whole window). The artefact says what it is, and the
    /// caption states it, so a reviewer comparing it against a reference decode
    /// knows why the colours differ.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError::EmptyFrame`] for a zero-sized frame, and
    /// [`FrameError::PlaneSizeMismatch`] when the luma plane is shorter than the
    /// declared dimensions — a truncated or hostile frame. Refused rather than
    /// padded, because padding would produce a plausible image with invented
    /// pixels along one edge.
    ///
    /// # Panics
    ///
    /// Never.
    pub fn to_greyscale(&self, time: MediaTime) -> Result<FrameImage, FrameError> {
        if self.width == 0 || self.height == 0 {
            return Err(FrameError::EmptyFrame);
        }
        let pixels = (self.width as usize)
            .checked_mul(self.height as usize)
            .ok_or(FrameError::EmptyFrame)?;

        if self.luma.len() < pixels {
            return Err(FrameError::PlaneSizeMismatch {
                width: self.width,
                height: self.height,
                actual: self.luma.len(),
            });
        }

        // R = G = B = Y, with no matrix applied. Applying BT.601 to a neutral
        // chroma would apply the matrix's offset and rescale the output, yielding a
        // different image from the one the analysers actually measured — and an
        // evidence frame that disagrees with the finding it supports is worse than
        // none.
        let rgb = self
            .luma
            .iter()
            .take(pixels)
            .flat_map(|&y| [y, y, y])
            .collect();

        Ok(FrameImage {
            rgb,
            width: self.width,
            height: self.height,
            time,
            // The packet index, matching what `extract` records and what the
            // pixel rules report, so a finding and its evidence frame agree on
            // which frame it means.
            frame_index: u32::try_from(self.index).unwrap_or(u32::MAX),
            // No matrix was applied, so none is claimed. The string says
            // which planes were missing rather than just "none", because a greyscale
            // frame and a frame that was already RGB need different explanations.
            colour_matrix: "none (luma only; chroma not retained)",
        })
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

    /// Decodes as much of the stream as possible, recording faults as it goes.
    ///
    /// This is the entry point an examination uses, and the one that implements
    /// spec §30's requirement that "a scan should continue after recoverable
    /// errors". [`Self::decode_prefix`] returns at the first bad packet, which
    /// was a reasonable reading of "how much can I decode" and the wrong one for
    /// "what is wrong with this file": a single corrupt frame near the start
    /// silently ends the examination and reports nothing about the rest.
    ///
    /// # What continuing actually means
    ///
    /// Resynchronisation, not persistence. When a packet fails the decoder's
    /// reference state is gone for good: a predicted frame is decoded *from*
    /// other frames, so once one is lost, every later predicted frame would be
    /// built on a picture that does not exist. Feeding them anyway produces
    /// output that decodes without error and is nevertheless wrong — and a
    /// plausible-looking wrong frame is the worst outcome this engine can
    /// produce, because a scene-change or near-duplicate finding computed from it
    /// reads as a measurement.
    ///
    /// So after a failure the session **skips forward to the next keyframe**,
    /// where decoding is self-contained and the pixels really are the media's.
    /// The skipped span is recorded as [`DecodeDamage::LostReference`] rather
    /// than dropped, so a gap between recovered frames is visible in the report
    /// instead of being a silent hole in a frame count.
    ///
    /// # What it never does
    ///
    /// It never guesses. Frames it could not decode are not interpolated,
    /// repeated from the previous frame, or substituted from elsewhere — every
    /// number downstream comes only from frames that genuinely decoded.
    #[must_use]
    pub fn decode_resilient(&mut self, packets: &[(Vec<u8>, bool)]) -> DecodeRun {
        let mut run = DecodeRun::default();
        // Packet whose failure we are still recovering from. `None` means the
        // decoder is in sync and every packet can be attempted.
        let mut lost_from: Option<usize> = None;

        for (index, (data, is_key_frame)) in packets.iter().enumerate() {
            if self.decoded >= self.limits.max_frames {
                run.stopped = Some(DecodeError::LimitReached {
                    limit: "the frame limit",
                    decoded: self.decoded,
                });
                break;
            }

            // Resynchronising: wait for a keyframe. Anything skipped here is
            // still accounted for, so the frame count can always be reconciled
            // against the packet count.
            if lost_from.is_some() && !is_key_frame {
                continue;
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
                    // A clean keyframe closes the previous loss, whatever the size
                    // of the span that led here.
                    if let Some(from) = lost_from.take() {
                        run.damage.push(DecodeDamage::LostReference {
                            from_packet: from,
                            skipped: index.saturating_sub(from),
                        });
                    }
                    self.retain(index, *is_key_frame, frame, &mut run);
                    // `retain` sets `stopped` when the in-memory limit is hit, so
                    // the check belongs here rather than at the top of the loop:
                    // the frame count has not moved, and only `retain` knows.
                    if run.stopped.is_some() {
                        break;
                    }
                }
                Ok(None) => {
                    // The decoder produced no picture and reported no error.
                    //
                    // This is not hypothetical, and it is the single most
                    // important thing this module knows about the decoders behind
                    // it: feeding an AV1 stream with one packet's bytes flipped
                    // yields 8 frames from 9 packets, with **no error anywhere**.
                    // Pure garbage yields 0 frames from 5 packets, still no error.
                    // A build that only counted `Err` would report such a file as
                    // perfectly clean while measuring nothing at all.
                    //
                    // So silence is read as damage, with one soundness guard: only
                    // a **keyframe** counts. A keyframe is self-contained by
                    // definition — it references nothing — so a decoder emitting no
                    // picture for one cannot be holding it back for reordering or
                    // waiting on a reference. A *predicted* frame yielding nothing
                    // is genuinely ambiguous, and is left unrecorded rather than
                    // guessed at.
                    if *is_key_frame {
                        run.damage.push(DecodeDamage::PacketFailed {
                            packet: index,
                            is_key_frame: true,
                            reason: "the decoder produced no frame for a keyframe. A keyframe \
                                     references no other frame, so there is no reordering or \
                                     missing-reference reason for it to yield nothing; the \
                                     packet was not decodable"
                                .to_owned(),
                        });
                        // A keyframe is self-contained, so losing it leaves the
                        // decoder in sync — the next keyframe needs nothing from it.
                        // `lost_from` is deliberately not set.
                    }
                }
                Err(error) => {
                    run.damage.push(DecodeDamage::PacketFailed {
                        packet: index,
                        is_key_frame: *is_key_frame,
                        reason: error.detail,
                    });

                    // A failed *keyframe* still leaves the decoder in sync — it is
                    // self-contained, so the next one needs nothing from it. A
                    // failed *predicted* frame destroys the reference chain, so
                    // the frames after it must not be attempted.
                    if !*is_key_frame && lost_from.is_none() {
                        lost_from = Some(index);
                    }
                }
            }
        }

        // A loss still open at the end of the stream never found its keyframe.
        // Recorded so the skipped tail is accounted for; dropping it would let a
        // run report a clean finish over a truncated track.
        if let Some(from) = lost_from {
            run.damage.push(DecodeDamage::LostReference {
                from_packet: from,
                skipped: packets.len().saturating_sub(from),
            });
        }

        // Only reconcile when the stream was walked to the end. If a limit stopped the
        // run, the packets after that point were never examined rather than
        // dropped, and calling them lost would be a false finding — on top of
        // inflating the count the report prints. `stopped` already states the
        // real reason the tail is missing.
        if run.stopped.is_none() {
            Self::reconcile(&mut run, packets.len());
        }
        run
    }

    /// Finds packets that vanished without the decoder saying so.
    ///
    /// The decoder behind this session does not report undecodable packets: an
    /// AV1 stream with one packet's bytes flipped yields eight frames from nine
    /// packets and raises nothing at all. The `Ok(None)` arm catches that when
    /// the victim is a keyframe, but a **predicted** frame dropped the same way is
    /// indistinguishable from reordering at the call site — and ignoring that case
    /// would mean reporting a damaged file as clean.
    ///
    /// So the gap is used as the evidence instead. Every recovered frame carries
    /// the packet index it came from, so a hole in that sequence is a packet that
    /// was presented and produced no picture. That is observable without knowing
    /// anything about the decoder's internals, which is what makes it a sound
    /// check rather than a guess.
    ///
    /// Deliberately **not** recorded for spans already accounted for by
    /// [`DecodeDamage::LostReference`]: this session skipped those packets itself,
    /// knows exactly which ones, and has already said so. Recording them twice
    /// would inflate the count spec §30 asks the report to print.
    fn reconcile(run: &mut DecodeRun, packet_count: usize) {
        // Spans this session skipped deliberately, as (first, last) inclusive.
        let explained: Vec<(usize, usize)> = run
            .damage
            .iter()
            .filter_map(|d| match d {
                DecodeDamage::LostReference {
                    from_packet,
                    skipped,
                } => Some((
                    *from_packet,
                    from_packet.saturating_add(*skipped).saturating_sub(1),
                )),
                _ => None,
            })
            .collect();

        let mut missing: Vec<usize> = Vec::new();
        let mut expected = 0usize;
        for frame in &run.frames {
            while expected < frame.index {
                missing.push(expected);
                expected = expected.saturating_add(1);
            }
            expected = frame.index.saturating_add(1);
        }
        // A trailing gap means the decoder stopped emitting before the stream
        // ended, which is the same kind of loss.
        while expected < packet_count {
            missing.push(expected);
            expected = expected.saturating_add(1);
        }

        for packet in missing {
            if explained
                .iter()
                .any(|(first, last)| packet >= *first && packet <= *last)
            {
                continue;
            }
            // Already named by the `Ok(None)` arm. One lost packet is one defect,
            // and spec §30 has the report print a *count* of them — counting it
            // twice would inflate a number an analyst is meant to rely on. The
            // keyframe diagnosis is kept because it says more, so the weaker
            // reconciliation reason stands down rather than the other way round.
            if run.damage.iter().any(|d| d.packet() == packet) {
                continue;
            }
            run.damage.push(DecodeDamage::PacketFailed {
                packet,
                is_key_frame: false,
                reason: "the decoder returned no frame for this packet and reported no error. \
                         Neighbouring packets did produce frames, so this one was presented and \
                         dropped rather than never read"
                    .to_owned(),
            });
        }
    }

    /// Reduces one decoded frame into the run, recording it or stopping on it.
    ///
    /// Split out so the body of [`Self::decode_resilient`] reads as the
    /// resynchronisation policy rather than as buffer management.
    fn retain(&mut self, index: usize, is_key_frame: bool, frame: VideoFrame, run: &mut DecodeRun) {
        let Some(reduced) = reduce(index, is_key_frame, frame) else {
            run.damage.push(DecodeDamage::UnusableFrame {
                packet: index,
                reason: "the decoder produced a frame this build cannot use: an unsupported \
                         pixel format, or a buffer shorter than the frame's own dimensions"
                    .to_owned(),
            });
            return;
        };

        if run.frames.len() >= self.limits.max_frames_in_memory {
            // The frame decoded but there is nowhere to put it. Reported as the
            // limit it is rather than as a second defect, so that one cause is
            // not counted twice, and the frame is kept so the count of what was
            // recovered stays honest.
            run.stopped = Some(DecodeError::LimitReached {
                limit: "the in-memory frame limit",
                decoded: run.frames.len(),
            });
        }

        run.frames.push(reduced);
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

    /// Encodes `count` real AV1 frames with a keyframe every 3, as a GOP holding
    /// both keyframe and predicted packets.
    ///
    /// Real encoded bytes, because the defect pinned here is a property of the
    /// decoder's *behaviour* on damaged input. Synthetic frames would let a
    /// regression in the detector pass without ever touching a real bitstream.
    fn encode_av1_gop(count: usize) -> Vec<(Vec<u8>, bool)> {
        use tpt_kinetix_av1::{Av1Encoder, Av1EncoderConfig};
        use tpt_kinetix_core::frame::VideoFrame;
        use tpt_kinetix_core::pixel_format::PixelFormat;
        use tpt_kinetix_core::timestamp::Timestamp;

        const W: u32 = 64;
        const H: u32 = 48;

        let picture = |level: u8| {
            let (w, h) = (W as usize, H as usize);
            let mut data = vec![0u8; w * h + (w * h) / 2];
            for y in 0..h {
                for x in 0..w {
                    data[y * w + x] = (level as usize + x + y) as u8;
                }
            }
            for sample in data.iter_mut().skip(w * h) {
                *sample = 128;
            }
            VideoFrame {
                pts: Timestamp::new(0, (1, 1000)),
                dts: Timestamp::new(0, (1, 1000)),
                data,
                width: W,
                height: H,
                pixel_format: PixelFormat::Yuv420p,
                is_key_frame: true,
            }
        };

        let mut encoder = Av1Encoder::new(&Av1EncoderConfig {
            width: W,
            height: H,
            bitrate: 0,
            quantizer: 80,
            speed: 10,
            keyframe_interval: 3,
        })
        .expect("encoder");

        let mut packets = Vec::new();
        for index in 0..count {
            if let Some(packet) = encoder
                .encode_frame(&picture((index * 30) as u8))
                .expect("encodes")
            {
                packets.push((packet.data, packet.is_key_frame));
            }
        }
        packets.extend(
            encoder
                .flush()
                .expect("flush")
                .into_iter()
                .map(|p| (p.data, p.is_key_frame)),
        );
        packets
    }

    #[test]
    fn a_corrupt_packet_is_found_although_the_decoder_reports_nothing() {
        // The regression this module's whole design turns on.
        //
        // The AV1 decoder does **not** report undecodable packets. Measured
        // directly: flipping the bytes of one packet in a nine-packet AV1 stream
        // yields eight frames and no error; pure garbage yields zero frames and
        // no error. A scheme built on `Err` alone would call both files clean
        // while measuring nothing, so detection rests on the gap in the recovered
        // frame indices instead.
        let gop = encode_av1_gop(9);
        assert!(
            gop.iter().any(|(_, key)| !key),
            "the fixture must contain predicted frames"
        );

        let mut session = DecodeSession::open("av01", DecodeLimits::default()).expect("decoder");
        let clean = session.decode_resilient(&gop);
        assert_eq!(
            clean.frames.len(),
            gop.len(),
            "a clean stream decodes fully"
        );
        assert!(
            clean.is_clean(),
            "a clean stream reports nothing: {clean:?}"
        );

        let victim = gop
            .iter()
            .position(|(_, key)| !*key)
            .expect("a predicted frame");
        let mut broken = gop.clone();
        for byte in broken[victim].0.iter_mut().skip(3) {
            *byte ^= 0xFF;
        }

        let mut session = DecodeSession::open("av01", DecodeLimits::default()).expect("decoder");
        let run = session.decode_resilient(&broken);

        assert!(
            run.frames.len() < broken.len(),
            "the corrupt packet should not have produced a frame"
        );
        assert!(
            run.damage.iter().any(|d| d.packet() == victim),
            "the silently dropped packet at {victim} must be reported: {:?}",
            run.damage
        );
        assert!(!run.is_clean(), "a damaged file may never read as clean");

        // The gap has to survive into the frames themselves, or the scene
        // analyser cannot know not to compare across it.
        let indices: Vec<usize> = run.frames.iter().map(|f| f.index).collect();
        assert!(
            !indices.contains(&victim),
            "the dropped frame must not appear: {indices:?}"
        );
    }

    #[test]
    fn one_lost_packet_is_reported_once() {
        // The count in spec §30's summary line must not be inflated. A keyframe
        // the decoder silently drops is diagnosable twice — once by the keyframe
        // rule, once by the index gap — and has to collapse to one defect.
        let gop = encode_av1_gop(9);
        let victim = gop.iter().position(|(_, key)| *key).expect("a keyframe");
        let mut broken = gop.clone();
        for byte in broken[victim].0.iter_mut().skip(3) {
            *byte ^= 0xFF;
        }

        let mut session = DecodeSession::open("av01", DecodeLimits::default()).expect("decoder");
        let run = session.decode_resilient(&broken);

        let reports = run.damage.iter().filter(|d| d.packet() == victim).count();
        assert_eq!(
            reports, 1,
            "one lost packet is one defect: {:?}",
            run.damage
        );
    }

    #[test]
    fn a_stream_that_decodes_fully_is_never_reconciled_into_damage() {
        // The negative guard. `reconcile` compares the highest recovered index
        // against the packet count, so a decoder that legitimately holds a frame
        // back could be mistaken for a dropped one. A clean run must stay clean.
        let gop = encode_av1_gop(6);
        let mut session = DecodeSession::open("av01", DecodeLimits::default()).expect("decoder");
        let run = session.decode_resilient(&gop);

        assert_eq!(run.frames.len(), gop.len());
        assert!(run.damage.is_empty(), "{:?}", run.damage);
    }

    #[test]
    fn a_run_stopped_by_a_limit_does_not_claim_the_unread_tail_was_lost() {
        // The tail of a bounded run was never examined, not dropped. Calling it a
        // decode failure would be a false finding *and* would inflate the very count
        // spec §30 asks the report to print.
        let mut session = DecodeSession::open(
            "av01",
            DecodeLimits {
                max_frames: 0,
                max_frames_in_memory: 8,
            },
        )
        .expect("av01 is decodable");

        let packets: Vec<(Vec<u8>, bool)> = (0..20).map(|_| (vec![0u8; 16], true)).collect();
        let run = session.decode_resilient(&packets);

        assert!(matches!(
            run.stopped,
            Some(DecodeError::LimitReached { .. })
        ));
        assert!(
            run.damage.is_empty(),
            "no packet was examined, so none was lost: {:?}",
            run.damage
        );
    }

    #[test]
    fn a_clean_stream_produces_no_damage_and_no_stop() {
        // The common path must report nothing at all, or every report carries a
        // corruption section that means nothing.
        let mut session =
            DecodeSession::open("av01", DecodeLimits::default()).expect("av01 is decodable");
        let run = session.decode_resilient(&[]);

        assert!(run.is_clean(), "{run:?}");
        assert_eq!(run.recoverable_error_count(), 0);
        assert!(run.frames.is_empty());
    }

    #[test]
    fn garbage_packets_are_recorded_rather_than_returned_as_errors() {
        // Bytes that are not AV1 at all. The point is that the caller gets a
        // *report*, not an `Err`: one corrupt file must not end an examination.
        let mut session =
            DecodeSession::open("av01", DecodeLimits::default()).expect("av01 is decodable");
        let packets: Vec<(Vec<u8>, bool)> = (0..3).map(|_| (vec![0xABu8; 64], true)).collect();

        let run = session.decode_resilient(&packets);
        assert!(
            !run.damage.is_empty(),
            "undecodable bytes must be recorded: {run:?}"
        );
        assert!(run.frames.is_empty(), "nothing decoded, nothing claimed");
        assert!(!run.is_clean());
    }

    #[test]
    fn damage_describes_itself_and_names_its_packet() {
        let damage = DecodeDamage::PacketFailed {
            packet: 7,
            is_key_frame: false,
            reason: "bitstream error".to_owned(),
        };
        assert_eq!(damage.tag(), "packet_failed");
        assert_eq!(damage.packet(), 7);
        assert!(damage.describe().contains("predicted"));
        assert!(damage.describe().contains("7"));
        assert!(damage.is_missing_data());
    }

    #[test]
    fn every_damage_variant_is_distinguishable() {
        // Spec §30's "17 recoverable decode errors" is only useful if the errors
        // can be told apart, so the three variants must not collapse.
        let variants = [
            DecodeDamage::PacketFailed {
                packet: 1,
                is_key_frame: true,
                reason: "x".to_owned(),
            },
            DecodeDamage::LostReference {
                from_packet: 2,
                skipped: 5,
            },
            DecodeDamage::UnusableFrame {
                packet: 3,
                reason: "x".to_owned(),
            },
        ];
        let tags: std::collections::BTreeSet<_> = variants.iter().map(DecodeDamage::tag).collect();
        assert_eq!(tags.len(), 3, "each variant needs its own tag");
        for damage in &variants {
            assert!(!damage.describe().is_empty());
        }
    }

    #[test]
    fn a_lost_reference_names_the_frames_it_cost() {
        let damage = DecodeDamage::LostReference {
            from_packet: 4,
            skipped: 9,
        };
        let text = damage.describe();
        assert!(text.contains('9'), "{text}");
        assert!(text.contains('4'), "{text}");
    }

    #[test]
    fn the_frame_limit_stops_the_run_but_keeps_what_was_recovered() {
        // A bounded run must say it was bounded. Returning the frames with no
        // indication the stream continued would read as a complete analysis.
        let mut session = DecodeSession::open(
            "av01",
            DecodeLimits {
                max_frames: 0,
                max_frames_in_memory: 8,
            },
        )
        .expect("av01 is decodable");

        let run = session.decode_resilient(&[(vec![0u8; 8], true)]);
        assert!(matches!(
            run.stopped,
            Some(DecodeError::LimitReached { .. })
        ));
        assert!(!run.is_clean());
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
