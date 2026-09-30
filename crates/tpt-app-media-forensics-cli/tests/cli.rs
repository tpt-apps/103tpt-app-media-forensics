//! End-to-end tests for the CLI binary.
//!
//! These drive the real executable rather than calling library functions, so
//! they also cover argument parsing, exit codes, and the read-only guarantee
//! as the analyst experiences it.
//!
//! The CLI shares the engine with the GUI (spec §51), so these tests are also
//! evidence that the two front ends cannot drift apart: anything asserted here
//! holds for the desktop app too.

use std::path::Path;
use std::process::{Command, Output};

use tpt_app_media_forensics_container::fixture::{build_mp4, TrackSpec};

/// Path to the CLI binary under test.
fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tpt-app-media-forensics-cli"))
}

/// Writes a small deterministic file and returns its path.
fn fixture(dir: &Path, name: &str, contents: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).expect("writes fixture");
    path
}

/// Converts a completed command into a `(success, stdout, stderr)` triple.
fn split(output: Output) -> (bool, String, String) {
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn help_lists_every_documented_subcommand() {
    let output = cli().arg("--help").output().expect("runs CLI");
    let (ok, stdout, _) = split(output);

    assert!(ok);
    for command in ["hash", "acquire", "inspect", "analyze", "report", "batch"] {
        assert!(
            stdout.contains(command),
            "`{command}` is missing from the help output"
        );
    }
}

#[test]
fn hash_reports_both_digests() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = fixture(dir.path(), "evidence.mp4", b"the quick brown fox");

    let output = cli()
        .args(["hash", file.to_str().expect("utf-8 path")])
        .output()
        .expect("runs CLI");
    let (ok, stdout, stderr) = split(output);

    assert!(ok, "hash failed: {stderr}");
    assert!(stdout.contains("SHA-256"));
    assert!(stdout.contains("BLAKE3"));
    assert!(stdout.contains("read-only"));
}

#[test]
fn hash_json_output_is_valid_json_with_both_digests() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = fixture(dir.path(), "evidence.mp4", b"payload");

    let output = cli()
        .args(["--json", "hash", file.to_str().expect("utf-8 path")])
        .output()
        .expect("runs CLI");
    let (ok, stdout, stderr) = split(output);

    assert!(ok, "hash failed: {stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(value["hashes_complete"], serde_json::Value::Bool(true));
    assert!(value["sha256"].is_string());
    assert!(value["blake3"].is_string());
}

#[test]
fn hash_is_deterministic_across_invocations() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = fixture(dir.path(), "evidence.mp4", b"stable payload");
    let path = file.to_str().expect("utf-8 path");

    let first = cli().args(["--json", "hash", path]).output().expect("runs");
    let second = cli().args(["--json", "hash", path]).output().expect("runs");

    assert_eq!(
        first.stdout, second.stdout,
        "CLI output must be reproducible"
    );
}

#[test]
fn hash_of_a_missing_file_fails_with_a_diagnosable_message() {
    let output = cli()
        .args(["hash", "no-such-file-98765.mp4"])
        .output()
        .expect("runs CLI");
    let (ok, _, stderr) = split(output);

    assert!(!ok, "a missing file must not report success");
    assert!(
        stderr.contains("no-such-file-98765.mp4"),
        "the error must name the file that failed: {stderr}"
    );
}

#[test]
fn hash_does_not_modify_the_source() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = fixture(dir.path(), "evidence.mp4", b"immutable evidence");
    let before = std::fs::read(&file).expect("reads");

    cli()
        .args(["hash", file.to_str().expect("utf-8 path")])
        .output()
        .expect("runs CLI");

    let after = std::fs::read(&file).expect("reads");
    assert_eq!(
        before, after,
        "the source must never be modified (spec §11)"
    );
}

#[test]
fn acquire_creates_a_case_with_the_documented_layout() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = fixture(dir.path(), "evidence.mp4", b"acquired payload");
    let parent = dir.path().join("out");

    let output = cli()
        .args([
            "acquire",
            file.to_str().expect("utf-8 path"),
            "--name",
            "Operation Alpha",
            "--parent",
            parent.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("runs CLI");
    let (ok, stdout, stderr) = split(output);

    assert!(ok, "acquire failed: {stderr}");
    let case_dir = parent.join("case.tptcase");
    for sub in ["assets", "evidence", "reports", "cache"] {
        assert!(case_dir.join(sub).is_dir(), "{sub} directory is missing");
    }
    assert!(case_dir.join("manifest.json").is_file());

    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(case_dir.join("manifest.json")).expect("reads manifest"),
    )
    .expect("manifest is valid JSON");
    assert_eq!(manifest["name"], "Operation Alpha");
    assert_eq!(
        manifest["assets"].as_array().expect("assets array").len(),
        1
    );
    assert!(stdout.contains("read-only"));
}

#[test]
fn acquire_refuses_to_overwrite_an_existing_case() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = fixture(dir.path(), "evidence.mp4", b"payload");
    let parent = dir.path().join("out");
    let path = file.to_str().expect("utf-8 path");
    let parent_str = parent.to_str().expect("utf-8 path");

    let first = cli()
        .args(["acquire", path, "--name", "Alpha", "--parent", parent_str])
        .output()
        .expect("runs CLI");
    assert!(first.status.success());

    let second = cli()
        .args(["acquire", path, "--name", "Beta", "--parent", parent_str])
        .output()
        .expect("runs CLI");
    assert!(
        !second.status.success(),
        "overwriting an existing case would destroy evidence"
    );
}

#[test]
fn analyze_rejects_a_directory_that_is_not_a_case() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = fixture(dir.path(), "evidence.mp4", b"payload");

    let output = cli()
        .args([
            "analyze",
            file.to_str().expect("utf-8 path"),
            "--case-dir",
            dir.path().to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("runs CLI");
    let (ok, _, stderr) = split(output);

    assert!(!ok, "an uninitialised case directory must be rejected");
    assert!(
        stderr.contains("not an initialised case"),
        "the error must explain why: {stderr}"
    );
}

#[test]
fn inspect_reports_container_structure_for_a_real_mp4() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("sample.mp4");
    std::fs::write(&file, build_mp4(&TrackSpec::video_25fps(1920, 1080, 50)))
        .expect("writes fixture");

    let output = cli()
        .args(["inspect", file.to_str().expect("utf-8 path")])
        .output()
        .expect("runs CLI");
    let (ok, stdout, stderr) = split(output);

    assert!(ok, "inspect failed: {stderr}");
    assert!(stdout.contains("isobmff"), "container not identified");
    assert!(stdout.contains("video"), "stream kind not reported");
    assert!(stdout.contains("avc1"), "codec not reported");
    assert!(stdout.contains("1920x1080"), "dimensions not reported");
    assert!(
        stdout.contains("frame rate: 25"),
        "measured rate not reported"
    );
    assert!(
        stdout.contains("read-only"),
        "read-only guarantee must be stated"
    );
}

#[test]
fn inspect_json_output_is_valid_json() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("sample.mp4");
    std::fs::write(&file, build_mp4(&TrackSpec::audio_48khz(480))).expect("writes");

    let output = cli()
        .args(["--json", "inspect", file.to_str().expect("utf-8 path")])
        .output()
        .expect("runs CLI");
    let (ok, stdout, stderr) = split(output);

    assert!(ok, "inspect failed: {stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(value["container"], "isobmff");
    assert_eq!(value["stream_count"], 1);
    assert_eq!(value["streams"][0]["kind"], "audio");
}

#[test]
fn inspect_reports_an_extension_mismatch() {
    // A Matroska file named .mp4 is a finding, not an error.
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("disguised.mp4");
    let mut bytes = vec![0x1A, 0x45, 0xDF, 0xA3];
    bytes.extend_from_slice(&[0u8; 64]);
    std::fs::write(&file, bytes).expect("writes");

    let output = cli()
        .args(["inspect", file.to_str().expect("utf-8 path")])
        .output()
        .expect("runs CLI");
    let (_, _, stderr) = split(output);

    assert!(
        stderr.contains("extension does not match"),
        "a renamed container must be reported: {stderr}"
    );
}

#[test]
fn inspect_of_a_non_media_file_fails_with_the_path() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("notes.mp4");
    std::fs::write(&file, b"this is plainly not a container").expect("writes");

    let output = cli()
        .args(["inspect", file.to_str().expect("utf-8 path")])
        .output()
        .expect("runs CLI");
    let (ok, _, stderr) = split(output);

    assert!(!ok, "a non-container must not report a clean inspection");
    assert!(stderr.contains("notes.mp4"), "{stderr}");
}
