//! Evidence bundle export (spec §62).
//!
//! Writes the deliverables a recipient needs, plus a manifest carrying the
//! SHA-256 of every generated file. The manifest is what makes the bundle
//! self-verifying: a recipient can confirm nothing was altered in transit
//! without re-running the analysis.
//!
//! # Writes only inside the target directory
//!
//! Every output path is derived from the bundle directory and a fixed file
//! name. Nothing is taken from the report's content, so a crafted report
//! cannot direct the writer outside the directory it was given.

use std::path::{Path, PathBuf};

use crate::error::ReportError;
use crate::html::to_html;
use crate::model::Report;
use crate::render::{asset_hashes_to_csv, findings_to_csv, measurements_to_csv, to_json};

/// One file written into the bundle.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BundleEntry {
    /// File name, relative to the bundle directory.
    pub name: String,
    /// Size in bytes.
    pub size_bytes: u64,
    /// SHA-256 of the written contents, lowercase hex.
    pub sha256: String,
}

/// A manifest of everything written into the bundle.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BundleManifest {
    /// Schema version of the bundle manifest.
    pub schema_version: u32,
    /// Case the bundle describes.
    pub case_id: String,
    /// Files written, in a deterministic order.
    pub files: Vec<BundleEntry>,
}

/// Writes the evidence bundle for `report` into `directory`.
///
/// # Errors
///
/// Returns an error if the directory cannot be created or a file cannot be
/// written.
pub fn write_bundle(report: &Report, directory: &Path) -> Result<BundleManifest, ReportError> {
    std::fs::create_dir_all(directory).map_err(|e| io("create bundle directory", e))?;

    let outputs: [(&str, String); 5] = [
        ("case-data.json", to_json(report)?),
        ("case-report.html", to_html(report)),
        ("findings.csv", findings_to_csv(report)),
        ("measurements.csv", measurements_to_csv(report)),
        ("asset-hashes.csv", asset_hashes_to_csv(report)),
    ];

    let mut files = Vec::with_capacity(outputs.len() + 1);
    for (name, contents) in outputs {
        let written = write_file(directory, name, &contents)?;
        files.push(written);
    }

    let manifest = BundleManifest {
        schema_version: 1,
        case_id: report.case_id.clone(),
        files,
    };
    // The manifest itself is written last and excluded from its own listing:
    // it cannot contain its own hash.
    let json = serde_json::to_string_pretty(&manifest).map_err(|e| ReportError::Render {
        format: "json",
        reason: e.to_string(),
    })?;
    std::fs::write(directory.join("bundle-manifest.json"), json.as_bytes())
        .map_err(|e| io("write bundle manifest", e))?;

    Ok(manifest)
}

/// Writes one file and returns its manifest entry.
fn write_file(directory: &Path, name: &str, contents: &str) -> Result<BundleEntry, ReportError> {
    let path = bundle_path(directory, name);
    std::fs::write(&path, contents.as_bytes()).map_err(|e| io("write bundle file", e))?;
    Ok(BundleEntry {
        name: name.to_owned(),
        size_bytes: contents.len() as u64,
        sha256: sha256_hex(contents.as_bytes()),
    })
}

/// Resolves a bundle file name to a path inside the directory.
///
/// `name` is a compile-time constant at every call site, never report
/// content. The `file_name` call is a belt-and-braces guard so a future caller
/// cannot introduce a traversal.
fn bundle_path(directory: &Path, name: &str) -> PathBuf {
    directory.join(Path::new(name).file_name().unwrap_or_default())
}

/// Computes the SHA-256 of some bytes, lowercase hex.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    tpt_app_media_forensics_model::asset::to_hex(&sha2::Sha256::digest(bytes))
}

/// Wraps an I/O error for the report layer.
fn io(operation: &'static str, source: std::io::Error) -> ReportError {
    ReportError::Render {
        format: "bundle",
        reason: format!("{operation} failed: {source}"),
    }
}
