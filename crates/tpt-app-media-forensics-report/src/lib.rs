//! Report generation (spec §59-§63).
//!
//! Renders findings and measurements to PDF, HTML, JSON, and CSV, and embeds
//! the reproducibility data — software version, analysis version, profile and
//! rule-set fingerprints — plus the required forensic disclaimers.

#![forbid(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
