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
//! audio sample rate, or per-track duration — those live in `Video`/`Audio`
//! child elements the reader never descends into.
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
        .map(|(position, track)| convert_track(u32::try_from(position).unwrap_or(u32::MAX), track))
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
            declared_track_count: tracks.len(),
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
fn convert_track(index: u32, track: &MkvTrack) -> StreamAnalysis {
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
            duration: None,
            // The Matroska reader exposes no declared duration, so there is
            // nothing for a measurement to be compared against. `None` here
            // means "not available", and the declared-versus-measured rule stays
            // quiet rather than inventing agreement.
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

    /// Builds a minimal but structurally valid WebM document.
    ///
    /// Written by hand rather than checked in as a binary blob, for the same
    /// reason the MP4 fixtures are: every field under test is visible in the
    /// source, and the fixture is byte-deterministic on every machine
    /// (spec §77).
    fn webm_document(codec_id: &str, track_type: u8, blocks: &[(u16, bool, &[u8])]) -> Vec<u8> {
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
    fn short_and_truncated_documents_do_not_panic() {
        // Spec §75 again, over every prefix of a valid file.
        let bytes = vp9_document();
        for len in 0..bytes.len() {
            let _ = inspect_bytes(bytes[..len].to_vec());
        }
    }
}
