//! Errors crossing the IPC boundary.
//!
//! # Every failure is a value, because a panic here is a crash
//!
//! Spec §75 and §96 require that malformed media cannot crash the application.
//! That obligation reaches the GUI differently than it reaches the engine: the
//! engine returns partial results, but a `#[tauri::command]` that panics takes
//! down the whole window with it, taking the analyst's unsaved screen state and
//! the case they were reading with it. So nothing in this crate unwinds on
//! attacker-controlled input. Every fallible path returns [`ShellError`], which
//! serialises to a tagged string the frontend can render.
//!
//! # Errors are already written for a human
//!
//! [`ShellError`] carries a finished sentence rather than a code for the UI to
//! translate. That is a deliberate inversion of the usual layering: a message
//! authored in the engine is available to the CLI, the GUI, and a log at the
//! same time, whereas a message authored in the frontend is available to
//! exactly one frontend. The cost is that a wording change needs a rebuild,
//! which is the right trade for a forensic tool whose output is read by people
//! deciding whether to trust a file.

use serde::{Deserialize, Serialize};

use tpt_app_media_forensics_core::CoreError;

/// A failure surfaced to the user instead of a crash.
///
/// Serialises as `{"kind": "...", "message": "..."}` so the frontend can
/// distinguish a refused operation from an unexpected one without matching on
/// message text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellError {
    /// A stable tag identifying the class of failure.
    pub kind: ErrorKind,
    /// The sentence shown to the analyst.
    pub message: String,
}

impl ShellError {
    /// Builds an error from a class and a message.
    #[must_use]
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ShellError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ShellError {}

/// The result type every command returns.
pub type ShellResult<T> = Result<T, ShellError>;

/// The class of a [`ShellError`].
///
/// Separate from the message so the frontend can react differently — an
/// unopened case is a routing problem, a corrupt file is a finding — without
/// parsing English.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// No case directory is open.
    NoCaseOpen,
    /// The path is not an initialised case directory.
    NotACase,
    /// The operation was cancelled by the analyst.
    Cancelled,
    /// The source file could not be read or analysed.
    UnreadableSource,
    /// The requested thing does not exist in this case.
    NotFound,
    /// The caller passed something the engine cannot work with.
    InvalidRequest,
    /// The case database or case directory could not be written.
    Storage,
    /// The analysis worker died, or a file defeated it.
    AnalysisFailed,
}

impl ErrorKind {
    /// Returns the stable tag used in JSON output.
    ///
    /// Present so a frontend can log or branch on a class without matching on
    /// the human-facing message. Distinct from the `serde` representation on
    /// purpose: renaming a displayed label must not break whatever switches on
    /// it, and these two are allowed to move independently.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::NoCaseOpen => "no_case_open",
            Self::NotACase => "not_a_case",
            Self::Cancelled => "cancelled",
            Self::UnreadableSource => "unreadable_source",
            Self::NotFound => "not_found",
            Self::InvalidRequest => "invalid_request",
            Self::Storage => "storage",
            Self::AnalysisFailed => "analysis_failed",
        }
    }
}

impl From<CoreError> for ShellError {
    /// Maps an engine failure onto a UI-facing one.
    ///
    /// The mapping is deliberately not a single catch-all. `Cancelled` in
    /// particular must stay distinguishable: it means the analyst asked for it
    /// and nothing is wrong, whereas every other variant means the run did not
    /// finish on its own. Collapsing them would make a cancelled analysis look
    /// like a failure the analyst needs to escalate.
    fn from(error: CoreError) -> Self {
        let kind = match &error {
            CoreError::Cancelled => ErrorKind::Cancelled,
            CoreError::CaseDirectoryNotInitialised(_) | CoreError::InvalidManifest { .. } => {
                ErrorKind::NotACase
            }
            CoreError::NotAFile { .. } | CoreError::SourceChangedDuringAcquisition { .. } => {
                ErrorKind::UnreadableSource
            }
            CoreError::Io { .. } => ErrorKind::UnreadableSource,
            CoreError::Serialise { .. } => ErrorKind::InvalidRequest,
            CoreError::Database { .. } => ErrorKind::Storage,
            CoreError::WorkerFailed { .. } => ErrorKind::AnalysisFailed,
        };
        Self::new(kind, error.to_string())
    }
}

impl From<tpt_app_media_forensics_report::ReportError> for ShellError {
    /// Maps a report-rendering failure.
    ///
    /// Reported as a storage problem rather than a generic one, because every
    /// variant here means the case directory could not be written to or the
    /// report was incomplete - and the analyst's remedy is the same in both
    /// cases: check the case directory's permissions and what was analysed.
    fn from(error: tpt_app_media_forensics_report::ReportError) -> Self {
        Self::new(ErrorKind::Storage, error.to_string())
    }
}

impl From<rusqlite::Error> for ShellError {
    /// Maps a database failure.
    ///
    /// Reached by read paths that call the store directly rather than through
    /// the pipeline, so a case whose database is unreadable reports as a storage
    /// problem rather than as a corrupt media file.
    fn from(error: rusqlite::Error) -> Self {
        Self::new(ErrorKind::Storage, format!("case database error: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancelled_run_is_not_reported_as_a_failure() {
        // The analyst stopping a run is not something that went wrong. If this
        // mapped to `AnalysisFailed` the UI would show an error banner over a
        // deliberate action, and an analyst would learn to ignore the banner.
        let error = ShellError::from(CoreError::Cancelled);
        assert_eq!(error.kind, ErrorKind::Cancelled);
    }

    #[test]
    fn an_uninitialised_directory_is_distinguished_from_a_corrupt_file() {
        // The analyst's remedy differs: open a real case, versus accept that the
        // file is damaged. The UI routes on the kind.
        let error = ShellError::from(CoreError::CaseDirectoryNotInitialised(
            std::path::PathBuf::from("C:/cases/nothing-here"),
        ));
        assert_eq!(error.kind, ErrorKind::NotACase);
        assert!(
            error.message.contains("nothing-here"),
            "the message must name the path the analyst tried: {error}"
        );
    }

    #[test]
    fn a_worker_panic_is_reported_rather_than_propagated() {
        // A dead worker is the one case where the process did not get to choose
        // to keep running, so it is its own kind.
        let error = ShellError::from(CoreError::WorkerFailed {
            path: "C:/cases/a/x.mp4".to_owned(),
            reason: "panicked: index 41 of 3".to_owned(),
            retryable: true,
        });
        assert_eq!(error.kind, ErrorKind::AnalysisFailed);
        assert!(error.message.contains("index 41 of 3"));
    }

    #[test]
    fn a_database_failure_is_not_reported_as_a_media_problem() {
        // A constraint violation is a storage fault. Labelling it an unreadable
        // source would send an investigator to the wrong file.
        let store = tpt_app_media_forensics_core::Store::open_in_memory().expect("opens");
        let error: ShellError = store
            .connection()
            .query_row("SELECT * FROM no_such_table", [], |r| r.get::<_, i64>(0))
            .unwrap_err()
            .into();
        assert_eq!(error.kind, ErrorKind::Storage);
    }

    #[test]
    fn every_error_kind_has_a_distinct_tag() {
        let kinds = [
            ErrorKind::NoCaseOpen,
            ErrorKind::NotACase,
            ErrorKind::Cancelled,
            ErrorKind::UnreadableSource,
            ErrorKind::NotFound,
            ErrorKind::InvalidRequest,
            ErrorKind::Storage,
            ErrorKind::AnalysisFailed,
        ];
        let mut tags: Vec<&str> = kinds.iter().map(|k| k.tag()).collect();
        tags.sort_unstable();
        let before = tags.len();
        tags.dedup();
        assert_eq!(tags.len(), before, "tags must be unique: {tags:?}");
    }

    #[test]
    fn an_error_round_trips_through_the_ipc_boundary() {
        // The frontend receives this as JSON, so a shape serde cannot encode is
        // a silently dropped failure rather than a shown one.
        let error = ShellError::from(CoreError::Cancelled);
        let json = serde_json::to_string(&error).expect("encodes");
        let decoded: ShellError = serde_json::from_str(&json).expect("decodes");
        assert_eq!(decoded, error);
    }
}
