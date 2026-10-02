//! Tests for the container, video, audio, and metadata rules.
//!
//! Each rule is checked twice: once against a fixture that should trip it, and
//! once against a fixture that should not. A rule that only ever fires is as
//! useless as one that never does, and the negative case is what catches a
//! comparison that is too permissive.
//!
//! Rules are pure functions over an `AnalysisBundle`, so these build the bundle
//! directly rather than running the whole pipeline.

use tpt_app_media_forensics_audio::{Measurement, Methodology};
use tpt_app_media_forensics_container::fixture::{
    build_mp4, build_mp4_stsd_gop_change, build_mp4_with_keyframes, build_mp4_without_stss,
    TrackSpec,
};
use tpt_app_media_forensics_metadata::{MetadataEntry, MetadataTree};
use tpt_app_media_forensics_model::{AssetId, Finding, Severity};
use tpt_app_media_forensics_rules::engine::{empty_bundle, AnalysisBundle};
use tpt_app_media_forensics_rules::{builtin_rules, RuleEngine, RuleProfile};

fn asset() -> AssetId {
    AssetId::new_derived(&[b"new-rules-asset"])
}

/// A bundle carrying a parsed container plus the GOP report the video rules
/// read. The pipeline populates both, so a bundle that omits the GOP report
/// would test a rule against a state the engine never produces.
fn bundle_with_container(bytes: Vec<u8>) -> AnalysisBundle {
    let mut bundle = empty_bundle(asset());
    bundle.container = tpt_app_media_forensics_container::inspect_bytes(bytes).ok();

    if let Some(inspection) = &bundle.container {
        if let Some(Some(info)) = inspection.frame_info.first() {
            bundle.gop = Some(tpt_app_media_forensics_video::gop::analyse(
                &info.keyframes,
                &info.frame_times,
                RuleProfile::default().gop_tolerance_frames,
            ));
        }
    }
    bundle
}

/// Runs one rule by ID against a bundle.
fn run(bundle: &AnalysisBundle, rule_id: &str) -> Vec<Finding> {
    let rule = builtin_rules()
        .into_iter()
        .find(|r| r.id() == rule_id)
        .unwrap_or_else(|| panic!("{rule_id} is not registered"));
    rule.evaluate(bundle, &RuleProfile::default())
}

fn clean_bytes() -> Vec<u8> {
    build_mp4(&TrackSpec::video_25fps(320, 240, 60))
}

/// `count` samples of identical size — a steady rate, the clean case.
fn uniform_samples(
    count: usize,
    size: u64,
) -> Vec<tpt_app_media_forensics_video::bitrate::BitrateSample> {
    (0..count)
        .map(|i| tpt_app_media_forensics_video::bitrate::BitrateSample {
            time: tpt_app_media_forensics_model::MediaTime::from_micros(i as i64 * 40_000),
            size,
            is_key_frame: i == 0,
        })
        .collect()
}

/// `high` large samples followed by `low` small ones — spec §29's shape.
fn mixed_samples(
    high: usize,
    low: usize,
) -> Vec<tpt_app_media_forensics_video::bitrate::BitrateSample> {
    (0..high + low)
        .map(|i| tpt_app_media_forensics_video::bitrate::BitrateSample {
            time: tpt_app_media_forensics_model::MediaTime::from_micros(i as i64 * 40_000),
            size: if i < high { 40_000 } else { 2_000 },
            is_key_frame: i == 0,
        })
        .collect()
}

/// A bundle whose structural scan has run over `bytes`, as the pipeline does.
///
/// The damage is derived from the same bytes rather than hand-built, so these
/// tests exercise the real scan. A test that assembled a `StructuralDamage` by
/// hand would pass even if the scan never produced it.
fn bundle_with_damage(bytes: &[u8]) -> AnalysisBundle {
    let mut bundle = bundle_with_container(bytes.to_vec());
    bundle.damage = tpt_app_media_forensics_container::scan_isobmff(bytes);
    bundle
}

/// A track carrying an explicit `stss`, so it is not an all-keyframes case.
fn keyed_bytes() -> Vec<u8> {
    build_mp4_with_keyframes(&TrackSpec::video_25fps(320, 240, 60), &[0, 12, 24, 36])
}

#[test]
fn the_full_rule_set_is_registered() {
    let ids = builtin_rules().iter().map(|r| r.id()).collect::<Vec<_>>();
    assert_eq!(ids.len(), 26, "expected the complete rule set, got {ids:?}");
    assert_eq!(
        ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
        ids.len(),
        "rule IDs must be unique"
    );
}

#[test]
fn every_rule_explains_what_it_checks_and_why_it_matters() {
    // spec 71: a rule with an empty rationale could not satisfy it.
    for rule in builtin_rules() {
        assert!(
            !rule.what_it_checks().trim().is_empty(),
            "{} has no description",
            rule.id()
        );
        assert!(
            rule.why_it_matters().trim().len() > 20,
            "{} gives no meaningful rationale",
            rule.id()
        );
    }
}

#[test]
fn rule_ids_are_stable_and_uppercase() {
    // The ID appears in reports and CSV exports, so its shape is a contract.
    for rule in builtin_rules() {
        let id = rule.id();
        assert!(id.contains('.'), "{id} is not DOMAIN.TECHNIQUE");
        assert!(
            id.chars()
                .all(|c| c.is_ascii_uppercase() || c == '.' || c == '_'),
            "{id} is not uppercase"
        );
    }
}

#[test]
fn a_clean_container_raises_no_parse_anomaly() {
    let bundle = bundle_with_container(clean_bytes());
    assert!(run(&bundle, "CONTAINER.PARSE_ANOMALY").is_empty());
}

#[test]
fn damage_is_placed_on_the_timeline_when_samples_were_read() {
    // The §31 case: a byte offset becomes a timecode an analyst can act on.
    let mut bytes = clean_bytes();
    bytes.truncate(bytes.len() / 2);

    let mut bundle = bundle_with_damage(&bytes);
    let samples = tpt_app_media_forensics_container::read_samples(bytes).expect("reads samples");
    bundle.sample_index = tpt_app_media_forensics_container::SampleIndex::build(&samples, 789);

    let findings = run(&bundle, "CONTAINER.TRUNCATED_MEDIA");
    assert_eq!(findings.len(), 1, "{findings:?}");
    let finding = &findings[0];
    let at = finding
        .timeline_start
        .expect("a truncated file must be placed on the timeline");

    // The placement must be the *last sample at or before the damage*, which for
    // this fixture is the first sample — legitimately at time zero. The
    // assertion is therefore that a placement exists and is consistent with the
    // sample index, not that it is non-zero: a sample index built from real
    // samples always starts at zero, and asserting otherwise would fail on
    // correct code.
    let located = bundle
        .sample_index
        .positions()
        .iter()
        .find(|position| position.byte_offset == 789)
        .expect("the index starts at the anchor");
    assert_eq!(at, located.time);
    assert_eq!(
        at,
        tpt_app_media_forensics_model::MediaTime::from_micros(0),
        "the first sample of any file is at time zero"
    );
    assert!(
        finding
            .observation
            .measurements
            .iter()
            .any(|m| m.contains("inferred")),
        "the finding must disclose that the offset was inferred: {:?}",
        finding.observation.measurements
    );
}

#[test]
fn damage_without_a_sample_index_is_reported_without_a_timecode() {
    // The negative case. A rule that filled `timeline_start` with zero here
    // would put every header-level defect at 00:00:00 on the timeline, which
    // reads as a measured position and is not one.
    let mut bytes = clean_bytes();
    bytes.truncate(bytes.len() / 2);

    let bundle = bundle_with_damage(&bytes);
    assert!(
        bundle.sample_index.is_empty(),
        "no samples were read, so there is nothing to place against"
    );

    let findings = run(&bundle, "CONTAINER.TRUNCATED_MEDIA");
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(
        findings[0].timeline_start.is_none(),
        "no timecode may be invented when none was measured"
    );
    assert!(
        !findings[0]
            .observation
            .measurements
            .iter()
            .any(|m| m.contains("last readable sample")),
        "the placement line must be absent, not present and wrong"
    );
}

#[test]
fn a_uniform_bitrate_profile_raises_no_bitrate_finding() {
    // The negative case, and the one that matters most: a rule that fires on
    // ordinary content trains an examiner to ignore the severity list.
    let mut bundle = bundle_with_container(clean_bytes());
    let samples = uniform_samples(60, 25_000);
    bundle.bitrate = Some(tpt_app_media_forensics_video::bitrate::analyse(
        &samples, 12, 0.5,
    ));

    assert!(run(&bundle, "VIDEO.BITRATE_DROP").is_empty());
}

#[test]
fn a_sustained_bitrate_drop_is_reported_with_the_spec_example_shape() {
    // Spec §29's own worked example, reproduced in the finding.
    let mut bundle = bundle_with_container(clean_bytes());
    let samples = mixed_samples(50, 50);
    bundle.bitrate = Some(tpt_app_media_forensics_video::bitrate::analyse(
        &samples, 12, 0.5,
    ));

    let findings = run(&bundle, "VIDEO.BITRATE_DROP");
    assert!(!findings.is_empty(), "a 20x drop must be reported");

    let finding = &findings[0];
    assert_eq!(finding.severity, Severity::Warning);
    assert_eq!(
        finding.observation.summary,
        "Bitrate falls well below the file average",
    );
    let joined = finding.observation.measurements.join(" | ");
    for expected in ["average:", "segment:", "observed:"] {
        assert!(
            joined.contains(expected),
            "the finding must carry '{expected}' as spec §29 shows it: {joined}"
        );
    }
    assert!(
        finding.timeline_start.is_some(),
        "the window must be placed on the timeline"
    );
}

#[test]
fn a_window_without_a_keyframe_says_so() {
    // The alternative explanation, offered rather than resolved. A window between
    // keyframes is a different observation from one where content stopped moving.
    let mut bundle = bundle_with_container(clean_bytes());
    let samples = mixed_samples(50, 50);
    bundle.bitrate = Some(tpt_app_media_forensics_video::bitrate::analyse(
        &samples, 12, 0.5,
    ));

    let findings = run(&bundle, "VIDEO.BITRATE_DROP");
    let any_explains = findings.iter().any(|f| {
        f.observation
            .measurements
            .iter()
            .any(|m| m.contains("no keyframe"))
    });
    assert!(
        any_explains,
        "a keyframe-free window must disclose that: {findings:?}"
    );
}

#[test]
fn no_bitrate_analysis_means_no_finding_rather_than_a_guess() {
    // A file above the sampling bound has no bitrate report. The rule must be
    // silent, not report a zero rate.
    let bundle = bundle_with_container(clean_bytes());
    assert!(bundle.bitrate.is_none());
    assert!(run(&bundle, "VIDEO.BITRATE_DROP").is_empty());
}

#[test]
fn a_clean_file_reports_no_structural_damage() {
    // The negative case, and the one most likely to regress: a scan that reports
    // damage on a well-formed file would make the two damage rules fire on every
    // asset in a case, which trains an examiner to ignore them.
    let bytes = clean_bytes();
    let bundle = bundle_with_damage(&bytes);
    assert!(bundle.damage.is_empty(), "{:?}", bundle.damage);
    assert!(run(&bundle, "CONTAINER.TRUNCATED_MEDIA").is_empty());
    assert!(run(&bundle, "CONTAINER.STRUCTURAL_DEFECT").is_empty());
}

#[test]
fn a_truncated_file_is_reported_as_missing_media() {
    let mut bytes = clean_bytes();
    bytes.truncate(bytes.len() / 2);

    let bundle = bundle_with_damage(&bytes);
    assert!(
        !bundle.damage.is_empty(),
        "a half-length file must produce damage"
    );

    let findings = run(&bundle, "CONTAINER.TRUNCATED_MEDIA");
    assert_eq!(findings.len(), 1, "{findings:?}");
    let finding = &findings[0];
    assert_eq!(finding.severity, Severity::Critical);
    assert!(
        finding.observation.summary.contains("truncated"),
        "the summary names the defect: {}",
        finding.observation.summary
    );
    assert!(
        finding
            .observation
            .measurements
            .iter()
            .any(|m| m.contains("offset")),
        "the finding carries the byte offset, not just a verdict: {:?}",
        finding.observation.measurements
    );
    assert!(
        finding
            .observation
            .measurements
            .iter()
            .any(|m| m.contains("only the portion")),
        "the finding must state that other measurements cover only what is present: {:?}",
        finding.observation.measurements
    );
}

#[test]
fn truncation_does_not_also_report_a_structural_defect() {
    // The two rules must not both fire on one defect. Overlapping findings make
    // severity counts meaningless, which is the number a dashboard leads with.
    let mut bytes = clean_bytes();
    bytes.truncate(bytes.len() / 2);
    let bundle = bundle_with_damage(&bytes);

    assert!(run(&bundle, "CONTAINER.STRUCTURAL_DEFECT").is_empty());
}

#[test]
fn a_recorded_anomaly_is_reported_with_its_text() {
    let mut bundle = bundle_with_container(clean_bytes());
    bundle
        .container
        .as_mut()
        .expect("parsed")
        .anomalies
        .push("stts entry overruns the sample table".to_owned());

    let findings = run(&bundle, "CONTAINER.PARSE_ANOMALY");
    assert_eq!(findings.len(), 1);
    assert!(
        findings[0]
            .observation
            .summary
            .contains("stts entry overruns"),
        "the anomaly text must reach the finding"
    );
}

#[test]
fn a_track_without_stss_is_reported_as_all_keyframes() {
    let bundle = bundle_with_container(build_mp4_without_stss(&TrackSpec::video_25fps(
        320, 240, 30,
    )));
    let findings = run(&bundle, "VIDEO.ALL_FRAMES_KEYFRAMES");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].severity, Severity::Info);
    assert!(
        findings[0].observation.summary.contains("no `stss` box"),
        "{}",
        findings[0].observation.summary
    );
}

#[test]
fn a_track_with_stss_is_not_reported_as_all_keyframes() {
    // `build_mp4` writes no `stss`, so the default fixture is itself an
    // all-keyframes case. A track that declares sync samples must stay silent.
    let bundle = bundle_with_container(keyed_bytes());
    assert!(run(&bundle, "VIDEO.ALL_FRAMES_KEYFRAMES").is_empty());
}

#[test]
fn a_single_keyframe_track_is_reported() {
    let bundle = bundle_with_container(build_mp4_with_keyframes(
        &TrackSpec::video_25fps(320, 240, 60),
        &[0],
    ));
    let findings = run(&bundle, "VIDEO.SINGLE_KEYFRAME");
    assert_eq!(findings.len(), 1, "a one-keyframe track should be reported");
    assert_eq!(findings[0].severity, Severity::Warning);
}

#[test]
fn a_multi_keyframe_track_is_not_reported() {
    let bundle = bundle_with_container(build_mp4_with_keyframes(
        &TrackSpec::video_25fps(320, 240, 60),
        &[0, 10, 20, 30],
    ));
    assert!(run(&bundle, "VIDEO.SINGLE_KEYFRAME").is_empty());
}

#[test]
fn a_constant_frame_rate_is_not_reported_as_a_change() {
    let bundle = bundle_with_container(clean_bytes());
    assert!(run(&bundle, "VIDEO.FRAME_RATE_CHANGE").is_empty());
}

#[test]
fn quiet_audio_is_reported_as_inaudible() {
    let mut bundle = empty_bundle(asset());
    bundle.loudness = Some(Measurement::new(-80.0, Methodology::ItuBs1770_4));
    let findings = run(&bundle, "AUDIO.INAUDIBLE");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].severity, Severity::Warning);
    assert!(findings[0].observation.summary.contains("-80"));
}

#[test]
fn normal_loudness_is_not_reported_as_inaudible() {
    let mut bundle = empty_bundle(asset());
    bundle.loudness = Some(Measurement::new(-23.0, Methodology::ItuBs1770_4));
    assert!(run(&bundle, "AUDIO.INAUDIBLE").is_empty());
}

#[test]
fn a_file_with_no_loudness_measurement_is_not_reported() {
    // An absent measurement is a limitation, not a finding.
    let bundle = empty_bundle(asset());
    assert!(run(&bundle, "AUDIO.INAUDIBLE").is_empty());
}

#[test]
fn metadata_without_a_creation_time_is_reported() {
    let mut bundle = empty_bundle(asset());
    bundle.metadata = Some(MetadataTree::new(vec![MetadataEntry::container(
        "title", "A clip", "moov",
    )]));
    let findings = run(&bundle, "METADATA.MISSING_CREATION_TIME");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].severity, Severity::Info);
}

#[test]
fn metadata_with_a_creation_time_is_not_reported() {
    let mut bundle = empty_bundle(asset());
    bundle.metadata = Some(MetadataTree::new(vec![MetadataEntry::container(
        "creation_time",
        "2024-01-01T00:00:00Z",
        "moov",
    )]));
    assert!(run(&bundle, "METADATA.MISSING_CREATION_TIME").is_empty());
}

#[test]
fn a_file_with_no_metadata_is_not_reported_by_the_narrow_rule() {
    // The absence of all metadata is a different observation; this rule asks a
    // narrower question and must not double-report it.
    let mut bundle = empty_bundle(asset());
    bundle.metadata = Some(MetadataTree::new(Vec::new()));
    assert!(run(&bundle, "METADATA.MISSING_CREATION_TIME").is_empty());
    assert!(run(&empty_bundle(asset()), "METADATA.MISSING_CREATION_TIME").is_empty());
}

#[test]
fn declared_track_mismatch_is_reported_only_when_the_counts_differ() {
    let bundle = bundle_with_container(clean_bytes());
    let inspection = bundle.container.as_ref().expect("parsed");
    let declared = inspection.declared_track_count;
    let actual = inspection.streams.len();

    // The fixture is well-formed, so the rule must stay silent.
    assert_eq!(declared, actual);
    assert!(run(&bundle, "CONTAINER.DECLARED_TRACK_MISMATCH").is_empty());

    let mut broken = bundle;
    let inspection = broken.container.as_mut().expect("parsed");
    inspection.declared_track_count = actual + 3;
    let findings = run(&broken, "CONTAINER.DECLARED_TRACK_MISMATCH");
    assert_eq!(findings.len(), 1);
    let summary = &findings[0].observation.summary;
    assert!(summary.contains(&(actual + 3).to_string()), "{summary}");
    assert!(summary.contains(&actual.to_string()), "{summary}");
}

/// A container whose `mdhd` declares a length its own sample table does not
/// support.
///
/// Built from real bytes rather than by patching a parsed bundle, so the test
/// exercises the same path a real file would take. Mutating the bundle instead
/// would let the rule pass while the reader that produces these numbers was
/// broken.
fn mis_declared_bytes(declared_ticks: u64) -> Vec<u8> {
    let mut track = TrackSpec::video_25fps(320, 240, 60);
    track.declared_duration = Some(declared_ticks);
    build_mp4(&track)
}

#[test]
fn a_header_that_contradicts_the_sample_table_is_reported() {
    // 60 ticks at 25 fps is 2.4 s of samples; the header claims 10 s.
    let bundle = bundle_with_container(mis_declared_bytes(250));

    let findings = run(&bundle, "METADATA.DECLARED_VS_MEASURED_MISMATCH");
    assert_eq!(
        findings.len(),
        1,
        "expected the disagreement to be reported"
    );
    let finding = &findings[0];
    assert_eq!(finding.severity, Severity::Warning);

    // Both numbers must appear, so a reader can check the engine's arithmetic
    // rather than take the verdict on trust.
    let text = format!(
        "{} {:?}",
        finding.observation.summary, finding.observation.measurements
    );
    assert!(text.contains("00:00:10.000"), "{text}");
    assert!(text.contains("00:00:02.400"), "{text}");
}

#[test]
fn a_correct_muxer_is_not_reported() {
    let bundle = bundle_with_container(clean_bytes());
    assert!(run(&bundle, "METADATA.DECLARED_VS_MEASURED_MISMATCH").is_empty());
}

#[test]
fn a_disagreement_inside_the_rounding_slack_is_not_reported() {
    // One tick at 25 fps is 40 ms. Well-formed muxers round durations, so a
    // disagreement this small must not be dressed up as tampering.
    let bundle = bundle_with_container(mis_declared_bytes(61));
    assert!(
        run(&bundle, "METADATA.DECLARED_VS_MEASURED_MISMATCH").is_empty(),
        "rounding slack should not be reported"
    );
}

#[test]
fn an_unmeasurable_duration_is_never_treated_as_agreement() {
    let mut bundle = bundle_with_container(mis_declared_bytes(250));
    bundle.container.as_mut().expect("parsed").streams[0]
        .timing
        .measured_duration = None;

    // Quiet, but for the right reason: there was nothing to compare. The
    // distinction matters because reporting it as agreement would let a
    // container with an unreadable sample table look verified.
    assert!(run(&bundle, "METADATA.DECLARED_VS_MEASURED_MISMATCH").is_empty());
}

#[test]
fn the_finding_reports_the_disagreement_without_asserting_a_cause() {
    let bundle = bundle_with_container(mis_declared_bytes(250));
    let finding = run(&bundle, "METADATA.DECLARED_VS_MEASURED_MISMATCH")
        .pop()
        .expect("a finding");

    let text = format!(
        "{} {:?}",
        finding.observation.summary, finding.observation.measurements
    )
    .to_lowercase();
    for claim in ["tamper", "edited", "falsif", "manipulat", "truncat"] {
        assert!(
            !text.contains(claim),
            "spec §26 forbids asserting a cause, but the text contains {claim:?}: {text}"
        );
    }
}

#[test]
fn missing_stream_duration_is_reported_per_stream() {
    let mut bundle = bundle_with_container(clean_bytes());
    assert!(!bundle
        .container
        .as_ref()
        .expect("parsed")
        .streams
        .is_empty());

    bundle.container.as_mut().expect("parsed").streams[0]
        .timing
        .duration = None;
    let findings = run(&bundle, "CONTAINER.STREAM_DURATION_MISSING");
    assert_eq!(findings.len(), 1);
    assert!(findings[0]
        .observation
        .summary
        .contains("declares no duration"));
}

#[test]
fn the_engine_runs_every_rule_without_error_on_a_damaged_file() {
    let bundle = bundle_with_container(build_mp4_stsd_gop_change());
    let findings = RuleEngine::new(builtin_rules())
        .evaluate(&bundle, &RuleProfile::default())
        .expect("evaluation succeeds");

    assert!(
        findings
            .iter()
            .any(|f| f.rule_id == "VIDEO.GOP_LENGTH_CHANGE"),
        "the pre-existing GOP finding must survive the new rules"
    );
}

#[test]
fn findings_come_back_most_severe_first() {
    let bundle = bundle_with_container(build_mp4_stsd_gop_change());
    let findings = RuleEngine::new(builtin_rules())
        .evaluate(&bundle, &RuleProfile::default())
        .expect("evaluation succeeds");

    for pair in findings.windows(2) {
        assert!(
            pair[0].severity >= pair[1].severity,
            "findings must be ordered most severe first"
        );
    }
}

#[test]
fn every_finding_carries_a_summary_and_a_measurement() {
    // spec 21: a bare observation without its method is not interpretable.
    for bytes in [clean_bytes(), build_mp4_stsd_gop_change()] {
        let bundle = bundle_with_container(bytes);
        let findings = RuleEngine::new(builtin_rules())
            .evaluate(&bundle, &RuleProfile::default())
            .expect("evaluation succeeds");

        for finding in &findings {
            assert!(
                !finding.observation.summary.trim().is_empty(),
                "{} produced an empty summary",
                finding.rule_id
            );
            assert!(
                !finding.observation.measurements.is_empty(),
                "{} produced no supporting measurement",
                finding.rule_id
            );
        }
    }
}

#[test]
fn the_documented_rule_set_matches_the_registered_rules() {
    // docs/rules.md lists the shipped rules. That list went stale once (it said
    // twenty-one while twenty-three were registered), which is exactly what a
    // hand-maintained list does. This test is the list: adding or removing a
    // rule without updating the document fails the build rather than shipping
    // a document that quietly describes a different product.
    const DOCUMENTED: &[&str] = &[
        "CONTAINER.PARSE_ANOMALY",
        "CONTAINER.DECLARED_TRACK_MISMATCH",
        "CONTAINER.STREAM_DURATION_MISSING",
        "CONTAINER.STREAM_START_OFFSET",
        "CONTAINER.MALFORMED_STRUCTURE",
        "CONTAINER.NO_USABLE_STREAMS",
        "CONTAINER.STRUCTURAL_DEFECT",
        "CONTAINER.TRUNCATED_MEDIA",
        "VIDEO.ALL_FRAMES_KEYFRAMES",
        "VIDEO.BITRATE_DROP",
        "VIDEO.SINGLE_KEYFRAME",
        "VIDEO.FRAME_RATE_CHANGE",
        "VIDEO.GOP_LENGTH_CHANGE",
        "VIDEO.DUPLICATE_FRAME_RUN",
        "VIDEO.SCENE_CHANGE",
        "VIDEO.NEAR_DUPLICATE_FRAME",
        "AUDIO.CLIPPING",
        "AUDIO.DC_OFFSET",
        "AUDIO.SILENCE_REGION",
        "AUDIO.INAUDIBLE",
        "TIMING.NON_MONOTONIC_PTS",
        "TIMING.TIMESTAMP_GAP",
        "TIMING.AV_SYNC_DRIFT",
        "METADATA.TIMESTAMP_CONFLICT",
        "METADATA.DECLARED_VS_MEASURED_MISMATCH",
        "METADATA.MISSING_CREATION_TIME",
    ];

    let mut registered: Vec<&str> = builtin_rules().iter().map(|rule| rule.id()).collect();
    registered.sort_unstable();

    let mut expected = DOCUMENTED.to_vec();
    expected.sort_unstable();

    assert_eq!(
        registered, expected,
        "docs/rules.md must list exactly the rules in builtin_rules()"
    );
    assert_eq!(DOCUMENTED.len(), 26, "the shipped count is 26 rules");
}

#[test]
fn rule_ids_are_unique() {
    // Duplicate IDs would collide in the cache fingerprint and in reports, where
    // two different findings would be indistinguishable.
    let ids: Vec<&str> = builtin_rules().iter().map(|rule| rule.id()).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    let before = sorted.len();
    sorted.dedup();
    assert_eq!(before, sorted.len(), "duplicate rule ID registered");
}
