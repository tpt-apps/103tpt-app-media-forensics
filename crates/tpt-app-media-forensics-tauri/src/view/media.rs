//! The Video and Audio screens (spec \u00a743, \u00a744).
//!
//! # What is measured and what is shown
//!
//! Both screens display values the engine already measured. The Video screen
//! shows the container's own frame timing and, where a codec is decodable here,
//! real decoded pixels. The Audio screen shows levels, silence and loudness
//! measured by `-audio`, plus a peak envelope built from the decoded PCM.
//!
//! Where the engine cannot measure something the screen says so. That is not a
//! display gap: spec \u00a721 forbids reporting a measurement that could not be taken
//! correctly, and this engine never decodes patent-encumbered codecs at all
//! (H.264, HEVC, AAC) - a deliberate policy, not a missing feature, so it is
//! worded as a policy rather than as a failure.
//!
//! # Frames are decoded on demand, never cached across the session
//!
//! A 4K master has tens of thousands of frames. Retaining decoded pixels for
//! all of them would exhaust memory long before the analyst reached the end, and
//! a viewer that has to load everything before showing frame one is a viewer
//! that appears to hang. The engine already bounds its Tier-2 window, and this
//! screen reuses that bound rather than inventing a larger one.

use serde::{Deserialize, Serialize};

use tpt_app_media_forensics_model::MediaTime;

use super::viewer::{FrameImageView, FrameStamp, FrameTimestamps, RgbBasis, ZoomLevel};

/// The Video screen for one asset (spec \u00a743, \u00a744).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoView {
    /// The asset's display name.
    pub asset_name: String,
    /// Display dimensions of the first video stream, when declared.
    pub dimensions: Option<String>,
    /// Declared frame rate, as the rational it is.
    pub frame_rate: Option<String>,
    /// Number of frames the container's tables describe.
    pub frame_count: u64,
    /// Whether the container declared every frame a sync sample.
    pub all_frames_are_keyframes: bool,
    /// Per-frame timing, for the viewer to step through.
    ///
    /// Both presentation and decode times, because their disagreement is the
    /// observation worth making and showing only one hides it.
    pub frames: Vec<FrameStamp>,
    /// Presentation timestamps, paired with their decode times.
    pub timestamps: Vec<FrameTimestamps>,
    /// Why no frame could be shown, when none can.
    ///
    /// Not an error: a file whose codec this build deliberately does not decode
    /// has a perfectly readable frame *table* and no pixels, and the screen must
    /// say which of those two situations it is in.
    pub pixels_unavailable: Option<String>,
    /// The decoder's notes, when it declined to run.
    pub decoder_notes: Vec<String>,
}

impl VideoView {
    /// A screen for a stream with frame timing but no decodable pixels.
    ///
    /// The frame *table* is still worth showing in full - it is what a reviewer
    /// examines when a file will not decode, and it is where the timing
    /// anomalies are. Only the pictures are missing.
    #[must_use]
    pub fn without_pixels(
        asset_name: impl Into<String>,
        frames: Vec<FrameStamp>,
        reason: impl Into<String>,
    ) -> Self {
        let frame_count = frames.len() as u64;
        Self {
            asset_name: asset_name.into(),
            dimensions: None,
            frame_rate: None,
            frame_count,
            all_frames_are_keyframes: false,
            timestamps: Vec::new(),
            frames,
            pixels_unavailable: Some(reason.into()),
            decoder_notes: Vec::new(),
        }
    }

    /// Frame indices of the random-access points.
    #[must_use]
    pub fn keyframes(&self) -> Vec<u64> {
        self.frames
            .iter()
            .filter(|f| f.is_key_frame)
            .map(|f| f.index)
            .collect()
    }

    /// Frames whose presentation order disagrees with their decode order.
    ///
    /// Reordering is normal for most codecs; what matters is that it is
    /// *visible*. A viewer showing only PTS would render a reordered stream as
    /// if it played in order.
    #[must_use]
    pub fn reordered_frames(&self) -> usize {
        self.timestamps.iter().filter(|t| t.is_reordered()).count()
    }

    /// A viewer positioned at the first frame.
    #[must_use]
    pub fn viewer(&self) -> super::viewer::ViewerState {
        super::viewer::ViewerState::new(self.frame_count)
    }

    /// The initial zoom, chosen from the frame's size.
    ///
    /// A small frame opens at 1:1 rather than "fit": at 320x240, "fit" would
    /// upscale it to fill a 1440-wide window and present interpolation as if it
    /// were detail. A frame no larger than the viewport opens actual-size, which
    /// is the only zoom at which reading individual pixels means anything.
    #[must_use]
    pub fn initial_zoom(&self) -> ZoomLevel {
        match self.dimensions.as_deref().and_then(parse_dimensions) {
            Some((width, height)) if width <= 1920 && height <= 1080 => ZoomLevel::Actual,
            _ => ZoomLevel::Fit,
        }
    }
}

/// Parses a `WxH` string into its parts.
fn parse_dimensions(text: &str) -> Option<(u32, u32)> {
    let (w, h) = text.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// The Audio screen for one asset (spec \u00a719-\u00a722, \u00a743).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioView {
    /// The asset's display name.
    pub asset_name: String,
    /// Declared sample rate, in Hz.
    pub sample_rate: Option<u32>,
    /// Declared channel count.
    pub channels: Option<u16>,
    /// Declared bit depth.
    pub bit_depth: Option<u16>,
    /// Highest sample peak, 0.0 to 1.0.
    pub peak: Option<f64>,
    /// Root-mean-square level, 0.0 to 1.0.
    pub rms: Option<f64>,
    /// Arithmetic mean; non-zero indicates DC offset.
    pub mean: Option<f64>,
    /// Integrated loudness, with the methodology that produced it.
    pub loudness: Option<MeasurementView>,
    /// Loudness range, the 10th to 95th percentile of short-term loudness.
    pub loudness_range: Option<MeasurementView>,
    /// Silent regions found.
    pub silence: Vec<SilenceView>,
    /// A peak envelope for the waveform display.
    pub waveform: Option<super::viewer::Waveform>,
    /// Why the audio could not be measured, when it could not.
    pub unavailable: Option<String>,
    /// Whether the decode hit its frame cap.
    ///
    /// Reported prominently: a truncated decode means these numbers describe a
    /// *prefix* of the track, and presenting that as the whole thing is the kind
    /// of quiet misrepresentation this project exists to avoid.
    pub truncated: bool,
}

impl AudioView {
    /// A screen for audio this build does not measure.
    #[must_use]
    pub fn unavailable(asset_name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            asset_name: asset_name.into(),
            sample_rate: None,
            channels: None,
            bit_depth: None,
            peak: None,
            rms: None,
            mean: None,
            loudness: None,
            loudness_range: None,
            silence: Vec::new(),
            waveform: None,
            unavailable: Some(reason.into()),
            truncated: false,
        }
    }

    /// Whether any audio was measured at all.
    #[must_use]
    pub fn is_measured(&self) -> bool {
        self.peak.is_some() || self.waveform.is_some()
    }
}

/// A measurement with the methodology that produced it (spec \u00a721).
///
/// The methodology travels with every figure: spec \u00a721 requires a named, citable
/// method behind each one, and a loudness reading without saying it was
/// BS.1770-4 gated is not reproducible by anyone reading it later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeasurementView {
    /// The measured value.
    pub value: f64,
    /// The unit, e.g. `LUFS`.
    pub unit: String,
    /// The named method.
    pub methodology: String,
}

/// One silent region.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SilenceView {
    /// First sample of the region.
    pub start_frame: u64,
    /// Last sample of the region.
    pub end_frame: u64,
    /// Length in samples.
    pub length_frames: u64,
    /// The region as a media time, when the sample rate is known.
    pub start: Option<MediaTime>,
    /// The region's length as a media time.
    pub duration: Option<MediaTime>,
}

/// One decoded frame, ready for the viewer to display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FramePayload {
    /// The frame's position in the sequence.
    pub frame_index: u64,
    /// The frame's timing.
    pub timestamps: FrameTimestamps,
    /// Interleaved 8-bit RGB, when the frame could be converted.
    ///
    /// `None` when the pixels were not retained, which is different from a
    /// frame that failed to convert - see [`VideoView::pixels_unavailable`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rgb: Option<Vec<u8>>,
    /// Where the RGB came from.
    pub basis: RgbBasis,
    /// The view model for the inspector and histogram.
    pub image: FrameImageView,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(index: u64, ms: i64, key: bool) -> FrameStamp {
        FrameStamp {
            index,
            time: MediaTime::from_millis(ms),
            is_key_frame: key,
        }
    }

    fn video(frames: Vec<FrameStamp>, dimensions: Option<&str>) -> VideoView {
        VideoView {
            asset_name: "master.mp4".to_owned(),
            dimensions: dimensions.map(ToOwned::to_owned),
            frame_rate: Some("25".to_owned()),
            frame_count: frames.len() as u64,
            all_frames_are_keyframes: false,
            timestamps: Vec::new(),
            frames,
            pixels_unavailable: None,
            decoder_notes: Vec::new(),
        }
    }

    #[test]
    fn a_file_with_no_decodable_codec_still_gets_a_screen() {
        // The frame *table* is readable whatever the codec, and it is exactly what
        // a reviewer needs when a file will not play. Refusing the screen would
        // hide the timing that explains why.
        let view = VideoView::without_pixels(
            "master.mp4",
            vec![stamp(0, 0, true), stamp(1, 40, false)],
            "this build does not decode `avc1`",
        );
        assert_eq!(view.frame_count, 2);
        assert_eq!(view.frames.len(), 2);
        assert!(view.pixels_unavailable.is_some());
    }

    #[test]
    fn a_refusal_names_the_policy_rather_than_blaming_the_file() {
        // "we do not decode this" and "we could not decode this" are different
        // statements. An analyst who read the second would go looking for a
        // corrupt file that is perfectly intact.
        let text = "this build does not decode `avc1`. Only royalty-free VP9 and AV1 \
                    are decoded here; the file is not damaged.";
        let view = VideoView::without_pixels("a.mp4", Vec::new(), text);
        let reason = view.pixels_unavailable.expect("a reason");
        assert!(reason.contains("does not decode"));
        assert!(reason.contains("not damaged"), "{reason}");
    }

    #[test]
    fn keyframes_are_reported_from_the_table() {
        let view = video(
            vec![
                stamp(0, 0, true),
                stamp(1, 40, false),
                stamp(2, 80, false),
                stamp(3, 120, true),
            ],
            Some("1920x1080"),
        );
        assert_eq!(view.keyframes(), vec![0, 3]);
    }

    #[test]
    fn a_small_frame_opens_actual_size_rather_than_upscaled() {
        // At 320x240 "fit" would upscale to fill a 1440-wide window and present
        // interpolation as if it were detail - and reading individual pixels is
        // the entire point of the viewer.
        assert_eq!(
            video(Vec::new(), Some("320x240")).initial_zoom(),
            ZoomLevel::Actual
        );
        assert_eq!(
            video(Vec::new(), Some("3840x2160")).initial_zoom(),
            ZoomLevel::Fit
        );
    }

    #[test]
    fn a_frame_size_that_cannot_be_parsed_does_not_panic() {
        // The dimension string is rendered text from a container; a malformed one
        // is attacker-controlled (spec §75).
        for text in ["", "x", "1920", "1920x", "axb", "1920x1080x2", "-1x-1"] {
            let _ = video(Vec::new(), Some(text)).initial_zoom();
        }
    }

    #[test]
    fn reordered_frames_are_counted_from_the_timestamps() {
        // Reordering is normal for most codecs; what matters is that it is
        // visible. A viewer showing only PTS renders a reordered stream as if it
        // played in order.
        let mut view = video(vec![stamp(0, 0, true), stamp(1, 40, false)], None);
        view.timestamps = vec![
            FrameTimestamps::new(
                MediaTime::from_millis(80),
                Some(MediaTime::from_millis(0)),
                1,
                tpt_app_media_forensics_model::Timebase::from_ticks_per_second(1_000),
                80,
                Some(0),
                false,
            ),
            FrameTimestamps::new(
                MediaTime::from_millis(40),
                Some(MediaTime::from_millis(40)),
                0,
                tpt_app_media_forensics_model::Timebase::from_ticks_per_second(1_000),
                40,
                Some(40),
                true,
            ),
        ];
        assert_eq!(view.reordered_frames(), 1);
    }

    #[test]
    fn a_viewer_over_a_known_frame_count_clamps_at_both_ends() {
        let view = video(vec![stamp(0, 0, true), stamp(1, 40, false)], None);
        let mut state = view.viewer();
        state.step(super::super::viewer::FrameStep::Back);
        assert_eq!(state.frame_index, 0);
        state.step(super::super::viewer::FrameStep::End);
        assert_eq!(state.frame_index, 1);
        state.step(super::super::viewer::FrameStep::Forward);
        assert_eq!(state.frame_index, 1);
    }

    #[test]
    fn unmeasured_audio_says_so_rather_than_showing_zeroes() {
        // Zero peak on a screen is indistinguishable from silence, and a file
        // whose audio could not be read is not a silent file.
        let view = AudioView::unavailable("master.mp4", "codec `mp4a` is not decoded");
        assert!(!view.is_measured());
        assert!(view.unavailable.is_some());
        assert_eq!(view.peak, None);
        assert_eq!(view.rms, None);
    }

    #[test]
    fn measured_audio_is_distinguishable_from_unmeasured() {
        let mut view = AudioView::unavailable("a.mp4", "unavailable");
        view.peak = Some(0.5);
        view.rms = Some(0.2);
        assert!(view.is_measured());
    }

    #[test]
    fn a_truncated_decode_is_reported_as_truncating() {
        // Truncation means the numbers describe a prefix of the track.
        // Presenting that as the whole thing is the quiet misrepresentation
        // this project exists to avoid (spec §21).
        let view = AudioView {
            truncated: true,
            ..AudioView::unavailable("a.mp4", "unavailable")
        };
        assert!(view.truncated);
    }

    #[test]
    fn a_measurement_carries_the_method_that_produced_it() {
        // Spec §21 requires a named, citable method behind every figure.
        let view = MeasurementView {
            value: -23.1,
            unit: "LUFS".to_owned(),
            methodology: "-23.1 LUFS (ITU-R BS.1770-4 / EBU R128)".to_owned(),
        };
        assert_eq!(view.unit, "LUFS");
        assert!(view.methodology.contains("BS.1770-4"));
    }

    #[test]
    fn both_screens_round_trip_through_the_ipc_boundary() {
        let view = video(vec![stamp(0, 0, true)], Some("1920x1080"));
        let json = serde_json::to_string(&view).expect("encodes");
        let decoded: VideoView = serde_json::from_str(&json).expect("decodes");
        assert_eq!(decoded, view);

        let audio = AudioView::unavailable("a.mp4", "not decoded");
        let json = serde_json::to_string(&audio).expect("encodes");
        let decoded: AudioView = serde_json::from_str(&json).expect("decodes");
        assert_eq!(decoded, audio);
    }
}
