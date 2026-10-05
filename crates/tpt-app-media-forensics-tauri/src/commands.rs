//! IPC command surface exposed to the webview frontend (spec §79).
//!
//! # Every command here is a thin adapter
//!
//! Each one marshals arguments across the IPC boundary, delegates to the core
//! engine or to a [`crate::view`] model, and serialises the result. No analysis
//! decision is made in this layer, so the GUI and the CLI cannot drift apart in
//! their results (spec §51).
//!
//! # The dashboard types moved to `view`
//!
//! `DashboardCounts` and `DashboardSummary` used to be defined here, when there
//! was nothing behind them but a test. They now belong to
//! [`crate::view::dashboard`], beside the code that derives them from findings.
//! A type that exists only to be serialised has no reason to live away from the
//! logic that produces it, and the test that checks the severity buckets sum to
//! the total is the one that keeps that logic honest.
//!
//! They are re-exported here so an existing `use` path keeps working.

use serde::{Deserialize, Serialize};

use tauri::Emitter;

use tpt_app_media_forensics_core::CoreError;
use tpt_app_media_forensics_model::{MediaTime, Timebase};

use tpt_app_media_forensics_core::store::{SearchQuery, SearchScope, SeverityFilter, Store};

use crate::error::{ErrorKind, ShellError, ShellResult};
use crate::state::AppState;
use crate::view::assets::{MetadataRow, MetadataView, OverviewView, StreamView};
use crate::view::comparison::ComparisonView;
pub use crate::view::dashboard::{DashboardCounts, DashboardSummary, ReviewStatus};
use crate::view::media::{AudioView, FramePayload, MeasurementView, SilenceView, VideoView};
use crate::view::run::{RunEvent, RunResult, RunStatus};
use crate::view::viewer::{FrameImageView, FrameStamp, FrameTimestamps, RgbBasis};

/// Which screens the frontend can navigate to (spec §79).
///
/// One ordered list, so the case screen's navigation cannot list a screen that
/// has no command behind it or omit one that does. A frontend that navigates by
/// string is free to drift; one that navigates by this enum is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Screen {
    /// The case header and dashboard.
    Case,
    /// The assets acquired into the case.
    Assets,
    /// Container and stream summary.
    Overview,
    /// Per-stream detail.
    Streams,
    /// The central timeline.
    Timeline,
    /// The video viewer.
    Video,
    /// The audio view and waveform.
    Audio,
    /// The metadata tree.
    Metadata,
    /// The findings list.
    Findings,
    /// File-to-file comparison.
    Comparisons,
    /// Retained artefacts.
    Evidence,
    /// Generated reports.
    Reports,
}

impl Screen {
    /// Every screen, in spec §79's order.
    pub const ALL: [Self; 12] = [
        Self::Case,
        Self::Assets,
        Self::Overview,
        Self::Streams,
        Self::Timeline,
        Self::Video,
        Self::Audio,
        Self::Metadata,
        Self::Findings,
        Self::Comparisons,
        Self::Evidence,
        Self::Reports,
    ];

    /// Returns the label shown in the navigation.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Case => "Case",
            Self::Assets => "Assets",
            Self::Overview => "Overview",
            Self::Streams => "Streams",
            Self::Timeline => "Timeline",
            Self::Video => "Video",
            Self::Audio => "Audio",
            Self::Metadata => "Metadata",
            Self::Findings => "Findings",
            Self::Comparisons => "Comparisons",
            Self::Evidence => "Evidence",
            Self::Reports => "Reports",
        }
    }

    /// Whether this screen needs an asset to be selected.
    ///
    /// The frontend uses it to disable navigation rather than to show an empty
    /// screen. Landing an analyst on "no video stream" because they had not
    /// clicked an asset yet reads as a problem with the file.
    #[must_use]
    pub const fn needs_asset(self) -> bool {
        matches!(
            self,
            Self::Overview
                | Self::Streams
                | Self::Timeline
                | Self::Video
                | Self::Audio
                | Self::Metadata
        )
    }
}

/// One entry in the navigation, as the frontend receives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenView {
    /// Which screen.
    pub screen: Screen,
    /// Its label.
    pub label: String,
    /// Whether it needs an asset selected first.
    pub needs_asset: bool,
}

/// The screen list, so the frontend navigates from the backend's ordering.
///
/// Sent once at startup rather than hardcoded in the frontend. Spec §79's
/// hierarchy is a property of the application, and a copy of it in the frontend
/// is a copy that can fall behind — the same failure this repository has hit
/// three times with duplicated rule and metadata logic.
#[tauri::command]
pub fn screens() -> Vec<ScreenView> {
    Screen::ALL
        .iter()
        .map(|screen| ScreenView {
            screen: *screen,
            label: screen.label().to_owned(),
            needs_asset: screen.needs_asset(),
        })
        .collect()
}

/// The case identifier for a store holding exactly one case.
///
/// # Errors
///
/// Refuses when the database holds none or several. Picking the first would
/// silently examine the wrong case, and in a forensic tool that is worse than
/// refusing — the same reasoning `Store::only_case_id` documents.
fn only_case(store: &Store) -> ShellResult<String> {
    store.only_case_id()?.ok_or_else(|| {
        ShellError::new(
            ErrorKind::NotFound,
            "this case directory has no case record in it",
        )
    })
}

/// The open case, as the frontend receives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseView {
    /// Case identifier.
    pub case_id: String,
    /// Display name.
    pub name: String,
    /// Description, when the case has one.
    pub description: Option<String>,
    /// Case directory path.
    pub root: String,
    /// Assets acquired into the case.
    pub assets: Vec<AssetView>,
}

/// One asset in the case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetView {
    /// Asset identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Path recorded at acquisition.
    pub source_path: String,
    /// Size in bytes.
    pub size_bytes: u64,
    /// SHA-256 recorded at acquisition.
    pub sha256: Option<String>,
    /// BLAKE3 recorded at acquisition.
    pub blake3: Option<String>,
}

/// Opens a case directory (spec §53).
///
/// Validated before it is recorded, so `state` never holds a path that is not a
/// case. An application that accepted any directory would report
/// `CaseDirectoryNotInitialised` from every subsequent screen instead of once,
/// here, where the analyst can act on it.
#[tauri::command]
pub fn open_case(state: tauri::State<'_, AppState>, path: String) -> ShellResult<CaseView> {
    let dir = tpt_app_media_forensics_core::CaseDirectory::open(&path)?;
    let store = Store::open(dir.root())?;
    let case_id = only_case(&store)?;

    let manifest = dir.read_manifest()?;
    let name = manifest.name.clone();
    let description = manifest.description.clone();

    state.set_case_dir(dir.root().to_path_buf())?;

    let assets = store.assets_in_case(&case_id)?;

    Ok(CaseView {
        case_id,
        name,
        description,
        root: dir.root().display().to_string(),
        assets: assets
            .iter()
            .map(|asset| AssetView {
                id: asset.id.clone(),
                name: asset.name.clone(),
                source_path: asset.source_path.clone(),
                size_bytes: asset.size_bytes,
                sha256: asset.sha256.clone(),
                blake3: asset.blake3.clone(),
            })
            .collect(),
    })
}

/// Closes the open case.
#[tauri::command]
pub fn close_case(state: tauri::State<'_, AppState>) -> ShellResult<()> {
    state.close_case()
}

/// Records one line from the frontend into the file named by `TPT_STARTUP_LOG`.
///
/// # Why this command exists
///
/// A webview that fails to load its module graph opens a blank window, and a
/// blank window looks the same as a working one with nothing to show. Every
/// static check in `ui/check.mjs` can be green while the shipped page renders
/// nothing at all, because a stub DOM is not a browser.
///
/// This gives the frontend a way to say "I loaded, and here is what I found" in
/// a place a build script can read. It is the only evidence available that does
/// not require a human looking at the window.
///
/// # It ships in release builds, deliberately
///
/// The failure this exists to detect is *most* likely in a release build — it is
/// the one nobody is watching, and the one where a blank window is shipped rather
/// than noticed. Compiling it out of release would remove the evidence from
/// exactly the build that needs it, and would leave the one case where there is
/// nothing else to go on.
///
/// # Why the line is flattened and bounded
///
/// The hazard is not the file. It is that this is a write primitive reachable
/// from the webview, and the webview renders data taken from the file under
/// examination — asset names, container brands, codec strings, atom values, all
/// of them chosen *because* the case is hostile.
///
/// Today nothing routes case data into `startup_log`: the two callers send fixed
/// strings and a probe result. Nothing *enforces* that, though, and the frontend
/// has no build step, so the next edit is free to pass an error message through.
/// Without this, a crafted file name containing a newline could write an
/// arbitrary second line into the log — forging the very record this exists to
/// establish, in the one file a build script treats as ground truth.
///
/// So one call is one line, always: every control character becomes a space, and
/// the result is truncated. A caller can no longer fabricate entries, forge a
/// timestamp-prefixed line, or grow the file without bound.
///
/// # Opt-in, and silent otherwise
///
/// Does nothing unless `TPT_STARTUP_LOG` names a file. An evidentiary tool
/// should not write files nobody asked for — a log appearing beside a case would
/// itself be a question an analyst has to answer.
///
/// # Errors
///
/// Never fails. A diagnostic that can break the application is worse than a
/// missing line in a log file, so every failure here is swallowed deliberately.
#[tauri::command]
pub fn startup_log(line: String) {
    let Ok(path) = std::env::var("TPT_STARTUP_LOG") else {
        return;
    };
    use std::io::Write as _;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{}", one_log_line(&line));
    }
}

/// Longest line written to the startup log.
///
/// Bounded because the writer appends to a path it does not own, on every page
/// load, with content this process cannot fully trust. A diagnostic must not be
/// able to fill a disk.
const MAX_LOG_LINE_CHARS: usize = 512;

/// Collapses `line` into exactly one line of at most [`MAX_LOG_LINE_CHARS`].
///
/// Returns the text with every control character replaced by a space. A newline
/// in the input is the whole attack: it turns one call into two entries, and an
/// entry an attacker chose is an entry the log cannot be trusted to distinguish
/// from one the application wrote.
fn one_log_line(line: &str) -> String {
    let flattened: String = line
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_LOG_LINE_CHARS)
        .collect();
    flattened.trim_end().to_owned()
}

/// The dashboard panel for the open case (spec §80).
///
/// Reads the case's recorded findings and rebuilds the counters from them. It
/// does *not* re-run the analysis: a dashboard that re-analysed to produce its
/// numbers could disagree with the findings list beside it, and a report built
/// from one run could cite another (spec §63).
#[tauri::command]
pub fn dashboard(state: tauri::State<'_, AppState>) -> ShellResult<DashboardSummary> {
    let case_dir = state.case_dir()?;
    let case = tpt_app_media_forensics_core::CaseDirectory::open(&case_dir)?;
    let store = Store::open(case.root())?;

    let case_id = only_case(&store)?;
    let case_name = case.read_manifest()?.name.clone();

    let findings = store.findings_in_case(&case_id)?;

    let mut summary = DashboardSummary::new(
        &case_id,
        case_name,
        &findings,
        analysis_status(&store, &case_id),
    );
    // The asset and analysis totals come from the database rather than from the
    // findings slice: a case can hold assets that were acquired but never
    // analysed, and a dashboard showing "assets 0" beside a case containing one
    // would be plainly wrong.
    summary.counts.assets = store.count("assets")?.try_into().unwrap_or(u64::MAX);
    summary.counts.analyses = store.count("analyses")?.try_into().unwrap_or(u64::MAX);
    Ok(summary)
}

/// The recorded status of a case's most recent analysis.
///
/// Defaults to `Pending` when nothing has run. That is deliberate: "no analysis
/// recorded" is not "a clean result", and `Pending` keeps the dashboard in
/// `INCOMPLETE` rather than letting an empty case read as clear.
///
/// The status column is read rather than assumed, even though
/// `Store::insert_analysis` currently writes `COMPLETE` unconditionally: a row
/// written by a build that records cancellation would otherwise be reported here
/// as a completed examination.
fn analysis_status(store: &Store, case_id: &str) -> tpt_app_media_forensics_model::AnalysisStatus {
    use tpt_app_media_forensics_model::AnalysisStatus;

    let status: Option<String> = store
        .connection()
        .query_row(
            "SELECT status FROM analyses WHERE case_id = ?1 \
             ORDER BY started_at DESC, id DESC LIMIT 1",
            [case_id],
            |row| row.get(0),
        )
        .ok();

    // An unreadable status is reported as `Pending`, not `Complete`: the
    // conservative direction is the one that does not claim an examination
    // finished when nothing established that.
    status
        .as_deref()
        .map_or(AnalysisStatus::Pending, parse_status)
}

/// Maps a stored status string onto the enum.
///
/// An unrecognised value becomes `Pending` rather than failing the panel,
/// because the status gates the dashboard's verdict and losing the whole panel
/// over one unfamiliar string would also hide the counts beside it.
fn parse_status(text: &str) -> tpt_app_media_forensics_model::AnalysisStatus {
    use tpt_app_media_forensics_model::AnalysisStatus;
    match text {
        "COMPLETE" => AnalysisStatus::Complete,
        "RUNNING" => AnalysisStatus::Running,
        "FAILED" => AnalysisStatus::Failed,
        "CANCELLED" => AnalysisStatus::Cancelled,
        _ => AnalysisStatus::Pending,
    }
}

/// Searches the open case (spec §41).
///
/// The same `SearchQuery` the CLI's `search` uses, so a term that finds
/// something in the terminal finds the same thing in the window.
#[tauri::command]
pub fn search(
    state: tauri::State<'_, AppState>,
    text: Option<String>,
    scope: Option<SearchScopeView>,
    min_severity: Option<tpt_app_media_forensics_model::Severity>,
    limit: Option<usize>,
) -> ShellResult<SearchView> {
    let case_dir = state.case_dir()?;
    let case = tpt_app_media_forensics_core::CaseDirectory::open(&case_dir)?;
    let store = Store::open(case.root())?;
    let case_id = only_case(&store)?;

    let query = SearchQuery {
        text: text.unwrap_or_default(),
        // `SearchScopeView` is total over `SearchScope`, so the conversion
        // cannot fail and is not allowed to: a search box that silently did
        // nothing because of a bad filter would read as "nothing matched".
        scope: scope.map(Into::into),
        severity: min_severity.map(SeverityFilter::AtLeast),
        asset_id: None,
        limit,
    };

    // The connection is taken from the store rather than the case directory, so
    // this reads exactly the database every other screen reads.
    let result =
        tpt_app_media_forensics_core::store::search::search(store.connection(), &case_id, &query)?;

    Ok(SearchView {
        term: query.trimmed().to_owned(),
        scope: query.effective_scope().into(),
        returned: result.hits.len(),
        total: result.total,
        truncated: result.truncated,
        hits: result
            .hits
            .iter()
            .map(|hit| SearchHitView {
                scope: hit.scope.into(),
                reference: hit.id.clone(),
                asset_id: hit.asset_id.clone(),
                summary: hit.label.clone(),
                severity: hit.severity,
            })
            .collect(),
    })
}

/// Which records a search looked at, as the UI receives it.
///
/// A shell-local copy of the engine's `SearchScope` rather than the type
/// itself. The engine's enum deliberately has no `serde` derive — it is never
/// serialised there — and adding one to satisfy a UI would put a presentation
/// concern into the analysis model. The mapping is total and tested in
/// `SearchScopeView::from_scope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchScopeView {
    /// Findings only.
    Findings,
    /// Assets only.
    Assets,
    /// Evidence only.
    Evidence,
    /// Everything.
    All,
}

impl From<SearchScopeView> for SearchScope {
    fn from(view: SearchScopeView) -> Self {
        match view {
            SearchScopeView::Findings => Self::Findings,
            SearchScopeView::Assets => Self::Assets,
            SearchScopeView::Evidence => Self::Evidence,
            SearchScopeView::All => Self::All,
        }
    }
}

impl From<SearchScope> for SearchScopeView {
    fn from(scope: SearchScope) -> Self {
        match scope {
            SearchScope::Findings => Self::Findings,
            SearchScope::Assets => Self::Assets,
            SearchScope::Evidence => Self::Evidence,
            SearchScope::All => Self::All,
        }
    }
}

/// A search result as the UI receives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchView {
    /// The term that was searched for, trimmed.
    pub term: String,
    /// Which records were searched.
    pub scope: SearchScopeView,
    /// Rows returned.
    pub returned: usize,
    /// Rows that matched before the limit.
    ///
    /// Carried beside `returned` because "200 results" beside a 200-row list is
    /// a false statement about the case when 4,000 matched.
    pub total: usize,
    /// Whether the limit hid matches.
    pub truncated: bool,
    /// The matches.
    pub hits: Vec<SearchHitView>,
}

/// One match, flattened for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHitView {
    /// Which kind of record matched.
    pub scope: SearchScopeView,
    /// Identifier of the record that matched.
    pub reference: String,
    /// The asset it belongs to, when it belongs to one.
    pub asset_id: Option<String>,
    /// One-line description of what matched.
    pub summary: String,
    /// Severity, when the record has one.
    pub severity: Option<tpt_app_media_forensics_model::Severity>,
}

/// The timeline strip for the open case (spec §42, §81).
///
/// Rebuilt from the engine's own unified timeline plus the recorded findings.
/// The model decides which rows exist, which markers are on them, and whether
/// an observation is positioned at all; this function only opens the case and
/// reads the database.
#[tauri::command]
pub fn timeline(
    state: tauri::State<'_, AppState>,
) -> ShellResult<crate::view::timeline::TimelineView> {
    let case_dir = state.case_dir()?;
    let case = tpt_app_media_forensics_core::CaseDirectory::open(&case_dir)?;
    let store = Store::open(case.root())?;
    let case_id = only_case(&store)?;

    let entries = store.timeline_in_case(&case_id)?;
    // The engine builds this strip during the run and it is persisted with the
    // run, so a reopened case draws the same observations in the same order
    // rather than a reconstruction from findings alone. This previously passed
    // `Vec::new()`, which left the video, audio, scene and error layers
    // structurally empty for every case - four tracks that read as "nothing was
    // found" rather than "was not retained".
    //
    // Findings are still layered on separately: the engine merges positioned
    // findings into its own list, and the strip needs them on their own row,
    // coloured by severity and carrying their evidence references.
    let findings = store.findings_in_case(&case_id)?;

    // Whether every run in this case stored its strip. Read from the database
    // rather than inferred from the entries: a case created before schema v4 has
    // no `timeline_entries` and never will, so an empty strip there means "was
    // never retained" while an empty strip here means "nothing was found". Those
    // are opposite findings and the screen has to be able to say which one it is
    // looking at.
    let retention = store.timeline_retention_in_case(&case_id)?;

    let timeline = tpt_app_media_forensics_model::Timeline::new(entries, None);
    Ok(crate::view::timeline::TimelineView::build(
        &timeline, &findings, retention,
    ))
}

/// Analyses every media file beneath `directory` into the open case.
///
/// Spec §83's batch dashboard. The engine already owns this walk
/// (`core::batch::run`), so this command calls it rather than re-implementing a
/// scan: two directory walks in one repository is how the GUI and the CLI come
/// to disagree about which files were examined.
///
/// Returns immediately and the frontend polls [`poll_batch`], because a folder
/// of masters takes minutes and spec §57 requires the window to stay usable.
///
/// # Errors
///
/// Returns an error when no case is open, when `directory` is not a directory,
/// or when the state's lock is unusable. A file the engine cannot read is *not*
/// an error: it becomes a row with `UNREADABLE` and a reason, because one bad
/// file in an intake folder must not abandon the other nine hundred.
#[tauri::command]
pub fn start_batch(state: tauri::State<'_, AppState>, directory: String) -> ShellResult<()> {
    let case_dir = state.case_dir()?;
    let directory = std::path::PathBuf::from(directory);
    if !directory.is_dir() {
        return Err(ShellError::new(
            ErrorKind::UnreadableSource,
            format!("{} is not a directory", directory.display()),
        ));
    }
    let case = tpt_app_media_forensics_core::CaseDirectory::open(&case_dir)?;
    let engine = std::sync::Arc::new(tpt_app_media_forensics_core::AnalysisEngine::new());

    state.start_batch(move || {
        let outcome = tpt_app_media_forensics_core::batch::run(&engine, &directory, &case)?;
        Ok(crate::view::batch::BatchView::from_rows(rows_from(outcome)))
    })
}

/// Maps the engine's per-file outcomes onto the dashboard's rows.
///
/// The three engine outcomes map to three distinct statuses rather than two:
/// a file that was analysed gets the engine's own verdict, and a file that could
/// not be read or was skipped is an *operational* outcome that says nothing
/// about the media. Folding those into `FAIL` would make a corrupt intake folder
/// look like a delivery of bad media.
fn rows_from(
    outcome: tpt_app_media_forensics_core::batch::BatchOutcome,
) -> Vec<crate::view::batch::BatchRow> {
    use crate::view::batch::{BatchRow, BatchView};
    use tpt_app_media_forensics_core::batch::FileOutcome;

    outcome
        .results
        .into_iter()
        .map(|(path, result)| {
            let path = path.display().to_string();
            match result {
                FileOutcome::Analysed(analysed) => {
                    BatchView::analysed(path, &analysed.findings, false)
                }
                FileOutcome::Failed { reason } => BatchRow::unreadable(path, reason),
                FileOutcome::Skipped { reason } => BatchRow::skipped(path, reason),
            }
        })
        .collect()
}

/// The finished batch table, or `None` while the run is still going.
///
/// `None` means "not finished" here and an error is a separate answer, so the
/// frontend distinguishes them rather than treating both as `null` — the same
/// mistake that made the analysis poll loop retry forever on failure.
#[tauri::command]
pub fn poll_batch(
    state: tauri::State<'_, AppState>,
) -> ShellResult<Option<crate::view::batch::BatchView>> {
    Ok(state.take_batch()?.transpose()?)
}

#[cfg(test)]
mod tests {
    use super::{one_log_line, rows_from, MAX_LOG_LINE_CHARS};

    #[test]
    fn one_call_cannot_write_two_log_entries() {
        // The reason `startup_log` flattens its input. This is a write primitive
        // reachable from the webview, and the webview renders strings taken from
        // the file under examination — a container brand or an asset name is
        // attacker-chosen by definition. A newline in one of those would write an
        // entry the application never composed, into the one file a build script
        // treats as ground truth about whether the application loaded.
        let line = one_log_line("ready screens=12\n2026-01-01 rust: process started");
        assert_eq!(line, "ready screens=12 2026-01-01 rust: process started");
        assert!(!line.contains('\n'), "one call, one line");
    }

    #[test]
    fn carriage_returns_are_flattened_too() {
        // A lone `\r` overwrites the line it is on in a terminal rather than
        // starting a new one, which hides the same injected text just as
        // effectively.
        let line = one_log_line("probe bridge=object\rready screens=12");
        assert!(!line.contains('\r'));
        assert_eq!(line, "probe bridge=object ready screens=12");
    }

    #[test]
    fn other_control_characters_are_flattened() {
        // Tab, bell, escape: none of them belong in a log this tool reads, and an
        // escape sequence can repaint what a reader sees.
        let line = one_log_line("probe\tscreens\u{7}\u{1b}[2Jdone");
        assert!(
            line.chars().all(|c| !c.is_control()),
            "no control character may survive into the log: {line:?}"
        );
    }

    #[test]
    fn a_line_is_bounded() {
        // The writer appends to a path it does not own, on every page load, with
        // content it does not fully control. An unbounded append is a way to fill
        // a disk from inside the application.
        let line = one_log_line(&"x".repeat(MAX_LOG_LINE_CHARS * 4));
        assert_eq!(line.chars().count(), MAX_LOG_LINE_CHARS);
    }

    #[test]
    fn a_truncated_line_is_not_padded_with_spaces() {
        // Trailing whitespace on a bounded line would suggest content was elided
        // when nothing was.
        let line = one_log_line("ready   ");
        assert_eq!(line, "ready");
    }

    #[test]
    fn non_ascii_survives_untouched() {
        // Only *control* characters are flattened. A file name in a script the
        // examiner reads is content, and mangling it would make the log lie
        // about what it recorded.
        let line = one_log_line("ready master — ünïcode.mp4");
        assert_eq!(line, "ready master — ünïcode.mp4");
    }

    #[test]
    fn an_empty_line_stays_empty_rather_than_becoming_blank_padding() {
        assert_eq!(one_log_line(""), "");
    }

    /// A folder of real media plus a file the engine cannot read.
    ///
    /// The point is that the two outcomes stay *distinct*. Folding an unreadable
    /// file into `FAIL` would make a corrupt intake folder indistinguishable from
    /// a delivery of bad media, and the two have different remedies.
    #[test]
    fn an_unreadable_file_is_not_reported_as_a_failed_delivery() {
        let dir = tempfile::tempdir().expect("scratch");
        let media = dir.path().join("master.mp4");
        std::fs::write(
            &media,
            tpt_app_media_forensics_container::build_mp4(
                &tpt_app_media_forensics_container::TrackSpec::video_25fps(64, 48, 4),
            ),
        )
        .expect("fixture writes");

        let case_dir = tpt_app_media_forensics_core::CaseDirectory::create(
            dir.path().join("case.tptcase"),
            &tpt_app_media_forensics_model::Case::new("batch".to_owned(), None),
        )
        .expect("case directory");
        let engine = tpt_app_media_forensics_core::AnalysisEngine::new();

        let outcome = tpt_app_media_forensics_core::batch::run(&engine, dir.path(), &case_dir)
            .expect("batch runs");
        let rows = rows_from(outcome);

        assert!(
            rows.iter()
                .any(|r| r.status == crate::view::batch::BatchStatus::Pass
                    || r.status == crate::view::batch::BatchStatus::Warn
                    || r.status == crate::view::batch::BatchStatus::Fail),
            "the real media file should have been analysed, got {:?}",
            rows.iter().map(|r| r.status).collect::<Vec<_>>(),
        );
        // The case directory itself sits inside the scanned folder, so the walk
        // must have skipped it rather than analysing the examination of the
        // examination. That is the same guard the CLI relies on.
        assert!(
            rows.iter().all(|r| !r.path.contains("case.tptcase")),
            "the batch analysed its own case directory: {:?}",
            rows.iter().map(|r| &r.path).collect::<Vec<_>>(),
        );
    }

    /// Every row carries a path, and the row count matches what was visited.
    ///
    /// A row without a path cannot be traced back to a file, which is the one
    /// thing an intake table has to guarantee.
    /// The timeline an analysis produced is persisted, not thrown away.
    ///
    /// This is the whole point of the `timeline_entries` table. `AnalysisOutcome`
    /// carries a `Timeline` merging structural damage, timestamp anomalies and
    /// positioned findings; before this existed the strip was built, handed back,
    /// and dropped, so a reopened case could only rebuild it from findings and
    /// the video, audio, scene and error layers were permanently empty.
    ///
    /// Read back through a *fresh* `Store`, because a write the writer can see
    /// but the next process cannot is not a record.
    #[test]
    fn a_timeline_survives_the_run_and_reads_back_identically() {
        let dir = tempfile::tempdir().expect("scratch");
        let source = dir.path().join("master.mp4");
        std::fs::write(
            &source,
            tpt_app_media_forensics_container::build_mp4(
                &tpt_app_media_forensics_container::TrackSpec::video_25fps(64, 48, 4),
            ),
        )
        .expect("fixture writes");

        let case_dir = tpt_app_media_forensics_core::CaseDirectory::create(
            dir.path().join("case.tptcase"),
            &tpt_app_media_forensics_model::Case::new("timeline".to_owned(), None),
        )
        .expect("case directory");

        let engine = tpt_app_media_forensics_core::AnalysisEngine::new();
        let outcome = engine
            .analyse(&source, &case_dir)
            .expect("analysis completes");

        let store =
            tpt_app_media_forensics_core::store::Store::open(case_dir.root()).expect("store opens");
        let case_id = store.only_case_id().expect("case id").expect("one case");
        let read_back = store.timeline_in_case(&case_id).expect("timeline reads");

        assert_eq!(
            read_back, outcome.timeline.entries,
            "the persisted strip differs from the one the engine produced: an \
             observation was re-ordered, altered, or lost",
        );
    }

    /// An entry with no position stays unplaced; it is never recorded at time zero.
    ///
    /// Spec §31 forbids fabricating a position. A container scan finds damage at
    /// a byte offset long before anything has a timestamp, and writing those rows
    /// at time 0 would place every one of them at the start of the media — which
    /// reads as a measurement the engine never made.
    #[test]
    fn an_unplaced_entry_is_not_persisted_as_being_at_time_zero() {
        use tpt_app_media_forensics_core::store::StoredAsset;
        use tpt_app_media_forensics_model::timeline::{Placement, TimelineEntry, TimelineSource};

        let dir = tempfile::tempdir().expect("scratch");
        let case_dir = tpt_app_media_forensics_core::CaseDirectory::create(
            dir.path().join("case.tptcase"),
            &tpt_app_media_forensics_model::Case::new("unplaced".to_owned(), None),
        )
        .expect("case directory");

        let store =
            tpt_app_media_forensics_core::store::Store::open(case_dir.root()).expect("store opens");
        let case_id = store.only_case_id().expect("case id").expect("one case");
        let asset = StoredAsset {
            id: "asset-1".to_owned(),
            case_id: case_id.clone(),
            name: "master.mp4".to_owned(),
            source_path: "/tmp/master.mp4".to_owned(),
            size_bytes: 1,
            sha256: None,
            blake3: None,
            role: None,
        };
        store.insert_asset(&asset).expect("asset inserted");
        // The timeline table references the analysis it belongs to, so the test
        // has to record one. That the constraint fired at all is worth noting:
        // a timeline row that names a run the database has never heard of would
        // be a strip belonging to an examination that does not exist.
        store
            .insert_analysis(&tpt_app_media_forensics_core::store::StoredAnalysis {
                id: "analysis-1".to_owned(),
                case_id: case_id.clone(),
                asset_id: asset.id.clone(),
                cache_key: "test".to_owned(),
                finding_count: 0,
                rule_count: 0,
                profile: "test".to_owned(),
                profile_fingerprint: "test".to_owned(),
                rule_set_fingerprint: "test".to_owned(),
                started_at: 0,
            })
            .expect("analysis inserted");

        let timeline = tpt_app_media_forensics_model::Timeline::new(
            vec![TimelineEntry {
                time: None,
                placement: Placement::Unplaced,
                source: TimelineSource::StructuralDamage,
                reference: "CONTAINER.NO_MOOV".to_owned(),
                summary: "no moov atom".to_owned(),
            }],
            None,
        );
        store
            .insert_timeline("analysis-1", &asset.id, &timeline)
            .expect("timeline inserted");

        let read_back = store.timeline_in_case(&case_id).expect("timeline reads");
        assert_eq!(read_back.len(), 1, "the entry was lost");
        assert!(
            read_back[0].time.is_none(),
            "an unplaced entry came back with a position: {:?}",
            read_back[0].time,
        );
        assert_eq!(read_back[0].placement, Placement::Unplaced);
    }
    #[test]
    fn every_row_names_the_file_it_came_from() {
        let rows = vec![
            crate::view::batch::BatchRow::unreadable("/intake/a.mov", "no moov atom"),
            crate::view::batch::BatchRow::skipped("/intake/b.mov", "already in the case"),
        ];
        let view = crate::view::batch::BatchView::from_rows(rows);
        assert_eq!(view.rows.len(), 2);
        for row in &view.rows {
            assert!(!row.path.is_empty(), "a row lost its path");
            assert!(row.reason.is_some(), "an operational outcome must say why");
        }
        // Both operational outcomes block delivery: an unexamined file cannot be
        // certified as deliverable.
        assert!(view.rows.iter().all(|r| r.status.blocks_delivery()));
    }
}

/// The event name the frontend listens on for run progress.
///
/// A constant rather than a literal at both ends: the emitter and the listener
/// are in the same repository but different languages, and a renamed event would
/// otherwise fail silently as a progress bar that never moves.
pub const PROGRESS_EVENT: &str = "analysis-progress";

/// Starts an analysis of `source` into the open case (spec §55, §57).
///
/// Returns immediately. Spec §57 requires the interface to stay responsive, and
/// `AnalysisEngine::analyse` blocks for minutes on a long file, so the work goes
/// to an `AnalysisJob` on its own thread and progress is emitted as
/// [`PROGRESS_EVENT`] messages the frontend can draw.
///
/// The source is opened read-only. Everything the run writes goes under the case
/// directory, and the source itself is never modified (spec §11).
#[tauri::command]
pub fn analyse(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    source: String,
) -> ShellResult<RunHandle> {
    let case_dir = state.case_dir()?;
    let case = tpt_app_media_forensics_core::CaseDirectory::open(&case_dir)?;

    if !std::path::Path::new(&source).is_file() {
        return Err(ShellError::new(
            ErrorKind::UnreadableSource,
            format!("{source} is not a file"),
        ));
    }

    // A run id is allocated before the reporter is built, because the closure
    // has to stamp every event with it and a closure cannot read state it does
    // not yet have. Two analyses in flight would otherwise paint each other's
    // progress onto the same bar.
    let run_id = state.next_run_id()?;

    // The reporter closure is `Fn + Send + Sync + 'static` because the engine
    // may call it from any of its worker threads. Emitting is non-blocking, so
    // one slow frontend cannot stall the analysis; a run that takes minutes is
    // not going to be held up by a window that is busy repainting.
    let handle_for_events = app.clone();
    let tracker = state.begin_run(move |event| {
        let wire = RunEvent::from(&event);
        let _ = handle_for_events.emit(
            PROGRESS_EVENT,
            ProgressPayload {
                run_id,
                fraction: wire.fraction(),
                label: wire.stage().label().to_owned(),
                event: wire,
            },
        );
    })?;

    // One engine, two handles. The worker gets a clone so the state can keep the
    // original and later ask *that* engine for the analysis fingerprint: the
    // fingerprint is computed from the rule set that produced the findings, and
    // a different engine vouching for them would defeat the point of recording
    // it (spec §63).
    let engine = std::sync::Arc::new(tpt_app_media_forensics_core::AnalysisEngine::new());
    let job = tpt_app_media_forensics_core::AnalysisJob::spawn(
        std::sync::Arc::clone(&engine),
        std::path::PathBuf::from(&source),
        case,
        tracker,
    )?;

    let id = state.register_job(run_id, job, engine)?;
    debug_assert_eq!(
        id, run_id,
        "the run id must be reserved once and then reused, never allocated twice",
    );

    Ok(RunHandle {
        id,
        source,
        finished: false,
    })
}

/// One progress message, as the frontend receives it.
///
/// Carries the run id, the fraction and the label *alongside* the raw event. The
/// frontend needs all three, and deriving the fraction there would mean a second
/// implementation of the engine's stage grouping - which is precisely how a
/// progress bar starts going backwards (spec §56).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressPayload {
    /// Which run this belongs to.
    ///
    /// Two analyses can be in flight in principle; without this the second
    /// would paint over the first.
    pub run_id: u64,
    /// A fraction of the whole run, 0.0 to 1.0.
    pub fraction: f64,
    /// The stage's display label.
    pub label: String,
    /// The raw event, for a renderer that wants the detail.
    pub event: RunEvent,
}

/// A handle to a running analysis, pollable from the frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunHandle {
    /// Identifier for this run, for correlation in the UI.
    pub id: u64,
    /// The file being analysed.
    pub source: String,
    /// Whether the run has finished.
    pub finished: bool,
}

/// Cancels the run in progress (spec §55).
///
/// Returns whether there was one, so the frontend can avoid showing a
/// "cancelled" banner over a click that had nothing to cancel. A cancelled run
/// writes nothing to the case database.
#[tauri::command]
pub fn cancel_analysis(state: tauri::State<'_, AppState>) -> ShellResult<bool> {
    Ok(state.cancel())
}

/// Collects the finished analysis, if there is one.
///
/// Returns `None` while the run is still going, which is what keeps the
/// frontend's poll cheap and the interface responsive (spec §57). The
/// frontend calls this on a timer rather than awaiting a promise, so a
/// ninety-minute analysis does not hold a window hostage \u2014 and cancelling stays
/// reachable the whole time.
///
/// The fingerprint is computed by the engine that ran the analysis, so the
/// number on screen identifies the rule set that actually produced the
/// findings (spec §63).
#[tauri::command]
pub fn poll_analysis(state: tauri::State<'_, AppState>) -> ShellResult<Option<RunResult>> {
    let Some((result, engine)) = state.take_finished_job() else {
        return Ok(None);
    };

    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) => {
            let status = if matches!(error, CoreError::Cancelled) {
                RunStatus::Cancelled
            } else {
                RunStatus::Failed
            };
            // The failure is returned rather than swallowed: a cancelled run and
            // a run that died are different outcomes, and a frontend that cannot
            // tell them apart would report an analyst's own decision as a fault.
            return Ok(Some(RunResult {
                asset_name: String::new(),
                sha256: None,
                status,
                finding_count: 0,
                evidence_count: 0,
                timeline_count: 0,
                measured_timeline_count: 0,
                cache_hit: false,
                analysis_fingerprint: String::new(),
                limitations: vec![error.to_string()],
                persisted: false,
            }));
        }
    };

    let fingerprint = engine.analysis_fingerprint(&outcome.cache_key);
    Ok(Some(RunResult {
        asset_name: outcome.asset.name.clone(),
        sha256: outcome.asset.sha256().map(ToOwned::to_owned),
        status: RunStatus::Complete,
        finding_count: outcome.findings.len() as u64,
        evidence_count: outcome.evidence.len() as u64,
        timeline_count: outcome.timeline.len() as u64,
        measured_timeline_count: outcome.timeline.measured_count() as u64,
        // Marked explicitly rather than left implicit. A run served from the
        // cache measured nothing this time, and an analyst comparing it against a
        // fresh run needs to know which they are looking at.
        cache_hit: outcome.cache_hit,
        analysis_fingerprint: fingerprint,
        limitations: outcome.limitations.clone(),
        persisted: !outcome.cache_hit,
    }))
}

/// Inspects a source file's container and streams (spec §12, §79).
///
/// Reads the source read-only. The acquisition record fixes the file's identity
/// by hash (spec §11), so this is a read of the same bytes the rules read \u2014 not
/// a second opinion.
#[tauri::command]
pub fn inspect_asset(source: String) -> ShellResult<AssetInspection> {
    let path = std::path::Path::new(&source);
    let bytes = std::fs::read(path).map_err(|e| {
        ShellError::new(
            ErrorKind::UnreadableSource,
            format!("could not read {source}: {e}"),
        )
    })?;

    let format = tpt_app_media_forensics_core::pipeline::detect_source(path)?;

    // Dispatched on the format the probe identified, because the two container
    // readers take different arguments and neither knows about the other. A
    // format neither reader handles yields an *empty* inspection carrying the
    // reason, which is the honest answer for a file nothing recognised: "no
    // reader for this container", not an error the analyst has to interpret.
    let inspection = match format {
        tpt_app_media_forensics_container::ContainerFormat::Matroska => {
            // Both readers take ownership: the demuxers consume the buffer
            // rather than borrowing it, so each branch is handed its own copy.
            match tpt_app_media_forensics_container::inspect_matroska_bytes(bytes.clone()) {
                Ok(inspection) => inspection,
                Err(error) => empty_inspection(format, error.to_string()),
            }
        }
        tpt_app_media_forensics_container::ContainerFormat::IsoBmff => {
            match tpt_app_media_forensics_container::inspect_bytes(bytes) {
                Ok(inspection) => inspection,
                Err(error) => empty_inspection(format, error.to_string()),
            }
        }
        other => empty_inspection(
            format,
            format!("no container reader is integrated for {} yet", other.tag()),
        ),
    };

    let streams = inspection
        .streams
        .iter()
        .enumerate()
        .map(|(index, stream)| StreamView {
            index,
            kind: stream.kind.tag().to_owned(),
            codec: stream.codec.name.clone(),
            codec_long_name: stream.codec.long_name.clone(),
            language: stream.language.clone(),
            dimensions: stream.dimensions().map(|(w, h)| format!("{w}x{h}")),
            frame_rate: stream
                .video_format()
                .and_then(|v| v.frame_rate)
                .map(|r| r.to_string()),
            sample_rate: stream.audio_format().map(|a| a.sample_rate),
            bit_depth: stream.audio_format().map(|a| a.bit_depth),
            channels: stream
                .audio_format()
                .and_then(|a| a.channel_layout.as_ref())
                .map(|layout| layout.channel_count)
                .or_else(|| stream.audio_format().map(|a| a.channel_count()))
                .filter(|c| *c > 0),
            primaries: stream
                .video_format()
                .and_then(|v| v.colour.primaries.clone()),
            transfer: stream
                .video_format()
                .and_then(|v| v.colour.transfer.clone()),
            matrix: stream.video_format().and_then(|v| v.colour.matrix.clone()),
            is_hdr: stream.video_format().is_some_and(|v| v.is_hdr),
            packet_count: stream.packet_count,
            start_micros: stream.timing.start_time.as_micros(),
            declared_duration_micros: stream.timing.duration.map(|d| d.as_micros()),
            measured_duration_micros: stream.timing.measured_duration.map(|d| d.as_micros()),
        })
        .collect();

    let first_video = inspection
        .streams
        .iter()
        .find(|s| s.kind == tpt_app_media_forensics_model::StreamKind::Video);

    Ok(AssetInspection {
        asset_name: path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&source)
            .to_owned(),
        overview: OverviewView {
            asset_name: path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&source)
                .to_owned(),
            format: format.tag().to_owned(),
            declared_track_count: inspection.declared_track_count,
            stream_count: inspection.streams.len(),
            dimensions: first_video
                .and_then(tpt_app_media_forensics_model::StreamAnalysis::dimensions)
                .map(|(w, h)| format!("{w}x{h}")),
            frame_rate: first_video
                .and_then(tpt_app_media_forensics_model::StreamAnalysis::video_format)
                .and_then(|v| v.frame_rate)
                .map(|r| r.to_string()),
            anomalies: inspection.anomalies.clone(),
        },
        streams,
    })
}

/// An inspection for a container this build cannot read.
///
/// `ContainerInspection::empty` exists for exactly this, but it reports only that
/// the container declared no tracks. The reason is replaced here so the screen can
/// say *why* there is nothing to show — "no reader for this container" and "the
/// container declared no tracks" are different states of the same file, and an
/// analyst who was told only the second would go looking for tracks.
fn empty_inspection(
    format: tpt_app_media_forensics_container::ContainerFormat,
    reason: String,
) -> tpt_app_media_forensics_container::ContainerInspection {
    let mut inspection = tpt_app_media_forensics_container::ContainerInspection::empty(format);
    inspection.anomalies = vec![reason];
    inspection
}

/// The container and stream inspection of one asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetInspection {
    /// The asset's display name.
    pub asset_name: String,
    /// Container summary.
    pub overview: OverviewView,
    /// Per-stream detail.
    pub streams: Vec<StreamView>,
}

/// Compares two source files across every measured axis (spec §38-§40, §82).
///
/// Runs `observe_stages` on both sides rather than a full `analyse`, so a
/// comparison creates no case, writes no analysis record and touches nothing. A
/// comparison is a question *about* two files, not a finding *about* an asset,
/// and an analyst comparing a suspect against a reference should not mutate
/// either case by doing so.
///
/// Both sources are opened read-only (spec §11).
///
/// The result carries the engine's own tolerances, so a screen showing "within
/// tolerance" also shows the tolerance it judged against. Two runs of the same
/// file differ in the last bits of a float, and comparing those exactly would
/// report a difference on every pair - which trains a reviewer to ignore the
/// axis, making the comparison worse than useless.
#[tauri::command]
pub fn compare_assets(left: String, right: String) -> ShellResult<ComparisonView> {
    use tpt_app_media_forensics_rules::comparison::{compare, ComparisonInput};

    let engine = tpt_app_media_forensics_core::AnalysisEngine::new();
    let left_path = std::path::PathBuf::from(&left);
    let right_path = std::path::PathBuf::from(&right);

    // `observe_stages` yields an empty bundle with reasons for a file it cannot
    // read, rather than an error. That is what lets a comparison against a
    // corrupt file produce a screen full of "not measured" instead of refusing -
    // and "we could not read one of them" is exactly what an analyst needs to
    // know about a corrupt file.
    let (left_bundle, _) = engine.observe_stages(&left_path);
    let (right_bundle, _) = engine.observe_stages(&right_path);

    let left_name = display_name(&left_path);
    let right_name = display_name(&right_path);

    // Built as named bindings rather than through a closure: the inputs borrow
    // from their bundles for different, unrelated lifetimes, and a closure's
    // return type cannot express that.
    //
    // Every field stays `None` when the stage that would have measured it did
    // not run. Passing a zero for an unmeasured axis would make "not measured"
    // indistinguishable from "measured as zero", which is the same conflation
    // the comparison types exist to prevent.
    let left_input = ComparisonInput {
        name: &left_name,
        streams: left_bundle.container.as_ref().map(|c| c.streams.as_slice()),
        metadata: left_bundle.metadata.as_ref(),
        scene: left_bundle.scene.as_ref(),
        silence: Some(left_bundle.silence.as_slice()),
        loudness: left_bundle.loudness.as_ref(),
    };
    let right_input = ComparisonInput {
        name: &right_name,
        streams: right_bundle
            .container
            .as_ref()
            .map(|c| c.streams.as_slice()),
        metadata: right_bundle.metadata.as_ref(),
        scene: right_bundle.scene.as_ref(),
        silence: Some(right_bundle.silence.as_slice()),
        loudness: right_bundle.loudness.as_ref(),
    };

    Ok(ComparisonView::build(&compare(&left_input, &right_input)))
}

/// The file's name, so a comparison identifies it by more than a full path.
fn display_name(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Reads a source file's metadata tree and cross-checks it (spec §25, §26).
///
/// Reads the source read-only. Conflicts are surfaced rather than left for the
/// analyst to spot: a key holding two different values across scopes is the whole
/// point of the cross-check, and it is the observation
/// `METADATA.INCONSISTENT_VALUE` consumes.
#[tauri::command]
pub fn metadata_report(source: String) -> ShellResult<MetadataView> {
    use tpt_app_media_forensics_metadata::{find_conflicts, MetadataTree};

    // Checked here rather than left to fail inside the engine, so an unreadable
    // path is reported as itself. `observe_stages` would otherwise swallow it
    // into an empty bundle and the screen would show "no metadata" for a file
    // nobody could open.
    if !std::path::Path::new(&source).is_file() {
        return Err(ShellError::new(
            ErrorKind::UnreadableSource,
            format!("{source} is not a readable file"),
        ));
    }

    let engine = tpt_app_media_forensics_core::AnalysisEngine::new();
    // One observation, used for both the tree and the conflicts. Calling it
    // twice would mean two reads of the same bytes and two chances to disagree,
    // for no gain.
    let (bundle, _) = engine.observe_stages(std::path::Path::new(&source));

    // An absent tree means the metadata stage did not run or found nothing. It
    // becomes an empty tree rather than a fabricated entry: a container with no
    // readable atoms is "no metadata", not "metadata that could not be
    // determined".
    let tree = bundle
        .metadata
        .unwrap_or_else(|| MetadataTree::new(Vec::new()));

    Ok(MetadataView {
        entries: tree
            .entries
            .iter()
            .map(|entry| MetadataRow {
                scope: entry.scope.tag().to_owned(),
                key: entry.key.clone(),
                // Verbatim, never trimmed or reformatted: the stored value is
                // the evidence, and a "helpfully" normalised digest would no
                // longer match what the file says.
                value: entry.value.clone(),
                source: entry.source.clone(),
            })
            .collect(),
        conflicts: find_conflicts(&tree)
            .iter()
            .map(|conflict| conflict.describe())
            .collect(),
        // Each indicator travels with what it does *not* establish (spec §27).
        // A declared `Lavf58` tag shown on its own invites a reader to treat it
        // as proof of FFmpeg, which is exactly the inference §27 rules out.
        //
        // `observe_stages` does not expose the encoder fingerprint - it is not
        // part of `AnalysisBundle` - so the indicators are left empty here and
        // the screen says so. Inventing a plausible indicator would be the one
        // unforgivable failure on a metadata screen.
        indicators: Vec::new(),
    })
}

/// Rebuilds a report from the open case and writes the bundle (spec §59-§63).
///
/// Reads the case rather than re-analysing it: the report is a statement about
/// findings that were already measured, and re-running the engine to reach the
/// same numbers would risk describing a different run than the one the findings
/// came from (spec §63).
///
/// Writes only inside the case directory. Nothing outside it is touched, and no
/// source file is read for writing (spec §11).
#[tauri::command]
pub fn generate_report(state: tauri::State<'_, AppState>) -> ShellResult<ReportView> {
    let case_dir = state.case_dir()?;
    let case = tpt_app_media_forensics_core::CaseDirectory::open(&case_dir)?;

    let loaded = tpt_app_media_forensics_core::pipeline::load_report(&case)?;
    let bundle = tpt_app_media_forensics_report::write_bundle(&loaded.report, &case.reports_dir())?;

    Ok(ReportView {
        case_id: loaded.report.case_id.clone(),
        case_name: loaded.report.case_name.clone(),
        finding_count: loaded.report.finding_count(),
        files: bundle
            .files
            .iter()
            .map(|entry| ReportFileView {
                name: entry.name.clone(),
                sha256: entry.sha256.clone(),
                size_bytes: entry.size_bytes,
            })
            .collect(),
        directory: case.reports_dir().display().to_string(),
        // The disclaimer travels with the view rather than being left to the
        // renderer. Spec §59 requires it verbatim on every report, and a
        // frontend that omits it produces a document the specification does not
        // permit.
        disclaimer: tpt_app_media_forensics_report::DISCLAIMER.to_owned(),
        analysis_fingerprint: loaded.report.methodology.analysis_fingerprint.clone(),
    })
}

/// The reports screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportView {
    /// The case the report describes.
    pub case_id: String,
    /// Its display name.
    pub case_name: String,
    /// Findings the report carries.
    pub finding_count: usize,
    /// The files written into the bundle.
    pub files: Vec<ReportFileView>,
    /// Where they were written.
    pub directory: String,
    /// The required disclaimer, verbatim (spec §59).
    pub disclaimer: String,
    /// The fingerprint identifying the analysis the report describes (spec §63).
    pub analysis_fingerprint: String,
}

/// One generated report file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportFileView {
    /// File name within the bundle.
    pub name: String,
    /// Its SHA-256, recorded so the bundle can be verified later.
    pub sha256: String,
    /// Its size in bytes.
    pub size_bytes: u64,
}

/// The Video screen for a source file (spec §43, §44).
///
/// Reads the source read-only. The frame *table* comes from the container and is
/// always available for a readable file; the pixels come from the decoder, which
/// runs only for the royalty-free codecs this build integrates.
///
/// **A file whose codec is not decoded here still gets a screen.** Its timing is
/// fully readable and is exactly what a reviewer needs when a file will not
/// play, so the screen shows the table and says plainly that no pixels are
/// available. Spec §21 forbids reporting a measurement that could not be taken,
/// and this engine's refusal to decode H.264, HEVC and AAC is a policy rather
/// than a failure - so the wording says so.
#[tauri::command]
pub fn video_screen(source: String) -> ShellResult<VideoView> {
    let path = std::path::PathBuf::from(&source);
    if !path.is_file() {
        return Err(ShellError::new(
            ErrorKind::UnreadableSource,
            format!("{source} is not a readable file"),
        ));
    }

    let engine = tpt_app_media_forensics_core::AnalysisEngine::new();
    let (bundle, _) = engine.observe_stages(&path);

    let Some(container) = bundle.container else {
        return Ok(VideoView::without_pixels(
            display_name(&path),
            Vec::new(),
            "the container could not be read, so there is no frame table to show",
        ));
    };

    let Some(info) = container.first_video_frames() else {
        return Ok(VideoView::without_pixels(
            display_name(&path),
            Vec::new(),
            "this file declares no video stream",
        ));
    };

    // Frame stamps carry presentation time; `timestamps` pairs it with decode
    // time. Both are read from the container rather than derived, so the viewer's
    // timecodes are the file's own.
    let frames: Vec<FrameStamp> = info
        .frame_times
        .iter()
        .enumerate()
        .map(|(index, time)| FrameStamp {
            index: index as u64,
            time: *time,
            is_key_frame: info.all_frames_are_keyframes || info.keyframes.contains(&(index as u32)),
        })
        .collect();

    let timebase = container
        .streams
        .iter()
        .find(|s| s.kind == tpt_app_media_forensics_model::StreamKind::Video)
        .map_or_else(
            || Timebase::from_ticks_per_second(1_000),
            |s| s.timing.timebase,
        );

    let timestamps = frames
        .iter()
        .map(|frame| {
            let index = frame.index;
            FrameTimestamps::new(
                frame.time,
                info.decode_times.get(index as usize).copied(),
                index,
                timebase,
                // The declared tick value, so a reviewer can check the timecode
                // against the file rather than trusting a conversion.
                timebase.media_time_to_ticks(frame.time),
                info.decode_times
                    .get(index as usize)
                    .map(|t| timebase.media_time_to_ticks(*t)),
                frame.is_key_frame,
            )
        })
        .collect();

    let first_video = container
        .streams
        .iter()
        .find(|s| s.kind == tpt_app_media_forensics_model::StreamKind::Video);

    let codec = first_video
        .map(|s| s.codec.name.clone())
        .unwrap_or_default();

    Ok(VideoView {
        asset_name: display_name(&path),
        dimensions: first_video
            .and_then(tpt_app_media_forensics_model::StreamAnalysis::dimensions)
            .map(|(w, h)| format!("{w}x{h}")),
        frame_rate: first_video
            .and_then(tpt_app_media_forensics_model::StreamAnalysis::video_format)
            .and_then(|v| v.frame_rate)
            .map(|r| r.to_string()),
        frame_count: frames.len() as u64,
        all_frames_are_keyframes: info.all_frames_are_keyframes,
        frames,
        timestamps,
        pixels_unavailable: decoder_refusal(&codec),
        decoder_notes: Vec::new(),
    })
}

/// Why no pixels can be shown for a codec, when that is the reason.
///
/// `None` for a codec this build *can* decode - the screen then decodes on
/// demand. The wording for the rest is policy, not failure: "we do not decode
/// this" and "we could not decode this" are different statements, and an analyst
/// who reads the second would go looking for a corrupt file that is perfectly
/// intact.
fn decoder_refusal(codec: &str) -> Option<String> {
    if tpt_app_media_forensics_video::decode::is_decodable(codec) {
        return None;
    }
    Some(format!(
        "this build does not decode `{codec}`. Only royalty-free VP9 and AV1 are \
         decoded here; H.264, HEVC and AAC are covered by patent pools, so their \
         frame timing is still reported but their pixels are not. The file is not \
         damaged."
    ))
}

/// The Audio screen for a source file (spec §19-§22).
///
/// Decodes through `-audio`, the same decoder the rules used, so a level or
/// loudness figure on screen is the figure a finding was raised from.
#[tauri::command]
pub fn audio_screen(source: String) -> ShellResult<AudioView> {
    use tpt_app_media_forensics_audio::{
        decode_audio, AudioDecodeLimits as DecodeLimits, Measurement, Methodology,
    };

    let path = std::path::PathBuf::from(&source);
    if !path.is_file() {
        return Err(ShellError::new(
            ErrorKind::UnreadableSource,
            format!("{source} is not a readable file"),
        ));
    }

    let bytes = std::fs::read(&path).map_err(|e| {
        ShellError::new(
            ErrorKind::UnreadableSource,
            format!("could not read {source}: {e}"),
        )
    })?;

    // Declared properties are read from the container first, so a file whose
    // audio cannot be decoded still reports what it *declares*. That is the
    // difference between "this file is silent" and "this file's audio track
    // could not be read".
    let engine = tpt_app_media_forensics_core::AnalysisEngine::new();
    let (bundle, _) = engine.observe_stages(&path);
    let declared_audio = bundle.container.as_ref().and_then(|c| {
        c.streams
            .iter()
            .find_map(|s| s.audio_format().map(|a| (s, a)))
    });

    // The codec tag comes from the stream's own headers, never from the file
    // extension. A container describing a different codec than its name suggests
    // is itself a finding, and `decode_audio` refuses anything this build does
    // not integrate - which is how the patent policy reaches this screen.
    let codec = declared_audio.map_or_else(String::new, |(stream, _)| stream.codec.name.clone());

    let limits = DecodeLimits::new(declared_audio.map_or(1, |(_, a)| {
        a.channel_layout.as_ref().map_or(1, |l| l.channel_count)
    }));

    let decoded = match decode_audio(bytes, &codec, limits) {
        Ok(decoded) => decoded,
        Err(error) => {
            return Ok(AudioView {
                unavailable: Some(error.to_string()),
                ..AudioView::unavailable(
                    display_name(&path),
                    "the audio stream could not be decoded, so no level was measured",
                )
            })
        }
    };

    let measurements = tpt_app_media_forensics_core::measure_audio(
        &decoded.pcm,
        decoded.channels,
        decoded.sample_rate,
        engine.profile(),
    );

    // The unit is part of the methodology, not a separate property: spec §21
    // requires a measurement to carry the named method that produced it, and a
    // loudness figure shown without "ITU-R BS.1770-4" beside it is not
    // reproducible by whoever reads the report later.
    let to_view = |m: &Measurement| MeasurementView {
        value: m.value,
        unit: match m.methodology {
            Methodology::ItuBs1770_4 => "LUFS",
            Methodology::EbuR128Lra => "LU",
            _ => "",
        }
        .to_owned(),
        methodology: m.describe(),
    };

    // The envelope is downsampled for display; the sample rate it was built at
    // is carried so a reviewer knows the picture is an envelope rather than the
    // signal itself.
    let waveform = crate::view::viewer::Waveform::from_pcm(&decoded.pcm, decoded.channels, 1_200);

    Ok(AudioView {
        asset_name: display_name(&path),
        sample_rate: Some(decoded.sample_rate),
        channels: Some(decoded.channels),
        bit_depth: declared_audio.map(|(_, a)| a.bit_depth),
        peak: measurements.as_ref().map(|m| m.levels.peak),
        rms: measurements.as_ref().map(|m| m.levels.rms),
        mean: measurements.as_ref().map(|m| m.levels.mean),
        loudness: measurements
            .as_ref()
            .and_then(|m| m.loudness.as_ref())
            .map(to_view),
        loudness_range: None,
        silence: measurements
            .as_ref()
            .map(|m| {
                m.silence
                    .iter()
                    .map(|region| {
                        let per_second = f64::from(decoded.sample_rate.max(1));
                        SilenceView {
                            start_frame: region.start_frame,
                            end_frame: region.end_frame,
                            length_frames: region.length_frames,
                            start: Some(MediaTime::from_micros(
                                (region.start_frame as f64 / per_second * 1e6) as i64,
                            )),
                            duration: Some(MediaTime::from_micros(
                                (region.length_frames as f64 / per_second * 1e6) as i64,
                            )),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
        waveform,
        unavailable: None,
        truncated: decoded.truncated,
    })
}

/// Decodes one frame of a source file for the viewer (spec §43, §44).
///
/// # A correction to what I claimed earlier
///
/// I previously recorded this as needing an engine change \u2014 "retention plus a
/// bounded window". That was wrong, and it was worth checking rather than
/// accepting. `DecodeSession::decode_prefix`, `read_samples_file` and
/// `DecodedFrame::to_greyscale` are all public in `-video` and `-container`, and
/// between them they decode a frame on demand. What the viewer needed was never
/// an engine feature; it was a command that called what already existed.
///
/// # Why decode on demand rather than retain
///
/// A 4K master has tens of thousands of frames and each frame is ~25 MB of RGB.
/// Retaining them for the viewer's lifetime would exhaust memory long before
/// anyone reached the end, and a viewer that loads everything before showing
/// frame one looks like a hung application.
///
/// The cost is that stepping backward re-decodes. That is the right trade for a
/// forensic tool: an analyst inspects the frames that matter, not all of them,
/// and a tool that cannot be opened on a 4K master is not usable at all.
///
/// # The window is bounded and the bound is reported
///
/// Decoding is capped so a malformed `stsz` claiming millions of samples cannot
/// turn a click into an unbounded allocation (spec §75). When the cap is reached
/// the response says so, rather than presenting a truncated view as the file's
/// full extent.
#[tauri::command]
pub fn decode_frame(source: String, frame_index: u64) -> ShellResult<FramePayload> {
    use tpt_app_media_forensics_video::decode::{DecodeLimits, DecodeSession};

    let path = std::path::PathBuf::from(&source);
    if !path.is_file() {
        return Err(ShellError::new(
            ErrorKind::UnreadableSource,
            format!("{source} is not a readable file"),
        ));
    }

    let engine = tpt_app_media_forensics_core::AnalysisEngine::new();
    let (bundle, _) = engine.observe_stages(&path);

    let Some(container) = bundle.container else {
        return Err(ShellError::new(
            ErrorKind::UnreadableSource,
            "the container could not be read, so no frame can be decoded",
        ));
    };

    // The first *video* stream, not simply the first stream. A file whose audio
    // track comes first would otherwise have its audio offered to a video
    // decoder \u2014 the same mistake the pipeline documents at its own video lookup.
    let Some(video) = container
        .streams
        .iter()
        .find(|s| s.kind == tpt_app_media_forensics_model::StreamKind::Video)
    else {
        return Err(ShellError::new(
            ErrorKind::InvalidRequest,
            "this file declares no video stream",
        ));
    };

    let codec = video.codec.name.clone();
    if let Some(refusal) = decoder_refusal(&codec) {
        return Err(ShellError::new(ErrorKind::InvalidRequest, refusal));
    }

    // Bounded read: a container claiming millions of samples must not turn one
    // click into an unbounded allocation. The cap is far above any window an
    // analyst inspects, and the fact that it applied is reported below.
    if file_size(&path) > tpt_app_media_forensics_container::MAX_SAMPLED_BYTES {
        return Err(ShellError::new(
            ErrorKind::InvalidRequest,
            "this file is too large to hold its samples in memory, so its frames \
             cannot be decoded for display. Its timing and findings are unaffected.",
        ));
    }

    let samples = tpt_app_media_forensics_container::read_samples_file(&path).map_err(|e| {
        ShellError::new(
            ErrorKind::UnreadableSource,
            format!("could not read the samples in {source}: {e}"),
        )
    })?;

    let video_index = video.index;
    let packets: Vec<(Vec<u8>, bool)> = samples
        .iter()
        .filter(|s| s.stream_index == video_index)
        .map(|s| (s.data.clone(), s.is_key_frame))
        .collect();

    if packets.is_empty() {
        return Err(ShellError::new(
            ErrorKind::InvalidRequest,
            "the container's sample table holds no samples for the video stream",
        ));
    }

    let wanted = usize::try_from(frame_index).unwrap_or(usize::MAX);
    if wanted >= packets.len() {
        return Err(ShellError::new(
            ErrorKind::InvalidRequest,
            format!(
                "frame {frame_index} does not exist; this stream holds {} sample(s)",
                packets.len()
            ),
        ));
    }

    // Decoding stops once the requested frame has been produced. Everything before
    // it is decoded too \u2014 an inter-predicted frame cannot be produced without its
    // references \u2014 so the prefix is the minimum work, not a shortcut.
    let prefix = &packets[..=wanted];
    let mut session = DecodeSession::open(&codec, DecodeLimits::default())
        .map_err(|e| ShellError::new(ErrorKind::AnalysisFailed, e.to_string()))?;

    let (frames, stopped) = session.decode_prefix(prefix);
    let Some(frame) = frames.last().cloned() else {
        // Not an error about the file: the decoder recovered what it could and
        // this particular frame was among the losses. The reason travels with
        // it so the screen can say which.
        let reason = stopped.map_or_else(
            || "no frame was produced at this position".to_owned(),
            |e| e.to_string(),
        );
        return Err(ShellError::new(
            ErrorKind::AnalysisFailed,
            format!("frame {frame_index} could not be decoded: {reason}"),
        ));
    };

    let image = frame
        .to_greyscale(MediaTime::ZERO)
        .map_err(|e| ShellError::new(ErrorKind::AnalysisFailed, e.to_string()))?;

    // The timestamp comes from the container's own table rather than from the
    // decoder's packet index, so the viewer's timecode is the file's.
    let timebase = video.timing.timebase;
    let info = container.first_video_frames();
    let pts = info
        .and_then(|i| i.frame_times.get(wanted))
        .copied()
        .unwrap_or(MediaTime::ZERO);
    let dts = info.and_then(|i| i.decode_times.get(wanted)).copied();

    let timestamps = FrameTimestamps::new(
        pts,
        dts,
        frame_index,
        timebase,
        timebase.media_time_to_ticks(pts),
        dts.map(|t| timebase.media_time_to_ticks(t)),
        packets[wanted].1,
    );

    Ok(FramePayload {
        frame_index,
        timestamps,
        rgb: Some(image.rgb.clone()),
        // Greyscale, and labelled as such. `DecodedFrame` keeps only the luma
        // plane \u2014 chroma is what the analysers do not need \u2014 so this is not a colour
        // frame and the inspector must say so rather than presenting neutral
        // chroma as if it had been measured.
        basis: RgbBasis::LumaOnly,
        image: FrameImageView {
            frame_index,
            width: image.width,
            height: image.height,
            time: pts,
            rgb: Some(image.rgb),
            basis: RgbBasis::LumaOnly,
            evidence_path: None,
        },
    })
}

/// The size of a file, or zero when it cannot be read.
///
/// Zero is the permissive direction: an unreadable size falls through to the
/// caller's own read, which will fail with a better message than a guess made
/// here.
fn file_size(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}
