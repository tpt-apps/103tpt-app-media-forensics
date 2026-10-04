//! Media viewer state: frame stepping, timestamps, zoom, pixel inspection,
//! histogram, waveform, and A/B comparison (spec §43, §44).
//!
//! # The viewer navigates; it does not measure
//!
//! Spec §43's viewer is a navigation surface. Every value it shows is read from
//! a measurement the engine already took: `MediaTime` for PTS, the container's
//! declared timebase for DTS, a decoded frame for pixels. This module holds the
//! *cursor* — which frame, how far zoomed, what is under the pointer — and the
//! arithmetic needed to turn a click into a coordinate. Decoding and measuring
//! stay in `-core` and `-video`.
//!
//! # One value the viewer does compute, and why
//!
//! The pixel inspector's Y'CbCr reading. Spec §44 requires one, and requires
//! that a conversion be indicated rather than silent. [`PixelSample`] therefore
//! carries [`ColourReading::rgb_basis`] and [`ColourReading::matrix`]: the
//! numbers are computed here because they are a *presentation* of a pixel the
//! engine already decoded, but the conversion that produced them travels with
//! them. An inspector showing `Cb: 128` beside a pixel that is actually
//! `Cb: 121` in BT.709, with nothing saying so, is worse than showing nothing.

use serde::{Deserialize, Serialize};

use tpt_app_media_forensics_model::{MediaTime, Timebase};

/// How the viewer moves between frames (spec §43).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameStep {
    /// One frame forward.
    Forward,
    /// One frame back.
    Back,
    /// To the next random-access point.
    NextKeyFrame,
    /// To the previous random-access point.
    PreviousKeyFrame,
    /// One second forward.
    ForwardSecond,
    /// One second back.
    BackSecond,
    /// To the first frame.
    Start,
    /// To the last frame.
    End,
}

/// The zoom levels the viewer offers (spec §43).
///
/// A closed set rather than a free scale. An unbounded zoom factor would let
/// the frontend request a magnification whose integer pixel size is zero, or a
/// magnification so large the frame cannot be addressed at all. Every level
/// here has an exact integer scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZoomLevel {
    /// Fits the frame to the viewport.
    Fit,
    /// 1:1 — one source pixel per screen pixel.
    Actual,
    /// 2x.
    Double,
    /// 4x.
    Quadruple,
    /// 8x.
    Octuple,
}

impl ZoomLevel {
    /// Every level, coarsest first.
    pub const ALL: [Self; 5] = [
        Self::Fit,
        Self::Actual,
        Self::Double,
        Self::Quadruple,
        Self::Octuple,
    ];

    /// The magnification as an exact integer.
    ///
    /// `None` for [`ZoomLevel::Fit`], which depends on the viewport and is
    /// therefore computed by the renderer. Returning `None` rather than `1`
    /// keeps "fit" from being mistaken for "actual size" — a distinction that
    /// matters when the whole point is reading individual pixels.
    #[must_use]
    pub const fn scale(self) -> Option<u32> {
        match self {
            Self::Fit => None,
            Self::Actual => Some(1),
            Self::Double => Some(2),
            Self::Quadruple => Some(4),
            Self::Octuple => Some(8),
        }
    }

    /// The next coarser level, saturating at the coarsest.
    #[must_use]
    pub fn zoom_out(self) -> Self {
        let index = ZoomLevel::ALL
            .iter()
            .position(|level| *level == self)
            .unwrap_or(0);
        ZoomLevel::ALL[index.saturating_sub(1)]
    }

    /// The next finer level, saturating at the finest.
    #[must_use]
    pub fn zoom_in(self) -> Self {
        let index = ZoomLevel::ALL
            .iter()
            .position(|level| *level == self)
            .unwrap_or(0);
        ZoomLevel::ALL[(index + 1).min(ZoomLevel::ALL.len() - 1)]
    }

    /// Returns the label shown on the zoom control.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Fit => "FIT",
            Self::Actual => "1:1",
            Self::Double => "2x",
            Self::Quadruple => "4x",
            Self::Octuple => "8x",
        }
    }
}

/// The timestamp fields the viewer displays for a frame (spec §43).
///
/// PTS and DTS are separate fields because they disagree, and that disagreement
/// is often the finding. A viewer showing only PTS would hide exactly the
/// reordering that spec §24 exists to detect.
///
/// `dts` is `None` rather than zero for a stream that declares no decode
/// order — which is every stream where DTS is genuinely not applicable. Zero
/// would be a real timecode saying the frame decodes first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameTimestamps {
    /// Presentation timestamp.
    pub pts: MediaTime,
    /// Decode timestamp, when the container declares one.
    pub dts: Option<MediaTime>,
    /// Frame number as counted from the start of the sequence.
    pub frame_number: u64,
    /// PTS in the stream's own tick units, exactly as declared.
    ///
    /// Carried verbatim so a reviewer can compare the viewer's timecode against
    /// the value in the file. A converted number that cannot be traced back to
    /// the declared one cannot be checked.
    pub pts_ticks: i64,
    /// DTS in the stream's own tick units, when declared.
    pub dts_ticks: Option<i64>,
    /// Whether this frame is a random-access point.
    pub is_key_frame: bool,
}

impl FrameTimestamps {
    /// Builds the display fields from the container's declared values.
    ///
    /// The tick counts are taken as given rather than recomputed from the
    /// timecodes: they are what the file says, and the timecodes are what this
    /// engine says about them. Keeping both lets the two be compared.
    #[must_use]
    pub fn new(
        pts: MediaTime,
        dts: Option<MediaTime>,
        frame_number: u64,
        timebase: Timebase,
        pts_ticks: i64,
        dts_ticks: Option<i64>,
        is_key_frame: bool,
    ) -> Self {
        // Consistency, not decoration: if the caller passed a DTS in one
        // representation, the other must agree. Deriving `dts` from `dts_ticks`
        // when only one was supplied means the display cannot show two numbers
        // contradicting each other.
        let dts = dts.or_else(|| dts_ticks.map(|ticks| timebase.ticks_to_media_time(ticks)));
        Self {
            pts,
            dts,
            frame_number,
            pts_ticks,
            dts_ticks,
            is_key_frame,
        }
    }

    /// The reorder delay between decode and presentation, when both are known.
    ///
    /// `None` when DTS is absent. This is the value that makes reordering
    /// visible; without it a viewer shows two columns of numbers and leaves the
    /// analyst to subtract them.
    #[must_use]
    pub fn reorder_delay(&self) -> Option<MediaTime> {
        self.dts
            .map(|dts| MediaTime::from_micros(self.pts.as_micros() - dts.as_micros()))
    }

    /// Returns true when the frame is presented before it is decoded.
    ///
    /// Negative reorder delay — a presentation order that runs ahead of decode
    /// order — which is normal for most codecs and only remarkable in
    /// magnitude. Reported rather than judged, since the threshold for
    /// "remarkable" belongs to a rule (spec §24), not to a viewer.
    #[must_use]
    pub fn is_reordered(&self) -> bool {
        self.reorder_delay()
            .is_some_and(|delay| delay.as_micros() > 0)
    }
}
/// Which RGB reading the inspector is displaying (spec §44).
///
/// The engine extracts evidence frames through the ITU-R BT.601 matrix, and
/// records that fact on every `FrameImage` (see `-video::frame::COLOUR_MATRIX`).
/// A greyscale evidence frame — which is what the Tier-2 analysers keep, because
/// they measure luma only — has no chroma at all and no matrix was applied.
///
/// These are genuinely different pictures that a viewer could otherwise present
/// as "the frame".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RgbBasis {
    /// Interleaved 8-bit RGB produced by the BT.601 matrix from decoded YUV.
    Bt601Matrix,
    /// Greyscale: the luma plane written to all three channels, no matrix.
    LumaOnly,
}

impl RgbBasis {
    /// Returns the matrix named on the inspector, or why there is none.
    #[must_use]
    pub const fn matrix(self) -> &'static str {
        match self {
            Self::Bt601Matrix => "ITU-R BT.601",
            Self::LumaOnly => "none (luma only; chroma not retained)",
        }
    }

    /// Whether chroma information exists in this reading.
    ///
    /// Drives the inspector's warning: a Y'CbCr value computed from a greyscale
    /// frame is arithmetically valid and completely uninformative, because Cb
    /// and Cr are 128 by construction. Showing it without saying so would let a
    /// reviewer conclude the content is neutral in colour.
    #[must_use]
    pub const fn has_chroma(self) -> bool {
        matches!(self, Self::Bt601Matrix)
    }
}

/// One pixel's value, with its conversions named (spec §44).
///
/// ```text
/// X: 1920
/// Y: 540
/// RGB: 120 / 122 / 125
/// Y'CbCr: ...
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelSample {
    /// Column, zero-based.
    pub x: u32,
    /// Row, zero-based.
    pub y: u32,
    /// Red, 0-255.
    pub r: u8,
    /// Green, 0-255.
    pub g: u8,
    /// Blue, 0-255.
    pub b: u8,
    /// Luma, 0-255, computed from the RGB above.
    pub y_prime: u8,
    /// Cb, 0-255, computed from the RGB above.
    ///
    /// Meaningless when [`RgbBasis::has_chroma`] is false; see that method.
    pub cb: u8,
    /// Cr, 0-255, computed from the RGB above.
    pub cr: u8,
    /// Where the RGB reading came from.
    pub basis: RgbBasis,
    /// The declared source colour space, when the container recorded one.
    ///
    /// `None` is the honest answer for a greyscale frame and for a container
    /// that declared no primaries. Substituting a guess here — "presumably
    /// BT.709" — is precisely what spec §44 forbids.
    pub source_colour_space: Option<String>,
}

impl PixelSample {
    /// Reads one pixel out of an interleaved RGB buffer.
    ///
    /// Returns `None` for a coordinate outside the frame or a buffer that is
    /// not the length the frame's dimensions imply. A malformed frame is
    /// attacker-controlled input (spec §75), so this is bounds-checked rather
    /// than indexing directly: the viewer runs on whatever the decoder
    /// produced, and a decoder handed hostile bytes may produce a short buffer.
    #[must_use]
    pub fn read(
        rgb: &[u8],
        width: u32,
        height: u32,
        x: u32,
        y: u32,
        basis: RgbBasis,
        source_colour_space: Option<String>,
    ) -> Option<Self> {
        // Bounds-checked before anything else: the coordinate came from a mouse
        // event, and the dimensions from a decoder that may have been fed
        // hostile bytes (spec §75).
        if width == 0 || height == 0 || x >= width || y >= height {
            return None;
        }
        // Reject a short buffer rather than reading past it. Padding a truncated
        // frame would show an analyst invented pixels along one edge.
        let needed = rgb_byte_len(width, height)?;
        if rgb.len() < needed {
            return None;
        }

        let offset = (y as usize * width as usize + x as usize) * 3;
        let (r, g, b) = (rgb[offset], rgb[offset + 1], rgb[offset + 2]);
        let (y_prime, cb, cr) = rgb_to_ycbcr(r, g, b);

        Some(Self {
            x,
            y,
            r,
            g,
            b,
            y_prime,
            cb,
            cr,
            basis,
            source_colour_space,
        })
    }

    /// The sentence the inspector prints under the numbers (spec §44).
    ///
    /// Spec §44 says: *do not silently convert values without indicating the
    /// conversion.* This is that indication. It names the matrix, and it says
    /// outright when the chroma reading carries no information — which is the
    /// case that would otherwise be most misleading.
    #[must_use]
    pub fn conversion_note(&self) -> String {
        let mut note = format!(
            "RGB shown as decoded ({}); Y'CbCr computed from those RGB values.",
            self.basis.matrix()
        );
        if !self.basis.has_chroma() {
            note.push_str(
                " This frame retains luma only, so Cb and Cr are 128 by construction \
                 and describe nothing about the content's colour.",
            );
        }
        match &self.source_colour_space {
            Some(space) => note.push_str(&format!(" Source colour space: {space}.")),
            None => note.push_str(" The container declared no colour space, so none is assumed."),
        }
        note
    }
}

/// Converts 8-bit RGB to BT.709 Y'CbCr, the convention a pixel inspector shows.
///
/// # Why BT.709 and not the frame's own matrix
///
/// The RGB above was produced by *some* matrix — BT.601 in the engine's frame
/// extractor. Converting that result to Y'CbCr with BT.601 and back is a round
/// trip; converting with BT.709 is not. But spec §44 asks for a Y'CbCr reading
/// as a property of the pixel, and the universally used convention for that is
/// BT.709. Which is precisely why the inspector names it: a reviewer who needs
/// the matrix-consistent figure can ask for it, and a reviewer who does not is
/// not silently handed one.
#[must_use]
pub fn rgb_to_ycbcr(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    // Fixed-point integer BT.709 full-range. Integer rather than float because
    // this runs under the pointer on every mouse move, and because a value that
    // could differ in the last bit between runs would be a poor thing to show in
    // a tool whose premise is reproducibility (spec §77).
    let y = ((19595 * i32::from(r) + 38470 * i32::from(g) + 7471 * i32::from(b) + 32768) >> 16)
        .clamp(0, 255);
    let cb = ((-11056 * i32::from(r) - 21712 * i32::from(g) + 32768 * i32::from(b) + 8388608)
        >> 16)
        .clamp(0, 255);
    let cr = ((32768 * i32::from(r) - 27440 * i32::from(g) - 5328 * i32::from(b) + 8388608) >> 16)
        .clamp(0, 255);
    (
        u8::try_from(y).unwrap_or(0),
        u8::try_from(cb).unwrap_or(0),
        u8::try_from(cr).unwrap_or(0),
    )
}

/// The luma histogram of one frame (spec §43).
///
/// # 256 bins, always
///
/// A fixed 256-bin histogram rather than a coarse one. A reviewer comparing two
/// frames needs to see that a bin moved from 3 to 4, and a histogram that
/// silently merged the neighbourhood would hide exactly the small differences
/// this tool exists to surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Histogram {
    /// Count per luma level, index 0 to 255.
    pub bins: Vec<u32>,
    /// Total pixels counted.
    pub total: u64,
    /// Pixels at or near full black.
    pub black_points: u64,
    /// Pixels at or near full white.
    pub white_points: u64,
    /// Smallest luma present, when the frame has any pixels.
    pub min_level: Option<u8>,
    /// Largest luma present, when the frame has any pixels.
    pub max_level: Option<u8>,
}

/// The number of bytes an interleaved 8-bit RGB frame of these dimensions needs.
///
/// # Why this exists as one function
///
/// Every consumer of a decoded frame needs the same figure, and computing it
/// inline three times produced three different overflow behaviours: each site
/// guarded `width * height` with `checked_mul` and then multiplied by three
/// *unchecked*. On a 64-bit target `u32::MAX * u32::MAX` passes the guard and
/// the `* 3` wraps, so a frame declaring 65536-square dimensions was reported
/// as needing 3 bytes — after which the viewer indexed a 3-byte buffer as
/// though it held four billion pixels.
///
/// Those dimensions come from a decoder fed attacker-controlled bytes (spec
/// §75), so the product is computed once, checked at both steps, and returned
/// as `None` when it does not fit. A frame that cannot be measured is refused;
/// it is never measured wrongly.
fn rgb_byte_len(width: u32, height: u32) -> Option<usize> {
    let width = width as usize;
    let height = height as usize;
    if width == 0 || height == 0 {
        return None;
    }
    width.checked_mul(height)?.checked_mul(3)
}

/// A luma value within this many levels of full black counts as black.
///
/// Two levels rather than one, because a legal-range video black sits at 16 and
/// a full-range one at 0; a single threshold would call one of them clipped.
const BLACK_LEVEL: u8 = 2;

/// A luma value at or above this counts as white.
const WHITE_LEVEL: u8 = 253;

impl Histogram {
    /// Builds a histogram from an interleaved RGB buffer.
    ///
    /// Returns `None` for an empty or malformed buffer rather than an
    /// all-zero histogram. Those mean different things: "this frame has no
    /// pixels" versus "every pixel in this frame is black", and a viewer that
    /// drew both as an empty chart would report a black frame as no frame.
    #[must_use]
    pub fn from_rgb(rgb: &[u8], width: u32, height: u32) -> Option<Self> {
        let needed = rgb_byte_len(width, height)?;
        if rgb.len() < needed {
            return None;
        }
        let pixels = needed / 3;

        let mut bins = vec![0u32; 256];
        let mut total = 0u64;
        let mut black_points = 0u64;
        let mut white_points = 0u64;
        let mut min_level = u8::MAX;
        let mut max_level = u8::MIN;

        for chunk in rgb.chunks_exact(3).take(pixels) {
            let (r, g, b) = (chunk[0], chunk[1], chunk[2]);
            // Rec. 601 luma in integer arithmetic: the same weighting the
            // engine's greyscale evidence frame uses, so the histogram and the
            // extracted evidence agree about what "luma" means.
            let y = ((77 * i32::from(r) + 150 * i32::from(g) + 29 * i32::from(b)) >> 8)
                .clamp(0, 255) as u8;

            bins[usize::from(y)] += 1;
            total += 1;
            black_points += u64::from(y <= BLACK_LEVEL);
            white_points += u64::from(y >= WHITE_LEVEL);
            min_level = min_level.min(y);
            max_level = max_level.max(y);
        }

        Some(Self {
            bins,
            total,
            black_points,
            white_points,
            min_level: (total > 0).then_some(min_level),
            max_level: (total > 0).then_some(max_level),
        })
    }

    /// The fraction of pixels at or near full black, 0.0 to 1.0.
    ///
    /// Reported as a fraction rather than a count because that is the figure a
    /// delivery specification thresholds on, and a count without the frame size
    /// beside it cannot be compared between a thumbnail and a 4K master.
    #[must_use]
    pub fn black_fraction(&self) -> f64 {
        self.fraction(self.black_points)
    }

    /// The fraction of pixels at or near full white, 0.0 to 1.0.
    #[must_use]
    pub fn white_fraction(&self) -> f64 {
        self.fraction(self.white_points)
    }

    /// Divides a count by the total, returning zero for an empty histogram.
    fn fraction(&self, count: u64) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            count as f64 / self.total as f64
        }
    }

    /// The largest bin, used by the renderer to scale the chart.
    #[must_use]
    pub fn peak(&self) -> u32 {
        self.bins.iter().copied().max().unwrap_or(0)
    }
}

/// One column of the waveform display (spec §43).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WaveformColumn {
    /// Lowest sample in this column, -1.0 to 1.0.
    pub min: f32,
    /// Highest sample in this column, -1.0 to 1.0.
    pub max: f32,
    /// Mean absolute level in this column, 0.0 to 1.0.
    ///
    /// Carried alongside the extremes because a column that spans the full
    /// range and one that is briefly loud differ in ways the extremes alone
    /// cannot show.
    pub rms: f32,
}

/// A peak envelope of an audio signal (spec §43).
///
/// # An envelope, and labelled as one
///
/// Min/max per column, not the samples themselves. A 48 kHz stereo file is
/// 192,000 values per second; sending those across the IPC boundary to draw a
/// few hundred columns would be slow for no gain, because the envelope is what a
/// waveform display shows anyway.
///
/// The column count is therefore the display resolution, and [`Waveform::pcm`]
/// is recorded so a reviewer knows the picture was downsampled rather than
/// measured at that width.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Waveform {
    /// The columns, in time order.
    pub columns: Vec<WaveformColumn>,
    /// Samples per column, before interleave.
    pub samples_per_column: u32,
    /// Samples the envelope was built from, per channel.
    pub pcm_length: u32,
    /// Channel count of the source.
    pub channels: u16,
    /// Highest peak anywhere in the signal, 0.0 to 1.0.
    ///
    /// Read from the signal rather than assumed to be 1.0. A renderer that
    /// normalised against an assumed full scale would render a quiet track as
    /// loud, which is the difference between "this file is quiet" and "this file
    /// is fine" to anyone using it for QC.
    pub peak: f32,
}

impl Waveform {
    /// Builds a peak envelope from interleaved `f32` samples.
    ///
    /// Returns `None` for an empty signal or a zero channel count. Both are
    /// "there is no audio to draw", which the viewer says, rather than an empty
    /// chart that looks like digital silence was measured.
    #[must_use]
    pub fn from_pcm(pcm: &[f32], channels: u16, columns: usize) -> Option<Self> {
        if pcm.is_empty() || channels == 0 || columns == 0 {
            return None;
        }
        // The declared channel count is kept as `u16` because it is what the
        // struct reports; a separate `usize` drives indexing below.
        let declared_channels = channels;
        let channels = usize::from(channels);
        if pcm.len() < channels {
            return None;
        }

        // Frames, not interleaved samples: one column must span the same amount
        // of time regardless of channel count, or a stereo file's display would
        // be twice as long as a mono file's for the same duration.
        let frames = pcm.len() / channels;
        if frames == 0 {
            return None;
        }
        let samples_per_column = frames.div_ceil(columns).max(1);

        let mut out = Vec::with_capacity(columns);
        let mut peak = 0.0f32;

        for column in 0..columns {
            let start = column * samples_per_column;
            if start >= frames {
                break;
            }
            let end = (start + samples_per_column).min(frames);

            let mut min = f32::MAX;
            let mut max = f32::MIN;
            let mut sum_squares = 0.0f64;
            let mut count = 0u64;

            for frame in start..end {
                // Every channel of the frame contributes, so a column's extremes
                // describe the frame rather than one arbitrary channel of it.
                for channel in 0..channels {
                    let sample = pcm[frame * channels + channel];
                    if sample.is_finite() {
                        min = min.min(sample);
                        max = max.max(sample);
                        peak = peak.max(sample.abs());
                        sum_squares += f64::from(sample) * f64::from(sample);
                        count += 1;
                    }
                }
            }

            if count == 0 {
                // Every sample in this column was NaN or infinite. Rendered as a
                // zero-height column rather than skipped, so the display keeps
                // its time axis and the gap stays visible instead of closing up.
                out.push(WaveformColumn {
                    min: 0.0,
                    max: 0.0,
                    rms: 0.0,
                });
                continue;
            }

            out.push(WaveformColumn {
                min,
                max,
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                rms: (sum_squares / count as f64).sqrt() as f32,
            });
        }

        Some(Self {
            columns: out,
            samples_per_column: u32::try_from(samples_per_column).unwrap_or(u32::MAX),
            pcm_length: u32::try_from(frames).unwrap_or(u32::MAX),
            channels: declared_channels,
            peak,
        })
    }

    /// Whether the signal contains nothing above the dBFS floor.
    ///
    /// The floor is below what a 32-bit float can represent distinctly from
    /// zero, so reporting true digital silence as merely "quiet" would be a
    /// measurement the sample format cannot support.
    #[must_use]
    pub fn is_silent(&self) -> bool {
        self.peak <= SILENCE_FLOOR
    }
}

/// The amplitude below which a float sample is treated as digital silence.
const SILENCE_FLOOR: f32 = 1e-6;

/// How two frames compare (spec §43, §82).
///
/// # `NotComparable` is the honest answer
///
/// Two frames of different dimensions cannot be differenced, and reporting them
/// as "identical" would be false — but so would reporting a difference. This
/// mirrors the engine's own `Difference::NotComparable` (spec §38), and carries
/// the same reasoning: "could not measure" and "measured as equal" are
/// different claims, and only one of them is a measurement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum FrameDifference {
    /// Both frames have identical pixels.
    Identical,
    /// Both frames were compared and differ by the given amount.
    Different {
        /// Mean absolute difference per channel, 0.0 to 255.0.
        mean_absolute: f64,
        /// Proportion of pixels differing by more than a small threshold.
        changed_fraction: f64,
        /// Largest single-channel difference observed, 0-255.
        max_delta: u8,
    },
    /// The two frames could not be compared.
    NotComparable {
        /// Why.
        reason: String,
    },
}

impl FrameDifference {
    /// Compares two frames' pixels.
    ///
    /// # The threshold is a named constant, not a parameter
    ///
    /// `SAMPLE_DELTA` matches the value `-video::scene` uses, and for the same
    /// reason: a handful of levels is compression noise, and counting it as
    /// change would report every real file as comprehensively different. If this
    /// and the scene analyser disagreed, the same cut would be "one scene
    /// change" in Findings and "10% of the frame changed" in the viewer.
    #[must_use]
    pub fn compare(left: &FrameImageView, right: &FrameImageView) -> Self {
        if left.width != right.width || left.height != right.height {
            return Self::NotComparable {
                reason: format!(
                    "frames are {}x{} and {}x{}; pixels cannot be compared across \
                     different dimensions",
                    left.width, left.height, right.width, right.height
                ),
            };
        }
        match (left.pixels(), right.pixels()) {
            (Some(a), Some(b)) => Self::compare_pixels(&a, &b),
            _ => Self::NotComparable {
                reason: "one frame's pixel data was not available to compare".to_owned(),
            },
        }
    }

    /// Compares two equally-sized RGB buffers.
    fn compare_pixels(left: &[u8], right: &[u8]) -> Self {
        if left.len() != right.len() || left.is_empty() {
            return Self::NotComparable {
                reason: "the frames' pixel buffers are not the same length".to_owned(),
            };
        }

        let mut total = 0f64;
        let mut changed = 0u64;
        let mut max_delta = 0u8;

        for (a, b) in left.iter().zip(right) {
            let delta = a.abs_diff(*b);
            max_delta = max_delta.max(delta);
            total += f64::from(delta);
            changed += u64::from(delta > SAMPLE_DELTA);
        }

        let samples = left.len() as f64;
        let mean_absolute = total / samples;
        let changed_fraction = changed as f64 / samples;

        // A pixel-difference of zero across every channel is a measurement, not
        // an absence of one, so `Identical` is correct here even though
        // `changed_fraction` is also zero. The distinction that matters is
        // against `NotComparable`.
        if max_delta == 0 {
            Self::Identical
        } else {
            Self::Different {
                mean_absolute,
                changed_fraction,
                max_delta,
            }
        }
    }
}

/// A per-sample difference this build ignores: compression noise.
const SAMPLE_DELTA: u8 = 16;

/// A decoded frame as the viewer sees it.
///
/// `pixels` is `None` when the frame was located but its data was not
/// retained — for instance when the engine measured it and discarded the
/// buffer. That is different from a frame that could not be decoded, and the
/// viewer shows it differently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameImageView {
    /// Position of this frame in the sequence.
    pub frame_index: u64,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// When the frame is presented.
    pub time: MediaTime,
    /// Interleaved 8-bit RGB, when retained.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rgb: Option<Vec<u8>>,
    /// Where the RGB came from.
    pub basis: RgbBasis,
    /// Path to an extracted PNG, when the frame was written as evidence.
    pub evidence_path: Option<String>,
}

impl FrameImageView {
    /// A frame whose pixels are not retained.
    #[must_use]
    pub fn without_pixels(frame_index: u64, width: u32, height: u32, time: MediaTime) -> Self {
        Self {
            frame_index,
            width,
            height,
            time,
            rgb: None,
            basis: RgbBasis::LumaOnly,
            evidence_path: None,
        }
    }

    /// The pixel buffer, when there is one and it is the right length.
    ///
    /// Checked rather than returned directly: a frame whose declared dimensions
    /// disagree with its buffer is a malformed frame (spec §75), and indexing it
    /// is how a viewer turns corrupt media into a crash.
    fn pixels(&self) -> Option<Vec<u8>> {
        let expected = rgb_byte_len(self.width, self.height)?;
        let rgb = self.rgb.as_ref()?;
        (rgb.len() >= expected).then(|| rgb[..expected].to_vec())
    }

    /// Whether this frame's pixels are available to the inspector and histogram.
    #[must_use]
    pub fn has_pixels(&self) -> bool {
        self.pixels().is_some()
    }
}

/// The viewer's position and zoom within one asset (spec §43).
///
/// # Why this is a value, not a variable in the frontend
///
/// Frame stepping has rules that are easy to get subtly wrong: stepping back
/// from the first frame must stay at the first frame rather than wrap or go
/// negative; jumping to a keyframe must land on the keyframe *before* the
/// cursor, not the one after; and a jump to a time past the end must clamp
/// rather than fail. Each of those is a decision, and each is testable here.
///
/// Holding the cursor on the Rust side also means the same position survives a
/// window reload and a frontend bug, because there is nothing in the frontend
/// that owns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewerState {
    /// Index of the frame under the cursor.
    pub frame_index: u64,
    /// Total frames available, when known.
    pub frame_count: u64,
    /// Current magnification.
    pub zoom: ZoomLevel,
    /// Which side of an A/B comparison is showing, when one is active.
    pub ab_side: Option<AbSide>,
}

impl ViewerState {
    /// A viewer at the first frame, fit to the viewport.
    #[must_use]
    pub fn new(frame_count: u64) -> Self {
        Self {
            frame_index: 0,
            frame_count,
            zoom: ZoomLevel::Fit,
            ab_side: None,
        }
    }

    /// Moves the cursor, clamping to the sequence.
    ///
    /// Saturating rather than wrapping. Wrapping from the last frame to the
    /// first is what a playlist does; in a forensic viewer it would silently
    /// move the analyst's reference point while they were looking away, and the
    /// next frame they examined would not be the one they intended.
    ///
    /// Time- and keyframe-based steps are no-ops here: they need the sequence,
    /// which this method is not given. Use [`ViewerState::step_with`].
    pub fn step(&mut self, step: FrameStep) {
        let last = self.frame_count.saturating_sub(1);
        self.frame_index = match step {
            FrameStep::Forward => self.frame_index.saturating_add(1).min(last),
            FrameStep::Back => self.frame_index.saturating_sub(1),
            FrameStep::Start => 0,
            FrameStep::End => last,
            FrameStep::ForwardSecond
            | FrameStep::BackSecond
            | FrameStep::NextKeyFrame
            | FrameStep::PreviousKeyFrame => self.frame_index,
        };
    }

    /// Moves the cursor with the sequence in hand, for time- and keyframe-based
    /// steps.
    ///
    /// Returns the index actually landed on.
    #[must_use]
    pub fn step_with(&mut self, step: FrameStep, frames: &[FrameStamp], key_frames: &[u64]) -> u64 {
        let last = self.frame_count.saturating_sub(1);
        self.frame_index = match step {
            FrameStep::Forward => self.frame_index.saturating_add(1).min(last),
            FrameStep::Back => self.frame_index.saturating_sub(1),
            FrameStep::Start => 0,
            FrameStep::End => last,
            FrameStep::ForwardSecond => next_second(frames, self.frame_index, last, true),
            FrameStep::BackSecond => next_second(frames, self.frame_index, last, false),
            FrameStep::NextKeyFrame => next_key_frame(key_frames, self.frame_index, last),
            FrameStep::PreviousKeyFrame => previous_key_frame(key_frames, self.frame_index),
        };
        self.frame_index
    }

    /// Jumps to the frame nearest a media time (spec §81).
    ///
    /// Clamps a time past the end to the last frame rather than refusing: the
    /// analyst clicked a position, and the nearest frame to it is the answer
    /// even when the click overshot.
    #[must_use]
    pub fn jump_to_time(&mut self, frames: &[FrameStamp], time: MediaTime) -> u64 {
        let last = self.frame_count.saturating_sub(1);
        let Some(nearest) = frames
            .iter()
            .min_by_key(|stamp| (stamp.time.as_micros() - time.as_micros()).abs())
        else {
            // No sequence to search. The cursor stays where it was rather than
            // jumping to zero, which would silently move the analyst's reference
            // point to the start of a file they were somewhere in the middle of.
            return self.frame_index;
        };
        self.frame_index = nearest.index.min(last);
        self.frame_index
    }

    /// Zooms in one step, saturating at the finest level.
    pub fn zoom_in(&mut self) {
        self.zoom = self.zoom.zoom_in();
    }

    /// Zooms out one step, saturating at the coarsest.
    pub fn zoom_out(&mut self) {
        self.zoom = self.zoom.zoom_out();
    }

    /// Sets the magnification directly.
    pub fn set_zoom(&mut self, zoom: ZoomLevel) {
        self.zoom = zoom;
    }
}

/// The minimum position change, in microseconds, that a timed step accepts.
///
/// One second. The constant this replaced was named "epsilon" and held half a
/// frame, and the two disagreed: a "step one second" satisfied by moving a
/// single frame is not a one-second step. The viewer's own tests caught it
/// landing on frame 1 of 120 after being asked to skip a second.
///
/// A whole second rather than a tolerance around one. Rounding the target to
/// the nearest frame is already what selecting the first surviving frame does,
/// so the only decision here is *which side of the mark* to fall on, and
/// falling short of it is the surprising answer for a control labelled in
/// seconds.
const ONE_SECOND_US: i64 = 1_000_000;

/// Finds the frame at least one second away in the given direction.
fn next_second(frames: &[FrameStamp], current: u64, last: u64, forwards: bool) -> u64 {
    let Some(here) = frames.iter().find(|stamp| stamp.index == current) else {
        return current;
    };
    let here_time = here.time.as_micros();

    // `min_by_key` returns the *first* of equal minima, and ordering by the
    // absolute timestamp would make every frame past the threshold tie on
    // "is this the earliest". Sorting by the signed distance from `here_time`
    // instead is what selects the frame actually nearest one second away: the
    // filter has already excluded everything closer, so the smallest surviving
    // key is the right one.
    let candidate = if forwards {
        frames
            .iter()
            .filter(|stamp| stamp.time.as_micros() - here_time >= ONE_SECOND_US)
            .min_by_key(|stamp| stamp.time.as_micros() - here_time)
    } else {
        frames
            .iter()
            .filter(|stamp| here_time - stamp.time.as_micros() >= ONE_SECOND_US)
            .min_by_key(|stamp| here_time - stamp.time.as_micros())
    };

    candidate.map_or(last.min(current), |stamp| stamp.index.min(last))
}

/// Finds the next keyframe strictly after `current`.
///
/// Strictly, because the keyframe under the cursor is not the next one — an
/// analyst stepping forward expects to move.
fn next_key_frame(key_frames: &[u64], current: u64, last: u64) -> u64 {
    key_frames
        .iter()
        .copied()
        .find(|index| *index > current)
        .unwrap_or(current)
        .min(last)
}

/// Finds the previous keyframe strictly before `current`.
fn previous_key_frame(key_frames: &[u64], current: u64) -> u64 {
    key_frames
        .iter()
        .copied()
        .filter(|index| *index < current)
        .max()
        .unwrap_or(current)
}

/// Which side of an A/B comparison is displayed (spec §43).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbSide {
    /// The asset under examination.
    Original,
    /// The asset being compared against it.
    Comparison,
}

impl AbSide {
    /// The other side, for the blink and toggle controls.
    #[must_use]
    pub const fn flip(self) -> Self {
        match self {
            Self::Original => Self::Comparison,
            Self::Comparison => Self::Original,
        }
    }

    /// Returns the label drawn above each pane.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Original => "Original",
            Self::Comparison => "Comparison",
        }
    }
}

/// The bare facts about one frame that navigation needs.
///
/// Separate from [`FrameImageView`] because navigation must work on a long
/// sequence without holding every frame's pixels in memory — a 4K master has
/// tens of thousands of frames, and the viewer only ever displays one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameStamp {
    /// Position of this frame in the sequence.
    pub index: u64,
    /// When the frame is presented.
    pub time: MediaTime,
    /// Whether this frame is a random-access point.
    pub is_key_frame: bool,
}

/// The A/B comparison state (spec §43).
///
/// Both sides are held at the same frame index so spec §82's paired layout —
/// `frame 1042` against `frame 1042` — is the default. An analyst comparing a
/// suspect frame against a reference is almost always asking "is this the same
/// picture", and unpaired frames answer a different question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AbComparison {
    /// The frame under examination.
    pub original: FrameStamp,
    /// The frame it is compared against.
    pub comparison: FrameStamp,
    /// Which pane is showing.
    pub side: AbSide,
    /// How the two compare.
    pub difference: FrameDifference,
}

impl AbComparison {
    /// Builds a comparison from the engine's frame comparison.
    #[must_use]
    pub fn new(original: FrameStamp, comparison: FrameStamp, difference: FrameDifference) -> Self {
        Self {
            original,
            comparison,
            side: AbSide::Original,
            difference,
        }
    }

    /// Returns the other pane's stamp, for the blink control.
    #[must_use]
    pub fn stamp_for(&self, side: AbSide) -> &FrameStamp {
        match side {
            AbSide::Original => &self.original,
            AbSide::Comparison => &self.comparison,
        }
    }

    /// The presentation-time difference between the two sides.
    ///
    /// This is one of the numbers spec §82 shows directly: two frames that look
    /// identical can carry different PTS values, and that difference is often
    /// the entire finding.
    #[must_use]
    pub fn pts_delta(&self) -> MediaTime {
        MediaTime::from_micros(self.comparison.time.as_micros() - self.original.time.as_micros())
    }

    /// Whether the two sides show the same frame number.
    ///
    /// Reported so the UI can say "frame 1042 vs 1043" rather than implying the
    /// panes are synchronised when they are not.
    #[must_use]
    pub fn is_frame_aligned(&self) -> bool {
        self.original.index == self.comparison.index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(width: u32, height: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        let mut out = Vec::new();
        for y in 0..height {
            for x in 0..width {
                out.extend_from_slice(&f(x, y));
            }
        }
        out
    }

    fn frame(width: u32, height: u32, pixels: Vec<u8>) -> FrameImageView {
        FrameImageView {
            frame_index: 0,
            width,
            height,
            time: MediaTime::from_millis(1_000),
            rgb: Some(pixels),
            basis: RgbBasis::Bt601Matrix,
            evidence_path: None,
        }
    }

    // -------------------------------------------------------------- stepping

    #[test]
    fn stepping_back_from_the_first_frame_stays_there() {
        // Not wrap-around, not an error, and never an underflow to `u64::MAX`,
        // which would ask for a frame that cannot exist.
        let mut state = ViewerState::new(10);
        state.step(FrameStep::Back);
        assert_eq!(state.frame_index, 0);
    }

    #[test]
    fn stepping_forward_past_the_last_frame_stays_at_the_end() {
        let mut state = ViewerState::new(10);
        state.step(FrameStep::End);
        state.step(FrameStep::Forward);
        assert_eq!(state.frame_index, 9);
    }

    #[test]
    fn an_empty_sequence_has_exactly_one_addressable_position() {
        // `frame_count: 0` must not produce `frame_count - 1` wrapping to the
        // largest possible index.
        let mut state = ViewerState::new(0);
        assert_eq!(state.frame_index, 0);
        state.step(FrameStep::End);
        state.step(FrameStep::Forward);
        state.step(FrameStep::Back);
        assert_eq!(state.frame_index, 0);
    }

    #[test]
    fn a_time_based_step_is_a_no_op_without_the_sequence() {
        // `step` is given no frame list, so it must not invent one. Moving here
        // would be a guess.
        let mut state = ViewerState::new(100);
        state.frame_index = 40;
        state.step(FrameStep::ForwardSecond);
        assert_eq!(state.frame_index, 40);
    }

    // ------------------------------------------------------------ timestamps

    fn stamps(count: u64, key_every: u64) -> Vec<FrameStamp> {
        (0..count)
            .map(|i| FrameStamp {
                index: i,
                // 33 ms per frame: close enough to 30.303 fps that a one-second
                // step lands on 30 frames, which is what the test below checks.
                time: MediaTime::from_millis(i64::try_from(i).unwrap_or(0) * 33),
                is_key_frame: i % key_every == 0,
            })
            .collect()
    }

    #[test]
    fn pts_and_dts_are_shown_side_by_side_because_they_disagree() {
        // Spec §43 asks for both, and the disagreement between them is often the
        // finding: reordering is invisible if only PTS is displayed.
        let stamps = FrameTimestamps::new(
            MediaTime::from_millis(40),
            Some(MediaTime::from_millis(0)),
            7,
            Timebase::from_ticks_per_second(1_000),
            40,
            Some(0),
            false,
        );
        assert_eq!(stamps.pts, MediaTime::from_millis(40));
        assert_eq!(stamps.dts, Some(MediaTime::from_millis(0)));
        assert!(stamps.is_reordered());
        assert_eq!(
            stamps.reorder_delay(),
            Some(MediaTime::from_millis(40)),
            "the delay is what makes the reordering legible"
        );
    }

    #[test]
    fn an_absent_dts_is_not_reported_as_zero() {
        // Zero would be a real timecode saying the frame decodes first, which
        // is a claim about the stream rather than an absence.
        let stamps = FrameTimestamps::new(
            MediaTime::from_millis(40),
            None,
            7,
            Timebase::from_ticks_per_second(1_000),
            40,
            None,
            true,
        );
        assert_eq!(stamps.dts, None);
        assert_eq!(stamps.reorder_delay(), None);
        assert!(!stamps.is_reordered());
    }

    #[test]
    fn a_dts_given_only_in_ticks_is_derived_rather_than_dropped() {
        // Otherwise the display would show a tick count with no timecode beside
        // it, and the two views of the same value could not be compared.
        let stamps = FrameTimestamps::new(
            MediaTime::from_millis(40),
            None,
            7,
            Timebase::from_ticks_per_second(1_000),
            40,
            Some(0),
            false,
        );
        assert_eq!(stamps.dts, Some(MediaTime::from_millis(0)));
    }

    #[test]
    fn declared_ticks_are_carried_verbatim() {
        // A reviewer compares the viewer's timecode against the value in the
        // file; a recomputed one could not be checked.
        let stamps = FrameTimestamps::new(
            MediaTime::from_millis(40),
            None,
            7,
            Timebase::from_ticks_per_second(1_000),
            40,
            None,
            true,
        );
        assert_eq!(stamps.pts_ticks, 40);
        assert!(stamps.is_key_frame);
    }

    #[test]
    fn a_one_second_step_moves_at_least_one_second_not_merely_one_frame() {
        // The threshold is a *minimum*, not a target: the guarantee is that the
        // cursor moves a whole second, so the first frame at or past that mark
        // is correct. Asserting "about one second" would be asserting an
        // approximation the code does not promise and should not.
        let frames = stamps(120, 30);
        let mut state = ViewerState::new(120);
        let landed = state.step_with(FrameStep::ForwardSecond, &frames, &[]);

        let elapsed = frames[landed as usize].time.as_micros() - frames[0].time.as_micros();
        assert!(
            elapsed >= 1_000_000,
            "expected at least one second, got {elapsed} us at frame {landed}"
        );
    }

    #[test]
    fn a_one_second_step_lands_on_the_first_frame_past_the_mark() {
        // 33 ms per frame: frame 30 is 990 ms (short of a second) and frame 31
        // is 1023 ms (past it). The answer is 31, not 30 and not 32.
        let frames = stamps(120, 30);
        let mut state = ViewerState::new(120);
        assert_eq!(state.step_with(FrameStep::ForwardSecond, &frames, &[]), 31);
    }

    #[test]
    fn a_one_second_step_never_lands_on_the_frame_it_started_from() {
        // The failure this guards against is a step that silently does nothing
        // because the sequence's rate is slightly under the nominal one, which
        // for NTSC-derived 29.97 it always is.
        let frames = stamps(120, 30);
        let mut state = ViewerState::new(120);
        state.frame_index = 10;
        let landed = state.step_with(FrameStep::ForwardSecond, &frames, &[]);
        assert_ne!(landed, 10);
    }

    #[test]
    fn a_one_second_step_back_actually_moves_back() {
        let frames = stamps(120, 30);
        let mut state = ViewerState::new(120);
        state.frame_index = 90;
        let landed = state.step_with(FrameStep::BackSecond, &frames, &[]);
        assert!(landed < 90, "landed on {landed}");
    }

    #[test]
    fn a_one_second_step_at_the_end_clamps_rather_than_failing() {
        let frames = stamps(10, 30);
        let mut state = ViewerState::new(10);
        state.frame_index = 9;
        let landed = state.step_with(FrameStep::ForwardSecond, &frames, &[]);
        assert_eq!(landed, 9);
    }

    #[test]
    fn the_next_keyframe_is_strictly_after_the_cursor() {
        // An analyst stepping forward expects to move; landing back on the
        // keyframe under the cursor would look like the key did nothing.
        let frames = stamps(120, 30);
        let keys = [0u64, 30, 60, 90];
        let mut state = ViewerState::new(120);
        state.frame_index = 30;
        assert_eq!(state.step_with(FrameStep::NextKeyFrame, &frames, &keys), 60);
    }

    #[test]
    fn the_previous_keyframe_is_strictly_before_the_cursor() {
        let frames = stamps(120, 30);
        let keys = [0u64, 30, 60, 90];
        let mut state = ViewerState::new(120);
        state.frame_index = 60;
        assert_eq!(
            state.step_with(FrameStep::PreviousKeyFrame, &frames, &keys),
            30
        );
    }

    #[test]
    fn a_keyframe_step_with_no_keyframes_does_nothing() {
        // A stream with no random-access points at all is a measurement, not a
        // licence to jump somewhere arbitrary.
        let frames = stamps(30, 30);
        let mut state = ViewerState::new(30);
        state.frame_index = 10;
        assert_eq!(state.step_with(FrameStep::NextKeyFrame, &frames, &[]), 10);
    }

    #[test]
    fn jumping_to_a_time_past_the_end_lands_on_the_last_frame() {
        let frames = stamps(30, 30);
        let mut state = ViewerState::new(30);
        assert_eq!(
            state.jump_to_time(&frames, MediaTime::from_millis(600_000)),
            29
        );
    }

    #[test]
    fn jumping_with_no_sequence_leaves_the_cursor_alone() {
        let mut state = ViewerState::new(30);
        state.frame_index = 12;
        assert_eq!(state.jump_to_time(&[], MediaTime::from_millis(500)), 12);
    }

    // ------------------------------------------------------------------ zoom

    #[test]
    fn zoom_saturates_at_both_ends() {
        let mut state = ViewerState::new(30);
        assert_eq!(state.zoom, ZoomLevel::Fit);
        state.zoom_out();
        assert_eq!(state.zoom, ZoomLevel::Fit, "already the coarsest");

        for _ in 0..10 {
            state.zoom_in();
        }
        assert_eq!(state.zoom, ZoomLevel::Octuple, "already the finest");
    }

    #[test]
    fn fit_is_not_reported_as_a_magnification() {
        // `None` rather than `1`: the whole point of zooming is to read
        // individual pixels, and "fit" is a viewport-dependent size.
        assert_eq!(ZoomLevel::Fit.scale(), None);
        assert_eq!(ZoomLevel::Actual.scale(), Some(1));
    }

    #[test]
    fn every_zoom_level_has_a_distinct_scale_or_is_fit() {
        let mut seen = Vec::new();
        for level in ZoomLevel::ALL {
            seen.push(level.scale());
            assert!(!level.label().is_empty());
        }
        let mut concrete = seen.clone();
        concrete.retain(Option::is_some);
        concrete.dedup();
        assert_eq!(
            concrete.len(),
            4,
            "the four fixed levels must differ: {seen:?}"
        );
    }

    // --------------------------------------------------------- pixel inspector

    #[test]
    fn the_inspector_reads_the_pixel_under_the_pointer() {
        let pixels = rgb(4, 4, |x, y| [x as u8 * 10, y as u8 * 10, 200]);
        let sample = PixelSample::read(
            &pixels,
            4,
            4,
            2,
            3,
            RgbBasis::Bt601Matrix,
            Some("BT.709".to_owned()),
        )
        .expect("in range");

        assert_eq!((sample.x, sample.y), (2, 3));
        assert_eq!((sample.r, sample.g, sample.b), (20, 30, 200));
    }

    #[test]
    fn the_inspector_refuses_a_coordinate_outside_the_frame() {
        // Coordinates come from a mouse event over a canvas; a click past the
        // edge is normal, not exceptional.
        let pixels = rgb(4, 4, |_, _| [1, 2, 3]);
        assert!(PixelSample::read(&pixels, 4, 4, 4, 0, RgbBasis::LumaOnly, None).is_none());
        assert!(PixelSample::read(&pixels, 4, 4, 0, 4, RgbBasis::LumaOnly, None).is_none());
    }

    #[test]
    fn the_inspector_refuses_a_buffer_shorter_than_the_frame_declares() {
        // Spec §75: a decoded frame's dimensions are attacker-controlled. The
        // refusal must happen before any indexing.
        let pixels = vec![7u8; 10];
        assert!(PixelSample::read(&pixels, 100, 100, 0, 0, RgbBasis::LumaOnly, None).is_none());
    }

    #[test]
    fn the_inspector_refuses_a_zero_sized_frame() {
        assert!(PixelSample::read(&[], 0, 0, 0, 0, RgbBasis::LumaOnly, None).is_none());
    }

    #[test]
    fn a_greyscale_reading_says_its_chroma_values_mean_nothing() {
        // Cb and Cr are 128 by construction here. Shown without that warning, a
        // reviewer would conclude the content is colour-neutral.
        let pixels = vec![128u8; 12];
        let sample =
            PixelSample::read(&pixels, 2, 2, 0, 0, RgbBasis::LumaOnly, None).expect("in range");

        assert!(!sample.basis.has_chroma());
        let note = sample.conversion_note();
        assert!(note.contains("128 by construction"), "{note}");
    }

    #[test]
    fn the_note_names_the_matrix_the_reading_used() {
        // Spec §44: do not convert silently.
        let pixels = vec![100u8; 12];
        let sample = PixelSample::read(
            &pixels,
            2,
            2,
            0,
            0,
            RgbBasis::Bt601Matrix,
            Some("BT.709".to_owned()),
        )
        .expect("in range");

        let note = sample.conversion_note();
        assert!(note.contains("BT.601"), "{note}");
        assert!(note.contains("BT.709"), "{note}");
    }

    #[test]
    fn an_undeclared_colour_space_is_not_guessed() {
        let pixels = vec![100u8; 12];
        let sample =
            PixelSample::read(&pixels, 2, 2, 0, 0, RgbBasis::Bt601Matrix, None).expect("in range");
        assert!(
            sample
                .conversion_note()
                .contains("declared no colour space"),
            "the inspector must say it is assuming nothing"
        );
    }

    #[test]
    fn neutral_grey_has_neutral_chroma() {
        let (y, cb, cr) = rgb_to_ycbcr(128, 128, 128);
        assert!((120..=136).contains(&y), "luma {y}");
        assert!((124..=132).contains(&cb), "cb {cb}");
        assert!((124..=132).contains(&cr), "cr {cr}");
    }

    #[test]
    fn the_ycbcr_conversion_clamps_rather_than_wrapping() {
        // Fixed-point arithmetic must saturate at the ends of the range, or a
        // saturated pixel would report a negative-looking chroma. The
        // interesting cases are the extremes: a neutral grey must stay neutral
        // and the ends of the scale must land on the ends of the scale.
        assert_eq!(rgb_to_ycbcr(255, 255, 255), (255, 128, 128));
        assert_eq!(rgb_to_ycbcr(0, 0, 0), (0, 128, 128));

        // A saturated primary pushes chroma hard; it must saturate at the
        // representable bounds rather than wrapping to a small positive value,
        // which would make pure blue read as mildly red.
        let (_, cb, _) = rgb_to_ycbcr(0, 0, 255);
        assert_eq!(cb, 255, "pure blue must saturate the chroma axis");
        let (_, _, cr) = rgb_to_ycbcr(255, 0, 0);
        assert_eq!(cr, 255, "pure red must saturate the other chroma axis");
    }

    #[test]
    fn the_ycbcr_conversion_is_monotonic_in_luma() {
        // Doubling a grey must not darken it, and darkening must not lighten
        // it. A wrap in the fixed-point terms would break both.
        let mut previous = 0u8;
        for step in 0..=25u8 {
            let level = step.saturating_mul(10);
            let (y, ..) = rgb_to_ycbcr(level, level, level);
            assert!(
                y >= previous,
                "luma fell at level {level}: {previous} -> {y}"
            );
            previous = y;
        }
    }

    // ------------------------------------------------------------- histogram

    #[test]
    fn dimensions_whose_product_overflows_are_refused_not_wrapped() {
        // The regression this pins: `u32::MAX * u32::MAX` fits a `usize`, so a
        // `checked_mul` on the pixel count passed, and the `* 3` for bytes then
        // wrapped to a small number. A frame declaring 65536-square dimensions
        // was measured as three bytes long, and the inspector went on to index
        // a three-byte buffer. Three separate call sites had the same shape.
        // Only the genuinely-overflowing pair is refused by the size check. On a
        // 64-bit target `u32::MAX * 2` and even 65536-square both *fit* in a
        // `usize`, so they are real (if absurd) frame sizes whose correct
        // answer is "needs more bytes than you have" rather than "cannot be
        // measured". Both refusals below arrive by that second route.
        assert!(
            rgb_byte_len(u32::MAX, u32::MAX).is_none(),
            "two 32-bit dimensions squared must overflow the byte count"
        );

        // And every oversized or undersized declaration is refused by the
        // callers, which is the property that actually prevents the crash.
        for (width, height) in [
            (u32::MAX, u32::MAX),
            (u32::MAX, 2),
            (2, u32::MAX),
            (65_536, 65_536),
            (4_096, 4_096),
        ] {
            assert!(
                PixelSample::read(&[0u8; 3], width, height, 0, 0, RgbBasis::Bt601Matrix, None)
                    .is_none(),
                "the inspector must refuse {width}x{height} against a three-byte buffer"
            );
            assert!(
                Histogram::from_rgb(&[0u8; 3], width, height).is_none(),
                "the histogram must refuse {width}x{height} against a three-byte buffer"
            );
        }
    }

    #[test]
    fn a_frame_with_no_pixels_reports_itself_as_unreadable_rather_than_identical() {
        // A frame whose buffer does not match its declared size cannot be
        // differenced. Reporting `Identical` would tell a reviewer two frames
        // match when neither could be read.
        let declared = FrameImageView {
            frame_index: 0,
            width: 4,
            height: 4,
            time: MediaTime::from_millis(1_000),
            rgb: Some(vec![0u8; 4]),
            basis: RgbBasis::Bt601Matrix,
            evidence_path: None,
        };
        assert!(!declared.has_pixels());
        assert!(matches!(
            FrameDifference::compare(&declared, &declared),
            FrameDifference::NotComparable { .. }
        ));
    }

    #[test]
    fn the_histogram_counts_every_pixel_once() {
        let pixels = rgb(8, 8, |_, _| [10, 20, 30]);
        let histogram = Histogram::from_rgb(&pixels, 8, 8).expect("a real frame");
        assert_eq!(histogram.total, 64);
        assert_eq!(histogram.bins.iter().sum::<u32>() as u64, 64);
        assert_eq!(histogram.bins.len(), 256);
    }

    #[test]
    fn a_black_frame_and_an_empty_frame_are_different_answers() {
        // Otherwise a viewer would report "no frame" as "a black frame".
        let black = vec![0u8; 12];
        let histogram = Histogram::from_rgb(&black, 2, 2).expect("a real frame");
        assert_eq!(histogram.black_fraction(), 1.0);

        assert!(Histogram::from_rgb(&[], 0, 0).is_none());
        assert!(Histogram::from_rgb(&[], 4, 4).is_none(), "short buffer");
    }

    #[test]
    fn a_white_frame_reports_full_scale_pixels() {
        let white = vec![255u8; 12];
        let histogram = Histogram::from_rgb(&white, 2, 2).expect("a real frame");
        assert_eq!(histogram.white_fraction(), 1.0);
        assert_eq!(histogram.max_level, Some(255));
    }

    #[test]
    fn the_histogram_records_its_extremes() {
        let pixels = rgb(
            2,
            2,
            |x, _| if x == 0 { [0, 0, 0] } else { [255, 255, 255] },
        );
        let histogram = Histogram::from_rgb(&pixels, 2, 2).expect("a real frame");
        assert_eq!(histogram.min_level, Some(0));
        assert_eq!(histogram.max_level, Some(255));
        assert_eq!(histogram.peak(), 2);
    }

    // -------------------------------------------------------------- waveform

    #[test]
    fn a_waveform_column_spans_every_channel_of_its_frame() {
        // Otherwise a stereo file's display would describe one arbitrary channel.
        let pcm = [0.5f32, -0.5, 0.25, -0.25];
        let waveform = Waveform::from_pcm(&pcm, 2, 1).expect("a real signal");
        assert_eq!(waveform.columns.len(), 1);
        assert!((waveform.columns[0].max - 0.5).abs() < 1e-6);
        assert!((waveform.columns[0].min + 0.5).abs() < 1e-6);
    }

    #[test]
    fn the_waveform_reports_the_signal_peak_not_an_assumed_full_scale() {
        // Normalising against an assumed 1.0 would render a quiet track as loud,
        // which is the difference between "quiet" and "fine" to anyone QC-ing.
        let quiet = vec![0.01f32; 100];
        let waveform = Waveform::from_pcm(&quiet, 1, 10).expect("a real signal");
        assert!((waveform.peak - 0.01).abs() < 1e-6);
        assert!(!waveform.is_silent());
    }

    #[test]
    fn true_digital_silence_is_reported_as_silent() {
        let waveform = Waveform::from_pcm(&vec![0.0f32; 100], 1, 10).expect("a real signal");
        assert!(waveform.is_silent());
    }

    #[test]
    fn no_audio_is_not_the_same_as_silent_audio() {
        // An empty signal has no waveform; a silent one has a flat line. Drawing
        // both as an empty chart would conflate them.
        assert!(Waveform::from_pcm(&[], 2, 100).is_none());
        assert!(
            Waveform::from_pcm(&[0.0; 10], 0, 100).is_none(),
            "no channels"
        );
        assert!(Waveform::from_pcm(&[0.0; 10], 1, 0).is_none(), "no columns");
        assert!(
            Waveform::from_pcm(&[0.0, 1.0], 8, 10).is_none(),
            "truncated frame"
        );
    }

    #[test]
    fn non_finite_samples_do_not_produce_a_nan_waveform() {
        // A decoder handed hostile bytes can emit NaN; propagating it would make
        // every later column NaN too and blank the whole display.
        let pcm = [f32::NAN, 0.5, f32::INFINITY, -0.25];
        let waveform = Waveform::from_pcm(&pcm, 1, 2).expect("a real signal");
        for column in &waveform.columns {
            assert!(column.min.is_finite() && column.max.is_finite() && column.rms.is_finite());
        }
    }

    #[test]
    fn the_waveform_records_the_resolution_it_downsampled_to() {
        // A reviewer must know the picture is an envelope, not the samples.
        let pcm = vec![0.5f32; 1_000];
        let waveform = Waveform::from_pcm(&pcm, 1, 100).expect("a real signal");
        assert_eq!(waveform.columns.len(), 100);
        assert_eq!(waveform.samples_per_column, 10);
        assert_eq!(waveform.pcm_length, 1_000);
    }

    // ------------------------------------------------------- frame difference

    #[test]
    fn identical_frames_are_reported_as_identical() {
        // A measurement, not an absence of one: zero difference across every
        // channel is exactly what "these are the same picture" means.
        let a = frame(2, 2, vec![100u8; 12]);
        let b = frame(2, 2, vec![100u8; 12]);
        assert_eq!(FrameDifference::compare(&a, &b), FrameDifference::Identical);
    }

    #[test]
    fn frames_of_different_sizes_are_not_comparable_not_different() {
        // Neither "identical" nor "different" would be a measurement.
        let a = frame(2, 2, vec![100u8; 12]);
        let b = frame(4, 4, vec![100u8; 48]);
        let FrameDifference::NotComparable { reason } = FrameDifference::compare(&a, &b) else {
            panic!("frames of different dimensions must not be compared");
        };
        assert!(reason.contains("different dimensions"), "{reason}");
    }

    #[test]
    fn a_frame_with_no_pixels_is_not_comparable() {
        let a = frame(2, 2, vec![100u8; 12]);
        let b = FrameImageView::without_pixels(0, 2, 2, MediaTime::from_millis(1_000));
        assert!(matches!(
            FrameDifference::compare(&a, &b),
            FrameDifference::NotComparable { .. }
        ));
    }

    #[test]
    fn a_short_buffer_is_not_comparable_rather_than_a_wrong_answer() {
        let a = frame(2, 2, vec![100u8; 12]);
        // Declares 2x2 but carries four bytes: a malformed frame (spec §75).
        let b = frame(2, 2, vec![0u8; 4]);
        assert!(matches!(
            FrameDifference::compare(&a, &b),
            FrameDifference::NotComparable { .. }
        ));
    }

    #[test]
    fn a_real_difference_is_measured_not_guessed() {
        let a = frame(2, 2, vec![0u8; 12]);
        let mut b_pixels = vec![0u8; 12];
        b_pixels[0] = 200;
        let b = frame(2, 2, b_pixels);

        let FrameDifference::Different {
            max_delta,
            changed_fraction,
            ..
        } = FrameDifference::compare(&a, &b)
        else {
            panic!("a changed pixel must be reported as a difference");
        };
        assert_eq!(max_delta, 200);
        assert!(changed_fraction > 0.0 && changed_fraction <= 1.0);
    }

    // --------------------------------------------------------------- A/B pane

    fn stamp(index: u64, ms: i64) -> FrameStamp {
        FrameStamp {
            index,
            time: MediaTime::from_millis(ms),
            is_key_frame: index == 0,
        }
    }

    #[test]
    fn the_pts_delta_between_two_panes_is_reported() {
        // Spec §82 shows `PTS 34.733` against `PTS 34.767`: two frames that look
        // identical can carry different times, and that is often the finding.
        let ab = AbComparison::new(
            stamp(1_042, 34_733),
            stamp(1_042, 34_767),
            FrameDifference::Identical,
        );
        assert_eq!(ab.pts_delta(), MediaTime::from_millis(34));
        assert!(ab.is_frame_aligned());
    }

    #[test]
    fn unaligned_panes_say_so() {
        let ab = AbComparison::new(
            stamp(1_042, 0),
            stamp(1_043, 33),
            FrameDifference::Identical,
        );
        assert!(
            !ab.is_frame_aligned(),
            "the UI must not imply the panes are synchronised"
        );
    }

    #[test]
    fn the_blink_control_returns_the_other_side() {
        assert_eq!(AbSide::Original.flip(), AbSide::Comparison);
        assert_eq!(AbSide::Comparison.flip(), AbSide::Original);
        assert_eq!(AbSide::Original.label(), "Original");
        assert_eq!(AbSide::Comparison.label(), "Comparison");
    }

    #[test]
    fn each_side_yields_its_own_stamp() {
        let ab = AbComparison::new(stamp(10, 100), stamp(20, 200), FrameDifference::Identical);
        assert_eq!(ab.stamp_for(AbSide::Original).index, 10);
        assert_eq!(ab.stamp_for(AbSide::Comparison).index, 20);
    }
}
