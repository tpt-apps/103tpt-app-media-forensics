//! Packet-layer corruption detection (spec §30).
//!
//! # Why this is separate from the box walk
//!
//! [`crate::damage`] answers "is the file's *structure* intact". This module
//! answers a different question: "are the *access units* that structure points at
//! usable". A file can be a textbook-clean ISO-BMFF — every box well-formed,
//! every size consistent, nothing trailing — and still carry packets no decoder
//! will accept. That is the file this module exists for, and it is precisely the
//! one a structural scan reports as perfect.
//!
//! # Why this needs no decoder
//!
//! Every defect here is decidable from the bytes the container hands back,
//! without decoding a single pixel. That is a deliberate constraint, and it is
//! what makes the check useful:
//!
//! - It runs on **every** track, including the patent-encumbered ones. H.264,
//!   HEVC and AAC are never decoded by this engine (see `-video::decode`), so a
//!   decoder-based check would silently skip exactly the files a working
//!   professional is most likely to be holding.
//! - It cannot be wrong for the wrong reason. A decode failure is ambiguous
//!   between damaged media and a decoder limitation; "this access unit contains
//!   no bytes" is not.
//! - It costs nothing. No decoder is constructed, so a check that never looks at
//!   pixels picks up no dependency on pixel-exactness.
//!
//! # No cause is asserted
//!
//! An unreadable packet is reported as unreadable. Whether it arrived that way,
//! was corrupted in transit, or was written by a muxer with a bug is not
//! decidable from the bytes, and spec §15's constraint — an observation is not a
//! cause — binds here exactly as it does across the rule set.

use crate::mp4::SampleRecord;
use tpt_app_media_forensics_model::MediaTime;

/// A defect found in the access units a container's index points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PacketDamage {
    /// An access unit contains no bytes at all.
    ///
    /// Structurally impossible: a sample is the unit a decoder reads, and there
    /// is nothing here to read. Distinct from a *small* sample — a genuinely
    /// low-bitrate frame can be a few dozen bytes, and calling that damage would
    /// report ordinary content as broken. Zero is the one size with no
    /// legitimate reading.
    EmptySample {
        /// Stream the sample belongs to.
        stream_index: u32,
        /// Index of the sample within that stream.
        frame_index: u32,
        /// The sample's own presentation time.
        time: MediaTime,
    },

    /// The container's index declares more samples than could be read.
    ///
    /// The index describes media that is not all recoverable, so every
    /// measurement drawn from this track covers only the part that was read.
    ///
    /// Reported per stream rather than per file, because one damaged track says
    /// nothing about its neighbours and a single total would let a healthy audio
    /// track hide a video track missing half its frames.
    DeclaredSampleCountMismatch {
        /// Stream the count belongs to.
        stream_index: u32,
        /// What the container's index says this stream contains.
        declared: u64,
        /// How many samples were actually recovered.
        recovered: u64,
        /// How many are therefore unaccounted for.
        missing: u64,
    },
}

impl PacketDamage {
    /// Returns the stable tag used in rule IDs and report output.
    #[must_use]
    pub fn tag(&self) -> &'static str {
        match self {
            Self::EmptySample { .. } => "empty_sample",
            Self::DeclaredSampleCountMismatch { .. } => "sample_count_mismatch",
        }
    }

    /// Whether this defect means content the file describes could not be read.
    ///
    /// The packet-layer counterpart to [`crate::StructuralDamage::is_missing_data`],
    /// and what separates the two rules consuming this module: an empty access
    /// unit is an anomaly in one unit, while a count mismatch means an unknown
    /// number of samples were never recovered at all.
    #[must_use]
    pub fn is_missing_data(&self) -> bool {
        matches!(self, Self::DeclaredSampleCountMismatch { .. })
    }

    /// Returns the presentation time of the damaged unit, when it has one.
    ///
    /// [`Self::DeclaredSampleCountMismatch`] describes a whole stream rather than
    /// a moment, so it has no time of its own. Returning `None` there is what
    /// keeps it off the ordered part of the timeline instead of being parked at
    /// `00:00:00`, which would read as a measured position and is not one.
    #[must_use]
    pub fn time(&self) -> Option<MediaTime> {
        match self {
            Self::EmptySample { time, .. } => Some(*time),
            Self::DeclaredSampleCountMismatch { .. } => None,
        }
    }

    /// Renders the damage as a single line for an anomaly list.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::EmptySample {
                stream_index,
                frame_index,
                time,
            } => format!(
                "sample {frame_index} on stream {stream_index} at {} contains no bytes, so it \
                 cannot be decoded",
                time.to_timecode()
            ),
            Self::DeclaredSampleCountMismatch {
                stream_index,
                declared,
                recovered,
                missing,
            } => format!(
                "stream {stream_index} declares {declared} samples but only {recovered} could be \
                 read; {missing} are unaccounted for"
            ),
        }
    }
}
/// Scans recovered access units for defects the container's index does not
/// describe.
///
/// `inspection` supplies the declared sample counts. Returns an empty vector for
/// a clean file: an intact track is the expected case and the common path must
/// not allocate.
///
/// # A count mismatch is not always damage
///
/// [`PacketDamage::DeclaredSampleCountMismatch`] compares what the container's
/// index *declares* against what was actually *recovered*, and the two sides do
/// not mean the same thing in every container:
///
/// - **ISO-BMFF**: `packet_count` is read from `stsz`, the index. Fewer packets
///   recovered than declared means the index describes media that is not all
///   readable — which is damage.
/// - **Matroska**: `packet_count` is *measured* from the packets during the same
///   parse, so the two agree by construction and this defect cannot fire.
///
/// That asymmetry is stated here rather than left to be discovered. The count
/// that *can* disagree is the one read from the index.
///
/// # Panics
///
/// Never. Recovered counts are keyed by stream number in a `BTreeMap` rather
/// than a `Vec` indexed by it, because `stream_index` comes from the file and a
/// hostile sample table can name a stream far past the end of the declared list
/// (spec §75).
#[must_use]
pub fn scan_packets(
    samples: &[SampleRecord],
    inspection: &crate::mp4::ContainerInspection,
) -> Vec<PacketDamage> {
    let mut damage = Vec::new();

    for sample in samples {
        if sample.data.is_empty() || sample.size == 0 {
            damage.push(PacketDamage::EmptySample {
                stream_index: sample.stream_index,
                frame_index: sample.frame_index,
                time: sample.time,
            });
        }
    }

    let mut recovered: std::collections::BTreeMap<u32, u64> = std::collections::BTreeMap::new();
    for sample in samples {
        *recovered.entry(sample.stream_index).or_insert(0) += 1;
    }

    for stream in &inspection.streams {
        // `packet_count` is the index's own claim. `None` means the container
        // never made one, so there is nothing to disagree with — reported as no
        // damage rather than as a mismatch against zero, which would report
        // every track in a format whose reader does not expose the count as
        // missing all of its samples.
        let Some(declared) = stream.packet_count else {
            continue;
        };
        let read = recovered.get(&stream.index).copied().unwrap_or(0);

        if read < declared {
            damage.push(PacketDamage::DeclaredSampleCountMismatch {
                stream_index: stream.index,
                declared,
                recovered: read,
                missing: declared - read,
            });
        }
    }

    damage
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{build_mp4, TrackSpec};

    /// A single hand-built sample, for defects a well-formed fixture cannot
    /// produce. A real `read_samples` stops at the first zero-byte packet by
    /// design, so `EmptySample` has to be constructed to be tested here; the
    /// count-mismatch tests do use real fixtures.
    fn sample(stream: u32, frame: u32, micros: i64, size: usize) -> SampleRecord {
        SampleRecord {
            stream_index: stream,
            data: vec![0xAAu8; size],
            frame_index: frame,
            digest: String::new(),
            time: MediaTime::from_micros(micros),
            is_key_frame: frame == 0,
            size,
        }
    }

    /// Inspects and reads real samples from fixture bytes, as the pipeline does.
    fn inspect_and_read(bytes: Vec<u8>) -> (crate::mp4::ContainerInspection, Vec<SampleRecord>) {
        let inspection = crate::mp4::inspect_bytes(bytes.clone()).expect("a parsable fixture");
        let samples = crate::mp4::read_samples(bytes).expect("readable samples");
        (inspection, samples)
    }

    #[test]
    fn an_intact_track_has_no_packet_damage() {
        let bytes = build_mp4(&TrackSpec::video_25fps(320, 240, 30));
        let (inspection, samples) = inspect_and_read(bytes);
        assert_eq!(samples.len(), 30, "the fixture must carry samples");
        assert!(scan_packets(&samples, &inspection).is_empty());
    }

    #[test]
    fn a_zero_byte_sample_is_reported_with_its_own_time() {
        // The placement is *measured*: the sample carries its own timestamp, so
        // nothing is inferred and nothing needs guessing.
        let (inspection, mut samples) =
            inspect_and_read(build_mp4(&TrackSpec::video_25fps(320, 240, 30)));
        samples[4].data.clear();
        samples[4].size = 0;

        let damage = scan_packets(&samples, &inspection);
        assert_eq!(damage.len(), 1, "{damage:?}");
        let PacketDamage::EmptySample {
            frame_index,
            stream_index,
            time,
        } = &damage[0]
        else {
            panic!("expected an empty sample, got {damage:?}");
        };
        assert_eq!(*frame_index, 4);
        assert_eq!(*stream_index, 0);
        assert_eq!(damage[0].time(), Some(*time));
        assert!(damage[0].describe().contains("no bytes"));
        assert!(
            !damage[0].is_missing_data(),
            "one empty unit is not an unknown number of lost samples"
        );
    }

    #[test]
    fn a_small_sample_is_not_damage() {
        // A low-bitrate frame is legitimately tiny. Only zero is impossible, and
        // treating "small" as corruption would report ordinary content as broken.
        let (inspection, _) = inspect_and_read(build_mp4(&TrackSpec::video_25fps(320, 240, 12)));
        let samples: Vec<SampleRecord> = (0..12)
            .map(|i| sample(0, i, i64::from(i) * 40_000, 12))
            .collect();
        assert!(scan_packets(&samples, &inspection).is_empty());
    }

    #[test]
    fn a_truncated_file_reports_samples_the_index_still_promises() {
        // The case the check exists for: the `stsz` entry count survives in the
        // header, but the `mdat` it points into no longer has the bytes. This is
        // "missing frames" in spec §30's list, and the box walk cannot see it —
        // every box is well-formed and `mdat` is merely short.
        let mut cut = build_mp4(&TrackSpec::video_25fps(320, 240, 40));
        cut.truncate(cut.len() * 2 / 3);
        let inspection = crate::mp4::inspect_bytes(cut.clone()).expect("header survives");
        let samples = crate::mp4::read_samples(cut).expect("partial samples");

        let damage = scan_packets(&samples, &inspection);
        let mismatch = damage
            .iter()
            .find(|d| matches!(d, PacketDamage::DeclaredSampleCountMismatch { .. }))
            .unwrap_or_else(|| panic!("a truncated file must show a count mismatch: {damage:?}"));

        let PacketDamage::DeclaredSampleCountMismatch {
            declared,
            recovered,
            missing,
            ..
        } = mismatch
        else {
            unreachable!()
        };
        assert_eq!(*declared, 40, "the index still declares every sample");
        assert!(*recovered < 40, "the truncated payload yields fewer");
        assert_eq!(*missing, 40 - *recovered);
        assert!(mismatch.is_missing_data());
        assert_eq!(mismatch.time(), None, "a stream-level fact has no moment");
    }

    #[test]
    fn more_samples_than_declared_is_not_a_mismatch() {
        // `recovered > declared` is the opposite condition and is deliberately
        // not reported: whether it is damage depends on which table was read,
        // and this module does not hold that evidence.
        let (inspection, mut samples) =
            inspect_and_read(build_mp4(&TrackSpec::video_25fps(320, 240, 12)));
        samples.extend((12..30u32).map(|i| sample(0, i, i64::from(i) * 40_000, 100)));

        let damage = scan_packets(&samples, &inspection);
        assert!(
            !damage
                .iter()
                .any(|d| matches!(d, PacketDamage::DeclaredSampleCountMismatch { .. })),
            "{damage:?}"
        );
    }

    #[test]
    fn an_empty_sample_reports_even_with_no_streams_declared() {
        // `EmptySample` reads the sample alone, so it must not depend on the
        // declared-stream list lining up with what was recovered.
        let inspection =
            crate::mp4::ContainerInspection::empty(crate::probe::ContainerFormat::IsoBmff);
        let damage = scan_packets(&[sample(0, 0, 0, 0)], &inspection);
        assert_eq!(damage.len(), 1, "{damage:?}");
        assert_eq!(damage[0].tag(), "empty_sample");
    }

    #[test]
    fn a_stream_index_past_the_declared_end_does_not_panic() {
        // `stream_index` comes from the file. A hostile table can name a stream
        // no track has, and counting must not index a vector by it.
        let (inspection, mut samples) =
            inspect_and_read(build_mp4(&TrackSpec::video_25fps(320, 240, 10)));
        samples.push(sample(9_999, 0, 0, 50));

        let damage = scan_packets(&samples, &inspection);
        assert!(
            !damage
                .iter()
                .any(|d| matches!(d, PacketDamage::DeclaredSampleCountMismatch { .. })),
            "the unknown stream simply has no declared claim: {damage:?}"
        );
    }

    #[test]
    fn every_defect_carries_a_tag_and_a_description() {
        let (inspection, mut samples) =
            inspect_and_read(build_mp4(&TrackSpec::video_25fps(320, 240, 12)));
        samples[2].data.clear();
        samples[2].size = 0;

        for damage in scan_packets(&samples[..6], &inspection) {
            assert!(!damage.tag().is_empty());
            assert!(!damage.describe().is_empty());
        }
    }
}
