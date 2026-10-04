//! Self-contained HTML report rendering (spec §61).
//!
//! The output is a single file with no external references, so it can be
//! archived alongside the case or attached to correspondence without breaking.
//!
//! Everything interpolated into the document is escaped. Report content is
//! analyst-supplied — file names, metadata values, and finding summaries all
//! come from the evidence — and unescaped interpolation would let a file named
//! `"><script>...</script>` execute in a reviewer's browser.

use crate::model::{Report, DISCLAIMER};
use crate::render::escape_html;

/// Renders a complete, self-contained HTML document.
#[must_use]
pub fn to_html(report: &Report) -> String {
    let mut out = String::with_capacity(8_192);
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str(&format!(
        "<title>Forensic report - {}</title>\n",
        escape_html(&report.case_name)
    ));
    out.push_str(STYLE);
    out.push_str("</head>\n<body>\n");

    out.push_str("<header><h1>Forensic Report</h1>\n");
    out.push_str(&format!(
        "<p class=\"case\">{}</p>\n",
        escape_html(&report.case_name)
    ));
    if let Some(result) = report.validation {
        out.push_str(&format!(
            "<p class=\"result {}\">{}</p>\n",
            match result {
                crate::model::ValidationResult::Pass => "pass",
                crate::model::ValidationResult::PassWithWarnings => "warn",
                crate::model::ValidationResult::Fail => "fail",
            },
            result.label()
        ));
    }
    out.push_str("</header>\n");

    // Summary counts, so the reader sees the shape before the detail.
    let counts = report.severity_counts();
    out.push_str("<section id=\"summary\"><h2>Summary</h2>\n<table>\n");
    out.push_str(&row("Assets", &report.assets.len().to_string()));
    out.push_str(&row("Findings", &report.finding_count().to_string()));
    out.push_str(&row("Critical", &counts[0].to_string()));
    out.push_str(&row("Significant", &counts[1].to_string()));
    out.push_str(&row("Warning", &counts[2].to_string()));
    out.push_str(&row("Info", &counts[3].to_string()));
    out.push_str("</table>\n</section>\n");

    out.push_str("<section id=\"assets\"><h2>Asset identification</h2>\n<table>\n");
    out.push_str("<tr><th>Name</th><th>SHA-256</th><th>BLAKE3</th><th>Size</th></tr>\n");
    for asset in &report.assets {
        out.push_str(&format!(
            "<tr><td>{}</td><td class=\"hash\">{}</td><td class=\"hash\">{}</td><td>{}</td></tr>\n",
            escape_html(&asset.name),
            escape_html(asset.sha256.as_deref().unwrap_or("not computed")),
            escape_html(asset.blake3.as_deref().unwrap_or("not computed")),
            asset.size_bytes
        ));
    }
    out.push_str("</table>\n</section>\n");

    out.push_str("<section id=\"findings\"><h2>Findings</h2>\n");
    if report.findings.is_empty() {
        out.push_str("<p>No findings were raised.</p>\n");
    } else {
        for finding in &report.findings {
            out.push_str(&format!(
                "<article class=\"finding {}\">\n<h3>{} <span class=\"sev\">{}</span> <span class=\"conf\">{} confidence</span></h3>\n",
                severity_class(finding.severity),
                escape_html(&finding.rule_id),
                finding.severity.tag(),
                finding.confidence.tag()
            ));
            out.push_str(&format!(
                "<p>{}</p>\n",
                escape_html(&finding.observation.summary)
            ));
            if !finding.observation.measurements.is_empty() {
                out.push_str("<ul>\n");
                for measurement in &finding.observation.measurements {
                    out.push_str(&format!("<li>{}</li>\n", escape_html(measurement)));
                }
                out.push_str("</ul>\n");
            }
            // Spec §71: what the rule checks, why it matters, what was observed, and what
            // the observation does not establish. The three prose blocks travel with
            // the finding rather than being looked up from the rule at render time,
            // so they survive into the JSON and the PDF a reviewer reads later.
            if let Some(rationale) = &finding.rationale {
                out.push_str("<dl class=\"rationale\">\n");
                out.push_str(&format!(
                    "<dt>What this checks</dt><dd>{}</dd>\n",
                    escape_html(&rationale.checks)
                ));
                out.push_str(&format!(
                    "<dt>Why it matters</dt><dd>{}</dd>\n",
                    escape_html(&rationale.why_it_matters)
                ));
                out.push_str("</dl>\n");
            }
            if let (Some(start), Some(end)) = (finding.timeline_start, finding.timeline_end) {
                out.push_str(&format!(
                    "<p class=\"time\">{} - {}</p>\n",
                    start.to_timecode(),
                    end.to_timecode()
                ));
            }
            // The rule's own caveat where it states one, the report-wide
            // disclaimer otherwise. A finding with no stated limits reads as more
            // conclusive than one that states them, so this is never omitted — but
            // boilerplate is labelled as boilerplate rather than presented as if
            // the rule author had written it about this specific observation.
            let limitation = match &finding.rationale {
                Some(rationale) if rationale.has_specific_limit() => {
                    rationale.does_not_establish.clone()
                }
                _ => tpt_app_media_forensics_model::RuleRationale::DEFAULT_LIMITATION.to_owned(),
            };
            out.push_str(&format!(
                "<p class=\"limits\">{}</p>\n</article>\n",
                escape_html(&limitation)
            ));
        }
    }
    out.push_str("</section>\n");

    if !report.evidence.is_empty() {
        out.push_str("<section id=\"evidence\"><h2>Evidence</h2>\n<table>\n");
        out.push_str("<tr><th>Kind</th><th>Path</th><th>SHA-256</th></tr>\n");
        for item in report.referenced_evidence() {
            out.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td class=\"hash\">{}</td></tr>\n",
                item.kind.tag(),
                escape_html(&item.relative_path),
                escape_html(item.integrity.hashes.sha256().unwrap_or("not computed"),)
            ));
        }
        out.push_str("</table>\n</section>\n");
    }

    if !report.notes.is_empty() {
        out.push_str("<section id=\"notes\"><h2>Analyst notes</h2>\n<table>\n");
        out.push_str("<tr><th>Subject</th><th>Note</th></tr>\n");
        for note in &report.notes {
            out.push_str(&format!(
                "<tr><td>{}</td><td class=\"note\">{}</td></tr>\n",
                escape_html(&note.subject_label()),
                escape_html(&note.body)
            ));
        }
        out.push_str("</table>\n</section>\n");
    }

    if !report.limitations.is_empty() {
        out.push_str("<section id=\"limitations\"><h2>Limitations</h2>\n<ul>\n");
        for limitation in &report.limitations {
            out.push_str(&format!("<li>{}</li>\n", escape_html(limitation)));
        }
        out.push_str("</ul>\n</section>\n");
    }

    out.push_str("<section id=\"methodology\"><h2>Technical methodology</h2>\n<table>\n");
    let m = &report.methodology;
    out.push_str(&row(
        "Application version",
        &escape_html(&m.application_version),
    ));
    out.push_str(&row(
        "Analysis engine version",
        &escape_html(&m.analysis_version),
    ));
    out.push_str(&row("Profile", &escape_html(&m.profile)));
    out.push_str(&row(
        "Profile fingerprint",
        &escape_html(&m.profile_fingerprint),
    ));
    out.push_str(&row(
        "Rule set fingerprint",
        &escape_html(&m.rule_set_fingerprint),
    ));
    out.push_str(&row(
        "Analysis fingerprint",
        &escape_html(&m.analysis_fingerprint),
    ));
    out.push_str(&row(
        "Enabled rules",
        &escape_html(&m.enabled_rules.join(", ")),
    ));
    out.push_str(&row(
        "Applicable standards",
        &escape_html(&m.applicable_standards.join(", ")),
    ));
    out.push_str("</table>\n</section>\n");

    out.push_str(&format!(
        "<section id=\"disclaimer\"><h2>Disclaimer</h2>\n<pre class=\"disclaimer\">{}</pre>\n</section>\n",
        escape_html(DISCLAIMER)
    ));
    out.push_str("</body>\n</html>\n");
    out
}

/// Renders one table row.
fn row(label: &str, value: &str) -> String {
    format!("<tr><th>{label}</th><td>{value}</td></tr>\n")
}

/// Maps a severity to a CSS class.
fn severity_class(severity: tpt_app_media_forensics_model::Severity) -> &'static str {
    use tpt_app_media_forensics_model::Severity;
    match severity {
        Severity::Critical => "critical",
        Severity::Significant => "significant",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

/// Inline stylesheet, so the document needs no external files.
const STYLE: &str = r#"<style>
body{font:14px/1.5 system-ui,sans-serif;margin:2rem auto;max-width:60rem;color:#1a1a1a}
header{border-bottom:2px solid #1a1a1a;padding-bottom:.5rem}
h1{margin:0}
table{border-collapse:collapse;width:100%;margin:.5rem 0 1.5rem}
th,td{border:1px solid #ccc;padding:.35rem .6rem;text-align:left;vertical-align:top}
th{background:#f4f4f4;font-weight:600}
.hash{font-family:ui-monospace,monospace;font-size:.8em;word-break:break-all}
.note{white-space:pre-wrap;width:60%}
.finding{border-left:4px solid #ccc;padding:.5rem 1rem;margin:1rem 0;background:#fafafa}
.finding.critical{border-color:#b00020}
.finding.significant{border-color:#d66b00}
.finding.warning{border-color:#c9a227}
.finding.info{border-color:#888}
.sev{font-size:.8em;text-transform:uppercase;letter-spacing:.04em}
.conf{font-size:.8em;color:#555}
.time{font-family:ui-monospace,monospace}
.limits{font-size:.85em;color:#555;font-style:italic}
.disclaimer{white-space:pre-wrap;font:inherit;background:#f4f4f4;padding:1rem;border:1px solid #ccc}
.result{font-weight:700;font-size:1.2em}
.result.pass{color:#0b6b2f}
.result.warn{color:#8a6100}
.result.fail{color:#b00020}
</style>
"#;
