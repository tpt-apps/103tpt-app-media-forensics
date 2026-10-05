//! Audio sample descriptions: the `mp4a` `AudioSampleEntry` (spec §19, §68).
//!
//! # Why this is parsed here rather than taken from the demuxer
//!
//! `tpt-kinetix-demux`'s `Mp4Track` carries geometry, timing, and a codec fourcc,
//! and stops there — it exposes no channel count, sample rate, or sample size. So
//! before this module, `StreamAnalysis::audio` was hardcoded to `None` for every
//! MP4 track.
//!
//! That left [`AudioFormat`] permanently empty on the one container format this
//! engine parses most, which is the same defect class as `VideoFormat::colour`
//! before `colr` was read: a modelled field that no reader ever filled. The
//! visible consequence is that spec §68's own example requirements —
//! `audio.channels: 2` and `audio.sample_rate: 48000` — could not be checked
//! against an MP4 at all, and a delivery profile would have had to skip them
//! silently.
//!
//! # The offset is the whole difficulty
//!
//! An `AudioSampleEntry` is a different fixed-width structure from the
//! [`crate::colr`] `VisualSampleEntry`. Per ISO/IEC 14496-12, after its own box
//! header it is 28 bytes: 6 reserved, 2 data reference index, 8 reserved,
//! 2 channel count, 2 sample size, 2 pre-defined, 2 reserved, and a 4-byte
//! 16.16 fixed-point sample rate.
//!
//! Reading those fields at the visual entry's offsets would take a channel count
//! out of the middle of the reserved run and report a sample rate assembled
//! from the wrong two words — a plausible-looking wrong answer, which is worse
//! than no answer at all.
//!
//! # What is declared, not what is decoded
//!
//! These are the values the container *asserts* about its audio. Nothing here
//! decodes a sample, so nothing here can tell whether the bitstream matches.
//! That distinction is the whole of spec §26's declared-versus-measured split.
//!
//! # Matroska
//!
//! There is deliberately no Matroska counterpart here, for the same reason
//! [`crate::colr`] has none: the EBML reader exposes no audio parameters, so a
//! WebM audio stream reports `AudioFormat` with no layout and a zero sample rate
//! rather than a fabricated one. `mkv.rs` says so in the code.

use tpt_app_media_forensics_model::{AudioFormat, ChannelLayout};

use crate::boxes::{box_body, next_box, u16_at, u32_at};

/// Fixed width of an audio sample entry's fields, in bytes, before its child
/// boxes begin.
///
/// Per ISO/IEC 14496-12: 6 reserved, 2 data reference index, 8 reserved,
/// 2 channel count, 2 sample size, 2 pre-defined (compression id), 2 reserved,
/// 4 sample rate. See the module docs for why this offset matters.
const AUDIO_SAMPLE_ENTRY_FIELDS: usize = 28;

/// Offset of `channelcount` within an audio sample entry's body.
const CHANNEL_COUNT_AT: usize = 16;

/// Offset of `samplesize` within an audio sample entry's body.
const SAMPLE_SIZE_AT: usize = 18;

/// Offset of the 16.16 fixed-point `samplerate` within an audio sample entry.
const SAMPLE_RATE_AT: usize = 24;

/// What one audio track declares about itself, per track in `moov` order.
///
/// One entry per `trak`, so index `n` corresponds to track `n` — the ordering
/// [`crate::ContainerInspection::streams`] uses. A track that declares no audio
/// parameters yields [`TrackAudio::default`] rather than being omitted, so the
/// vector's length always equals the number of tracks found and index `n` always
/// refers to track `n`.
///
/// # Panics
///
/// Never. Every read is bounds-checked, because the input is attacker-controlled
/// (spec §75).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrackAudio {
    /// The audio format this track declares, when it declared one.
    ///
    /// `None` for a video track, and `None` for an audio track whose sample entry
    /// was absent, truncated, or of a type this reader does not recognise.
    pub format: Option<AudioFormat>,
}

/// Reads the audio sample description of every track in `moov`, in document order.
#[must_use]
pub fn parse_track_audio(input: &[u8]) -> Vec<TrackAudio> {
    let mut out = Vec::new();
    let Some(moov) = crate::boxes::moov_body(input) else {
        return out;
    };

    let mut cursor = 0usize;
    while let Some((kind, body, next)) = next_box(moov, cursor) {
        cursor = next;
        if &kind == b"trak" {
            out.push(track_audio(body));
        }
    }
    out
}

/// Reads one track's audio declaration from `trak`.
fn track_audio(trak: &[u8]) -> TrackAudio {
    // `stsd` sits four levels down. A direct-child search would find nothing and
    // report "this file declares no audio format", which reads as a statement
    // about the file rather than about where the reader looked.
    let Some(stbl) = descend(trak, &[b"mdia", b"minf", b"stbl"]) else {
        return TrackAudio::default();
    };
    let Some(entries) = box_body(stbl, b"stsd").map(stsd_entries) else {
        return TrackAudio::default();
    };

    // Only the first sample description is consulted, matching `colr`: it is the
    // one `stsc` points at by default, so it is the description the track's
    // samples are actually decoded with.
    let Some((kind, body)) = entries.first().copied() else {
        return TrackAudio::default();
    };
    if !is_audio_entry(kind) {
        return TrackAudio::default();
    }

    TrackAudio {
        format: read_audio_entry(body),
    }
}
/// Reads an audio sample entry body into an [`AudioFormat`].
///
/// Returns `None` when the entry is too short to hold the fields. A truncated
/// entry is *not* zero-filled: reporting a channel count of 0 as though the file
/// declared it would be an invented measurement, and reporting `Some` with
/// zeros would be worse.
fn read_audio_entry(body: &[u8]) -> Option<AudioFormat> {
    // The declared fields must all be present. The child boxes that follow are
    // optional, so this bounds the fixed prefix, not the whole entry.
    if body.len() < AUDIO_SAMPLE_ENTRY_FIELDS {
        return None;
    }

    let channels = u16_at(body, CHANNEL_COUNT_AT)?;
    let sample_size = u16_at(body, SAMPLE_SIZE_AT)?;

    // The sample rate is a 16.16 fixed-point value. Only the integer part is
    // meaningful as a rate in Hz, and the fractional part is discarded rather
    // than rounded into a rate no real file declares.
    let raw_rate = u32_at(body, SAMPLE_RATE_AT)?;
    let sample_rate = raw_rate >> 16;

    Some(AudioFormat {
        sample_rate,
        bit_depth: sample_size,
        channel_layout: Some(ChannelLayout {
            // The entry declares a count, not a name. "stereo" would be an
            // interpretation, and a 6-channel layout is not necessarily 5.1 —
            // so the count is named as a count and nothing more is claimed.
            name: format!("{channels} channel(s)"),
            channel_count: channels,
            channel_mask: None,
        }),
    })
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
/// [`crate::colr`] documents.
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

/// Whether a sample entry fourcc is an audio one, so its fields are read.
///
/// A reader that read a `VisualSampleEntry` with the audio offsets would take a
/// channel count out of the middle of a reserved run. Restricting the read to
/// entries whose type says "audio" means an unrecognised codec reports
/// unmeasured rather than reporting another codec's numbers.
fn is_audio_entry(kind: [u8; 4]) -> bool {
    matches!(
        kind,
        [b'm', b'p', b'4', b'a']       // AAC
            | [b'a', b'p', b'c', b'h'] // Apple ProRes lossless
            | [b'a', b'p', b'c', b'n'] // Apple ProRes 422
            | [b'a', b'l', b'a', b'c'] // Apple Lossless
            | [b's', b'o', b'w', b't'] // little-endian PCM
            | [b't', b'w', b'o', b's'] // big-endian PCM
            | [b'l', b'p', b'c', b'm'] // linear PCM
            | [b'i', b'n', b'2', b'4'] // 24-bit integer
            | [b'i', b'n', b'3', b'2'] // 32-bit integer
            | [b'f', b'l', b'3', b'2'] // 32-bit float
            | [b'f', b'l', b'6', b'4'] // 64-bit float
            | [b'u', b'l', b'a', b'w'] // unsigned 8-bit PCM
            | [b'O', b'p', b'u', b's'] // Opus
            | [b'f', b'L', b'a', b'C'] // FLAC
            | [b'.', b'm', b'p', b'3'] // MPEG-1/2 audio layer III
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{build_mp4, build_mp4_av, TrackSpec};

    /// The audio stream of an inspected fixture, if it has one.
    fn audio_of(bytes: Vec<u8>) -> Option<tpt_app_media_forensics_model::AudioFormat> {
        crate::inspect_bytes(bytes)
            .expect("the fixture parses")
            .streams
            .into_iter()
            .find(|s| s.kind == tpt_app_media_forensics_model::StreamKind::Audio)
            .and_then(|s| s.audio)
    }

    #[test]
    fn an_av_fixture_declares_the_audio_parameters_its_track_spec_names() {
        // The corpus-level proof: `build_mp4_av` writes an `AudioSampleEntry`,
        // and the reader gets the channel count and sample rate back out of it.
        // Before both existed, `StreamAnalysis::audio` was `None` on every MP4.
        let bytes = build_mp4_av(
            &TrackSpec::video_25fps(320, 240, 50),
            &TrackSpec::audio_48khz_channels(2_400, 6),
            0,
        );
        let format = audio_of(bytes).expect("the track declares an audio format");
        assert_eq!(format.channel_count(), 6);
        assert_eq!(format.sample_rate, 48_000);
        assert_eq!(format.bit_depth, 16);
    }

    #[test]
    fn a_video_track_does_not_pick_up_audio_fields() {
        // A reader that read the visual entry at the audio offsets would find a
        // channel count here, out of the reserved run.
        let bytes = build_mp4(&TrackSpec::video_25fps(320, 240, 50));
        let inspection = crate::inspect_bytes(bytes).expect("the fixture parses");
        let video = inspection
            .streams
            .iter()
            .find(|s| s.kind == tpt_app_media_forensics_model::StreamKind::Video)
            .expect("the file has a video track");
        assert!(
            video.audio.is_none(),
            "a video track must not report an audio format: {:?}",
            video.audio
        );
    }

    #[test]
    fn the_stereo_fixture_declares_two_channels() {
        // Spec §68's example requirement is `audio.channels: 2`, so the ordinary
        // fixture has to be able to satisfy it.
        let bytes = build_mp4_av(
            &TrackSpec::video_25fps(320, 240, 50),
            &TrackSpec::audio_48khz(2_400),
            0,
        );
        let format = audio_of(bytes).expect("an audio format");
        assert_eq!(format.channel_count(), 2);
    }

    #[test]
    fn one_entry_per_track_so_indices_line_up_with_streams() {
        // The vector is parallel to `streams`; dropping a video track would shift
        // every later audio track onto the wrong index.
        let bytes = build_mp4_av(
            &TrackSpec::video_25fps(320, 240, 30),
            &TrackSpec::audio_48khz(2_400),
            0,
        );
        let tracks = parse_track_audio(&bytes);
        assert_eq!(tracks.len(), 2, "one entry per trak, video included");
        assert!(tracks[0].format.is_none(), "the video track declares none");
        assert!(
            tracks[1].format.is_some(),
            "the second trak is the audio one"
        );
    }

    #[test]
    fn a_file_with_no_moov_has_no_audio_declarations() {
        assert!(parse_track_audio(b"not an mp4 at all").is_empty());
        assert!(parse_track_audio(&[]).is_empty());
    }

    #[test]
    fn a_truncated_entry_reports_nothing_rather_than_zeros() {
        // A short entry must not become a channel count of 0 wearing the
        // authority of a declaration.
        let mut body = vec![0u8; 20]; // one field short of the 28-byte prefix
        body[CHANNEL_COUNT_AT..CHANNEL_COUNT_AT + 2].copy_from_slice(&2u16.to_be_bytes());
        body[SAMPLE_SIZE_AT..SAMPLE_SIZE_AT + 2].copy_from_slice(&16u16.to_be_bytes());
        assert!(read_audio_entry(&body).is_none(), "must not be zero-filled");
    }

    #[test]
    fn a_video_sample_entry_is_not_read_at_the_audio_offsets() {
        // The type check has to reject it first, or the channel count comes out
        // of the visual entry's reserved run.
        assert!(!is_audio_entry(*b"avc1"), "avc1 is not an audio entry");
        assert!(is_audio_entry(*b"mp4a"), "mp4a is");
        assert!(is_audio_entry(*b"sowt"), "PCM is");
        assert!(!is_audio_entry(*b"zzzz"), "an unknown fourcc is not");
    }

    #[test]
    fn the_sample_rate_fractional_half_is_discarded_not_rounded() {
        // A 16.16 rate of 48000.0 is the only form a real muxer writes, but a
        // file carrying a fractional value must not be rounded into a rate no
        // real file declares.
        let mut body = vec![0u8; AUDIO_SAMPLE_ENTRY_FIELDS];
        body[CHANNEL_COUNT_AT..CHANNEL_COUNT_AT + 2].copy_from_slice(&2u16.to_be_bytes());
        body[SAMPLE_RATE_AT..SAMPLE_RATE_AT + 4]
            .copy_from_slice(&((48_000u32 << 16) | 0x8000).to_be_bytes());
        let format = read_audio_entry(&body).expect("a complete entry is read");
        assert_eq!(
            format.sample_rate, 48_000,
            "the fraction is dropped, not rounded"
        );
    }
}
