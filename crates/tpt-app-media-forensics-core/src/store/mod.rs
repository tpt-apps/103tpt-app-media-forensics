//! SQLite persistence for a case (spec §52).
//!
//! Stores cases, assets, analyses, streams, findings, evidence, rule results,
//! reports, and analyst notes.
//!
//! # The database is part of the record
//!
//! `case.db` lives inside the case directory and is not disposable. The only
//! removable directory is `cache/`. Deleting this file destroys the record of
//! what was examined.
//!
//! # Integrity pragmas
//!
//! Foreign keys are enforced (`PRAGMA foreign_keys = ON`), which SQLite does
//! *not* do by default. Without it a finding could reference an analysis that
//! does not exist, and the database would happily accept it — producing a
//! report that cites evidence from nowhere.

pub mod schema;
pub mod search;

pub use search::{SearchQuery, SearchResult, SearchScope, SeverityFilter};

use rusqlite::{Connection, OptionalExtension};

use tpt_app_media_forensics_model::time::MediaTime;

use crate::error::CoreError;

/// A case database.
pub struct Store {
    connection: Connection,
}

impl Store {
    /// Opens (creating if necessary) the database inside a case directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created, the database cannot
    /// be opened, or migrations fail.
    pub fn open(case_dir: &std::path::Path) -> Result<Self, CoreError> {
        std::fs::create_dir_all(case_dir).map_err(|e| {
            CoreError::io("create case directory", case_dir.display().to_string(), e)
        })?;
        Self::open_file(&case_dir.join("case.db"))
    }

    /// Opens a database at a specific path.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be opened or migrated.
    pub fn open_file(path: &std::path::Path) -> Result<Self, CoreError> {
        let connection =
            Connection::open(path).map_err(|e| CoreError::database("open case database", e))?;
        Self::from_connection(connection)
    }

    /// Opens an in-memory database, for tests.
    ///
    /// # Errors
    ///
    /// Returns an error if pragmas or migrations fail.
    pub fn open_in_memory() -> Result<Self, CoreError> {
        let connection = Connection::open_in_memory()
            .map_err(|e| CoreError::database("open case database", e))?;
        Self::from_connection(connection)
    }

    /// Opens a database from an existing connection.
    ///
    /// # Errors
    ///
    /// Returns an error if pragmas or migrations fail.
    pub fn from_connection(connection: Connection) -> Result<Self, CoreError> {
        // SQLite leaves foreign keys off by default, per connection.
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| CoreError::database("enable foreign keys", e))?;
        // Durability: the record must survive a crash mid-write.
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| CoreError::database("enable write-ahead log", e))?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(|e| CoreError::database("set durability", e))?;

        schema::apply_migrations(&connection)
            .map_err(|e| CoreError::database("apply schema migrations", e))?;

        Ok(Self { connection })
    }

    /// Returns the underlying connection.
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Returns the current schema version.
    ///
    /// # Errors
    ///
    /// Returns an error if the pragma cannot be read.
    pub fn schema_version(&self) -> rusqlite::Result<i64> {
        self.connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
    }

    /// Runs `f` inside a transaction, committing on success.
    ///
    /// # Errors
    ///
    /// Returns an error if the transaction cannot begin, if `f` fails, or if
    /// the commit fails.
    pub fn transaction<T>(
        &mut self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T> {
        let tx = self.connection.transaction()?;
        let value = f(&tx)?;
        tx.commit()?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_database_is_migrated_to_the_current_version() {
        let store = Store::open_in_memory().expect("opens");
        assert_eq!(
            store.schema_version().expect("reads"),
            schema::SCHEMA_VERSION
        );
    }

    #[test]
    fn migrations_are_idempotent() {
        // Re-running on an already-migrated database must be a no-op, or
        // opening a case twice would fail.
        let connection = Connection::open_in_memory().expect("opens");
        schema::apply_migrations(&connection).expect("first");
        schema::apply_migrations(&connection).expect("second");
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("reads");
        assert_eq!(version, schema::SCHEMA_VERSION);
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let store = Store::open_in_memory().expect("opens");
        let enabled: i64 = store
            .connection()
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("reads");
        assert_eq!(
            enabled, 1,
            "foreign keys must be on for referential integrity"
        );
    }

    #[test]
    fn a_finding_cannot_reference_a_missing_analysis() {
        // Without enforced foreign keys this insert would silently succeed and
        // the database would later cite evidence from nowhere.
        let store = Store::open_in_memory().expect("opens");
        let result = store.connection().execute(
            "INSERT INTO findings (id, analysis_id, asset_id, rule_id, severity, \
             confidence, summary, measurements) \
             VALUES ('f1', 'missing', 'missing', 'R', 'WARNING', 'High', 's', '[]')",
            [],
        );
        assert!(result.is_err(), "a dangling finding must be rejected");
    }

    #[test]
    fn a_newer_schema_is_refused_rather_than_misread() {
        let connection = Connection::open_in_memory().expect("opens");
        connection
            .pragma_update(None, "user_version", 9_999_i64)
            .expect("sets");
        assert!(
            schema::apply_migrations(&connection).is_err(),
            "a newer case must not be opened by an older build"
        );
    }

    #[test]
    fn a_transaction_commits_on_success() {
        let mut store = Store::open_in_memory().expect("opens");
        store
            .transaction(|c| {
                c.execute("INSERT INTO cases (id, name) VALUES ('c1', 'Alpha')", [])?;
                Ok(())
            })
            .expect("commits");

        let name: String = store
            .connection()
            .query_row("SELECT name FROM cases WHERE id = 'c1'", [], |r| r.get(0))
            .expect("reads");
        assert_eq!(name, "Alpha");
    }

    #[test]
    fn a_failed_transaction_rolls_back() {
        let mut store = Store::open_in_memory().expect("opens");
        store
            .transaction(|c| {
                c.execute("INSERT INTO cases (id, name) VALUES ('c1', 'Alpha')", [])?;
                Err::<(), _>(rusqlite::Error::InvalidQuery)
            })
            .expect_err("the closure failed");

        let count: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM cases", [], |r| r.get(0))
            .expect("reads");
        assert_eq!(count, 0, "a failed transaction must leave no partial rows");
    }

    #[test]
    fn duplicate_asset_hashes_are_rejected_within_a_case() {
        let store = Store::open_in_memory().expect("opens");
        store
            .connection()
            .execute("INSERT INTO cases (id, name) VALUES ('c1', 'Alpha')", [])
            .expect("inserts case");

        let insert = |id: &str| {
            store.connection().execute(
                "INSERT INTO assets (id, case_id, name, media_type, source_path, \
                 size_bytes, sha256) VALUES (?1, 'c1', 'x.mp4', 'container', 'x.mp4', 1, 'aa')",
                [id],
            )
        };
        assert!(insert("a1").is_ok());
        assert!(
            insert("a2").is_err(),
            "two assets cannot claim the same content in one case"
        );
    }

    #[test]
    fn a_finding_is_not_modified_by_a_later_review() {
        // The review workflow records a verdict beside the observation, never in
        // place of it (spec §66).
        let store = Store::open_in_memory().expect("opens");
        let c = store.connection();
        c.execute("INSERT INTO cases (id, name) VALUES ('c1', 'A')", [])
            .unwrap();
        c.execute(
            "INSERT INTO assets (id, case_id, name, media_type, source_path, size_bytes, sha256) \
             VALUES ('a1','c1','x.mp4','container','x.mp4',1,'aa')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO analyses (id, case_id, asset_id, cache_key, software_version, \
             analysis_version, status) VALUES ('an1','c1','a1','k','1.0.0',1,'COMPLETE')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO findings (id, analysis_id, asset_id, rule_id, severity, confidence, \
             summary, measurements) VALUES ('f1','an1','a1','R','WARNING','High','original','[]')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO finding_reviews (finding_id, analysis_id, status, note) \
             VALUES ('f1','an1','ACCEPTED','expected')",
            [],
        )
        .unwrap();

        let (summary, status): (String, String) = c
            .query_row(
                "SELECT f.summary, r.status FROM findings f \
                 JOIN finding_reviews r ON r.finding_id = f.id \
                 AND r.analysis_id = f.analysis_id WHERE f.id = 'f1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(summary, "original", "the observation must be untouched");
        assert_eq!(status, "ACCEPTED");
    }

    #[test]
    fn the_same_finding_can_be_recorded_by_two_analyses() {
        // A finding id is derived from the observation, so re-analysing the same
        // file under a different profile produces the same id. It must be stored
        // as a separate row rather than colliding (spec §66: re-analysing
        // produces new findings, it never edits old ones).
        let store = Store::open_in_memory().expect("opens");
        let c = store.connection();
        c.execute("INSERT INTO cases (id, name) VALUES ('c1', 'A')", [])
            .unwrap();
        c.execute(
            "INSERT INTO assets (id, case_id, name, media_type, source_path, size_bytes, sha256) \
             VALUES ('a1','c1','x.mp4','container','x.mp4',1,'aa')",
            [],
        )
        .unwrap();
        for analysis in ["an1", "an2"] {
            c.execute(
                "INSERT INTO analyses (id, case_id, asset_id, cache_key, software_version, \
                 analysis_version, status) VALUES (?1,'c1','a1','k','1.0.0',1,'COMPLETE')",
                [analysis],
            )
            .unwrap();
            c.execute(
                "INSERT INTO findings (id, analysis_id, asset_id, rule_id, severity, confidence, \
                 summary, measurements) VALUES ('f1', ?1, 'a1','R','WARNING','High','s','[]')",
                [analysis],
            )
            .unwrap();
        }

        let count: i64 = c
            .query_row("SELECT COUNT(*) FROM findings WHERE id = 'f1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            count, 2,
            "the same observation under two analyses is two rows"
        );
    }
}

/// A stored asset, as read back from the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAsset {
    /// Asset identifier.
    pub id: String,
    /// Case the asset belongs to.
    pub case_id: String,
    /// Display name.
    pub name: String,
    /// Absolute path recorded at acquisition.
    pub source_path: String,
    /// Size in bytes.
    pub size_bytes: u64,
    /// SHA-256 recorded at acquisition, verbatim.
    pub sha256: Option<String>,
    /// BLAKE3 recorded at acquisition, verbatim.
    pub blake3: Option<String>,
    /// The role this asset plays in the case (spec §67), verbatim.
    ///
    /// `None` means "not recorded as any role" — either genuinely undesignated, or
    /// written by a build predating the column. Both read the same way on purpose:
    /// an asset nobody called a reference must never be used as one.
    pub role: Option<String>,
}

impl StoredAsset {
    /// Whether this asset is the case's declared reference (spec §67).
    ///
    /// The content digest is already on the row, so a reference is bound to
    /// specific bytes the moment it is designated — nothing further has to be
    /// recorded to make "what changed?" reproducible.
    #[must_use]
    pub fn is_reference(&self) -> bool {
        self.role.as_deref() == Some(AssetRole::Reference.tag())
    }
}

/// The role an asset plays in a case (spec §67).
///
/// A tag rather than a boolean so the schema does not have to change to record a
/// second role; see [`schema`]'s migration for why the column is nullable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetRole {
    /// The asset every delivery in the case is measured against.
    ///
    /// Spec §67's "known reference": the master a delivery is checked against to
    /// answer "what changed?".
    Reference,
}

impl AssetRole {
    /// The stored spelling. Stable, because the database outlives the enum.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::Reference => "reference",
        }
    }
}

/// The stored spelling of how a timeline position was arrived at.
///
/// A stable tag rather than `{:?}`: the database is a record that outlives the
/// Rust enum, and a debug rendering would change with the type's name. These
/// columns exist for querying; `payload` is the authority, so a tag that ever
/// fell behind would not corrupt the record.
fn placement_tag(placement: tpt_app_media_forensics_model::timeline::Placement) -> &'static str {
    use tpt_app_media_forensics_model::timeline::Placement;
    match placement {
        Placement::Measured => "measured",
        Placement::Inferred => "inferred",
        // An entry with no position is *unplaced*, which is not the same as
        // placed at zero. Recording it as `measured` at time 0 would fabricate
        // the one position the engine never established.
        Placement::Unplaced => "unplaced",
    }
}

/// The stored spelling of which stage produced a timeline entry.
fn source_tag(source: tpt_app_media_forensics_model::timeline::TimelineSource) -> &'static str {
    use tpt_app_media_forensics_model::timeline::TimelineSource;
    match source {
        TimelineSource::StructuralDamage => "structural_damage",
        TimelineSource::Timestamp => "timestamp",
        TimelineSource::Finding => "finding",
        TimelineSource::PacketDamage => "packet_damage",
        TimelineSource::DecodeDamage => "decode_damage",
    }
}
impl Store {
    /// Inserts or replaces a case.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn upsert_case(
        &self,
        id: &str,
        name: &str,
        description: Option<&str>,
    ) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO cases (id, name, description) VALUES (?1, ?2, ?3) \
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, \
             description = excluded.description",
            rusqlite::params![id, name, description],
        )?;
        Ok(())
    }

    /// Inserts an asset.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails, including when an asset with the
    /// same SHA-256 already exists in this case.
    pub fn insert_asset(&self, asset: &StoredAsset) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO assets (id, case_id, name, media_type, source_path, size_bytes, \
             sha256, blake3) VALUES (?1, ?2, ?3, 'container', ?4, ?5, ?6, ?7)",
            rusqlite::params![
                asset.id,
                asset.case_id,
                asset.name,
                asset.source_path,
                i64::try_from(asset.size_bytes).unwrap_or(i64::MAX),
                asset.sha256,
                asset.blake3,
            ],
        )?;
        Ok(())
    }

    /// Reads every asset in a case, ordered by identifier for determinism.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub fn assets_in_case(&self, case_id: &str) -> rusqlite::Result<Vec<StoredAsset>> {
        let mut stmt = self.connection.prepare(
            "SELECT id, case_id, name, source_path, size_bytes, sha256, blake3, role \
             FROM assets WHERE case_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([case_id], |row| {
            Ok(StoredAsset {
                id: row.get(0)?,
                case_id: row.get(1)?,
                name: row.get(2)?,
                source_path: row.get(3)?,
                size_bytes: u64::try_from(row.get::<_, i64>(4)?).unwrap_or(0),
                sha256: row.get(5)?,
                blake3: row.get(6)?,
                role: row.get(7)?,
            })
        })?;
        rows.collect()
    }

    /// Records an asset's role in the case (spec §67).
    ///
    /// # Why this is a write and not a derived fact
    ///
    /// "This is the master" is a decision somebody made about a file, and nothing
    /// in the bytes can recover it. The engine can tell a delivery from a master by
    /// inspecting both, and it will be wrong sometimes — which of two encodes is
    /// authoritative is a question about the job, not the media. So it is recorded,
    /// and recorded against the asset rather than the case.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails, including when `case_id` does not name
    /// an asset in that case. Silently succeeding would leave the designation
    /// claimed and unrecorded, which is the one outcome a reviewer could not detect.
    pub fn set_asset_role(
        &self,
        case_id: &str,
        asset_id: &str,
        role: Option<AssetRole>,
    ) -> rusqlite::Result<()> {
        let updated = self.connection.execute(
            "UPDATE assets SET role = ?3 WHERE case_id = ?1 AND id = ?2",
            rusqlite::params![case_id, asset_id, role.map(AssetRole::tag)],
        )?;
        if updated == 0 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok(())
    }

    /// The case's declared reference assets (spec §67).
    ///
    /// A list rather than a single asset because a case may legitimately hold more
    /// than one master — a campaign's masters, one per deliverable — and collapsing
    /// them to "the" reference would have to pick one arbitrarily. Ordered by id so
    /// two reads of the same case agree (spec §77).
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub fn reference_assets(&self, case_id: &str) -> rusqlite::Result<Vec<StoredAsset>> {
        Ok(self
            .assets_in_case(case_id)?
            .into_iter()
            .filter(StoredAsset::is_reference)
            .collect())
    }

    /// The one asset matching `selector` by id, else by exact name (spec §67).
    ///
    /// An analyst has the file name to hand, not the row id, so a name has to work.
    /// Names are matched exactly rather than fuzzily: two assets in one case can
    /// share a name only if their content differs, and picking the "closest" match
    /// would designate a master nobody chose. Ambiguity is reported rather than
    /// resolved, because the alternative is measuring a delivery against a file the
    /// analyst did not name.
    ///
    /// # Errors
    ///
    /// Returns an error when the query fails. A miss is `Ok(None)`.
    pub fn find_asset(
        &self,
        case_id: &str,
        selector: &str,
    ) -> rusqlite::Result<Option<StoredAsset>> {
        let assets = self.assets_in_case(case_id)?;
        if let Some(by_id) = assets.iter().find(|a| a.id == selector) {
            return Ok(Some(by_id.clone()));
        }
        if let Some(exact) = assets.iter().find(|a| a.name == selector) {
            return Ok(Some(exact.clone()));
        }

        // Case-insensitive fallback, and only when it is unambiguous. A file name
        // comes off a filesystem and through an analyst's keyboard, and on Windows
        // neither is case-sensitive, so `MASTER.MP4` must reach `master.mp4`. But
        // two assets in one case differing *only* in case are two different files,
        // and picking one would designate a master nobody named.
        let lowered = selector.to_lowercase();
        let mut matches = assets
            .into_iter()
            .filter(|a| a.name.to_lowercase() == lowered);
        let first = matches.next();
        if matches.next().is_some() {
            return Ok(None);
        }
        Ok(first)
    }

    /// Counts rows in a table, for verification.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub fn count(&self, table: &str) -> rusqlite::Result<i64> {
        // The table name cannot be a bound parameter. It comes from engine code
        // and a test, never from analyst input, so it is validated rather than
        // interpolated blindly.
        const TABLES: [&str; 10] = [
            "cases",
            "assets",
            "analyses",
            "streams",
            "findings",
            "finding_reviews",
            "evidence",
            "rule_results",
            "reports",
            "notes",
        ];
        if !TABLES.contains(&table) {
            return Err(rusqlite::Error::InvalidParameterName(
                "unknown table".to_owned(),
            ));
        }
        self.connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
    }
}

/// One analyst note, as stored (spec §65).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredNote {
    /// Row identifier, unique within the case.
    pub id: i64,
    /// What kind of thing the note is about, e.g. `"asset"` or `"finding"`.
    pub subject_kind: Option<String>,
    /// Which one, when the note is attached to a subject.
    pub subject_id: Option<String>,
    /// The analyst's text, stored verbatim.
    pub body: String,
    /// When the note was written, in Unix seconds.
    pub created_at: i64,
}

impl StoredNote {
    /// Whether this note is attached to a specific subject.
    #[must_use]
    pub fn is_attached(&self) -> bool {
        self.subject_kind.is_some() && self.subject_id.is_some()
    }
}

/// One reviewer verdict, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredReview {
    /// The verdict, as a [`tpt_app_media_forensics_model::FindingStatus`] tag.
    pub status: String,
    /// The reviewer's note, if they wrote one.
    pub note: Option<String>,
    /// When the review was recorded, in Unix seconds.
    pub reviewed_at: i64,
}

/// A stored analysis run, as read back from the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAnalysis {
    /// Analysis identifier.
    pub id: String,
    /// Case the analysis belongs to.
    pub case_id: String,
    /// Asset that was analysed.
    pub asset_id: String,
    /// Cache key that produced this run (spec §54).
    pub cache_key: String,
    /// Findings produced.
    pub finding_count: i64,
    /// Rules that ran.
    pub rule_count: i64,
    /// Profile identifier used for the run.
    pub profile: String,
    /// Profile threshold fingerprint.
    pub profile_fingerprint: String,
    /// Rule-set fingerprint.
    pub rule_set_fingerprint: String,
    /// When the run started, in Unix seconds.
    pub started_at: i64,
}

/// How much of a case's timeline is a record of what the runs observed.
///
/// Schema v4 began retaining the timeline with the run. A case whose analyses
/// predate it opens cleanly and shows an empty strip, and the empty strip is
/// true — nothing was retained — but it reads as "this run found nothing", which
/// is the more dangerous of the two claims in a forensic tool. This type exists so
/// the screen can tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineRetention {
    /// The case holds no analysis at all.
    ///
    /// Distinct from `Complete` with an empty strip: nothing was examined, which
    /// is not the same as examining something and observing nothing.
    NoRuns,
    /// Every recorded run retained its timeline, so the strip is a complete
    /// record of what those runs observed.
    Complete,
    /// At least one run predates timeline retention and recorded no strip.
    ///
    /// The strip still shows what the *later* runs found. What it cannot show is
    /// anything about the earlier ones, and an analyst must be told so rather than
    /// shown a partial strip that looks whole.
    Partial {
        /// Runs that retained no timeline.
        unrecorded_runs: usize,
        /// Runs recorded in the case.
        total_runs: usize,
    },
}

impl TimelineRetention {
    /// Whether the strip accounts for every run in the case.
    ///
    /// `Partial` is not complete even though the strip draws: the markers are the
    /// later runs', and their absence from the earlier runs is a gap in the record
    /// rather than an absence of observations.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        matches!(self, Self::Complete)
    }
}

impl Store {
    /// Records an analysis run.
    ///
    /// Returns an error if the write fails, including when a finding is inserted
    /// against an analysis that does not exist.
    pub fn insert_analysis(&self, analysis: &StoredAnalysis) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO analyses (id, case_id, asset_id, cache_key, software_version, \
         analysis_version, status, finding_count, rule_count, profile, \
         profile_fingerprint, rule_set_fingerprint, started_at, writer_schema_version) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'COMPLETE', ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                analysis.id,
                analysis.case_id,
                analysis.asset_id,
                analysis.cache_key,
                env!("CARGO_PKG_VERSION"),
                i64::from(tpt_app_media_forensics_model::AnalysisVersion::CURRENT.value()),
                analysis.finding_count,
                analysis.rule_count,
                analysis.profile,
                analysis.profile_fingerprint,
                analysis.rule_set_fingerprint,
                analysis.started_at,
                schema::SCHEMA_VERSION,
            ],
        )?;
        Ok(())
    }

    /// How much of a case's timeline can be read as a record of what was found.
    ///
    /// Three outcomes, and the difference between the last two is the whole
    /// point:
    ///
    /// * `Complete` — every recorded run was written by a build that persisted
    ///   the timeline, so an empty strip means the run observed nothing.
    /// * `Partial { unrecorded_runs }` — at least one run predates timeline
    ///   retention. Its strip was never written, and no conclusion about that
    ///   run can be drawn from this case.
    /// * `NoRuns` — the case has no analysis at all, which is not the same as a
    ///   run that found nothing.
    ///
    /// # Errors
    ///
    /// Returns a `rusqlite` error if the query fails.
    pub fn timeline_retention_in_case(&self, case_id: &str) -> rusqlite::Result<TimelineRetention> {
        // The count is over `analyses`, not `timeline_entries`: a run that
        // recorded an empty strip and a run that recorded nothing at all are
        // indistinguishable from the entries alone, and treating them as equal
        // is the conflation this type exists to prevent.
        let (runs, unrecorded): (i64, i64) = self.connection.query_row(
            "SELECT COUNT(*), \
             COALESCE(SUM(writer_schema_version IS NULL), 0) FROM analyses \
             WHERE case_id = ?1",
            [case_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        let runs = usize::try_from(runs).unwrap_or(usize::MAX);
        let unrecorded = usize::try_from(unrecorded).unwrap_or(usize::MAX);
        Ok(match (runs, unrecorded) {
            (0, _) => TimelineRetention::NoRuns,
            (_, 0) => TimelineRetention::Complete,
            (_, n) => TimelineRetention::Partial {
                unrecorded_runs: n,
                total_runs: runs,
            },
        })
    }

    /// Records every rule that ran, so a case shows what was considered even when
    /// a rule raised nothing.
    pub fn insert_rule_results(
        &self,
        analysis_id: &str,
        rule_ids: &[String],
    ) -> rusqlite::Result<()> {
        for rule_id in rule_ids {
            self.connection.execute(
                "INSERT OR IGNORE INTO rule_results (analysis_id, rule_id) VALUES (?1, ?2)",
                rusqlite::params![analysis_id, rule_id],
            )?;
        }
        Ok(())
    }

    /// Appends a finding to an analysis.
    ///
    /// The complete finding is stored as canonical JSON in `payload` alongside the
    /// typed columns, so `report` re-renders the engine's output exactly rather
    /// than a reconstruction from lossy columns. Append-only: an existing finding
    /// is never updated (spec §66).
    /// Records the timeline an analysis produced (spec §31).
    ///
    /// The engine merges structural damage, timestamp anomalies and positioned
    /// findings into one ordered strip and returns it in `AnalysisOutcome`.
    /// Without this call the strip is built, handed back, and dropped — which is
    /// what left the video, audio, scene and error layers permanently empty for
    /// every case reopened after its run.
    ///
    /// `position` is the index within the timeline and is what preserves the
    /// engine's ordering. The stored order is the run's order, so re-reading it
    /// reproduces the strip exactly; it is not re-sorted on the way out, because
    /// re-deriving an order at read time is how two renderings of one run come
    /// to disagree.
    ///
    /// # Errors
    ///
    /// Returns a `rusqlite` error if the insert fails. Serialisation failure is
    /// reported rather than swallowed: an entry that could not be written is a
    /// run whose record is incomplete, and a silently dropped observation is
    /// exactly the failure this whole table exists to prevent.
    pub fn insert_timeline(
        &self,
        analysis_id: &str,
        asset_id: &str,
        timeline: &tpt_app_media_forensics_model::Timeline,
    ) -> rusqlite::Result<()> {
        for (position, entry) in timeline.entries.iter().enumerate() {
            let payload = serde_json::to_string(entry)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            let micros = entry.time.map_or(0, MediaTime::as_micros);
            self.connection.execute(
                "INSERT INTO timeline_entries (analysis_id, asset_id, position, time_micros, \
                 placement, source, reference, summary, payload) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    analysis_id,
                    asset_id,
                    i64::try_from(position).unwrap_or(i64::MAX),
                    micros,
                    placement_tag(entry.placement),
                    source_tag(entry.source),
                    entry.reference,
                    entry.summary,
                    payload,
                ],
            )?;
        }
        Ok(())
    }

    /// Every timeline entry recorded for a case, in the order the runs produced.
    ///
    /// Entries are read back from `payload`, which is the authority: the typed
    /// columns exist for querying, not for reconstructing. An entry whose payload
    /// cannot be parsed is skipped rather than fatal, so one unreadable row does
    /// not make an entire case unopenable.
    ///
    /// Ordering is `(position, id)` per analysis, analyses by start time, so a
    /// case opened twice draws the same strip both times (spec §77).
    ///
    /// # Errors
    ///
    /// Returns a `rusqlite` error if the query fails.
    pub fn timeline_in_case(
        &self,
        case_id: &str,
    ) -> rusqlite::Result<Vec<tpt_app_media_forensics_model::TimelineEntry>> {
        let mut statement = self.connection.prepare(
            "SELECT te.payload FROM timeline_entries te \
             JOIN analyses a ON a.id = te.analysis_id \
             WHERE a.case_id = ?1 \
             ORDER BY a.started_at, te.analysis_id, te.position, te.id",
        )?;
        let rows = statement.query_map([case_id], |row| row.get::<_, String>(0))?;
        let mut entries = Vec::new();
        for row in rows {
            let payload = row?;
            if let Ok(entry) = serde_json::from_str(&payload) {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    /// Writes one finding and its review disposition.
    ///
    /// The finding is appended, never updated: a review records a disposition
    /// beside the observation rather than changing it (spec §66).
    pub fn insert_finding(
        &self,
        analysis_id: &str,
        finding: &tpt_app_media_forensics_model::Finding,
    ) -> rusqlite::Result<()> {
        let payload = serde_json::to_string(finding)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

        self.connection.execute(
            "INSERT INTO findings (id, analysis_id, asset_id, rule_id, severity, confidence, \
         summary, measurements, timeline_start_micros, timeline_end_micros, payload) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                finding.id.to_string(),
                analysis_id,
                finding.asset_id.to_string(),
                finding.rule_id,
                finding.severity.tag(),
                finding.confidence.tag(),
                finding.observation.summary,
                serde_json::to_string(&finding.observation.measurements)
                    .unwrap_or_else(|_| "[]".to_owned()),
                finding.timeline_start.map_or(0, MediaTime::as_micros),
                finding.timeline_end.map_or(0, MediaTime::as_micros),
                payload,
            ],
        )?;

        // The reviewer disposition lives in its own table (spec §66): a review must
        // never overwrite the observation, so it is recorded as a separate row.
        if finding.status != tpt_app_media_forensics_model::FindingStatus::New {
            // `analysis_id` is part of the primary key of the finding itself, so it
            // identifies *which run* is being reviewed. Omitting it left the column
            // NULL and every reviewed finding was rejected by the database: the
            // review workflow could not have worked on any file.
            self.connection.execute(
                "INSERT INTO finding_reviews (finding_id, analysis_id, status, note) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    finding.id.to_string(),
                    analysis_id,
                    finding.status.tag(),
                    finding.review_note,
                ],
            )?;
        }

        Ok(())
    }

    /// Records a reviewer's verdict against a finding (spec §66).
    ///
    /// Appends a row to `finding_reviews` and leaves the observation untouched, so
    /// the original measurement remains exactly as the engine produced it. A
    /// second review of the same finding appends a second row rather than
    /// replacing the first: the history of who concluded what, and when, is part of
    /// the record.
    ///
    /// # Errors
    ///
    /// Returns an error if the finding does not exist in this analysis, or if the
    /// database rejects the write. Recording a verdict against a finding that was
    /// never stored is rejected rather than creating a review with no observation
    /// beside it.
    pub fn record_review(
        &self,
        analysis_id: &str,
        finding_id: &str,
        status: tpt_app_media_forensics_model::FindingStatus,
        note: Option<&str>,
        reviewed_at: i64,
    ) -> rusqlite::Result<()> {
        // Verified up front so the error names the real problem — a missing
        // observation — rather than surfacing as an opaque constraint failure.
        let exists: Option<String> = self
            .connection
            .query_row(
                "SELECT id FROM findings WHERE analysis_id = ?1 AND id = ?2",
                rusqlite::params![analysis_id, finding_id],
                |r| r.get(0),
            )
            .optional()?;

        if exists.is_none() {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "finding {finding_id} is not recorded in analysis {analysis_id}"
            )));
        }

        self.connection.execute(
            "INSERT INTO finding_reviews (finding_id, analysis_id, status, note, reviewed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![finding_id, analysis_id, status.tag(), note, reviewed_at],
        )?;
        Ok(())
    }

    /// Returns every review of one finding, oldest first.
    ///
    /// Ordered by row id rather than by timestamp: two reviews recorded in the same
    /// second must still come back in the order they were written, and a
    /// same-second tie would otherwise leave the order to the database.
    pub fn reviews_of(&self, finding_id: &str) -> rusqlite::Result<Vec<StoredReview>> {
        let mut stmt = self.connection.prepare(
            "SELECT status, note, reviewed_at FROM finding_reviews \
             WHERE finding_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([finding_id], |row| {
            Ok(StoredReview {
                status: row.get::<_, String>(0)?,
                note: row.get::<_, Option<String>>(1)?,
                reviewed_at: row.get(2)?,
            })
        })?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Records an analyst note against a subject (spec §65).
    ///
    /// `subject_kind` and `subject_id` identify what the note is about, e.g. an
    /// asset or a finding. Both are optional together: a note with neither is a
    /// case-level note, which is a real thing an analyst writes ("the client
    /// disputes the timestamp"), while a note with one but not the other would name
    /// a subject ambiguously. That pairing is rejected rather than stored.
    ///
    /// The body is stored verbatim, including its line endings and internal
    /// formatting. A note is evidence of what the analyst concluded, and
    /// "helpfully" reflowing their prose would alter the record (spec §77).
    ///
    /// # Errors
    ///
    /// Returns [`rusqlite::Error::InvalidParameterName`] if only one of
    /// `subject_kind`/`subject_id` is supplied, and an error if the case does not
    /// exist.
    ///
    /// `created_at` is supplied rather than read from the clock so the store stays
    /// deterministic and a caller can record the true wall-clock time (spec §77):
    /// the store has no notion of "now" and asserting one would make two identical
    /// runs differ. A caller-chosen time is not a fabrication risk here because the
    /// note body is the analyst's, not the engine's observation.
    pub fn add_note(
        &self,
        case_id: &str,
        subject_kind: Option<&str>,
        subject_id: Option<&str>,
        body: &str,
        created_at: i64,
    ) -> rusqlite::Result<i64> {
        match (subject_kind, subject_id) {
            (Some(_), None) | (None, Some(_)) => {
                return Err(rusqlite::Error::InvalidParameterName(
                    "a note must name both the kind and the id of its subject, or neither"
                        .to_owned(),
                ));
            }
            _ => {}
        }

        self.connection.execute(
            "INSERT INTO notes (case_id, subject_kind, subject_id, body, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![case_id, subject_kind, subject_id, body, created_at],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    /// Returns the notes attached to one subject, oldest first.
    ///
    /// Ordering is by row id rather than timestamp so two notes written in the same
    /// second still come back in the order they were written.
    pub fn notes_on(
        &self,
        subject_kind: &str,
        subject_id: &str,
    ) -> rusqlite::Result<Vec<StoredNote>> {
        let mut stmt = self.connection.prepare(
            "SELECT id, subject_kind, subject_id, body, created_at FROM notes \
             WHERE subject_kind = ?1 AND subject_id = ?2 ORDER BY id",
        )?;
        let mut rows = stmt.query(rusqlite::params![subject_kind, subject_id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(StoredNote {
                id: row.get(0)?,
                subject_kind: row.get(1)?,
                subject_id: row.get(2)?,
                body: row.get(3)?,
                created_at: row.get(4)?,
            });
        }
        Ok(out)
    }

    /// Returns every note in a case, oldest first.
    ///
    /// Case-level notes included, so a report or a UI showing "what the analyst
    /// said about this case" does not silently omit the observations that were not
    /// attached to one subject.
    pub fn notes_in_case(&self, case_id: &str) -> rusqlite::Result<Vec<StoredNote>> {
        let mut stmt = self.connection.prepare(
            "SELECT id, subject_kind, subject_id, body, created_at FROM notes \
             WHERE case_id = ?1 ORDER BY id",
        )?;
        let mut rows = stmt.query([case_id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(StoredNote {
                id: row.get(0)?,
                subject_kind: row.get(1)?,
                subject_id: row.get(2)?,
                body: row.get(3)?,
                created_at: row.get(4)?,
            });
        }
        Ok(out)
    }

    /// Reads every finding in a case, most severe first.
    ///
    /// Ordering is `severity, rule_id, id` so the sequence is stable across runs and
    /// matches the order the engine already sorted them into (spec §77).
    pub fn findings_in_case(
        &self,
        case_id: &str,
    ) -> rusqlite::Result<Vec<tpt_app_media_forensics_model::Finding>> {
        let mut stmt = self.connection.prepare(
            "SELECT f.payload FROM findings f \
     JOIN analyses a ON a.id = f.analysis_id \
     WHERE a.case_id = ?1 \
     ORDER BY CASE f.severity \
         WHEN 'CRITICAL' THEN 0 WHEN 'SIGNIFICANT' THEN 1 \
         WHEN 'WARNING' THEN 2 ELSE 3 END, f.rule_id, f.id",
        )?;

        let rows = stmt.query_map([case_id], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            let payload = row?;
            // A payload that will not parse is skipped rather than failing the whole
            // read: one corrupt row must not make a case unreportable.
            if let Ok(finding) = serde_json::from_str(&payload) {
                out.push(finding);
            }
        }
        Ok(out)
    }

    /// Records a retained artefact and returns its identifier (spec §32-§33).
    ///
    /// The complete evidence record is stored as canonical JSON in `payload`
    /// alongside the typed columns, for the same reason findings are: a report
    /// rebuilt from the database must state exactly what was verified, not a
    /// reconstruction from the lossy columns. `sha256` and `blake3` are stored
    /// separately because `search` matches against them, and a search that had to
    /// deserialise every row to find a hash would not be a search.
    ///
    /// Append-only like findings: re-writing an artefact's row would silently
    /// rewrite the record of what was examined.
    ///
    /// # Errors
    ///
    /// Returns an error if the database rejects the write.
    pub fn insert_evidence(
        &self,
        analysis_id: &str,
        evidence: &tpt_app_media_forensics_model::Evidence,
    ) -> rusqlite::Result<()> {
        let payload = serde_json::to_string(evidence)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

        self.connection.execute(
            "INSERT INTO evidence (id, analysis_id, asset_id, kind, provenance, \
             relative_path, caption, size_bytes, sha256, blake3, verified, created_at, payload) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                evidence.id.to_string(),
                analysis_id,
                evidence.asset_id.to_string(),
                evidence.kind.tag(),
                evidence.provenance.tag(),
                evidence.relative_path,
                evidence.caption,
                i64::try_from(evidence.integrity.size_bytes).unwrap_or(i64::MAX),
                evidence.integrity.hashes.sha256(),
                evidence.integrity.hashes.blake3(),
                i64::from(evidence.integrity.verified),
                0_i64,
                payload,
            ],
        )?;

        Ok(())
    }

    /// Reads every evidence artefact recorded against a case.
    ///
    /// Ordered by `id` so a rebuilt report lists the same artefacts in the same
    /// order on every run; evidence has no severity to sort by, and an unstable
    /// order would make two reports of one case differ for no stated reason.
    ///
    /// A payload that will not parse is skipped rather than failing the read, for
    /// the same reason `findings_in_case` does.
    pub fn evidence_in_case(
        &self,
        case_id: &str,
    ) -> rusqlite::Result<Vec<tpt_app_media_forensics_model::Evidence>> {
        let mut stmt = self.connection.prepare(
            "SELECT e.payload FROM evidence e \
             JOIN analyses a ON a.id = e.analysis_id \
             WHERE a.case_id = ?1 \
             ORDER BY e.id",
        )?;

        let rows = stmt.query_map([case_id], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            if let Ok(evidence) = serde_json::from_str(&row?) {
                out.push(evidence);
            }
        }
        Ok(out)
    }

    /// Records a generated report and returns its identifier.
    pub fn insert_report(
        &self,
        case_id: &str,
        analysis_id: Option<&str>,
        format: &str,
        relative_path: &str,
        sha256: &str,
    ) -> rusqlite::Result<String> {
        let id = tpt_app_media_forensics_model::ReportId::new_derived(&[
            case_id.as_bytes(),
            format.as_bytes(),
            relative_path.as_bytes(),
        ]);
        self.connection.execute(
            "INSERT INTO reports (id, case_id, analysis_id, format, relative_path, sha256) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
     ON CONFLICT(id) DO UPDATE SET sha256 = excluded.sha256",
            rusqlite::params![
                id.to_string(),
                case_id,
                analysis_id,
                format,
                relative_path,
                sha256
            ],
        )?;
        Ok(id.to_string())
    }

    /// Returns the most recent analysis in a case, if any.
    ///
    /// Ordered by `started_at`, not by `id`. An analysis id is derived from the
    /// asset content and cache key, so it carries no chronological meaning:
    /// ordering by it returns whichever run happens to hash lowest, which for a
    /// re-analysis of the same file is the *older* run. `id` breaks ties so two
    /// runs started in the same second still come back in a stable order (§77).
    pub fn latest_analysis(&self, case_id: &str) -> rusqlite::Result<Option<StoredAnalysis>> {
        let mut stmt = self.connection.prepare(
    "SELECT id, case_id, asset_id, cache_key, finding_count, rule_count, profile, profile_fingerprint, rule_set_fingerprint, started_at FROM analyses \
     WHERE case_id = ?1 ORDER BY started_at DESC, id DESC LIMIT 1",
)?;
        let mut rows = stmt.query([case_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(StoredAnalysis {
            id: row.get(0)?,
            case_id: row.get(1)?,
            asset_id: row.get(2)?,
            cache_key: row.get(3)?,
            finding_count: row.get(4)?,
            rule_count: row.get(5)?,
            profile: row.get(6)?,
            profile_fingerprint: row.get(7)?,
            rule_set_fingerprint: row.get(8)?,
            started_at: row.get(9)?,
        }))
    }

    /// Returns the case identifier when the database holds exactly one case.
    ///
    /// `None` both when the database is empty and when it holds several. That is
    /// the point of the method: a CLI using it to skip a `--case` argument must be
    /// given nothing when the choice is ambiguous. Returning the first case
    /// regardless would silently pick one, which in a forensic tool is worse than
    /// refusing.
    pub fn only_case_id(&self) -> rusqlite::Result<Option<String>> {
        let mut stmt = self
            .connection
            .prepare("SELECT id FROM cases ORDER BY id LIMIT 2")?;
        let mut rows = stmt.query([])?;

        let Some(first) = rows.next()? else {
            return Ok(None);
        };
        let first: String = first.get(0)?;
        if rows.next()?.is_some() {
            return Ok(None);
        }
        Ok(Some(first))
    }
}
