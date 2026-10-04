//! Integration tests for the analysis cache.
//!
//! The cache's whole purpose is to avoid stale results, so most of these tests
//! are about when a cached result must be *refused*.

use tpt_app_media_forensics_core::cache::{AnalysisCache, CacheEntry};
use tpt_app_media_forensics_model::{
    AnalysisVersion, CacheKey, Confidence, Finding, FindingId, FindingStatus, MediaTime,
    Observation, ProfileFingerprint, RuleSetFingerprint, Severity, StreamId,
};

fn key(asset: &str, version: u32, profile: &str, rules: &str) -> CacheKey {
    CacheKey {
        asset_sha256: asset.to_owned(),
        analysis_version: AnalysisVersion::new(version),
        profile: ProfileFingerprint::from_serialized(profile),
        rules: RuleSetFingerprint::from_rule_ids(&[rules.to_owned()]),
    }
}

fn entry(k: CacheKey) -> CacheEntry {
    CacheEntry {
        key: k.clone(),
        findings: vec![Finding {
            id: FindingId::new_derived(&["RULE", k.asset_sha256.as_str()]),
            rule_id: "RULE".to_owned(),
            severity: Severity::Warning,
            confidence: Confidence::High,
            observation: Observation {
                summary: "a finding".to_owned(),
                measurements: vec![],
            },
            rationale: None,
            asset_id: tpt_app_media_forensics_model::AssetId::new_derived(&["a"]),
            stream_id: Some(StreamId::new_derived(&["a", "0"])),
            timeline_start: Some(MediaTime::ZERO),
            timeline_end: None,
            evidence: Vec::new(),
            frame_index: None,
            status: FindingStatus::New,
            review_note: None,
        }],
        rule_ids: vec!["RULE".to_owned()],
        stream_count: 2,
    }
}

#[test]
fn a_stored_result_is_read_back_unchanged() {
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = AnalysisCache::new(dir.path());
    let k = key("aa", 1, "default", "RULE");

    cache.store(&entry(k.clone())).expect("stores");
    let loaded = cache.load(&k).expect("loads").expect("a hit");

    assert_eq!(loaded.findings.len(), 1);
    assert_eq!(loaded.key, k);
}

#[test]
fn a_miss_is_not_an_error() {
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = AnalysisCache::new(dir.path());
    assert!(cache
        .load(&key("aa", 1, "p", "r"))
        .expect("no error")
        .is_none());
    assert!(cache.is_empty().expect("counts"));
}

#[test]
fn a_different_asset_does_not_hit_the_cache() {
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = AnalysisCache::new(dir.path());
    cache.store(&entry(key("aa", 1, "p", "r"))).expect("stores");
    assert!(cache
        .load(&key("bb", 1, "p", "r"))
        .expect("loads")
        .is_none());
}

#[test]
fn a_bumped_analysis_version_invalidates_the_cache() {
    // The single most important property: engine behaviour changed, so the
    // old findings must not be served.
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = AnalysisCache::new(dir.path());
    cache.store(&entry(key("aa", 1, "p", "r"))).expect("stores");
    assert!(cache
        .load(&key("aa", 2, "p", "r"))
        .expect("loads")
        .is_none());
}

#[test]
fn a_changed_profile_invalidates_the_cache() {
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = AnalysisCache::new(dir.path());
    cache
        .store(&entry(key("aa", 1, "loose", "r")))
        .expect("stores");
    assert!(
        cache
            .load(&key("aa", 1, "tight", "r"))
            .expect("loads")
            .is_none(),
        "a tightened profile must not reuse findings from a looser one"
    );
}

#[test]
fn a_changed_rule_set_invalidates_the_cache() {
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = AnalysisCache::new(dir.path());
    cache
        .store(&entry(key("aa", 1, "p", "RULE_A")))
        .expect("stores");
    assert!(cache
        .load(&key("aa", 1, "p", "RULE_B"))
        .expect("loads")
        .is_none());
}

#[test]
fn an_entry_whose_contents_disagree_with_its_key_is_discarded() {
    // A file that does not match what we asked for must not be trusted, even
    // if it parses.
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = AnalysisCache::new(dir.path());
    let stored_key = key("aa", 1, "p", "r");
    cache.store(&entry(stored_key.clone())).expect("stores");

    let path = cache.entry_path(&stored_key);
    let mut wrong = entry(key("zz", 1, "p", "r"));
    wrong.key = key("zz", 1, "p", "r");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&wrong).expect("serialises"),
    )
    .expect("writes");

    assert!(cache.load(&stored_key).expect("loads").is_none());
}

#[test]
fn a_corrupt_entry_is_a_miss_not_a_failure() {
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = AnalysisCache::new(dir.path());
    let k = key("aa", 1, "p", "r");
    cache.store(&entry(k.clone())).expect("stores");
    std::fs::write(cache.entry_path(&k), b"{ not json").expect("writes");

    assert!(cache.load(&k).expect("loads").is_none());
}

#[test]
fn storing_leaves_no_temporary_file_behind() {
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = AnalysisCache::new(dir.path());
    cache.store(&entry(key("aa", 1, "p", "r"))).expect("stores");
    assert!(
        !cache.directory().join("entry.tmp").exists(),
        "the atomic-rename temp file must not survive"
    );
}

#[test]
fn entries_live_under_the_case_cache_directory_only() {
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = AnalysisCache::new(dir.path());
    cache.store(&entry(key("aa", 1, "p", "r"))).expect("stores");
    assert!(cache
        .entry_path(&key("aa", 1, "p", "r"))
        .starts_with(dir.path()));
}

#[test]
fn a_review_decision_survives_a_cache_hit() {
    // spec §66: a reviewer's verdict must survive re-analysis rather than
    // being recomputed, so reviews live outside the cache key.
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = AnalysisCache::new(dir.path());
    let k = key("aa", 1, "p", "r");
    cache.store(&entry(k.clone())).expect("stores");

    let finding_id = entry(k.clone()).findings[0].id;
    let updated = cache
        .apply_review(
            &finding_id,
            FindingStatus::Accepted,
            Some("expected".to_owned()),
        )
        .expect("applies");
    assert_eq!(updated, 1);

    let loaded = cache.load(&k).expect("loads").expect("hit");
    assert_eq!(loaded.findings[0].status, FindingStatus::Accepted);
    assert_eq!(loaded.findings[0].review_note.as_deref(), Some("expected"));
}

#[test]
fn applying_a_review_to_an_unknown_finding_changes_nothing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = AnalysisCache::new(dir.path());
    let k = key("aa", 1, "p", "r");
    cache.store(&entry(k.clone())).expect("stores");

    let unknown = FindingId::new_derived(&["NOPE"]);
    assert_eq!(
        cache
            .apply_review(&unknown, FindingStatus::Rejected, None)
            .expect("applies"),
        0
    );
    let loaded = cache.load(&k).expect("loads").expect("hit");
    assert_eq!(loaded.findings[0].status, FindingStatus::New);
}

#[test]
fn entry_filenames_are_digest_based_and_distinct() {
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = AnalysisCache::new(dir.path());
    let a = cache.entry_path(&key("aa", 1, "p", "r"));
    let b = cache.entry_path(&key("bb", 1, "p", "r"));
    assert_ne!(a, b);
    assert_eq!(
        a.file_name().expect("name").to_str().expect("utf8").len(),
        32 + ".json".len(),
        "filenames are a fixed-length digest plus extension"
    );
}
