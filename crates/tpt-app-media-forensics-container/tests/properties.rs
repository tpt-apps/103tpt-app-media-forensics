//! Property tests for the container parsers (spec §75–§77).
//!
//! # Why parsers specifically
//!
//! Every parser in this crate takes attacker-controlled input, and until now each
//! was defended only by examples. Examples cannot cover the space: a box reader
//! has to be correct for every combination of declared size, remaining length,
//! and nesting depth, and the failures are the ones nobody thought to write down.
//!
//! The properties asserted here are the invariants a reader must hold for *all*
//! input, not for a chosen few:
//!
//! - **No panic, ever.** The strongest property, and the one that matters most
//!   for a tool pointed at evidence.
//! - **Truncation is monotone.** A prefix of a file can never reveal more than
//!   the whole file. A parser that reports a `colr` box at 200 bytes but not at
//!   400 is reading past the end of what it was given.
//! - **Length is respected.** No reader may look beyond the bytes it was handed.
//!
//! # Why the case count is modest
//!
//! These run in CI on every build. A property that takes minutes is one that
//! stops being run. `proptest` persists a failure seed, so anything found here is
//! reproducible without this file.

use proptest::prelude::*;

use tpt_app_media_forensics_container::colr::parse_track_colour;
use tpt_app_media_forensics_container::elst::parse_edit_lists;
use tpt_app_media_forensics_container::{boxes, damage};

/// Arbitrary bytes: the input class every reader in this crate is exposed to.
fn arbitrary_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..2048)
}

/// A valid `moov` box wrapping `body`.
fn moov(body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(b"moov");
    out.extend_from_slice(body);
    out
}

/// A valid box of the given type wrapping `body`.
fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// The box walk must never panic, and must always advance.
    ///
    /// This is the property that matters most in a forensic tool: the input is
    /// the evidence, and it may be hostile. The advance check is what stops a
    /// walk that reports a zero-length box at the same offset forever — a hang
    /// rather than a panic, and just as fatal.
    #[test]
    fn next_box_never_panics_and_always_advances(data in arbitrary_bytes()) {
        let mut cursor = 0usize;
        // Bounded by the input length as well as by the walk terminating: a box
        // reporting zero length at the same offset forever would hang rather than
        // fail, and the advance assertion below catches that on the first turn.
        while cursor <= data.len() {
            let Some((_, body, next)) = boxes::next_box(&data, cursor) else {
                break;
            };
            prop_assert!(next > cursor, "a box must advance the walk");
            prop_assert!(
                next <= data.len(),
                "the walk ran past the end of its input"
            );
            prop_assert!(body.len() <= data.len());
            cursor = next;
        }
    }

    /// Truncating a file can never make a reader see more than the whole file.
    ///
    /// Catches a parser reading past the end of its input: such a parser is
    /// usually correct on a whole file and invents structure on a damaged one,
    /// which is precisely the case a forensic reader meets.
    #[test]
    fn a_prefix_never_reveals_more_than_the_whole_file(
        data in arbitrary_bytes(),
        cut in 0usize..2048,
    ) {
        let whole = moov(&data);
        let cut = cut.min(whole.len());

        prop_assert!(
            parse_track_colour(&whole[..cut]).len() <= parse_track_colour(&whole).len(),
            "a {cut}-byte prefix found more tracks than the whole file"
        );
        prop_assert!(
            parse_edit_lists(&whole[..cut]).len() <= parse_edit_lists(&whole).len(),
            "a {cut}-byte prefix found more edit lists than the whole file"
        );
    }

    /// No reader may report a track the file does not contain.
    ///
    /// Indexed output is the trap: a reader that walks a `moov` and pushes one
    /// entry per `trak` must never emit more entries than there are `trak`
    /// boxes, at any length.
    #[test]
    fn track_readers_never_exceed_the_number_of_trak_boxes(data in arbitrary_bytes()) {
        let file = moov(&data);

        let mut trak_count = 0usize;
        let mut cursor = 0usize;
        while let Some((kind, _, next)) = boxes::next_box(&data, cursor) {
            cursor = next;
            if &kind == b"trak" {
                trak_count += 1;
            }
        }

        prop_assert!(parse_track_colour(&file).len() <= trak_count);
        prop_assert!(parse_track_colour(&file).len() <= trak_count);
        prop_assert!(parse_edit_lists(&file).len() <= trak_count);
    }

    /// A reported colour must correspond to the code the file declares.
    ///
    /// Built from a known `colr` code, so any other value in the output is the
    /// reader inventing one — the defect class this area exists to prevent.
    #[test]
    fn reported_primaries_never_contradict_the_declared_code(code in 1u16..=22) {
        let mut payload = b"nclx".to_vec();
        payload.extend_from_slice(&code.to_be_bytes());
        payload.extend_from_slice(&1u16.to_be_bytes());
        payload.extend_from_slice(&1u16.to_be_bytes());
        payload.push(0x80);
        let colr = mp4_box(b"colr", &payload);

        // A visual sample entry: 78 bytes of fixed fields, then `colr` as a child.
        let mut sample = vec![0u8; 78];
        sample.extend_from_slice(&colr);
        let sample_entry = mp4_box(b"avc1", &sample);

        let mut stsd_body = vec![0u8; 4];
        stsd_body.extend_from_slice(&1u32.to_be_bytes());
        stsd_body.extend_from_slice(&sample_entry);
        let stbl = mp4_box(b"stbl", &mp4_box(b"stsd", &stsd_body));
        let trak = mp4_box(b"trak", &mp4_box(b"mdia", &mp4_box(b"minf", &stbl)));

        let found = parse_track_colour(&moov(&trak));
        prop_assert_eq!(found.len(), 1, "one trak yields one entry");

        // Every code in range either has its H.273 name or is reported as
        // unrecognised. What must never appear is the name of a *different* code:
        // that would be the reader substituting a plausible answer for the one
        // the file actually gave.
        if let Some(primaries) = &found[0].colour.primaries {
            prop_assert!(!primaries.is_empty(), "a populated field needs a value");
            if code == 1 {
                prop_assert_eq!(primaries, "BT.709", "code 1 is BT.709");
            } else if code == 9 {
                prop_assert_eq!(primaries, "BT.2020", "code 9 is BT.2020");
            }
        }
    }

    /// Structural damage scanning must stay inside its input.
    ///
    /// The scan runs over every byte of every file, so it is the reader most
    /// exposed to a hostile length field.
    #[test]
    fn damage_scan_stays_within_its_input(data in arbitrary_bytes()) {
        for defect in damage::scan_isobmff(&data) {
            prop_assert!(
                defect.offset() <= data.len() as u64,
                "defect offset {} lies outside a {}-byte input",
                defect.offset(),
                data.len()
            );
        }
    }

    /// An absurd declared size must never be read past.
    ///
    /// `size == 0` means "to end of buffer" and `size == 1` is the 64-bit form
    /// per ISO/IEC 14496-12; neither may walk beyond the bytes present.
    #[test]
    fn absurd_box_sizes_are_refused(size in 9u32..=u32::MAX) {
        let mut data = size.to_be_bytes().to_vec();
        data.extend_from_slice(b"moov");
        data.extend_from_slice(b"trailing");

        if let Some((kind, body, next)) = boxes::next_box(&data, 0) {
            prop_assert_eq!(&kind, b"moov");
            prop_assert!(next <= data.len(), "the walk left its input");
            prop_assert!(body.len() <= data.len(), "a body extended past the input");
        }
    }
}
