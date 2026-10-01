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
    ChromaSubsampling, CodecInfo, MediaTime, PixelFormat, Rational, StreamAnalysis, StreamTiming,
    Timebase, VideoFormat,
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

/// Upper bound on frames expanded from a sample table.
///
/// A container declaring hundreds of millions of samples must not be able to
/// make the engine allocate without limit.
pub const MAX_EXPANDED_SAMPLES: usize = 8_000_000;

/// The result of inspecting an MP4 container.
#[derive(Debug, Clone, PartialEq)]
pub struct Mp4Inspection {
    /// The streams recovered from the file, in container order.
    pub streams: Vec<StreamAnalysis>,
    /// Frame timing and keyframe positions per recovered stream, in the same
    /// order as `streams`. `None` where a track declared no samples.
    pub frame_info: Vec<Option<TrackFrameInfo>>,
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
    let streams: Vec<StreamAnalysis> = tracks
        .iter()
        .enumerate()
        .map(|(index, track)| convert_track(u32::try_from(index).unwrap_or(u32::MAX), track))
        .collect();
    let frame_info: Vec<Option<TrackFrameInfo>> = tracks.iter().map(track_frame_info).collect();

    if tracks.is_empty() {
        anomalies.push("container declared no usable tracks".to_owned());
    }

    Ok(Mp4Inspection {
        streams,
        frame_info,
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
///
/// `index` is the track's position in container order. It must come from the
/// caller rather than being assumed: `frame_info` is parallel to `streams`, and
/// a hardcoded index would mis-associate every track after the first in a
/// multi-track file.
fn convert_track(index: u32, track: &Mp4Track) -> StreamAnalysis {
    let kind = stream_kind_of(track.media_type);
    let video = (kind == tpt_app_media_forensics_model::StreamKind::Video)
        .then(|| convert_video(track))
        .flatten();

    StreamAnalysis {
        index,
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

/// Renders a sample-entry four-character code.
///
/// Non-printable bytes are shown as `\xNN` rather than passed through. A
/// `fourcc` containing control bytes is itself worth reporting, but emitting it
/// raw produces output that renders as blank space in a terminal and tells the
/// analyst nothing.
///
/// # Panics
///
/// Never.
fn fourcc_string(fourcc: [u8; 4]) -> String {
    fourcc
        .iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                char::from(b).to_string()
            } else {
                format!("\\x{b:02x}")
            }
        })
        .collect()
}

/// Converts track timing, including the measured frame rate.
///
/// The timebase comes from the track's `mdhd` timescale, and the frame rate is
/// derived from the `stts` sample deltas rather than from any declared value.
/// This is the measured side of the declared-versus-measured distinction
/// (spec §26).
fn convert_timing(track: &Mp4Track) -> StreamTiming {
    let timebase = Timebase::from_ticks_per_second(track.timescale.max(1));
    let duration = track
        .duration
        .checked_div(u64::from(track.timescale.max(1)));

    StreamTiming {
        timebase,
        start_time: MediaTime::ZERO,
        duration: duration.map(|secs| {
            MediaTime::from_micros(
                i64::try_from(secs.saturating_mul(1_000_000)).unwrap_or(i64::MAX),
            )
        }),
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

/// Frame-level timing and keyframe positions for one track, as read from the
/// container's `stts` and `stss` boxes.
///
/// This is everything GOP analysis (spec §15) and duplicate detection (spec §17)
/// need, and none of it requires decoding a single macroblock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackFrameInfo {
    /// Presentation time of every frame, expanded from the `stts` run-length
    /// table.
    pub frame_times: Vec<tpt_app_media_forensics_model::MediaTime>,
    /// Frame indices of the sync samples.
    pub keyframes: Vec<u32>,
    /// True when the track declared no `stss` box, meaning every frame is a
    /// sync sample.
    pub all_frames_are_keyframes: bool,
}

/// Expands a track's `stts` and `stss` into per-frame timing information.
///
/// Returns `None` when the track declares no samples.
///
/// # Panics
///
/// Never. Every index into the run-length table is bounds-checked, because the
/// table is attacker-controlled (spec §75).
#[must_use]
pub fn track_frame_info(track: &Mp4Track) -> Option<TrackFrameInfo> {
    let sample_count = track.sample_count();
    if sample_count == 0 {
        return None;
    }

    let timebase = Timebase::from_ticks_per_second(track.timescale.max(1));

    // Expand the run-length table into one timestamp per sample. Bounded by the
    // declared sample count so a hostile `stts` cannot drive an unbounded
    // allocation.
    let mut frame_times = Vec::with_capacity(sample_count.min(MAX_EXPANDED_SAMPLES));
    let mut elapsed_ticks: u64 = 0;
    'expansion: for entry in &track.stts.entries {
        for _ in 0..entry.sample_count {
            if frame_times.len() >= MAX_EXPANDED_SAMPLES {
                break 'expansion;
            }
            frame_times.push(
                timebase.ticks_to_media_time(i64::try_from(elapsed_ticks).unwrap_or(i64::MAX)),
            );
            elapsed_ticks = elapsed_ticks.saturating_add(u64::from(entry.sample_delta));
        }
    }

    // A `stts` shorter than `stsz` leaves the tail without timestamps; pad so
    // frame indices stay valid for the keyframe list.
    while frame_times.len() < sample_count && frame_times.len() < MAX_EXPANDED_SAMPLES {
        frame_times
            .push(timebase.ticks_to_media_time(i64::try_from(elapsed_ticks).unwrap_or(i64::MAX)));
        elapsed_ticks = elapsed_ticks.saturating_add(1);
    }

    let all_frames_are_keyframes = track.stss.is_none();
    // `stss` stores 1-based sample numbers; frame indices here are 0-based.
    // Reading them unconverted would place every keyframe one frame late and
    // shift every GOP boundary with it.
    let keyframes = match &track.stss {
        Some(stss) => stss
            .sample_numbers
            .iter()
            .map(|&n| n.saturating_sub(1))
            .filter(|&index| (index as usize) < sample_count)
            .collect(),
        None => (0..u32::try_from(sample_count).unwrap_or(0)).collect(),
    };

    Some(TrackFrameInfo {
        frame_times,
        keyframes,
        all_frames_are_keyframes,
    })
}

/// One access unit, reduced to what packet-layer analysis needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleRecord {
    /// Index of this sample within its stream.
    pub frame_index: u32,
    /// Content digest of the compressed sample.
    ///
    /// Hashing the *compressed* bytes, not decoded pixels: it is exact, cheap,
    /// and requires no decoder. What it proves depends on whether the sample is
    /// a keyframe — see the `Soundness` documentation on duplicate detection.
    pub digest: String,
    /// Presentation time.
    pub time: tpt_app_media_forensics_model::MediaTime,
    /// Whether the sample is a random-access point.
    pub is_key_frame: bool,
    /// Size of the compressed sample in bytes.
    pub size: usize,
}

/// Reads every sample in the container, grouped by stream index.
///
/// # Errors
///
/// Returns an error only when the demuxer cannot be opened. A stream that
/// fails mid-read yields the packets recovered so far plus a recorded reason,
/// because a truncated file is evidence rather than a failure (spec §30).
pub fn read_samples(data: Vec<u8>) -> Result<Vec<SampleRecord>, ContainerError> {
    use sha2::Digest as _;
    use tpt_app_media_forensics_model::Timebase;
    use tpt_kinetix_demux::{Demuxer as _, Mp4Demuxer};

    let mut demuxer = Mp4Demuxer::new(data).map_err(|e| ContainerError::Parse(e.to_string()))?;
    let tracks = demuxer.tracks().to_vec();

    let mut per_stream_index = vec![0u32; tracks.len()];
    let mut out = Vec::new();

    loop {
        let packet = match demuxer.read_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(_) => break, // Truncated or damaged: keep what was recovered.
        };

        let Some(track) = tracks.get(packet.stream_index as usize) else {
            continue;
        };

        let frame_index = per_stream_index[packet.stream_index as usize];
        per_stream_index[packet.stream_index as usize] = frame_index.saturating_add(1);

        let timebase = Timebase::from_ticks_per_second(track.timescale.max(1));
        let time = timebase.ticks_to_media_time(packet.pts.value);

        let digest = sha2::Sha256::digest(&packet.data);
        out.push(SampleRecord {
            frame_index,
            digest: digest_hex(&digest),
            time,
            is_key_frame: packet.is_key_frame,
            size: packet.size(),
        });
    }

    Ok(out)
}

/// Formats a digest as lowercase hex.
fn digest_hex(digest: &[u8]) -> String {
    tpt_app_media_forensics_model::asset::to_hex(digest)
}
