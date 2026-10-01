//! Evidence storage (spec §32, §33).
//!
//! # Write, then verify
//!
//! An artefact is written, then read back and re-hashed before it is recorded
//! as verified. Recording a hash taken from the bytes *about to be written*
//! would assert something unchecked; re-reading proves the bytes on disk match
//! what the record claims.
//!
//! This is the difference between evidence that can be relied on and a claim
//! that cannot.
//!
//! # Paths are derived, never accepted
//!
//! An artefact's path is built from a sanitised name inside the case
//! directory. No caller-supplied string becomes a path component, so a name
//! containing `..` or an absolute prefix cannot escape the case.
//!
//! # Provenance is recorded
//!
//! Whether an artefact is a direct copy, a lossless extract, a lossy
//! transform, or derived changes how much it is worth as evidence. The
//! provenance travels with the record.

use std::io::Write;
use std::path::{Path, PathBuf};

use tpt_app_media_forensics_model::{
    AssetId, Evidence, EvidenceIntegrity, EvidenceKind, Provenance,
};

/// Sanitises a caller-supplied name into a safe relative path.
///
/// Each component is reduced to letters, digits, dash, underscore, and single
/// dots; separators in the input are honoured as path separators so
/// `frames/shot.png` organises as intended. Every component is then checked to
/// be neither empty nor `.`/`..`.
///
/// Sanitising per component is what makes traversal impossible: `..` never
/// survives as a component, so `../../etc/passwd` cannot escape `evidence/`.
/// An absolute input cannot escape either, because the first component of an
/// absolute path is empty or a drive designator and is dropped.
fn sanitise(name: &str) -> String {
    let mut parts: Vec<String> = Vec::new();

    for component in name.split(['/', '\\']) {
        let mapped: String = component
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        // Collapse dot runs so `..` cannot survive as a component, then trim.
        let mut collapsed = String::new();
        let mut last_was_dot = false;
        for c in mapped.chars() {
            if c == '.' {
                if !last_was_dot {
                    collapsed.push('.');
                }
                last_was_dot = true;
            } else {
                collapsed.push(c);
                last_was_dot = false;
            }
        }
        let trimmed = collapsed.trim_matches('.').to_owned();
        if trimmed.is_empty() {
            continue;
        }
        // Bound each component so a long caption cannot exhaust path limits.
        parts.push(trimmed.chars().take(64).collect());
    }

    if parts.is_empty() {
        "artifact".to_owned()
    } else {
        parts.join("/")
    }
}

/// Writes evidence artefacts into a case directory.
pub struct EvidenceStore {
    case_dir: PathBuf,
}

impl EvidenceStore {
    /// Opens a store rooted at a case directory.
    #[must_use]
    pub fn new(case_dir: &Path) -> Self {
        Self {
            case_dir: case_dir.to_path_buf(),
        }
    }

    /// Returns the `evidence/` directory.
    #[must_use]
    pub fn root(&self) -> PathBuf {
        self.case_dir.join("evidence")
    }

    /// Writes an artefact and returns its evidence record, verified.
    ///
    /// `relative_name` is sanitised and placed under `evidence/`. The record's
    /// `relative_path` is relative to the case root, never absolute, so a case
    /// can be moved or archived without breaking its references.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created, the artefact
    /// cannot be written, or the verification read fails.
    pub fn write_verified(
        &self,
        asset_id: AssetId,
        kind: EvidenceKind,
        provenance: Provenance,
        relative_name: &str,
        contents: &[u8],
    ) -> Result<Evidence, EvidenceError> {
        let relative_path = format!("evidence/{}", sanitise(relative_name));
        let path = self.case_dir.join(&relative_path);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| EvidenceError::Io {
                operation: format!("create {parent:?}"),
                source: e,
            })?;
        }

        let mut file = std::fs::File::create(&path).map_err(|e| EvidenceError::Io {
            operation: format!("create {path:?}"),
            source: e,
        })?;
        file.write_all(contents).map_err(|e| EvidenceError::Io {
            operation: format!("write {path:?}"),
            source: e,
        })?;
        // Flush to the OS before re-reading: buffered bytes would not be on
        // disk yet, and the verification read would see a short file.
        file.flush().map_err(|e| EvidenceError::Io {
            operation: format!("flush {path:?}"),
            source: e,
        })?;
        drop(file);

        // Re-read from disk and hash what is actually there.
        let stored = std::fs::read(&path).map_err(|e| EvidenceError::Io {
            operation: format!("verify {path:?}"),
            source: e,
        })?;

        if stored != contents {
            return Err(EvidenceError::VerificationFailed {
                path: relative_path.clone(),
                reason: "bytes on disk differ from those written".to_owned(),
            });
        }

        let hashes = hash_set(&stored);
        // `Evidence::new` deliberately records `verified: false`; the flag is
        // raised only here, after the bytes have been read back and compared.
        let mut evidence = Evidence::new(
            asset_id,
            kind,
            provenance,
            relative_path,
            EvidenceIntegrity {
                size_bytes: stored.len() as u64,
                hashes,
                verified: false,
            },
        );
        evidence.mark_verified();
        Ok(evidence)
    }

    /// Re-verifies a stored artefact against its record.
    ///
    /// Returns `false` for anything it cannot confirm: a missing file, a size
    /// change, or a hash mismatch. "Cannot confirm" is never reported as
    /// "verified".
    pub fn reverify(&self, evidence: &Evidence) -> bool {
        let path = self.case_dir.join(&evidence.relative_path);
        let Ok(bytes) = std::fs::read(&path) else {
            return false;
        };
        evidence.verify(bytes.len() as u64, &hash_set(&bytes))
    }
}

/// Computes SHA-256 and BLAKE3 over some bytes.
fn hash_set(bytes: &[u8]) -> tpt_app_media_forensics_model::HashSet {
    use sha2::Digest as _;
    use tpt_app_media_forensics_model::{FileHash, HashAlgorithm, HashSet};

    let mut hashes = HashSet::default();
    hashes.insert(FileHash::from_bytes(
        HashAlgorithm::Sha256,
        &sha2::Sha256::digest(bytes),
    ));
    hashes.insert(FileHash::from_bytes(
        HashAlgorithm::Blake3,
        blake3::hash(bytes).as_bytes(),
    ));
    hashes
}

/// An error raised while storing evidence.
#[derive(Debug, thiserror::Error)]
pub enum EvidenceError {
    /// A filesystem operation failed.
    #[error("{operation} failed: {source}")]
    Io {
        /// What was being attempted.
        operation: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },

    /// The bytes on disk did not match what was written.
    #[error("evidence verification failed for {path}: {reason}")]
    VerificationFailed {
        /// The artefact's path within the case.
        path: String,
        /// What differed.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &tempfile::TempDir) -> EvidenceStore {
        EvidenceStore::new(dir.path())
    }

    #[test]
    fn a_written_artefact_is_verified() {
        let dir = tempfile::tempdir().expect("temp dir");
        let evidence = store(&dir)
            .write_verified(
                AssetId::new_derived(&["a"]),
                EvidenceKind::ExtractedFrame,
                Provenance::LosslessExtract,
                "frames/frame-0001.png",
                b"pretend this is a frame",
            )
            .expect("writes");

        assert!(evidence.integrity.verified);
        assert_eq!(evidence.relative_path, "evidence/frames/frame-0001.png");
        assert!(evidence.integrity.hashes.is_complete());
    }

    #[test]
    fn stored_paths_are_always_relative_and_contained() {
        let dir = tempfile::tempdir().expect("temp dir");
        let evidence = store(&dir)
            .write_verified(
                AssetId::new_derived(&["a"]),
                EvidenceKind::Other,
                Provenance::Derived,
                "../../escape.txt",
                b"x",
            )
            .expect("writes");

        assert!(
            !evidence.relative_path.contains(".."),
            "traversal must not survive: {}",
            evidence.relative_path
        );
        assert!(evidence.relative_path.starts_with("evidence/"));
        assert!(dir.path().join(&evidence.relative_path).exists());
    }

    #[test]
    fn sanitising_removes_separators_and_traversal() {
        assert!(!sanitise("../../etc/passwd").contains(".."));
        assert_eq!(sanitise("a/b\\c"), "a/b/c");
        assert_eq!(sanitise(""), "artifact");
        assert_eq!(sanitise("..."), "artifact");
        assert_eq!(sanitise(""), "artifact");
    }

    #[test]
    fn an_empty_artefact_is_still_verified() {
        let dir = tempfile::tempdir().expect("temp dir");
        let evidence = store(&dir)
            .write_verified(
                AssetId::new_derived(&["a"]),
                EvidenceKind::Other,
                Provenance::Derived,
                "empty.bin",
                b"",
            )
            .expect("writes");
        assert_eq!(evidence.integrity.size_bytes, 0);
        assert!(evidence.integrity.verified);
    }

    #[test]
    fn reverification_succeeds_for_an_untouched_artefact() {
        let dir = tempfile::tempdir().expect("temp dir");
        let s = store(&dir);
        let evidence = s
            .write_verified(
                AssetId::new_derived(&["a"]),
                EvidenceKind::Other,
                Provenance::Derived,
                "x.bin",
                b"payload",
            )
            .expect("writes");
        assert!(s.reverify(&evidence));
    }

    #[test]
    fn reverification_fails_after_the_artefact_is_altered() {
        let dir = tempfile::tempdir().expect("temp dir");
        let s = store(&dir);
        let evidence = s
            .write_verified(
                AssetId::new_derived(&["a"]),
                EvidenceKind::Other,
                Provenance::Derived,
                "x.bin",
                b"payload",
            )
            .expect("writes");

        std::fs::write(dir.path().join(&evidence.relative_path), b"tampered").expect("writes");
        assert!(!s.reverify(&evidence), "altered evidence must not verify");
    }

    #[test]
    fn reverification_of_a_missing_artefact_is_false_not_an_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let s = store(&dir);
        let evidence = s
            .write_verified(
                AssetId::new_derived(&["a"]),
                EvidenceKind::Other,
                Provenance::Derived,
                "x.bin",
                b"payload",
            )
            .expect("writes");

        std::fs::remove_file(dir.path().join(&evidence.relative_path)).expect("removes");
        assert!(!s.reverify(&evidence));
    }

    #[test]
    fn provenance_is_recorded_with_the_artefact() {
        let dir = tempfile::tempdir().expect("temp dir");
        let evidence = store(&dir)
            .write_verified(
                AssetId::new_derived(&["a"]),
                EvidenceKind::ExtractedFrame,
                Provenance::LossyTransform,
                "thumb.jpg",
                b"jpeg",
            )
            .expect("writes");
        assert_eq!(evidence.provenance, Provenance::LossyTransform);
        assert_eq!(evidence.kind, EvidenceKind::ExtractedFrame);
    }

    #[test]
    fn a_long_name_is_bounded() {
        let dir = tempfile::tempdir().expect("temp dir");
        let long = "x".repeat(500);
        let evidence = store(&dir)
            .write_verified(
                AssetId::new_derived(&["a"]),
                EvidenceKind::Other,
                Provenance::Derived,
                &long,
                b"x",
            )
            .expect("writes");
        assert!(evidence.relative_path.len() < 120, "path must stay bounded");
    }
}
