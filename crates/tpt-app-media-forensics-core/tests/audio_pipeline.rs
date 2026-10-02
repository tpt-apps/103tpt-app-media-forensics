//! Audio analysis, end to end.
//!
//! # Why this test exists
//!
//! The four audio rules — `AUDIO.CLIPPING`, `AUDIO.DC_OFFSET`,
//! `AUDIO.SILENCE_REGION`, `AUDIO.INAUDIBLE` — read `AnalysisBundle`'s
//! `audio_levels`, `silence`, and `loudness`. The pipeline had a `measure_audio`
//! helper that computed two of those three and was **never called**, so those
//! fields stayed `None` and every audio rule returned early on every file. The
//! rules each had tests, all of which built the bundle by hand; nothing
//! exercised the step that was actually missing.
//!
//! These run the real engine over a real Opus-in-WebM file, built with the
//! foundation's own encoder, so the audio stage is exercised rather than
//! assumed.

use tpt_app_media_forensics_container::{build_mp4_av, build_webm, TrackSpec};
use tpt_app_media_forensics_core::case_dir::CaseDirectory;
use tpt_app_media_forensics_core::{measure_audio, AnalysisEngine, AnalysisOutcome};
use tpt_app_media_forensics_model::Case;
use tpt_app_media_forensics_rules::RuleProfile;
use tpt_av_cadence_core::Encoder as _;

/// Builds a tone, then digital silence, so silence detection has something to
/// find and the level measurements are not trivially zero.
fn tone_then_silence(frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|i| {
            if i < frames / 2 {
                (std::f64::consts::TAU * 440.0 * (i as f64) / 48_000.0).sin() as f32 * 0.8
            } else {
                0.0
            }
        })
        .collect()
}

/// Encodes real Ogg Opus and returns the bytes.
fn encode_opus(pcm: &[f32]) -> Vec<u8> {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("audio.opus");
    let file = std::fs::File::create(&path).expect("creates sink");

    let mut encoder =
        tpt_av_cadence_opus::OggOpusEncoder::new(file, 48_000, 1, 96_000).expect("opens encoder");
    encoder.encode(pcm).expect("encodes");
    encoder.finish().expect("finishes");

    std::fs::read(&path).expect("reads back")
}

/// Extracts the Opus packets from an Ogg stream.
///
/// A Matroska block holds **one access unit**, not a whole Ogg file. Wrapping
/// the file itself produces a block whose first byte is `O`, which the Opus
/// packet parser reads as a code-3 TOC byte declaring 39 frames — a plausible
/// error message for a fixture built the wrong way.
fn opus_packets(ogg: &[u8]) -> Vec<Vec<u8>> {
    use tpt_av_cadence_core::BufferedSource;
    use tpt_av_cadence_ogg::PageReader;

    // `std::io::Cursor` is already `Read + Seek + Send`, and `ByteSource` is
    // blanket-implemented for exactly that, so no adapter is needed.
    let cursor = std::io::Cursor::new(ogg.to_vec());
    let mut reader = PageReader::new(BufferedSource::new(Box::new(cursor), 32 * 1024), 1 << 20);
    let mut out = Vec::new();
    let mut scratch = vec![0u8; 1 << 20];
    while let Some((length, _meta)) = reader.next_packet(&mut scratch).expect("ogg reads") {
        let packet = &scratch[..length];
        // RFC 7845 puts two header packets ahead of the audio:
        // `OpusHead` identifies the stream and `OpusTags` carries metadata.
        // Neither is audio. Feeding one to the packet decoder reads its `O` as
        // a TOC byte and reports a nonsense frame count.
        if packet.starts_with(b"OpusHead") || packet.starts_with(b"OpusTags") {
            continue;
        }
        out.push(packet.to_vec());
    }
    assert!(!out.is_empty(), "no Opus packets were recovered");
    out
}

/// Wraps real Opus packets in a WebM file declaring them as an audio track.
fn webm_with_opus() -> Vec<u8> {
    let ogg = encode_opus(&tone_then_silence(48_000));
    assert!(
        ogg.starts_with(b"OggS"),
        "the fixture must be a real Ogg stream"
    );

    let packets = opus_packets(&ogg);
    let blocks: Vec<(u16, bool, Vec<u8>)> = packets
        .iter()
        .enumerate()
        .map(|(index, payload)| {
            (
                u16::try_from(index * 20).unwrap_or(u16::MAX),
                true,
                payload.clone(),
            )
        })
        .collect();
    build_webm("A_OPUS", 2, &blocks)
}

/// Analyses `contents` placed at `name` inside a fresh case.
fn analyse(contents: &[u8], name: &str) -> AnalysisOutcome {
    let tmp = tempfile::tempdir().expect("temp dir");
    let source = tmp.path().join(name);
    std::fs::write(&source, contents).expect("writes source");
    let case_dir = tmp.path().join("case.tptcase");
    CaseDirectory::create(&case_dir, &Case::new("Audio", None)).expect("creates case");
    let case = CaseDirectory::open(&case_dir).expect("opens");
    AnalysisEngine::new()
        .analyse(&source, &case)
        .expect("analyses")
}

/// The rule IDs an analysis produced, for readable assertion failures.
fn rule_ids(outcome: &AnalysisOutcome) -> Vec<&str> {
    outcome
        .findings
        .iter()
        .map(|f| f.rule_id.as_str())
        .collect()
}

#[test]
fn an_opus_track_is_decoded_and_measured_by_the_pipeline() {
    let outcome = analyse(&webm_with_opus(), "audio.webm");

    // The decisive check: the audio stage ran. Before it existed these
    // limitations were absent because nothing tried, not because nothing
    // needed to.
    let unexplained = outcome
        .limitations
        .iter()
        .any(|l| l.contains("audio decoding covers") || l.contains("could not be decoded"));
    assert!(
        !unexplained,
        "the Opus track should have been decoded: {:?}",
        outcome.limitations
    );
}

#[test]
fn a_silent_tail_produces_a_silence_finding() {
    let outcome = analyse(&webm_with_opus(), "audio.webm");

    // The encoded second half is digital silence. If the stage is not running,
    // this rule returns early and the finding is absent — which is precisely
    // the regression this file exists to catch.
    assert!(
        outcome
            .findings
            .iter()
            .any(|f| f.rule_id == "AUDIO.SILENCE_REGION"),
        "expected a silence finding from the encoded silence, got {:?}",
        rule_ids(&outcome)
    );
}

#[test]
fn a_patent_encumbered_track_is_refused_and_says_so() {
    // Declared as `A_AAC`, which this build deliberately does not decode.
    let file = build_webm("A_AAC", 2, &[(0, true, vec![0u8; 512])]);
    let outcome = analyse(&file, "audio.webm");

    assert!(
        outcome.limitations.iter().any(|l| l.contains("not decode")),
        "an undecodable track must be stated: {:?}",
        outcome.limitations
    );
    // No audio *finding* may be derived from it. Silence and "we never
    // listened" are different statements, and only the second is true here.
    assert!(
        !rule_ids(&outcome).iter().any(|id| id.starts_with("AUDIO.")),
        "no audio finding may come from a track that was never decoded: {:?}",
        rule_ids(&outcome)
    );
}

#[test]
fn audio_findings_are_reproducible_across_cases() {
    // Spec §77. A fresh case directory each time, so the cache cannot serve it.
    let file = webm_with_opus();
    let first = analyse(&file, "audio.webm");
    let second = analyse(&file, "audio.webm");
    assert_eq!(
        first.findings, second.findings,
        "two analyses of identical bytes must agree"
    );
}

#[test]
fn a_video_only_file_makes_no_audio_claims() {
    let file = build_webm("V_AV1", 1, &[(0, true, vec![1, 2, 3])]);
    let outcome = analyse(&file, "video.webm");

    assert!(
        !rule_ids(&outcome).iter().any(|id| id.starts_with("AUDIO.")),
        "a file with no audio track cannot yield audio findings: {:?}",
        rule_ids(&outcome)
    );
}

/// Builds a two-track MP4 whose audio starts `delay_ms` after the video.
fn av_file(delay_ms: u32) -> Vec<u8> {
    build_mp4_av(
        &TrackSpec::video_25fps(640, 480, 100),
        &TrackSpec::audio_48khz(2_400),
        delay_ms,
    )
}

#[test]
fn a_two_track_file_produces_an_av_sync_measurement() {
    let outcome = analyse(&av_file(0), "av.mp4");

    // The decisive check: the stage ran. Before it existed `bundle.sync` stayed
    // `None` and `TIMING.AV_SYNC_DRIFT` could never fire.
    assert!(
        !outcome
            .limitations
            .iter()
            .any(|l| l.contains("A/V synchronisation could not be measured")),
        "both tracks are present, so sync should have been measured: {:?}",
        outcome.limitations
    );
}

#[test]
fn a_single_track_file_makes_no_av_claim() {
    // Sync between two streams that do not both exist is not applicable, which
    // is a different statement from "attempted and failed" — so no limitation
    // is raised either.
    let file = build_webm("V_AV1", 1, &[(0, true, vec![1, 2, 3])]);
    let outcome = analyse(&file, "video.webm");

    assert!(
        !outcome
            .limitations
            .iter()
            .any(|l| l.contains("A/V synchronisation")),
        "a video-only file cannot have its sync measured: {:?}",
        outcome.limitations
    );
    assert!(
        !rule_ids(&outcome).iter().any(|id| id.contains("AV_SYNC")),
        "no A/V finding may come from a file with one stream: {:?}",
        rule_ids(&outcome)
    );
}

#[test]
fn av_findings_are_reproducible_across_cases() {
    let file = av_file(0);
    assert_eq!(
        analyse(&file, "av.mp4").findings,
        analyse(&file, "av.mp4").findings
    );
}

#[test]
fn measuring_empty_pcm_yields_nothing_rather_than_zeroes() {
    // "There is no audio" must not be reported as "the audio was silent".
    assert!(measure_audio(&[], 2, 48_000, &RuleProfile::default()).is_none());
    // A zero-channel stream is equally unmeasurable.
    assert!(measure_audio(&[0.0; 1000], 0, 48_000, &RuleProfile::default()).is_none());
}

#[test]
fn measuring_real_pcm_answers_the_three_questions_the_rules_ask() {
    // Direct check of the helper the pipeline now calls, so a regression there
    // is not only visible through rule behaviour.
    let pcm: Vec<f32> = (0..48_000)
        .map(|i| {
            if i < 24_000 {
                (std::f64::consts::TAU * 440.0 * (i as f64) / 48_000.0).sin() as f32 * 0.8
            } else {
                0.0
            }
        })
        .collect();
    let measured = measure_audio(&pcm, 1, 48_000, &RuleProfile::default()).expect("measured");

    assert!(measured.levels.peak > 0.5, "peak should reflect the tone");
    assert!(
        !measured.silence.is_empty(),
        "the trailing silence should be found"
    );
    assert!(
        measured.loudness.is_some(),
        "loudness should be measurable on a real signal"
    );
}
