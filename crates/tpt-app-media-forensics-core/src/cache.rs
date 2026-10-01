//! Analysis cache (spec §54, §57).
//!
//! # Validity is conjunctive
//!
//! A cached result is usable only when the asset content, the analysis engine
//! version, the profile thresholds, and the rule set are *all* unchanged. The
//! key carries all four, and a lookup verifies every component rather than
//! trusting the filename.
//!
//! Omitting any one of them is the classic way a forensic tool serves stale
//! results. Tightening a rule threshold changes the profile fingerprint; if the
//! key ignored profiles, re-analysing would keep returning findings computed
//! under the looser threshold, and nobody would notice.
//!
//! # The cache is disposable, the case is not
//!
//! Entries live under `cache/`, the only directory [`crate::case_dir`]
//! allows to be removed. Deleting the cache must cost time, never data.
//!
//! # Writes are atomic
//!
//! An entry is written to a temporary file and renamed into place. A crash
//! mid-write therefore leaves either the previous entry or nothing — never a
//! truncated file that would be read back as valid.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tpt_app_media_forensics_model::{CacheKey, Finding, FindingStatus};

use crate::error::CoreError;

/// A cached analysis result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheEntry {
    /// The key this entry was computed under.
    ///
    /// Verified on read: a file whose contents disagree with its own name is
    /// discarded rather than trusted.
    pub key: CacheKey,
    /// Findings produced by that run.
    pub findings: Vec<Finding>,
    /// Rules that were evaluated.
    pub rule_ids: Vec<String>,
    /// Number of streams the analysis examined.
    pub stream_count: usize,
}

/// Reads and writes analysis results under a case's cache directory.
pub struct AnalysisCache {
    directory: PathBuf,
}

impl AnalysisCache {
    /// Opens the cache inside a case directory.
    ///
    /// The directory is created lazily on first write, so opening a cache for a
    /// read-only inspection does not modify the case.
    #[must_use]
    pub fn new(case_dir: &Path) -> Self {
        Self {
            directory: case_dir.join("cache"),
        }
    }

    /// Returns the cache directory.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Returns the path an entry for `key` would occupy.
    ///
    /// The filename is a digest of the key, so it is filesystem-safe
    /// regardless of what the key contains.
    #[must_use]
    pub fn entry_path(&self, key: &CacheKey) -> PathBuf {
        self.directory.join(format!("{}.json", key_digest(key)))
    }

    /// Reads a cached result.
    ///
    /// Returns `Ok(None)` for a miss, which includes an entry that is present
    /// but was computed under a different key. A miss is not an error.
    ///
    /// # Errors
    ///
    /// Returns an error only when the entry exists and cannot be read.
    pub fn load(&self, key: &CacheKey) -> Result<Option<CacheEntry>, CoreError> {
        let path = self.entry_path(key);
        if !path.is_file() {
            return Ok(None);
        }

        let bytes = std::fs::read(&path)
            .map_err(|e| CoreError::io("read cache entry", path.display().to_string(), e))?;

        match serde_json::from_slice::<CacheEntry>(&bytes) {
            // A file that does not parse, or whose recorded key disagrees with
            // the one we asked for, is treated as a miss and removed. Serving
            // it would mean trusting a file we cannot explain.
            Ok(entry) if entry.key == *key => Ok(Some(entry)),
            Ok(_) | Err(_) => {
                let _ = std::fs::remove_file(&path);
                Ok(None)
            }
        }
    }

    /// Stores a result, replacing any entry for the same key.
    ///
    /// # Errors
    ///
    /// Returns an error if the cache directory or the entry cannot be written.
    pub fn store(&self, entry: &CacheEntry) -> Result<(), CoreError> {
        std::fs::create_dir_all(&self.directory).map_err(|e| {
            CoreError::io(
                "create cache directory",
                self.directory.display().to_string(),
                e,
            )
        })?;

        let final_path = self.entry_path(&entry.key);
        let temp_path = self.directory.join("entry.tmp");

        let json = serde_json::to_vec_pretty(entry).map_err(|e| CoreError::Serialise {
            operation: "serialize cache entry",
            reason: e.to_string(),
        })?;

        std::fs::write(&temp_path, &json)
            .map_err(|e| CoreError::io("write cache entry", temp_path.display().to_string(), e))?;
        // Rename is atomic within a filesystem: a reader sees the old entry or
        // the new one, never a half-written file.
        std::fs::rename(&temp_path, &final_path).map_err(|e| {
            CoreError::io("commit cache entry", final_path.display().to_string(), e)
        })?;
        Ok(())
    }

    /// Returns the number of cached entries.
    ///
    /// # Errors
    ///
    /// Returns an error if the cache directory cannot be read.
    pub fn len(&self) -> Result<usize, CoreError> {
        if !self.directory.is_dir() {
            return Ok(0);
        }
        let count = std::fs::read_dir(&self.directory)
            .map_err(|e| {
                CoreError::io(
                    "read cache directory",
                    self.directory.display().to_string(),
                    e,
                )
            })?
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|e| e == "json"))
            .count();
        Ok(count)
    }

    /// Returns `true` when nothing is cached.
    ///
    /// # Errors
    ///
    /// Returns an error if the cache directory cannot be read.
    pub fn is_empty(&self) -> Result<bool, CoreError> {
        Ok(self.len()? == 0)
    }

    /// Applies a review decision to every cached copy of a finding.
    ///
    /// Review state is *not* part of the cache key: the same analysis under the
    /// same profile yields the same findings, and a reviewer's verdict must
    /// survive re-analysis rather than being recomputed (spec §66). Rewriting
    /// them on the way through keeps the reviewer's decision attached to the
    /// finding they judged.
    ///
    /// # Errors
    ///
    /// Returns an error if any affected entry cannot be rewritten.
    pub fn apply_review(
        &self,
        finding_id: &tpt_app_media_forensics_model::FindingId,
        status: FindingStatus,
        note: Option<String>,
    ) -> Result<usize, CoreError> {
        let mut updated = 0usize;
        if !self.directory.is_dir() {
            return Ok(updated);
        }

        let entries = std::fs::read_dir(&self.directory).map_err(|e| {
            CoreError::io(
                "read cache directory",
                self.directory.display().to_string(),
                e,
            )
        })?;

        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(mut cached) = serde_json::from_slice::<CacheEntry>(&bytes) else {
                continue;
            };

            let mut touched = false;
            for finding in &mut cached.findings {
                if finding.id == *finding_id {
                    finding.review(status, note.clone());
                    touched = true;
                }
            }
            if touched {
                self.store(&cached)?;
                updated += 1;
            }
        }
        Ok(updated)
    }
}

/// Returns a short, filesystem-safe digest of a cache key.
///
/// The key itself contains `:` separators and a 64-character hash; embedding it
/// in a filename would work but is needlessly awkward and risks exceeding
/// path limits once a directory is prefixed.
fn key_digest(key: &CacheKey) -> String {
    let digest = tpt_app_media_forensics_model::EntityId::new_derived(
        tpt_app_media_forensics_model::EntityKind::Analysis,
        &[key.to_key_string().as_bytes()],
    );
    tpt_app_media_forensics_model::asset::to_hex(&digest.value().to_be_bytes())
}
