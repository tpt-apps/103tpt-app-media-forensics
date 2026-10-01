//! Report renderers (spec §61).
//!
//! # JSON is canonical
//!
//! JSON carries the machine-readable raw findings; HTML and CSV are renderings
//! of the same data. Nothing is computed during rendering, so a figure shown
//! in HTML can always be traced back to a value in the JSON.
//!
//! # Output is deterministic
//!
//! Findings are emitted in the order they appear in the report, which the
//! engine has already sorted by severity, rule ID, and timeline position
//! (spec §77). Two renders of the same report are byte-identical, which is
//! what makes a report's hash usable in the evidence bundle manifest (§62).

use crate::error::ReportError;
use crate::model::{Report, DISCLAIMER};

/// Renders the report as canonical JSON (spec §61).
///
/// Pretty-printed rather than compact: the JSON is a deliverable a human may
/// have to read during a dispute, and the size difference is irrelevant at
/// report scale.
pub fn to_json(report: &Report) -> Result<String, ReportError> {
    let mut value = serde_json::to_value(report).map_err(|e| ReportError::Render {
        format: "json",
        reason: e.to_string(),
    })?;

    // The disclaimer is attached at render time so it cannot be omitted from
    // the serialised form by constructing a `Report` directly.
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "disclaimer".to_owned(),
            serde_json::Value::String(DISCLAIMER.to_owned()),
        );
    }
    serde_json::to_string_pretty(&value).map_err(|e| ReportError::Render {
        format: "json",
        reason: e.to_string(),
    })
}

/// Renders findings as CSV (spec §61).
///
/// RFC 4180 quoting: a field containing a comma, quote, or newline is quoted
/// and internal quotes doubled. A finding summary routinely contains commas, so
/// unquoted output would be unparseable.
pub fn findings_to_csv(report: &Report) -> String {
    let mut out = String::from(
        "finding_id,rule_id,severity,confidence,status,asset,stream,timeline_start,timeline_end,summary,measurements\n",
    );

    for finding in &report.findings {
        let fields = [
            finding.id.to_string(),
            finding.rule_id.clone(),
            finding.severity.tag().to_owned(),
            finding.confidence.tag().to_owned(),
            finding.status.tag().to_owned(),
            finding.asset_id.to_string(),
            finding
                .stream_id
                .map_or_else(String::new, |s| s.to_string()),
            finding
                .timeline_start
                .map_or_else(String::new, |t| t.to_timecode()),
            finding
                .timeline_end
                .map_or_else(String::new, |t| t.to_timecode()),
            finding.observation.summary.clone(),
            finding.observation.measurements.join("; "),
        ];
        out.push_str(
            &fields
                .iter()
                .map(|f| csv_field(f))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push('\n');
    }
    out
}

/// Renders measurements as CSV (spec §61).
///
/// The measurement table carries the methodology alongside each value, because
/// a bare number without its method is not interpretable (spec §21).
pub fn measurements_to_csv(report: &Report) -> String {
    let mut out = String::from("rule_id,severity,measurement,methodology\n");

    for finding in &report.findings {
        for measurement in &finding.observation.measurements {
            let (value, method) = split_measurement(measurement);
            let fields = [
                finding.rule_id.clone(),
                finding.severity.tag().to_owned(),
                value,
                method,
            ];
            out.push_str(
                &fields
                    .iter()
                    .map(|f| csv_field(f))
                    .collect::<Vec<_>>()
                    .join(","),
            );
            out.push('\n');
        }
    }
    out
}

/// Renders asset hashes as CSV, for the evidence bundle (spec §62).
pub fn asset_hashes_to_csv(report: &Report) -> String {
    let mut out = String::from("name,source_path,sha256,blake3,size_bytes\n");
    for asset in &report.assets {
        let fields = [
            asset.name.clone(),
            asset.source_path.clone(),
            asset.sha256.clone().unwrap_or_default(),
            asset.blake3.clone().unwrap_or_default(),
            asset.size_bytes.to_string(),
        ];
        out.push_str(
            &fields
                .iter()
                .map(|f| csv_field(f))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push('\n');
    }
    out
}

/// Quotes a CSV field when it contains a delimiter, quote, or newline.
fn csv_field(value: &str) -> String {
    let needs_quotes = value.contains([',', '"', '\n', '\r']);
    if needs_quotes {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

/// Splits `"value (method)"` into its two halves.
///
/// Findings render measurements as `value (method)` so a reader sees the
/// method inline; CSV keeps them in separate columns so they remain sortable.
fn split_measurement(measurement: &str) -> (String, String) {
    match measurement.rfind(" (") {
        Some(index) if measurement.ends_with(')') => (
            measurement[..index].to_owned(),
            measurement[index + 2..measurement.len() - 1].to_owned(),
        ),
        _ => (measurement.to_owned(), String::new()),
    }
}

/// Escapes text for inclusion in HTML element content.
#[must_use]
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}
