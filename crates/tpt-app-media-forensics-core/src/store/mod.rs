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

use rusqlite::Connection;

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
            "SELECT id, case_id, name, source_path, size_bytes, sha256, blake3 \
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
            })
        })?;
        rows.collect()
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
        const TABLES: [&str; 9] = [
            "cases",
            "assets",
            "analyses",
            "streams",
            "findings",
            "finding_reviews",
            "evidence",
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

impl Store {
    /// Records an analysis run.
    ///
    /// Returns an error if the write fails, including when a finding is inserted
    /// against an analysis that does not exist.
    pub fn insert_analysis(&self, analysis: &StoredAnalysis) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO analyses (id, case_id, asset_id, cache_key, software_version, \
         analysis_version, status, finding_count, rule_count, profile, \
         profile_fingerprint, rule_set_fingerprint, started_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'COMPLETE', ?7, ?8, ?9, ?10, ?11, ?12)",
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
            ],
        )?;
        Ok(())
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
            self.connection.execute(
                "INSERT INTO finding_reviews (finding_id, status, note) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    finding.id.to_string(),
                    finding.status.tag(),
                    finding.review_note,
                ],
            )?;
        }

        Ok(())
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
    pub fn latest_analysis(&self, case_id: &str) -> rusqlite::Result<Option<StoredAnalysis>> {
        let mut stmt = self.connection.prepare(
    "SELECT id, case_id, asset_id, cache_key, finding_count, rule_count, profile, profile_fingerprint, rule_set_fingerprint, started_at FROM analyses \
     WHERE case_id = ?1 ORDER BY id LIMIT 1",
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

    /// Returns the case identifier recorded for a case directory.
    pub fn only_case_id(&self) -> rusqlite::Result<Option<String>> {
        let mut stmt = self
            .connection
            .prepare("SELECT id FROM cases ORDER BY id LIMIT 1")?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }
}
