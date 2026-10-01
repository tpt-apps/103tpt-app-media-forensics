//! Batch analysis over a directory of media (spec §48-49).
//!
//! Runs the engine over every media file beneath a directory into a single case,
//! which is the shape of real intake work: a handover arrives as a folder, not as
//! individual files.
//!
//! # One failure does not stop the batch
//!
//! A corrupt file is a normal outcome, not an abort condition. Each file is
//! analysed independently and a failure is recorded against that file with its
//! path, so a single unreadable item cannot hide the results for the other
//! thousand (spec §75).
//!
//! # Everything is skipped explicitly
//!
//! The case directory is excluded from the scan. Otherwise writing evidence into
//! the case while scanning it would feed the engine its own output on the next
//! run, which is both wasteful and confusing.
//!
//! # Ordering is deterministic
//!
//! Files are visited in sorted path order so two runs over the same tree produce
//! the same sequence of results (spec §77).

use std::path::{Path, PathBuf};

use tpt_app_media_forensics_container::probe::detect;
use tpt_app_media_forensics_model::Finding;

use crate::case_dir::CaseDirectory;
use crate::error::CoreError;
use crate::pipeline::{AnalysisEngine, AnalysisOutcome};

/// Extensions considered plausible media during a scan.
///
/// An extension is a claim, not evidence, so a file on this list is still
/// verified by signature before being analysed. The list exists so an intake
/// folder full of `.txt` and `.xlsx` files is not read in full.
const CANDIDATE_EXTENSIONS: [&str; 14] = [
    "mp4", "m4v", "mov", "3gp", "mkv", "webm", "avi", "wav", "aiff", "aif", "flac", "ogg", "m4a",
    "mpg",
];

/// How a single file fared.
#[derive(Debug)]
pub enum FileOutcome {
    /// The file was analysed.
    Analysed(Box<AnalysisOutcome>),
    /// The file could not be analysed.
    Failed {
        /// Why it failed, already labelled with the path.
        reason: String,
    },
    /// The file was skipped, with the reason.
    Skipped {
        /// Why it was skipped.
        reason: String,
    },
}

impl FileOutcome {
    /// Returns the findings an outcome produced, if any.
    #[must_use]
    pub fn findings(&self) -> &[Finding] {
        match self {
            Self::Analysed(outcome) => &outcome.findings,
            Self::Failed { .. } | Self::Skipped { .. } => &[],
        }
    }
}

/// The result of a batch run.
#[derive(Debug)]
pub struct BatchOutcome {
    /// One entry per file visited, in path order.
    pub results: Vec<(PathBuf, FileOutcome)>,
}

impl BatchOutcome {
    /// Counts the files that were analysed, failed, and skipped.
    #[must_use]
    pub fn counts(&self) -> (usize, usize, usize) {
        let mut analysed = 0;
        let mut failed = 0;
        let mut skipped = 0;
        for (_, outcome) in &self.results {
            match outcome {
                FileOutcome::Analysed(_) => analysed += 1,
                FileOutcome::Failed { .. } => failed += 1,
                FileOutcome::Skipped { .. } => skipped += 1,
            }
        }
        (analysed, failed, skipped)
    }

    /// Every finding across every analysed file.
    #[must_use]
    pub fn findings(&self) -> Vec<&Finding> {
        self.results
            .iter()
            .flat_map(|(_, o)| o.findings())
            .collect()
    }
}

/// Analyses every media file beneath `directory` into `case_dir`.
///
/// # Errors
///
/// Returns an error only if the case directory is unusable. Per-file failures
/// are recorded in the returned outcome rather than raised.
pub fn run(
    engine: &AnalysisEngine,
    directory: &Path,
    case_dir: &CaseDirectory,
) -> Result<BatchOutcome, CoreError> {
    let files = scan(directory, case_dir.root())?;
    let mut results = Vec::with_capacity(files.len());

    for path in files {
        let outcome = match engine.analyse(&path, case_dir) {
            Ok(outcome) => FileOutcome::Analysed(Box::new(outcome)),
            // `{error}` carries the context the pipeline already attached, so a
            // failure always names the file it came from.
            Err(error) => FileOutcome::Failed {
                reason: format!("{}: {error}", path.display()),
            },
        };
        results.push((path, outcome));
    }

    Ok(BatchOutcome { results })
}

/// Finds candidate media files beneath `directory`, in sorted order.
///
/// # Errors
///
/// Returns an error if a directory cannot be read.
fn scan(directory: &Path, case_root: &Path) -> Result<Vec<PathBuf>, CoreError> {
    let mut found = Vec::new();
    walk(directory, case_root, &mut found)?;
    // Sorted so two runs over an unchanged tree visit files in the same order.
    found.sort();
    Ok(found)
}

/// Recursively collects candidate files, skipping the case directory.
fn walk(directory: &Path, case_root: &Path, out: &mut Vec<PathBuf>) -> Result<(), CoreError> {
    let entries = std::fs::read_dir(directory)
        .map_err(|e| CoreError::io("read directory", directory.display().to_string(), e))?;

    for entry in entries {
        let entry = entry.map_err(|e| {
            CoreError::io("read directory entry", directory.display().to_string(), e)
        })?;
        let path = entry.path();
        let is_dir = entry
            .file_type()
            .map_err(|e| CoreError::io("stat entry", path.display().to_string(), e))?
            .is_dir();

        if is_dir {
            // Never descend into the case being written to, or into a cache
            // directory: both would feed the engine its own output.
            if is_same_or_within(&path, case_root) || is_cache_dir(&path) {
                continue;
            }
            walk(&path, case_root, out)?;
            continue;
        }

        if is_candidate(&path) {
            out.push(path);
        }
    }
    Ok(())
}

/// Returns `true` if `path` is the case root or lives inside it.
fn is_same_or_within(path: &Path, case_root: &Path) -> bool {
    let path = normalise(path);
    let root = normalise(case_root);
    path == root || path.starts_with(&root)
}

/// Normalises a path for comparison without touching the filesystem.
fn normalise(path: &Path) -> PathBuf {
    path.components()
        .fold(PathBuf::new(), |mut acc, component| {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    acc.pop();
                }
                other => acc.push(other.as_os_str()),
            }
            acc
        })
}

/// Returns `true` for a directory named like a cache.
fn is_cache_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("cache"))
}

/// Returns `true` if a path has a media extension and a recognised signature.
///
/// The extension check is a cheap filter; the signature is the evidence.
fn is_candidate(path: &Path) -> bool {
    let has_extension = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| CANDIDATE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()));
    if !has_extension {
        return false;
    }

    let Ok(bytes) = std::fs::read(path) else {
        // Unreadable, but still a candidate: let the engine report the failure.
        return true;
    };
    let header = &bytes[..bytes.len().min(16 * 1024)];
    detect(header) != tpt_app_media_forensics_container::ContainerFormat::Unknown
}
