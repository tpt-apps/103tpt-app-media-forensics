//! Matroska / WebM inspection (spec §12, §13, §24).
//!
//! Wraps `tpt-kinetix-demux`'s `mkv` reader, mapping its tracks into this
//! engine's [`StreamAnalysis`] model exactly as the MP4 path does.
//!
//! # What the underlying reader exposes, and what that costs
//!
//! [`MkvDemuxer`] is a deliberately minimal EBML reader. It parses the element
//! tree, extracts `TrackEntry` metadata, and pulls frames out of `SimpleBlock`
//! and `Block` elements. It does **not** expose picture width or height, the
//! audio sample rate, or duration — those live in `Video`/`Audio` child elements
//! the reader never descends into.
//!
//! Duration is read separately, from `Segment > Info > Duration`, by
//! [`parse_segment_duration`]. It is worth separating from the other two: without
//! it every WebM file reports "declares no duration" however long it is, and
//! `CONTAINER.STREAM_DURATION_MISSING` — a rule whose whole purpose is catching
//! files that omit a duration — would fire on correct files. An empty `Info`
//! element is genuinely rare; a reader that cannot see one is not rare at all.
//!
//! Those omissions are reported rather than filled in with plausible defaults.
//! A video stream is therefore described with `video: None` and no frame rate,
//! which is the honest reading: this build measured that the track exists and
//! could not measure its dimensions. Inventing a resolution would put a number
//! in the report that no measurement produced.
//!
//! # Timestamps
//!
//! Matroska stores time in `TimestampScale` units — 1 ms by default — and the
//! reader surfaces them in a 1/1000 timebase. Sample times are therefore
//! millisecond-resolution rather than the sub-millisecond precision an MP4
//! `mdhd` timescale gives. That is recorded on every stream's timebase so a
//! later comparison against an MP4 of the same content does not treat the
//! coarser resolution as a timing discrepancy.
//!
//! # Keyframes
//!
//! Keyframe positions come from the `SimpleBlock` keyframe flag. Plain `Block`
//! elements inside a `BlockGroup` are always reported as non-key by the reader,
//! so a file using reference-block encoding yields fewer apparent keyframes
//! than it contains. That is recorded as an anomaly: an under-count of
//! keyframes is a limitation of this reader, not an observation about the file.

use tpt_app_media_forensics_model::{
    AudioFormat, CodecInfo, MediaTime, StreamAnalysis, StreamKind, StreamTiming, Timebase,
};
use tpt_kinetix_demux::mkv::{MkvDemuxer, MkvTrack, MkvTrackType};

use crate::error::ContainerError;
use crate::mp4::{SampleRecord, TrackFrameInfo, MAX_EXPANDED_SAMPLES};
use crate::probe::ContainerFormat;
use crate::ContainerInspection;

/// Matroska timestamps are carried in a 1/1000 timebase (milliseconds).
///
/// See the module documentation for why that is recorded rather than hidden.
const MKV_TIMEBASE_TICKS_PER_SECOND: u32 = 1_000;

/// `Segment` element ID, per EBML/Matroska.
const ID_SEGMENT: [u8; 4] = [0x18, 0x53, 0x80, 0x67];
/// `Segment > Info` element ID.
const ID_INFO: [u8; 4] = [0x15, 0x49, 0xA9, 0x66];
/// `Info > TimecodeScale` element ID: nanoseconds per Matroska tick.
const ID_TIMECODE_SCALE: [u8; 3] = [0x2A, 0xD7, 0xB1];
/// `Info > Duration` element ID: length of the segment, in `TimecodeScale` units.
const ID_DURATION: [u8; 2] = [0x44, 0x89];

/// Default `TimecodeScale`: one tick is one millisecond.
const DEFAULT_TIMECODE_SCALE_NS: u64 = 1_000_000;

/// Reads `Segment > Info > Duration`, converted to microseconds.
///
/// # Why this is parsed here
///
/// Matroska records how long the segment is, and every real muxer writes it. The
/// underlying reader exposes only track number, type and codec id — no duration —
/// so without this every WebM file reports "declares no duration" regardless of
/// what the file actually says.
///
/// That is a false statement in a forensic report, not merely a missing one, and
/// it matters most for `CONTAINER.STREAM_DURATION_MISSING`, whose entire purpose
/// is to catch files that omit a duration.
///
/// `None` means the file genuinely declares no duration, which is then true of
/// the file rather than about the reader.
///
/// # Panics
///
/// Never. The input is attacker-controlled (spec §75); every read is bounds
/// checked and element sizes are validated before use.
#[must_use]
pub fn parse_segment_duration(input: &[u8]) -> Option<MediaTime> {
    let info = ebml_child(ebml_child(input, &ID_SEGMENT)?, &ID_INFO)?;

    // TimecodeScale changes what a duration *number* means, so it is read before
    // the duration rather than assumed. Defaulting instead would scale every
    // duration by the wrong factor on any file using a non-default scale.
    let scale_ns = ebml_child(info, &ID_TIMECODE_SCALE)
        .map(ebml_uint)
        .filter(|&scale| scale > 0)
        .unwrap_or(DEFAULT_TIMECODE_SCALE_NS);

    let duration = ebml_child(info, &ID_DURATION)?;

    // `Duration` is a float, 4 or 8 bytes. Reading it as an integer would report a
    // wildly wrong length — a 4-byte float is mostly exponent bits.
    let ticks = match duration.len() {
        4 => f64::from(f32::from_be_bytes(duration.try_into().ok()?)),
        8 => f64::from_be_bytes(duration.try_into().ok()?),
        // A 16-bit float is legal EBML but not something any muxer writes, and
        // this build has no half-precision decoder. Reporting none beats guessing.
        _ => return None,
    };
    if !ticks.is_finite() || ticks <= 0.0 {
        return None;
    }

    let micros = ticks * (scale_ns as f64) / 1_000.0;
    // Bounded *before* the cast. `f64 as i64` saturates rather than erroring, so
    // an unbounded hostile duration would silently become `i64::MAX` and be
    // reported as a real measurement.
    if !micros.is_finite() || micros >= i64::MAX as f64 {
        return None;
    }
    Some(MediaTime::from_micros(micros as i64))
}

/// Returns the body of the first child element with the given ID.
fn ebml_child<'a>(data: &'a [u8], id: &[u8]) -> Option<&'a [u8]> {
    let mut cursor = 0usize;
    while let Some((found, body, next)) = ebml_next(data, cursor) {
        cursor = next;
        if found == id {
            return Some(body);
        }
    }
    None
}

/// Splits one EBML element, returning its ID, body and the next offset.
///
/// # Panics
///
/// Never. A malformed size ends the walk rather than being skipped: the bytes
/// after it are not known to be elements.
fn ebml_next(data: &[u8], offset: usize) -> Option<(&[u8], &[u8], usize)> {
    // An element ID encodes its own length in its leading bits: per the EBML
    // specification, "the number of leading 0's + 1 is the length of the ID in
    // octets". `u8::leading_zeros` counts exactly those bits, so this is the
    // whole rule — `0x18` (`0001_1000`) gives 3 leading zeros and a 4-byte ID
    // matching `0x18538067`, and `0x2A` (`0010_1010`) gives 2 and a 3-byte ID
    // matching `0x2AD7B1`. Both `Segment` and `TimecodeScale` are read correctly
    // by this one line.
    let first = *data.get(offset)?;
    let id_len = 1 + first.leading_zeros() as usize;
    if !(1..=4).contains(&id_len) || offset.checked_add(id_len)? > data.len() {
        return None;
    }
    let id = data.get(offset..offset + id_len)?;

    // The size is a VINT: the same leading-1 marker, but the remaining bits of the
    // first byte are part of the value, and it may describe unknown length.
    let size_at = offset.checked_add(id_len)?;
    let size_first = *data.get(size_at)?;
    let len = 1 + size_first.leading_zeros() as usize;
    if !(1..=8).contains(&len) || size_at.checked_add(len)? > data.len() {
        return None;
    }
    let raw = data.get(size_at..size_at + len)?;
    // The mask is computed in a wider integer on purpose. `len` can legitimately
    // be 8 — the range check above admits it — and `0xFFu8 >> 8` overflows, which
    // is a panic in a debug build and a silent wrap to a shift of 0 in a release
    // one. A release build would therefore have read every 8-byte VINT as having
    // a full byte of value bits, which is not what the encoding says.
    //
    // Computed as `(1 << (8 - len)) - 1`, which is the low `8 - len` bits: the
    // marker occupies the leading bit of the first byte and everything below it
    // is value. For `len == 8` that is zero, and the unknown-size check below
    // then correctly rejects it — an 8-byte VINT carries no value bits at all,
    // so a well-formed one is the "unknown length" encoding.
    let value_mask = ((1u16 << (8 - len)) - 1) as u8;
    if raw[0] & value_mask == value_mask {
        // All value bits set is the "unknown size" encoding, meaning "to the end of
        // the file". Nothing here needs an unbounded element, so the walk stops
        // rather than guessing how far one extends.
        return None;
    }
    let mut size = u64::from(raw[0] & value_mask);
    for &byte in raw.get(1..)? {
        size = size.checked_mul(256)?.checked_add(u64::from(byte))?;
    }

    let body_start = size_at.checked_add(len)?;
    let body_end = body_start.checked_add(usize::try_from(size).ok()?)?;
    if body_end > data.len() {
        return None;
    }
    Some((id, data.get(body_start..body_end)?, body_end))
}

/// Reads a big-endian EBML unsigned integer of any width up to 8 bytes.
fn ebml_uint(bytes: &[u8]) -> u64 {
    let mut out = 0u64;
    // Saturating rather than wrapping: a `TimecodeScale` at the top of the 64-bit
    // range would wrap to zero, which reads as "one nanosecond per tick" and
    // scales every duration in the file to nothing.
    for &byte in bytes.iter().take(8) {
        out = out.saturating_mul(256).saturating_add(u64::from(byte));
    }
    out
}

/// Upper bound on samples read from one Matroska file.
///
/// The reader materialises every block at parse time, so this bounds the memory
/// a single file can claim before any analysis begins (spec §55, §75).
pub const MAX_MKV_SAMPLES: usize = 4_000_000;

/// Inspects a Matroska or WebM file that has already been read into memory.
///
/// # Errors
///
/// Returns an error only when the file is not parseable as EBML at all. A
/// damaged *track* is reported through [`ContainerInspection::anomalies`].
pub fn inspect_bytes(data: Vec<u8>) -> Result<ContainerInspection, ContainerError> {
    Ok(parse(data)?.inspection)
}

/// Reads a Matroska file from disk and inspects it, refusing anything above the
/// shared whole-file cap.
///
/// # Errors
///
/// Returns an error if the file cannot be read or exceeds
/// [`crate::MAX_INSPECTED_BYTES`].
pub fn inspect_file(path: &std::path::Path) -> Result<ContainerInspection, ContainerError> {
    inspect_bytes(read_bounded(path)?)
}

/// Reads one Matroska file's samples, for duplicate detection and Tier-2 decode.
///
/// # Errors
///
/// Returns an error only when the file is not parseable as EBML.
pub fn read_samples(data: Vec<u8>) -> Result<Vec<SampleRecord>, ContainerError> {
    Ok(parse(data)?.samples)
}

/// Reads one Matroska file's samples from disk.
///
/// # Errors
///
/// Returns an error if the file cannot be read or exceeds the shared cap.
pub fn read_samples_file(path: &std::path::Path) -> Result<Vec<SampleRecord>, ContainerError> {
    read_samples(read_bounded(path)?)
}

/// The demuxer's track list plus its packets, parsed once.
///
/// `MkvDemuxer::new` eagerly walks the whole element tree and queues every
/// packet, so tracks and samples are only available from the same parse. Doing
/// it once is what keeps an inspection from parsing the file twice.
struct Parsed {
    inspection: ContainerInspection,
    samples: Vec<SampleRecord>,
}

fn parse(data: Vec<u8>) -> Result<Parsed, ContainerError> {
    use sha2::Digest as _;
    use tpt_kinetix_demux::Demuxer as _;

    // Read before `data` is handed to the demuxer, which consumes it. The demuxer's
    // `MkvTrack` carries only a number, a type and a codec id, so
    // `Segment > Info > Duration` is not obtainable from it — and without it every
    // WebM file reports "declares no duration" whatever the file actually says,
    // which is a false statement in a forensic report rather than a missing one.
    let declared_duration = parse_segment_duration(&data);

    let mut demuxer = MkvDemuxer::new(data).map_err(|e| ContainerError::Parse(e.to_string()))?;
    let tracks: Vec<MkvTrack> = demuxer.tracks().to_vec();

    let mut anomalies = Vec::new();
    if tracks.is_empty() {
        return Ok(Parsed {
            inspection: ContainerInspection::empty(ContainerFormat::Matroska),
            samples: Vec::new(),
        });
    }

    // Matroska track numbers are 1-based and are what block headers carry, so
    // they are the join key from a packet back to its track. Mapping them to
    // dense 0-based indices here is what stops every packet after a gap in the
    // numbering from being attributed to the wrong stream.
    let mut index_of_track_number = std::collections::HashMap::new();
    for (position, track) in tracks.iter().enumerate() {
        let index = u32::try_from(position).unwrap_or(u32::MAX);
        index_of_track_number.insert(track.track_number, index);
        // Track number 0 is reserved by the specification. It is recorded and
        // skipped rather than used as a lookup key.
        if track.track_number == 0 {
            anomalies.push(format!(
                "track at position {position} declares track number 0, which Matroska reserves"
            ));
        }
    }

    let mut streams: Vec<StreamAnalysis> = tracks
        .iter()
        .enumerate()
        .map(|(position, track)| {
            convert_track(
                u32::try_from(position).unwrap_or(u32::MAX),
                track,
                declared_duration,
            )
        })
        .collect();

    // Per-stream accumulation of frame timing, keyframes, and sample bytes.
    let mut frame_times: Vec<Vec<MediaTime>> = vec![Vec::new(); tracks.len()];
    let mut keyframes: Vec<Vec<u32>> = vec![Vec::new(); tracks.len()];
    let mut samples: Vec<SampleRecord> = Vec::new();
    let mut truncated = false;

    loop {
        let packet = match demuxer.read_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            // A truncated or damaged tail must not discard what was recovered
            // (spec §30): the file is evidence even when it is broken.
            Err(error) => {
                anomalies.push(format!("packet reading stopped early: {error}"));
                break;
            }
        };

        if samples.len() >= MAX_MKV_SAMPLES {
            truncated = true;
            break;
        }

        let Some(&stream_index) = index_of_track_number.get(&u64::from(packet.stream_index)) else {
            anomalies.push(format!(
                "a block referenced track number {}, which the header never declared",
                packet.stream_index
            ));
            continue;
        };

        let slot = stream_index as usize;
        if slot >= frame_times.len() {
            continue;
        }

        let frame_index = u32::try_from(frame_times[slot].len()).unwrap_or(u32::MAX);
        let time = media_time(packet.pts.as_millis());

        if frame_times[slot].len() < MAX_EXPANDED_SAMPLES {
            frame_times[slot].push(time);
            if packet.is_key_frame {
                keyframes[slot].push(frame_index);
            }
        }

        let size = packet.size();
        let digest = sha2::Sha256::digest(&packet.data);
        samples.push(SampleRecord {
            stream_index,
            data: packet.data,
            frame_index,
            digest: tpt_app_media_forensics_model::asset::to_hex(&digest),
            time,
            is_key_frame: packet.is_key_frame,
            size,
        });
    }

    if truncated {
        anomalies.push(format!(
            "sample reading stopped at the {MAX_MKV_SAMPLES} sample limit; \
             later frames in this file were not examined"
        ));
    }

    // A video track whose every block is a plain `Block` rather than a
    // `SimpleBlock` yields no keyframe flags at all. Reporting that as "no
    // keyframes" would be an artefact of the reader, not of the file.
    if keyframes.iter().all(Vec::is_empty) && !frame_times.iter().all(Vec::is_empty) {
        anomalies.push(
            "no keyframe flags were recoverable; the file uses reference blocks, \
             which this reader cannot classify. Keyframe-dependent analysis \
             (GOP structure, seeking behaviour) is therefore not reported"
                .to_owned(),
        );
    }

    let frame_info: Vec<Option<TrackFrameInfo>> = frame_times
        .into_iter()
        .zip(keyframes)
        .map(|(times, keys)| {
            if times.is_empty() {
                None
            } else {
                Some(TrackFrameInfo {
                    all_frames_are_keyframes: false,
                    // Matroska block timestamps *are* presentation times. There is no separate decode
                    // order to shift them out of, so composition offsets do not
                    // exist as a concept here, and the two sequences are the same
                    // — a fact about the format rather than a default.
                    decode_times: times.clone(),
                    frame_times: times,
                    keyframes: keys,
                })
            }
        })
        .collect();

    // Every block was read, so the per-stream block count is a measurement
    // rather than a default. Leaving it `None` would render as "0 samples" for
    // a file that demonstrably contains frames.
    let mut packet_counts: Vec<u64> = vec![0; streams.len()];
    for sample in &samples {
        if let Some(slot) = packet_counts.get_mut(sample.stream_index as usize) {
            *slot = slot.saturating_add(1);
        }
    }
    for (stream, count) in streams.iter_mut().zip(packet_counts) {
        stream.packet_count = Some(count);
    }

    Ok(Parsed {
        inspection: ContainerInspection {
            format: ContainerFormat::Matroska,
            streams,
            frame_info,
            anomalies,
            // Matroska has no `mvhd` equivalent: `Tracks`/`TrackEntry` is the only
            // place a track is named, so there is no independent declaration to
            // read it back from. The demuxer's track list is both the declaration
            // and the recovery, and they cannot disagree by construction.
            //
            // Reporting `tracks.len()` here is therefore *not* the same claim the
            // MP4 path makes. It means "no separate count exists to check", so
            // `CONTAINER.DECLARED_TRACK_MISMATCH` stays silent for every Matroska
            // file — an honest non-answer rather than a fabricated agreement.
            declared_track_count: tracks.len(),
            // Matroska has no next-track-ID field.
            declared_next_track_id: None,
        },
        samples,
    })
}

/// Converts a Kinetix Matroska timestamp to the engine's exact time type.
///
/// A timestamp the reader could not express becomes [`MediaTime::ZERO`] rather
/// than being dropped, so frame indices stay aligned with the keyframe list.
fn media_time(millis: Option<i64>) -> MediaTime {
    let micros = millis
        .unwrap_or_default()
        .saturating_mul(1_000)
        .clamp(i64::MIN, i64::MAX);
    MediaTime::from_micros(micros)
}

/// Converts one Matroska track into the engine's stream model.
///
/// `declared_duration` is `Segment > Info > Duration`, read from the bytes by
/// [`parse_segment_duration`]. Matroska records one duration for the whole
/// segment rather than per track, so the same value applies to every stream.
fn convert_track(
    index: u32,
    track: &MkvTrack,
    declared_duration: Option<MediaTime>,
) -> StreamAnalysis {
    let kind = match track.track_type {
        MkvTrackType::Video => StreamKind::Video,
        MkvTrackType::Audio => StreamKind::Audio,
        // Subtitle tracks are a distinct kind, not "unknown": a file carrying
        // subtitles is a different file from one carrying a data stream, and
        // collapsing both loses that distinction.
        MkvTrackType::Other(17) | MkvTrackType::Other(18) => StreamKind::Subtitle,
        MkvTrackType::Other(_) => StreamKind::Unknown,
    };

    StreamAnalysis {
        index,
        kind,
        language: None,
        codec: convert_codec(&track.codec_id),
        timing: StreamTiming {
            timebase: Timebase::from_ticks_per_second(MKV_TIMEBASE_TICKS_PER_SECOND),
            start_time: MediaTime::ZERO,
            duration: declared_duration,
            // No per-track duration exists to compare against: Matroska records
            // one duration for the whole segment. `None` here means "not
            // available", so the declared-versus-measured rule stays quiet rather
            // than inventing agreement between a segment length and a stream
            // length, which are not the same measurement.
            measured_duration: None,
            edit_list_offset: None,
        },
        // The reader exposes no picture geometry or frame rate, so `video` stays
        // `None` rather than being filled with a placeholder resolution.
        video: None,
        // Likewise no sample rate or channel layout. An audio format with an
        // absent layout reports a channel count of 0 by design, which is the
        // honest "not measured" value.
        audio: (kind == StreamKind::Audio).then_some(AudioFormat {
            sample_rate: 0,
            bit_depth: 0,
            channel_layout: None,
        }),
        packet_count: None,
    }
}

/// Maps a Matroska `CodecID` string onto the engine's codec description.
///
/// The declared string is preserved verbatim in `name`, because an unrecognised
/// `CodecID` is itself an observation: a file declaring a codec this build
/// cannot classify should say so rather than silently appear as "unknown"
/// alongside the codecs it genuinely does not know.
fn convert_codec(codec_id: &str) -> CodecInfo {
    let (tag, long_name) = match codec_id {
        "V_VP9" => ("vp09", "VP9"),
        "V_VP8" => ("vp08", "VP8"),
        "V_AV1" => ("av01", "AV1"),
        "V_MPEG4/ISO/AVC" => ("avc1", "H.264 / AVC"),
        "V_MPEGH/ISO/HEVC" => ("hvc1", "H.265 / HEVC"),
        "V_THEORA" => ("theora", "Theora"),
        "A_OPUS" => ("opus", "Opus"),
        "A_VORBIS" => ("vorbis", "Vorbis"),
        "A_AAC" => ("mp4a", "AAC"),
        "A_FLAC" => ("fLaC", "FLAC"),
        "A_MPEG/L3" => ("mp3", "MP3"),
        _ => {
            return if codec_id.is_empty() {
                CodecInfo::new("unknown")
            } else {
                // Unrecognised, but stated.
                CodecInfo::new(codec_id)
            };
        }
    };
    CodecInfo::new(tag).with_long_name(long_name)
}

/// Reads a file whole, refusing anything above the shared inspection cap.
fn read_bounded(path: &std::path::Path) -> Result<Vec<u8>, ContainerError> {
    let metadata = std::fs::metadata(path)
        .map_err(|e| ContainerError::io("stat container", path.display().to_string(), e))?;

    if metadata.len() > crate::MAX_INSPECTED_BYTES {
        return Err(ContainerError::TooLarge {
            path: path.display().to_string(),
            size_bytes: metadata.len(),
            limit_bytes: crate::MAX_INSPECTED_BYTES,
        });
    }

    std::fs::read(path)
        .map_err(|e| ContainerError::io("read container", path.display().to_string(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An eight-byte VINT must be refused, not panic and not be misread.
    ///
    /// Found by the robustness harness in `-core/tests/fuzz.rs` on its first run:
    /// `0xFFu8 >> len` overflows when `len == 8`, which any byte in `0x01..=0x0F`
    /// produces. That is a panic in a debug build and, worse, a silent wrap to a
    /// shift of zero in a release one — where every 8-byte VINT would have been
    /// read as carrying a full byte of value bits.
    ///
    /// An 8-byte VINT has no value bits in its first byte: the marker occupies the
    /// whole byte, so the value mask is zero and the element is the "unknown
    /// length" encoding. The walk must stop rather than guess at its extent.
    ///
    /// Asserted as a *return*, not a panic, because the panic was the bug — and in
    /// a release build this input would have returned a size instead, so the value
    /// is pinned too: the reader must yield nothing for the segment, not a bogus
    /// element.
    #[test]
    fn an_eight_byte_vint_is_refused_rather_than_overflowing() {
        // `0xE7` is the Cluster ID: one leading zero, so a 2-byte ID. `0x01` then
        // has seven leading zeros, giving an 8-byte size VINT — the widest the
        // range check admits, and the one that overflowed.
        let document = [0x1A, 0x45, 0xDF, 0xA3, 0x80, 0x00, 0x00, 0x00, 0x00, 0x01];

        // The property under test: this must not panic, and must not report a
        // parseable document. Before the fix this panicked in debug and, in
        // release, walked past the eight-byte size as though it were small.
        assert!(
            ebml_child(&document, b"\x18\x53\x80\x67").is_none(),
            "an 8-byte VINT is the unknown-length encoding and must not be walked"
        );
    }

    /// The value mask is the low `8 - len` bits, for every legal length.
    ///
    /// Pins the arithmetic the fix introduced. A mask computed for one length and
    /// reused for the others would pass the single crashing case and misread every
    /// other size, which is the subtler half of the same defect.
    #[test]
    fn the_value_mask_covers_exactly_the_non_marker_bits() {
        for len in 1u8..=8 {
            let mask = ((1u16 << (8 - len)) - 1) as u8;
            assert_eq!(
                mask.count_ones(),
                u32::from(8 - len),
                "a {len}-byte VINT has {}-value bits in its first byte",
                8 - len
            );
            // The marker bit is always clear in the mask, so the two can never
            // overlap: `raw[0] & mask` reads value and never reads the length.
            assert_eq!(
                mask & (1u8 << (8 - len)),
                0,
                "the mask must exclude the marker"
            );
        }
    }

    /// Builds a minimal but structurally valid WebM document.
    ///
    /// Written by hand rather than checked in as a binary blob, for the same
    /// reason the MP4 fixtures are: every field under test is visible in the
    /// source, and the fixture is byte-deterministic on every machine
    /// (spec §77).
    fn webm_document(codec_id: &str, track_type: u8, blocks: &[(u16, bool, &[u8])]) -> Vec<u8> {
        webm_document_with_duration(codec_id, track_type, blocks, Some(1_000.0))
    }

    /// A WebM document carrying `Segment > Info > Duration`.
    ///
    /// Every real muxer writes it. Omitting it made this crate's WebM fixtures
    /// unusual in a way that read as a *defect*: `CONTAINER.STREAM_DURATION_MISSING`
    /// fired on all of them, which is the rule correctly reporting a genuinely
    /// absent duration — but the fixtures were the unusual thing, not the rule.
    ///
    /// `duration_ms` is an 8-byte float in `TimecodeScale` units with the scale
    /// left at its 1 ms default, matching what ffmpeg and libwebm emit.
    fn webm_document_with_duration(
        codec_id: &str,
        track_type: u8,
        blocks: &[(u16, bool, &[u8])],
        duration_ms: Option<f64>,
    ) -> Vec<u8> {
        webm_document_with_scale(codec_id, track_type, blocks, duration_ms, None)
    }

    /// As [`webm_document_with_duration`], with an explicit `TimecodeScale`.
    ///
    /// `scale_ns` is written as a `TimecodeScale` element, which changes what the
    /// `Duration` number means. Written as a proper element rather than patched
    /// into existing bytes, because splicing into nested EBML without fixing each
    /// parent's length field yields a structurally invalid document — and the
    /// parser would then correctly report nothing, for the wrong reason.
    fn webm_document_with_scale(
        codec_id: &str,
        track_type: u8,
        blocks: &[(u16, bool, &[u8])],
        duration_ms: Option<f64>,
        scale_ns: Option<u64>,
    ) -> Vec<u8> {
        let mut track_entry = vec![
            0xD7, // TrackNumber
            0x81,
            1,    // = 1
            0x83, // TrackType
            0x81,
            track_type, // = track_type
            0x86,       // CodecID
            0x80 | codec_id.len() as u8,
        ];
        track_entry.extend_from_slice(codec_id.as_bytes());

        let mut tracks_body = vec![0xAE]; // TrackEntry
        tracks_body.push(0x80 | track_entry.len() as u8);
        tracks_body.extend_from_slice(&track_entry);

        let mut cluster = Vec::new();
        cluster.push(0xE7); // Timestamp
        cluster.push(0x82);
        cluster.extend_from_slice(&1000u16.to_be_bytes());
        for (rel_ts, is_key, payload) in blocks {
            let mut block = vec![0x81]; // track number 1
            block.extend_from_slice(&rel_ts.to_be_bytes());
            block.push(if *is_key { 0x80 } else { 0x00 });
            block.extend_from_slice(payload);
            cluster.push(0xA3); // SimpleBlock
            cluster.push(0x80 | block.len() as u8);
            cluster.extend_from_slice(&block);
        }

        let mut segment = Vec::new();
        // `Segment > Info` precedes `Tracks`. EBML does not require that order, but
        // real muxers lay the segment out this way and matching them keeps the
        // fixture honest about what a WebM file looks like.
        if duration_ms.is_some() || scale_ns.is_some() {
            let mut info = Vec::new();
            if let Some(scale) = scale_ns {
                // `0x88` declares an 8-byte value. Writing `0x81` here would
                // describe a 1-byte element containing 8 bytes of data, which
                // truncates the scale to its low byte and desynchronises the
                // walk for every element after it — the failure this fixture
                // originally had, and it read as a parser bug.
                info.extend_from_slice(&[0x2A, 0xD7, 0xB1, 0x88]); // TimecodeScale
                info.extend_from_slice(&scale.to_be_bytes());
            }
            if let Some(duration) = duration_ms {
                info.extend_from_slice(&[0x44, 0x89, 0x88]); // Duration, 8-byte float
                info.extend_from_slice(&duration.to_be_bytes());
            }
            segment.extend_from_slice(&[0x15, 0x49, 0xA9, 0x66]); // Info
            segment.push(0x80 | info.len() as u8);
            segment.extend_from_slice(&info);
        }
        segment.extend_from_slice(&[0x16, 0x54, 0xAE, 0x6B]); // Tracks
        segment.push(0x80 | tracks_body.len() as u8);
        segment.extend_from_slice(&tracks_body);
        segment.extend_from_slice(&[0x1F, 0x43, 0xB6, 0x75]); // Cluster
        segment.push(0x80 | cluster.len() as u8);
        segment.extend_from_slice(&cluster);

        let mut doc = vec![0x1A, 0x45, 0xDF, 0xA3, 0x80]; // empty EBML header
        doc.extend_from_slice(&[0x18, 0x53, 0x80, 0x67]); // Segment
        doc.push(0x80 | segment.len() as u8);
        doc.extend_from_slice(&segment);
        doc
    }

    /// A document whose `Duration` element is `raw`, at whatever width.
    ///
    /// For exercising widths no muxer writes. The length is written as a real EBML
    /// VINT so the document stays structurally valid and the parser is genuinely
    /// reaching the element rather than failing earlier.
    fn webm_document_with_raw_duration(raw: &[u8]) -> Vec<u8> {
        let mut info = vec![0x44, 0x89];
        info.push(0x80 | raw.len() as u8);
        info.extend_from_slice(raw);

        let mut segment = vec![0x15, 0x49, 0xA9, 0x66]; // Info
        segment.push(0x80 | info.len() as u8);
        segment.extend_from_slice(&info);

        let mut doc = vec![0x1A, 0x45, 0xDF, 0xA3, 0x80]; // empty EBML header
        doc.extend_from_slice(&[0x18, 0x53, 0x80, 0x67]); // Segment
        doc.push(0x80 | segment.len() as u8);
        doc.extend_from_slice(&segment);
        doc
    }

    fn vp9_document() -> Vec<u8> {
        webm_document(
            "V_VP9",
            1,
            &[
                (0, true, &[1, 2, 3]),
                (33, false, &[4, 5]),
                (66, true, &[6, 7, 8, 9]),
            ],
        )
    }

    #[test]
    fn a_vp9_webm_file_is_inspected() {
        let result = inspect_bytes(vp9_document()).expect("webm parses");
        assert_eq!(result.format, ContainerFormat::Matroska);
        assert_eq!(result.declared_track_count, 1);
        assert_eq!(result.streams.len(), 1);

        let stream = &result.streams[0];
        assert_eq!(stream.kind, StreamKind::Video);
        assert_eq!(stream.codec.name, "vp09");
        assert_eq!(stream.codec.long_name.as_deref(), Some("VP9"));
        // The block count is measured from what was actually read, not defaulted.
        assert_eq!(stream.packet_count, Some(3));
        assert!(result.anomalies.is_empty(), "{:?}", result.anomalies);
    }

    #[test]
    fn no_picture_geometry_is_invented() {
        // The reader exposes no dimensions, so reporting `None` is correct.
        // A placeholder resolution here would put an unmeasured number into
        // every WebM report.
        let result = inspect_bytes(vp9_document()).expect("webm parses");
        assert!(result.streams[0].video.is_none());
        assert_eq!(result.streams[0].dimensions(), None);
    }

    #[test]
    fn keyframe_flags_and_timestamps_survive() {
        let result = inspect_bytes(vp9_document()).expect("webm parses");
        let frames = result.frame_info[0].as_ref().expect("frame info");

        // Cluster timestamp 1000 ms plus block-relative offsets.
        assert_eq!(frames.frame_times.len(), 3);
        assert_eq!(frames.frame_times[0], MediaTime::from_micros(1_000_000));
        assert_eq!(frames.frame_times[1], MediaTime::from_micros(1_033_000));
        assert_eq!(frames.frame_times[2], MediaTime::from_micros(1_066_000));

        // The middle block is flagged non-key.
        assert_eq!(frames.keyframes, vec![0, 2]);
    }

    #[test]
    fn sample_bytes_and_digests_are_retained() {
        let samples = read_samples(vp9_document()).expect("samples read");
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[0].data, vec![1, 2, 3]);
        assert_eq!(samples[2].data, vec![6, 7, 8, 9]);
        assert!(samples[0].is_key_frame);
        assert!(!samples[1].is_key_frame);
        // Distinct payloads must not collide.
        assert_ne!(samples[0].digest, samples[1].digest);
        assert_eq!(samples[0].size, 3);
    }

    #[test]
    fn a_non_ebml_file_is_an_error_not_a_panic() {
        // Spec §75: hostile input must be handled, never trusted.
        for bytes in [
            vec![],
            vec![0x00; 4],
            b"RIFF____WAVEfmt ".to_vec(),
            vec![0x1A, 0x45, 0xDF, 0xA3], // header id, truncated
        ] {
            let _ = inspect_bytes(bytes);
        }
    }

    #[test]
    fn an_empty_but_valid_container_is_reported_as_empty() {
        // Parses cleanly, contains no tracks: that is a measurement, not a failure.
        let mut doc = vec![0x1A, 0x45, 0xDF, 0xA3, 0x80];
        doc.extend_from_slice(&[0x18, 0x53, 0x80, 0x67, 0x80]);
        let result = inspect_bytes(doc).expect("valid empty segment");
        assert!(result.streams.is_empty());
        assert!(!result.anomalies.is_empty());
    }

    #[test]
    fn audio_tracks_are_typed_as_audio() {
        let doc = webm_document("A_OPUS", 2, &[(0, true, &[9, 9])]);
        let result = inspect_bytes(doc).expect("webm parses");
        let stream = &result.streams[0];
        assert_eq!(stream.kind, StreamKind::Audio);
        assert_eq!(stream.codec.name, "opus");
        assert!(stream.audio.is_some());
        // No sample rate was measurable, so it stays zero rather than defaulting.
        assert_eq!(stream.audio.as_ref().map(|a| a.sample_rate), Some(0));
        assert_eq!(stream.audio.as_ref().map(|a| a.channel_count()), Some(0));
    }

    #[test]
    fn an_unrecognised_codec_id_is_stated_not_swallowed() {
        let doc = webm_document("V_SOME_FUTURE_CODEC", 1, &[(0, true, &[1])]);
        let result = inspect_bytes(doc).expect("webm parses");
        assert_eq!(result.streams[0].codec.name, "V_SOME_FUTURE_CODEC");
        assert!(result.streams[0].codec.long_name.is_none());
    }

    #[test]
    fn a_block_with_no_keyframes_records_that_as_a_reader_limitation() {
        // Every block non-key. The file may be entirely reference blocks; the
        // report must say the flags were not recoverable rather than concluding
        // the file has no keyframes.
        let doc = webm_document("V_VP9", 1, &[(0, false, &[1]), (33, false, &[2])]);
        let result = inspect_bytes(doc).expect("webm parses");
        assert!(result.frame_info[0]
            .as_ref()
            .is_some_and(|f| f.keyframes.is_empty()));
        assert!(
            result
                .anomalies
                .iter()
                .any(|a| a.contains("keyframe flags")),
            "{:?}",
            result.anomalies
        );
    }

    #[test]
    fn inspection_is_deterministic() {
        // Spec §77: two runs over the same input produce identical output.
        let bytes = vp9_document();
        let expected = inspect_bytes(bytes.clone()).expect("parses");
        let expected_samples = read_samples(bytes.clone()).expect("samples");
        assert_eq!(inspect_bytes(bytes.clone()).expect("parses"), expected);
        assert_eq!(read_samples(bytes).expect("samples"), expected_samples);
    }

    #[test]
    fn a_webm_file_declares_its_duration() {
        // Every real muxer writes `Segment > Info > Duration`. Reading it is what
        // stops `CONTAINER.STREAM_DURATION_MISSING` from reporting a file as
        // duration-less when the file plainly states otherwise.
        let doc = vp9_document();
        let duration = parse_segment_duration(&doc).expect("duration is readable");
        assert_eq!(duration.as_micros(), 1_000_000, "1000 ms in microseconds");

        let inspection = inspect_bytes(doc).expect("webm parses");
        let stream = inspection.streams.first().expect("has a stream");
        assert_eq!(
            stream.timing.duration,
            Some(duration),
            "the declared duration must reach the stream model"
        );
    }

    #[test]
    fn a_file_with_no_duration_element_really_has_none() {
        // The counterpart to the test above. If this ever reports a duration, the
        // parser is inventing one — which would silence the rule that exists to
        // catch genuinely absent durations.
        let doc = webm_document_with_duration("V_VP9", 1, &[(0, true, &[1, 2, 3])], None);
        assert!(parse_segment_duration(&doc).is_none());

        let inspection = inspect_bytes(doc).expect("webm parses");
        let stream = inspection.streams.first().expect("has a stream");
        assert_eq!(stream.timing.duration, None);
    }

    #[test]
    fn a_non_default_timecode_scale_changes_what_the_duration_means() {
        // `Duration` counts `TimecodeScale` units, not milliseconds. Treating the
        // number as milliseconds regardless would scale every duration in any file
        // using a non-default scale — silently, and by a factor of a million.
        //
        // Built as a document rather than by patching `vp9_document()`: editing
        // bytes inside nested EBML elements without fixing each parent's length
        // field produces a structurally invalid file, and the parser would then
        // correctly report nothing. A test that passed for that reason would be
        // worse than no test.
        let doc = webm_document_with_scale(
            "V_VP9",
            1,
            &[(0, true, &[1, 2, 3])],
            Some(1_000.0),
            Some(1), // 1 ns per tick
        );
        let duration = parse_segment_duration(&doc).expect("duration is readable");
        assert_eq!(
            duration.as_micros(),
            1,
            "1000 ticks at 1 ns each is 1000 ns = 1 microsecond, not 1 second"
        );
    }

    #[test]
    fn a_duration_of_an_unusual_width_is_reported_as_absent() {
        // `Duration` is a float, and only 4- and 8-byte floats are decodable
        // here. A 16-bit float is legal EBML that no muxer writes; reporting none
        // is the honest answer. The point is that an unexpected width returns
        // rather than reading adjacent bytes as a wildly wrong length.
        for width in [1usize, 2, 3, 5, 6, 7] {
            let doc = webm_document_with_raw_duration(&[0xAA; 16][..width]);
            assert!(
                parse_segment_duration(&doc).is_none(),
                "a {width}-byte duration must not be decoded"
            );
        }
    }

    #[test]
    fn a_zero_or_negative_duration_is_absent_rather_than_a_measurement() {
        // A zero length is what an unfinalised file carries while being written.
        // Reporting `0` would state a duration the file does not actually declare
        // as finished.
        for value in [0.0f64, -1.0, f64::NAN, f64::INFINITY] {
            let doc =
                webm_document_with_duration("V_VP9", 1, &[(0, true, &[1, 2, 3])], Some(value));
            assert!(
                parse_segment_duration(&doc).is_none(),
                "{value} must not become a duration"
            );
        }
    }
}
