//! Report generation (spec §59-§63).
//!
//! Renders findings and measurements to PDF, HTML, JSON, and CSV, and embeds
//! the reproducibility data — software version, analysis version, profile and
//! rule-set fingerprints — plus the required forensic disclaimers.

#![forbid(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
//! Report generation (spec §59-§63).
//!
//! A report is the deliverable the entire product exists to produce, so two
//! properties matter more than presentation:
//!
//! 1. **It cannot overclaim.** The disclaimer is a constant on the report type
//!    rather than a template variable (spec §59), and no rule asserts a cause.
//! 2. **It can be reproduced and verified.** Every report carries the software
//!    version, analysis version, profile, rule set, input hashes, applicable
//!    standards, and a combined analysis fingerprint (spec §60, §63). The same
//!    four inputs that key the analysis cache key the fingerprint, so a report
//!    and its cache validity cannot disagree.

pub mod bundle;
pub mod error;
pub mod html;
pub mod model;
pub mod pdf;
pub mod render;

pub use bundle::{write_bundle, BundleEntry, BundleManifest};
pub use error::ReportError;
pub use html::to_html;
pub use model::{
    standard_limitations, AssetSummary, Methodology, Note, Report, ValidationResult, DISCLAIMER,
    REPORT_SCHEMA_VERSION,
};
pub use pdf::to_pdf;
pub use render::{
    asset_hashes_to_csv, escape_html, findings_to_csv, measurements_to_csv, notes_to_csv, to_json,
};
