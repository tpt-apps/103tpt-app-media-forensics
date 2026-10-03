//! Container, stream, and codec description types (spec §12, §13).
//!
//! These are plain descriptions of what a container *claims* about itself.
//! Deliberately separate from measurements: a declared frame rate and a
//! measured frame rate are different facts, and the metadata consistency
//! cross-check (spec §26) depends on keeping them apart.

use serde::{Deserialize, Serialize};

use crate::time::{MediaTime, Rational, Timebase};

/// The kind of elementary stream inside a container.
///
/// Serialises as its lowercase tag (`"video"`) rather than the Rust variant
/// name, because this value appears in the machine-readable CLI output and in
/// report columns, where it is a stable interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamKind {
    /// Video elementary stream.
    #[serde(rename = "video")]
    Video,
    /// Audio elementary stream.
    #[serde(rename = "audio")]
    Audio,
    /// Subtitle or closed-caption stream.
    #[serde(rename = "subtitle")]
    Subtitle,
    /// Opaque data stream (timecode, CEA-708, etc.).
    #[serde(rename = "data")]
    Data,
    /// Embedded file, e.g. a font or a cover image.
    #[serde(rename = "attachment")]
    Attachment,
    /// Stream whose type the container did not declare.
    #[serde(rename = "unknown")]
    Unknown,
}

impl StreamKind {
    /// Returns the stable lowercase tag used in reports and rule IDs.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Subtitle => "subtitle",
            Self::Data => "data",
            Self::Attachment => "attachment",
            Self::Unknown => "unknown",
        }
    }
}

/// Codec identity, with the raw container tag preserved.
///
/// `name` holds exactly what the container declared (e.g. `avc1`, `mp4a`)
/// rather than a normalised name, because the declared tag is itself
/// forensic evidence — a stream tagged `avc1` in a file with no `avcC` box is
/// a meaningful observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodecInfo {
    /// The tag exactly as declared by the container.
    pub name: String,
    /// Long-form codec name when recognised.
    pub long_name: Option<String>,
    /// Codec profile (e.g. H.264 `High`), where the container records one.
    pub profile: Option<String>,
    /// Codec level (e.g. H.264 level 4.1), where recorded.
    pub level: Option<String>,
    /// Codec-specific constraints, e.g. `Constrained Baseline`.
    pub constraints: Option<String>,
}

impl CodecInfo {
    /// Builds a codec description from the container's declared tag.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            long_name: None,
            profile: None,
            level: None,
            constraints: None,
        }
    }

    /// Attaches a recognised long-form codec name.
    ///
    /// The declared `fourcc` is always kept; the long name is additional
    /// context, never a replacement for what the container actually said.
    #[must_use]
    pub fn with_long_name(mut self, long_name: impl Into<String>) -> Self {
        self.long_name = Some(long_name.into());
        self
    }
}

/// A chroma subsampling description.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChromaSubsampling {
    /// Monochrome; no chroma planes.
    Monochrome,
    /// 4:2:0, e.g. HD video.
    Cs420,
    /// 4:2:2, e.g. professional 10-bit video.
    Cs422,
    /// 4:4:4, no chroma subsampling.
    Cs444,
    /// A value the container declared that this build does not model.
    Unknown(String),
}

impl ChromaSubsampling {
    /// Returns the conventional notation, e.g. `"4:2:0"`.
    ///
    /// A report shows `4:2:0`, not `Cs420`: the former is what a colourist and a
    /// specification both use, and an undeclared value is shown as such rather
    /// than silently normalised to something this build happens to model.
    #[must_use]
    pub fn tag(&self) -> String {
        match self {
            Self::Monochrome => "monochrome".to_owned(),
            Self::Cs420 => "4:2:0".to_owned(),
            Self::Cs422 => "4:2:2".to_owned(),
            Self::Cs444 => "4:4:4".to_owned(),
            Self::Unknown(value) => format!("undeclared ({value})"),
        }
    }
}

/// A video pixel format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelFormat {
    /// Raw container tag, e.g. `yuv420p10le`.
    pub name: String,
    /// Chroma subsampling implied by the format.
    pub chroma: ChromaSubsampling,
    /// Significant bits per colour component.
    pub bit_depth: u8,
}

impl PixelFormat {
    /// Builds a pixel format description.
    #[must_use]
    pub fn new(name: impl Into<String>, chroma: ChromaSubsampling, bit_depth: u8) -> Self {
        Self {
            name: name.into(),
            chroma,
            bit_depth,
        }
    }
}

/// Colour primaries, transfer function, matrix, and range (spec §14).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ColourInfo {
    /// Colour primaries, e.g. `BT.709`.
    pub primaries: Option<String>,
    /// Transfer characteristics, e.g. `PQ`, `HLG`, `BT.1886`.
    pub transfer: Option<String>,
    /// Matrix coefficients, e.g. `BT.709`.
    pub matrix: Option<String>,
    /// Full/limited range flag.
    pub full_range: Option<bool>,
    /// HDR static metadata (HDR10, mastering display, CLL) when present.
    pub hdr_metadata: Option<String>,
}

/// Video stream properties as declared by the container (spec §14).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoFormat {
    /// Coded frame width in pixels.
    pub coded_width: u32,
    /// Coded frame height in pixels.
    pub coded_height: u32,
    /// Display width after crop, when a crop box is present.
    pub display_width: Option<u32>,
    /// Display height after crop, when a crop box is present.
    pub display_height: Option<u32>,
    /// Declared frame rate as an exact rational.
    pub frame_rate: Option<Rational>,
    /// Sample aspect ratio, when distinct from 1:1.
    pub sample_aspect_ratio: Option<Rational>,
    /// Display aspect ratio, when declared.
    pub display_aspect_ratio: Option<Rational>,
    /// Rotation in degrees (0, 90, 180, 270) from a transform matrix.
    pub rotation_degrees: Option<u16>,
    /// Pixel and colour description.
    pub pixel_format: PixelFormat,
    /// Colour information.
    pub colour: ColourInfo,
    /// True when the stream carries HDR (BT.2020 or PQ/HLG) signalling.
    pub is_hdr: bool,
}

impl VideoFormat {
    /// Returns the pixel aspect ratio, defaulting to square when undeclared.
    #[must_use]
    pub fn pixel_aspect_ratio(&self) -> Rational {
        self.sample_aspect_ratio
            .unwrap_or_else(|| Rational::from_integer(1))
    }
}

/// An audio channel layout description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelLayout {
    /// Layout name as declared, e.g. `5.1`.
    pub name: String,
    /// Number of discrete channels.
    pub channel_count: u16,
    /// Per-channel channel mask, where the container supplies one.
    pub channel_mask: Option<u32>,
}

/// Audio stream properties as declared by the container (spec §19).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioFormat {
    /// Declared sample rate in Hz.
    pub sample_rate: u32,
    /// Significant bits per sample.
    pub bit_depth: u16,
    /// Channel layout, when the container describes one.
    pub channel_layout: Option<ChannelLayout>,
}

impl AudioFormat {
    /// Returns the channel count, or 0 when no layout is declared.
    ///
    /// Returns 0 rather than guessing a channel count from a sample rate:
    /// an absent layout is an observation the report should surface, not
    /// something to paper over with an assumption.
    #[must_use]
    pub fn channel_count(&self) -> u16 {
        self.channel_layout
            .as_ref()
            .map_or(0, |layout| layout.channel_count)
    }
}

/// Per-stream timing and identity as declared by the container (spec §13, §24).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamTiming {
    /// Timebase used by this stream's timestamps.
    pub timebase: Timebase,
    /// Declared stream start time.
    pub start_time: MediaTime,
    /// Duration the container **declares** for this stream, from its `mdhd`.
    pub duration: Option<MediaTime>,
    /// Duration the container's **sample tables** actually add up to.
    ///
    /// Kept separate from `duration` because they are different claims and
    /// disagreeing is the observation (spec §26). A container whose header says
    /// one length and whose sample table sums to another has had its headers
    /// rewritten without its media data being rewritten to match — which is
    /// what a partial re-mux, a splice, or a header-only edit leaves behind.
    ///
    /// `None` when the sample table yields no usable total, which is "could not
    /// be measured" rather than "agrees with the declaration".
    pub measured_duration: Option<MediaTime>,
    /// Container edit-list offset applied to this stream, when present.
    pub edit_list_offset: Option<MediaTime>,
}

/// Everything the container declares about one stream (spec §13).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamAnalysis {
    /// Index of this stream within the container, in container order.
    pub index: u32,
    /// The stream's kind.
    pub kind: StreamKind,
    /// The language tag, where declared.
    pub language: Option<String>,
    /// Codec description.
    pub codec: CodecInfo,
    /// Declared timing.
    pub timing: StreamTiming,
    /// Video properties, for video streams.
    pub video: Option<VideoFormat>,
    /// Audio properties, for audio streams.
    pub audio: Option<AudioFormat>,
    /// Number of packets the container index reports for this stream.
    pub packet_count: Option<u64>,
}

impl StreamAnalysis {
    /// Returns the video format, if this is a video stream.
    #[must_use]
    pub fn video_format(&self) -> Option<&VideoFormat> {
        self.video.as_ref()
    }

    /// Returns the audio format, if this is an audio stream.
    #[must_use]
    pub fn audio_format(&self) -> Option<&AudioFormat> {
        self.audio.as_ref()
    }

    /// Returns the video pixel dimensions as `(width, height)`.
    ///
    /// Prefers the cropped display size, falling back to the coded size,
    /// because a 1920x1088 coded frame with a 1080 crop is a 1080p signal and
    /// should be reported as such.
    #[must_use]
    pub fn dimensions(&self) -> Option<(u32, u32)> {
        let video = self.video.as_ref()?;
        Some((
            video.display_width.unwrap_or(video.coded_width),
            video.display_height.unwrap_or(video.coded_height),
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn sample_video_stream() -> StreamAnalysis {
        StreamAnalysis {
            index: 0,
            kind: StreamKind::Video,
            language: Some("und".to_owned()),
            codec: CodecInfo::new("avc1"),
            timing: StreamTiming {
                timebase: Timebase::from_ticks_per_second(30_000),
                start_time: MediaTime::ZERO,
                duration: None,
                measured_duration: None,
                edit_list_offset: None,
            },
            video: Some(VideoFormat {
                coded_width: 1920,
                coded_height: 1088,
                display_width: Some(1920),
                display_height: Some(1080),
                frame_rate: Some(Rational::new(30_000, 1_001).unwrap()),
                sample_aspect_ratio: None,
                display_aspect_ratio: None,
                rotation_degrees: Some(0),
                pixel_format: PixelFormat::new("yuv420p", ChromaSubsampling::Cs420, 8),
                colour: ColourInfo {
                    primaries: Some("BT.709".to_owned()),
                    ..ColourInfo::default()
                },
                is_hdr: false,
            }),
            audio: None,
            packet_count: Some(89_571),
        }
    }

    #[test]
    fn dimensions_prefer_crop_over_coded_size() {
        assert_eq!(sample_video_stream().dimensions(), Some((1920, 1080)));
    }

    #[test]
    fn dimensions_fall_back_to_coded_size() {
        let mut stream = sample_video_stream();
        let video = stream.video.as_mut().expect("video format present");
        video.display_width = None;
        video.display_height = None;
        assert_eq!(stream.dimensions(), Some((1920, 1088)));
    }

    #[test]
    fn non_video_stream_has_no_dimensions() {
        let mut stream = sample_video_stream();
        stream.kind = StreamKind::Audio;
        stream.video = None;
        assert_eq!(stream.dimensions(), None);
    }

    #[test]
    fn pixel_aspect_ratio_defaults_to_square() {
        let stream = sample_video_stream();
        let sar = stream
            .video_format()
            .expect("video format")
            .pixel_aspect_ratio();
        assert_eq!(sar.to_f64(), 1.0);
    }

    #[test]
    fn absent_channel_layout_reports_zero_not_a_guess() {
        let audio = AudioFormat {
            sample_rate: 48_000,
            bit_depth: 24,
            channel_layout: None,
        };
        assert_eq!(audio.channel_count(), 0);
    }

    #[test]
    fn declared_channel_count_is_read_from_layout() {
        let audio = AudioFormat {
            sample_rate: 48_000,
            bit_depth: 24,
            channel_layout: Some(ChannelLayout {
                name: "5.1".to_owned(),
                channel_count: 6,
                channel_mask: Some(0x3F),
            }),
        };
        assert_eq!(audio.channel_count(), 6);
    }

    #[test]
    fn codec_preserves_declared_tag_verbatim() {
        // The raw tag is forensic evidence; it must not be normalised away.
        let codec = CodecInfo::new("mp4a");
        assert_eq!(codec.name, "mp4a");
        assert!(codec.long_name.is_none());
    }

    #[test]
    fn stream_kind_tags_are_stable() {
        // These strings appear in rule IDs and report columns.
        for kind in [
            StreamKind::Video,
            StreamKind::Audio,
            StreamKind::Subtitle,
            StreamKind::Data,
            StreamKind::Attachment,
            StreamKind::Unknown,
        ] {
            assert!(!kind.tag().is_empty());
        }
        assert_eq!(StreamKind::Video.tag(), "video");
        assert_eq!(StreamKind::Attachment.tag(), "attachment");
    }
}
