//! File-to-file comparison (spec §38–40).
//!
//! # Comparing two assets is not the same as comparing two numbers
//!
//! The question this module answers is not "are these files different" but
//! "*how*, and along which axes". A transcode to a lower bitrate and a re-mux
//! with a different atom order produce byte-different files; only one of them
//! changed anything a reviewer would care about. Flattening that into a single
//! similarity score would discard exactly the information the tool exists to
//! surface, so there is no score here.
//!
//! # Unmeasurable is not equal
//!
//! A property that could not be read on one side is
//! [`Difference::NotComparable`], never [`Difference::Equal`]. Reporting
//! "identical" for something never measured would be a claim the evidence does
//! not support, and in a forensic report that is the difference between a true
//! statement and a fabricated one.
//!
//! # Streams are paired, not zipped
//!
//! Stream comparison pairs streams by kind and position within that kind,
//! because index `0` in one file is not necessarily index `0` in the other. A
//! file that dropped its first audio track would otherwise report every
//! subsequent track as changed.

use serde::{Deserialize, Serialize};

use crate::media::{
    AudioFormat, CodecInfo, ColourInfo, PixelFormat, StreamAnalysis, StreamKind, StreamTiming,
    VideoFormat,
};

/// How one property compares between two assets.
///
/// `Left` and `Right` follow *argument position*: a caller passing the left
/// asset's value first gets [`Difference::OnlyLeft`] for a property only that
/// asset declared. The names track argument order rather than asset identity so
/// that a caller which reorders its arguments produces correspondingly renamed
/// variants in the report — visibly — rather than silently reporting the wrong
/// side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Difference {
    /// Both sides hold the same value, and both were actually measured.
    Equal,
    /// Both sides were measured and disagree.
    Different {
        /// The left-hand value, rendered.
        left: String,
        /// The right-hand value, rendered.
        right: String,
    },
    /// Only the left asset has the property at all.
    OnlyLeft {
        /// The value the left asset holds.
        value: String,
    },
    /// Only the right asset has the property at all.
    OnlyRight {
        /// The value the right asset holds.
        value: String,
    },
    /// The property could not be measured on at least one side.
    ///
    /// Never collapsed into [`Difference::Equal`]: see the module docs.
    ///
    /// The reason is a `String` rather than `&'static str` so that the type
    /// round-trips through serde. A comparison is persisted with a case and read
    /// back by a later run, and a field that cannot be written is a field that
    /// silently cannot be reported on.
    NotComparable {
        /// Why the comparison could not be made.
        reason: String,
    },
}

impl Difference {
    /// Builds a comparison from two optional measurements.
    ///
    /// Total so callers can compare `Option<T>` fields directly without each
    /// hand-rolling the same four-way match.
    fn from_options<T, F>(left: &Option<T>, right: &Option<T>, render: F) -> Self
    where
        T: PartialEq,
        F: Fn(&T) -> String,
    {
        match (left, right) {
            (Some(l), Some(r)) => {
                if l == r {
                    Self::Equal
                } else {
                    Self::Different {
                        left: render(l),
                        right: render(r),
                    }
                }
            }
            (Some(l), None) => Self::OnlyLeft { value: render(l) },
            (None, Some(r)) => Self::OnlyRight { value: render(r) },
            (None, None) => Self::NotComparable {
                reason: "neither asset reported this property".to_owned(),
            },
        }
    }

    /// Whether the two sides were measured and agreed.
    #[must_use]
    pub fn is_equal(&self) -> bool {
        matches!(self, Self::Equal)
    }

    /// Whether the two sides were measured and disagree.
    #[must_use]
    pub fn is_different(&self) -> bool {
        matches!(self, Self::Different { .. })
    }

    /// Whether the comparison could not be made at all.
    #[must_use]
    pub fn is_not_comparable(&self) -> bool {
        matches!(self, Self::NotComparable { .. })
    }

    /// Whether anything about this property is worth reporting.
    ///
    /// A comparison where every axis is equal is still meaningful — it is the
    /// evidence that two files are interchangeable — so this is not the same
    /// as "is there a difference".
    #[must_use]
    pub fn is_interesting(&self) -> bool {
        !matches!(self, Self::Equal)
    }
}

/// The axes along which two assets are compared (spec §38–40).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonAxis {
    /// Container format.
    Container,
    /// Number and kinds of streams.
    StreamLayout,
    /// Per-stream codec identity.
    Codec,
    /// Video frame size, frame rate, and pixel format.
    VideoFormat,
    /// Primaries, transfer, matrix, and full-range flags.
    Colour,
    /// Sample rate, bit depth, and channel layout.
    AudioFormat,
    /// Stream start times and durations.
    Timing,
    /// Metadata entries.
    Metadata,
    /// Scene-change structure.
    SceneStructure,
    /// Detected silence regions.
    Silence,
    /// Integrated loudness.
    Loudness,
}

impl ComparisonAxis {
    /// Every axis, in report order.
    ///
    /// One ordered list, so two callers cannot report a different subset or a
    /// different order for the same comparison.
    pub const ALL: &'static [Self] = &[
        Self::Container,
        Self::StreamLayout,
        Self::Codec,
        Self::VideoFormat,
        Self::Colour,
        Self::AudioFormat,
        Self::Timing,
        Self::Metadata,
        Self::SceneStructure,
        Self::Silence,
        Self::Loudness,
    ];

    /// Returns a stable lowercase identifier for reports and CLI output.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::Container => "container",
            Self::StreamLayout => "stream_layout",
            Self::Codec => "codec",
            Self::VideoFormat => "video_format",
            Self::Colour => "colour",
            Self::AudioFormat => "audio_format",
            Self::Timing => "timing",
            Self::Metadata => "metadata",
            Self::SceneStructure => "scene_structure",
            Self::Silence => "silence",
            Self::Loudness => "loudness",
        }
    }
}

/// Which side of a comparison an observation came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonSide {
    /// The first asset.
    Left,
    /// The second asset.
    Right,
}

/// One property compared between two assets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldComparison {
    /// The axis this property belongs to.
    pub axis: ComparisonAxis,
    /// What was compared, e.g. `"video codec"`.
    pub field: String,
    /// How it compares.
    pub difference: Difference,
}

impl FieldComparison {
    /// Builds a field comparison.
    #[must_use]
    pub fn new(axis: ComparisonAxis, field: impl Into<String>, difference: Difference) -> Self {
        Self {
            axis,
            field: field.into(),
            difference,
        }
    }
}

/// A stream present in one asset with no counterpart in the other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnmatchedStream {
    /// The asset the stream was found in.
    pub side: ComparisonSide,
    /// Its container index.
    pub index: usize,
    /// Its kind.
    pub kind: StreamKind,
    /// Its codec name.
    pub codec: String,
}

/// The outcome of comparing one pair of streams.
///
/// Streams are paired by kind and position rather than by index, because index
/// `0` in one file is not necessarily index `0` in the other. A file that lost
/// its first audio track would otherwise report every later track as changed,
/// which buries the one real difference in a wall of false ones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamComparison {
    /// The left stream's container index.
    pub left_index: usize,
    /// The right stream's container index.
    pub right_index: usize,
    /// The stream kind both sides agreed on.
    ///
    /// Streams are only paired within a kind, so this is a real correspondence
    /// rather than a guess.
    pub kind: StreamKind,
    /// Per-property results for this pair.
    pub fields: Vec<FieldComparison>,
}

impl StreamComparison {
    /// Properties that were measured and disagree.
    pub fn differences(&self) -> impl Iterator<Item = &FieldComparison> {
        self.fields.iter().filter(|f| f.difference.is_different())
    }

    /// Properties that could not be compared.
    pub fn uncomparable(&self) -> impl Iterator<Item = &FieldComparison> {
        self.fields
            .iter()
            .filter(|f| f.difference.is_not_comparable())
    }
}

/// The stream-level half of a comparison.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamComparisonResult {
    /// Pairs that exist on both sides, in [`ComparisonAxis`] report order.
    pub streams: Vec<StreamComparison>,
    /// Streams with no counterpart, in either direction.
    pub unmatched: Vec<UnmatchedStream>,
    /// Whether the stream layout as a whole matches.
    pub layout: Difference,
}

/// Pairs two assets' streams by kind and position within that kind.
///
/// Pairs the *n*th video stream of each side with the *n*th video stream of the
/// other, and likewise per kind. A file that dropped its first audio track
/// therefore reports one unmatched stream, not three changed ones.
///
/// [`Difference::NotComparable`] is returned when one side had no container at
/// all, which keeps "unreadable" distinct from "different" — the distinction
/// the rest of the engine is careful about (spec §77).
#[must_use]
pub fn compare_streams(
    left: Option<&[StreamAnalysis]>,
    right: Option<&[StreamAnalysis]>,
) -> StreamComparisonResult {
    let (Some(left), Some(right)) = (left, right) else {
        return StreamComparisonResult {
            streams: Vec::new(),
            unmatched: Vec::new(),
            layout: Difference::NotComparable {
                reason: "one asset's container could not be read".to_owned(),
            },
        };
    };

    let mut streams = Vec::new();
    let mut unmatched = Vec::new();

    for kind in [
        StreamKind::Video,
        StreamKind::Audio,
        StreamKind::Data,
        StreamKind::Subtitle,
        StreamKind::Attachment,
        StreamKind::Unknown,
    ] {
        let left_of_kind: Vec<(usize, &StreamAnalysis)> = left
            .iter()
            .enumerate()
            .filter(|(_, s)| s.kind == kind)
            .collect();
        let right_of_kind: Vec<(usize, &StreamAnalysis)> = right
            .iter()
            .enumerate()
            .filter(|(_, s)| s.kind == kind)
            .collect();

        let pairs = left_of_kind.len().min(right_of_kind.len());
        for i in 0..pairs {
            let (left_index, left_stream) = left_of_kind[i];
            let (right_index, right_stream) = right_of_kind[i];
            streams.push(StreamComparison {
                left_index,
                right_index,
                kind,
                fields: compare_pair(left_stream, right_stream),
            });
        }
        for (index, stream) in left_of_kind.iter().skip(pairs) {
            unmatched.push(unmatched_stream(ComparisonSide::Left, *index, stream));
        }
        for (index, stream) in right_of_kind.iter().skip(pairs) {
            unmatched.push(unmatched_stream(ComparisonSide::Right, *index, stream));
        }
    }

    let layout = if left.len() == right.len()
        && left.iter().map(|s| s.kind).eq(right.iter().map(|s| s.kind))
    {
        Difference::Equal
    } else {
        Difference::Different {
            left: render_layout(left),
            right: render_layout(right),
        }
    };

    StreamComparisonResult {
        streams,
        unmatched,
        layout,
    }
}

fn render_layout(streams: &[StreamAnalysis]) -> String {
    let kinds: Vec<&str> = streams.iter().map(|s| s.kind.tag()).collect();
    format!("{} stream(s): {}", streams.len(), kinds.join(", "))
}

fn unmatched_stream(
    side: ComparisonSide,
    index: usize,
    stream: &StreamAnalysis,
) -> UnmatchedStream {
    UnmatchedStream {
        side,
        index,
        kind: stream.kind,
        codec: stream.codec.name.clone(),
    }
}

/// Compares one stream pair's properties.
///
/// Public so a caller that paired streams itself — after its own ordering
/// decision, which it must then record — can still use the same property set
/// rather than reimplementing it slightly differently.
#[must_use]
pub fn compare_pair(left: &StreamAnalysis, right: &StreamAnalysis) -> Vec<FieldComparison> {
    let mut fields = vec![
        FieldComparison::new(
            ComparisonAxis::Codec,
            "codec",
            Difference::from_options(&Some(&left.codec), &Some(&right.codec), |c| render_codec(c)),
        ),
        FieldComparison::new(
            ComparisonAxis::Codec,
            "codec_long_name",
            Difference::from_options(&left.codec.long_name, &right.codec.long_name, |s| s.clone()),
        ),
        FieldComparison::new(
            ComparisonAxis::Timing,
            "language",
            Difference::from_options(&left.language, &right.language, |s| s.clone()),
        ),
    ];
    fields.extend(compare_timing(&left.timing, &right.timing));

    // Optional per-kind blocks. A video stream and an audio stream are not
    // expected to carry the same properties, so comparing a video field against
    // an audio field would compare noise and report a difference where there is
    // none.
    match (&left.video, &right.video) {
        (Some(l), Some(r)) => {
            fields.push(FieldComparison::new(
                ComparisonAxis::VideoFormat,
                "coded_dimensions",
                Difference::from_options(
                    &Some((l.coded_width, l.coded_height)),
                    &Some((r.coded_width, r.coded_height)),
                    |(w, h)| format!("{w}x{h}"),
                ),
            ));
            fields.push(FieldComparison::new(
                ComparisonAxis::VideoFormat,
                "frame_rate",
                Difference::from_options(&l.frame_rate, &r.frame_rate, |r| {
                    format!("{:.3}fps", r.to_f64())
                }),
            ));
            fields.push(FieldComparison::new(
                ComparisonAxis::VideoFormat,
                "pixel_format",
                Difference::from_options(&Some(&l.pixel_format), &Some(&r.pixel_format), |p| {
                    render_pixel_format(p)
                }),
            ));
            fields.push(FieldComparison::new(
                ComparisonAxis::VideoFormat,
                "rotation",
                Difference::from_options(&l.rotation_degrees, &r.rotation_degrees, |r| {
                    format!("{r} deg")
                }),
            ));
            fields.push(FieldComparison::new(
                ComparisonAxis::Colour,
                "colour",
                Difference::from_options(&Some(&l.colour), &Some(&r.colour), |c| render_colour(c)),
            ));
            fields.push(FieldComparison::new(
                ComparisonAxis::Colour,
                "is_hdr",
                Difference::from_options(&Some(l.is_hdr), &Some(r.is_hdr), |v| v.to_string()),
            ));
        }
        (None, None) => {}
        (Some(_), None) | (None, Some(_)) => fields.push(FieldComparison::new(
            ComparisonAxis::VideoFormat,
            "video_format",
            Difference::NotComparable {
                reason: "only one of the two streams carried video properties".to_owned(),
            },
        )),
    }

    match (&left.audio, &right.audio) {
        (Some(l), Some(r)) => {
            fields.push(FieldComparison::new(
                ComparisonAxis::AudioFormat,
                "sample_rate",
                Difference::from_options(&Some(l.sample_rate), &Some(r.sample_rate), |r| {
                    format!("{r} Hz")
                }),
            ));
            fields.push(FieldComparison::new(
                ComparisonAxis::AudioFormat,
                "bit_depth",
                Difference::from_options(&Some(l.bit_depth), &Some(r.bit_depth), |d| {
                    format!("{d}-bit")
                }),
            ));
            fields.push(FieldComparison::new(
                ComparisonAxis::AudioFormat,
                "channel_count",
                Difference::from_options(
                    &l.channel_layout.as_ref().map(|c| c.channel_count),
                    &r.channel_layout.as_ref().map(|c| c.channel_count),
                    |c| c.to_string(),
                ),
            ));
        }
        (None, None) => {}
        (Some(_), None) | (None, Some(_)) => fields.push(FieldComparison::new(
            ComparisonAxis::AudioFormat,
            "audio_format",
            Difference::NotComparable {
                reason: "only one of the two streams carried audio properties".to_owned(),
            },
        )),
    }

    fields
}

/// Compares two streams' timing.
///
/// Exposed so a caller holding timings from a source other than a
/// [`StreamAnalysis`] — a track header read directly, say — gets the same
/// semantics rather than a subtly different copy.
#[must_use]
pub fn compare_timing(left: &StreamTiming, right: &StreamTiming) -> Vec<FieldComparison> {
    vec![
        FieldComparison::new(
            ComparisonAxis::Timing,
            "timebase",
            Difference::from_options(&Some(left.timebase), &Some(right.timebase), |t| {
                format!("1/{} tick/s", t.ticks_per_second())
            }),
        ),
        FieldComparison::new(
            ComparisonAxis::Timing,
            "start_time",
            Difference::from_options(&Some(left.start_time), &Some(right.start_time), |t| {
                render_time(t)
            }),
        ),
        FieldComparison::new(
            ComparisonAxis::Timing,
            "duration",
            Difference::from_options(&left.duration, &right.duration, render_time),
        ),
    ]
}

/// Renders a codec for a report.
///
/// The short name is the identity; the profile distinguishes two encodings of
/// the same codec, so both are shown.
#[must_use]
pub fn render_codec(codec: &CodecInfo) -> String {
    match &codec.profile {
        Some(profile) => format!("{} ({profile})", codec.name),
        None => codec.name.clone(),
    }
}

/// Renders a pixel format for a report.
#[must_use]
pub fn render_pixel_format(pixel: &PixelFormat) -> String {
    format!("{}-bit {}", pixel.bit_depth, pixel.chroma.tag())
}

/// Renders an audio format for a report.
#[must_use]
pub fn render_audio(audio: &AudioFormat) -> String {
    format!(
        "{} Hz, {}-bit, {}",
        audio.sample_rate,
        audio.bit_depth,
        audio
            .channel_layout
            .as_ref()
            .map_or_else(|| "unknown layout".to_owned(), |l| l.name.clone())
    )
}

/// Renders a media time for a report.
///
/// Uses the display impl, which already carries sub-second precision in a form
/// a reviewer can check by hand. Written in terms of that rather than a private
/// conversion so a change to [`MediaTime`]'s formatting propagates here instead
/// of silently producing a second, disagreeing rendering.
fn render_time(time: &crate::time::MediaTime) -> String {
    time.to_string()
}

/// Renders a video format for a report.
#[must_use]
pub fn render_video(video: &VideoFormat) -> String {
    format!(
        "{}x{} @ {}",
        video.coded_width,
        video.coded_height,
        video.frame_rate.map_or_else(
            || "unknown rate".to_owned(),
            |r| format!("{:.3}fps", r.to_f64())
        )
    )
}

/// Renders the colour properties that are actually present.
///
/// Absent colour tags are listed as `undeclared` rather than omitted, so a
/// report can distinguish "the file declares no colour information" from "the
/// comparison did not look at colour".
#[must_use]
pub fn render_colour(colour: &ColourInfo) -> String {
    let part = |value: &Option<String>| value.clone().unwrap_or_else(|| "undeclared".to_owned());
    format!(
        "primaries={} transfer={} matrix={} range={}",
        part(&colour.primaries),
        part(&colour.transfer),
        part(&colour.matrix),
        colour
            .full_range
            .map_or_else(|| "undeclared".to_owned(), |v| v.to_string())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{
        AudioFormat, ChannelLayout, ChromaSubsampling, CodecInfo, ColourInfo, PixelFormat,
        StreamKind,
    };
    use crate::time::{MediaTime, Rational, Timebase};

    fn video_stream(codec: &str, width: u32, height: u32) -> StreamAnalysis {
        StreamAnalysis {
            index: 0,
            kind: StreamKind::Video,
            language: None,
            codec: CodecInfo {
                name: codec.to_owned(),
                long_name: None,
                profile: None,
                level: None,
                constraints: None,
            },
            timing: StreamTiming {
                timebase: Timebase::from_ticks_per_second(30_000),
                start_time: MediaTime::from_millis(0),
                duration: Some(MediaTime::from_millis(1_000)),
                measured_duration: None,
                edit_list_offset: None,
            },
            video: Some(VideoFormat {
                coded_width: width,
                coded_height: height,
                display_width: None,
                display_height: None,
                frame_rate: Some(Rational::new(25, 1).expect("valid rational")),
                sample_aspect_ratio: None,
                display_aspect_ratio: None,
                rotation_degrees: None,
                pixel_format: PixelFormat {
                    name: "yuv420p".to_owned(),
                    chroma: ChromaSubsampling::Cs420,
                    bit_depth: 8,
                },
                colour: ColourInfo {
                    primaries: None,
                    transfer: None,
                    matrix: None,
                    full_range: None,
                    hdr_metadata: None,
                },
                is_hdr: false,
            }),
            audio: None,
            packet_count: None,
        }
    }

    fn audio_stream(codec: &str, sample_rate: u32) -> StreamAnalysis {
        StreamAnalysis {
            index: 1,
            kind: StreamKind::Audio,
            language: None,
            codec: CodecInfo {
                name: codec.to_owned(),
                long_name: None,
                profile: None,
                level: None,
                constraints: None,
            },
            timing: StreamTiming {
                timebase: Timebase::from_ticks_per_second(48_000),
                start_time: MediaTime::from_millis(0),
                duration: Some(MediaTime::from_millis(1_000)),
                measured_duration: None,
                edit_list_offset: None,
            },
            video: None,
            audio: Some(AudioFormat {
                sample_rate,
                bit_depth: 16,
                channel_layout: Some(ChannelLayout {
                    name: "stereo".to_owned(),
                    channel_count: 2,
                    channel_mask: None,
                }),
            }),
            packet_count: None,
        }
    }

    #[test]
    fn identical_streams_compare_equal() {
        let a = video_stream("h264", 320, 240);
        let b = video_stream("h264", 320, 240);

        let result = compare_streams(Some(&[a]), Some(&[b]));
        assert_eq!(result.streams.len(), 1);
        assert_eq!(result.streams[0].kind, StreamKind::Video);
        assert!(
            result.streams[0].differences().next().is_none(),
            "identical streams must report no differences"
        );
        assert!(result.unmatched.is_empty(), "nothing is unmatched");
    }

    #[test]
    fn a_resolution_change_is_reported_as_one_difference() {
        let result = compare_streams(
            Some(&[video_stream("h264", 320, 240)]),
            Some(&[video_stream("h264", 1920, 1080)]),
        );

        let fields: Vec<&str> = result.streams[0]
            .differences()
            .map(|f| f.field.as_str())
            .collect();
        assert_eq!(
            fields,
            vec!["coded_dimensions"],
            "only the resolution changed: {fields:?}"
        );
    }

    #[test]
    fn a_dropped_audio_track_reports_one_unmatched_stream_not_a_shifted_comparison() {
        // The pairing bug this guards: indexing both sides by position would
        // pair the left's *second* audio track with the right's *first*, and
        // report every one of them as changed.
        let left = vec![
            video_stream("h264", 320, 240),
            audio_stream("aac", 44_100),
            audio_stream("aac", 48_000),
        ];
        let right = vec![video_stream("h264", 320, 240), audio_stream("aac", 44_100)];

        let result = compare_streams(Some(&left), Some(&right));

        assert_eq!(result.streams.len(), 2, "video and first audio pair up");
        assert!(
            result.streams[0].differences().next().is_none(),
            "the surviving streams are unchanged"
        );
        assert_eq!(result.unmatched.len(), 1, "exactly one stream is orphaned");
    }

    #[test]
    fn a_stream_missing_its_video_block_is_not_comparable() {
        let mut stripped = video_stream("h264", 320, 240);
        stripped.video = None;

        let result = compare_streams(Some(&[video_stream("h264", 320, 240)]), Some(&[stripped]));
        let fields: Vec<&str> = result.streams[0]
            .uncomparable()
            .map(|f| f.field.as_str())
            .collect();
        assert!(
            fields.contains(&"video_format"),
            "a one-sided video block must be uncomparable, not different: {fields:?}"
        );
    }

    #[test]
    fn an_absent_duration_is_not_reported_as_zero() {
        // "Not declared" and "zero seconds long" are different facts about a file.
        let mut left = video_stream("h264", 320, 240);
        left.timing.duration = None;

        let result = compare_streams(Some(&[left]), Some(&[video_stream("h264", 320, 240)]));
        let duration = result.streams[0]
            .fields
            .iter()
            .find(|f| f.field == "duration")
            .expect("duration is compared");

        assert!(
            matches!(duration.difference, Difference::OnlyRight { .. }),
            "an absent duration must be one-sided, got {:?}",
            duration.difference
        );
    }

    #[test]
    fn comparison_is_deterministic_across_runs() {
        // Reproducibility (spec §77): the same inputs must give the same output,
        // or two runs of the same comparison disagree.
        let left = vec![video_stream("h264", 320, 240), audio_stream("aac", 44_100)];
        let right = vec![video_stream("h264", 640, 360), audio_stream("aac", 48_000)];

        let first = compare_streams(Some(&left), Some(&right));
        let second = compare_streams(Some(&left), Some(&right));
        assert_eq!(first, second, "comparison must be reproducible");
    }

    #[test]
    fn every_axis_has_a_distinct_tag() {
        let mut tags: Vec<&str> = ComparisonAxis::ALL.iter().map(|a| a.tag()).collect();
        tags.sort_unstable();
        let before = tags.len();
        tags.dedup();
        assert_eq!(tags.len(), before, "axis tags must be unique");
    }

    #[test]
    fn a_comparison_round_trips_through_serde() {
        // A comparison is stored with the case and read back by a later run; a
        // field that cannot be serialised is a field that cannot be reported on.
        let result = compare_streams(
            Some(&[video_stream("h264", 320, 240)]),
            Some(&[video_stream("h264", 1920, 1080)]),
        );
        let json = serde_json::to_string(&result).expect("serialises");
        let back: StreamComparisonResult = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(result, back);
    }

    #[test]
    fn a_not_comparable_result_survives_serialisation() {
        let result = compare_streams(None, None);
        let json = serde_json::to_string(&result).expect("serialises");
        assert!(
            json.contains("NotComparable"),
            "the uncomparable state must survive the round trip: {json}"
        );
    }

    #[test]
    fn colour_reports_undeclared_tags_rather_than_omitting_them() {
        // A reviewer must be able to tell "the file declares no primaries" from
        // "the comparison did not look at primaries".
        let colour = ColourInfo {
            primaries: None,
            transfer: None,
            matrix: None,
            full_range: None,
            hdr_metadata: None,
        };
        let rendered = render_colour(&colour);
        assert!(rendered.contains("primaries=undeclared"), "{rendered}");
        assert!(rendered.contains("range=undeclared"), "{rendered}");
    }

    #[test]
    fn a_colour_difference_is_reported_on_the_colour_axis() {
        let mut left = video_stream("h264", 320, 240);
        if let Some(video) = left.video.as_mut() {
            video.colour.primaries = Some("bt709".to_owned());
        }

        let result = compare_streams(Some(&[left]), Some(&[video_stream("h264", 320, 240)]));

        let colour: Vec<&FieldComparison> = result.streams[0]
            .differences()
            .filter(|f| f.axis == ComparisonAxis::Colour)
            .collect();
        assert_eq!(colour.len(), 1, "only the primaries differ");
        assert_eq!(colour[0].field, "colour");
    }

    #[test]
    fn an_audio_sample_rate_change_lands_on_the_audio_axis() {
        let result = compare_streams(
            Some(&[audio_stream("aac", 44_100)]),
            Some(&[audio_stream("aac", 48_000)]),
        );
        let fields: Vec<&str> = result.streams[0]
            .differences()
            .map(|f| f.field.as_str())
            .collect();
        assert_eq!(fields, vec!["sample_rate"], "{fields:?}");
        assert_eq!(
            result.streams[0].differences().next().map(|f| f.axis),
            Some(ComparisonAxis::AudioFormat)
        );
    }

    #[test]
    fn a_codec_change_is_reported_with_both_rendered_names() {
        let result = compare_streams(
            Some(&[video_stream("h264", 320, 240)]),
            Some(&[video_stream("hevc", 320, 240)]),
        );
        let codec = result.streams[0]
            .differences()
            .next()
            .expect("the codec changed");
        assert_eq!(codec.field, "codec");
        match &codec.difference {
            Difference::Different { left, right } => {
                assert_eq!(left, "h264");
                assert_eq!(right, "hevc");
            }
            other => panic!("expected a rendered difference, got {other:?}"),
        }
    }
}
