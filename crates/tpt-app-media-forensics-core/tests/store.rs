//! Integration tests for case persistence.
//!
//! Exercises the store against a real database file in a case directory, so
//! the layout, pragmas, and round-trip behaviour are covered together rather
//! than in isolation.

use tpt_app_media_forensics_core::store::{Store, StoredAsset};

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
