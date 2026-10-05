//! PDF report rendering (spec §61).
//!
//! # Why this writes PDF directly rather than depending on a library
//!
//! PDF is a text-based container format, and this report is plain text —
//! headings, a findings table, and the disclaimer. Emitting it directly keeps
//! the output byte-deterministic (spec §77), which a PDF library with its own
//! metadata, font subsetting, or timestamp defaults would not be. A report whose
//! hash is recorded in the evidence manifest has to be reproducible, and that
//! property is worth more here than typesetting sophistication.
//!
//! # What it can and cannot do
//!
//! Uses the base-14 Helvetica font, which every conforming reader provides, so no
//! font is embedded and the file stays small. Text is drawn as positioned
//! `Tj`/`TJ` operators with no shaping: no kerning, no ligatures, no right-to-left
//! scripts, and no non-Latin-1 glyphs. A character outside Latin-1 is replaced
//! rather than dropped, because silently omitting text would misrepresent the
//! finding.
//!
//! # The disclaimer is not optional
//!
//! Spec §59 requires it on every report. [`to_pdf`] appends it as its own page
//! section, so a renderer cannot be called in a way that leaves it off.

use crate::model::{Report, ValidationResult, DISCLAIMER};

/// Page geometry in PostScript points (A4).
const PAGE_WIDTH: f64 = 595.28;
const PAGE_HEIGHT: f64 = 841.89;
/// Left and right margin.
const MARGIN: f64 = 56.0;
/// Usable text width.
const CONTENT_WIDTH: f64 = PAGE_WIDTH - (2.0 * MARGIN);
/// Distance from the top of the page to the first baseline.
const TOP_BASELINE: f64 = PAGE_HEIGHT - MARGIN;
/// Baseline-to-baseline distance for body text.
const LEADING: f64 = 14.0;

/// Renders the report as a PDF document.
///
/// # Errors
///
/// Returns an error only if the report cannot be serialised, which indicates a
/// programming error rather than bad input.
pub fn to_pdf(report: &Report) -> Result<Vec<u8>, crate::error::ReportError> {
    let pages = layout(report);
    assemble(&pages)
}

/// A laid-out page: text lines with their sizes.
struct Page {
    lines: Vec<Line>,
}

struct Line {
    text: String,
    size: f64,
    /// Extra space above this line, for headings.
    space_before: f64,
}

/// Lays the report out into pages.
fn layout(report: &Report) -> Vec<Page> {
    let mut lines: Vec<Line> = Vec::new();
    let mut push = |text: String, size: f64, space_before: f64| {
        // Only the first wrapped line carries the spacing; the rest are
        // continuations of the same logical line.
        let mut first = true;
        for wrapped in wrap(&text, size) {
            lines.push(Line {
                text: wrapped,
                size,
                space_before: if first { space_before } else { 0.0 },
            });
            first = false;
        }
    };

    push("Forensic Report".to_owned(), 20.0, 0.0);
    push(report.case_name.clone(), 13.0, 6.0);
    if let Some(description) = &report.case_description {
        push(description.clone(), 10.0, 2.0);
    }

    if let Some(result) = report.validation {
        push(format!("Validation result: {}", result.label()), 12.0, 10.0);
    }

    // The delivery profile, immediately after the verdict. A recipient disputing
    // a rejection needs the specification line that was missed beside the value
    // that was observed; putting it after the findings buries the only part of
    // this document a client will argue with.
    if let Some(delivery) = &report.delivery {
        push("Delivery validation".to_owned(), 14.0, 16.0);
        push(
            format!(
                "Profile {} (fingerprint {})",
                delivery.profile_identifier, delivery.profile_fingerprint
            ),
            9.0,
            2.0,
        );
        for check in &delivery.checks {
            push(
                format!(
                    "{}: {}  [{}]",
                    check.requirement_id,
                    check.outcome.label(),
                    check.observed.as_deref().unwrap_or("not measured")
                ),
                10.0,
                4.0,
            );
            push(format!("  Expected: {}", check.expected), 9.0, 0.0);
            push(format!("  {}", check.detail), 9.0, 0.0);
        }
    }

    // Summary counts first, so the reader sees the shape before the detail.
    let counts = report.severity_counts();
    push("Summary".to_owned(), 14.0, 16.0);
    push(
        format!("Assets examined: {}", report.assets.len()),
        10.0,
        4.0,
    );
    push(format!("Findings: {}", report.finding_count()), 10.0, 0.0);
    push(
        format!(
            "Critical {}   Significant {}   Warning {}   Info {}",
            counts[0], counts[1], counts[2], counts[3]
        ),
        10.0,
        0.0,
    );

    push("Assets".to_owned(), 14.0, 16.0);
    for asset in &report.assets {
        push(format!("{} ({})", asset.name, asset.size_bytes), 11.0, 6.0);
        if let Some(hash) = &asset.sha256 {
            push(format!("SHA-256 {hash}"), 9.0, 1.0);
        }
        if let Some(hash) = &asset.blake3 {
            push(format!("BLAKE3  {hash}"), 9.0, 1.0);
        }
        push(format!("Source {}", asset.source_path), 9.0, 1.0);
    }

    push("Findings".to_owned(), 14.0, 16.0);
    if report.findings.is_empty() {
        push(
            "No findings were raised by the enabled rules.".to_owned(),
            10.0,
            4.0,
        );
    }
    for (index, finding) in report.findings.iter().enumerate() {
        let placement = match (finding.timeline_start, finding.timeline_end) {
            (Some(start), Some(end)) => {
                format!("   [{} - {}]", start.to_timecode(), end.to_timecode())
            }
            (Some(start), None) => format!("   [{}]", start.to_timecode()),
            _ => String::new(),
        };
        push(
            format!(
                "{}. {} {}{}",
                index + 1,
                finding.severity.tag(),
                finding.rule_id,
                placement
            ),
            11.0,
            8.0,
        );
        push(finding.observation.summary.clone(), 10.0, 2.0);
        push(
            format!("Confidence: {}", finding.confidence.tag()),
            9.0,
            2.0,
        );
        for measurement in &finding.observation.measurements {
            push(measurement.clone(), 9.0, 1.0);
        }
    }

    if !report.notes.is_empty() {
        push("Analyst notes".to_owned(), 14.0, 16.0);
        for note in &report.notes {
            push(format!("[{}]", note.subject_label()), 9.5, 6.0);
            // The body is pushed line by line so the analyst's own paragraph breaks
            // survive into the PDF. `wrap` handles long lines; a single string would
            // be reflowed into one paragraph and lose where they intended a break.
            for line in note.body.lines() {
                if !line.trim().is_empty() {
                    push(line.to_owned(), 10.0, 1.0);
                }
            }
            push(String::new(), 6.0, 4.0);
        }
    }

    if !report.limitations.is_empty() {
        push("Limitations".to_owned(), 14.0, 16.0);
        for limitation in &report.limitations {
            push(format!("- {limitation}"), 10.0, 3.0);
        }
    }

    push("Methodology".to_owned(), 14.0, 16.0);
    let m = &report.methodology;
    for (label, value) in [
        ("Software version", m.application_version.as_str()),
        ("Analysis version", m.analysis_version.as_str()),
        ("Profile", m.profile.as_str()),
        ("Profile fingerprint", m.profile_fingerprint.as_str()),
        ("Rule set fingerprint", m.rule_set_fingerprint.as_str()),
        ("Analysis fingerprint", m.analysis_fingerprint.as_str()),
    ] {
        push(format!("{label}: {value}"), 10.0, 2.0);
    }
    push(
        format!("Rules applied: {}", m.enabled_rules.join(", ")),
        10.0,
        2.0,
    );

    // spec \u{a7}60: the applicable standards are part of the methodology, not
    // an optional extra. A measurement without the method it follows cannot be
    // interpreted, so this has to travel with the report.
    push(
        format!(
            "Applicable standards: {}",
            m.applicable_standards.join("; ")
        ),
        10.0,
        2.0,
    );
    for (name, hash) in &m.input_hashes {
        push(format!("Input {name}: {hash}"), 9.0, 1.0);
    }

    // Spec §59: the disclaimer is required on every report. It is appended here
    // rather than left to the caller, so no invocation can produce a PDF without
    // it.
    push("Important notice".to_owned(), 14.0, 16.0);
    for paragraph in DISCLAIMER.split("\n\n") {
        push(paragraph.replace('\n', " "), 10.0, 3.0);
    }

    paginate(lines)
}

/// Splits laid-out lines into pages.
fn paginate(lines: Vec<Line>) -> Vec<Page> {
    let mut pages = Vec::new();
    let mut current: Vec<Line> = Vec::new();
    let mut used = 0.0f64;

    for line in lines {
        let height = LEADING + line.space_before;
        // A page must always hold one line, or an oversized finding could loop
        // forever trying to place it.
        if used + height > TOP_BASELINE - MARGIN && !current.is_empty() {
            pages.push(Page {
                lines: std::mem::take(&mut current),
            });
            used = 0.0;
        }
        used += height;
        current.push(line);
    }

    if !current.is_empty() {
        pages.push(Page { lines: current });
    }
    if pages.is_empty() {
        pages.push(Page { lines: Vec::new() });
    }
    pages
}

/// Wraps text to the content width, approximately.
///
/// The column count is derived from the font size and the usable width rather
/// than fixed, so a change to either does not silently start overflowing the
/// margin. Wrapping is done on word boundaries, falling back to a hard break
/// for a word longer than the line (a long hex digest or a path).
fn wrap(text: &str, size: f64) -> Vec<String> {
    let sanitised = sanitise(text);
    if sanitised.is_empty() {
        return vec![String::new()];
    }

    // Helvetica averages roughly half the point size in glyph width; 0.5 is a
    // conservative estimate that keeps lines inside the margin.
    let columns = ((CONTENT_WIDTH / (size * 0.5)).floor() as usize).clamp(24, 120);

    let mut lines = Vec::new();
    let mut current = String::new();
    for word in sanitised.split_whitespace() {
        if current.is_empty() {
            current = word.to_owned();
        } else if current.chars().count() + 1 + word.chars().count() <= columns {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current = word.to_owned();
        }

        while current.chars().count() > columns {
            let head: String = current.chars().take(columns).collect();
            lines.push(head);
            current = current.chars().skip(columns).collect();
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Replaces characters the base-14 Latin-1 encoding cannot represent.
///
/// A dropped character would misrepresent a finding, so unrepresentable input
/// becomes `?` and the substitution is visible rather than silent.
fn sanitise(text: &str) -> String {
    text.chars()
        .map(|c| if (c as u32) < 256 { c } else { '?' })
        .collect()
}

/// Assembles laid-out pages into a PDF byte stream.
fn assemble(pages: &[Page]) -> Result<Vec<u8>, crate::error::ReportError> {
    // Object numbering:
    //   1 catalog, 2 page tree, 3 font, 4 info,
    //   then per page: a page object and a content stream.
    const CATALOG: usize = 1;
    const PAGE_TREE: usize = 2;
    const FONT: usize = 3;
    const INFO: usize = 4;
    const FIRST_PAGE: usize = 5;

    let page_count = pages.len();
    let kids: String = (0..page_count)
        .map(|i| format!("{} 0 R ", FIRST_PAGE + (i * 2)))
        .collect();

    let mut objects: Vec<(usize, Vec<u8>)> = Vec::new();
    objects.push((
        CATALOG,
        format!("<< /Type /Catalog /Pages {PAGE_TREE} 0 R >>").into_bytes(),
    ));
    objects.push((
        PAGE_TREE,
        format!("<< /Type /Pages /Count {page_count} /Kids [{kids}] >>").into_bytes(),
    ));
    objects.push((
        FONT,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    ));
    objects.push((INFO, b"<< /Producer (TPT Media Forensics) >>".to_vec()));

    for (index, page) in pages.iter().enumerate() {
        let content = content_stream(page);
        let content_object = FIRST_PAGE + (index * 2) + 1;

        objects.push((
            FIRST_PAGE + (index * 2),
            format!(
                "<< /Type /Page /Parent {PAGE_TREE} 0 R /MediaBox [0 0 {PAGE_WIDTH} {PAGE_HEIGHT}] \
                 /Resources << /Font << /F1 {FONT} 0 R >> >> /Contents {content_object} 0 R >>"
            )
            .into_bytes(),
        ));
        objects.push((content_object, stream(&content)));
    }

    let mut out: Vec<u8> = Vec::with_capacity(16 * 1024);
    out.extend_from_slice(b"%PDF-1.4\n");
    // A binary comment marks the file as containing binary data, which keeps
    // naive transport from corrupting it.
    out.extend_from_slice(b"%\xE2\xE3\xCF\xD3\n");

    // Offsets are recorded as each object is written; the xref table needs them.
    let mut offsets = Vec::with_capacity(objects.len() + 1);
    for (number, body) in &objects {
        offsets.push(out.len());
        out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }

    let xref_offset = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        out.extend_from_slice(format!("{:010} 00000 n \n", offset).as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root {CATALOG} 0 R /Info {INFO} 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );

    Ok(out)
}

/// Builds a content stream for one page.
fn content_stream(page: &Page) -> Vec<u8> {
    let mut out = String::with_capacity(2_048);
    out.push_str("BT\n");

    let mut y = TOP_BASELINE;
    for line in &page.lines {
        y -= line.space_before + LEADING;
        if y < MARGIN {
            // Unreachable in practice: `paginate` breaks before this. Guarded
            // anyway so a future layout change cannot emit off-page text.
            break;
        }
        out.push_str(&format!("{} TL\n", LEADING));
        out.push_str(&format!("/F1 {} Tf\n", line.size));
        out.push_str(&format!("1 0 0 1 {MARGIN} {y:.2} Tm\n"));
        out.push_str(&format!("({}) Tj\n", escape(&line.text)));
    }

    out.push_str("ET\n");
    out.into_bytes()
}

/// Wraps a byte string as a PDF stream object.
fn stream(content: &[u8]) -> Vec<u8> {
    let mut out = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    out.extend_from_slice(content);
    out.extend_from_slice(b"\nendstream");
    out
}

/// Escapes a string for a PDF literal.
///
/// Parentheses, backslashes, and control characters are all significant inside
/// a literal string. Report content comes from media metadata, which is
/// attacker-controlled, so this is a correctness requirement, not a nicety.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for byte in text.as_bytes() {
        match byte {
            b'(' => out.push_str("\\("),
            b')' => out.push_str("\\)"),
            b'\\' => out.push_str("\\\\"),
            b'\n' | b'\r' | b'\t' => out.push(' '),
            0x20..=0x7E => out.push(char::from(*byte)),
            _ => out.push_str(&format!("\\{byte:03o}")),
        }
    }
    out
}

/// Returns the validation label, exposed for the CLI summary.
#[must_use]
pub fn validation_label(result: ValidationResult) -> &'static str {
    result.label()
}
