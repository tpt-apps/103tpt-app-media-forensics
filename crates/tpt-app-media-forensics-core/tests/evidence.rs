//! Tests for evidence extraction (spec §32–§33).
//!
//! # What was broken
//!
//! Every piece of this existed and nothing called it. `EvidenceStore` was
//! implemented, verified, and unit-tested; `FrameImage::to_png` was implemented
//! and unit-tested; `Report.evidence` and `Finding.evidence` were fields, and
//! `Report::referenced_evidence()` filtered one against the other. `Report.evidence`
//! was hardcoded to `Vec::new()` at both construction sites, so that filter could
//! only ever return an empty list, and the `evidence` table existed in the schema
//! with nothing ever inserting into it.
//!
//! The engine decoded frames in Tier-2, measured scene changes and near-duplicates
//! from them, and then dropped the pixels on the floor. A report claimed to cite
//! evidence and cited none.
//!
//! # Why these tests exist at all
//!
//! The `readme_claims` guard passed while all of this was true. It checked that
//! `pub fn to_png` appeared in a source file — proving the *implementation* exists,
//! never that anything *reaches* it. That is the same class of defect these tests
//! exist to catch, and the guard is why it survived: a capability claim checked
//! against a string in a file is a claim nothing verifies.

use std::path::Path;

use tpt_app_media_forensics_core::case_dir::CaseDirectory;
use tpt_app_media_forensics_core::pipeline::load_report;
use tpt_app_media_forensics_core::{AnalysisEngine, AnalysisOutcome};
use tpt_app_media_forensics_model::Case;

/// Encodes eight real AV1 frames and wraps them in WebM.
///
/// Real encoded bytes rather than stub payloads: a stub is undecodable for the
/// uninteresting reason that it was never anything else, and the point of this
/// fixture is to reach the decoder that produces the frames evidence is made of.
fn av1_webm() -> Vec<u8> {
    use tpt_kinetix_av1::{Av1Encoder, Av1EncoderConfig};
    use tpt_kinetix_core::frame::VideoFrame;
    use tpt_kinetix_core::pixel_format::PixelFormat;
    use tpt_kinetix_core::timestamp::Timestamp;

    const W: u32 = 64;
    const H: u32 = 48;

    let mut encoder = Av1Encoder::new(&Av1EncoderConfig {
        width: W,
        height: H,
        bitrate: 0,
        quantizer: 80,
        speed: 10,
        keyframe_interval: 8,
    })
    .expect("opens AV1 encoder");

    let mut payloads = Vec::new();
    for index in 0..8u8 {
        let (w, h) = (W as usize, H as usize);
        let mut data = vec![0u8; w * h + (w * h) / 2];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] = ((index as usize * 30) + x + y) as u8;
            }
        }
        for sample in data.iter_mut().skip(w * h) {
            *sample = 128;
        }
        let ts = Timestamp::new(i64::from(index) * 33, (1, 1_000));
        let frame = VideoFrame {
            pts: ts,
            dts: ts,
            data,
            width: W,
            height: H,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: true,
        };
        if let Some(packet) = encoder.encode_frame(&frame).expect("encodes") {
            payloads.push(packet.data);
        }
    }
    payloads.extend(
        encoder
            .flush()
            .expect("flushes")
            .into_iter()
            .map(|p| p.data),
    );
    assert!(!payloads.is_empty(), "the AV1 encoder produced nothing");

    let blocks: Vec<(u16, bool, Vec<u8>)> = payloads
        .iter()
        .enumerate()
        .map(|(i, p)| (u16::try_from(i * 33).unwrap_or(u16::MAX), true, p.clone()))
        .collect();
    tpt_app_media_forensics_container::fixture::build_webm("V_AV1", 1, &blocks)
}

/// Creates a case at `dir/case.tptcase` and returns it with the source path.
fn case_at(dir: &Path, contents: &[u8], name: &str) -> (CaseDirectory, std::path::PathBuf) {
    std::fs::create_dir_all(dir).expect("creates case parent");
    let source = dir.join(name);
    std::fs::write(&source, contents).expect("writes source");

    let case_dir = dir.join("case.tptcase");
    CaseDirectory::create(&case_dir, &Case::new("Evidence", None)).expect("creates case");
    (CaseDirectory::open(&case_dir).expect("opens case"), source)
}

/// Writes the AV1 fixture into a fresh case and analyses it.
fn analyse_av1(dir: &Path) -> (CaseDirectory, AnalysisOutcome) {
    let (case_dir, source) = case_at(dir, &av1_webm(), "sample.webm");
    let outcome = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("analysis runs");
    (case_dir, outcome)
}

/// Builds the report the CLI builds, so the test exercises the real type rather
/// than a stand-in that could drift from it.
fn report_of(outcome: &AnalysisOutcome) -> tpt_app_media_forensics_report::Report {
    tpt_app_media_forensics_report::Report {
        schema_version: tpt_app_media_forensics_report::REPORT_SCHEMA_VERSION,
        case_name: "Evidence".to_owned(),
        case_id: "case:00000000-0000-0000-0000-000000000001".to_owned(),
        case_description: None,
        assets: Vec::new(),
        findings: outcome.findings.clone(),
        evidence: outcome.evidence.clone(),
        limitations: outcome.limitations.clone(),
        notes: Vec::new(),
        methodology: tpt_app_media_forensics_report::Methodology {
            application_version: "0.1.0".to_owned(),
            analysis_version: "test".to_owned(),
            profile: "test".to_owned(),
            profile_fingerprint: "test".to_owned(),
            enabled_rules: Vec::new(),
            rule_set_fingerprint: "test".to_owned(),
            input_hashes: Vec::new(),
            analysis_timestamp_unix: 0,
            applicable_standards: Vec::new(),
            analysis_fingerprint: "test".to_owned(),
        },
        validation: None,
        delivery: None,
    }
}

#[test]
fn decoded_frames_are_written_as_real_pngs_into_the_case() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, outcome) = analyse_av1(&tmp.path().join("run"));

    assert!(
        !outcome.evidence.is_empty(),
        "Tier-2 decoded frames but the engine wrote no evidence: {}",
        outcome.limitations.join("; ")
    );

    for artefact in &outcome.evidence {
        let bytes = std::fs::read(case_dir.root().join(&artefact.relative_path))
            .unwrap_or_else(|_| panic!("{} was recorded but not written", artefact.relative_path));

        // A real PNG, checked by its magic bytes rather than by trusting the
        // extension. Writing PNG exists so a reviewer can open the artefact with
        // their own tools; a file named `.png` that is not a PNG fails that.
        assert_eq!(
            &bytes[..8],
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
            "{} is not a PNG",
            artefact.relative_path
        );
        // Checking against the record catches a truncated write, which the magic
        // bytes alone would not.
        assert_eq!(
            bytes.len() as u64,
            artefact.integrity.size_bytes,
            "{} is not the size its own record claims",
            artefact.relative_path
        );
    }
}

#[test]
fn every_written_artefact_is_marked_verified_with_hashes() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (_case_dir, outcome) = analyse_av1(&tmp.path().join("run"));

    for artefact in &outcome.evidence {
        // `EvidenceStore::write_verified` re-reads the bytes from disk and
        // compares them before raising this flag. A record claiming verification
        // without that check behind it is exactly the "evidence that was assumed
        // rather than verified" the spec forbids.
        assert!(
            artefact.integrity.verified,
            "{} claims no verification",
            artefact.relative_path
        );
        assert!(
            artefact.integrity.hashes.sha256().is_some(),
            "{} was stored without a SHA-256",
            artefact.relative_path
        );
    }
}

#[test]
fn evidence_paths_are_relative_to_the_case_so_a_case_can_be_moved() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, outcome) = analyse_av1(&tmp.path().join("run"));

    for artefact in &outcome.evidence {
        assert!(
            Path::new(&artefact.relative_path).is_relative(),
            "{} is absolute, which would break when the case is archived",
            artefact.relative_path
        );
        assert!(
            artefact.relative_path.starts_with("evidence/"),
            "{} is not under the case's evidence directory",
            artefact.relative_path
        );
        assert!(
            case_dir.root().join(&artefact.relative_path).exists(),
            "{} does not resolve inside the case",
            artefact.relative_path
        );
    }
}

#[test]
fn a_finding_cites_only_evidence_that_exists_and_a_report_resolves_it() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (_case_dir, outcome) = analyse_av1(&tmp.path().join("run"));

    let known: Vec<String> = outcome.evidence.iter().map(|e| e.id.to_string()).collect();
    let mut cited = 0_usize;

    for finding in &outcome.findings {
        for id in &finding.evidence {
            cited += 1;
            assert!(
                known.contains(&id.to_string()),
                "{} cites evidence {id}, which no artefact backs",
                finding.rule_id
            );
        }
    }

    // `referenced_evidence` is the filter the HTML and PDF render through, and it
    // was permanently empty while `Report.evidence` was hardcoded to `Vec::new()`.
    let report = report_of(&outcome);
    assert!(
        !report.evidence.is_empty(),
        "the report carries no evidence, so the evidence table renders empty"
    );
    assert_eq!(
        report.referenced_evidence().len(),
        cited,
        "the report resolved {} artefacts for {} citations",
        report.referenced_evidence().len(),
        cited
    );
}

#[test]
fn evidence_survives_into_a_report_rebuilt_from_the_case_database() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, outcome) = analyse_av1(&tmp.path().join("run"));

    assert!(!outcome.evidence.is_empty(), "no evidence to rebuild from");

    // `load_report` is what `report --case-dir` uses. It hardcoded
    // `evidence: Vec::new()` before, so a rebuilt report showed findings naming
    // evidence beside an empty evidence table.
    let rebuilt = load_report(&case_dir).expect("report rebuilds");

    assert_eq!(
        rebuilt.report.evidence.len(),
        outcome.evidence.len(),
        "the rebuilt report lost evidence: {} written, {} read back",
        outcome.evidence.len(),
        rebuilt.report.evidence.len()
    );

    let rebuilt_ids: Vec<String> = rebuilt
        .report
        .evidence
        .iter()
        .map(|e| e.id.to_string())
        .collect();
    for artefact in &outcome.evidence {
        assert!(
            rebuilt_ids.contains(&artefact.id.to_string()),
            "{} did not survive the round trip through the database",
            artefact.relative_path
        );
    }
}

#[test]
fn a_second_run_over_the_same_file_writes_no_further_evidence() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (case_dir, source) = case_at(&tmp.path().join("run"), &av1_webm(), "sample.webm");

    let first = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("first analysis");
    assert!(!first.cache_hit, "the first run cannot be a cache hit");
    assert!(
        !first.evidence.is_empty(),
        "the first run wrote no evidence"
    );

    let second = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("second analysis");
    assert!(
        second.cache_hit,
        "the second run should be served from the cache"
    );

    // A cache hit runs no decode, so it has no frames to write. Writing again
    // would duplicate every artefact in the case for no reason — and the store is
    // append-only, so the duplicates could never be cleaned up.
    assert_eq!(
        second.evidence.len(),
        0,
        "a cache hit wrote evidence; a cached run decodes nothing and should write nothing"
    );
}

#[test]
fn a_file_with_no_decodable_pixels_states_the_gap_rather_than_falling_silent() {
    let tmp = tempfile::tempdir().expect("temp dir");
    // A well-formed MP4 whose video track this build never decodes: identified
    // from the container, never turned into pixels.
    let (case_dir, source) = case_at(
        &tmp.path().join("run"),
        &tpt_app_media_forensics_container::fixture::build_mp4_with_metadata(
            &tpt_app_media_forensics_container::fixture::TrackSpec::video_25fps(64, 48, 4),
        ),
        "sample.mp4",
    );

    let outcome = AnalysisEngine::new()
        .analyse(&source, &case_dir)
        .expect("analysis runs");

    assert!(
        outcome.evidence.is_empty(),
        "a non-decodable codec produced evidence"
    );
    // The absence must be stated. "No evidence because nothing was gathered" and
    // "no evidence because the writer is not wired" are indistinguishable to a
    // reader unless the report says which one happened — which is exactly how the
    // original bug stayed invisible.
    assert!(
        outcome
            .limitations
            .iter()
            .any(|l| l.contains("Tier-2") || l.contains("decod")),
        "no limitation explained the missing evidence: {:?}",
        outcome.limitations
    );
}
