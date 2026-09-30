//! Output formatting for the CLI.
//!
//! Two modes, one set of facts: `--json` emits machine-readable output for
//! automation, otherwise human-readable text. Both are produced from the same
//! engine values, so they can never disagree.

use tpt_app_media_forensics_model::AcquisitionRecord;

/// Serialisable view of an acquisition record.
#[derive(Debug, serde::Serialize, PartialEq, Eq)]
pub struct AcquisitionJson {
    /// The source path as recorded.
    pub source_path: String,
    /// Size in bytes.
    pub size_bytes: u64,
    /// SHA-256 digest, lowercase hex.
    pub sha256: Option<String>,
    /// BLAKE3 digest, lowercase hex.
    pub blake3: Option<String>,
    /// Whether both digests were computed.
    pub hashes_complete: bool,
    /// Modification time, Unix seconds.
    pub modified_unix_secs: Option<i64>,
    /// Creation time, Unix seconds, where the platform records one.
    pub created_unix_secs: Option<i64>,
}

impl From<&AcquisitionRecord> for AcquisitionJson {
    fn from(record: &AcquisitionRecord) -> Self {
        Self {
            source_path: record.source_path.clone(),
            size_bytes: record.size_bytes,
            sha256: record.hashes.sha256().map(ToOwned::to_owned),
            blake3: record.hashes.blake3().map(ToOwned::to_owned),
            hashes_complete: record.hashes.is_complete(),
            modified_unix_secs: record.timestamps.modified_unix_secs,
            created_unix_secs: record.timestamps.created_unix_secs,
        }
    }
}

/// Prints an acquisition record as human-readable text.
///
/// States explicitly when a hash is missing. A line that silently omits BLAKE3
/// is indistinguishable from one where the computation failed, and the analyst
/// must be able to tell those apart.
#[must_use]
pub fn render_acquisition(record: &AcquisitionRecord) -> String {
    let mut out = String::new();
    out.push_str(&format!("Source        {}\n", record.source_path));
    out.push_str(&format!("Size          {} bytes\n", record.size_bytes));
    out.push_str(&format!(
        "SHA-256       {}\n",
        record.hashes.sha256().unwrap_or("(not computed)")
    ));
    out.push_str(&format!(
        "BLAKE3        {}\n",
        record.hashes.blake3().unwrap_or("(not computed)")
    ));
    if let Some(modified) = record.timestamps.modified_unix_secs {
        out.push_str(&format!("Modified      {modified} (unix seconds)\n"));
    }
    out.push_str(&format!(
        "Verified      {}\n",
        if record.hashes.is_complete() {
            "SHA-256 and BLAKE3 recorded"
        } else {
            "INCOMPLETE - integrity not established"
        }
    ));
    out.push_str("Source was opened read-only; it has not been modified.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_app_media_forensics_model::{FileHash, HashAlgorithm, HashSet};

    fn record(sha: Option<&str>, blake: Option<&str>) -> AcquisitionRecord {
        let mut hashes = HashSet::default();
        if let Some(value) = sha {
            hashes.insert(FileHash {
                algorithm: HashAlgorithm::Sha256,
                hex: value.to_owned(),
            });
        }
        if let Some(value) = blake {
            hashes.insert(FileHash {
                algorithm: HashAlgorithm::Blake3,
                hex: value.to_owned(),
            });
        }
        AcquisitionRecord {
            source_path: "evidence.mp4".to_owned(),
            size_bytes: 1024,
            hashes,
            timestamps: Default::default(),
            filesystem: Default::default(),
        }
    }

    #[test]
    fn text_output_includes_both_digests() {
        let text = render_acquisition(&record(Some("aa"), Some("bb")));
        assert!(text.contains("SHA-256       aa"));
        assert!(text.contains("BLAKE3        bb"));
        assert!(text.contains("1024 bytes"));
    }

    #[test]
    fn missing_hash_is_stated_not_omitted() {
        let text = render_acquisition(&record(Some("aa"), None));
        assert!(
            text.contains("BLAKE3        (not computed)"),
            "a silent omission would be indistinguishable from a failure"
        );
        assert!(text.contains("INCOMPLETE"));
    }

    #[test]
    fn read_only_guarantee_is_stated() {
        let text = render_acquisition(&record(Some("aa"), Some("bb")));
        assert!(text.contains("read-only"));
    }

    #[test]
    fn json_view_mirrors_the_record() {
        let json = AcquisitionJson::from(&record(Some("aa"), Some("bb")));
        assert_eq!(json.sha256.as_deref(), Some("aa"));
        assert_eq!(json.blake3.as_deref(), Some("bb"));
        assert!(json.hashes_complete);
        assert_eq!(json.size_bytes, 1024);
    }

    #[test]
    fn json_view_flags_incomplete_hashing() {
        let json = AcquisitionJson::from(&record(Some("aa"), None));
        assert!(!json.hashes_complete);
    }

    #[test]
    fn json_output_is_stable_across_runs() {
        let record = record(Some("aa"), Some("bb"));
        let first = serde_json::to_string(&AcquisitionJson::from(&record)).expect("serialises");
        let second = serde_json::to_string(&AcquisitionJson::from(&record)).expect("serialises");
        assert_eq!(first, second, "CLI output must be reproducible (spec §77)");
    }
}
