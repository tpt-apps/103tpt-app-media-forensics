//! Integration tests for report generation.
//!
//! The properties that matter here are not cosmetic: the disclaimer must be
//! present, findings must not overclaim, methodology must be complete, and
//! output must be byte-identical across runs.

use tpt_app_media_forensics_model::{
    AssetId, Confidence, Finding, FindingId, MediaTime, Observation, Severity,
};
use tpt_app_media_forensics_report::{
    asset_hashes_to_csv, escape_html, findings_to_csv, measurements_to_csv, notes_to_csv,
    standard_limitations, to_html, to_json, to_pdf, write_bundle, AssetSummary, Methodology, Note,
    Report, ValidationResult, DISCLAIMER,
};

fn finding(rule: &str, severity: Severity, summary: &str) -> Finding {
    let asset = AssetId::new_derived(&["asset-1"]);
    Finding {
        id: FindingId::new_derived(&[rule, asset.to_string().as_str()]),
        rule_id: rule.to_owned(),
        severity,
        confidence: Confidence::High,
        observation: Observation {
            summary: summary.to_owned(),
            measurements: vec!["observed: 50 -> 15 frames (measured from stts)".to_owned()],
        },
        rationale: Some(tpt_app_media_forensics_model::RuleRationale {
            checks: "Whether frame timing changes beyond the profile tolerance.".to_owned(),
            why_it_matters: "A cadence conversion or re-encode can change it harmlessly."
                .to_owned(),
            does_not_establish: tpt_app_media_forensics_model::RuleRationale::DEFAULT_LIMITATION
                .to_owned(),
        }),
        asset_id: asset,
        stream_id: None,
        timeline_start: Some(MediaTime::from_millis(2_000)),
        timeline_end: None,
        evidence: Vec::new(),
        frame_index: None,
        status: Default::default(),
        review_note: None,
    }
}

fn methodology() -> Methodology {
    Methodology {
        application_version: "0.1.0".to_owned(),
        analysis_version: "1".to_owned(),
        profile: "default-forensic v1".to_owned(),
        profile_fingerprint: "a".repeat(32),
        enabled_rules: vec!["VIDEO.GOP_LENGTH_CHANGE".to_owned()],
        rule_set_fingerprint: "b".repeat(32),
        input_hashes: vec![("asset-1".to_owned(), "c".repeat(64))],
        analysis_timestamp_unix: 1_755_000_000,
        applicable_standards: vec!["ITU-R BS.1770-4".to_owned()],
        analysis_fingerprint: "d".repeat(32),
    }
}

fn report() -> Report {
    Report {
        schema_version: tpt_app_media_forensics_report::REPORT_SCHEMA_VERSION,
        case_name: "Operation Alpha".to_owned(),
        case_id: "case:1".to_owned(),
        case_description: Some("A delivery dispute".to_owned()),
        assets: vec![AssetSummary {
            name: "original.mp4".to_owned(),
            source_path: "C:\\evidence\\original.mp4".to_owned(),
            sha256: Some("c".repeat(64)),
            blake3: Some("d".repeat(64)),
            size_bytes: 4096,
            stream_count: 2,
        }],
        findings: vec![
            finding(
                "VIDEO.GOP_LENGTH_CHANGE",
                Severity::Significant,
                "GOP length changed",
            ),
            finding(
                "AUDIO.CLIPPING",
                Severity::Warning,
                "Audio reaches clipping",
            ),
        ],
        evidence: Vec::new(),
        methodology: methodology(),
        limitations: standard_limitations(true, false, Some(48_000)),
        notes: Vec::new(),
        validation: None,
        delivery: None,
    }
}

#[test]
fn every_json_report_carries_the_disclaimer() {
    let json = to_json(&report()).expect("renders");
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(value["disclaimer"], DISCLAIMER);
}

#[test]
fn the_disclaimer_states_that_findings_are_not_proof() {
    // spec §59, verbatim requirement.
    let text = DISCLAIMER.to_lowercase();
    assert!(text.contains("must not be interpreted as proof"));
    assert!(text.contains("intent"));
    assert!(text.contains("authenticity"));
}

#[test]
fn the_html_report_carries_the_disclaimer_and_methodology() {
    let html = to_html(&report());
    assert!(html.contains("Disclaimer"));
    assert!(html.contains("must not be interpreted as proof"));
    // spec §60 requires each of these to be stated.
    for field in [
        "Application version",
        "Analysis engine version",
        "Profile",
        "Rule set fingerprint",
        "Analysis fingerprint",
        "Enabled rules",
        "Applicable standards",
    ] {
        assert!(html.contains(field), "methodology is missing {field}");
    }
}

#[test]
fn every_finding_renders_its_explanation_in_html() {
    // Spec §71 asks that every finding explain itself: what the rule checks, why
    // it matters, what was observed, and what the observation does not establish.
    // Three of those four were already rendered; the first two existed only as
    // trait methods that nothing outside their own tests ever called, so a report
    // reached a reviewer with the observation and no explanation of it.
    //
    // Asserted against the rendered document rather than the struct because the
    // struct passing proves nothing about what a reviewer is shown.
    let html = to_html(&report());

    assert!(
        html.contains("What this checks"),
        "the report must say what each rule checks: {html}"
    );
    assert!(
        html.contains("Why it matters"),
        "the report must say why each condition matters: {html}"
    );
    assert!(
        html.contains("frame timing changes beyond the profile tolerance"),
        "the rule's own prose must appear verbatim, not a paraphrase: {html}"
    );
}

#[test]
fn a_finding_with_no_rationale_still_carries_a_limitation() {
    // A finding assembled by a caller rather than raised by a rule has no
    // rationale to show. It must still state its limits rather than render as an
    // unexplained assertion — the disclaimer is the floor, not the ceiling.
    let mut report = report();
    for finding in &mut report.findings {
        finding.rationale = None;
    }

    let html = to_html(&report);
    assert!(html.contains("does not establish intent"));
    // And it does not claim an explanation it does not have.
    assert!(
        !html.contains("What this checks"),
        "a finding with no rationale must not render an empty explanation: {html}"
    );
}

#[test]
fn json_contains_machine_readable_raw_findings() {
    // spec §61: JSON should contain the raw findings.
    let value: serde_json::Value =
        serde_json::from_str(&to_json(&report()).expect("renders")).expect("valid");
    let findings = value["findings"].as_array().expect("findings array");
    assert_eq!(findings.len(), 2);
    assert_eq!(findings[0]["rule_id"], "VIDEO.GOP_LENGTH_CHANGE");
    assert_eq!(findings[0]["severity"], "SIGNIFICANT");
}

#[test]
fn csv_quotes_fields_containing_commas() {
    let mut r = report();
    r.findings[0].observation.summary = "GOP length 50 -> 15 frames, then back".to_owned();

    let csv = findings_to_csv(&r);
    assert!(csv
        .lines()
        .next()
        .expect("header")
        .starts_with("finding_id,rule_id,severity"));

    // The summary contains a comma, so its field must be quoted.
    let data = csv.lines().nth(1).expect("a data row");
    assert!(data.contains("\"GOP length 50 -> 15 frames, then back\""));
    assert_eq!(data.matches('"').count() % 2, 0, "quotes must be balanced");
}

#[test]
fn validation_result_follows_severity() {
    // The sample report carries a Significant finding, which blocks delivery
    // under spec §68.
    assert_eq!(
        ValidationResult::from_findings(&report().findings),
        ValidationResult::Fail
    );

    let mut warning_only = report();
    warning_only.findings.truncate(1);
    warning_only.findings[0].severity = Severity::Warning;
    assert_eq!(
        ValidationResult::from_findings(&warning_only.findings),
        ValidationResult::PassWithWarnings
    );

    let mut none = report();
    none.findings.clear();
    assert_eq!(
        ValidationResult::from_findings(&none.findings),
        ValidationResult::Pass
    );
}
#[test]
fn csv_escapes_embedded_quotes() {
    let mut r = report();
    r.findings[0].observation.summary = r#"he said "fake""#.to_owned();
    let csv = findings_to_csv(&r);
    assert!(
        csv.contains(r#""he said ""fake""""#),
        "quotes must be doubled"
    );
}

#[test]
fn measurements_csv_separates_value_from_methodology() {
    let csv = measurements_to_csv(&report());
    let row = csv.lines().nth(1).expect("a row");
    assert!(
        row.contains("ITU-R BS.1770-4") || row.contains("stts"),
        "{row}"
    );
}

#[test]
fn asset_hashes_csv_lists_every_asset() {
    let csv = asset_hashes_to_csv(&report());
    assert!(csv.starts_with("name,source_path,sha256,blake3,size_bytes"));
    assert!(csv.contains("original.mp4"));
    assert!(csv.contains(&"c".repeat(64)));
}

#[test]
fn notes_csv_carries_every_note_with_its_subject() {
    // The gap this closes: notes reached HTML, PDF and JSON, and reached none of
    // the CSVs. A recipient who worked from the spreadsheet rather than the
    // document lost the analyst's own words entirely, with nothing recording that
    // they had been dropped.
    let mut r = report();
    r.notes = vec![
        tpt_app_media_forensics_report::Note::on("finding", "finding-1", "Checked by hand."),
        tpt_app_media_forensics_report::Note::case("Whole case reviewed."),
    ];

    let csv = notes_to_csv(&r);
    assert!(
        csv.starts_with("subject_kind,subject_id,body"),
        "the header must name the columns: {csv}"
    );
    assert!(
        csv.contains("finding,finding-1,Checked by hand."),
        "a finding-level note must keep both halves of its subject: {csv}"
    );
    assert!(
        csv.contains("Whole case reviewed."),
        "a case-level note must be emitted: {csv}"
    );
    // The case-level note has no subject, and must not be given a fabricated one.
    let case_row = csv
        .lines()
        .find(|line| line.contains("Whole case reviewed."))
        .expect("a case-level row");
    assert!(
        case_row.starts_with(','),
        "a case-level note leaves the subject columns blank rather than inventing one: {case_row}"
    );
}

#[test]
fn a_note_body_survives_the_csv_round_trip_verbatim() {
    // A note is the one part of a report that is testimony rather than
    // measurement. Trimming it, joining its wrapped lines, or collapsing its
    // whitespace would edit the analyst's words, and a report that quietly
    // tidies up what someone wrote about a disputed file is not a faithful record
    // of it.
    let awkward = "First paragraph,\n  indented second.\n\nHe said \"check the master\".";
    let mut r = report();
    r.notes = vec![tpt_app_media_forensics_report::Note::case(awkward)];

    let csv = notes_to_csv(&r);
    // The body is the last column, so the record is everything after the header
    // line. Splitting on *lines* here would be wrong: the body itself contains
    // newlines, and they sit inside one quoted CSV record rather than ending it.
    let record = csv
        .split_once('\n')
        .expect("a header line")
        .1
        .trim_end_matches('\n');
    // Two blank subject columns — `subject_kind` and `subject_id` — and then the
    // quoted body.
    assert!(
        record.starts_with(",,\""),
        "the subject columns are blank and the body is quoted: {record}"
    );

    // And the escaped form decodes back to exactly what the analyst wrote.
    let inner = record.trim_start_matches(',').trim_matches('"');
    let decoded = inner.replace("\"\"", "\"");
    assert_eq!(
        decoded, awkward,
        "the body must round-trip byte-for-byte, not merely survive"
    );
}

#[test]
fn rendering_is_deterministic() {
    // spec §77: two renders of the same report are byte-identical.
    assert_eq!(
        to_json(&report()).expect("a"),
        to_json(&report()).expect("b")
    );
    assert_eq!(to_html(&report()), to_html(&report()));
    assert_eq!(findings_to_csv(&report()), findings_to_csv(&report()));
}

#[test]
fn html_escapes_content_so_report_data_cannot_inject_markup() {
    let mut r = report();
    r.case_name = "<script>alert(1)</script>".to_owned();
    r.findings[0].observation.summary = r#"<img src=x onerror="alert(2)">"#.to_owned();

    let html = to_html(&r);
    assert!(!html.contains("<script>"), "script tags must be escaped");
    assert!(!html.contains("<img"), "injected tags must be escaped");
    assert!(html.contains("&lt;img"));
    assert!(html.contains("&lt;script&gt;"));
}

#[test]
fn escape_html_covers_every_dangerous_character() {
    assert_eq!(
        escape_html(r#"<a href="x">&'</a>"#),
        "&lt;a href=&quot;x&quot;&gt;&amp;&#39;&lt;/a&gt;"
    );
}

#[test]
fn severity_counts_are_indexed_most_severe_first() {
    let mut r = report();
    r.findings = vec![
        finding("A", Severity::Info, "i"),
        finding("B", Severity::Critical, "c"),
        finding("C", Severity::Warning, "w"),
    ];
    assert_eq!(r.severity_counts(), [1, 0, 1, 1]);
}

#[test]
fn limitations_record_what_could_not_be_measured() {
    // spec §60: an approximate measurement must be stated as such.
    let limitations = standard_limitations(true, true, None);
    assert!(limitations.iter().any(|l| l.contains("48 kHz")));
    assert!(limitations
        .iter()
        .any(|l| l.contains("could not be decoded")));
}

#[test]
fn the_analysis_fingerprint_changes_with_every_input() {
    let base = Methodology::compute_fingerprint("h", "1", "p", "r");
    assert_eq!(base, Methodology::compute_fingerprint("h", "1", "p", "r"));
    assert_ne!(base, Methodology::compute_fingerprint("h2", "1", "p", "r"));
    assert_ne!(base, Methodology::compute_fingerprint("h", "2", "p", "r"));
    assert_ne!(base, Methodology::compute_fingerprint("h", "1", "p2", "r"));
    assert_ne!(base, Methodology::compute_fingerprint("h", "1", "p", "r2"));
}

#[test]
fn the_bundle_writes_every_deliverable_and_a_manifest() {
    let dir = tempfile::tempdir().expect("temp dir");
    let manifest = write_bundle(&report(), dir.path()).expect("writes");

    for expected in [
        "case-data.json",
        "case-report.html",
        "findings.csv",
        "measurements.csv",
        "asset-hashes.csv",
        "bundle-manifest.json",
    ] {
        assert!(
            dir.path().join(expected).exists(),
            "{expected} is missing from the bundle"
        );
    }

    // Six deliverables, and the manifest is excluded from its own listing: it`n    // cannot contain its own hash.`n    assert_eq!(manifest.files.len(), 6, "the manifest cannot list itself");
    for entry in &manifest.files {
        assert_eq!(entry.sha256.len(), 64, "each entry carries a SHA-256");
    }
}

#[test]
fn the_bundle_manifest_matches_the_written_files() {
    let dir = tempfile::tempdir().expect("temp dir");
    let manifest = write_bundle(&report(), dir.path()).expect("writes");

    use sha2::Digest as _;
    for entry in &manifest.files {
        let bytes = std::fs::read(dir.path().join(&entry.name)).expect("reads back");
        let digest = tpt_app_media_forensics_model::asset::to_hex(&sha2::Sha256::digest(&bytes));
        assert_eq!(digest, entry.sha256, "{} hash does not match", entry.name);
        assert_eq!(bytes.len() as u64, entry.size_bytes);
    }
}

#[test]
fn analyst_notes_are_rendered_in_html_and_pdf() {
    let mut report = report();
    report.notes = vec![Note::on("asset", "asset-1", "Frame rate disputed.")];

    let html = to_html(&report);
    assert!(
        html.contains("Analyst notes"),
        "the section must be present: {}",
        &html[html.len().saturating_sub(400)..]
    );
    assert!(html.contains("Frame rate disputed."));

    let pdf = to_pdf(&report).expect("pdf renders");
    assert!(
        pdf.starts_with(b"%PDF"),
        "the pdf must still be well formed"
    );
}

#[test]
fn a_report_with_no_notes_omits_the_section() {
    // An empty section heading would imply notes existed and none were shown.
    let html = to_html(&report());
    assert!(
        !html.contains("Analyst notes"),
        "no notes must mean no section, not an empty one"
    );
}

#[test]
fn a_note_body_is_escaped_in_html() {
    let mut report = report();
    report.notes = vec![Note::case("<script>alert(1)</script> & \"quotes\"")];

    let html = to_html(&report);
    assert!(
        !html.contains("<script>alert(1)</script>"),
        "a note is analyst-supplied text and must be escaped like any other"
    );
    assert!(html.contains("&lt;script&gt;"));
}

#[test]
fn a_case_level_note_is_labelled_as_such() {
    let mut report = report();
    report.notes = vec![Note::case("Client disputes the timestamp.")];

    let html = to_html(&report);
    assert!(
        html.contains(">case<"),
        "a note with no subject must be labelled, not left blank: {html}"
    );
}

#[test]
fn a_note_naming_half_a_subject_is_labelled_as_ambiguous() {
    // `add_note` refuses these, so this can only come from a hand-edited or older
    // report file. It is labelled rather than silently presented as case-level.
    let mut report = report();
    report.notes = vec![tpt_app_media_forensics_report::Note {
        subject_kind: Some("finding".to_owned()),
        subject_id: None,
        body: "half a subject".to_owned(),
    }];

    let html = to_html(&report);
    assert!(
        html.contains("subject not identified"),
        "an ambiguous subject must be visible as such"
    );
}

#[test]
fn notes_survive_a_json_round_trip() {
    let mut report = report();
    report.notes = vec![Note::on("asset", "asset-1", "first"), Note::case("second")];

    let json = to_json(&report).expect("json renders");
    let back: Report = serde_json::from_str(&json).expect("parses");
    assert_eq!(back.notes, report.notes);
}

#[test]
fn a_report_written_before_notes_existed_still_parses() {
    // `notes` is `#[serde(default)]`: an older report file has no `notes` key, and
    // failing to parse it would make every previously-generated report unreadable.
    let json = r#"{
        "schema_version": 1,
        "case_name": "Old",
        "case_id": "case:1",
        "case_description": null,
        "assets": [],
        "findings": [],
        "evidence": [],
        "methodology": {
            "application_version": "1.0.0",
            "analysis_version": "2",
            "profile": "default",
            "profile_fingerprint": "pf",
            "enabled_rules": [],
            "rule_set_fingerprint": "rs",
            "input_hashes": [],
            "analysis_timestamp_unix": 0,
            "applicable_standards": [],
            "analysis_fingerprint": "fp"
        },
        "limitations": [],
        "validation": null
    }"#;

    let parsed: Report = serde_json::from_str(json).expect("an older report must parse");
    assert!(
        parsed.notes.is_empty(),
        "absent notes must read as none recorded, not as a parse failure"
    );
}

#[test]
fn the_report_declares_the_schema_version_it_writes() {
    // If this drifts from the constant, a consumer cannot tell what a report
    // contains.
    assert_eq!(
        tpt_app_media_forensics_report::REPORT_SCHEMA_VERSION,
        report().schema_version,
        "the fixture must use the same version the build writes"
    );
}
