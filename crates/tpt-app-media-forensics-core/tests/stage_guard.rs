//! The guard against unwired analysis stages.
//!
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
    build_mp4, build_mp4_av, build_mp4_with_bitrate_drop, build_mp4_with_repeated_frames,
    build_webm, TrackSpec,
};
use tpt_app_media_forensics_core::AnalysisEngine;
use tpt_app_media_forensics_rules::{builtin_rules, BundleInput, RuleProfile};

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

/// Builds a corpus of fixtures written to `dir`, and returns their paths.
fn corpus(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    use tpt_av_cadence_core::Encoder as _;

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
    let tone_then_silence: Vec<f32> = (0..48_000)
        .map(|i| {
            if i < 24_000 {
                (std::f64::consts::TAU * 440.0 * (i as f64) / 48_000.0).sin() as f32 * 0.8
            } else {
                0.0
            }
        })
        .collect();
    let opus_dir = dir.join("opus");
    std::fs::create_dir_all(&opus_dir).expect("creates opus dir");
    let opus_path = opus_dir.join("a.opus");
    let mut encoder = tpt_av_cadence_opus::OggOpusEncoder::new(
        std::fs::File::create(&opus_path).expect("creates sink"),
        48_000,
        1,
        96_000,
    )
    .expect("opens encoder");
    encoder.encode(&tone_then_silence).expect("encodes");
    encoder.finish().expect("finishes");

    let ogg = std::fs::read(&opus_path).expect("reads opus");
    let mut packets = Vec::new();
    {
        use tpt_av_cadence_core::BufferedSource;
        use tpt_av_cadence_ogg::PageReader;
        let mut reader = PageReader::new(
            BufferedSource::new(Box::new(std::io::Cursor::new(ogg.clone())), 32 * 1024),
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
    put("audio.webm", &build_webm("A_OPUS", 2, &blocks));

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

/// Rules no fixture in the corpus triggers *end to end*.
///
/// Not "untested". Every rule here is exercised by `-rules/tests/new_rules.rs`
/// against a hand-assembled `AnalysisBundle`, and `required_inputs` is declared
/// and checked by the guards above. What none of them has is a file: every test
/// builds the state its rule reads rather than deriving it from bytes on disk.
///
/// That is a narrower gap than it looks, and the difference matters. A
/// hand-built bundle can drift from what the pipeline actually produces, and
/// nothing here would notice. The guards close the worst version of that — an
/// unwired stage is caught — but they cannot catch a stage that is wired and
/// populates a slightly different shape.
///
/// Recorded rather than fixed, because closing it means roughly fifteen new
/// fixtures, each built to trip one rule. The list is here so that cost is
/// visible and so that a *newly* unfireable rule fails this test rather than
/// joining the list quietly.
const NO_END_TO_END_FIXTURE: &[&str] = &[
    "AUDIO.CLIPPING",
    "AUDIO.DC_OFFSET",
    "AUDIO.INAUDIBLE",
    "CONTAINER.DECLARED_TRACK_MISMATCH",
    "CONTAINER.MALFORMED_STRUCTURE",
    "CONTAINER.NO_USABLE_STREAMS",
    "CONTAINER.PARSE_ANOMALY",
    "CONTAINER.STREAM_START_OFFSET",
    "CONTAINER.STRUCTURAL_DEFECT",
    "METADATA.DECLARED_VS_MEASURED_MISMATCH",
    "TIMING.NON_MONOTONIC_PTS",
    "TIMING.TIMESTAMP_GAP",
    "VIDEO.FRAME_RATE_CHANGE",
    "VIDEO.GOP_LENGTH_CHANGE",
    "VIDEO.SINGLE_KEYFRAME",
];

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
