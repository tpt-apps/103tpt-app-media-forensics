//! Structural damage detection (spec §30).
//!
//! # Why this is separate from the demuxer
//!
//! `tpt-kinetix-demux` answers "what tracks does this file declare". It stops
//! when the bytes stop making sense, and reports success for everything before
//! that point. That is the right behaviour for a player and the wrong behaviour
//! for a forensic examination: the question here is not only *what is in the
//! file* but *where does the file stop being trustworthy*, and a demuxer that
//! returns the tracks it managed to read has, by construction, lost the
//! boundary.
//!
//! So the damage is recovered by walking the top-level box list independently.
//! The walk is deliberately shallow. A damaged file's inner structure is exactly
//! what cannot be trusted, and a recursive descent into it is how a malformed
//! file turns an examination into a crash (spec §75).
//!
//! # Typed rather than free text
//!
//! Each finding is a [`StructuralDamage`] variant carrying the numbers behind
//! it. A string such as `"file is truncated"` cannot be filtered, compared, or
//! graded; an examiner asking "is this file damaged?" needs an answer that
//! distinguishes *the media data is cut short* from *the header is malformed*,
//! because those carry different weight and point at different causes.
//!
//! No cause is asserted. A truncated file is reported as truncated. Whether that
//! came from a failed copy, an incomplete upload, or deliberate editing is a
//! conclusion for a human, and the engine does not have the evidence to reach it.

/// A structural defect found in a container's top-level box list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructuralDamage {
    /// A box declares a size that runs past the end of the file.
    ///
    /// The most consequential defect forensically: the media data the file
    /// describes is not present, so every measurement derived from it is
    /// partial by construction.
    Truncated {
        /// The four-character box type, rendered for display.
        box_type: String,
        /// Byte offset where the box starts.
        offset: u64,
        /// Size the box claims.
        declared_size: u64,
        /// Bytes actually available from `offset` to the end of the file.
        available_bytes: u64,
    },

    /// Bytes remain after the last complete top-level box.
    ///
    /// Not necessarily damage on its own — appended data is a legitimate
    /// technique, and some muxers pad — but it means the file contains bytes
    /// no declared structure accounts for, which is worth recording.
    TrailingData {
        /// Byte offset where the unexplained bytes begin.
        offset: u64,
        /// How many bytes are unaccounted for.
        byte_count: u64,
    },

    /// A box declares a size smaller than the header describing it.
    ///
    /// Structurally impossible: the box cannot contain its own size and type
    /// fields. No interpretation is attempted, because there is none that makes
    /// the bytes consistent.
    ImpossibleBoxSize {
        /// The four-character box type, rendered for display.
        box_type: String,
        /// Byte offset where the box starts.
        offset: u64,
        /// The nonsensical declared size.
        declared_size: u64,
    },

    /// A box type contains bytes that are not printable ASCII.
    ///
    /// Usually a sign of misalignment rather than a genuinely odd box, which is
    /// why it is reported rather than acted on: a reader that has lost sync will
    /// happily interpret sample payload as structure.
    NonPrintableBoxType {
        /// Byte offset where the box starts.
        offset: u64,
        /// The type field, rendered with `\xNN` escapes.
        rendered: String,
    },
}
impl StructuralDamage {
    /// Returns the stable tag used in rule IDs and report output.
    #[must_use]
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Truncated { .. } => "truncated",
            Self::TrailingData { .. } => "trailing_data",
            Self::ImpossibleBoxSize { .. } => "impossible_box_size",
            Self::NonPrintableBoxType { .. } => "non_printable_box_type",
        }
    }

    /// Whether this defect means declared content is missing from the file.
    ///
    /// Distinct from "the file is unusual": only truncation asserts that
    /// something the file describes is not there, which is what makes it
    /// significant enough to change how every other measurement is read.
    #[must_use]
    pub fn is_missing_data(&self) -> bool {
        matches!(self, Self::Truncated { .. })
    }

    /// Returns the byte offset the damage was found at.
    #[must_use]
    pub fn offset(&self) -> u64 {
        match self {
            Self::Truncated { offset, .. }
            | Self::TrailingData { offset, .. }
            | Self::ImpossibleBoxSize { offset, .. }
            | Self::NonPrintableBoxType { offset, .. } => *offset,
        }
    }

    /// Renders the damage as a single line for an anomaly list.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Truncated {
                box_type,
                offset,
                declared_size,
                available_bytes,
            } => format!(
                "box '{box_type}' at offset {offset} declares {declared_size} bytes but only \
                 {available_bytes} remain: the file is truncated"
            ),
            Self::TrailingData { offset, byte_count } => format!(
                "{byte_count} bytes at offset {offset} follow the last box and are not described \
                 by any declared structure"
            ),
            Self::ImpossibleBoxSize {
                box_type,
                offset,
                declared_size,
            } => format!(
                "box '{box_type}' at offset {offset} declares {declared_size} bytes, less than \
                 the 8-byte header describing it"
            ),
            Self::NonPrintableBoxType { offset, rendered } => format!(
                "box type at offset {offset} is '{rendered}', which is not a printable box name; \
                 the structure may be misaligned"
            ),
        }
    }
}

/// Walks the top-level box list and records every structural defect.
///
/// Returns an empty vector for a well-formed file: a clean container is the
/// expected case, and the common path must not allocate.
///
/// # Panics
///
/// Never. Every read is bounds-checked and every offset computation saturates,
/// because the input is attacker-controlled by definition (spec §75).
#[must_use]
pub fn scan_isobmff(data: &[u8]) -> Vec<StructuralDamage> {
    let mut damage = Vec::new();
    let mut offset: u64 = 0;
    let total = data.len() as u64;

    while offset.saturating_add(8) <= total {
        let at = offset as usize;
        let declared = u64::from(u32::from_be_bytes(
            data[at..at + 4].try_into().unwrap_or([0; 4]),
        ));
        let box_type = &data[at + 4..at + 8];

        if !box_type.iter().all(u8::is_ascii_graphic) {
            damage.push(StructuralDamage::NonPrintableBoxType {
                offset,
                rendered: render_type(box_type),
            });
        }

        // Size 0 means "extends to the end of the file" and size 1 means the
        // real size follows as a 64-bit value. Both are legal; treating either
        // as a literal small size would report damage in a valid file.
        let (header_len, body_len) = if declared == 1 {
            // The 64-bit size word follows the type field, so the box occupies
            // 16 bytes of header in total.
            if offset.saturating_add(16) > total {
                damage.push(StructuralDamage::Truncated {
                    box_type: render_type(box_type),
                    offset,
                    declared_size: 0,
                    available_bytes: total.saturating_sub(offset),
                });
                offset = total;
                break;
            }
            let at = offset as usize + 8;
            let extended = u64::from_be_bytes(data[at..at + 8].try_into().unwrap_or([0; 8]));
            (16_u64, extended.saturating_sub(16))
        } else if declared == 0 {
            // Runs to end of file: nothing follows, so nothing can be trailing.
            offset = total;
            continue;
        } else {
            (8_u64, declared.saturating_sub(8))
        };

        let full_size = header_len.saturating_add(body_len);

        // Sizes 0 and 1 are the two special encodings handled above; anything
        // else below 8 cannot contain the header that describes it.
        if declared > 1 && declared < 8 {
            // Reported rather than repaired: no layout reconciles these bytes,
            // and guessing a size would fabricate a file the examiner never
            // received.
            damage.push(StructuralDamage::ImpossibleBoxSize {
                box_type: render_type(box_type),
                offset,
                declared_size: declared,
            });
            // The remainder is unparseable, not merely unaccounted for, so it is
            // not also reported as trailing data.
            offset = total;
            break;
        }

        if offset.saturating_add(full_size) > total {
            damage.push(StructuralDamage::Truncated {
                box_type: render_type(box_type),
                offset,
                declared_size: full_size,
                available_bytes: total.saturating_sub(offset),
            });
            // Consume the remainder: the bytes after this point are the *shortfall*
            // already reported above, and counting them a second time as
            // "unaccounted for" would describe one defect as two.
            offset = total;
            break;
        }

        offset = offset.saturating_add(full_size);
    }

    if offset < total {
        damage.push(StructuralDamage::TrailingData {
            offset,
            byte_count: total.saturating_sub(offset),
        });
    }

    damage
}

/// Renders a four-byte box type for display.
///
/// Non-printable bytes become `\xNN` rather than passing through: a box type
/// containing control characters is itself worth reporting, but emitting it raw
/// produces output that renders as blank space and tells the analyst nothing.
fn render_type(box_type: &[u8]) -> String {
    box_type
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

/// Where a sample's byte offset came from.
///
/// # Why this exists
///
/// A [`StructuralDamage`] carries a byte offset because that is what the box walk
/// can observe. An analyst asking "when does this file go wrong?" wants a
/// timecode, and the two connect only through the sample table.
///
/// # What this deliberately does not claim
///
/// A sample's offset is inferred as *an anchor plus the cumulative sizes of the
/// samples before it*. Sound for a single contiguous `mdat`; wrong for a file
/// whose chunk offsets (`stco`) point at scattered locations. This build does not
/// read `stco`, so a multi-track file can place a defect against the wrong track.
///
/// Rather than present a confident wrong timecode, every position records how it
/// was obtained, so a caller can see exactly what a finding's timeline placement
/// rests on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleOrigin {
    /// The offset was read from the container's chunk offset table.
    ///
    /// Not produced by this build, which does not parse `stco`. Reserved so the
    /// distinction survives if that is implemented later, rather than every
    /// offset silently becoming "measured".
    Declared,

    /// The offset was inferred by accumulating sample sizes from an anchor.
    ///
    /// Sound for a single contiguous `mdat`. Unreliable across interleaved
    /// streams, where the running sum would include another track's bytes.
    Derived {
        /// Byte offset the accumulation started from.
        anchor: u64,
    },
}

impl SampleOrigin {
    /// Whether the offset was read rather than computed.
    #[must_use]
    pub fn is_measured(self) -> bool {
        matches!(self, Self::Declared)
    }
}

/// One sample's position in both bytes and time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SamplePosition {
    /// Byte offset of this sample within the file.
    pub byte_offset: u64,
    /// Presentation time of this sample.
    pub time: tpt_app_media_forensics_model::MediaTime,
    /// Index of the stream this sample belongs to.
    pub stream_index: u32,
    /// How `byte_offset` was obtained.
    pub origin: SampleOrigin,
}

/// Locates media samples in byte order.
///
/// Built from [`crate::SampleRecord`]s, which carry both a presentation time and
/// a compressed size, so each sample's offset follows from the one before it.
///
/// # Panics
///
/// Never. Accumulation saturates, because a hostile sample table can declare
/// sizes whose sum overflows (spec §75).
#[derive(Debug, Clone, Default)]
pub struct SampleIndex {
    positions: Vec<SamplePosition>,
}

impl SampleIndex {
    /// Builds an index from sample records.
    ///
    /// `anchor` is the byte offset of the first sample. The caller supplies it
    /// because only the container layout knows where media data begins —
    /// guessing here would place every defect at an invented offset.
    ///
    /// Offsets advance **per stream**, not globally: a single running counter
    /// would fold one track's bytes into another track's offsets.
    #[must_use]
    pub fn build(records: &[crate::SampleRecord], anchor: u64) -> Self {
        let mut positions: Vec<SamplePosition> = Vec::with_capacity(records.len());
        let mut running: std::collections::BTreeMap<u32, u64> = std::collections::BTreeMap::new();

        for record in records {
            let base = *running.entry(record.stream_index).or_insert(anchor);
            positions.push(SamplePosition {
                byte_offset: base,
                time: record.time,
                stream_index: record.stream_index,
                origin: SampleOrigin::Derived { anchor: base },
            });
            running.insert(record.stream_index, base.saturating_add(record.size as u64));
        }

        // Sorted so a lookup is a binary search rather than a scan, and so
        // `locate` can find the sample containing an offset.
        positions.sort_by(|a, b| a.byte_offset.cmp(&b.byte_offset).then(a.time.cmp(&b.time)));
        Self { positions }
    }

    /// Returns `true` when no samples were indexed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// Number of samples indexed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    /// Finds the last sample starting at or before `offset`.
    ///
    /// Returns `None` when `offset` precedes the first sample, meaning the defect
    /// is in the header rather than the media. That is a real and different
    /// finding, so it is reported as "before the first sample" rather than
    /// attached to sample zero at time zero.
    #[must_use]
    pub fn locate(&self, offset: u64) -> Option<&SamplePosition> {
        match self
            .positions
            .binary_search_by(|position| position.byte_offset.cmp(&offset))
        {
            Ok(index) => self.positions.get(index),
            Err(0) => None,
            Err(index) => index.checked_sub(1).and_then(|i| self.positions.get(i)),
        }
    }

    /// Returns every indexed position, ordered by byte offset.
    #[must_use]
    pub fn positions(&self) -> &[SamplePosition] {
        &self.positions
    }
}

#[cfg(test)]
mod index_tests {
    use super::*;
    use crate::SampleRecord;
    use tpt_app_media_forensics_model::MediaTime;

    fn record(stream: u32, index: u32, micros: i64, size: usize) -> SampleRecord {
        SampleRecord {
            stream_index: stream,
            data: Vec::new(),
            frame_index: index,
            digest: String::new(),
            time: MediaTime::from_micros(micros),
            is_key_frame: index == 0,
            size,
        }
    }

    /// Three 100-byte samples at 40 ms intervals, starting at offset 1000.
    fn three_samples() -> SampleIndex {
        SampleIndex::build(
            &[
                record(0, 0, 0, 100),
                record(0, 1, 40_000, 100),
                record(0, 2, 80_000, 100),
            ],
            1_000,
        )
    }

    #[test]
    fn offsets_accumulate_from_the_anchor() {
        let offsets: Vec<u64> = three_samples()
            .positions()
            .iter()
            .map(|p| p.byte_offset)
            .collect();
        assert_eq!(offsets, vec![1_000, 1_100, 1_200]);
    }

    #[test]
    fn an_offset_inside_a_sample_resolves_to_that_sample() {
        let index = three_samples();
        let found = index.locate(1_150).expect("resolves");
        assert_eq!(found.byte_offset, 1_100);
        assert_eq!(found.time, MediaTime::from_micros(40_000));
    }

    #[test]
    fn an_exact_offset_resolves_to_that_sample() {
        let index = three_samples();
        let found = index.locate(1_200).expect("resolves");
        assert_eq!(found.time, MediaTime::from_micros(80_000));
    }

    #[test]
    fn an_offset_before_the_first_sample_is_not_attached_to_it() {
        // A header defect is a different finding from a media one, and reporting
        // "time 00:00:00" for a header problem would be wrong.
        assert!(three_samples().locate(10).is_none());
    }

    #[test]
    fn an_offset_past_the_end_resolves_to_the_last_sample() {
        let index = three_samples();
        let found = index.locate(99_999).expect("resolves");
        assert_eq!(found.time, MediaTime::from_micros(80_000));
    }

    #[test]
    fn streams_accumulate_independently() {
        // A single global counter would give stream 1 offsets including stream
        // 0's bytes, placing its defects at fabricated positions.
        let index = SampleIndex::build(
            &[
                record(0, 0, 0, 100),
                record(1, 0, 0, 50),
                record(0, 1, 40_000, 100),
                record(1, 1, 20_000, 50),
            ],
            1_000,
        );
        let stream_one: Vec<u64> = index
            .positions()
            .iter()
            .filter(|p| p.stream_index == 1)
            .map(|p| p.byte_offset)
            .collect();
        assert_eq!(stream_one, vec![1_000, 1_050]);
    }

    #[test]
    fn every_inferred_offset_declares_itself_inferred() {
        // A report must be able to distinguish measurement from inference.
        for position in three_samples().positions() {
            assert!(
                !position.origin.is_measured(),
                "this build reads no stco, so no offset is measured"
            );
        }
    }

    #[test]
    fn an_empty_sample_list_produces_an_empty_index() {
        let index = SampleIndex::build(&[], 0);
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
        assert!(index.locate(500).is_none());
    }

    #[test]
    fn absurd_sample_sizes_saturate_rather_than_wrapping() {
        // A hostile stsz could declare sizes whose sum overflows u64.
        let index = SampleIndex::build(
            &[
                record(0, 0, 0, usize::MAX),
                record(0, 1, 40_000, usize::MAX),
            ],
            u64::MAX - 10,
        );
        for position in index.positions() {
            assert!(position.byte_offset > 0);
        }
    }

    #[test]
    fn positions_are_sorted_by_offset() {
        let index = SampleIndex::build(
            &[
                record(0, 0, 0, 100),
                record(1, 0, 0, 50),
                record(0, 1, 40_000, 100),
            ],
            1_000,
        );
        let offsets: Vec<u64> = index.positions().iter().map(|p| p.byte_offset).collect();
        let mut sorted = offsets.clone();
        sorted.sort_unstable();
        assert_eq!(offsets, sorted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wraps `body` in a box header of the given type and declared size.
    fn box_of(box_type: &[u8; 4], declared: u32, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&declared.to_be_bytes());
        out.extend_from_slice(box_type);
        out.extend_from_slice(body);
        out
    }

    /// A minimal but valid two-box file.
    fn well_formed() -> Vec<u8> {
        let mut data = box_of(b"ftyp", 20, b"isom0000isom");
        data.extend_from_slice(&box_of(b"mdat", 48, &[0xAB; 40]));
        data
    }

    #[test]
    fn a_well_formed_file_has_no_damage() {
        assert_eq!(scan_isobmff(&well_formed()), Vec::new());
    }

    #[test]
    fn an_empty_file_has_no_damage() {
        // Nothing is claimed, so nothing is missing. A file too short to hold a
        // box has not misdescribed itself.
        assert_eq!(scan_isobmff(&[]), Vec::new());
    }

    #[test]
    fn truncation_is_reported_with_both_numbers() {
        let mut data = box_of(b"ftyp", 20, b"isom0000isom");
        // Declares 108 bytes of mdat, supplies only 10 after its header.
        data.extend_from_slice(&box_of(b"mdat", 108, &[0xAB; 10]));

        let damage = scan_isobmff(&data);
        assert_eq!(damage.len(), 1, "one defect: {damage:?}");
        let StructuralDamage::Truncated {
            box_type,
            declared_size,
            available_bytes,
            ..
        } = &damage[0]
        else {
            panic!("expected truncation, got {damage:?}");
        };
        assert_eq!(box_type, "mdat");
        assert_eq!(*declared_size, 108);
        assert_eq!(*available_bytes, 18);
        assert!(damage[0].is_missing_data());
    }

    #[test]
    fn trailing_data_is_reported_separately_from_truncation() {
        let mut data = well_formed();
        // Fewer than 8 bytes, so it cannot be mistaken for a box header. See
        // `appended_data_long_enough_to_mimic_a_box_is_read_as_one` for why the
        // length matters here.
        data.extend_from_slice(b"JUNK");

        let damage = scan_isobmff(&data);
        assert_eq!(damage.len(), 1, "{damage:?}");
        let StructuralDamage::TrailingData { byte_count, .. } = &damage[0] else {
            panic!("expected trailing data, got {damage:?}");
        };
        assert_eq!(*byte_count, 4);
        assert!(
            !damage[0].is_missing_data(),
            "appended bytes are unaccounted for, not missing"
        );
    }

    #[test]
    fn appended_data_long_enough_to_mimic_a_box_is_read_as_one() {
        // A documented limitation, pinned so a change to it is deliberate.
        //
        // There is no way to tell an appended payload from a further box by
        // looking at the bytes: a box is exactly a size followed by a type, and
        // appended data frequently has that shape. This walker therefore parses
        // the first 8 bytes as a box header and reports what the bytes say.
        //
        // The alternative — treating leftover bytes as opaque — would hide
        // genuine trailing boxes, which are themselves a forensic signal.
        let mut data = well_formed();
        data.extend_from_slice(b"APPENDED-NOT-IN-ANY-BOX");

        let damage = scan_isobmff(&data);
        assert!(
            !damage.is_empty(),
            "the bytes are read as structure, so something must be reported"
        );
        // 'A' 'P' 'P' 'E' = 0x41505045 as a big-endian size: far past the end.
        assert!(
            matches!(damage[0], StructuralDamage::Truncated { .. }),
            "expected the over-read to surface, got {damage:?}"
        );
    }

    #[test]
    fn a_box_size_smaller_than_its_own_header_is_impossible() {
        let damage = scan_isobmff(&box_of(b"mdat", 4, &[]));
        assert_eq!(damage.len(), 1, "{damage:?}");
        assert!(matches!(
            damage[0],
            StructuralDamage::ImpossibleBoxSize { .. }
        ));
    }

    #[test]
    fn a_size_of_zero_means_extent_to_end_of_file_not_a_defect() {
        // ISO-BMFF defines size 0 as "this box runs to the end of the file".
        // Reading it as a literal size of 0 would report damage in a valid file.
        let mut data = box_of(b"ftyp", 20, b"isom0000isom");
        data.extend_from_slice(&[0, 0, 0, 0]);
        data.extend_from_slice(b"mdat");
        data.extend_from_slice(&[0xCD; 32]);

        assert_eq!(scan_isobmff(&data), Vec::new());
    }

    #[test]
    fn an_extended_size_is_read_not_mistaken_for_the_literal_one() {
        let mut data = box_of(b"ftyp", 20, b"isom0000isom");
        // size == 1 marks a 64-bit size that follows the type field.
        data.extend_from_slice(&[0, 0, 0, 1]);
        data.extend_from_slice(b"mdat");
        data.extend_from_slice(&24_u64.to_be_bytes());
        data.extend_from_slice(&[0xEF; 8]);

        assert_eq!(scan_isobmff(&data), Vec::new());
    }

    #[test]
    fn an_extended_size_running_past_the_end_is_truncation() {
        let mut data = box_of(b"ftyp", 20, b"isom0000isom");
        data.extend_from_slice(&[0, 0, 0, 1]);
        data.extend_from_slice(b"mdat");
        data.extend_from_slice(&9_000_u64.to_be_bytes());

        let damage = scan_isobmff(&data);
        assert!(
            matches!(damage[0], StructuralDamage::Truncated { .. }),
            "{damage:?}"
        );
    }

    #[test]
    fn a_truncated_extended_size_header_does_not_panic() {
        let mut data = box_of(b"ftyp", 20, b"isom0000isom");
        data.extend_from_slice(&[0, 0, 0, 1]);
        data.extend_from_slice(b"mda");
        assert!(!scan_isobmff(&data).is_empty());
    }

    #[test]
    fn a_non_printable_box_type_is_reported_rather_than_trusted() {
        // What a reader sees when it has lost sync and is interpreting sample
        // payload as box structure.
        let damage = scan_isobmff(&box_of(&[0x00, 0x01, 0x02, 0x03], 8, &[]));
        assert_eq!(damage.len(), 1, "{damage:?}");
        assert!(matches!(
            damage[0],
            StructuralDamage::NonPrintableBoxType { .. }
        ));
        assert!(damage[0].describe().contains("\\x00"));
    }

    #[test]
    fn every_short_prefix_of_a_valid_file_is_handled_without_panicking() {
        // Truncation is the input, not an exception. A forensic tool that
        // crashes on a cut-off file has failed at its one job (spec §75).
        let full = well_formed();
        for len in 0..full.len() {
            let _ = scan_isobmff(&full[..len]);
        }
    }

    #[test]
    fn offsets_are_reported_for_every_variant() {
        let mut data = well_formed();
        data.extend_from_slice(b"TRAILING");
        for damage in scan_isobmff(&data) {
            assert!(damage.offset() < data.len() as u64);
            assert!(!damage.describe().is_empty());
            assert!(!damage.tag().is_empty());
        }
    }
}
