//! Colour signalling: `colr`, and the HDR static metadata beside it (spec §14,
//! §45, §46).
//!
//! # Why this is parsed here rather than taken from the demuxer
//!
//! `tpt-kinetix-demux` has no colour support of any kind: its `Mp4Track` carries
//! geometry, timing and a codec fourcc, and stops there. So this is the only
//! place primaries, transfer, matrix, and range can come from.
//!
//! That left `VideoFormat::colour` permanently `Default::default()` and
//! `is_hdr` permanently `false` — a modelled field that no reader ever filled,
//! which is the same defect class as the edit list and the composition offsets
//! this crate has already had to correct. The result was that every colour field
//! in every report was empty, and `is_hdr` was a constant rather than a
//! measurement.
//!
//! # What is declared, not what is rendered
//!
//! These are the values the *container asserts* about its video. Nothing here
//! decodes a sample, so nothing here can tell whether the pixels match. That
//! distinction is the whole of spec §46's "provide a readable interpretation
//! while preserving raw metadata": the numbers are preserved verbatim, and a
//! value this build does not recognise is reported as unrecognised rather than
//! dropped.
//!
//! # Matroska
//!
//! There is deliberately no Matroska counterpart here. The underlying EBML
//! reader exposes no picture geometry at all, so a WebM video stream is reported
//! with `video: None` — there is no
//! [`tpt_app_media_forensics_model::VideoFormat`] to attach colour to, and
//! inventing one purely to hold a primaries value would put a resolution in a
//! report that no measurement produced. The Matroska `Colour` element is
//! therefore reported as unmeasured, which is the honest reading.

use tpt_app_media_forensics_model::ColourInfo;

use crate::boxes::{box_body, next_box, u16_at};

/// Fixed width of a visual sample entry's fields, in bytes, before its child
/// boxes begin.
///
/// Per ISO/IEC 14496-12 the `VisualSampleEntry` is 78 bytes past its own box
/// header: 6 reserved, 2 data reference index, 16 pre-defined/reserved, 2
/// width, 2 height, 4 horizontal resolution, 4 vertical resolution, 4 reserved,
/// 2 frame count, 32 compressor name, 2 depth, 2 pre-defined. `colr`, `mdcv`, and
/// `clli` are children that follow it.
///
/// This offset is the whole difficulty here. A reader that searched the sample
/// entry for a `colr` box from offset 0 would instead interpret the width and
/// height fields as a box size and type, walk off into the middle of the entry,
/// and report no colour on every file — indistinguishable from a file that
/// carries none.
const VISUAL_SAMPLE_ENTRY_FIELDS: usize = 78;

/// What one video track declares about its colour, per track in `moov` order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrackColour {
    /// Primaries, transfer, matrix and range, when a `colr` box supplies them.
    pub colour: ColourInfo,
    /// True when the declaration is HDR: BT.2020 primaries, or a PQ/HLG transfer.
    pub is_hdr: bool,
    /// Mastering-display and content-light-level values, rendered readably.
    ///
    /// `None` when the track carries no HDR static metadata, which is different
    /// from carrying none that this build could read.
    pub static_metadata: Option<String>,
}

/// Reads the colour signalling of every track in `moov`, in document order.
///
/// Returns one entry per `trak`, so index `n` corresponds to track `n` — the
/// ordering [`crate::ContainerInspection::streams`] uses. A track with no colour
/// declaration yields an all-`None` entry rather than being omitted, so the
/// vector's length always equals the number of tracks found.
///
/// # Panics
///
/// Never. Every read is bounds-checked, because the input is attacker-controlled
/// (spec §75).
#[must_use]
pub fn parse_track_colour(input: &[u8]) -> Vec<TrackColour> {
    let mut out = Vec::new();
    let Some(moov) = crate::boxes::moov_body(input) else {
        return out;
    };

    let mut cursor = 0usize;
    while let Some((kind, body, next)) = next_box(moov, cursor) {
        cursor = next;
        if &kind == b"trak" {
            out.push(track_colour(body));
        }
    }
    out
}

/// Reads one track's colour declaration from `trak`.
fn track_colour(trak: &[u8]) -> TrackColour {
    // `colr` sits five levels down, under the video sample description. A
    // direct-child search finds nothing and reports "this file declares no
    // colour", which reads as a statement about the file rather than about where
    // the reader looked — the silent-skip failure this crate's other readers
    // document at length.
    let Some(stbl) = descend(trak, &[b"mdia", b"minf", b"stbl"]) else {
        return TrackColour::default();
    };
    let Some(entries) = box_body(stbl, b"stsd").map(stsd_entries) else {
        return TrackColour::default();
    };

    // Only the first sample description is consulted. It is the one `stsc` points
    // at by default, so it is the description the track's samples are actually
    // decoded with; a later entry is an alternative the track does not use unless
    // `stsc` says so, which this build does not resolve.
    let Some((kind, body)) = entries.first().copied() else {
        return TrackColour::default();
    };
    if !is_visual_entry(kind) {
        return TrackColour::default();
    }

    let Some(children) = body.get(VISUAL_SAMPLE_ENTRY_FIELDS..) else {
        return TrackColour::default();
    };

    let mut result = TrackColour::default();
    let mut metadata: Vec<String> = Vec::new();

    let mut cursor = 0usize;
    while let Some((child, child_body, next)) = next_box(children, cursor) {
        cursor = next;
        match &child {
            b"colr" => read_colr(child_body, &mut result),
            // Accumulated and joined rather than appended to one string, so that a
            // truncated `mdcv` cannot run its text into the `clli` line beside it.
            b"mdcv" | b"clli" => {
                let rendered = read_static_metadata(&child, child_body);
                if !rendered.is_empty() {
                    metadata.push(rendered);
                }
            }
            _ => {}
        }
    }

    if !metadata.is_empty() {
        result.static_metadata = Some(metadata.join("; "));
    }

    result
}

/// Walks a chain of nested box types, returning the innermost body.
fn descend<'a>(data: &'a [u8], path: &[&[u8; 4]]) -> Option<&'a [u8]> {
    let mut current = data;
    for kind in path {
        current = box_body(current, kind)?;
    }
    Some(current)
}

/// Splits an `stsd` payload into its sample entries.
///
/// The payload opens with 4 bytes of version and flags and a 4-byte entry count
/// before the first nested box. Reading an entry from offset 0 would take the
/// version byte as the high byte of a box size — the same off-by-a-header error
/// the `mvhd` reader documents.
fn stsd_entries(payload: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let Some(entries) = payload.get(8..) else {
        return out;
    };

    let mut cursor = 0usize;
    while let Some((kind, body, next)) = next_box(entries, cursor) {
        cursor = next;
        out.push((kind, body));
    }
    out
}

/// Whether a sample entry fourcc is a video one, so its children are read.
///
/// `avc1`, `hvc1`, `hev1`, `av01`, `vp09`, and `encv` cover what this build can
/// meet. An audio entry has no `colr`, and reading one would interpret an audio
/// sample entry's channel count as a box header.
fn is_visual_entry(kind: [u8; 4]) -> bool {
    matches!(
        kind,
        [b'a', b'v', b'c', b'1']
            | [b'a', b'v', b'c', b'3']
            | [b'h', b'v', b'c', b'1']
            | [b'h', b'e', b'v', b'1']
            | [b'a', b'v', b'0', b'1']
            | [b'v', b'p', b'0', b'9']
            | [b'e', b'n', b'c', b'v']
            | [b'd', b'v', b'h', b'e']
            | [b'd', b'v', b'h', b'1']
    )
}

/// Reads a `colr` payload into `result`.
///
/// `colour_type` selects the layout: `nclx` carries the range flag after the
/// three code points, `nclc` does not. Treating an `nclc` as `nclx` would report
/// a range for a box that declares none, which is exactly the kind of invented
/// measurement this crate exists to avoid — so the two are read separately and
/// `nclc` leaves range unmeasured.
///
/// `rICC` and `prof` carry an ICC profile, which this build does not parse.
/// Leaving every field `None` says "not measured" rather than "absent": the file
/// may well declare colour, just not in the enumerated form this reader
/// understands.
fn read_colr(payload: &[u8], result: &mut TrackColour) {
    let Some(kind) = payload.get(..4) else {
        return;
    };

    let (primaries, transfer, matrix, range) = match kind {
        b"nclx" => {
            // Bit 7 of the byte after the codes is `full_range_flag`; the
            // remaining bits are reserved. Reading the whole byte as a boolean
            // would report "limited" for a box whose reserved bits happen to be
            // set.
            let full_range = payload.get(10).map(|byte| byte & 0x80 != 0);
            (
                u16_at(payload, 4),
                u16_at(payload, 6),
                u16_at(payload, 8),
                full_range,
            )
        }
        b"nclc" => (
            u16_at(payload, 4),
            u16_at(payload, 6),
            u16_at(payload, 8),
            None,
        ),
        _ => return,
    };

    result.colour.primaries = primaries.and_then(primaries_name);
    result.colour.transfer = transfer.and_then(transfer_name);
    result.colour.matrix = matrix.and_then(matrix_name);
    result.colour.full_range = range;
    result.is_hdr = is_hdr(
        result.colour.primaries.as_deref(),
        result.colour.transfer.as_deref(),
    );
}

/// Whether a primaries/transfer pair describes HDR.
///
/// BT.2020 primaries or a PQ/HLG transfer function, which is the model's own
/// definition of the flag. An unrecognised code is never treated as HDR:
/// guessing would turn an unknown value into a positive claim about the file.
fn is_hdr(primaries: Option<&str>, transfer: Option<&str>) -> bool {
    primaries == Some("BT.2020")
        || matches!(
            transfer,
            Some("PQ (SMPTE ST 2084)") | Some("HLG (ARIB STD-B67)")
        )
}

/// Renders `mdcv` or `clli` as a readable line, or an empty string if unreadable.
///
/// An empty string rather than `None` because the caller is accumulating: a file
/// carrying a truncated `mdcv` should not have its `clli` line suppressed by the
/// failure of the box before it.
fn read_static_metadata(kind: &[u8; 4], payload: &[u8]) -> String {
    match kind {
        b"mdcv" => read_mastering_display(payload),
        b"clli" => read_content_light_level(payload),
        _ => String::new(),
    }
}

/// Renders the mastering display colour volume (`mdcv`, ISO/IEC 23001-7).
///
/// Chromaticity coordinates are unsigned 16-bit fractions of 0.0001 and
/// luminance a 32-bit fraction of 0.0001 cd/m², so both are divided to report
/// real units. The raw values are kept alongside: a reviewer needs to see what
/// the file actually said, not only this build's reading of it.
fn read_mastering_display(payload: &[u8]) -> String {
    // A full box: one byte of version, three of flags, before the payload.
    // Reading from offset 0 would take the version byte as the high byte of the
    // first display-primary coordinate.
    const HEADER: usize = 4;
    // 3 display primaries (x, y) plus the white point (x, y): eight u16 fields.
    const CHROMATICITIES: usize = 16;
    const LUMINANCE: usize = 8;

    let Some(end) = HEADER
        .checked_add(CHROMATICITIES + LUMINANCE)
        .filter(|end| *end <= payload.len())
    else {
        return String::new();
    };
    let body = &payload[HEADER..end];

    let chromaticity = |offset: usize| {
        u16_at(body, offset).map_or_else(String::new, |raw| {
            format!("{:.4}", f64::from(raw) / 10_000.0)
        })
    };

    let luminance = |offset: usize| {
        // The 32-bit fields are read as two 16-bit halves rather than by casting
        // a slice, so the read stays bounds-checked like every other one here.
        u16_at(body, offset)
            .zip(u16_at(body, offset + 2))
            .map_or_else(String::new, |(high, low)| {
                let raw = (u64::from(high) << 16) | u64::from(low);
                format!("{:.4} cd/m2 (raw {raw})", raw as f64 / 10_000.0)
            })
    };

    // Renders one primary's (x, y) pair. Both halves are shown: printing only the x
    // coordinate makes R(0.6800) G(0.2650) B(0.1500) look like the fixture's
    // y values had been dropped, when in fact the caller only asked for one of
    // each pair.
    let primary =
        |offset: usize| format!("({}, {})", chromaticity(offset), chromaticity(offset + 2));

    format!(
        "mastering display: primaries R{primary_r} G{primary_g} B{primary_b}, \
         white point ({white_x}, {white_y}), max luminance {max_lum}, \
         min luminance {min_lum}",
        primary_r = primary(0),
        primary_g = primary(4),
        primary_b = primary(8),
        white_x = chromaticity(12),
        white_y = chromaticity(14),
        max_lum = luminance(CHROMATICITIES),
        min_lum = luminance(CHROMATICITIES + 4),
    )
}

/// Renders the content light level (`clli`, CTA-861.3).
///
/// Both fields are in lumels, and 10,000 lumels is 1 cd/m², so they are divided
/// to report real units and the raw value kept beside them.
///
/// Four decimal places, not zero: `clli` stores whole lumels, so the smallest
/// non-zero value it can express is 0.0001 cd/m². Formatting to whole cd/m²
/// renders that as `0` — indistinguishable from "no value", and a reader would
/// have no way to tell a genuinely light signal from a rounding artefact.
fn read_content_light_level(payload: &[u8]) -> String {
    let Some(max_average) = u16_at(payload, 4) else {
        return String::new();
    };
    let peak = u16_at(payload, 6);

    let lumels = |raw: u16| format!("{:.4} cd/m2 (raw {raw} lumels)", f64::from(raw) / 10_000.0);
    let average = lumels(max_average);
    let peak = peak.map_or_else(|| "not declared".to_owned(), lumels);

    format!("content light: max average {average}, max peak {peak}")
}

/// Names a colour primaries code, per ITU-T H.273 / ISO 23091-2.
///
/// An unrecognised code is rendered as `unrecognised code N` rather than dropped:
/// the number is itself the evidence, and a file declaring a code this build
/// cannot name has still declared *something*. Codes 0 and 2 mean "unspecified"
/// and are reported as such rather than treated as unknown.
fn primaries_name(code: u16) -> Option<String> {
    Some(match code {
        0 | 2 => "unspecified".to_owned(),
        1 => "BT.709".to_owned(),
        4 => "BT.470M".to_owned(),
        5 => "BT.470BG".to_owned(),
        6 => "SMPTE 170M".to_owned(),
        7 => "SMPTE 240M".to_owned(),
        8 => "film (generic)".to_owned(),
        9 => "BT.2020".to_owned(),
        10 => "SMPTE ST 428".to_owned(),
        11 => "SMPTE ST 431 (DCI P3)".to_owned(),
        12 => "SMPTE ST 432 (DCI P3 D65)".to_owned(),
        22 => "EBU Tech. 3213".to_owned(),
        other => format!("unrecognised code {other}"),
    })
}

/// Names a transfer characteristics code, per ITU-T H.273 / ISO 23091-2.
///
/// Codes 12 and 16 are both written by real muxers for PQ and 14 and 18 both for
/// HLG; they are aliases rather than distinct functions, so each pair renders
/// the same name. Reporting them differently would make two files carrying the
/// same transfer look different in a report.
fn transfer_name(code: u16) -> Option<String> {
    Some(match code {
        0 | 2 => "unspecified".to_owned(),
        1 => "BT.709".to_owned(),
        4 => "BT.470M (gamma 2.2)".to_owned(),
        5 => "BT.470BG (gamma 2.8)".to_owned(),
        6 => "SMPTE 170M".to_owned(),
        7 => "SMPTE 240M".to_owned(),
        8 => "linear".to_owned(),
        9 => "logarithmic (100:1 range)".to_owned(),
        10 => "BT.2020 10-bit".to_owned(),
        11 => "BT.2020 12-bit".to_owned(),
        12 | 15 | 16 => "PQ (SMPTE ST 2084)".to_owned(),
        13 => "SMPTE ST 428".to_owned(),
        14 | 18 => "HLG (ARIB STD-B67)".to_owned(),
        17 => "reserved".to_owned(),
        other => format!("unrecognised code {other}"),
    })
}

/// Names a matrix coefficients code, per ITU-T H.273 / ISO 23091-2.
fn matrix_name(code: u16) -> Option<String> {
    Some(match code {
        0 => "identity (GBR)".to_owned(),
        1 => "BT.709".to_owned(),
        2 | 3 => "unspecified".to_owned(),
        4 => "FCC".to_owned(),
        5 => "BT.470BG".to_owned(),
        6 => "SMPTE 170M".to_owned(),
        7 => "SMPTE 240M".to_owned(),
        8 => "YCgCo".to_owned(),
        9 => "BT.2020 non-constant luminance".to_owned(),
        10 => "BT.2020 constant luminance".to_owned(),
        11 => "SMPTE ST 2085".to_owned(),
        12 => "chroma-derived non-constant luminance".to_owned(),
        13 => "chroma-derived constant luminance".to_owned(),
        14 => "ICtCp".to_owned(),
        other => format!("unrecognised code {other}"),
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_track_colour, TrackColour};
    use crate::fixture::{
        build_mp4, build_mp4_av, build_mp4_with_colour, build_mp4_with_hdr_colour, TrackSpec,
    };
    use tpt_app_media_forensics_model::ColourInfo;

    /// The first track's colour, from a built file.
    fn first(bytes: &[u8]) -> TrackColour {
        parse_track_colour(bytes)
            .into_iter()
            .next()
            .expect("one track")
    }

    #[test]
    fn a_colour_box_populates_every_field_it_declares() {
        let colour = first(&build_mp4_with_colour());

        assert_eq!(colour.colour.primaries.as_deref(), Some("BT.709"));
        assert_eq!(colour.colour.transfer.as_deref(), Some("BT.709"));
        assert_eq!(colour.colour.matrix.as_deref(), Some("BT.709"));
        assert_eq!(colour.colour.full_range, Some(false));
        assert!(!colour.is_hdr, "BT.709 signalling is not HDR");
    }

    #[test]
    fn hdr_signalling_sets_the_hdr_flag() {
        let colour = first(&build_mp4_with_hdr_colour());
        assert_eq!(colour.colour.primaries.as_deref(), Some("BT.2020"));
        assert_eq!(
            colour.colour.transfer.as_deref(),
            Some("PQ (SMPTE ST 2084)")
        );
        assert!(colour.is_hdr, "BT.2020 with PQ is HDR");
    }

    #[test]
    fn hdr_static_metadata_is_reported_with_its_raw_values() {
        let colour = first(&build_mp4_with_hdr_colour());
        let metadata = colour.static_metadata.expect("mdcv and clli present");

        // Both boxes were read, not one of them.
        assert!(metadata.contains("mastering display"), "{metadata}");
        assert!(metadata.contains("content light"), "{metadata}");

        // Readable units, so a reviewer can read the numbers: the fixture's
        // 10,000,000 is 1000 cd/m² at 0.0001 per unit.
        assert!(metadata.contains("1000.0000 cd/m2"), "{metadata}");
        // And the raw values, so a reviewer can check this build's reading.
        assert!(metadata.contains("raw 10000000"), "{metadata}");
        // Display primaries survive as chromaticity, and both halves of each
        // pair are shown: reading only x would report R(0.6800) and silently
        // discard the 0.3200 the file also declares.
        assert!(metadata.contains("R(0.6800, 0.3200)"), "{metadata}");
        assert!(
            metadata.contains("white point (0.3127, 0.3290)"),
            "{metadata}"
        );
        // Whole-lumel values must not round to "0", which reads as no value.
        assert!(metadata.contains("0.4000 cd/m2"), "{metadata}");
        // And the two boxes must not run together into one unreadable line.
        assert!(metadata.contains("; "), "{metadata}");
    }

    #[test]
    fn a_file_carrying_no_colour_box_reports_no_colour_at_all() {
        // Every fixture but the two colour builders writes no `colr`, which is
        // what an SDR file with no signalling at all looks like.
        let colour = first(&build_mp4(&TrackSpec::video_25fps(320, 240, 30)));

        assert_eq!(colour.colour, ColourInfo::default());
        assert!(!colour.is_hdr);
        assert!(colour.static_metadata.is_none());
    }

    #[test]
    fn a_track_with_no_colour_still_occupies_its_slot() {
        // The vector is indexed by track position, so an omitted entry would
        // shift every later track's colour onto the wrong track.
        let bytes = build_mp4_av(
            &TrackSpec::video_25fps(320, 240, 30),
            &TrackSpec::audio_48khz(2_400),
            0,
        );
        assert_eq!(parse_track_colour(&bytes).len(), 2);
    }

    #[test]
    fn an_audio_sample_entry_is_not_read_as_a_video_one() {
        // An audio entry's channel count, read as a box header, is a
        // plausible-looking box size. Nothing should come of it.
        let bytes = build_mp4_av(
            &TrackSpec::video_25fps(320, 240, 30),
            &TrackSpec::audio_48khz(2_400),
            0,
        );
        assert!(parse_track_colour(&bytes)[1].colour.primaries.is_none());
    }

    #[test]
    fn a_truncated_colour_box_is_refused_rather_than_half_read() {
        // The input is attacker-controlled (spec §75): every truncation must be
        // survivable, and none may produce a colour the complete file does not
        // declare.
        let bytes = build_mp4_with_hdr_colour();
        let complete = parse_track_colour(&bytes);
        for len in 0..bytes.len() {
            for track in parse_track_colour(&bytes[..len]) {
                if let (Some(seen), Some(reference)) = (
                    track.colour.primaries.as_ref(),
                    complete.first().and_then(|t| t.colour.primaries.as_ref()),
                ) {
                    assert_eq!(
                        seen, reference,
                        "a truncated read invented primaries at {len} bytes"
                    );
                }
            }
        }
    }

    #[test]
    fn arbitrary_bytes_are_handled_without_panicking() {
        for seed in 1..64u8 {
            let junk: Vec<u8> = (0..(seed as usize) * 7)
                .map(|i| (i * 31 + usize::from(seed)) as u8)
                .collect();
            let _ = parse_track_colour(&junk);
        }
    }

    #[test]
    fn empty_input_yields_no_tracks() {
        assert!(parse_track_colour(&[]).is_empty());
    }

    #[test]
    fn a_file_that_is_not_an_mp4_yields_no_tracks() {
        assert!(parse_track_colour(b"not an mp4 at all").is_empty());
    }
}
