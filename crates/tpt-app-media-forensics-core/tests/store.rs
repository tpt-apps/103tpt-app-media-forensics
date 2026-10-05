//! Integration tests for case persistence.
//!
//! Exercises the store against a real database file in a case directory, so
//! the layout, pragmas, and round-trip behaviour are covered together rather
//! than in isolation.

use tpt_app_media_forensics_core::case_dir::CaseDirectory;
use tpt_app_media_forensics_core::store::{Store, StoredAnalysis, StoredAsset, TimelineRetention};
use tpt_app_media_forensics_model::{
    AssetId, Case, Confidence, Finding, FindingId, FindingStatus, Observation, Severity,
};

fn asset(id: &str, sha: &str) -> StoredAsset {
    StoredAsset {
        id: id.to_owned(),
        case_id: "case-1".to_owned(),
        name: format!("{id}.mp4"),
        source_path: format!("C:\\evidence\\{id}.mp4"),
        size_bytes: 4096,
        sha256: Some(sha.to_owned()),
        blake3: Some("bb".repeat(32)),
    }
}

#[test]
fn a_case_database_is_created_inside_the_case_directory() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Store::open(dir.path()).expect("opens");

    assert!(
        dir.path().join("case.db").exists(),
        "case.db must live in the case"
    );
    assert!(store.schema_version().expect("reads") >= 1);
}

#[test]
fn a_case_and_its_assets_round_trip() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Store::open(dir.path()).expect("opens");

    store
        .upsert_case("case-1", "Operation Alpha", Some("a description"))
        .expect("inserts case");
    store
        .insert_asset(&asset("a1", "aa"))
        .expect("inserts asset");

    let assets = store.assets_in_case("case-1").expect("reads");
    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0].id, "a1");
    assert_eq!(assets[0].size_bytes, 4096);
    assert_eq!(assets[0].sha256.as_deref(), Some("aa"));
}

#[test]
fn hashes_are_stored_verbatim_without_normalisation() {
    // The stored value is the evidence; reformatting it would break
    // comparison against what the file actually reports.
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Store::open(dir.path()).expect("opens");

    store.upsert_case("case-1", "A", None).expect("case");
    let mut mixed = asset("a1", "DeAdBeEf");
    mixed.sha256 = Some("D3ADB33F".to_owned());
    store.insert_asset(&mixed).expect("asset");

    let back = store.assets_in_case("case-1").expect("reads");
    assert_eq!(
        back[0].sha256.as_deref(),
        Some("D3ADB33F"),
        "the case must not alter the casing of an observed digest"
    );
}

#[test]
fn results_are_ordered_deterministically() {
    // spec §77: the same input must produce the same output ordering.
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Store::open(dir.path()).expect("opens");
    store.upsert_case("case-1", "A", None).expect("case");

    for id in ["a3", "a1", "a2"] {
        store
            .insert_asset(&asset(id, &format!("sha-{id}")))
            .expect("asset");
    }

    let ids: Vec<String> = store
        .assets_in_case("case-1")
        .expect("reads")
        .into_iter()
        .map(|a| a.id)
        .collect();
    assert_eq!(ids, vec!["a1", "a2", "a3"]);
}

#[test]
fn reopening_a_case_preserves_the_record() {
    let dir = tempfile::tempdir().expect("temp dir");
    {
        let store = Store::open(dir.path()).expect("opens");
        store
            .upsert_case("case-1", "Operation Alpha", None)
            .expect("case");
        store.insert_asset(&asset("a1", "aa")).expect("asset");
    }
    let store = Store::open(dir.path()).expect("reopens");
    assert_eq!(store.assets_in_case("case-1").expect("reads").len(), 1);
}

#[test]
fn the_same_content_cannot_be_imported_twice_into_one_case() {
    // Content-derived IDs make duplicate imports detectable; the database
    // enforces it independently of that.
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Store::open(dir.path()).expect("opens");
    store.upsert_case("case-1", "A", None).expect("case");

    store.insert_asset(&asset("a1", "same")).expect("first");
    assert!(
        store.insert_asset(&asset("a2", "same")).is_err(),
        "a duplicate import must be rejected"
    );
    assert_eq!(store.count("assets").expect("counts"), 1);
}

#[test]
fn the_same_content_may_appear_in_two_different_cases() {
    // Comparing one file across cases is legitimate; that is not a duplicate.
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Store::open(dir.path()).expect("opens");

    store.upsert_case("case-1", "A", None).expect("case");
    store.upsert_case("case-2", "B", None).expect("case");
    store.insert_asset(&asset("a1", "same")).expect("first");

    let mut second = asset("b1", "same");
    second.case_id = "case-2".to_owned();
    store
        .insert_asset(&second)
        .expect("second case may hold the same file");

    assert_eq!(store.count("assets").expect("counts"), 2);
}

#[test]
fn deleting_a_case_cascades_to_its_assets() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Store::open(dir.path()).expect("opens");
    store.upsert_case("case-1", "A", None).expect("case");
    store.insert_asset(&asset("a1", "aa")).expect("asset");

    store
        .connection()
        .execute("DELETE FROM cases WHERE id = 'case-1'", [])
        .expect("deletes");
    assert_eq!(store.count("assets").expect("counts"), 0);
}

#[test]
fn an_unknown_table_name_is_rejected() {
    // `count` interpolates a table name; it must never be attacker-controlled.
    let store = Store::open_in_memory().expect("opens");
    assert!(store.count("assets; DROP TABLE cases").is_err());
}

#[test]
fn the_database_does_not_touch_the_source_file() {
    // The record is written beside the source, never into it (spec §11).
    let dir = tempfile::tempdir().expect("temp dir");
    let source = dir.path().join("original.mp4");
    std::fs::write(&source, b"evidence").expect("writes");
    let before = std::fs::read(&source).expect("reads");

    let case_dir = dir.path().join("case.tptcase");
    let store = Store::open(&case_dir).expect("opens");
    store.upsert_case("case-1", "A", None).expect("case");
    store.insert_asset(&asset("a1", "aa")).expect("asset");

    assert_eq!(std::fs::read(&source).expect("reads"), before);
    assert!(case_dir.join("case.db").exists());
}

/// Builds a case with one asset, one analysis, and no findings.
///
/// The asset id is derived once and reused, because `insert_finding` writes
/// `finding.asset_id` as a foreign key: a finding naming a different id than the
/// asset row is rejected by the database, and that is the constraint under test
/// rather than a mismatch to paper over.
fn asset_id() -> AssetId {
    AssetId::new_derived(&["a1"])
}

/// Builds a case with one asset, one analysis, and no findings.
fn seeded_store() -> Store {
    let store = Store::open_in_memory().expect("opens");
    store.upsert_case("case-1", "Case", None).expect("case");
    store
        .insert_asset(&StoredAsset {
            id: asset_id().to_string(),
            case_id: "case-1".to_owned(),
            name: "a1.mp4".to_owned(),
            source_path: "C:\\evidence\\a1.mp4".to_owned(),
            size_bytes: 4096,
            sha256: Some("aa".to_owned()),
            blake3: None,
        })
        .expect("asset");
    store
        .insert_analysis(&StoredAnalysis {
            id: "an1".to_owned(),
            case_id: "case-1".to_owned(),
            asset_id: asset_id().to_string(),
            cache_key: "k".to_owned(),
            finding_count: 1,
            rule_count: 1,
            profile: "default".to_owned(),
            profile_fingerprint: "pf".to_owned(),
            rule_set_fingerprint: "rs".to_owned(),
            started_at: 1_700_000_000,
        })
        .expect("analysis");
    store
}

fn finding(status: FindingStatus) -> Finding {
    Finding {
        id: FindingId::new_derived(&["RULE.ONE", "a1"]),
        rule_id: "RULE.ONE".to_owned(),
        severity: Severity::Warning,
        confidence: Confidence::High,
        observation: Observation {
            summary: "observed something".to_owned(),
            measurements: vec!["1.0".to_owned()],
        },
        rationale: None,
        asset_id: asset_id(),
        stream_id: None,
        timeline_start: None,
        timeline_end: None,
        evidence: Vec::new(),
        frame_index: None,
        status,
        review_note: (status != FindingStatus::New).then(|| "reviewer note".to_owned()),
    }
}

#[test]
fn a_new_finding_is_stored_without_a_review_row() {
    let store = seeded_store();
    store
        .insert_finding("an1", &finding(FindingStatus::New))
        .expect("a New finding has no review to record");

    assert_eq!(store.count("findings").expect("counted"), 1);
    assert_eq!(
        store.count("finding_reviews").expect("counted"),
        0,
        "an unreviewed finding must not fabricate a review row"
    );
}

#[test]
fn a_reviewed_finding_is_stored_alongside_its_verdict() {
    // `finding_reviews.analysis_id` is NOT NULL, so a verdict recorded without one
    // is rejected by the database. Nothing in the pipeline reviewed a finding, so
    // this path had never run before this test existed.
    let store = seeded_store();
    store
        .insert_finding("an1", &finding(FindingStatus::Accepted))
        .expect("a reviewed finding must persist its verdict");

    assert_eq!(store.count("findings").expect("counted"), 1);
    assert_eq!(store.count("finding_reviews").expect("counted"), 1);
}

#[test]
fn each_verdict_state_round_trips() {
    // Every state a reviewer can choose must survive a write. A state that only
    // fails for one variant looks like a bug confined to a rare path.
    for status in [
        FindingStatus::Reviewed,
        FindingStatus::Accepted,
        FindingStatus::Rejected,
        FindingStatus::RequiresInvestigation,
    ] {
        let store = seeded_store();
        store
            .insert_finding("an1", &finding(status))
            .unwrap_or_else(|e| panic!("{status:?} must be storable: {e}"));
        assert_eq!(
            store.count("finding_reviews").expect("counted"),
            1,
            "{status:?} must record exactly one review"
        );
    }
}

#[test]
fn a_review_does_not_change_the_stored_observation() {
    let store = seeded_store();
    let reviewed = finding(FindingStatus::Accepted);
    store.insert_finding("an1", &reviewed).expect("stored");

    let (summary, payload): (String, String) = store
        .connection()
        .query_row(
            "SELECT summary, payload FROM findings WHERE id = ?1",
            [reviewed.id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("finding row exists");

    assert_eq!(
        summary, "observed something",
        "the observation is untouched"
    );
    let decoded: Finding = serde_json::from_str(&payload).expect("payload parses");
    assert_eq!(decoded.observation.summary, "observed something");
    assert_eq!(decoded.status, FindingStatus::Accepted);
    assert_eq!(decoded.review_note.as_deref(), Some("reviewer note"));
}

#[test]
fn findings_are_read_back_with_their_verdicts() {
    let store = seeded_store();
    store
        .insert_finding("an1", &finding(FindingStatus::Rejected))
        .expect("stored");

    let findings = store.findings_in_case("case-1").expect("case reads");
    assert_eq!(findings.len(), 1);
    assert_eq!(
        findings[0].status,
        FindingStatus::Rejected,
        "a verdict recorded at insert time must be readable"
    );
}

#[test]
fn a_verdict_can_be_recorded_after_the_finding_was_stored() {
    // The normal path: the engine writes New findings, and a reviewer decides
    // later. A workflow that only worked if the verdict existed at insert time
    // would be unusable.
    let store = seeded_store();
    let recorded = finding(FindingStatus::New);
    store.insert_finding("an1", &recorded).expect("stored");

    store
        .record_review(
            "an1",
            &recorded.id.to_string(),
            FindingStatus::Accepted,
            Some("confirmed against the source"),
            1_700_000_100,
        )
        .expect("a verdict against a stored finding must be accepted");

    let reviews = store
        .reviews_of(&recorded.id.to_string())
        .expect("reviews read");
    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0].status, "accepted");
    assert_eq!(
        reviews[0].note.as_deref(),
        Some("confirmed against the source")
    );
}

#[test]
fn a_verdict_against_an_unknown_finding_is_refused() {
    // Recording a verdict with no observation beside it would let a report claim a
    // reviewer examined something that was never measured.
    let store = seeded_store();
    let result = store.record_review(
        "an1",
        "no-such-finding",
        FindingStatus::Accepted,
        None,
        1_700_000_100,
    );

    assert!(result.is_err(), "a review of nothing must be refused");
    assert_eq!(
        store.count("finding_reviews").expect("counted"),
        0,
        "the refusal must not leave a review row behind"
    );
}

#[test]
fn repeated_verdicts_append_rather_than_replace() {
    // Who concluded what, and when, is part of the record. Replacing the earlier
    // verdict would erase a reviewer's earlier reasoning.
    let store = seeded_store();
    let recorded = finding(FindingStatus::New);
    store.insert_finding("an1", &recorded).expect("stored");
    let id = recorded.id.to_string();

    store
        .record_review(
            "an1",
            &id,
            FindingStatus::RequiresInvestigation,
            Some("need the source file"),
            1_700_000_100,
        )
        .expect("first verdict");
    store
        .record_review(
            "an1",
            &id,
            FindingStatus::Accepted,
            Some("checked the source, it is real"),
            1_700_000_200,
        )
        .expect("second verdict");

    let reviews = store.reviews_of(&id).expect("reviews read");
    assert_eq!(reviews.len(), 2, "both verdicts must survive");
    assert_eq!(reviews[0].status, "requires-investigation");
    assert_eq!(reviews[1].status, "accepted");
}

#[test]
fn reviews_come_back_in_the_order_they_were_written() {
    // Ordered by row id, not timestamp: two reviews in the same second must still
    // come back in write order, or the history reads as if it happened backwards.
    let store = seeded_store();
    let recorded = finding(FindingStatus::New);
    store.insert_finding("an1", &recorded).expect("stored");
    let id = recorded.id.to_string();

    for (i, status) in [
        FindingStatus::Reviewed,
        FindingStatus::Accepted,
        FindingStatus::Rejected,
    ]
    .into_iter()
    .enumerate()
    {
        store
            .record_review("an1", &id, status, None, 1_700_000_000)
            .expect("verdict");
        assert_eq!(
            store.reviews_of(&id).expect("reviews").len(),
            i + 1,
            "each verdict must be readable immediately"
        );
    }

    let reviews = store.reviews_of(&id).expect("reviews");
    let statuses: Vec<&str> = reviews.iter().map(|r| r.status.as_str()).collect();
    assert_eq!(statuses, vec!["reviewed", "accepted", "rejected"]);
}

#[test]
fn recording_a_review_leaves_the_observation_untouched() {
    let store = seeded_store();
    let recorded = finding(FindingStatus::New);
    store.insert_finding("an1", &recorded).expect("stored");

    store
        .record_review(
            "an1",
            &recorded.id.to_string(),
            FindingStatus::Rejected,
            Some("not applicable to this delivery"),
            1_700_000_100,
        )
        .expect("verdict");

    let findings = store.findings_in_case("case-1").expect("case reads");
    assert_eq!(
        findings[0].observation.summary, "observed something",
        "the engine's own words must survive a review"
    );
}

#[test]
fn a_report_is_recorded_against_its_case() {
    let store = seeded_store();
    let id = store
        .insert_report("case-1", Some("an1"), "pdf", "reports/a1.pdf", "d1")
        .expect("a report must be recordable");

    let (format, path, digest): (String, String, String) = store
        .connection()
        .query_row(
            "SELECT format, relative_path, sha256 FROM reports WHERE id = ?1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("report row exists");

    assert_eq!(format, "pdf");
    assert_eq!(path, "reports/a1.pdf");
    assert_eq!(digest, "d1");
}

#[test]
fn regenerating_the_same_report_updates_its_digest_rather_than_duplicating_it() {
    let store = seeded_store();
    let first = store
        .insert_report("case-1", Some("an1"), "pdf", "reports/a1.pdf", "d1")
        .expect("first report");
    let second = store
        .insert_report("case-1", Some("an1"), "pdf", "reports/a1.pdf", "d2")
        .expect("regenerated report");

    assert_eq!(first, second, "the same report must keep its identifier");
    assert_eq!(store.count("reports").expect("counted"), 1);
    let digest: String = store
        .connection()
        .query_row("SELECT sha256 FROM reports WHERE id = ?1", [&first], |r| {
            r.get(0)
        })
        .expect("report row exists");
    assert_eq!(digest, "d2", "the newer digest must win");
}

#[test]
fn a_report_with_no_analysis_is_recorded() {
    // A report generated from a case rather than a single run has no analysis to
    // point at. The column is nullable for exactly that reason.
    let store = seeded_store();
    store
        .insert_report("case-1", None, "json", "reports/case.json", "d1")
        .expect("a case-level report must be recordable");

    assert_eq!(store.count("reports").expect("counted"), 1);
}

#[test]
fn the_latest_analysis_is_returned_by_start_time() {
    let store = seeded_store();
    let later = StoredAnalysis {
        id: "an2".to_owned(),
        cache_key: "k2".to_owned(),
        started_at: 1_700_000_500,
        ..StoredAnalysis {
            id: "an1".to_owned(),
            case_id: "case-1".to_owned(),
            asset_id: asset_id().to_string(),
            cache_key: "k".to_owned(),
            finding_count: 1,
            rule_count: 1,
            profile: "default".to_owned(),
            profile_fingerprint: "pf".to_owned(),
            rule_set_fingerprint: "rs".to_owned(),
            started_at: 1_700_000_000,
        }
    };
    store.insert_analysis(&later).expect("second analysis");

    let latest = store
        .latest_analysis("case-1")
        .expect("reads")
        .expect("a case with analyses has a latest");
    assert_eq!(
        latest.started_at, 1_700_000_500,
        "the newest run must win, not the alphabetically first id"
    );
}

#[test]
fn a_case_with_no_analyses_has_no_latest() {
    let store = Store::open_in_memory().expect("opens");
    store.upsert_case("empty", "Empty", None).expect("case");
    assert!(
        store.latest_analysis("empty").expect("reads").is_none(),
        "no run means no latest run, not an error"
    );
}

#[test]
fn rules_that_raised_nothing_are_still_recorded() {
    // A case shows what was considered, so a rule that ran and found nothing is
    // part of the record (spec §35).
    let store = seeded_store();
    let rules: Vec<String> = ["VIDEO.A", "VIDEO.B", "AUDIO.C"]
        .iter()
        .map(|r| (*r).to_owned())
        .collect();
    store
        .insert_rule_results("an1", &rules)
        .expect("rules recorded");

    assert_eq!(store.count("rule_results").expect("counted"), 3);
}

#[test]
fn recording_the_same_rule_twice_does_not_duplicate_it() {
    let store = seeded_store();
    let rules = vec!["VIDEO.A".to_owned()];
    store.insert_rule_results("an1", &rules).expect("first");
    store.insert_rule_results("an1", &rules).expect("second");

    assert_eq!(
        store.count("rule_results").expect("counted"),
        1,
        "a re-run must not inflate the count of rules considered"
    );
}

#[test]
fn the_only_case_id_is_reported_when_there_is_exactly_one() {
    let store = seeded_store();
    assert_eq!(
        store.only_case_id().expect("reads").as_deref(),
        Some("case-1")
    );
}

#[test]
fn there_is_no_only_case_id_when_a_database_holds_several() {
    // A caller using this to skip a `--case` argument must be given nothing when the
    // choice is ambiguous, rather than one arbitrary case.
    let store = seeded_store();
    store
        .upsert_case("case-2", "Second", None)
        .expect("second case");

    assert!(
        store.only_case_id().expect("reads").is_none(),
        "two cases means the choice is not determined"
    );
}

#[test]
fn an_empty_database_has_no_only_case_id() {
    let store = Store::open_in_memory().expect("opens");
    assert!(store.only_case_id().expect("reads").is_none());
}

#[test]
fn a_current_run_reports_a_complete_timeline() {
    // The baseline the other two are measured against: a run written by this
    // build stored its strip, so an empty one would mean it found nothing.
    let store = seeded_store();
    assert_eq!(
        store.timeline_retention_in_case("case-1").expect("reads"),
        TimelineRetention::Complete,
    );
}

#[test]
fn a_case_with_no_analysis_is_not_reported_as_a_complete_empty_timeline() {
    // Nothing was examined, which is not the same as examining something and
    // observing nothing. Reporting `Complete` here would put the two on the same
    // footing, which is the conflation the type exists to prevent.
    let store = Store::open_in_memory().expect("opens");
    store.upsert_case("case-1", "Case", None).expect("case");
    store.insert_asset(&asset("a1", "aa")).expect("asset");

    assert_eq!(
        store.timeline_retention_in_case("case-1").expect("reads"),
        TimelineRetention::NoRuns,
    );
}

#[test]
fn a_case_written_before_timeline_retention_is_reported_as_partial() {
    // The question this answers: does the case predating schema v4 know that it
    // did?
    //
    // Built by hand rather than by a flag, because the failure mode being guarded
    // against is precisely that a case migrated to the current schema looks
    // identical to one created at it. The only honest source for this fact is
    // what the writing build recorded, so the test writes an analysis the way a
    // pre-v4 build did: with no `writer_schema_version` at all.
    let store = Store::open_in_memory().expect("opens");
    store.upsert_case("case-1", "Case", None).expect("case");
    store
        .insert_asset(&StoredAsset {
            id: asset_id().to_string(),
            case_id: "case-1".to_owned(),
            name: "a1.mp4".to_owned(),
            source_path: "C:\\evidence\\a1.mp4".to_owned(),
            size_bytes: 4096,
            sha256: Some("aa".to_owned()),
            blake3: None,
        })
        .expect("asset");
    store
        .connection()
        .execute(
            "INSERT INTO analyses (id, case_id, asset_id, cache_key, software_version, \
             analysis_version, status, finding_count, rule_count, started_at) \
             VALUES ('legacy', 'case-1', ?1, 'k', '0.1.0', 1, 'COMPLETE', 0, 0, 1)",
            [asset_id().to_string()],
        )
        .expect("an analysis row as a pre-v4 build wrote it");

    assert_eq!(
        store.timeline_retention_in_case("case-1").expect("reads"),
        TimelineRetention::Partial {
            unrecorded_runs: 1,
            total_runs: 1,
        },
    );
}

#[test]
fn a_mixed_case_counts_only_the_runs_that_predate_retention() {
    // The common shape once the feature has shipped: an old case re-analysed
    // under a current build. The strip holds the new run's entries, and the
    // screen still has to say the old run contributed nothing.
    let store = seeded_store();
    store
        .connection()
        .execute(
            "INSERT INTO analyses (id, case_id, asset_id, cache_key, software_version, \
             analysis_version, status, finding_count, rule_count, started_at) \
             VALUES ('legacy', 'case-1', ?1, 'k2', '0.1.0', 1, 'COMPLETE', 0, 0, 2)",
            [asset_id().to_string()],
        )
        .expect("a legacy row beside a current one");

    assert_eq!(
        store.timeline_retention_in_case("case-1").expect("reads"),
        TimelineRetention::Partial {
            unrecorded_runs: 1,
            total_runs: 2,
        },
    );
}

#[test]
fn an_analysis_records_the_schema_version_that_wrote_it() {
    // Without this, every future case reads as `NoRuns`/`Partial` forever and the
    // notice stops meaning anything.
    let store = seeded_store();
    let version: Option<i64> = store
        .connection()
        .query_row(
            "SELECT writer_schema_version FROM analyses WHERE id = 'an1'",
            [],
            |row| row.get(0),
        )
        .expect("reads");

    assert_eq!(
        version,
        Some(tpt_app_media_forensics_core::store::schema::SCHEMA_VERSION)
    );
}

#[test]
fn a_transaction_rolls_back_on_failure() {
    let mut store = seeded_store();
    let result = store.transaction(|tx| {
        tx.execute(
            "INSERT INTO cases (id, name) VALUES ('case-2', 'Partial')",
            [],
        )?;
        Err::<(), rusqlite::Error>(rusqlite::Error::QueryReturnedNoRows)
    });

    assert!(
        result.is_err(),
        "the closure failed, so the transaction must"
    );
    assert_eq!(
        store.count("cases").expect("counted"),
        1,
        "the case written before the failure must not survive"
    );
}

#[test]
fn a_transaction_commits_when_the_closure_succeeds() {
    let mut store = seeded_store();
    store
        .transaction(|tx| {
            tx.execute("INSERT INTO cases (id, name) VALUES ('case-2', 'Good')", [])?;
            Ok(())
        })
        .expect("commits");

    assert_eq!(store.count("cases").expect("counted"), 2);
}

#[test]
fn a_note_can_be_attached_to_an_asset() {
    let store = seeded_store();
    let id = store
        .add_note(
            "case-1",
            Some("asset"),
            Some(&asset_id().to_string()),
            "Frame rate looks wrong for the delivery spec.",
            1_700_000_900,
        )
        .expect("a note must be recordable");

    let notes = store
        .notes_on("asset", &asset_id().to_string())
        .expect("notes read");
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].id, id);
    assert!(notes[0].is_attached());
    assert_eq!(
        notes[0].body,
        "Frame rate looks wrong for the delivery spec."
    );
}

#[test]
fn a_case_level_note_needs_no_subject() {
    // "The client disputes the timestamp" is about the case, not one asset. That is
    // a real thing analysts write, so it must be storable.
    let store = seeded_store();
    store
        .add_note(
            "case-1",
            None,
            None,
            "Client disputes the timestamp.",
            1_700_000_900,
        )
        .expect("a case-level note must be recordable");

    let notes = store.notes_in_case("case-1").expect("notes read");
    assert_eq!(notes.len(), 1);
    assert!(!notes[0].is_attached());
}

#[test]
fn a_note_naming_only_half_its_subject_is_refused() {
    // A note with a kind but no id names a subject ambiguously; storing it would let
    // a later reader attach the conclusion to the wrong thing.
    let store = seeded_store();
    assert!(
        store
            .add_note("case-1", Some("asset"), None, "half a subject", 1)
            .is_err(),
        "kind without id must be refused"
    );
    assert!(
        store
            .add_note("case-1", None, Some("a1"), "half a subject", 1)
            .is_err(),
        "id without kind must be refused"
    );
    assert_eq!(store.count("notes").expect("counted"), 0);
}

#[test]
fn notes_are_returned_in_the_order_they_were_written() {
    // Ordered by row id, not timestamp: two notes in the same second must still come
    // back in write order.
    let store = seeded_store();
    let subject = asset_id().to_string();
    for body in ["first", "second", "third"] {
        store
            .add_note("case-1", Some("asset"), Some(&subject), body, 1_700_000_000)
            .expect("note");
    }

    let notes = store.notes_on("asset", &subject).expect("notes read");
    let bodies: Vec<&str> = notes.iter().map(|n| n.body.as_str()).collect();
    assert_eq!(bodies, vec!["first", "second", "third"]);
}

#[test]
fn notes_on_one_subject_do_not_appear_on_another() {
    let store = seeded_store();
    store
        .add_note(
            "case-1",
            Some("asset"),
            Some(&asset_id().to_string()),
            "about the asset",
            1_700_000_900,
        )
        .expect("note");

    assert!(
        store
            .notes_on("finding", "f-other")
            .expect("notes read")
            .is_empty(),
        "a note on an asset must not appear under a finding"
    );
}

#[test]
fn a_case_listing_includes_both_attached_and_case_level_notes() {
    // A report showing "what the analyst said about this case" must not silently
    // drop the observations that were not attached to one subject.
    let store = seeded_store();
    store
        .add_note("case-1", None, None, "case level", 1_700_000_800)
        .expect("case note");
    store
        .add_note(
            "case-1",
            Some("asset"),
            Some(&asset_id().to_string()),
            "asset level",
            1_700_000_900,
        )
        .expect("asset note");

    let notes = store.notes_in_case("case-1").expect("notes read");
    assert_eq!(notes.len(), 2, "both notes must be listed");
    assert_eq!(notes[0].body, "case level");
    assert_eq!(notes[1].body, "asset level");
}

#[test]
fn a_note_body_is_stored_verbatim() {
    // A note is evidence of what the analyst concluded. Reflowing their prose or
    // trimming their whitespace would alter the record.
    let store = seeded_store();
    let body = "  line one\r\n\n\tline two with trailing space   ";
    store
        .add_note("case-1", None, None, body, 1_700_000_900)
        .expect("note");

    let notes = store.notes_in_case("case-1").expect("notes read");
    assert_eq!(
        notes[0].body, body,
        "the analyst's text must survive byte for byte"
    );
}

#[test]
fn a_new_case_is_readable_from_its_database_immediately() {
    // `acquire` creates the case; `analyze` used to be the only thing that wrote a
    // `cases` row. So an acquired-but-never-analysed case reported no case id, and
    // every read path -- notes, reviews, reports -- treated it as empty.
    let dir = tempfile::tempdir().expect("temp dir");
    let case = Case::new(
        "Acquired Only".to_owned(),
        Some("no analysis yet".to_owned()),
    );
    let root = dir.path().join("case.tptcase");
    CaseDirectory::create(&root, &case).expect("creates case");

    let store = tpt_app_media_forensics_core::store::Store::open(&root).expect("opens");
    assert_eq!(
        store.only_case_id().expect("reads").as_deref(),
        Some(case.id.to_string().as_str()),
        "a freshly acquired case must be visible in its own database"
    );
}

#[test]
fn notes_from_one_case_are_invisible_to_another() {
    let store = seeded_store();
    store
        .upsert_case("case-2", "Second", None)
        .expect("second case");
    store
        .add_note("case-1", None, None, "about case one", 1_700_000_900)
        .expect("note");

    assert!(
        store.notes_in_case("case-2").expect("reads").is_empty(),
        "a note must not leak across cases"
    );
}
