//! Tests for the PDF renderer.
//!
//! PDF is a hand-assembled binary container, so these tests check the parts a
//! reader actually depends on: the header, a correct cross-reference table with
//! real byte offsets, and stream lengths that match their bodies. A PDF with
//! plausible-looking objects but wrong offsets opens as "damaged", which a
//! snapshot test of the rendered text would never catch.

use tpt_app_media_forensics_model::{
    AssetId, Confidence, Finding, FindingId, FindingStatus, Observation, Severity,
};
use tpt_app_media_forensics_report::model::{AssetSummary, Methodology, Report, ValidationResult};
use tpt_app_media_forensics_report::to_pdf;

fn asset() -> AssetId {
    AssetId::new_derived(&[b"asset"])
}

fn methodology() -> Methodology {
    Methodology {
        application_version: "0.1.0".to_owned(),
        analysis_version: "1".to_owned(),
        profile: "default-forensic v1".to_owned(),
        profile_fingerprint: "aaaa".to_owned(),
        enabled_rules: vec!["VIDEO.GOP_LENGTH_CHANGE".to_owned()],
        rule_set_fingerprint: "bbbb".to_owned(),
        input_hashes: vec![("clip.mp4".to_owned(), "ccdd".to_owned())],
        analysis_timestamp_unix: 1_700_000_000,
        applicable_standards: vec!["ITU-R BS.1770-4".to_owned()],
        analysis_fingerprint: "eeee".to_owned(),
    }
}

fn finding(index: u8) -> Finding {
    Finding {
        id: FindingId::new_derived(&[&[index]]),
        rule_id: format!("VIDEO.RULE_{index}"),
        severity: Severity::Warning,
        confidence: Confidence::High,
        observation: Observation {
            summary: format!("Observation number {index} was recorded."),
            measurements: vec!["gop length: 50 -> 15".to_owned()],
        },
        rationale: None,
        asset_id: asset(),
        stream_id: None,
        timeline_start: None,
        timeline_end: None,
        evidence: Vec::new(),
        frame_index: None,
        status: FindingStatus::default(),
        review_note: None,
    }
}

fn report(count: usize) -> Report {
    Report {
        schema_version: tpt_app_media_forensics_report::REPORT_SCHEMA_VERSION,
        case_name: "Case Alpha".to_owned(),
        case_id: "case:00000000-0000-0000-0000-000000000001".to_owned(),
        case_description: Some("A description.".to_owned()),
        assets: vec![AssetSummary {
            name: "clip.mp4".to_owned(),
            source_path: "evidence/clip.mp4".to_owned(),
            sha256: Some("a".repeat(64)),
            blake3: Some("b".repeat(64)),
            size_bytes: 12_345,
            stream_count: 2,
        }],
        findings: (0..count).map(|i| finding(i as u8)).collect(),
        evidence: Vec::new(),
        methodology: methodology(),
        limitations: vec!["Audio was not decoded.".to_owned()],
        notes: Vec::new(),
        validation: None,
    }
}

fn pdf_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Returns the cross-reference rows, located in the raw bytes.
///
/// Offsets in a PDF are byte positions, so they must be measured against the
/// real bytes. The document carries a deliberately binary comment marker
/// (`%` followed by four high bytes) that is not valid UTF-8; measuring against
/// `String::from_utf8_lossy` would replace each of those bytes with a
/// three-byte replacement character and shift every later offset.
fn xref_rows(bytes: &[u8]) -> Vec<String> {
    let needle = b"\nxref\n";
    let start = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("has a cross-reference table")
        + needle.len();

    let tail = String::from_utf8_lossy(&bytes[start..]).into_owned();
    tail.lines()
        .skip(1)
        .map(ToOwned::to_owned)
        .take_while(|row| row.ends_with(" n ") || row.ends_with(" f "))
        .collect()
}

#[test]
fn the_output_is_a_pdf_document() {
    let bytes = to_pdf(&report(3)).expect("renders");
    assert!(
        pdf_text(&bytes).starts_with("%PDF-1.4"),
        "missing PDF header"
    );
    assert!(
        pdf_text(&bytes).trim_end().ends_with("%%EOF"),
        "missing EOF marker"
    );
}

#[test]
fn the_cross_reference_offsets_point_at_their_objects() {
    // The xref table is the part most likely to be silently wrong. Every offset
    // must be the exact byte position of `N 0 obj` for that object number.
    let bytes = to_pdf(&report(2)).expect("renders");
    let rows = xref_rows(&bytes);

    // `xref_rows` drops the subsection header and keeps only real entries, so
    // row 0 is the free head and rows 1.. are objects 1..n.
    assert!(
        rows[0].starts_with("0000000000 65535 f"),
        "object 0 must be the free entry, got {}",
        rows[0]
    );

    for (index, row) in rows[1..].iter().enumerate() {
        let object = index + 1;
        let offset: usize = row
            .split_whitespace()
            .next()
            .expect("offset field")
            .parse()
            .expect("offset parses");
        let prefix = format!("{object} 0 obj");
        assert!(
            bytes[offset..].starts_with(prefix.as_bytes()),
            "xref entry {object} points at {offset}, which does not start `{prefix}`"
        );
    }
}

#[test]
fn the_startxref_offset_is_correct() {
    let bytes = to_pdf(&report(1)).expect("renders");
    let text = pdf_text(&bytes);

    let marker = text.rfind("startxref").expect("has startxref");
    let offset: usize = text[marker..]
        .lines()
        .nth(1)
        .expect("has an offset line")
        .trim()
        .parse()
        .expect("offset parses");

    assert!(
        bytes[offset..].starts_with(b"xref"),
        "startxref points at {offset}, which is not the xref table"
    );
}

#[test]
fn every_content_stream_declares_its_true_length() {
    let bytes = to_pdf(&report(2)).expect("renders");
    let mut checked = 0usize;
    let mut search = 0usize;

    while let Some(offset) = bytes[search..]
        .windows(11)
        .position(|w| w == b"<< /Length ")
    {
        let start = search + offset;
        let rest = &bytes[start + 11..];
        let end = rest
            .windows(3)
            .position(|w| w == b" >>")
            .expect("stream dictionary closes");
        let declared: usize = std::str::from_utf8(&rest[..end])
            .expect("ascii")
            .trim()
            .parse()
            .expect("length parses");

        let body_start = rest
            .windows(7)
            .position(|w| w == b"stream\n")
            .expect("has a stream")
            + 7;
        assert!(
            rest.len() >= body_start + declared + 11,
            "/Length {declared} runs past the stream body"
        );
        assert!(
            rest[body_start + declared..].starts_with(b"\nendstream"),
            "/Length {declared} does not land on the end of the stream"
        );

        search = start + 11 + body_start + declared;
        checked += 1;
    }
    assert!(checked > 0, "expected at least one content stream");
}

#[test]
fn the_disclaimer_is_always_present() {
    // spec 59: no invocation may produce a report without it.
    for count in [0usize, 1, 5] {
        let text = pdf_text(&to_pdf(&report(count)).expect("renders"));
        assert!(
            text.contains("must not be interpreted as proof"),
            "disclaimer missing with {count} findings"
        );
    }
}

#[test]
fn findings_appear_in_the_document() {
    let text = pdf_text(&to_pdf(&report(2)).expect("renders"));
    assert!(text.contains("VIDEO.RULE_0"));
    assert!(text.contains("VIDEO.RULE_1"));
    assert!(text.contains("Observation number 0"));
}

#[test]
fn a_report_with_no_findings_still_renders() {
    let bytes = to_pdf(&report(0)).expect("renders");
    assert!(bytes.starts_with(b"%PDF-1.4"));
    assert!(pdf_text(&bytes).contains("No findings"));
}

#[test]
fn the_output_is_byte_identical_across_runs() {
    // spec 77: determinism. A PDF whose bytes vary cannot be hashed into the
    // evidence manifest.
    assert_eq!(
        to_pdf(&report(4)).expect("renders"),
        to_pdf(&report(4)).expect("renders")
    );
}

#[test]
fn a_long_report_paginates() {
    let text = pdf_text(&to_pdf(&report(120)).expect("renders"));

    let pages = text.matches("/Type /Page ").count();
    assert!(
        pages > 1,
        "120 findings should span several pages, got {pages}"
    );
    assert!(
        text.contains(&format!("/Count {pages}")),
        "the page tree count must match the number of pages"
    );
}

#[test]
fn drawn_strings_are_balanced_under_hostile_metadata() {
    // Metadata is attacker-controlled, so an unbalanced parenthesis would close
    // the string early and spill the remainder into the document body.
    let mut hostile = report(1);
    hostile.case_name = r#"Case \\ with (parens) and \\ backslashes"#.to_owned();
    hostile.assets[0].name = r#"a\)b"#.to_owned();

    let text = pdf_text(&to_pdf(&hostile).expect("renders"));

    let mut drawn = 0usize;
    for line in text.lines().filter(|l| l.ends_with(" Tj")) {
        drawn += 1;
        let mut depth = 0i32;
        let mut escaped = false;
        for c in line.chars() {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '(' {
                depth += 1;
            } else if c == ')' {
                depth -= 1;
            }
        }
        assert_eq!(depth, 0, "unbalanced string in: {line}");
    }
    assert!(drawn > 0, "expected some text to be drawn");
}

#[test]
fn non_latin1_characters_are_substituted_not_dropped() {
    let mut wide = report(1);
    wide.assets[0].name = "clip-\u{4f60}\u{597d}.mp4".to_owned();

    let text = pdf_text(&to_pdf(&wide).expect("renders"));
    assert!(
        text.contains("clip-??.mp4"),
        "expected visible substitution markers"
    );
}

#[test]
fn validation_results_are_rendered() {
    let mut failing = report(1);
    failing.validation = Some(ValidationResult::Fail);
    assert!(pdf_text(&to_pdf(&failing).expect("renders")).contains("FAIL"));

    let mut warning = report(1);
    warning.validation = Some(ValidationResult::PassWithWarnings);
    assert!(pdf_text(&to_pdf(&warning).expect("renders")).contains("PASS WITH WARNINGS"));
}

#[test]
fn control_characters_are_escaped_not_emitted_raw() {
    let mut hostile = report(0);
    hostile.limitations = vec!["\u{0}\u{7}\u{1f}control characters".to_owned()];

    let text = pdf_text(&to_pdf(&hostile).expect("renders"));
    assert!(
        text.contains("\\000") || text.contains("\\001"),
        "control characters were not octal-escaped"
    );
    assert!(
        !text.contains('\u{0}'),
        "a raw NUL would truncate the stream for a reader"
    );
}

#[test]
fn the_reproducibility_data_travels_with_the_pdf() {
    // spec 60, 63: the report must be reproducible from what it states.
    let text = pdf_text(&to_pdf(&report(1)).expect("renders"));
    for expected in ["0.1.0", "default-forensic v1", "eeee", "ITU-R BS.1770-4"] {
        assert!(
            text.contains(expected),
            "missing reproducibility data: {expected}"
        );
    }
}
