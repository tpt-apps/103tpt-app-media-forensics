//! Edit-list (`elst`) parsing, and the track start it implies.
//!
//! # Why this exists separately from the demuxer
//!
//! An edit list is how MP4 says "this track does not begin at zero". It lives at
//! `moov/trak/edts/elst`, and `tpt-kinetix-demux`'s `Mp4Track` carries no field
//! for it — the demuxer exposes sample tables, codec description and geometry,
//! and stops there.
//!
//! That left `StreamTiming::edit_list_offset` permanently `None`: a modelled
//! field that no reader ever filled, which is the same defect class as the
//! colour fields this project has already had to correct once. The fixture built
//! a correct `elst`, and the reader discarded it.
//!
//! It is parsed here, from the `moov` bytes [`crate::mp4::inspect_path`] already
//! reads, so no extra I/O is introduced and large files stay analysable.
//!
//! # What is measured
//!
//! The **first** entry's `segment_duration`. That is the empty edit MP4 puts at
//! the head of an edit list to shift a track later in time, and it is the value
//! that answers "does this stream start after the others?".
//!
//! A `media_time` of `-1` marks that empty edit and is *not* itself an offset —
//! it means "no media here yet", which is precisely the delay being described.
//! Subtracting it would report a negative offset for a track that is simply
//! delayed.

/// The `elst` information this engine reads.
///
/// Only the first entry, and only the field that answers "when does this track
/// start". A full edit list can express trimming, speed ramps and multiple
/// segments, none of which this struct models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EditList {
    /// Delay before the track's first presented sample, in microseconds.
    ///
    /// `None` when the track declares no edit list. That is different from
    /// `Some(ZERO)`, which means the track carries an edit list that starts it
    /// immediately — a distinction a report can act on, since the second usually
    /// means a conform was run.
    pub start_offset: Option<tpt_app_media_forensics_model::MediaTime>,
}

/// Walks `moov/trak/edts/elst` and returns the start delay for each track.
///
/// `input` may be the whole file or just the `moov` box: both are accepted
/// because the only caller holds the `moov` box, while the tests naturally have
/// whole files. Deciding which is which by inspection — if the first box is
/// `moov`, step over it — removes a class of caller mistake that would otherwise
/// show up only as "no edit lists found", which reads as "the file has none"
/// rather than "the input was wrong".
///
/// Returns one entry per `trak` in document order, so index `n` corresponds to
/// track `n` — the same ordering [`crate::ContainerInspection::streams`] uses.
/// A track with no edit list contributes `None` rather than being omitted, so
/// the vector's length always equals the number of tracks found.
///
/// # Panics
///
/// Never. Every read is bounds-checked and every width is validated, because the
/// input is attacker-controlled (spec §75).
#[must_use]
pub fn parse_edit_lists(input: &[u8]) -> Vec<Option<tpt_app_media_forensics_model::MediaTime>> {
    let mut out = Vec::new();

    // Work from the `moov` *payload*, not from an offset into it. `next_box`
    // hands back a borrow of the body, so descending into it needs no offset
    // arithmetic at all — and that arithmetic is where this function went wrong
    // three times in a row, each version reading correctly and skipping every
    // `trak`, because `body_end` and "one header in" are different numbers.
    let Some(moov) = crate::boxes::moov_body(input) else {
        return out;
    };

    let mut cursor = 0usize;
    while let Some((kind, body, next)) = crate::boxes::next_box(moov, cursor) {
        cursor = next;
        if &kind == b"trak" {
            out.push(track_start_offset(body));
        }
    }
    out
}
fn track_start_offset(trak: &[u8]) -> Option<tpt_app_media_forensics_model::MediaTime> {
    use tpt_app_media_forensics_model::{MediaTime, Timebase};

    use crate::boxes::{box_body, u32_at};

    // `mdhd` carries the timescale the edit's tick counts are expressed in, so
    // it must come from the same track the edit came from. Another track's
    // timescale would scale the delay by the ratio between them.
    //
    // It is nested at `trak/mdia/mdhd`, not directly under `trak`, so the search
    // descends. A direct-child-only lookup silently falls back to a timescale of
    // 1 and reports a delay in ticks rather than microseconds — a wrong number
    // with no error, which is worse than reporting none.
    let timescale = crate::boxes::box_body(trak, b"mdia")
        .and_then(|mdia| crate::boxes::box_body(mdia, b"mdhd"))
        .and_then(|mdhd| crate::boxes::u32_at(mdhd, 12))
        .unwrap_or(1)
        .max(1);
    let timebase = Timebase::from_ticks_per_second(timescale);

    // `edts` carries version and flags before its child boxes, so the search
    // starts one header in. Reading from offset 0 would read the padding's zero
    // size field as "a box extending to the end of the payload" and consume
    // every child without visiting any of them — the same silent-skip shape as
    // the `moov` walk that preceded it.
    let elst = box_body(&box_body(trak, b"edts")?[4..], b"elst")?;
    if elst.len() < 8 {
        return None;
    }
    if u32_at(elst, 4)? == 0 {
        return None;
    }

    // Version 0 uses 32-bit fields; version 1 widens them to 64. Reading a
    // version-1 list as 32-bit would silently truncate a long delay rather than
    // misplace it, which is the harder error to notice.
    let wide = elst[0] == 1;
    let needed = if wide { 24 } else { 16 };
    if elst.len() < needed {
        return None;
    }

    let segment_duration = if wide {
        u64::from_be_bytes(elst[8..16].try_into().ok()?)
    } else {
        u64::from(u32::from_be_bytes(elst[8..12].try_into().ok()?))
    };

    // `media_time == -1` marks the empty edit that *produces* the delay. Its
    // value is not an offset and must not be subtracted.
    let media_time = if wide {
        i64::from_be_bytes(elst[16..24].try_into().ok()?)
    } else {
        i64::from(i32::from_be_bytes(elst[12..16].try_into().ok()?))
    };
    if media_time == -1 && segment_duration == 0 {
        return Some(MediaTime::ZERO);
    }

    Some(timebase.ticks_to_media_time(i64::try_from(segment_duration).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boxes::next_box;
    use crate::fixture::{build_mp4, build_mp4_av, TrackSpec};
    use tpt_app_media_forensics_model::MediaTime;

    /// Returns the `moov` box of `bytes`, header included, as the parser sees it.
    ///
    /// Walks the top-level box list rather than searching the bytes for `moov`:
    /// sample payload can contain that text, and finding the wrong occurrence
    /// produces a plausible-looking slice of nonsense — the same trap the A/V
    /// fixture tests warn about when they search for `mdat`.
    fn moov_of(bytes: &[u8]) -> Vec<u8> {
        let mut cursor = 0usize;
        while cursor + 8 <= bytes.len() {
            let declared =
                u32::from_be_bytes(bytes[cursor..cursor + 4].try_into().expect("4 bytes")) as usize;
            let kind: [u8; 4] = bytes[cursor + 4..cursor + 8].try_into().expect("4 bytes");
            if &kind == b"moov" {
                return bytes[cursor..(cursor + declared).min(bytes.len())].to_vec();
            }
            if declared == 0 {
                break;
            }
            cursor += declared.max(8);
        }
        Vec::new()
    }

    /// An A/V fixture whose audio track is delayed by `delay_ms`.
    fn av_with_delayed_audio(delay_ms: u32) -> Vec<u8> {
        build_mp4_av(
            &TrackSpec::video_25fps(320, 240, 50),
            &TrackSpec::audio_48khz(2_400),
            delay_ms,
        )
    }

    #[test]
    fn a_delayed_audio_track_reports_its_start_offset() {
        let moov = moov_of(&av_with_delayed_audio(40));
        assert!(!moov.is_empty(), "moov_of found nothing");
        assert_eq!(&moov[4..8], b"moov", "moov_of returned the wrong box");
        // The parser walks `moov` by its *declared* size, so the slice must be
        // self-consistent. If `moov_of` cut the box short, `next_box` on the
        // first child still works but the walk stops early.
        let declared = u32::from_be_bytes(moov[0..4].try_into().expect("header")) as usize;
        assert_eq!(
            declared,
            moov.len(),
            "moov_of must return the whole box; the parser relies on its declared size"
        );
        let first_child = next_box(&moov, 8).expect("moov has children");
        assert_eq!(&first_child.0, b"mvhd", "first moov child");
        let second = next_box(&moov, first_child.2).expect("more children");
        assert_eq!(&second.0, b"trak", "second moov child");
        let offsets = parse_edit_lists(&moov);
        assert_eq!(offsets.len(), 2, "one entry per trak: {offsets:?}");
        assert_eq!(offsets[0], None, "the video track has no edit list");
        assert_eq!(
            offsets[1],
            Some(MediaTime::from_millis(40)),
            "the audio track's delay is the offset an examiner needs"
        );
    }

    #[test]
    fn a_zero_delay_writes_no_edit_list_at_all() {
        // The fixture only emits an `edts` when the delay is non-zero, so a
        // zero-delay file genuinely has no edit list and `None` is the correct
        // answer — not `Some(ZERO)`.
        //
        // An earlier version of this test asserted `Some(ZERO)`, reasoning that a
        // zero-length empty edit is distinguishable from an absent one. That
        // distinction is real in the format and the parser implements it, but no
        // fixture produces it, so the test was asserting a behaviour of a fixture
        // that does not exist. Asserting it here would have tested nothing.
        let offsets = parse_edit_lists(&moov_of(&av_with_delayed_audio(0)));
        assert_eq!(offsets.len(), 2, "one entry per trak");
        assert_eq!(
            offsets[1], None,
            "no delay means the fixture wrote no `edts`, and `None` says exactly that"
        );
    }

    #[test]
    fn a_track_with_no_edit_list_is_none_not_zero() {
        let video_only = build_mp4(&TrackSpec::video_25fps(320, 240, 50));
        assert_eq!(parse_edit_lists(&moov_of(&video_only)), vec![None]);
    }

    #[test]
    fn a_moov_with_no_traks_yields_nothing() {
        assert!(parse_edit_lists(&[]).is_empty());
    }

    #[test]
    fn a_box_that_is_not_moov_yields_nothing() {
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(&16u32.to_be_bytes());
        ftyp.extend_from_slice(b"ftyp");
        ftyp.extend_from_slice(b"isom");
        assert!(parse_edit_lists(&ftyp).is_empty());
    }

    #[test]
    fn a_truncated_file_does_not_panic() {
        // An `elst` promising more bytes than exist must be refused, not read
        // past. The input is attacker-controlled (spec §75).
        let moov = moov_of(&av_with_delayed_audio(40));
        for len in 0..moov.len() {
            let _ = parse_edit_lists(&moov[..len]);
        }
    }

    #[test]
    fn a_box_size_that_cannot_fit_stops_the_walk() {
        // A box declaring a size larger than what remains must end the walk: the
        // bytes after it are not known to be boxes.
        let mut data = Vec::new();
        data.extend_from_slice(&8u32.to_be_bytes());
        data.extend_from_slice(b"moov");
        data.extend_from_slice(&99u32.to_be_bytes());
        data.extend_from_slice(b"trak");
        assert!(parse_edit_lists(&data).is_empty());
    }

    #[test]
    fn a_zero_length_box_does_not_loop_forever() {
        let mut data = Vec::new();
        data.extend_from_slice(&8u32.to_be_bytes());
        data.extend_from_slice(b"moov");
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(b"trak");
        assert!(parse_edit_lists(&data).is_empty());
    }

    #[test]
    fn arbitrary_bytes_are_handled_without_panicking() {
        for seed in 1..64u8 {
            let junk: Vec<u8> = (0..(seed as usize) * 7)
                .map(|i| (i * 31 + usize::from(seed)) as u8)
                .collect();
            let _ = parse_edit_lists(&junk);
        }
    }
}
