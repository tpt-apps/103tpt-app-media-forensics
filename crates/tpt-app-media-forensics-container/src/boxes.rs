//! The box primitives shared by every hand-rolled ISO-BMFF reader in this crate.
//!
//! # Why these live here rather than inside one reader
//!
//! [`crate::elst`] and [`crate::mp4`] both need to walk boxes that
//! `tpt-kinetix-demux` does not expose, and both arrived at the *same* helpers
//! independently. The walkers are small but unforgiving: each has a way of
//! walking a file correctly and reporting nothing at all, which is why the
//! comments below are as long as they are. Duplicating that reasoning across two
//! readers would guarantee one of them drifts.
//!
//! # Safety posture
//!
//! Input is attacker-controlled (spec §75). Every read is bounds-checked, every
//! width is validated before use, and a malformed box ends the walk rather than
//! being skipped — the bytes after a box with an impossible size are not known to
//! be boxes, and treating sample payload as structure is how a reader invents
//! boxes that are not there.
//!
//! # Panics
//!
//! Never. These functions index only behind checked bounds.

/// Returns the payload of the first `moov` box in `input`, wherever it sits.
///
/// Seeking the *body* rather than an offset into it sidesteps the whole class of
/// off-by-a-header errors that trying to walk into a box from outside provokes:
/// there is no arithmetic to get wrong, only a borrow the parser already has.
pub fn moov_body(input: &[u8]) -> Option<&[u8]> {
    let mut cursor = 0usize;
    while let Some((kind, body, next)) = next_box(input, cursor) {
        if &kind == b"moov" {
            return Some(body);
        }
        cursor = next;
    }
    None
}

/// Returns the body of the first box of type `kind`, if `data` contains one.
pub fn box_body<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    let mut cursor = 0usize;
    while let Some((found, body, next)) = next_box(data, cursor) {
        cursor = next;
        if &found == kind {
            return Some(body);
        }
    }
    None
}

/// Reads a big-endian `u16` at `offset`, if the bytes are there.
///
/// Here rather than borrowed from elsewhere because `colr`, `mdcv`, and `clli`
/// are all fields of two bytes, and a reader that hand-rolls a second bounds
/// check per field is a reader that will eventually skip one.
pub fn u16_at(data: &[u8], offset: usize) -> Option<u16> {
    let end = offset.checked_add(2)?;
    if end > data.len() {
        return None;
    }
    Some(u16::from_be_bytes(data[offset..end].try_into().ok()?))
}

/// Reads a big-endian `u32` at `offset`, if the bytes are there.
pub fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    if end > data.len() {
        return None;
    }
    Some(u32::from_be_bytes(data[offset..end].try_into().ok()?))
}

/// Splits one box at `offset`, returning its type, body and the next offset.
///
/// Returns `None` at end of input or on a box whose declared size cannot fit
/// what remains. A malformed box ends the walk rather than being skipped: the
/// bytes after it are not known to be boxes, and treating sample payload as
/// structure is how a reader invents boxes that are not there.
pub fn next_box(data: &[u8], offset: usize) -> Option<([u8; 4], &[u8], usize)> {
    let header_end = offset.checked_add(8)?;
    if header_end > data.len() {
        return None;
    }
    // The size is four bytes and the type is four bytes. Slicing `offset..end`
    // (eight bytes) for the size looks right and silently fails: `try_into`
    // yields `None` for a length mismatch, `.ok()?` turns that into an early
    // return, and the box walk reports "end of input" for every box it meets.
    // This cost several rounds of probing a walk that read correctly throughout.
    let declared = u32::from_be_bytes(data[offset..offset + 4].try_into().ok()?) as usize;
    let kind: [u8; 4] = data[offset + 4..header_end].try_into().ok()?;

    let end = header_end;

    let (body_start, body_len) = match declared {
        0 => (end, data.len() - end),
        1 => {
            let wide_end = end.checked_add(8)?;
            if wide_end > data.len() {
                return None;
            }
            let wide = u64::from_be_bytes(data[end..wide_end].try_into().ok()?) as usize;
            (wide_end, wide.checked_sub(16)?)
        }
        _ => (end, declared.checked_sub(8)?),
    };

    let body_end = body_start.checked_add(body_len)?;
    if body_end > data.len() {
        return None;
    }
    Some((kind, &data[body_start..body_end], body_end))
}

/// What `mvhd` declares about the file as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MovieHeader {
    /// The track ID the writer expects the *next* track to use.
    ///
    /// Per ISO/IEC 14496-12 this is one past the highest track ID in the file,
    /// which is the closest thing MP4 has to a declared track count. It is not a
    /// count, and not every muxer writes it — a file leaving it zero declares
    /// nothing, and [`Option::None`] is returned rather than a fabricated zero.
    pub next_track_id: Option<u32>,
    /// Number of `trak` boxes actually present under `moov`.
    ///
    /// Counted by walking the boxes rather than read from a field, because this
    /// is the count that can disagree with what a demuxer recovers.
    pub trak_count: usize,
}

/// Reads the `mvhd` of the first `moov` in `input`.
///
/// Returns `None` when the file has no `moov` or the `mvhd` cannot be read. That
/// is distinct from a [`MovieHeader`] reporting `next_track_id: None`: one means
/// "no header here", the other "header here, declaring nothing".
#[must_use]
pub fn parse_movie_header(input: &[u8]) -> Option<MovieHeader> {
    let moov = moov_body(input)?;

    let trak_count = {
        let mut count = 0usize;
        let mut cursor = 0usize;
        while let Some((kind, _body, next)) = next_box(moov, cursor) {
            cursor = next;
            if &kind == b"trak" {
                count += 1;
            }
        }
        count
    };

    let next_track_id = box_body(moov, b"mvhd")
        .filter(|mvhd| mvhd.len() > 4)
        .and_then(|mvhd| {
            // `mvhd` is a full box: one byte of version, three of flags, before
            // its payload. Reading from offset 0 would take the version byte as
            // the high byte of the creation time and shift every field by one.
            //
            // The times are version-dependent — version 0 writes 32-bit creation
            // and modification times, version 1 widens both to 64 — so the offset
            // of `next_track_ID` differs. Reading a version-1 header as version 0
            // yields a plausible-looking but wrong track ID, which is worse than
            // reporting none.
            let wide = mvhd[0] == 1;
            // Past version+flags and the two times come: rate (4), volume (2),
            // reserved (2), the 9×4 matrix (36), and six reserved 32-bit fields
            // (24) before `next_track_ID`.
            let times = if wide { 8 + 8 } else { 4 + 4 };
            u32_at(mvhd, 4 + times + 4 + 2 + 2 + 36 + 24).filter(|id| *id > 0)
        });

    Some(MovieHeader {
        next_track_id,
        trak_count,
    })
}

/// Composition offsets for each track, expanded to one signed offset per sample.
///
/// Returns one entry per `trak` in document order, matching
/// [`parse_edit_lists`] and [`crate::ContainerInspection::streams`]. A track with
/// no `ctts` contributes an **empty** vector rather than a zero per sample, so a
/// caller can tell "no composition offsets" from "offsets that happen to be zero"
/// — the difference between a file that reorders frames and one that does not.
///
/// # Why this is parsed here
///
/// `stts` gives *decode* time. Presentation time is decode time plus the
/// composition offset, and for any file with B-frames the two differ. Without
/// this, every derived timestamp is monotonic by construction and
/// `TIMING.NON_MONOTONIC_PTS` can never fire on any MP4 — the presentation order
/// that makes it meaningful is exactly what was not being computed.
///
/// `tpt-kinetix-demux` has no `ctts` support at all, so this cannot be obtained
/// from the demuxer.
///
/// # Panics
///
/// Never. The input is attacker-controlled (spec §75) and every read is checked.
#[must_use]
pub fn parse_composition_offsets(input: &[u8]) -> Vec<Vec<i64>> {
    let mut out = Vec::new();
    let Some(moov) = moov_body(input) else {
        return out;
    };

    let mut cursor = 0usize;
    while let Some((kind, body, next)) = next_box(moov, cursor) {
        cursor = next;
        if &kind == b"trak" {
            out.push(track_composition_offsets(body));
        }
    }
    out
}

/// Expands one track's `ctts` into one signed offset per sample.
fn track_composition_offsets(trak: &[u8]) -> Vec<i64> {
    // `ctts` is nested at `trak/mdia/minf/stbl`, four levels down. A
    // direct-child search finds nothing and reports "no offsets", which reads as
    // "this file does not reorder frames" rather than "the reader looked in the
    // wrong place" — the silent-skip failure this file's other readers document.
    let Some(ctts) = box_body(trak, b"mdia")
        .and_then(|mdia| box_body(mdia, b"minf"))
        .and_then(|minf| box_body(minf, b"stbl"))
        .and_then(|stbl| box_body(stbl, b"ctts"))
    else {
        return Vec::new();
    };

    // Full box: one byte of version, three of flags, then entry count.
    if ctts.len() < 8 {
        return Vec::new();
    }
    // Version 1 widens the offset from unsigned 32-bit to signed 64-bit. Reading a
    // version-1 table as version 0 truncates a large offset and can turn a
    // positive one negative, which would fabricate a reordering that is not in
    // the file.
    let wide = ctts[0] == 1;
    let count =
        usize::try_from(u32::from_be_bytes(ctts[4..8].try_into().unwrap_or([0; 4]))).unwrap_or(0);

    // Each entry is a run: (sample_count, sample_offset). 8 bytes in version 0,
    // 16 in version 1.
    let entry_width = if wide { 16 } else { 8 };
    let available = ctts.len().saturating_sub(8) / entry_width;

    let mut offsets = Vec::new();
    for entry in 0..available.min(count) {
        let start = 8 + entry * entry_width;
        let (samples, offset) = if wide {
            (
                u64::from_be_bytes(ctts[start..start + 8].try_into().unwrap_or([0; 8])),
                i64::from_be_bytes(ctts[start + 8..start + 16].try_into().unwrap_or([0; 8])),
            )
        } else {
            (
                u64::from(u32::from_be_bytes(
                    ctts[start..start + 4].try_into().unwrap_or([0; 4]),
                )),
                i64::from(i32::from_be_bytes(
                    ctts[start + 4..start + 8].try_into().unwrap_or([0; 4]),
                )),
            )
        };

        // A hostile table can declare billions of samples in a single run. The cap
        // below is what bounds this loop — not the declared count, which is
        // attacker-controlled and cannot be trusted as a limit.
        for _ in 0..samples {
            if offsets.len() >= MAX_COMPOSITION_OFFSETS {
                return offsets;
            }
            offsets.push(offset);
        }
    }
    offsets
}

/// Ceiling on composition offsets expanded for one track.
///
/// A `ctts` entry count is attacker-controlled and each entry can declare a very
/// large run length, so the product is not bounded by the file's size. This caps
/// the work rather than trusting the declaration.
const MAX_COMPOSITION_OFFSETS: usize = 8_000_000;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{
        build_mp4, build_mp4_av, build_mp4_with_declared_track_mismatch,
        build_mp4_with_reordered_frames, TrackSpec,
    };

    #[test]
    fn a_track_with_no_ctts_has_no_composition_offsets() {
        let bytes = build_mp4(&TrackSpec::video_25fps(320, 240, 30));
        let offsets = parse_composition_offsets(&bytes);
        assert_eq!(offsets.len(), 1, "one entry per trak");
        assert!(
            offsets[0].is_empty(),
            "no `ctts` means no offsets, not a zero offset per sample"
        );
    }

    #[test]
    fn composition_offsets_are_read_per_track() {
        let bytes = build_mp4_with_reordered_frames(30);
        let offsets = parse_composition_offsets(&bytes);
        assert_eq!(offsets.len(), 1);
        assert_eq!(offsets[0].len(), 30, "one offset per sample");
    }

    #[test]
    fn the_reordered_fixture_presents_every_frame_exactly_once() {
        // The property that makes the B-frame fixture a description of real media
        // rather than a file that merely looks unusual.
        //
        // Composition offsets must be a *permutation*: each frame appears once, at
        // a distinct presentation time, and the presentation times are the decode
        // times reordered. An earlier version of this fixture used offsets that
        // were not a permutation and produced presentation times of
        // `3, 3, 3, 1, 7, 7, 7, 5` — three frames sharing a time and two times
        // with no frame at all. No encoder emits that, and it made the track look
        // like it had both duplicated and missing frames.
        let bytes = build_mp4_with_reordered_frames(30);
        let offsets = parse_composition_offsets(&bytes);
        let info = crate::mp4::track_frame_info(
            &tpt_kinetix_demux::mp4::Mp4Demuxer::new(bytes)
                .expect("demuxes")
                .tracks()[0],
            &offsets[0],
        )
        .expect("has frame info");

        let mut presentation = info.frame_times.clone();
        presentation.sort_unstable();

        let unique = presentation
            .iter()
            .map(|t| t.as_micros())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            unique.len(),
            presentation.len(),
            "every frame must present at a distinct time: {:?}",
            presentation
                .iter()
                .map(|t| t.as_micros())
                .collect::<Vec<_>>()
        );

        // Presentation order is a reordering of decode order, not a different timeline:
        // sorted, every interval must be the nominal frame duration except the
        // last, which runs long because the final group's delayed frames have no
        // successors to absorb them.
        //
        // That trailing long interval is not a fixture artefact — it is what any
        // closed IBBP stream does at its end, and it is why `TIMING.TIMESTAMP_GAP`
        // fires on this file. Comparing all intervals would hide the very thing
        // the fixture exists to show.
        let nominal = info
            .decode_times
            .get(1)
            .map(|t| t.signed_diff(info.decode_times[0]).as_micros())
            .unwrap_or_default();

        let presentation_intervals: Vec<i64> = presentation
            .windows(2)
            .map(|w| w[1].signed_diff(w[0]).as_micros())
            .collect();
        // Every interval is nominal except the last, which runs long because the
        // final group's delayed frames have nothing after them to absorb the delay.
        //
        // That trailing long interval is not a fixture artefact — it is what any
        // closed IBBP stream does at its end, and it is why
        // `TIMING.TIMESTAMP_GAP` fires on this file. Asserting uniform intervals
        // throughout would hide the very thing the fixture exists to show.
        let (last, leading) = presentation_intervals.split_last().expect("30 frames");
        assert!(
            leading.iter().all(|d| *d == nominal),
            "every interval but the last must be the nominal {nominal}us: {leading:?}"
        );
        assert!(
            *last > nominal,
            "the final interval should be long by the reorder delay, got {last}us \
             against a nominal {nominal}us"
        );
        assert!(
            presentation_intervals.len() == info.decode_times.len() - 1,
            "reordering must not change the frame count"
        );

        // And it must actually be reordered, or the fixture is not testing what
        // it claims to.
        assert!(
            info.frame_times.windows(2).any(|w| w[0] > w[1]),
            "sanity: some presentation timestamps must move backwards"
        );
        assert!(
            info.decode_times.windows(2).all(|w| w[0] < w[1]),
            "decode time must still be strictly increasing"
        );
    }

    #[test]
    fn a_file_without_a_moov_yields_no_offsets() {
        assert!(parse_composition_offsets(b"not an mp4").is_empty());
    }

    #[test]
    fn a_normal_file_declares_one_more_track_than_it_contains() {
        // What a muxer writes: `next_track_ID` is one past the highest ID used.
        let bytes = build_mp4(&TrackSpec::video_25fps(320, 240, 30));
        let header = parse_movie_header(&bytes).expect("mvhd is readable");
        assert_eq!(header.trak_count, 1);
        assert_eq!(header.next_track_id, Some(2));
    }

    #[test]
    fn an_av_file_counts_both_traks() {
        let bytes = build_mp4_av(
            &TrackSpec::video_25fps(320, 240, 30),
            &TrackSpec::audio_48khz(2_400),
            0,
        );
        let header = parse_movie_header(&bytes).expect("mvhd is readable");
        assert_eq!(header.trak_count, 2);
        assert_eq!(header.next_track_id, Some(3));
    }

    #[test]
    fn a_header_can_claim_more_tracks_than_the_file_carries() {
        let bytes = build_mp4_with_declared_track_mismatch(4);
        let header = parse_movie_header(&bytes).expect("mvhd is readable");
        assert_eq!(header.trak_count, 1, "only one trak box is present");
        assert_eq!(
            header.next_track_id,
            Some(4),
            "the header claims three tracks were expected"
        );
    }

    #[test]
    fn a_file_without_a_moov_has_no_header() {
        assert!(parse_movie_header(b"not an mp4 at all").is_none());
    }

    #[test]
    fn empty_input_is_handled_without_panicking() {
        assert!(parse_movie_header(&[]).is_none());
    }
}
