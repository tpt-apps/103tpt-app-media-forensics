//! # The failure this exists to prevent
//!
//! Two analysis stages shipped that were fully implemented, documented, and
//! unit-tested, and that no test could catch:
//!
//! - `measure_audio` computed exactly what `AUDIO.CLIPPING`, `AUDIO.DC_OFFSET`,
//!   `AUDIO.SILENCE_REGION`, and `AUDIO.INAUDIBLE` read — and was never called.
//! - `av_sync::analyse` computed exactly what `TIMING.AV_SYNC_DRIFT` read — and
//!   was never called.
//!
//! Six rules could not fire on any file, over any number of passing tests. Each
//! analyser had tests; nothing tested the *composition*. The analyser was
//! healthy and its caller was missing, which is precisely the class of defect a
//! unit test cannot see.
//!
//! # What the guard asserts
//!
//! Every rule declares the analysis it reads via
//! [`ForensicRule::required_inputs`]. This test runs the real pipeline over a
//! corpus whose fixtures between them exercise every stage, then asserts that
//! each declared input was actually populated by at least one of them.
//!
//! An input no fixture populates means either the stage is unwired or the
//! corpus has lost the fixture that exercised it. Both are bugs, and the
//! message names which rules are affected.

use tpt_app_media_forensics_container::{
    build_mp4, build_mp4_av, build_mp4_empty_moov, build_mp4_stsd_gop_change,
    build_mp4_with_bitrate_drop, build_mp4_with_colour, build_mp4_with_declared_track_mismatch,
    build_mp4_with_frame_rate_change, build_mp4_with_hdr_signalling_only, build_mp4_with_keyframes,
    build_mp4_with_reordered_frames, build_mp4_with_repeated_frames,
    build_mp4_with_wrong_declared_duration, build_webm, build_webm_without_duration, TrackSpec,
};
use tpt_app_media_forensics_core::case_dir::CaseDirectory;
use tpt_app_media_forensics_core::AnalysisEngine;
use tpt_app_media_forensics_model::Case;
use tpt_app_media_forensics_rules::{builtin_rules, BundleInput, RuleProfile};
// Needed by `webm_with_opus`, which is a free function rather than part of the
// corpus body — hence here rather than inside `corpus`.
use tpt_av_cadence_core::Encoder as _;

/// Inputs this build knowingly cannot populate.
///
/// Empty. Every one of the ten inputs is now populated by the corpus, and this
/// list exists so that a future gap has to be written down as a decision rather
/// than discovered as a silence.
///
/// It last held `RepeatedRuns`, on the belief that no fixture produced a
/// repeated compressed run. That was wrong twice over: the fixtures were not
/// lacking, they were producing a 60-frame run each by accident, because their
/// `mdat` was all zeros. A list like this can hide a defect as easily as it can
/// record a limitation, which is why removing the entry was the point.
const KNOWN_UNREACHABLE: &[(BundleInput, &[&str], &str)] = &[];

/// One second of a 440 Hz tone at `amplitude`.
fn tone(amplitude: f32) -> Vec<f32> {
    (0..48_000)
        .map(|i| {
            (std::f64::consts::TAU * 440.0 * (f64::from(i)) / 48_000.0).sin() as f32 * amplitude
        })
        .collect()
}

/// A 440 Hz tone for half a second, then digital silence.
///
/// The tone gives the encoder something to work with before the silence begins.
/// An all-zero signal tends to be coded as one silent frame, which would leave
/// `AUDIO.SILENCE_REGION` with nothing but silence to find.
fn tone_then_silence() -> Vec<f32> {
    let mut samples = tone(0.8);
    samples[24_000..].fill(0.0);
    samples
}

/// A square wave: the signal that actually clips.
///
/// A sine at full scale spends most of its time well below its peak, so it is a
/// poor way to reach a peak threshold — every sample has to be pushed over by
/// decoder ringing. A square wave sits at its extreme for half of every cycle.
fn square_wave(amplitude: f32) -> Vec<f32> {
    (0..48_000)
        .map(|i| if i % 100 < 50 { amplitude } else { -amplitude })
        .collect()
}

/// A sine with a constant `offset` added to every sample.
///
/// A DC offset is a constant bias across the whole waveform, so the mean of the
/// signal *is* that bias. The tone underneath keeps the file from being pure
/// silence, which a decoder may legitimately collapse to zero.
fn biased_tone(offset: f32) -> Vec<f32> {
    tone(0.3).into_iter().map(|s| s + offset).collect()
}

/// Encodes `samples` as Opus and wraps the result in a WebM file.
///
/// The round trip is real in both directions — a real encoder, real Ogg pages,
/// real container — because the rules this serves read *decoded* levels. A stub
/// payload would parse as an audio track and then decode to nothing, which is a
/// different thing from the analysers running over real samples.
///
/// Every audio fixture goes through this helper, so the amplitude extremes below
/// differ only in signal and never in how they were packaged. A fixture that
/// reached its threshold because of its container rather than its samples would
/// be measuring the wrong thing.
fn webm_with_opus(dir: &std::path::Path, samples: &[f32]) -> Vec<u8> {
    let opus_dir = dir.join("opus");
    std::fs::create_dir_all(&opus_dir).expect("creates opus dir");
    let opus_path = opus_dir.join("fixture.opus");
    let mut encoder = tpt_av_cadence_opus::OggOpusEncoder::new(
        std::fs::File::create(&opus_path).expect("creates sink"),
        48_000,
        1,
        96_000,
    )
    .expect("opens encoder");
    encoder.encode(samples).expect("encodes");
    encoder.finish().expect("finishes");

    let ogg = std::fs::read(&opus_path).expect("reads opus");
    let mut packets = Vec::new();
    {
        use tpt_av_cadence_core::BufferedSource;
        use tpt_av_cadence_ogg::PageReader;
        let mut reader = PageReader::new(
            BufferedSource::new(Box::new(std::io::Cursor::new(ogg)), 32 * 1024),
            1 << 20,
        );
        let mut scratch = vec![0u8; 1 << 20];
        while let Some((n, _)) = reader.next_packet(&mut scratch).expect("ogg reads") {
            let packet = &scratch[..n];
            // `OpusHead`/`OpusTags` are headers, not audio.
            if packet.starts_with(b"OpusHead") || packet.starts_with(b"OpusTags") {
                continue;
            }
            packets.push(packet.to_vec());
        }
    }
    let blocks: Vec<(u16, bool, Vec<u8>)> = packets
        .iter()
        .enumerate()
        .map(|(i, p)| (u16::try_from(i * 20).unwrap_or(u16::MAX), true, p.clone()))
        .collect();
    build_webm("A_OPUS", 2, &blocks)
}

/// Builds a corpus of fixtures written to `dir`, and returns their paths.
/// Builds a corpus of fixtures written to `dir`, and returns their paths.
fn corpus(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut written = Vec::new();
    let mut put = |name: &str, bytes: &[u8]| {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("writes fixture");
        written.push(path);
    };

    // Container, GOP, and timestamps: a plain video track.
    put(
        "video.mp4",
        &build_mp4(&TrackSpec::video_25fps(640, 480, 50)),
    );

    // Repeated compressed samples: a frozen stretch of video. This is the only
    // way `VIDEO.DUPLICATE_FRAME_RUN` gets exercised, because every other
    // fixture gives each frame its own bytes and so has no repeated run to find.
    put(
        "repeated-frames.mp4",
        &build_mp4_with_repeated_frames(10, 15),
    );

    // Colour: an HDR-signalled file carrying no mastering-display or
    // content-light metadata. Only `VIDEO.HDR_METADATA_MISSING` can be reached
    // by it, and it needs a `colr` box written into the sample entry — which no
    // other fixture has.
    put(
        "hdr-signalling-only.mp4",
        &build_mp4_with_hdr_signalling_only(),
    );
    // And a complete BT.709 file, so the colour rules are also checked against a
    // file that declares colour and has nothing missing.
    put("sdr-colour.mp4", &build_mp4_with_colour());

    // Metadata: a track carrying text atoms.
    put(
        "metadata.mp4",
        &tpt_app_media_forensics_container::fixture::build_mp4_with_metadata(
            &TrackSpec::video_25fps(640, 480, 50),
        ),
    );

    // A/V sync: two tracks, deliberately offset.
    put(
        "av.mp4",
        &build_mp4_av(
            &TrackSpec::video_25fps(640, 480, 100),
            &TrackSpec::audio_48khz(2_400),
            40,
        ),
    );

    // Audio levels, silence, and loudness: real Opus, tone then silence.
    put("audio.webm", &webm_with_opus(dir, &tone_then_silence()));

    // A WebM file that genuinely omits `Segment > Info > Duration`.
    //
    // Every other WebM fixture declares one, as real muxers do. This is the file
    // `CONTAINER.STREAM_DURATION_MISSING` is actually for, and it could not exist
    // in the corpus before: the builder wrote no duration at all, so the rule
    // fired on all five WebM fixtures. That was a correct finding about an
    // unusual fixture, and it meant the rule was never observed on a file that
    // omitted duration *by design*.
    //
    // Carries several distinct blocks with differing payloads so it reaches no
    // *other* condition: one block would be trivially a single-keyframe track, and
    // identical payloads would be a duplicate-frame run. A fixture testing one
    // condition must not incidentally test another, or the corpus stops being
    // able to tell which finding came from which defect.
    let blocks: Vec<(u16, bool, Vec<u8>)> = (0..8u16)
        .map(|i| {
            let mut payload = vec![0u8; 32];
            payload[0] = i as u8;
            payload[1] = (i as u8).wrapping_mul(7);
            (i * 40, i % 4 == 0, payload)
        })
        .collect();
    put(
        "no-duration.webm",
        &build_webm_without_duration("V_VP9", 1, &blocks),
    );

    // The three amplitude extremes the audio rules read.
    //
    // `audio.webm` is a tone at 0.8 followed by digital silence, so it exercises
    // `AUDIO.SILENCE_REGION` and leaves `AUDIO.CLIPPING`, `AUDIO.DC_OFFSET`, and
    // `AUDIO.INAUDIBLE` silent: 0.8 is below the 0.999 clipping threshold, a sine
    // has a mean of zero, and 0.8 is far above the -70 LUFS floor. Those three
    // were on the unexercised list for that reason alone — not because the
    // analysis was missing, but because no file reached the thresholds.
    //
    // Each signal is pushed well past its threshold rather than to it. Opus is
    // lossy and does not reproduce an input sample-for-sample, so a value sitting
    // exactly on a threshold would be a coin flip after encoding.
    put(
        "audio-clipped.webm",
        &webm_with_opus(dir, &square_wave(1.0)),
    );
    put(
        "audio-dc-offset.webm",
        &webm_with_opus(dir, &biased_tone(0.2)),
    );
    // Amplitude 1e-4 is roughly -80 dBFS: far below the -70 LUFS floor, and well
    // clear of digital silence, so the file carries real audio that happens to be
    // too quiet to hear. That is the case `AUDIO.INAUDIBLE` names.
    put("audio-inaudible.webm", &webm_with_opus(dir, &tone(1.0e-4)));

    // Tier-2: real AV1 in a real WebM container, which is the only way to reach
    // the scene-change and near-duplicate analysers. Stub payloads parse as a
    // video track and then fail in the decoder, which is a different thing from
    // the analysers running.
    put("video.av1.webm", &av1_webm(dir));

    // Structural damage (spec §30): a well-formed MP4 with its `mdat` cut short.
    //
    // Built by truncation rather than as its own fixture builder, because the
    // defect *is* the absence of bytes and a builder that emitted the damage
    // directly would be asserting the result rather than producing the cause.
    // The remaining bytes are exactly what a failed copy leaves behind: a valid
    // header and sample tables describing media that is not all there.
    //
    // This is the only way `BundleInput::Damage` is populated. Without it the
    // two damage rules are registered, documented, and unfireable — the shape
    // this guard was written to catch.
    let mut truncated = build_mp4(&TrackSpec::video_25fps(640, 480, 50));
    truncated.truncate(truncated.len() / 2);
    put("truncated.mp4", &truncated);

    // Bitrate drop (§28-§29). Every other fixture gives each sample the same
    // 100 bytes, so the corpus as a whole holds one flat bitrate and
    // `VIDEO.BITRATE_DROP` cannot fire on any of them.
    //
    // This is the same defect `repeated_frames` was added for, one analysis layer
    // up, and it is worth naming: the stage guard confirmed `BundleInput::Bitrate`
    // was populated, because the report *is* produced. Populated is not the same
    // as *interesting*. A rule can be reachable, wired, declared, documented, and
    // still be unfireable on every file the project owns.
    put("bitrate-drop.mp4", &build_mp4_with_bitrate_drop(60, 40));

    // Structural defects that are *declared* rather than inflicted. Each of
    // these reaches a rule that `truncated.mp4` alone does not: that one damages
    // the bytes, these damage the tables that describe them.
    put("empty-moov.mp4", &build_mp4_empty_moov());
    put("gop-change.mp4", &build_mp4_stsd_gop_change());
    put(
        "single-keyframe.mp4",
        &build_mp4_with_keyframes(&TrackSpec::video_25fps(320, 240, 60), &[0]),
    );
    put(
        "all-keyframes.mp4",
        &tpt_app_media_forensics_container::fixture::build_mp4_without_stss(
            &TrackSpec::video_25fps(320, 240, 60),
        ),
    );

    // Bytes the container does not account for.
    //
    // `truncated.mp4` produces a `Truncated` defect, which
    // `CONTAINER.STRUCTURAL_DEFECT` deliberately excludes: it reports only
    // *missing* media there, because the declared content may still be intact.
    // This fixture is the complement — data present that nothing describes — and
    // is what reaches the structural rule.
    //
    // Built inline rather than through a fixture builder because the defect is
    // the surplus: a builder would have to assert how much to append to be
    // *wrong*, which is the opposite of what a fixture should encode.
    let mut trailing = build_mp4(&TrackSpec::video_25fps(320, 240, 30));
    trailing.extend_from_slice(b"payload a real muxer never writes");
    put("trailing-data.mp4", &trailing);

    // A cadence change mid-track, which is both a frame-rate change and a gap in
    // presentation timestamps. One fixture, two rules: doubling the frame
    // duration necessarily leaves the later samples further apart in time.
    put(
        "frame-rate-change.mp4",
        &build_mp4_with_frame_rate_change(30, 30),
    );

    // An `mdhd` duration the `stts` table does not support.
    //
    // `declared_duration` on `TrackSpec` already existed for exactly this, and
    // the container's own unit tests use it. It had no corpus entry for the same
    // reason the builders above did: it was reachable but never exercised
    // end to end.
    put(
        "wrong-duration.mp4",
        &build_mp4_with_wrong_declared_duration(),
    );

    // A header claiming three tracks, with one `trak` box in the file.
    //
    // This is the only fixture that can reach `CONTAINER.DECLARED_TRACK_MISMATCH`.
    // The rule compares what the container declared against what was recovered,
    // and until `declared_track_count` was read from the boxes instead of from
    // the demuxer's own track list, the two sides were the same number by
    // construction — unfireable on any file, not merely unexercised.
    put(
        "track-mismatch.mp4",
        &build_mp4_with_declared_track_mismatch(4),
    );

    // A track whose presentation order differs from its decode order, via `ctts`.
    //
    // The last rule in the corpus to gain a file, and the one that needed the most
    // to get there: timestamps were previously decode time, which `stts` builds
    // from unsigned deltas and is therefore monotonic by construction. Reading
    // composition offsets is what makes presentation order observable at all.
    put("reordered.mp4", &build_mp4_with_reordered_frames(40));

    written
}

/// Encodes real AV1 frames and wraps them in a WebM container.
fn av1_webm(dir: &std::path::Path) -> Vec<u8> {
    use tpt_kinetix_av1::{Av1Encoder, Av1EncoderConfig};
    use tpt_kinetix_core::frame::VideoFrame;
    use tpt_kinetix_core::pixel_format::PixelFormat;
    use tpt_kinetix_core::timestamp::Timestamp;

    const W: u32 = 64;
    const H: u32 = 48;

    let mut encoder = Av1Encoder::new(&Av1EncoderConfig {
        width: W,
        height: H,
        bitrate: 0,
        quantizer: 80,
        speed: 10,
        keyframe_interval: 8,
    })
    .expect("opens AV1 encoder");

    let mut payloads = Vec::new();
    for index in 0..8u8 {
        let w = W as usize;
        let h = H as usize;
        let mut data = vec![0u8; w * h + (w * h) / 2];
        for y in 0..h {
            for x in 0..w {
                // A moving gradient keeps the image non-uniform, so the
                // analysers see structure rather than a flat field.
                data[y * w + x] = ((index as usize * 30) + x + y) as u8;
            }
        }
        for sample in data.iter_mut().skip(w * h) {
            *sample = 128;
        }
        let ts = Timestamp::new(i64::from(index) * 33, (1, 1_000));
        let frame = VideoFrame {
            pts: ts,
            dts: ts,
            data,
            width: W,
            height: H,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: true,
        };
        if let Some(packet) = encoder.encode_frame(&frame).expect("encodes") {
            payloads.push(packet.data);
        }
    }
    payloads.extend(
        encoder
            .flush()
            .expect("flushes")
            .into_iter()
            .map(|p| p.data),
    );
    assert!(!payloads.is_empty(), "the AV1 encoder produced nothing");

    let blocks: Vec<(u16, bool, Vec<u8>)> = payloads
        .iter()
        .enumerate()
        .map(|(i, p)| (u16::try_from(i * 33).unwrap_or(u16::MAX), true, p.clone()))
        .collect();
    let _ = dir;
    build_webm("V_AV1", 1, &blocks)
}

/// Inputs no rule requires, and so nothing is obliged to populate.
fn unrequired_inputs() -> Vec<BundleInput> {
    let required: Vec<BundleInput> = builtin_rules()
        .iter()
        .flat_map(|rule| rule.required_inputs().iter().copied())
        .collect();
    BundleInput::ALL
        .iter()
        .copied()
        .filter(|input| !required.contains(input))
        .collect()
}

#[test]
fn every_rule_input_is_populated_by_some_pipeline_run() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let fixtures = corpus(tmp.path());
    let engine = AnalysisEngine::new();

    // Which stages actually ran, across the whole corpus.
    let mut populated: Vec<BundleInput> = Vec::new();
    for path in &fixtures {
        let (bundle, _limitations) = engine.observe_stages(path);
        for input in BundleInput::ALL {
            if input.is_populated(&bundle) && !populated.contains(input) {
                populated.push(*input);
            }
        }
    }

    let unreachable: Vec<BundleInput> = KNOWN_UNREACHABLE.iter().map(|(i, _, _)| *i).collect();
    let missing: Vec<String> = builtin_rules()
        .iter()
        .flat_map(|rule| {
            rule.required_inputs()
                .iter()
                .filter(|input| !populated.contains(input) && !unreachable.contains(input))
                .map(move |input| {
                    format!(
                        "{} needs `{}`, which no fixture populated",
                        rule.id(),
                        input.tag()
                    )
                })
        })
        .collect();

    assert!(
        missing.is_empty(),
        "a rule reads analysis that the pipeline never produced.\n\
         These stages are unwired, or a fixture that exercised them has gone:\n  {}\n\
         Populated by the corpus: {:?}\n\
         Add the stage call, or add the fixture that proves it runs.",
        missing.join("\n  "),
        populated.iter().map(|i| i.tag()).collect::<Vec<_>>()
    );
}

#[test]
fn the_corpus_populates_the_inputs_the_rules_rely_on() {
    // The guard above is only as good as its corpus. This asserts the corpus
    // directly, so a fixture silently losing its effect is caught separately
    // from a stage being unwired.
    let tmp = tempfile::tempdir().expect("temp dir");
    let fixtures = corpus(tmp.path());
    let engine = AnalysisEngine::new();

    let mut populated: Vec<BundleInput> = Vec::new();
    for path in &fixtures {
        let (bundle, _limitations) = engine.observe_stages(path);
        for input in BundleInput::ALL {
            if input.is_populated(&bundle) && !populated.contains(input) {
                populated.push(*input);
            }
        }
    }

    let expected = [
        BundleInput::Container,
        BundleInput::Gop,
        BundleInput::Timestamps,
        BundleInput::Sync,
        BundleInput::AudioLevels,
        BundleInput::Silence,
        BundleInput::Loudness,
    ];
    let missing: Vec<&str> = expected
        .iter()
        .filter(|i| !populated.contains(i))
        .map(|i| i.tag())
        .collect();
    assert!(
        missing.is_empty(),
        "the corpus no longer exercises: {:?}. The fixtures have stopped producing \
         these measurements, which would leave the guard above vacuous.",
        missing
    );
}

/// The rule implementations, as source text.
///
/// Source analysis is the only way to catch an *under*-declared rule: calling
/// `evaluate` cannot tell you what it would have read had the bundle been
/// fuller, and the engine has no reflection to ask.
const RULES_SOURCE: &str = include_str!("../../tpt-app-media-forensics-rules/src/builtin.rs");

/// The bundle definition, read so a new field cannot be added without a way to
/// declare it. Derived rather than hard-coded, so this check extends itself.
const ENGINE_SOURCE: &str = include_str!("../../tpt-app-media-forensics-rules/src/engine.rs");

/// Names a [`BundleInput`] variant.
///
/// Written as an exhaustive `match` on purpose: adding a variant breaks this
/// function's compilation, which is the intended prompt to extend the guard
/// rather than let a new input slip past it undeclared.
fn input_name(input: BundleInput) -> &'static str {
    match input {
        BundleInput::Container => "Container",
        BundleInput::Gop => "Gop",
        BundleInput::RepeatedRuns => "RepeatedRuns",
        BundleInput::Timestamps => "Timestamps",
        BundleInput::Sync => "Sync",
        BundleInput::AudioLevels => "AudioLevels",
        BundleInput::Silence => "Silence",
        BundleInput::Loudness => "Loudness",
        BundleInput::Scene => "Scene",
        BundleInput::NearDuplicates => "NearDuplicates",
        BundleInput::Metadata => "Metadata",
        BundleInput::Damage => "Damage",
        BundleInput::SampleIndex => "SampleIndex",
        BundleInput::Bitrate => "Bitrate",
    }
}

/// `repeated_runs` -> `RepeatedRuns`, the naming the two types share.
fn input_name_for_field(field: &str) -> String {
    field
        .split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// Blanks out comments and string literals, preserving byte offsets.
///
/// Slices the original string rather than rebuilding it character by character,
/// so multi-byte characters outside comments survive and every index stays on a
/// character boundary.
fn code_only(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::new();
    let mut seg_start = 0;
    let mut i = 0;
    let mut in_string = false;

    while i < bytes.len() {
        if in_string {
            if bytes[i] == b'\\' {
                i += 2;
            } else {
                if bytes[i] == b'"' {
                    in_string = false;
                }
                i += 1;
            }
            continue;
        }
        match bytes[i] {
            b'"' => {
                out.push_str(&src[seg_start..i]);
                out.push(' ');
                in_string = true;
                i += 1;
                seg_start = i;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                out.push_str(&src[seg_start..i]);
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                seg_start = i;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                out.push_str(&src[seg_start..i]);
                out.push(' ');
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                seg_start = i;
            }
            _ => i += 1,
        }
    }
    out.push_str(&src[seg_start..]);
    out
}

/// The extent of the brace-delimited block starting at the first `{` at or
/// after `from`, as `(start, end_inclusive)` byte offsets into `src`.
///
/// Both ends land on ASCII braces, so callers can slice without splitting a
/// multi-byte character.
fn block_extent(src: &str, from: usize) -> (usize, usize) {
    let bytes = src.as_bytes();
    let mut start = from;
    while start < bytes.len() && bytes[start] != b'{' {
        start += 1;
    }
    let mut depth = 0i32;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return (start, i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    (start, bytes.len().saturating_sub(1))
}

/// Every `impl ForensicRule for ... { ... }` body in `src`.
fn rule_bodies(src: &str) -> Vec<&str> {
    const MARKER: &str = "impl ForensicRule for ";
    let mut bodies = Vec::new();
    let mut cursor = 0;

    while let Some(offset) = src[cursor..].find(MARKER) {
        let (start, end) = block_extent(src, cursor + offset + MARKER.len());
        bodies.push(&src[start..=end]);
        cursor = end + 1;
    }
    bodies
}

/// The rule id a source block declares, leaked to `'static` so it can be
/// compared against the real rule objects rather than a re-parse of the enum.
///
/// Reads the raw source, not the comment-stripped view: the id *is* a string
/// literal, and blanking literals would erase the one thing being looked up.
fn id_in(block: &str) -> &'static str {
    let after = &block[block.find("fn id(").expect("every rule defines id") + "fn id(".len()..];
    let start = after.find('"').expect("an id string") + 1;
    let rest = &after[start..];
    let end = rest.find('"').expect("a closed id string");
    Box::leak(rest[..end].to_owned().into_boxed_str())
}

/// Bundle fields a rule body reads.
///
/// Matches `bundle`, whitespace, `.`, whitespace, name — so the rustfmt-idiomatic
/// `bundle\n    .field` is found as readily as `bundle.field`.
fn read_fields(block: &str) -> std::collections::BTreeSet<String> {
    let code = code_only(block);
    let bytes = code.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut found = std::collections::BTreeSet::new();
    let mut i = 0;

    while let Some(offset) = code[i..].find("bundle") {
        let start = i + offset;
        i = start + "bundle".len();
        if start > 0 && ident(bytes[start - 1]) {
            continue; // part of a longer identifier
        }
        let mut j = start + "bundle".len();
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if bytes.get(j) != Some(&b'.') {
            continue; // `bundle` passed along whole, not read through
        }
        j += 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let name_start = j;
        while j < bytes.len() && ident(bytes[j]) {
            j += 1;
        }
        if j > name_start {
            found.insert(code[name_start..j].to_owned());
        }
    }
    found
}

#[test]
fn every_rule_declares_the_bundle_fields_it_reads() {
    let rules = builtin_rules();
    let blocks = rule_bodies(RULES_SOURCE);

    // Without this the test could pass by scanning nothing, which is the trap
    // this whole file exists to avoid.
    assert_eq!(
        blocks.len(),
        rules.len(),
        "the source scan found {} rule bodies but {} rules are registered; the \
         scanner has drifted from the source layout",
        blocks.len(),
        rules.len()
    );

    // Accumulated rather than asserted per rule, so one run names every
    // offender. Failing on the first would make a three-rule mistake a
    // three-run fix, and the author would have to guess at the other two.
    let mut failures: Vec<String> = Vec::new();

    for rule in &rules {
        let block = blocks
            .iter()
            .find(|b| id_in(b) == rule.id())
            .unwrap_or_else(|| panic!("no source block found for {}", rule.id()));

        let declared: Vec<&str> = rule
            .required_inputs()
            .iter()
            .copied()
            .map(input_name)
            .collect();
        let mut undeclared: Vec<String> = read_fields(block)
            .into_iter()
            .filter(|field| !declared.contains(&input_name_for_field(field).as_str()))
            .collect();
        undeclared.sort();

        if !undeclared.is_empty() {
            failures.push(format!(
                "{} reads {} but declared {declared:?}",
                rule.id(),
                undeclared.join(", ")
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "rules read an analysis they do not declare:\n  {}\n\nA rule that reads an \
         analysis it did not declare is skipped whenever the engine cannot prove that \
         input was produced, so it fails silently.",
        failures.join("\n  "),
    );
}

#[test]
fn every_bundle_field_has_an_input_a_rule_can_declare() {
    // Closes the other direction. A field with no matching `BundleInput` could be
    // read by a rule with no way to declare the dependency, leaving the guard
    // above with nothing to check.
    let code = code_only(ENGINE_SOURCE);
    let header = code
        .find("pub struct AnalysisBundle")
        .expect("the bundle exists");
    // Bounded to the struct itself. Scanning onward from the header would walk
    // into `fn new` and read its parameters as though they were fields.
    let (start, end) = block_extent(&code, header);
    let body = &code[start..=end];

    let mut checked = 0;
    for line in body.lines() {
        let Some(rest) = line.trim().strip_prefix("pub ") else {
            continue;
        };
        let Some((field, ty)) = rest.split_once(':') else {
            continue;
        };

        // Only an analysis can be *absent*, and only an analysis is worth
        // declaring. `asset_id` is always present and identifies the asset; it
        // is not a stage that could have failed to run, so it has no input to
        // declare and demanding one would be asking for a fiction.
        let ty = ty.trim().trim_end_matches(',').trim();
        if !ty.starts_with("Option<") && !ty.starts_with("Vec<") {
            continue;
        }

        let name = input_name_for_field(field);
        assert!(
            BundleInput::ALL.iter().any(|i| input_name(*i) == name),
            "AnalysisBundle::{field} holds {ty}, so it can be absent, but it has no \
             BundleInput variant. A rule reading it could not declare the dependency, so \
             add a variant and wire it through the pipeline."
        );
        checked += 1;
    }
    assert!(
        checked >= 11,
        "only {checked} optional bundle fields were seen; the scanner has drifted"
    );
}

/// Rules that no fixture triggers end to end, and which no fixture can.
///
/// **Currently empty** — every rule fires from a file this project owns. See
/// [`NO_END_TO_END_FIXTURE`] for the route there, and keep the mechanism rather
/// than the constant: a new rule that cannot fire will fail the guard below.
///
/// Not "untested". Every rule listed here is exercised by
/// `-rules/tests/new_rules.rs` against a hand-assembled `AnalysisBundle`, and
/// `required_inputs` is declared and checked by the guards above. What none of
/// them has is a file: every test builds the state its rule reads rather than
/// deriving it from bytes on disk.
///
/// That is a narrower gap than it looks, and the difference matters. A
/// hand-built bundle can drift from what the pipeline actually produces, and
/// nothing here would notice. The guards close the worst version of that — an
/// unwired stage is caught — but they cannot catch a stage that is wired and
/// populates a slightly different shape.
///
/// The list is here so that a *newly* unfireable rule fails this test rather
/// than joining it quietly.
///
/// # Now empty
///
/// Every rule fires end to end from a file this project owns. It began at
/// fifteen, and the route there was worth recording:
///
/// - **Ten** were builders that already existed and had never been written to
///   disk, or signals the corpus's existing Opus path could already produce. No
///   new analysis.
/// - **Two** (`CONTAINER.DECLARED_TRACK_MISMATCH`, `TIMING.NON_MONOTONIC_PTS`)
///   turned out not to be fixture problems at all. Both could not fire on *any*
///   file, which is a materially different claim from "not yet exercised":
///   `declared_track_count` came from the demuxer's own track list and so equalled
///   `streams.len()` by construction, and frame timestamps were decode time, which
///   `stts` builds from unsigned deltas and is therefore monotonic by
///   construction. Both needed a *reader* — `mvhd`, then `ctts` — rather than a
///   fixture.
/// - **Three** (`CONTAINER.STRUCTURAL_DEFECT` and friends) needed fixtures that
///   reach a different condition from the one already present, not louder
///   instances of it.
///
/// An entry added to this list should say which of those three shapes it is.
/// "No fixture yet" and "no fixture could exist" call for different work, and
/// conflating them is how a rule stays unfireable while looking merely untested.
const NO_END_TO_END_FIXTURE: &[&str] = &[];

#[test]
fn every_rule_fires_end_to_end_or_is_recorded_as_unexercised() {
    // The general form of the check that caught `VIDEO.BITRATE_DROP`.
    //
    // Every other guard in this file asks whether a rule's *inputs* are
    // reachable. None asks whether the rule can produce a finding from a real
    // file. Those are different questions, and a rule can satisfy the first and
    // fail the second: a bitrate report describing a file whose bitrate never
    // varies is perfectly populated and perfectly silent.
    //
    // `VIDEO.BITRATE_DROP` shipped in exactly that state — every fixture gave
    // every sample the same size, so nothing the project owns could trigger it,
    // and no test could tell. A rule that can never fire can only be tested by
    // asserting it finds nothing, which is indistinguishable from a rule that is
    // correct on a corpus lacking the condition.
    let dir = tempfile::tempdir().expect("temp dir");
    let engine = AnalysisEngine::new();

    let mut fired: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for path in corpus(dir.path()) {
        // Every fixture is evaluated, whatever its container. Filtering by format
        // would hide the rules whose only triggering fixtures are WebM — the
        // audio rules and the Tier-2 scene/near-duplicate pair — and report them
        // as inert when they are merely unexercised here.
        //
        // A rule whose analysis is absent for a file returns empty for that
        // file, which is correct behaviour and not a false negative: the question
        // is whether *any* fixture makes the rule fire.
        let (bundle, _) = engine.observe_stages(&path);
        for rule in builtin_rules() {
            if !rule.evaluate(&bundle, &RuleProfile::default()).is_empty() {
                fired.insert(rule.id().to_owned());
            }
        }
    }

    let unaccounted: Vec<&str> = builtin_rules()
        .iter()
        .map(|rule| rule.id())
        .filter(|id| !fired.contains(*id) && !NO_END_TO_END_FIXTURE.contains(id))
        .collect();

    assert!(
        unaccounted.is_empty(),
        "these rules fire on no fixture and are not recorded in NO_END_TO_END_FIXTURE: {}. \
         Either add a fixture that triggers them, or record them with the reason they cannot \
         be exercised yet. A rule that can never fire can only be tested by asserting it finds \
         nothing.",
        unaccounted.join(", "),
    );

    // The reverse direction: a rule listed as unexercised that now fires anyway.
    // The entry has served its purpose and is now misleading — a gap list that
    // silently contains solved problems is worse than no gap list.
    let resolved: Vec<&str> = NO_END_TO_END_FIXTURE
        .iter()
        .copied()
        .filter(|id| fired.contains(*id))
        .collect();
    assert!(
        resolved.is_empty(),
        "these are listed in NO_END_TO_END_FIXTURE but now fire end to end: {}. Remove them \
         from the list; the fixture that triggers them is presumably still in the corpus.",
        resolved.join(", "),
    );
}

#[test]
fn observing_stages_does_not_write_to_the_case() {
    // `observe_stages` is an observation surface, not a second analysis path.
    // It must not persist, cache, or touch the evidence.
    let tmp = tempfile::tempdir().expect("temp dir");
    let fixtures = corpus(tmp.path());
    let before: Vec<_> = fixtures
        .iter()
        .map(|p| std::fs::metadata(p).expect("stat").len())
        .collect();
    let digests: Vec<_> = fixtures
        .iter()
        .map(|p| std::fs::read(p).expect("read"))
        .collect();

    let engine = AnalysisEngine::new();
    for path in &fixtures {
        let _ = engine.observe_stages(path);
    }

    let after: Vec<_> = fixtures
        .iter()
        .map(|p| std::fs::metadata(p).expect("stat").len())
        .collect();
    for (i, path) in fixtures.iter().enumerate() {
        assert_eq!(before[i], after[i], "{path:?} changed size");
        assert_eq!(
            digests[i],
            std::fs::read(path).expect("read"),
            "{path:?} was modified; analysis must be read-only"
        );
    }
}

#[test]
fn every_rule_declares_at_least_what_it_needs() {
    // A rule declaring nothing while reading a field is exactly the hole the
    // declaration exists to close. This cannot detect it automatically, so it
    // asserts the weaker property that the declaration is at least populated:
    // the list of inputs with no requiring rule is stable and small.
    let unused = unrequired_inputs();
    assert!(
        unused.len() <= 3,
        "inputs no rule reads: {:?}. Either a rule stopped using one, or a rule \
         under-declares its inputs — which is the hole this whole mechanism has.",
        unused.iter().map(|i| i.tag()).collect::<Vec<_>>()
    );
}
#[test]
fn a_file_yielding_several_findings_from_one_rule_stores_without_a_key_collision() {
    // Regression guard for a crash that sat in the engine for a long time.
    //
    // `findings` has a primary key on `(analysis_id, id)`, and any rule producing
    // two findings at the same timeline position derived the *same* ID, because
    // the ID was built from rule ID and locator alone. The second insert then
    // aborted the entire analysis with a UNIQUE constraint failure: no findings
    // were stored, and the analyst was told the file could not be read.
    //
    // It surfaced only once a fixture produced two `TIMING.TIMESTAMP_GAP`
    // findings, which is to say: only when the corpus was finally good enough to
    // reach the bug. Every earlier fixture happened to produce at most one
    // finding per rule per position, so nothing caught it — and the corpus work
    // that uncovered it is why this test sits next to the other end-to-end
    // guards rather than in the engine's own tests.
    //
    // Driven through `analyse` rather than the rule engine alone, because the
    // failure was in persistence. A test that stopped at the rules would have
    // seen a plausible-looking list of findings and passed.
    let dir = tempfile::tempdir().expect("temp dir");
    let source = dir.path().join("multi-finding.mp4");
    // Doubling the frame duration partway through leaves a gap at every
    // subsequent sample, so the timing rules fire repeatedly.
    std::fs::write(&source, build_mp4_with_frame_rate_change(30, 30)).expect("writes source");

    let case_dir = dir.path().join("case.tptcase");
    CaseDirectory::create(&case_dir, &Case::new("Collision", None)).expect("creates case");
    let case_dir = CaseDirectory::open(&case_dir).expect("opens case");

    let outcome = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("analysis persists findings without a key collision");

    assert!(
        outcome.findings.len() > 2,
        "this fixture must produce several findings from one rule, or it proves nothing"
    );

    let mut ids: Vec<String> = outcome.findings.iter().map(|f| f.id.to_string()).collect();
    let total = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(
        ids.len(),
        total,
        "{} of {total} findings share an ID; the second insert fails the whole analysis",
        total - ids.len()
    );
}

#[test]
fn b_frame_reordering_alone_is_not_a_frame_rate_change() {
    // `VIDEO.FRAME_RATE_CHANGE` measures the spacing between presentation times.
    // Those are in *decode* order, so any file with B-frames has unevenly spaced
    // presentation times even when the frame rate never changes.
    //
    // Before the fix this reported a frame-rate change on `reordered.mp4` — a file
    // whose frame rate is constant for its whole length. Since essentially every
    // real encoded video file uses B-frames, the rule would have fired on nearly
    // all real media, which is the loudest possible way for a forensic rule to be
    // wrong: it trains an analyst to ignore it.
    //
    // Sorting the presentation timeline before measuring is what separates the
    // two cases. A reordered-but-constant track becomes uniform; a genuine rate
    // change stays uneven.
    let dir = tempfile::tempdir().expect("temp dir");

    let reordered = dir.path().join("reordered.mp4");
    std::fs::write(&reordered, build_mp4_with_reordered_frames(40)).expect("writes reordered");

    let changed = dir.path().join("frame-rate-change.mp4");
    std::fs::write(&changed, build_mp4_with_frame_rate_change(30, 30)).expect("writes changed");

    let engine = AnalysisEngine::new();

    let (bundle, _) = engine.observe_stages(&reordered);
    assert!(
        bundle.container.is_some(),
        "sanity: the reordered fixture must still parse"
    );
    let reordered_findings = builtin_rules()
        .iter()
        .find(|r| r.id() == "VIDEO.FRAME_RATE_CHANGE")
        .map(|r| r.evaluate(&bundle, &RuleProfile::default()))
        .unwrap_or_default();
    assert!(
        reordered_findings.is_empty(),
        "reordering frames is not a frame-rate change, but got: {:?}",
        reordered_findings
            .iter()
            .map(|f| &f.observation.summary)
            .collect::<Vec<_>>()
    );

    // The rule must still fire when the rate genuinely changes. A fix that silences
    // the rule is not a fix.
    let (bundle, _) = engine.observe_stages(&changed);
    let changed_findings = builtin_rules()
        .iter()
        .find(|r| r.id() == "VIDEO.FRAME_RATE_CHANGE")
        .map(|r| r.evaluate(&bundle, &RuleProfile::default()))
        .unwrap_or_default();
    assert!(
        !changed_findings.is_empty(),
        "a genuine mid-file frame-rate change must still be reported"
    );
}

#[test]
fn a_reordered_file_is_out_of_order_only_in_presentation_time() {
    // The reordering fixture is the one place a backwards timestamp is expected,
    // so the guard that would normally catch such a thing has to be shown still
    // working rather than switched off.
    //
    // What distinguishes a B-frame stream from a corrupt one is that *decode*
    // time stays strictly increasing while *presentation* time does not. A
    // fixture that went backwards in both would be proving nothing: any
    // backwards-timestamp reader would report it, including one that had merely
    // mis-parsed the table.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("reordered.mp4");
    std::fs::write(&path, build_mp4_with_reordered_frames(40)).expect("writes fixture");

    let (bundle, _) = AnalysisEngine::new().observe_stages(&path);
    let info = bundle
        .container
        .as_ref()
        .and_then(|c| c.first_video_frames())
        .expect("the fixture has a video track");

    assert!(
        info.decode_times.windows(2).all(|w| w[0] < w[1]),
        "decode time must stay strictly increasing; a file that goes backwards in \
         both is a broken table, not reordered frames"
    );
    assert!(
        info.frame_times.windows(2).any(|w| w[0] > w[1]),
        "presentation time must actually go backwards, or the fixture is not \
         exercising what it claims to"
    );
    assert_eq!(
        info.frame_times.len(),
        info.decode_times.len(),
        "the two sequences must describe the same frames"
    );
}

#[test]
fn each_audio_amplitude_fixture_triggers_only_its_own_rule() {
    // The corpus satisfies the coverage guard as a *set*: some file trips each
    // rule. That is weaker than it looks — a single loud file with a DC offset
    // would satisfy `AUDIO.CLIPPING`, `AUDIO.DC_OFFSET`, and `AUDIO.INAUDIBLE` at
    // once, and all three would pass while none of the fixtures actually isolated
    // the condition it claims to.
    //
    // Each fixture is therefore checked on its own, and checked for what it must
    // *not* trip. A fixture that trips extra rules is not wrong, but it stops
    // being evidence for the rule it was built for.
    let dir = tempfile::tempdir().expect("temp dir");
    let engine = AnalysisEngine::new();

    let cases: &[(&str, &[f32], &str, &[&str])] = &[
        (
            "audio-clipped.webm",
            &square_wave(1.0),
            "AUDIO.CLIPPING",
            &["AUDIO.DC_OFFSET", "AUDIO.INAUDIBLE"],
        ),
        (
            "audio-dc-offset.webm",
            &biased_tone(0.2),
            "AUDIO.DC_OFFSET",
            &["AUDIO.CLIPPING"],
        ),
        (
            "audio-inaudible.webm",
            &tone(1.0e-4),
            "AUDIO.INAUDIBLE",
            &["AUDIO.CLIPPING", "AUDIO.DC_OFFSET"],
        ),
    ];

    for (name, samples, expected, forbidden) in cases {
        let path = dir.path().join(name);
        std::fs::write(&path, webm_with_opus(dir.path(), samples)).expect("writes fixture");

        let (bundle, _) = engine.observe_stages(&path);
        assert!(
            !bundle.audio_levels.is_none(),
            "{name}: audio was never analysed, so the rule could not have fired"
        );

        let fired: Vec<&str> = builtin_rules()
            .iter()
            .map(|r| r.id())
            .filter(|id| {
                builtin_rules().iter().any(|r| {
                    r.id() == *id && !r.evaluate(&bundle, &RuleProfile::default()).is_empty()
                })
            })
            .collect();

        assert!(
            fired.contains(expected),
            "{name} should trip {expected} but only tripped {fired:?}"
        );
        for rule in *forbidden {
            assert!(
                !fired.contains(rule),
                "{name} also tripped {rule}, so it is not isolating {expected}: {fired:?}"
            );
        }
    }
}

#[test]
fn a_rule_fires_only_on_files_built_for_its_condition() {
    // The coverage guard asks "does every rule fire on *some* fixture". That
    // question is satisfiable by a rule firing everywhere, and one did: the
    // Matroska reader could not see `Segment > Info > Duration`, so every WebM
    // fixture reported "declares no duration". The finding was *correct about the
    // fixture* — the fixtures simply had no duration element, which no real muxer
    // omits — so the corpus trained a reader to expect that rule to fire on WebM
    // and would have hidden it on a file that genuinely lacked one.
    //
    // This asserts attribution instead: each rule fires only where its condition
    // was built. The list is the mapping from fixture to the rule it exists for.
    // A rule appearing twice is a fixture reaching two conditions, which is fine
    // and is why this is a set of pairs rather than one rule per fixture.
    let dir = tempfile::tempdir().expect("temp dir");
    let engine = AnalysisEngine::new();

    let expected: &[(&str, &str)] = &[
        // Duration is absent here by construction, and declared everywhere else.
        ("no-duration.webm", "CONTAINER.STREAM_DURATION_MISSING"),
        ("trailing-data.mp4", "CONTAINER.STRUCTURAL_DEFECT"),
        ("truncated.mp4", "CONTAINER.TRUNCATED_MEDIA"),
        ("empty-moov.mp4", "CONTAINER.NO_USABLE_STREAMS"),
        // An empty `moov` is one defect with three true answers: the structure is
        // malformed, the demuxer reported an anomaly, and no usable stream
        // resulted. All three are correct statements about the same bytes. Listing
        // all three is the point — a reader seeing one finding should be able to
        // find the other two without concluding the engine invented them.
        ("empty-moov.mp4", "CONTAINER.MALFORMED_STRUCTURE"),
        ("empty-moov.mp4", "CONTAINER.PARSE_ANOMALY"),
        ("track-mismatch.mp4", "CONTAINER.DECLARED_TRACK_MISMATCH"),
        (
            "wrong-duration.mp4",
            "METADATA.DECLARED_VS_MEASURED_MISMATCH",
        ),
        ("frame-rate-change.mp4", "VIDEO.FRAME_RATE_CHANGE"),
        ("frame-rate-change.mp4", "TIMING.TIMESTAMP_GAP"),
        ("reordered.mp4", "TIMING.NON_MONOTONIC_PTS"),
        // True of any closed IBBP stream, and the reason the frame-rate fix was
        // needed at all: composition offsets are applied per group, so the gap
        // between one group's last frame and the next group's first is not the
        // nominal frame duration. `scan_presentation` sees that as a gap in the
        // decode-to-presentation timeline.
        //
        // This is a real property of B-frame video, not an artefact of the
        // fixture, which is why it is listed rather than engineered away — a
        // fixture that hid it would misrepresent what the reader sees.
        ("reordered.mp4", "TIMING.TIMESTAMP_GAP"),
        ("bitrate-drop.mp4", "VIDEO.BITRATE_DROP"),
        // A consequence of how that fixture expresses a bitrate drop: the reduced
        // frames are 8 bytes of `0x5A`, so 40 consecutive samples are identical
        // and are therefore also a duplicate-frame run.
        //
        // Recorded rather than fixed. Byte-identical reduced frames are the
        // simplest way to shrink a sample, and a real encoder produces small but
        // *distinct* frames — so a fixture matching that more closely is worth
        // doing, but not at the cost of making the bitrate drop itself harder to
        // see. The pair is listed so the extra finding is a known quantity.
        ("bitrate-drop.mp4", "VIDEO.DUPLICATE_FRAME_RUN"),
        ("repeated-frames.mp4", "VIDEO.DUPLICATE_FRAME_RUN"),
        ("single-keyframe.mp4", "VIDEO.SINGLE_KEYFRAME"),
        ("gop-change.mp4", "VIDEO.GOP_LENGTH_CHANGE"),
        ("audio-clipped.webm", "AUDIO.CLIPPING"),
        ("audio-dc-offset.webm", "AUDIO.DC_OFFSET"),
        ("audio-inaudible.webm", "AUDIO.INAUDIBLE"),
        // Also correct, and worth stating rather than hiding: a tone at 1e-4 sits
        // below the 0.001 silence threshold as well as below the -70 LUFS floor.
        // "Too quiet to hear" and "sustained sub-threshold amplitude" are the same
        // observation measured two ways, so a file with this condition legitimately
        // answers both questions. Suppressing one to satisfy this guard would hide
        // a true finding.
        ("audio-inaudible.webm", "AUDIO.SILENCE_REGION"),
        ("audio.webm", "AUDIO.SILENCE_REGION"),
        ("video.av1.webm", "VIDEO.SCENE_CHANGE"),
        // The gradient in this fixture translates by 30 units per frame, so
        // consecutive frames are similar without being identical. That is a real
        // near-duplicate run, not an accident of the fixture: any moving-content
        // clip produces one. Recorded so the pair is visible rather than
        // discovered later as an unexplained extra finding.
        ("video.av1.webm", "VIDEO.NEAR_DUPLICATE_FRAME"),
        ("all-keyframes.mp4", "VIDEO.ALL_FRAMES_KEYFRAMES"),
        // One metadata fixture reaches two conditions: the atoms it carries have
        // both a conflicting timestamp pair and no creation time. Listing both is
        // the honest record — the fixtures are built for the metadata layer, and
        // these are the two questions that layer can answer about them.
        ("metadata.mp4", "METADATA.TIMESTAMP_CONFLICT"),
        ("metadata.mp4", "METADATA.MISSING_CREATION_TIME"),
        ("av.mp4", "TIMING.AV_SYNC_DRIFT"),
        // Also intended: this fixture is built with a 40 ms edit-list delay on its
        // audio track, so the audio stream's start offset *is* a condition it was
        // constructed to produce.
        ("av.mp4", "CONTAINER.STREAM_START_OFFSET"),
        // This fixture exists for the colour layer and nothing else: a `colr`
        // box naming BT.2020 and PQ, with no `mdcv` or `clli` beside it.
        ("hdr-signalling-only.mp4", "VIDEO.HDR_METADATA_MISSING"),
    ];

    for path in corpus(dir.path()) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let (bundle, _) = engine.observe_stages(&path);
        let fired: Vec<&str> = builtin_rules()
            .iter()
            .map(|r| r.id())
            .filter(|id| {
                builtin_rules().iter().any(|r| {
                    r.id() == *id && !r.evaluate(&bundle, &RuleProfile::default()).is_empty()
                })
            })
            .collect();

        for rule in &fired {
            assert!(
                expected.contains(&(name.as_str(), *rule)),
                "{name} fired {rule}, which it was not built to test. Either the \
                 condition is broader than intended, or the fixture is reaching \
                 something incidental. All of it fired: {fired:?}"
            );
        }
    }

    // And the other direction: every listed pair must still fire. A guard that
    // only rejects unexpected findings would pass on a corpus that quietly stopped
    // testing anything.
    for (name, rule) in expected {
        let path = dir.path().join(name);
        assert!(path.exists(), "{name} is missing from the corpus");
        let (bundle, _) = engine.observe_stages(&path);
        assert!(
            builtin_rules().iter().any(
                |r| r.id() == *rule && !r.evaluate(&bundle, &RuleProfile::default()).is_empty()
            ),
            "{name} no longer fires {rule}; the fixture is no longer testing what it was built for"
        );
    }
}
