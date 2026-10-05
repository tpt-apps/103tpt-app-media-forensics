//! Search over recorded case data (spec §41).
//!
//! # Search is a filter, not an interpreter
//!
//! Every term is compiled to a bound SQL parameter. A reviewer typing a hash or
//! a file name must not be able to reach the query language by typing a quote —
//! not because an injection here would be catastrophic, but because a search box
//! that can fail in surprising ways is a search box people stop trusting. There
//! is no expression syntax to learn and no way to inject one.
//!
//! # An empty query is not an empty result
//!
//! [`SearchQuery::text`] is trimmed before use, and a term that is empty after
//! trimming searches for nothing rather than returning every row. Returning
//! everything because someone pressed space would make "no matches" mean two
//! different things depending on an invisible character. Callers wanting a
//! listing use [`SearchQuery::all`].
//!
//! # Truncation is reported, never silent
//!
//! [`SearchResult::truncated`] says whether more rows matched than were
//! returned. A search that quietly showed the first 200 of 5,000 hits would be
//! read as "there are 200", which is a false statement about the case.

use rusqlite::types::ToSqlOutput;
use rusqlite::{Connection, ToSql};

use tpt_app_media_forensics_model::Severity;

/// What a search looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchScope {
    /// Findings: rule identifier, summary, and measurements.
    Findings,
    /// Assets: display name, source path, and hashes.
    Assets,
    /// Evidence: kind, caption, and relative path.
    Evidence,
    /// Every scope at once.
    All,
}

impl SearchScope {
    /// Every scope, for a caller iterating rather than selecting one.
    pub const ALL: [Self; 4] = [Self::All, Self::Findings, Self::Assets, Self::Evidence];

    /// Whether this scope includes `other`.
    ///
    /// [`SearchScope::All`] includes everything; each concrete scope includes only
    /// itself. Written as a method rather than a `matches!` at each call site so a
    /// new scope cannot be silently excluded from a "search everything" query.
    #[must_use]
    pub fn includes(self, other: Self) -> bool {
        matches!(self, Self::All) || self == other
    }
}

/// A minimum severity for findings.
///
/// Named variants rather than a bare `Option<Severity>` because "no filter" and
/// "everything at Info or above" are different intents that would otherwise share
/// a representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeverityFilter {
    /// No severity constraint.
    Any,
    /// Findings at this severity or above.
    AtLeast(Severity),
}

impl SeverityFilter {
    /// Returns the minimum rank to accept, or `None` for no constraint.
    fn rank(self) -> Option<u8> {
        match self {
            Self::Any => None,
            Self::AtLeast(s) => Some(Self::rank_of(s)),
        }
    }

    fn rank_of(severity: Severity) -> u8 {
        match severity {
            Severity::Critical => 0,
            Severity::Significant => 1,
            Severity::Warning => 2,
            Severity::Info => 3,
        }
    }
}

/// A search over one case.
#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    /// Free text to match, or empty for a structured-only search.
    pub text: String,
    /// Which data to search. `None` means [`SearchScope::All`].
    pub scope: Option<SearchScope>,
    /// Minimum finding severity.
    pub severity: Option<SeverityFilter>,
    /// Restrict to one asset.
    pub asset_id: Option<String>,
    /// Maximum rows to return.
    ///
    /// Defaults to [`SearchQuery::DEFAULT_LIMIT`]. A search with no limit over a
    /// large case would pull every row into memory, which is a denial of service
    /// reachable by typing in a search box.
    pub limit: Option<usize>,
}

impl SearchQuery {
    /// Rows returned when no limit is given.
    pub const DEFAULT_LIMIT: usize = 200;

    /// A listing query with no text and no filters.
    #[must_use]
    pub fn all() -> Self {
        Self::default()
    }

    /// A query matching `term`.
    #[must_use]
    pub fn text(term: impl Into<String>) -> Self {
        Self {
            text: term.into(),
            ..Self::default()
        }
    }

    /// Returns the search term with surrounding whitespace removed.
    ///
    /// Separate from the stored `text` so a caller can show exactly what was
    /// searched for rather than re-deriving it.
    #[must_use]
    pub fn trimmed(&self) -> &str {
        self.text.trim()
    }

    /// Returns the effective row limit.
    #[must_use]
    pub fn effective_limit(&self) -> usize {
        self.limit.unwrap_or(Self::DEFAULT_LIMIT)
    }

    /// Whether a search term was supplied at all.
    ///
    /// Distinct from [`Self::trimmed`] being empty. A query built with
    /// [`SearchQuery::all`] has no term and lists everything; one built with
    /// `SearchQuery::text("   ")` has a term that matches nothing. Collapsing the
    /// two would make a whitespace-only search silently return the entire case.
    #[must_use]
    pub fn has_term(&self) -> bool {
        !self.text.is_empty()
    }

    /// Returns the scope to search, defaulting to [`SearchScope::All`].
    #[must_use]
    pub fn effective_scope(&self) -> SearchScope {
        self.scope.unwrap_or(SearchScope::All)
    }
}

/// One row a search matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// Which kind of record matched.
    pub scope: SearchScope,
    /// Primary identifier of the record.
    pub id: String,
    /// The asset the record belongs to.
    pub asset_id: Option<String>,
    /// A human-readable description of what matched.
    pub label: String,
    /// The record's severity, for findings.
    pub severity: Option<Severity>,
}

/// The outcome of a search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    /// Matching rows, most severe first then by id.
    pub hits: Vec<SearchHit>,
    /// Whether more rows matched than `limit` allowed.
    ///
    /// Never silently implied by a short list: see the module docs.
    pub truncated: bool,
    /// Total rows that matched, ignoring the limit.
    pub total: usize,
}

impl SearchResult {
    /// Whether the search found nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hits.is_empty()
    }

    /// Returns the hits for one scope.
    #[must_use]
    pub fn in_scope(&self, scope: SearchScope) -> Vec<&SearchHit> {
        self.hits.iter().filter(|h| h.scope == scope).collect()
    }
}

/// Escapes a LIKE pattern's wildcards.
///
/// The search term is bound as a parameter, so it cannot break out of the query.
/// It can still change *what the query matches*: a user typing `%` would
/// otherwise match every row. Escaping makes the term literal, which is what
/// someone searching for a hash expects.
fn escape_like(term: &str) -> String {
    let mut out = String::with_capacity(term.len());
    for c in term.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Parses a severity string as stored in the database.
fn severity_of(tag: &str) -> Severity {
    match tag {
        "CRITICAL" => Severity::Critical,
        "SIGNIFICANT" => Severity::Significant,
        "WARNING" => Severity::Warning,
        _ => Severity::Info,
    }
}

/// One arm of the search.
///
/// Every arm returns the same column shape — `(id, asset_id, label, severity_tag,
/// rank, arm)` — so the arms can be `UNION ALL`ed and sorted once in SQL rather
/// than merged in Rust. `arm` distinguishes scopes in the output; the scope is
/// recovered from it rather than from a column, to keep every arm's SELECT list
/// identical.
struct Arm {
    sql: String,
    params: Vec<ArmParam>,
}

/// A bound value in an arm.
///
/// Typed rather than a bare `String` because SQLite compares values by their
/// storage class: a rank bound as text is compared against an integer expression
/// as text, so `rank >= '0'` fails for every row. That failure is silent — the
/// search returns nothing rather than erroring — so the type is enforced here
/// instead of trusted at each call site.
#[derive(Debug, Clone)]
enum ArmParam {
    Text(String),
    Int(i64),
}

impl ToSql for ArmParam {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        match self {
            Self::Text(t) => t.to_sql(),
            Self::Int(i) => i.to_sql(),
        }
    }
}

impl Arm {
    fn new(sql: String, params: Vec<ArmParam>) -> Self {
        Self { sql, params }
    }
}

/// What a search's term compiles to.
///
/// Three states rather than `Option<String>`, because "no term" and "a term that
/// matches nothing" must not collapse into one.
enum Term {
    /// A real pattern to match.
    Matchable(String),
    /// A term was given that cannot match anything.
    Impossible,
}

impl Term {
    /// Appends this term's SQL fragment for `columns`, or nothing when listing.
    ///
    /// Takes the columns to match so the same term compiles correctly against
    /// findings, assets, or evidence rather than each arm hand-rolling it.
    fn apply(&self, sql: &mut String, columns: &[&str]) {
        match self {
            // The pattern itself is not interpolated: `apply` only emits
            // placeholders, and `params` supplies the bound values. Keeping the two
            // apart is what guarantees a term can never reach the SQL as text.
            Self::Matchable(_) => {
                let conditions = columns
                    .iter()
                    .map(|c| format!("{c} LIKE ? ESCAPE '\\'"))
                    .collect::<Vec<_>>()
                    .join(" OR ");
                sql.push_str(" AND (");
                sql.push_str(&conditions);
                sql.push(')');
            }
            // `0` is false for every row; `?` is avoided entirely so no bound
            // parameter is expected and the positional numbering stays intact.
            Self::Impossible => sql.push_str(" AND 0"),
        }
    }

    /// Returns the parameters this term contributes, one per matched column.
    fn params(&self, columns: usize) -> Vec<ArmParam> {
        match self {
            Self::Matchable(pattern) => (0..columns)
                .map(|_| ArmParam::Text(pattern.clone()))
                .collect(),
            Self::Impossible => Vec::new(),
        }
    }
}

/// Builds the arms a query needs, one per selected scope.
///
/// Placeholders are positional (`?`), so the parameters for an arm are collected
/// in the same order they appear in its SQL. The case identifier is the first
/// parameter of every arm, which keeps the arms self-contained.
fn arms(query: &SearchQuery) -> Vec<Arm> {
    let term = query.trimmed();
    let scope = query.effective_scope();
    let min_rank = query.severity.unwrap_or(SeverityFilter::Any).rank();

    // A term that is present but blank after trimming must match nothing. Treating
    // it as "no term" would return the whole case, so "no matches" would mean two
    // different things depending on an invisible space.
    //
    // An unsatisfiable predicate is used rather than an early return, because every
    // scope arm needs the same treatment and one arm disagreeing would be the bug.
    // It is emitted as SQL, not as a pattern: SQLite's LIKE ignores a NUL byte in a
    // bound string, so a pattern built from one still matches everything.
    let pattern = if term.is_empty() {
        // `all()` is a deliberate listing; `text("   ")` is a search that found
        // nothing. The difference is whether a term was supplied at all, not whether
        // it had content.
        if query.has_term() {
            Some(Term::Impossible)
        } else {
            None
        }
    } else {
        Some(Term::Matchable(format!("%{}%", escape_like(term))))
    };
    let mut out = Vec::new();

    if scope.includes(SearchScope::Findings) {
        let mut sql = String::from(
            "SELECT f.id, f.asset_id, f.summary, f.severity, \
             CASE f.severity WHEN 'CRITICAL' THEN 0 WHEN 'SIGNIFICANT' THEN 1 \
             WHEN 'WARNING' THEN 2 ELSE 3 END, 0 \
             FROM findings f JOIN analyses a ON a.id = f.analysis_id \
             WHERE a.case_id = ?",
        );
        let mut params = vec![ArmParam::Text(String::new())]; // case_id, filled by the caller
        if let Some(asset) = &query.asset_id {
            sql.push_str(" AND f.asset_id = ?");
            params.push(ArmParam::Text(asset.clone()));
        }
        if let Some(rank) = min_rank {
            // Rank is 0 for the most severe and 3 for the least, so "at least this
            // severe" is `rank <= given`. The comparison is numeric, not textual:
            // alphabetically 'CRITICAL' sorts before 'INFO', so comparing the tags
            // as strings would invert the order and return the wrong findings.
            sql.push_str(
                " AND CASE f.severity WHEN 'CRITICAL' THEN 0 WHEN 'SIGNIFICANT' THEN 1 \
                 WHEN 'WARNING' THEN 2 ELSE 3 END <= ?",
            );
            params.push(ArmParam::Int(i64::from(rank)));
        }
        if let Some(term) = &pattern {
            let columns = ["f.rule_id", "f.summary", "f.measurements"];
            term.apply(&mut sql, &columns);
            params.extend(term.params(columns.len()));
        }
        out.push(Arm::new(sql, params));
    }

    if scope.includes(SearchScope::Assets) {
        let mut sql =
            String::from("SELECT s.id, s.id, s.name, NULL, 3, 1 FROM assets s WHERE s.case_id = ?");
        let mut params = vec![ArmParam::Text(String::new())];
        if let Some(asset) = &query.asset_id {
            sql.push_str(" AND s.id = ?");
            params.push(ArmParam::Text(asset.clone()));
        }
        if let Some(term) = &pattern {
            // A severity filter constrains findings only. Assets carry no severity,
            // so applying it to them would hide every asset a reviewer asked to see
            // alongside its findings.
            let columns = [
                "s.name",
                "s.source_path",
                "IFNULL(s.sha256, '')",
                "IFNULL(s.blake3, '')",
            ];
            term.apply(&mut sql, &columns);
            params.extend(term.params(columns.len()));
        }
        out.push(Arm::new(sql, params));
    }

    if scope.includes(SearchScope::Evidence) {
        let mut sql = String::from(
            "SELECT e.id, e.asset_id, IFNULL(e.caption, e.kind), NULL, 3, 2 FROM evidence e \
             JOIN analyses a ON a.id = e.analysis_id WHERE a.case_id = ?",
        );
        let mut params = vec![ArmParam::Text(String::new())];
        if let Some(asset) = &query.asset_id {
            sql.push_str(" AND e.asset_id = ?");
            params.push(ArmParam::Text(asset.clone()));
        }
        if let Some(term) = &pattern {
            let columns = ["e.kind", "IFNULL(e.caption, '')", "e.relative_path"];
            term.apply(&mut sql, &columns);
            params.extend(term.params(columns.len()));
        }
        out.push(Arm::new(sql, params));
    }

    out
}

/// Maps an arm index back to the scope it searched.
fn scope_of_arm(index: i64) -> SearchScope {
    match index {
        0 => SearchScope::Findings,
        1 => SearchScope::Assets,
        _ => SearchScope::Evidence,
    }
}

/// Runs a search over one case.
///
/// # Errors
///
/// Returns a `rusqlite::Error` if the query fails. Search is read-only, so the
/// only failures are a corrupt or unavailable database.
///
/// # Panics
///
/// Never on user input: every term is a bound parameter and every scope is chosen
/// from a closed enum.
pub fn search(
    connection: &Connection,
    case_id: &str,
    query: &SearchQuery,
) -> rusqlite::Result<SearchResult> {
    let built = arms(query);
    if built.is_empty() {
        // Unreachable through the enum, but an empty `UNION ALL` is a SQL error
        // rather than an empty result, and returning "nothing matched" is the
        // honest answer if it ever happens.
        return Ok(SearchResult {
            hits: Vec::new(),
            truncated: false,
            total: 0,
        });
    }

    // Fetch one more than the limit so truncation is detected from the data
    // rather than assumed. `total` comes from a second, cheaper query so a
    // truncated page still says how much is actually there.
    let limit = query.effective_limit();
    let union = built
        .iter()
        .map(|a| a.sql.as_str())
        .collect::<Vec<_>>()
        .join(" UNION ALL ");

    let body = format!("{union} ORDER BY 5, 1, 6 LIMIT ?");

    let mut bound: Vec<ArmParam> = Vec::new();
    for arm in &built {
        for (i, param) in arm.params.iter().enumerate() {
            // The first parameter of every arm is the case identifier.
            bound.push(match (i, param) {
                (0, _) => ArmParam::Text(case_id.to_owned()),
                (_, p) => p.clone(),
            });
        }
    }
    bound.push(ArmParam::Int(limit as i64 + 1));

    let mut stmt = connection.prepare(&body)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(bound.iter()))?;
    let mut hits = Vec::new();
    while let Some(row) = rows.next()? {
        let arm: i64 = row.get(5)?;
        let severity: Option<String> = row.get(3)?;
        hits.push(SearchHit {
            scope: scope_of_arm(arm),
            id: row.get(0)?,
            asset_id: row.get(1)?,
            label: row.get(2)?,
            severity: severity.as_deref().map(severity_of),
        });
    }

    let truncated = hits.len() > limit;
    hits.truncate(limit);

    Ok(SearchResult {
        hits,
        truncated,
        total: count_matching(connection, case_id, query)?,
    })
}

/// Counts the rows a query matches, ignoring its limit.
///
/// A second query rather than `hits.len()`, because a truncated page that reported
/// its own length as the total would tell a reviewer there were 200 findings when
/// the case holds thousands.
fn count_matching(
    connection: &Connection,
    case_id: &str,
    query: &SearchQuery,
) -> rusqlite::Result<usize> {
    let built = arms(query);
    if built.is_empty() {
        return Ok(0);
    }
    let union = built
        .iter()
        .map(|a| a.sql.as_str())
        .collect::<Vec<_>>()
        .join(" UNION ALL ");

    let mut bound: Vec<ArmParam> = Vec::new();
    for arm in &built {
        for (i, param) in arm.params.iter().enumerate() {
            bound.push(match (i, param) {
                (0, _) => ArmParam::Text(case_id.to_owned()),
                (_, p) => p.clone(),
            });
        }
    }

    let mut stmt = connection.prepare(&format!("SELECT COUNT(*) FROM ({union})"))?;
    let count: i64 = stmt.query_row(rusqlite::params_from_iter(bound.iter()), |row| row.get(0))?;
    Ok(count.max(0) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Store, StoredAnalysis, StoredAsset};

    const CASE: &str = "case-1";

    fn seeded() -> Store {
        let store = Store::open_in_memory().expect("in-memory store");
        store
            .upsert_case(CASE, "Test Case", None)
            .expect("case exists");

        store
            .insert_asset(&StoredAsset {
                id: "asset-a".to_owned(),
                case_id: CASE.to_owned(),
                name: "promo-master.mp4".to_owned(),
                source_path: "/delivery/promo-master.mp4".to_owned(),
                size_bytes: 1024,
                sha256: Some("a".repeat(64)),
                blake3: None,
                role: None,
            })
            .expect("asset inserted");

        store
            .insert_analysis(&StoredAnalysis {
                id: "analysis-1".to_owned(),
                case_id: CASE.to_owned(),
                asset_id: "asset-a".to_owned(),
                cache_key: "key-1".to_owned(),
                finding_count: 2,
                rule_count: 10,
                profile: "default".to_owned(),
                profile_fingerprint: "pf".to_owned(),
                rule_set_fingerprint: "rs".to_owned(),
                started_at: 1_700_000_000,
            })
            .expect("analysis inserted");

        for (rule, severity, summary) in [
            ("VIDEO.NO_VIDEO", "CRITICAL", "track declares no video"),
            (
                "AUDIO.LOUDNESS",
                "WARNING",
                "loudness below delivery target",
            ),
        ] {
            store
                .connection()
                .execute(
                    "INSERT INTO findings (id, analysis_id, asset_id, rule_id, severity, \
                     confidence, summary, measurements) \
                     VALUES (?1, 'analysis-1', 'asset-a', ?2, ?3, 'HIGH', ?4, '[]')",
                    rusqlite::params![format!("f-{rule}"), rule, severity, summary],
                )
                .expect("finding inserted");
        }

        store
    }

    #[test]
    fn a_term_matches_a_finding_summary() {
        let store = seeded();
        let result = search(
            store.connection(),
            CASE,
            &SearchQuery::text("delivery target"),
        )
        .expect("search runs");

        assert_eq!(result.total, 1, "{:?}", result.hits);
        assert_eq!(result.hits[0].scope, SearchScope::Findings);
        assert_eq!(result.hits[0].severity, Some(Severity::Warning));
    }

    #[test]
    fn a_term_matches_a_rule_identifier() {
        let store = seeded();
        let result = search(
            store.connection(),
            CASE,
            &SearchQuery::text("VIDEO.NO_VIDEO"),
        )
        .expect("search runs");

        assert_eq!(result.total, 1);
        assert_eq!(result.hits[0].severity, Some(Severity::Critical));
    }

    #[test]
    fn a_hash_matches_an_asset() {
        let store = seeded();
        let term = "a".repeat(64);
        let result =
            search(store.connection(), CASE, &SearchQuery::text(term)).expect("search runs");

        assert_eq!(result.total, 1, "{:?}", result.hits);
        assert_eq!(result.hits[0].scope, SearchScope::Assets);
        assert_eq!(result.hits[0].id, "asset-a");
        assert_eq!(result.hits[0].severity, None, "assets carry no severity");
    }

    #[test]
    fn findings_are_returned_most_severe_first() {
        let store = seeded();
        let result = search(store.connection(), CASE, &SearchQuery::all()).expect("search runs");

        let findings: Vec<&SearchHit> = result.in_scope(SearchScope::Findings);
        assert_eq!(findings.len(), 2);
        assert_eq!(
            findings[0].severity,
            Some(Severity::Critical),
            "CRITICAL must sort above WARNING, not below it alphabetically"
        );
    }
    #[test]
    fn a_severity_filter_excludes_anything_below_it() {
        let store = seeded();
        let mut query = SearchQuery::all();
        query.scope = Some(SearchScope::Findings);
        query.severity = Some(SeverityFilter::AtLeast(Severity::Critical));

        let result = search(store.connection(), CASE, &query).expect("search runs");
        assert_eq!(result.total, 1, "only the CRITICAL finding qualifies");
        assert_eq!(result.hits[0].severity, Some(Severity::Critical));
    }

    #[test]
    fn a_severity_filter_does_not_hide_assets() {
        // Assets carry no severity. Applying a findings-only filter to them would
        // silently hide every asset a reviewer asked to see alongside its findings.
        let store = seeded();
        let mut query = SearchQuery::all();
        query.severity = Some(SeverityFilter::AtLeast(Severity::Critical));

        let result = search(store.connection(), CASE, &query).expect("search runs");
        assert!(
            !result.in_scope(SearchScope::Assets).is_empty(),
            "assets must survive a severity filter"
        );
    }

    #[test]
    fn a_whitespace_only_term_matches_nothing() {
        // Returning everything because someone pressed space would make "no
        // matches" mean two different things depending on an invisible character.
        let store = seeded();
        let result =
            search(store.connection(), CASE, &SearchQuery::text("   ")).expect("search runs");

        assert!(result.is_empty(), "{:?}", result.hits);
        assert_eq!(result.total, 0);
    }

    #[test]
    fn a_like_wildcard_in_the_term_is_literal() {
        // The term is bound, so it cannot inject — but an unescaped `%` would still
        // match every row, which is not what someone searching for a hash expects.
        let store = seeded();
        let result =
            search(store.connection(), CASE, &SearchQuery::text("%")).expect("search runs");

        assert!(
            result.is_empty(),
            "a bare percent must match nothing, got {:?}",
            result.hits
        );
    }

    #[test]
    fn a_quote_in_the_term_does_not_break_the_query() {
        let store = seeded();
        let result = search(
            store.connection(),
            CASE,
            &SearchQuery::text("'; DROP TABLE findings; --"),
        )
        .expect("search runs");

        assert!(result.is_empty(), "{:?}", result.hits);
        let remaining = store.count("findings").expect("counted");
        assert_eq!(remaining, 2, "the findings table must survive");
    }

    #[test]
    fn truncation_is_reported_rather_than_implied_by_a_short_list() {
        let store = seeded();
        let mut query = SearchQuery::all();
        query.limit = Some(1);

        let result = search(store.connection(), CASE, &query).expect("search runs");

        assert_eq!(result.hits.len(), 1, "the limit is honoured");
        assert!(result.truncated, "more rows matched than were returned");
        assert_eq!(
            result.total, 3,
            "the total must report every match, not the page size"
        );
    }

    #[test]
    fn an_untruncated_result_is_not_marked_truncated() {
        let store = seeded();
        let result = search(store.connection(), CASE, &SearchQuery::all()).expect("search runs");

        assert_eq!(result.hits.len(), result.total);
        assert!(!result.truncated);
    }
    #[test]
    fn a_scope_restriction_excludes_the_other_scopes() {
        let store = seeded();
        let mut query = SearchQuery::all();
        query.scope = Some(SearchScope::Findings);

        let result = search(store.connection(), CASE, &query).expect("search runs");
        assert!(
            result.in_scope(SearchScope::Assets).is_empty(),
            "assets must be excluded when findings alone were asked for"
        );
        assert_eq!(result.total, 2, "only the two findings");
    }

    #[test]
    fn an_asset_filter_restricts_to_that_asset() {
        let store = seeded();
        let mut query = SearchQuery::all();
        query.asset_id = Some("asset-a".to_owned());

        let result = search(store.connection(), CASE, &query).expect("search runs");
        assert!(
            result
                .hits
                .iter()
                .all(|h| h.asset_id.as_deref() == Some("asset-a")),
            "{:?}",
            result.hits
        );
    }

    #[test]
    fn searching_a_case_with_no_data_returns_nothing_rather_than_everything() {
        let store = Store::open_in_memory().expect("in-memory store");
        store
            .upsert_case("empty", "Empty", None)
            .expect("case exists");

        let result = search(store.connection(), "empty", &SearchQuery::all()).expect("search runs");

        assert!(result.is_empty());
        assert_eq!(result.total, 0);
    }

    #[test]
    fn one_case_never_sees_another_cases_rows() {
        let store = seeded();
        store
            .upsert_case("case-2", "Other", None)
            .expect("second case exists");

        let result =
            search(store.connection(), "case-2", &SearchQuery::all()).expect("search runs");
        assert!(
            result.is_empty(),
            "a search must not leak across cases: {:?}",
            result.hits
        );
    }

    #[test]
    fn search_results_are_reproducible() {
        // Spec §77: the same case and query must produce the same rows in the same
        // order, or two runs of a search disagree.
        let store = seeded();
        let first = search(store.connection(), CASE, &SearchQuery::all()).expect("search runs");
        let second = search(store.connection(), CASE, &SearchQuery::all()).expect("search runs");

        assert_eq!(first, second);
    }

    #[test]
    fn scope_includes_covers_every_selection() {
        assert!(SearchScope::All.includes(SearchScope::Findings));
        assert!(SearchScope::All.includes(SearchScope::Assets));
        assert!(SearchScope::All.includes(SearchScope::Evidence));
        assert!(SearchScope::Findings.includes(SearchScope::Findings));
        assert!(!SearchScope::Findings.includes(SearchScope::Assets));
    }
}
