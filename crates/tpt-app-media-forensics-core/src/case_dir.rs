//! Case directory layout and manifest (spec §53, §58).
//!
//! # A case is self-contained and portable
//!
//! Everything that constitutes the record lives under one directory, so a case
//! can be archived, copied to another machine, or handed to another party as a
//! unit.
//!
//! ```text
//! case.tptcase/
//!   manifest.json          case identity, assets, analyses, reports
//!   case.db                SQLite: findings, evidence, notes, review state
//!   assets/                references to sources (never copies)
//!   evidence/              extracted frames, audio excerpts, traces
//!   reports/               generated PDF / HTML / JSON / CSV
//!   cache/                 analysis cache, safe to delete
//! ```
//!
//! # Sources are referenced, not copied
//!
//! `assets/` holds metadata about the sources, not copies of them. Copying a
//! multi-gigabyte evidentiary file into a case directory by default would be
//! surprising and expensive, and the source must not be modified (spec §11).
//!
//! # Only `cache/` is disposable
//!
//! Deleting anything else destroys evidence or the record of the examination.
//! `clear_cache` is therefore the only operation that removes files, and it
//! refuses to touch anything outside the cache directory.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tpt_app_media_forensics_model::{Case, CaseId, EntityKind};

use crate::error::CoreError;
use crate::store::Store;

/// Directory name of the manifest file.
const MANIFEST_FILE: &str = "manifest.json";

/// Schema version of the on-disk manifest.
///
/// Bumped whenever the manifest layout changes incompatibly, so a newer build
/// can detect and refuse an older case rather than misreading it.
const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// An initialised case directory on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseDirectory {
    root: PathBuf,
}

impl CaseDirectory {
    /// Creates and initialises a new case directory.
    ///
    /// Fails if the directory already contains a manifest, so an existing case
    /// can never be silently overwritten.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created, if it already holds
    /// a case, or if the manifest cannot be written.
    pub fn create(root: impl Into<PathBuf>, case: &Case) -> Result<Self, CoreError> {
        let root = root.into();
        let dir = Self { root };

        if dir.manifest_path().exists() {
            return Err(CoreError::InvalidManifest {
                reason: format!(
                    "{} already contains a case; refusing to overwrite it",
                    dir.manifest_path().display()
                ),
            });
        }

        dir.ensure_layout()?;
        dir.write_manifest(case)?;
        dir.record_case_row(case)?;
        Ok(dir)
    }

    /// Writes the case's identity into the case database.
    ///
    /// The manifest is the authority on what a case *is*, but the database is what
    /// every read path goes through — findings, notes, reviews, reports. A case
    /// created without a row here is one that `only_case_id` reports as absent and
    /// that `load_report` refuses, even though the directory looks complete.
    ///
    /// Best-effort by design: a case whose database cannot be opened is still a
    /// valid case directory, and refusing to create one because SQLite is
    /// unavailable would make the on-disk record depend on a database that may be
    /// rebuilt later. The row is written when the case is next analysed.
    ///
    /// # Errors
    ///
    /// Returns an error if the case database is present but rejects the write, which
    /// means the case exists in two places that disagree.
    fn record_case_row(&self, case: &Case) -> Result<(), CoreError> {
        let store = match Store::open(self.root()) {
            Ok(store) => store,
            // A missing or unreadable database is not a failure here; see above.
            Err(_) => return Ok(()),
        };
        store
            .upsert_case(
                &case.id.to_string(),
                &case.name,
                case.description.as_deref(),
            )
            .map_err(|e| CoreError::InvalidManifest {
                reason: format!("recording the case in the database failed: {e}"),
            })
    }

    /// Opens an existing case directory.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::CaseDirectoryNotInitialised`] if no manifest is
    /// present.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, CoreError> {
        let dir = Self { root: root.into() };
        if !dir.manifest_path().exists() {
            return Err(CoreError::CaseDirectoryNotInitialised(dir.root.clone()));
        }
        Ok(dir)
    }

    /// Returns the case directory root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the path to the manifest file.
    #[must_use]
    pub fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST_FILE)
    }

    /// Returns the directory holding derived evidence artefacts.
    #[must_use]
    pub fn evidence_dir(&self) -> PathBuf {
        self.root.join("evidence")
    }

    /// Returns the directory holding generated reports.
    #[must_use]
    pub fn reports_dir(&self) -> PathBuf {
        self.root.join("reports")
    }

    /// Returns the disposable analysis cache directory.
    #[must_use]
    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    /// Creates every directory the layout requires.
    ///
    /// # Errors
    ///
    /// Returns an error if any directory cannot be created.
    pub fn ensure_layout(&self) -> Result<(), CoreError> {
        for dir in [
            self.root.clone(),
            self.root.join("assets"),
            self.evidence_dir(),
            self.reports_dir(),
            self.cache_dir(),
        ] {
            std::fs::create_dir_all(&dir).map_err(|e| {
                CoreError::io("create case directory", dir.display().to_string(), e)
            })?;
        }
        Ok(())
    }

    /// Writes the manifest for `case`.
    ///
    /// Written via a temporary file and renamed into place, so an interrupted
    /// write cannot leave a half-written manifest — which would render the case
    /// unreadable and look like tampering.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest cannot be serialised or written.
    pub fn write_manifest(&self, case: &Case) -> Result<(), CoreError> {
        let manifest = CaseManifest::from_case(case);
        let json =
            serde_json::to_string_pretty(&manifest).map_err(|e| CoreError::InvalidManifest {
                reason: e.to_string(),
            })?;

        let final_path = self.manifest_path();
        let temp_path = final_path.with_extension("json.tmp");
        std::fs::write(&temp_path, json.as_bytes())
            .map_err(|e| CoreError::io("write manifest", temp_path.display().to_string(), e))?;
        std::fs::rename(&temp_path, &final_path)
            .map_err(|e| CoreError::io("commit manifest", final_path.display().to_string(), e))
    }

    /// Reads the manifest.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest is unreadable, malformed, or written by
    /// an incompatible schema version.
    pub fn read_manifest(&self) -> Result<CaseManifest, CoreError> {
        let path = self.manifest_path();
        let text = std::fs::read_to_string(&path)
            .map_err(|e| CoreError::io("read manifest", path.display().to_string(), e))?;
        let manifest: CaseManifest =
            serde_json::from_str(&text).map_err(|e| CoreError::InvalidManifest {
                reason: format!("{}: {e}", path.display()),
            })?;

        if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(CoreError::InvalidManifest {
                reason: format!(
                    "schema version {} is not supported (expected {MANIFEST_SCHEMA_VERSION})",
                    manifest.schema_version
                ),
            });
        }
        Ok(manifest)
    }

    /// Removes cached analysis results.
    ///
    /// This is the only operation that deletes files, and it refuses to remove
    /// anything outside the cache directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the cache directory cannot be removed.
    pub fn clear_cache(&self) -> Result<(), CoreError> {
        let cache = self.cache_dir();
        if !cache.starts_with(&self.root) {
            return Err(CoreError::InvalidManifest {
                reason: format!("refusing to remove {} outside the case", cache.display()),
            });
        }
        if cache.exists() {
            std::fs::remove_dir_all(&cache)
                .map_err(|e| CoreError::io("clear cache", cache.display().to_string(), e))?;
        }
        Ok(())
    }
}
/// The on-disk case manifest (spec §58).
///
/// Records case identity and what the case contains. Serialised as pretty JSON
/// so a case can be inspected and understood without the application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseManifest {
    /// Manifest schema version, for forward compatibility.
    pub schema_version: u32,
    /// The case ID.
    pub case_id: String,
    /// The case name.
    pub name: String,
    /// The case description, if any.
    pub description: Option<String>,
    /// IDs of the assets in this case, in acquisition order.
    pub assets: Vec<String>,
    /// IDs of analyses recorded against this case.
    pub analyses: Vec<String>,
    /// IDs of findings raised in this case.
    pub findings: Vec<String>,
    /// IDs of retained evidence artefacts.
    pub evidence: Vec<String>,
    /// IDs of generated reports.
    pub reports: Vec<String>,
}

impl CaseManifest {
    /// Builds a manifest snapshot from a [`Case`].
    #[must_use]
    pub fn from_case(case: &Case) -> Self {
        Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            case_id: case.id.to_string(),
            name: case.name.clone(),
            description: case.description.clone(),
            assets: case.assets.iter().map(ToString::to_string).collect(),
            analyses: case.analyses.iter().map(ToString::to_string).collect(),
            findings: case.findings.iter().map(ToString::to_string).collect(),
            evidence: case.evidence.iter().map(ToString::to_string).collect(),
            reports: case.reports.iter().map(ToString::to_string).collect(),
        }
    }

    /// Returns the case ID, parsed back into its typed form.
    ///
    /// Returns `None` if the stored value does not belong to a case, which
    /// would indicate a corrupt manifest.
    #[must_use]
    pub fn parsed_case_id(&self) -> Option<CaseId> {
        // The manifest stores the canonical string form, which is prefixed with
        // the entity kind; parsing requires the raw value, so the prefix is
        // stripped and the kind is re-attached by the ID constructor.
        let raw = self.case_id.strip_prefix("case:")?;
        tpt_app_media_forensics_model::EntityId::from_canonical_str(EntityKind::Case, raw)
            .and_then(CaseId::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(name: &str) -> Case {
        Case::new(name, Some("A test case".to_owned()))
    }

    #[test]
    fn create_initialises_the_full_layout() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("case.tptcase");
        let case_dir = CaseDirectory::create(&root, &case("Alpha")).expect("creates");

        assert!(root.join("manifest.json").exists());
        assert!(root.join("assets").is_dir());
        assert!(root.join("evidence").is_dir());
        assert!(root.join("reports").is_dir());
        assert!(root.join("cache").is_dir());
        assert_eq!(case_dir.root(), root.as_path());
    }

    #[test]
    fn create_refuses_to_overwrite_an_existing_case() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("case.tptcase");
        CaseDirectory::create(&root, &case("Alpha")).expect("first create");

        let result = CaseDirectory::create(&root, &case("Beta"));
        assert!(
            result.is_err(),
            "clobbering an existing case would destroy evidence"
        );
    }

    #[test]
    fn manifest_round_trips() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("case.tptcase");
        let case_dir = CaseDirectory::create(&root, &case("Alpha")).expect("creates");

        let written = case_dir.read_manifest().expect("reads");
        assert_eq!(written.name, "Alpha");
        assert_eq!(written.description.as_deref(), Some("A test case"));
        assert_eq!(written.case_id, case("Alpha").id.to_string());
        assert_eq!(written.parsed_case_id(), Some(case("Alpha").id));
    }

    #[test]
    fn manifest_write_is_deterministic() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("case.tptcase");
        let case_dir = CaseDirectory::create(&root, &case("Alpha")).expect("creates");

        case_dir.write_manifest(&case("Alpha")).expect("writes");
        let first = std::fs::read_to_string(case_dir.manifest_path()).expect("reads");

        case_dir
            .write_manifest(&case("Alpha"))
            .expect("writes again");
        let second = std::fs::read_to_string(case_dir.manifest_path()).expect("reads");

        assert_eq!(
            first, second,
            "manifest bytes must be reproducible (spec §77)"
        );
    }

    #[test]
    fn write_leaves_no_temporary_file_behind() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("case.tptcase");
        let case_dir = CaseDirectory::create(&root, &case("Alpha")).expect("creates");

        case_dir.write_manifest(&case("Alpha")).expect("rewrites");

        assert!(
            !root.join("manifest.json.tmp").exists(),
            "the atomic-rename temp file must not survive"
        );
        assert!(
            case_dir.manifest_path().is_file(),
            "the real manifest must exist"
        );
    }

    #[test]
    fn open_requires_an_initialised_directory() {
        let dir = tempfile::tempdir().expect("temp dir");
        let result = CaseDirectory::open(dir.path());
        assert!(matches!(
            result,
            Err(CoreError::CaseDirectoryNotInitialised(_))
        ));
    }

    #[test]
    fn open_succeeds_after_create() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("case.tptcase");
        CaseDirectory::create(&root, &case("Alpha")).expect("creates");
        assert!(CaseDirectory::open(&root).is_ok());
    }

    #[test]
    fn unsupported_schema_version_is_rejected() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("case.tptcase");
        let case_dir = CaseDirectory::create(&root, &case("Alpha")).expect("creates");

        let path = case_dir.manifest_path();
        let text = std::fs::read_to_string(&path).expect("reads");
        std::fs::write(
            &path,
            text.replace("\"schema_version\": 1", "\"schema_version\": 99"),
        )
        .expect("writes");

        let result = case_dir.read_manifest();
        assert!(
            result.is_err(),
            "a newer manifest must not be misread as current"
        );
    }

    #[test]
    fn malformed_manifest_is_reported_not_panicked() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("case.tptcase");
        let case_dir = CaseDirectory::create(&root, &case("Alpha")).expect("creates");

        std::fs::write(case_dir.manifest_path(), b"{ not json").expect("writes");
        assert!(matches!(
            case_dir.read_manifest(),
            Err(CoreError::InvalidManifest { .. })
        ));
    }

    #[test]
    fn clear_cache_removes_only_the_cache() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("case.tptcase");
        let case_dir = CaseDirectory::create(&root, &case("Alpha")).expect("creates");

        std::fs::write(case_dir.cache_dir().join("entry.bin"), b"cached").expect("writes");
        case_dir.clear_cache().expect("clears");

        assert!(!case_dir.cache_dir().exists(), "cache should be gone");
        assert!(
            case_dir.manifest_path().exists(),
            "clearing the cache must never remove the record"
        );
        assert!(case_dir.evidence_dir().is_dir());
    }

    #[test]
    fn evidence_paths_are_inside_the_case() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("case.tptcase");
        let case_dir = CaseDirectory::create(root, &case("Alpha")).expect("creates");

        for path in [
            case_dir.evidence_dir(),
            case_dir.reports_dir(),
            case_dir.cache_dir(),
        ] {
            assert!(
                path.starts_with(case_dir.root()),
                "{path:?} escaped the case directory"
            );
        }
    }
}
