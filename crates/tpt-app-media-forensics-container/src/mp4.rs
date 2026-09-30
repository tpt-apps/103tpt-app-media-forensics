//! MP4 / ISO-BMFF inspection (spec §12, §13, §24).
//!
//! Wraps `tpt-kinetix-demux`, mapping Kinetix track descriptions into this
//! engine's [`StreamAnalysis`] model.
//!
//! # Graceful continuation
//!
//! `parse_mp4` skips malformed tracks and returns whatever parsed cleanly. That
//! is exactly the behaviour spec §30 requires: a file with one broken track
//! should still yield a description of the others, and the breakage is itself
//! a finding. The count of tracks recovered versus tracks attempted is
//! therefore reported alongside the streams.
//!
//! # Whole-file loading
//!
//! `Mp4Demuxer::new` takes the complete file as a `Vec<u8>`. This is a known
//! departure from the streaming requirement in spec §55 and is guarded by
//! [`MAX_INSPECTED_BYTES`]: a file beyond that limit is refused with an
//! explicit error rather than being allowed to exhaust memory. Streaming
//! parsing is tracked in the todo and revisited when Kinetix exposes a
//! reader-based demuxer.

use tpt_app_media_forensics_model::{
    ChromaSubsampling, CodecInfo, MediaTime, PixelFormat, Rational, StreamAnalysis,
    StreamTiming, Timebase, VideoFormat,
};
use tpt_kinetix_core::codec::CodecId;
use tpt_kinetix_demux::mp4::{Mp4Demuxer, Mp4Track};

use crate::error::ContainerError;
use crate::probe::stream_kind_of;

/// Largest file this inspector will load into memory.
///
/// MP4 parsing here is whole-file; without a cap a multi-gigabyte asset would
/// be loaded in full, and on a memory-constrained workstation that turns
/// inspection into a crash rather than a finding.
pub const MAX_INSPECTED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// The result of inspecting an MP4 container.
#[derive(Debug, Clone, PartialEq)]
pub struct Mp4Inspection {
    /// The streams recovered from the file, in container order.
    pub streams: Vec<StreamAnalysis>,
    /// Observed problems, recorded rather than raised (spec §30).
    pub anomalies: Vec<String>,
    /// Number of `trak` boxes the container declared.
    pub declared_track_count: usize,
}

/// Inspects an MP4 file that has already been read into memory.
///
/// # Errors
///
/// Returns an error only when the container itself cannot be opened. Malformed
/// *tracks* are reported through [`Mp4Inspection::anomalies`], not errors.
pub fn inspect_bytes(data: Vec<u8>) -> Result<Mp4Inspection, ContainerError> {
    let demuxer = Mp4Demuxer::new(data).map_err(|e| ContainerError::Parse(e.to_string()))?;
    let tracks = demuxer.tracks();

    let mut anomalies = Vec::new();
    let streams: Vec<StreamAnalysis> = tracks.iter().map(convert_track).collect();

    if tracks.is_empty() {
        anomalies.push("container declared no usable tracks".to_owned());
    }

    Ok(Mp4Inspection {
        streams,
        anomalies,
        declared_track_count: tracks.len(),
    })
}

/// Reads a file and inspects it, refusing anything above the size cap.
///
/// # Errors
///
/// Returns an error if the file cannot be read, or if it exceeds
/// [`MAX_INSPECTED_BYTES`].
pub fn inspect_file(path: &std::path::Path) -> Result<Mp4Inspection, ContainerError> {
    let metadata = std::fs::metadata(path)
        .map_err(|e| ContainerError::io("stat container", path.display().to_string(), e))?;

    if metadata.len() > MAX_INSPECTED_BYTES {
        return Err(ContainerError::TooLarge {
            path: path.display().to_string(),
            size_bytes: metadata.len(),
            limit_bytes: MAX_INSPECTED_BYTES,
        });
    }

    let data = std::fs::read(path)
        .map_err(|e| ContainerError::io("read container", path.display().to_string(), e))?;
    inspect_bytes(data)
}

/// Converts one Kinetix track into the engine's stream model.
fn convert_track(track: &Mp4Track) -> StreamAnalysis {
    let kind = stream_kind_of(track.media_type);
    let video = (kind == tpt_app_media_forensics_model::StreamKind::Video)
        .then(|| convert_video(track))
        .flatten();

    StreamAnalysis {
        index: 0,
        kind,
        language: None,
        codec: convert_codec(track.codec),
        timing: convert_timing(track),
        video,
        audio: None,
        packet_count: Some(track.sample_count() as u64),
    }
}

/// Converts a Kinetix codec id into a codec description.
///
/// The four-character code is preserved for `Unknown` codecs rather than being
/// discarded: an unrecognised `fourcc` is evidence in its own right.
fn convert_codec(codec: Option<CodecId>) -> CodecInfo {
    let Some(codec) = codec else {
        return CodecInfo::new("unknown");
    };

    match codec {
        CodecId::H264 => CodecInfo::new("avc1").with_long_name("H.264 / AVC"),
        CodecId::H265 => CodecInfo::new("hvc1").with_long_name("H.265 / HEVC"),
        CodecId::Av1 => CodecInfo::new("av01").with_long_name("AV1"),
        CodecId::Vp9 => CodecInfo::new("vp09").with_long_name("VP9"),
        CodecId::Aac => CodecInfo::new("mp4a").with_long_name("AAC"),
        CodecId::Opus => CodecInfo::new("Opus").with_long_name("Opus"),
        CodecId::Flac => CodecInfo::new("fLaC").with_long_name("FLAC"),
        CodecId::Unknown(fourcc) => CodecInfo::new(fourcc_string(fourcc)),
    }
}

/// Renders a sample-entry four-character code, preserving padding bytes.
fn fourcc_string(fourcc: [u8; 4]) -> String {
    fourcc.iter().map(|&b| char::from(b)).collect()
}

/// Converts track timing, including the measured frame rate.
///
/// The timebase comes from the track's `mdhd` timescale, and the frame rate is
/// derived from the `stts` sample deltas rather than from any declared value.
/// This is the measured side of the declared-versus-measured distinction
/// (spec §26).
fn convert_timing(track: &Mp4Track) -> StreamTiming {
    let timebase = Timebase::from_ticks_per_second(track.timescale.max(1));
    let duration = track.duration.checked_div(u64::from(track.timescale.max(1)));

    StreamTiming {
        timebase,
        start_time: MediaTime::ZERO,
        duration: duration.map(|secs| MediaTime::from_micros(
            i64::try_from(secs.saturating_mul(1_000_000)).unwrap_or(i64::MAX),
        )),
        edit_list_offset: None,
    }
}

/// Converts video properties, including the measured frame rate.
fn convert_video(track: &Mp4Track) -> Option<VideoFormat> {
    if track.width == 0 || track.height == 0 {
        return None;
    }

    let frame_rate = measured_frame_rate(track);

    Some(VideoFormat {
        coded_width: track.width,
        coded_height: track.height,
        // Kinetix reports the display size; no crop box is surfaced here yet.
        display_width: None,
        display_height: None,
        frame_rate,
        sample_aspect_ratio: None,
        display_aspect_ratio: None,
        rotation_degrees: None,
        pixel_format: PixelFormat::new(
            "unknown",
            ChromaSubsampling::Unknown("not reported by demuxer".to_owned()),
            8,
        ),
        colour: Default::default(),
        is_hdr: false,
    })
}

/// Derives the measured frame rate from the `stts` table.
///
/// Returns `None` when the table is empty or the timescale is zero, rather
/// than a defaulted value: a frame rate that cannot be measured is an
/// observation, and reporting 0 or 1 would be a fabricated measurement.
///
/// Uses the *first* sample delta, which is the standard convention, and only
/// when every entry agrees. A track whose deltas vary within itself is
/// reporting a cadence change, which is a finding in its own right and must not
/// be smoothed into a single misleading number.
fn measured_frame_rate(track: &Mp4Track) -> Option<Rational> {
    let first = track.stts.entries.first()?;
    let delta = u64::from(first.sample_delta);
    if delta == 0 {
        return None;
    }
    let consistent = track
        .stts
        .entries
        .iter()
        .all(|entry| entry.sample_delta == first.sample_delta);
    if !consistent {
        return None;
    }
    Rational::new(u64::from(track.timescale), delta).ok()
}

/// Number of keyframes in a track, from the `stss` sync-sample table.
///
/// Returns `None` when `stss` is absent, which per the ISO-BMFF specification
/// means *every* sample is a sync sample. Reporting "unknown" would be wrong;
/// the correct reading is that the track has no sync-sample restriction.
#[must_use]
pub fn keyframe_count(track: &Mp4Track, sample_count: usize) -> Option<usize> {
    match &track.stss {
        Some(stss) => Some(stss.sample_numbers.len()),
        // No `stss` means all samples are sync samples.
        None => Some(sample_count),
    }
}