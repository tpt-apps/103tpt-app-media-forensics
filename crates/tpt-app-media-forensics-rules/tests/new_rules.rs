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

/// A track carrying an explicit `stss`, so it is not an all-keyframes case.
fn keyed_bytes() -> Vec<u8> {
    build_mp4_with_keyframes(&TrackSpec::video_25fps(320, 240, 60), &[0, 12, 24, 36])
}

#[test]
fn the_full_rule_set_is_registered() {
    let ids = builtin_rules().iter().map(|r| r.id()).collect::<Vec<_>>();
    assert_eq!(ids.len(), 21, "expected the complete rule set, got {ids:?}");
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
