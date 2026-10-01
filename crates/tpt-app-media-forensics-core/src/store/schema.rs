//! Database schema and migrations (spec §52).
//!
//! # Schema versioning
//!
//! `PRAGMA user_version` carries the schema version. Migrations are applied in
//! order and never rewritten, so a case opened by an older build either opens
//! cleanly or reports that it cannot — it is never silently misread.
//!
//! # Findings and evidence are append-only
//!
//! The review workflow (spec §66) records a reviewer's conclusion *without*
//! changing the original observation. That is only possible if the observation
//! is not overwritten. So findings and evidence are `INSERT` only, and a
//! reviewer's verdict lives in its own table keyed to the finding it refers to.
//! Re-analysing a file produces new findings; it never edits old ones.
//!
//! # Text is stored verbatim
//!
//! Hashes, timestamps, and observed values are stored exactly as observed.
//! Nothing is normalised or reformatted, because the stored value is the
//! evidence: a hex digest a tool "helpfully" upper-cased would no longer match
//! what the file says.

/// Current schema version. Bump when adding a migration.
pub const SCHEMA_VERSION: i64 = 2;

/// Migrations, applied in order. Index + 1 is the version it produces.
const MIGRATIONS: &[&str] = &[BASE_SCHEMA, FINDING_PAYLOADS];

/// Adds a `payload` column to `findings` and `relative_dir` to `reports`.
///
/// The typed columns on `findings` exist so a case can be queried by severity
/// or rule without parsing anything, but they cannot reconstruct a `Finding`:
/// stream scope, evidence references, and reviewer state have no column of
/// their own. Rather than denormalise those into more columns, the complete
/// finding is stored as its canonical JSON, so a report re-renders exactly what
/// the engine produced.
///
/// This stays consistent with findings being append-only: the payload is
/// written once and never updated.
const FINDING_PAYLOADS: &str = r#"
ALTER TABLE findings ADD COLUMN payload TEXT;
ALTER TABLE reports ADD COLUMN relative_dir TEXT;
CREATE INDEX idx_finding_reviews_finding ON finding_reviews (finding_id, analysis_id);
ALTER TABLE analyses ADD COLUMN profile TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE analyses ADD COLUMN profile_fingerprint TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE analyses ADD COLUMN rule_set_fingerprint TEXT NOT NULL DEFAULT 'unknown';
"#;

/// The initial schema.
const BASE_SCHEMA: &str = r#"
CREATE TABLE cases (
    id          TEXT PRIMARY KEY NOT NULL,
    name        TEXT NOT NULL,
    description TEXT,
    created_at  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE assets (
    id            TEXT PRIMARY KEY NOT NULL,
    case_id       TEXT NOT NULL REFERENCES cases(id) ON DELETE CASCADE,
    name          TEXT NOT NULL,
    media_type    TEXT NOT NULL,
    source_path   TEXT NOT NULL,
    size_bytes    INTEGER NOT NULL,
    sha256        TEXT,
    blake3        TEXT,
    modified_secs INTEGER,
    created_secs  INTEGER,
    filesystem    TEXT,
    acquired_at   INTEGER NOT NULL DEFAULT 0,
    UNIQUE (case_id, sha256)
);

CREATE TABLE analyses (
    id               TEXT PRIMARY KEY NOT NULL,
    case_id          TEXT NOT NULL REFERENCES cases(id) ON DELETE CASCADE,
    asset_id         TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    cache_key        TEXT NOT NULL,
    software_version TEXT NOT NULL,
    analysis_version INTEGER NOT NULL,
    status           TEXT NOT NULL,
    finding_count    INTEGER NOT NULL DEFAULT 0,
    rule_count       INTEGER NOT NULL DEFAULT 0,
    started_at       INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE streams (
    asset_id         TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    stream_index     INTEGER NOT NULL,
    kind             TEXT NOT NULL,
    codec            TEXT NOT NULL,
    codec_long       TEXT,
    timebase         TEXT NOT NULL,
    start_micros     INTEGER NOT NULL DEFAULT 0,
    duration_micros  INTEGER,
    frame_count      INTEGER,
    declared_json    TEXT,
    PRIMARY KEY (asset_id, stream_index)
);

CREATE TABLE findings (
    id                     TEXT NOT NULL,
    analysis_id            TEXT NOT NULL REFERENCES analyses(id) ON DELETE CASCADE,
    asset_id               TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    rule_id                TEXT NOT NULL,
    severity               TEXT NOT NULL,
    confidence             TEXT NOT NULL,
    summary                TEXT NOT NULL,
    measurements           TEXT NOT NULL DEFAULT '[]',
    timeline_start_micros  INTEGER,
    timeline_end_micros    INTEGER,
    created_at             INTEGER NOT NULL DEFAULT 0,
    -- A finding identifier is derived from its content, so the same rule
    -- firing on the same file across two runs yields the same id. The primary
    -- key is therefore (analysis_id, id): re-analysing records a new
    -- observation rather than colliding with the previous run.
    PRIMARY KEY (analysis_id, id)
);

CREATE TABLE finding_reviews (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    finding_id  TEXT NOT NULL,
    analysis_id TEXT NOT NULL,
    status      TEXT NOT NULL,
    note        TEXT,
    reviewed_at INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE evidence (
    id            TEXT PRIMARY KEY NOT NULL,
    analysis_id   TEXT NOT NULL REFERENCES analyses(id) ON DELETE CASCADE,
    asset_id      TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    kind          TEXT NOT NULL,
    provenance    TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    caption       TEXT,
    size_bytes    INTEGER NOT NULL,
    sha256        TEXT,
    blake3        TEXT,
    verified      INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE rule_results (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    analysis_id TEXT NOT NULL REFERENCES analyses(id) ON DELETE CASCADE,
    rule_id     TEXT NOT NULL,
    ran_at      INTEGER NOT NULL DEFAULT 0,
    UNIQUE (analysis_id, rule_id)
);

CREATE TABLE reports (
    id            TEXT PRIMARY KEY NOT NULL,
    case_id       TEXT NOT NULL REFERENCES cases(id) ON DELETE CASCADE,
    analysis_id   TEXT REFERENCES analyses(id) ON DELETE SET NULL,
    format        TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    sha256        TEXT,
    created_at    INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE notes (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    case_id      TEXT NOT NULL REFERENCES cases(id) ON DELETE CASCADE,
    subject_kind TEXT,
    subject_id   TEXT,
    body         TEXT NOT NULL,
    created_at   INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_assets_case ON assets(case_id);
CREATE INDEX idx_analyses_asset ON analyses(asset_id);
CREATE INDEX idx_findings_analysis ON findings(analysis_id);
CREATE INDEX idx_findings_asset ON findings(asset_id);
CREATE INDEX idx_reviews_finding ON finding_reviews(finding_id);
CREATE INDEX idx_evidence_analysis ON evidence(analysis_id);
CREATE INDEX idx_reports_case ON reports(case_id);
CREATE INDEX idx_notes_case ON notes(case_id);
"#;

/// Applies any migrations the database has not yet seen.
///
/// # Errors
///
/// Returns an error if a migration fails, or if the database was written by a
/// newer build than this one understands.
pub fn apply_migrations(connection: &rusqlite::Connection) -> rusqlite::Result<()> {
    let current: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let target = i64::try_from(MIGRATIONS.len()).unwrap_or(i64::MAX);

    if current > target {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "database schema version {current} is newer than this build understands ({target})"
        )));
    }

    for (index, sql) in MIGRATIONS.iter().enumerate() {
        let version = i64::try_from(index + 1).unwrap_or(i64::MAX);
        if version <= current {
            continue;
        }
        connection.execute_batch(sql)?;
        connection.pragma_update(None, "user_version", version)?;
    }
    Ok(())
}
