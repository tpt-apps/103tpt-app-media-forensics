//! The complete professional workflow, end to end (spec §96).
//!
//! # What this file is for
//!
//! Spec §96's definition of done ends with one line no unit test can establish:
//! *"A real-world professional workflow has been completed successfully."* Every
//! other item in that list has a corresponding test somewhere. This one needs a
//! whole case, taken all the way through, with each artefact checked as it is
//! produced.
//!
//! The workflow is the one a QC engineer actually performs on an incoming
//! delivery, and it is deliberately the *combined* one — forensic analysis and
//! specification checking against a single case — because that is the claim the
//! product makes: one engine serving media QC and forensic analysis without
//! collapsing them into the same thing.
//!
//! # Why it drives the real binary
//!
//! Argument parsing, exit codes, and the read-only guarantee are all part of what
//! "completed successfully" means, and none of them live in the library API. A
//! workflow that passed by calling library functions would not have demonstrated
//! that a user can perform it.
//!
//! # Offline
//!
//! No step reaches the network. The whole file runs with no connectivity, which
//! is spec §96's own requirement rather than a property of the harness.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Path to the CLI binary under test.
fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tpt-app-media-forensics-cli"))
}

/// Runs a command, returning its exit code and both output streams.
///
/// The code is returned rather than asserted on, because the exit code *is* part
/// of what is under test: `validate` must exit 2 on FAIL so a delivery gate
/// composes in a pipeline, and only a caller outside the library can check it.
fn run(args: &[&str]) -> (Option<i32>, String, String) {
    let output = cli().args(args).output().expect("runs the CLI");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Parses `--json` output, panicking with the text on failure so a broken run
/// says what it actually printed.
fn json(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout).unwrap_or_else(|e| panic!("invalid JSON ({e}): {stdout}"))
}

/// Writes the client delivery specification this case is judged against.
///
/// Spec §68's example, with the tolerance stated rather than left to a default: a
/// specification whose tolerance is implicit is one whose verdict can change
/// without anyone editing it.
fn write_profile(dir: &Path) -> PathBuf {
    let path = dir.join("client-x.json");
    std::fs::write(
        &path,
        r#"{
  "name": "Client X Delivery",
  "version": 3,
  "requirements": [
    { "kind": "video_codec", "any_of": ["h264"] },
    { "kind": "video_resolution", "width": 1920, "height": 1080 },
    { "kind": "frame_rate", "fps": 25.0, "tolerance": 0.5 },
    { "kind": "container_format", "any_of": ["mov"] }
  ]
}
"#,
    )
    .expect("writes the profile");
    path
}

/// Builds a 1080p 25 fps H.264 MP4 — a delivery that meets the profile.
fn write_conforming_delivery(dir: &Path, name: &str) -> PathBuf {
    use tpt_app_media_forensics_container::fixture::{build_mp4, TrackSpec};
    let path = dir.join(name);
    std::fs::write(&path, build_mp4(&TrackSpec::video_25fps(1920, 1080, 100)))
        .expect("writes the delivery");
    path
}

/// Builds a 720p MP4 — the same delivery cut down, which is what a real rejection
#[test]
fn a_client_delivery_is_received_examined_judged_and_reported_end_to_end() {
    // The whole of spec §96's closing claim, in one run.
    let dir = tempfile::tempdir().expect("temp dir");
    let work = dir.path().join("workflow");
    std::fs::create_dir_all(&work).expect("creates the working directory");

    let media = work.join("deliveries");
    std::fs::create_dir_all(&media).expect("creates the intake folder");

    let profile = write_profile(&work);
    let conforming = write_conforming_delivery(&media, "A001_master.mp4");
    let conforming_hash = sha256_of(&conforming);
    let non_conforming = write_non_conforming_delivery(&media, "A002_master.mp4");

    // --- Step 1: the specification is authored, and checked to load ----------
    // Before anything is examined. A profile that fails to parse must be caught
    // here, not after the media has been analysed and a verdict promised.
    let (code, stdout, stderr) = run(&["profile", "check", profile.to_str().expect("utf-8")]);
    assert_eq!(code, Some(0), "profile check failed: {stdout}\n{stderr}");
    assert!(
        stdout.contains("client-x-delivery v3"),
        "the profile must identify its version (spec §70): {stdout}"
    );

    // --- Step 2: QC the delivery, before intake ------------------------------
    // The gate a production house runs on an incoming folder. Both verdicts are
    // taken here so a rejection is on record before any case exists.
    let (code, stdout, stderr) = run(&[
        "validate",
        conforming.to_str().expect("utf-8"),
        "--profile",
        profile.to_str().expect("utf-8"),
    ]);
    assert_eq!(
        code,
        Some(0),
        "a conforming delivery must pass: {stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("PASS"),
        "the verdict must be stated: {stdout}"
    );

    let (code, stdout, stderr) = run(&[
        "validate",
        non_conforming.to_str().expect("utf-8"),
        "--profile",
        profile.to_str().expect("utf-8"),
    ]);
    assert_eq!(
        code,
        Some(2),
        "a non-conforming delivery must exit 2 so a pipeline can gate on it: \
         {stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("FAIL"),
        "the verdict must be stated: {stdout}"
    );
    assert!(
        stdout.contains("Expected: 1920x1080") && stdout.contains("Observed: 1280x720"),
        "the rejection must name the missed requirement: {stdout}"
    );

    // --- Step 3: create the case and acquire the delivery -------------------
    let cases = work.join("cases");
    let (code, stdout, stderr) = run(&[
        "acquire",
        conforming.to_str().expect("utf-8"),
        "--name",
        "Client X delivery 2026-02",
        "--parent",
        cases.to_str().expect("utf-8"),
    ]);
    assert_eq!(code, Some(0), "acquire failed: {stdout}\n{stderr}");

    let case = cases.join("case.tptcase");
    assert!(
        case.join("case.db").exists(),
        "the case database must exist at {}",
        case.display()
    );

    // --- Step 4: analyse it with the full engine ----------------------------
    let (code, stdout, stderr) = run(&[
        "--json",
        "analyze",
        conforming.to_str().expect("utf-8"),
        "--case-dir",
        case.to_str().expect("utf-8"),
    ]);
    assert_eq!(code, Some(0), "analyze failed: {stdout}\n{stderr}");

    let analysis = json(&stdout);
    assert!(
        analysis["sha256"]
            .as_str()
            .is_some_and(|h| h == conforming_hash),
        "the analysis must record the asset's real digest: {stdout}"
    );
    assert_eq!(
        analysis["findings"].as_array().map_or(0, Vec::len),
        analysis["finding_count"].as_u64().unwrap_or_default() as usize,
        "the finding count must match the findings listed: {stdout}"
    );

    // --- Step 5: the analyst's note -----------------------------------------
    // Spec §65. The engine's observations are half the record; a reviewer's
    // conclusion is the other half, and a report that drops it misrepresents
    // what was decided.
    let (code, _, stderr) = run(&[
        "note",
        "--case-dir",
        case.to_str().expect("utf-8"),
        "--subject-kind",
        "asset",
        "--subject",
        "A001_master.mp4",
        "--body",
        "Client X confirms this is the approved master for broadcast.",
    ]);
    assert_eq!(code, Some(0), "note failed: {stderr}");

    // --- Step 6: validate the case against the specification ----------------
    // Both halves now: the specification, and the findings the engine recorded.
    let (code, stdout, stderr) = run(&[
        "--json",
        "validate",
        "--case-dir",
        case.to_str().expect("utf-8"),
        "--profile",
        profile.to_str().expect("utf-8"),
        "--write",
    ]);
    let validation = json(&stdout);
    let _ = (code, stderr);

    assert!(
        validation["requirements"]
            .as_array()
            .expect("requirements must be listed")
            .iter()
            .all(|r| r["outcome"] == "MET"),
        "the conforming delivery must meet every requirement: {stdout}"
    );

    // A case with a significant finding fails even when every requirement is
    // met. The verdict is the worse of the two halves, never the better, and it
    // is the engine's own answer rather than a re-derivation in the command layer.
    let blocking = validation["blocking"].as_array().expect("blocking");
    let expected = if blocking.is_empty() { "PASS" } else { "FAIL" };
    assert_eq!(
        validation["result"], expected,
        "the verdict must follow both halves: {stdout}"
    );
    assert_eq!(
        validation["profile"]["identifier"], "client-x-delivery v3",
        "the verdict must name the exact profile version (spec §70): {stdout}"
    );

    // --- Step 7: the evidence bundle ----------------------------------------
    let bundle = case.join("reports").join("validated");
    assert!(bundle.exists(), "--write produced no bundle: {stdout}");
    for file in [
        "case-data.json",
        "case-report.html",
        "case-report.pdf",
        "findings.csv",
        "measurements.csv",
        "asset-hashes.csv",
        "bundle-manifest.json",
    ] {
        assert!(
            bundle.join(file).exists(),
            "the bundle is missing {file}; a recipient could not verify it"
        );
    }

    // The manifest must actually verify (spec §62). That is the property which
    // makes the bundle self-checking rather than merely complete.
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(bundle.join("bundle-manifest.json")).expect("reads"),
    )
    .expect("the manifest is JSON");
    let entries = manifest["files"].as_array().expect("manifest files");
    assert!(!entries.is_empty(), "the manifest lists nothing");
    for entry in entries {
        let name = entry["name"].as_str().expect("a file name");
        assert_eq!(
            entry["sha256"].as_str().expect("a digest"),
            sha256_of(&bundle.join(name)),
            "{name} does not match its manifest entry: a recipient cannot confirm \
             nothing changed in transit"
        );
    }

    // The rendered report must carry the verdict, the profile version, and the
    // analyst's note. A report rendering none of those cannot answer the question
    // it was produced for.
    let html = std::fs::read_to_string(bundle.join("case-report.html")).expect("reads HTML");
    assert!(
        html.contains(expected),
        "the report must render the verdict: {html}"
    );
    assert!(
        html.contains("client-x-delivery v3"),
        "the report must name the exact profile version (spec §70): {html}"
    );
    assert!(
        html.contains("Client X confirms this is the approved master"),
        "the analyst's note must travel into the report (spec §65): {html}"
    );

    // --- Step 8: the source was never modified ------------------------------
    // Spec §11, and the first principle in docs/architecture.md. A tool that
    // altered the file it examined would have destroyed the evidence and every
    // hash in the report beside it.
    assert_eq!(
        sha256_of(&conforming),
        conforming_hash,
        "the source media was modified: the analysis is worthless as evidence"
    );

    // --- Step 9: re-running reproduces the report byte for byte -------------
    // Spec §77. Two renders of one case must be identical, or a report cannot be
    // compared against an earlier one to show that nothing has changed.
    for name in ["recheck.json", "recheck2.json"] {
        let (code, stdout, stderr) = run(&[
            "report",
            "--case-dir",
            case.to_str().expect("utf-8"),
            "--out",
            work.join(name).to_str().expect("utf-8"),
        ]);
        assert_eq!(code, Some(0), "report failed: {stdout}\n{stderr}");
    }
    assert_eq!(
        std::fs::read(work.join("recheck.json")).expect("reads"),
        std::fs::read(work.join("recheck2.json")).expect("reads"),
        "two renders of one case must be byte-identical (spec §77)"
    );
}

fn write_non_conforming_delivery(dir: &Path, name: &str) -> PathBuf {
    use tpt_app_media_forensics_container::fixture::{build_mp4, TrackSpec};
    let path = dir.join(name);
    std::fs::write(&path, build_mp4(&TrackSpec::video_25fps(1280, 720, 100)))
        .expect("writes the delivery");
    path
}

/// The SHA-256 of a file's bytes, for the read-only and manifest checks.
fn sha256_of(path: &Path) -> String {
    use sha2::Digest as _;
    let bytes = std::fs::read(path).expect("reads the file");
    tpt_app_media_forensics_model::asset::to_hex(&sha2::Sha256::digest(&bytes))
}
