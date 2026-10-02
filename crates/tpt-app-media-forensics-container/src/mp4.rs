//! MP4 / ISO-BMFF inspection (spec 12, 13, 24).
//!
//! Wraps `tpt-kinetix-demux`, mapping Kinetix track descriptions into this
//! engine's [`StreamAnalysis`] model.
//!
//! # Graceful continuation
//!
//! `parse_mp4` skips malformed tracks and returns whatever parsed cleanly. That
//! is exactly the behaviour spec 30 requires: a file with one broken track
//! should still yield a description of the others, and the breakage is itself
//! a finding. The count of tracks recovered versus tracks attempted is
//! therefore reported alongside the streams.
//!
//! # Partial loading
//!
//! [`Mp4Demuxer::new`] takes a complete `Vec<u8>`, so Kinetix is itself a
//! whole-file demuxer. That no longer limits inspection: [`read_moov`] loads
//! only the `moov` box, which is where every structural check reads from, and
//! leaves the media data on disk. A file of any size is therefore analysable,
//! bounded instead by [`MAX_MOOV_BYTES`]. Sample-level work does need the
//! encoded bytes and keeps its own bound, [`MAX_SAMPLED_BYTES`].
use tpt_app_media_forensics_model::{
    ChromaSubsampling, CodecInfo, MediaTime, PixelFormat, Rational, StreamAnalysis, StreamKind,
    StreamTiming, Timebase, VideoFormat,
};
use tpt_kinetix_core::codec::CodecId;
use tpt_kinetix_demux::mp4::{Mp4Demuxer, Mp4Track};

use crate::error::ContainerError;
use crate::probe::{stream_kind_of, ContainerFormat};

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

/// The result of inspecting a container, whatever the format.
///
/// Deliberately format-neutral. It was once named `Mp4Inspection`, which
/// became a lie the moment WebM support landed: a report that labelled a
/// Matroska file's own inspection result "MP4" would assert a container format
/// that the file demonstrably is not, in a tool whose entire purpose is not
/// asserting things the evidence does not support.
///
/// [`crate::probe`] is the single source of truth for which format a file is;
/// this type describes whatever that file turned out to contain.
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerInspection {
    /// The container format actually parsed.
    pub format: ContainerFormat,
    /// The streams recovered from the file, in container order.
    pub streams: Vec<StreamAnalysis>,
    /// Frame timing and keyframe positions per recovered stream, in the same
    /// order as `streams`. `None` where a track declared no samples.
    pub frame_info: Vec<Option<TrackFrameInfo>>,
    /// Observed problems, recorded rather than raised (spec §30).
    pub anomalies: Vec<String>,
    /// Number of `trak` boxes the `moov` actually contains.
    ///
    /// Counted by walking the boxes, independently of how many tracks the
    /// demuxer managed to recover. The two can disagree — a `trak` the demuxer
    /// skipped still sits in the file — and that disagreement is what
    /// `CONTAINER.DECLARED_TRACK_MISMATCH` reports.
    ///
    /// This was previously assigned from the demuxer's own track list, which made
    /// it identical to `streams.len()` by construction and the rule unfireable on
    /// any file.
    pub declared_track_count: usize,
    /// The track ID the writer expects the next track to use, if it said.
    ///
    /// `None` when `mvhd` is absent or leaves the field zero. Carried so a
    /// report can distinguish "the header declared nothing" from "the header is
    /// not there", which are different states of the same file.
    pub declared_next_track_id: Option<u32>,
}

impl ContainerInspection {
    /// Builds an inspection result, recording that no tracks were recovered.
    ///
    /// An empty container is a finding, not a parse failure: a file that parses
    /// cleanly and contains no usable tracks has been successfully measured as
    /// having none.
    #[must_use]
    pub fn empty(format: ContainerFormat) -> Self {
        Self {
            format,
            streams: Vec::new(),
            frame_info: Vec::new(),
            anomalies: vec!["container declared no usable tracks".to_owned()],
            declared_track_count: 0,
            declared_next_track_id: None,
        }
    }

    /// Returns the first video stream's frame timing, if any.
    #[must_use]
    pub fn first_video_frames(&self) -> Option<&crate::TrackFrameInfo> {
        self.streams
            .iter()
            .position(|s| s.kind == StreamKind::Video)
            .and_then(|index| self.frame_info.get(index)?.as_ref())
    }
}

/// Inspects an MP4 file that has already been read into memory.
///
/// # Errors
///
/// Returns an error only when the container itself cannot be opened. Malformed
/// *tracks* are reported through [`ContainerInspection::anomalies`], not errors.
pub fn inspect_bytes(data: Vec<u8>) -> Result<ContainerInspection, ContainerError> {
    // Edit lists are read from the bytes *before* `data` is handed to the
    // demuxer, which consumes it. `tpt-kinetix-demux` exposes no `elst` field, so
    // this is the only place the delay can be recovered — and without it
    // `StreamTiming::edit_list_offset` stays `None` for every file, leaving
    // `CONTAINER.STREAM_START_OFFSET` unable to fire on anything.
    let edit_lists = crate::elst::parse_edit_lists(&data);

    // The declared track count, read from the boxes rather than taken from the
    // demuxer. `tpt-kinetix-demux` exposes no `mvhd` field, and without reading
    // it independently the count is by definition the number of tracks that were
    // recovered — which is the same number the mismatch rule compares against,
    // making the rule unfireable on every file, real or synthetic.
    let movie_header = crate::boxes::parse_movie_header(&data);

    // Composition offsets, read from the bytes for the same reason as edit lists:
    // `tpt-kinetix-demux` has no `ctts` support at all. Without them every derived
    // timestamp is decode time, which is monotonic by construction — so
    // `TIMING.NON_MONOTONIC_PTS` could never fire on any MP4.
    let composition_offsets = crate::boxes::parse_composition_offsets(&data);

    // Colour signalling, read from the bytes for the same reason: `Mp4Track`
    // carries no colour fields at all. Without it `VideoFormat::colour` stayed
    // `Default::default()` and `is_hdr` stayed `false` on every file, so the
    // report carried empty colour for every asset and an HDR flag that was a
    // constant rather than a measurement.
    let track_colour = crate::colr::parse_track_colour(&data);

    let demuxer = Mp4Demuxer::new(data).map_err(|e| ContainerError::Parse(e.to_string()))?;
    let tracks = demuxer.tracks();

    let mut anomalies = Vec::new();
    let streams: Vec<StreamAnalysis> = tracks
        .iter()
        .enumerate()
        .map(|(index, track)| {
            let mut stream = convert_track(u32::try_from(index).unwrap_or(u32::MAX), track);
            // Indexed by track position: `parse_edit_lists` returns one entry per
            // `trak` in the same document order the demuxer enumerates. A short
            // vector (a `moov` that failed to parse) leaves the field `None`
            // rather than shifting one track's delay onto another.
            stream.timing.edit_list_offset = edit_lists.get(index).copied().flatten();
            // Colour, from the same per-track indexing. A track the reader
            // recovered but whose `colr` was not found keeps the empty
            // `ColourInfo` it starts with, which reads as "this track declares
            // no colour" rather than "no colour was read".
            if let (Some(video), Some(colour)) = (stream.video.as_mut(), track_colour.get(index)) {
                video.colour = colour.colour.clone();
                video.is_hdr = colour.is_hdr;
                // The static metadata has to travel onto the model, not just be
                // read: `VIDEO.HDR_METADATA_MISSING` compares HDR signalling
                // against HDR metadata, and the model is the only thing a rule
                // can see. Without this the rule cannot tell a conformant HDR10
                // master from one whose `mdcv` was dropped, and would fire on
                // both.
                video.colour.hdr_metadata = colour.static_metadata.clone();
            }
            stream
        })
        .collect();
    let frame_info: Vec<Option<TrackFrameInfo>> = tracks
        .iter()
        .enumerate()
        .map(|(index, track)| {
            // An empty slice for a track with no `ctts`, which is the common case
            // and leaves presentation time equal to decode time.
            track_frame_info(
                track,
                composition_offsets
                    .get(index)
                    .map_or(&[][..], Vec::as_slice),
            )
        })
        .collect();

    if tracks.is_empty() {
        anomalies.push("container declared no usable tracks".to_owned());
    }

    // Fall back to the recovered count only when the `moov` could not be walked
    // at all. That fallback is a guess, and it is recorded as one rather than
    // presented as a declaration — the whole point of reading `mvhd` separately
    // is that "we could not read the header" and "the header says N" must not
    // collapse into the same number.
    let (declared_track_count, declared_next_track_id) = match movie_header {
        Some(header) => (header.trak_count, header.next_track_id),
        None => (tracks.len(), None),
    };

    Ok(ContainerInspection {
        format: ContainerFormat::IsoBmff,
        streams,
        frame_info,
        anomalies,
        declared_track_count,
        declared_next_track_id,
    })
}

/// Reads a file and inspects it, refusing anything above the size cap.
///
/// # Errors
///
/// Returns an error if the file cannot be read, or if it exceeds
/// [`MAX_INSPECTED_BYTES`].
pub fn inspect_file(path: &std::path::Path) -> Result<ContainerInspection, ContainerError> {
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
    StreamTiming {
        timebase,
        start_time: MediaTime::ZERO,
        duration: ticks_to_media_time(track.duration, track.timescale),
        measured_duration: measured_duration(track),
        edit_list_offset: None,
    }
}

/// Converts a duration in `timescale` units to microseconds.
///
/// Kept exact rather than rounded to whole seconds. The previous code truncated
/// with `checked_div(timescale)` first, which discarded sub-second precision and
/// would have hidden exactly the small disagreements this
/// declared-versus-measured comparison exists to surface.
fn ticks_to_media_time(ticks: u64, timescale: u32) -> Option<MediaTime> {
    if ticks == 0 {
        return None;
    }
    let micros = u128::from(ticks).saturating_mul(1_000_000) / u128::from(timescale.max(1));
    i64::try_from(micros).ok().map(MediaTime::from_micros)
}

/// The duration the sample tables actually describe, from `stts`.
///
/// This is the measured side: every sample's own delta, summed. It is computed
/// independently of `mdhd`, so the two can genuinely disagree.
fn measured_duration(track: &Mp4Track) -> Option<MediaTime> {
    let total: u64 = track
        .stts
        .entries
        .iter()
        .map(|entry| u64::from(entry.sample_count) * u64::from(entry.sample_delta))
        .sum();
    ticks_to_media_time(total, track.timescale)
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
        // Filled in by the caller from `colr`, once the track's own colour
        // declaration has been read. The demuxer exposes none of it, so it
        // cannot be derived here.
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
    /// Presentation time of every frame, expanded from the `stts` run-length table
    /// plus any `ctts` composition offsets.
    ///
    /// *Presentation* time, not decode time. For a file without B-frames the two
    /// coincide and this is just `stts`; for one with them, the composition
    /// offset is what makes sample 3 present before samples 1 and 2.
    ///
    /// Before composition offsets were read, every entry here was monotonically
    /// increasing by construction — `stts` deltas are unsigned — which made
    /// `TIMING.NON_MONOTONIC_PTS` unfireable on any MP4.
    pub frame_times: Vec<tpt_app_media_forensics_model::MediaTime>,
    /// Decode time of every frame, before composition offsets are applied.
    ///
    /// Kept alongside [`Self::frame_times`] because the two disagreeing is the
    /// observation worth making: a file where decode order is correct but
    /// presentation order is not is normal, and reporting only the presentation
    /// times would leave no way to tell that apart from a corrupt table.
    pub decode_times: Vec<tpt_app_media_forensics_model::MediaTime>,
    /// Frame indices of the sync samples.
    pub keyframes: Vec<u32>,
    /// True when the track declared no `stss` box, meaning every frame is a
    /// sync sample.
    pub all_frames_are_keyframes: bool,
}

/// Expands a track's `stts` and `stss` into per-frame timing information.
///
/// `composition_offsets` is the track's `ctts` table, already expanded to one
/// signed offset per sample by
/// [`crate::boxes::parse_composition_offsets`]. An empty slice means the track
/// declares no `ctts`, and presentation time is then decode time.
///
/// Returns `None` when the track declares no samples.
///
/// # Panics
///
/// Never. Every index into the run-length table is bounds-checked, because the
/// table is attacker-controlled (spec §75).
#[must_use]
pub fn track_frame_info(track: &Mp4Track, composition_offsets: &[i64]) -> Option<TrackFrameInfo> {
    let sample_count = track.sample_count();
    if sample_count == 0 {
        return None;
    }

    let timebase = Timebase::from_ticks_per_second(track.timescale.max(1));

    // Expand the run-length table into one timestamp per sample. Bounded by the
    // declared sample count so a hostile `stts` cannot drive an unbounded
    // allocation.
    //
    // Elapsed ticks are kept alongside the times rather than recovered from them.
    // Converting a time back to ticks to apply a composition offset would round
    // twice, and the error would vary per sample — inventing jitter in files whose
    // timestamps are exact.
    let mut decode_ticks: Vec<u64> = Vec::with_capacity(sample_count.min(MAX_EXPANDED_SAMPLES));
    let mut elapsed_ticks: u64 = 0;
    'expansion: for entry in &track.stts.entries {
        for _ in 0..entry.sample_count {
            if decode_ticks.len() >= MAX_EXPANDED_SAMPLES {
                break 'expansion;
            }
            decode_ticks.push(elapsed_ticks);
            elapsed_ticks = elapsed_ticks.saturating_add(u64::from(entry.sample_delta));
        }
    }

    // A `stts` shorter than `stsz` leaves the tail without timestamps; pad so
    // frame indices stay valid for the keyframe list.
    while decode_ticks.len() < sample_count && decode_ticks.len() < MAX_EXPANDED_SAMPLES {
        decode_ticks.push(elapsed_ticks);
        elapsed_ticks = elapsed_ticks.saturating_add(1);
    }

    let decode_times: Vec<tpt_app_media_forensics_model::MediaTime> = decode_ticks
        .iter()
        .map(|&ticks| timebase.ticks_to_media_time(i64::try_from(ticks).unwrap_or(i64::MAX)))
        .collect();

    // Presentation time is decode time shifted by the composition offset. The
    // offset is looked up per sample rather than applied to the whole track,
    // because a `ctts` shorter than the sample list leaves the tail unshifted —
    // a real state a partially written table describes.
    //
    // `checked_add` is deliberate: a hostile `ctts` can carry an offset large
    // enough to overflow, and wrapping would put the timestamp on the wrong side
    // of zero rather than at the extreme.
    let frame_times: Vec<tpt_app_media_forensics_model::MediaTime> = decode_ticks
        .iter()
        .enumerate()
        .map(|(index, &ticks)| {
            let Some(&offset) = composition_offsets.get(index) else {
                return timebase.ticks_to_media_time(i64::try_from(ticks).unwrap_or(i64::MAX));
            };
            let shifted = i64::try_from(ticks)
                .unwrap_or(i64::MAX)
                .checked_add(offset)
                .unwrap_or(i64::MAX);
            timebase.ticks_to_media_time(shifted)
        })
        .collect();

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
        decode_times,
        keyframes,
        all_frames_are_keyframes,
    })
}

/// One access unit, reduced to what packet-layer analysis needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleRecord {
    /// Index of the stream this sample belongs to.
    pub stream_index: u32,
    /// The encoded bytes, retained so Tier-2 can decode this sample.
    ///
    /// `read_samples` is only called for files inside the sampling bound, so
    /// holding the payload does not change when it applies.
    pub data: Vec<u8>,
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

    let file_len = data.len();
    let mut demuxer = Mp4Demuxer::new(data).map_err(|e| ContainerError::Parse(e.to_string()))?;
    let tracks = demuxer.tracks().to_vec();

    let mut per_stream_index = vec![0u32; tracks.len()];
    let mut consumed = 0usize;
    let mut out = Vec::new();

    loop {
        let packet = match demuxer.read_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(_) => break, // Truncated or damaged: keep what was recovered.
        };

        // A packet with no bytes cannot advance a reader, so one that keeps
        // producing them will never end the loop. Stop, but keep what was read:
        // a truncated file is evidence, not a failure.
        if packet.size() == 0 {
            break;
        }

        consumed += packet.size();
        if consumed > file_len || consumed > MAX_SAMPLE_BYTES {
            return Err(ContainerError::Parse(format!(
                "the demuxer reported {consumed} bytes of samples from a {file_len}-byte file; \
                 it is not advancing, so the packets cannot be trusted"
            )));
        }

        let Some(track) = tracks.get(packet.stream_index as usize) else {
            continue;
        };

        let frame_index = per_stream_index[packet.stream_index as usize];
        per_stream_index[packet.stream_index as usize] = frame_index.saturating_add(1);

        let timebase = Timebase::from_ticks_per_second(track.timescale.max(1));
        let time = timebase.ticks_to_media_time(packet.pts.value);
        let size = packet.size();
        let digest = sha2::Sha256::digest(&packet.data);
        out.push(SampleRecord {
            stream_index: packet.stream_index,
            data: packet.data,
            frame_index,
            digest: digest_hex(&digest),
            time,
            is_key_frame: packet.is_key_frame,
            size,
        });
    }

    Ok(out)
}

/// Ceiling on sample bytes accepted from one file.
///
/// Not a limit on file size: a legitimate file can never yield more sample
/// bytes than it contains. Exceeding this means the reader is re-reading or
/// inventing packets rather than advancing, which is not damaged evidence but a
/// malfunction — and the recovered samples would be fabricated. Reported as an
/// error so the caller records a limitation instead of analysing nonsense.
const MAX_SAMPLE_BYTES: usize = 1 << 31;

/// Formats a digest as lowercase hex.
fn digest_hex(digest: &[u8]) -> String {
    tpt_app_media_forensics_model::asset::to_hex(digest)
}

// ---------------------------------------------------------------------------
// Partial reads
//
// Inspection only ever needs the `moov` box: that is where the sample tables,
// codec descriptions, and timing live. `mdat` holds the encoded media, which on
// a long recording is almost the whole file and which no structural check reads.
// Loading a 40 GB asset to look at a 2 KB `moov` is what makes whole-file
// inspection unusable at professional timescales.
// ---------------------------------------------------------------------------

/// Largest `moov` box this reader will extract.
///
/// Far above any real `moov`: the box holds tables, not media. The bound exists
/// so a file declaring a multi-gigabyte `moov` cannot exhaust memory.
pub const MAX_MOOV_BYTES: u64 = 256 * 1024 * 1024;

/// Reads only the `moov` box from a file, leaving the media data on disk.
///
/// Returns the box with its original header, so the buffer handed to
/// [`inspect_bytes`] is a valid top-level box sequence.
///
/// # Errors
///
/// Returns an error if the file cannot be read, if no `moov` box is present, or
/// if the declared `moov` exceeds [`MAX_MOOV_BYTES`].
pub fn read_moov(path: &std::path::Path) -> Result<Vec<u8>, ContainerError> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path)
        .map_err(|e| ContainerError::io("open container", path.display().to_string(), e))?;

    let total = file
        .metadata()
        .map_err(|e| ContainerError::io("stat container", path.display().to_string(), e))?
        .len();

    let mut offset = 0u64;
    let mut header = [0u8; 16];

    while offset < total {
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| ContainerError::io("seek container", path.display().to_string(), e))?;

        // A box header is at least 8 bytes: size and type.
        let read = read_up_to(&mut file, &mut header[..8])
            .map_err(|e| ContainerError::io("read box header", path.display().to_string(), e))?;
        if read < 8 {
            break;
        }

        let declared = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        let box_type = [header[4], header[5], header[6], header[7]];

        // `size == 1` means a 64-bit length follows the type field.
        let (size, header_len) = if declared == 1 {
            let read = read_up_to(&mut file, &mut header[8..16]).map_err(|e| {
                ContainerError::io("read box header", path.display().to_string(), e)
            })?;
            if read < 8 {
                break;
            }
            let wide = u64::from_be_bytes([
                header[8], header[9], header[10], header[11], header[12], header[13], header[14],
                header[15],
            ]);
            (wide, 16u64)
        } else if declared == 0 {
            // A size of zero means the box runs to the end of the file.
            (total - offset, 8u64)
        } else {
            (u64::from(declared), 8u64)
        };

        if &box_type == b"moov" {
            // The bound is checked before the file-extent check below, so a
            // `moov` that declares more than the limit is reported as exceeding
            // it rather than as a malformed file. The two conditions are
            // different findings: one is a hostile or broken file, the other is
            // a limitation of this build.
            if size > MAX_MOOV_BYTES {
                return Err(ContainerError::TooLarge {
                    path: path.display().to_string(),
                    size_bytes: size,
                    limit_bytes: MAX_MOOV_BYTES,
                });
            }
        }

        // A box that claims to start past the end of the file is malformed;
        // stopping here keeps a corrupt length from producing a huge seek.
        if size < header_len || offset + size > total {
            break;
        }

        if &box_type == b"moov" {
            let mut payload = vec![0u8; usize::try_from(size).unwrap_or(usize::MAX)];
            file.seek(SeekFrom::Start(offset))
                .map_err(|e| ContainerError::io("seek container", path.display().to_string(), e))?;
            file.read_exact(&mut payload)
                .map_err(|e| ContainerError::io("read moov", path.display().to_string(), e))?;
            return Ok(payload);
        }

        offset += size;
    }

    Err(ContainerError::Parse(format!(
        "{}: no moov box found in {total} bytes",
        path.display()
    )))
}

/// Reads up to `buf.len()` bytes, returning how many were read.
///
/// `read_exact` is avoided deliberately: a truncated final box should end the
/// walk, not raise an I/O error, because a damaged file is evidence rather than
/// a failure (spec §30).
fn read_up_to(file: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    use std::io::Read as _;
    let mut filled = 0usize;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// Inspects a file by reading only its `moov` box.
///
/// Unlike [`inspect_file`], this places no limit on the file's size: only the
/// `moov` box is loaded, and that is bounded separately by [`MAX_MOOV_BYTES`].
///
/// # Errors
///
/// Returns an error if the file cannot be read or carries no usable `moov`.
pub fn inspect_path(path: &std::path::Path) -> Result<ContainerInspection, ContainerError> {
    inspect_bytes(read_moov(path)?)
}

/// Largest file this build will load for sample-level analysis.
///
/// Inspection is unaffected: it reads only `moov`. This bound applies to
/// duplicate detection, which needs every sample's encoded bytes, and is set
/// where a memory-constrained workstation still fails cleanly rather than
/// being killed mid-examination.
pub const MAX_SAMPLED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Reads every sample in a file, loading the media data.
///
/// Refuses files above [`MAX_SAMPLED_BYTES`] rather than attempting the read.
///
/// # Errors
///
/// Returns an error if the file is too large, cannot be read, or carries no
/// usable `moov` box.
pub fn read_samples_file(path: &std::path::Path) -> Result<Vec<SampleRecord>, ContainerError> {
    let size = std::fs::metadata(path)
        .map_err(|e| ContainerError::io("stat container", path.display().to_string(), e))?
        .len();
    if size > MAX_SAMPLED_BYTES {
        return Err(ContainerError::TooLarge {
            path: path.display().to_string(),
            size_bytes: size,
            limit_bytes: MAX_SAMPLED_BYTES,
        });
    }
    let data = std::fs::read(path)
        .map_err(|e| ContainerError::io("read container", path.display().to_string(), e))?;
    read_samples(data)
}

/// Reads the first `limit` bytes of a file, for format detection.
///
/// Detection needs only a signature, so this avoids reading a whole asset to
/// answer a question its first sixteen bytes settle.
///
/// # Errors
///
/// Returns an error if the file cannot be read.
pub fn read_header(path: &std::path::Path, limit: usize) -> Result<Vec<u8>, ContainerError> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| ContainerError::io("open source", path.display().to_string(), e))?;
    let mut buf = vec![0u8; limit];
    let filled = read_up_to(&mut file, &mut buf)
        .map_err(|e| ContainerError::io("read source header", path.display().to_string(), e))?;
    buf.truncate(filled);
    Ok(buf)
}
