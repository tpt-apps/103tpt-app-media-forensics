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
    for command in [
        "hash", "acquire", "inspect", "analyze", "report", "note", "batch", "compare", "search",
        "validate",
    ] {
        assert!(
            stdout.contains(command),
            "`{command}` is missing from the help output"
        );
    }
}

#[test]
fn compare_reports_a_real_difference_between_two_mp4_files() {
    // The comparison engine's reason for existing (spec §38-40). Driven through
    // the real binary because the point is not only that `compare` works but that
    // a caller reaches it: two commits shipped it, unit-tested it, and nothing
    // outside those tests ever invoked it.
    let dir = tempfile::tempdir().expect("temp dir");
    let left = dir.path().join("left.mp4");
    let right = dir.path().join("right.mp4");
    std::fs::write(&left, build_mp4(&TrackSpec::video_25fps(1920, 1080, 50))).expect("writes left");
    std::fs::write(&right, build_mp4(&TrackSpec::video_25fps(1280, 720, 50)))
        .expect("writes right");

    let output = cli()
        .args([
            "compare",
            left.to_str().expect("utf-8 path"),
            right.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("runs CLI");
    let (ok, stdout, stderr) = split(output);

    assert!(ok, "compare failed: {stderr}");
    assert!(
        stdout.contains("1920x1080") || stdout.contains("1280x720"),
        "the resolution difference must be surfaced: {stdout}"
    );
    assert!(
        stdout.contains("DIFFERS"),
        "a difference must be marked as such, not merely implied: {stdout}"
    );
}

#[test]
fn compare_reports_a_file_as_identical_to_itself() {
    // Without this, a comparison that reported everything as different would
    // still pass the test above. Two runs of one file are the control.
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("sample.mp4");
    let bytes = build_mp4(&TrackSpec::video_25fps(1280, 720, 50));
    std::fs::write(&file, bytes).expect("writes fixture");

    let output = cli()
        .args([
            "compare",
            file.to_str().expect("utf-8 path"),
            file.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("runs CLI");
    let (ok, stdout, stderr) = split(output);

    assert!(ok, "compare failed: {stderr}");
    assert!(
        !stdout.contains("DIFFERS"),
        "a file must not differ from itself: {stdout}"
    );
    assert!(
        stdout.contains("no measured differences"),
        "a self-comparison must report no differences: {stdout}"
    );
    // And it must *not* claim equivalence. This file is H.264 with no audio, so
    // several axes were never measured, and `is_equivalent` is deliberately false
    // whenever anything went uncomparable — "they match on everything we looked
    // at" is a weaker claim than "they match". Asserting it here is what stops a
    // future change from quietly relaxing that.
    assert!(
        stdout.contains("not every axis could be compared"),
        "an uncomparable axis must not be reported as agreement: {stdout}"
    );
}

#[test]
fn a_declared_reference_is_recorded_by_digest_and_relabels_the_sides() {
    // Spec §67: "users should be able to define a reference asset... what
    // changed?". The load-bearing part is that the answer is bound to *bytes*.
    //
    // A file name is not evidence — `Master.mov` survives being overwritten by a
    // different encode — so this asserts the digest travels with the result, and
    // that the two sides are named for the question being asked rather than for
    // their position on the command line.
    let dir = tempfile::tempdir().expect("temp dir");
    let master = dir.path().join("Master.mp4");
    let delivery = dir.path().join("Delivery.mp4");
    std::fs::write(&master, build_mp4(&TrackSpec::video_25fps(1920, 1080, 50)))
        .expect("writes master");
    std::fs::write(&delivery, build_mp4(&TrackSpec::video_25fps(1280, 720, 50)))
        .expect("writes delivery");

    let output = cli()
        .args([
            "compare",
            master.to_str().expect("utf-8 path"),
            delivery.to_str().expect("utf-8 path"),
            "--reference",
        ])
        .output()
        .expect("runs CLI");
    let (ok, stdout, stderr) = split(output);

    assert!(ok, "compare --reference failed: {stderr}");
    assert!(
        stdout.contains("Reference"),
        "the declared side must be labelled as the reference: {stdout}"
    );
    assert!(
        stdout.contains("Delivery"),
        "the examined side must be labelled as the delivery: {stdout}"
    );
    assert!(
        stdout.contains("sha256"),
        "the reference digest must be printed or the answer names no master: {stdout}"
    );
    // And the differences themselves must still be reported: a reference
    // designation is not an excuse to summarise.
    assert!(
        stdout.contains("DIFFERS"),
        "what changed must still be reported: {stdout}"
    );
}

#[test]
fn a_plain_comparison_claims_no_reference() {
    // The reverse direction, and the one that stops §67 from quietly becoming
    // every comparison. An ordinary file-to-file comparison has no declared
    // master, and printing one — or an empty placeholder for one — would dress a
    // question nobody asked up as a provenance claim.
    let dir = tempfile::tempdir().expect("temp dir");
    let left = dir.path().join("left.mp4");
    let right = dir.path().join("right.mp4");
    std::fs::write(&left, build_mp4(&TrackSpec::video_25fps(640, 480, 30))).expect("writes left");
    std::fs::write(&right, build_mp4(&TrackSpec::video_25fps(320, 240, 30))).expect("writes right");

    // Two runs, because the two claims live in two different outputs: the JSON
    // document carries the structured fields, and only the rendered text carries
    // the side labels. Checking one for the other would pass vacuously.
    let json_run = cli()
        .args([
            "compare",
            left.to_str().expect("utf-8 path"),
            right.to_str().expect("utf-8 path"),
            "--json",
        ])
        .output()
        .expect("runs CLI");
    let (ok, json_out, stderr) = split(json_run);
    assert!(ok, "compare failed: {stderr}");

    let value: serde_json::Value =
        serde_json::from_str(&json_out).unwrap_or_else(|e| panic!("not JSON ({e}): {json_out}"));
    assert!(
        value.get("reference").is_none(),
        "a comparison with no declared reference must not carry one: {json_out}"
    );

    let text_run = cli()
        .args([
            "compare",
            left.to_str().expect("utf-8 path"),
            right.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("runs CLI");
    let (ok, text_out, stderr) = split(text_run);
    assert!(ok, "compare failed: {stderr}");
    assert!(
        text_out.contains("Left") && text_out.contains("Right"),
        "without a reference the sides stay positional: {text_out}"
    );
    assert!(
        !text_out.contains("Reference") && !text_out.contains("sha256"),
        "a plain comparison must not print a reference designation: {text_out}"
    );
}

#[test]
fn a_reference_that_cannot_be_read_fails_rather_than_comparing_unlabelled() {
    // The failure mode the flag exists to prevent. Silently degrading to a plain
    // comparison would print "what changed since the master?" with nothing
    // recording which master — the one output that must never exist.
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = dir.path().join("no-such-master.mp4");
    let delivery = dir.path().join("delivery.mp4");
    std::fs::write(&delivery, build_mp4(&TrackSpec::video_25fps(320, 240, 30)))
        .expect("writes delivery");

    let output = cli()
        .args([
            "compare",
            missing.to_str().expect("utf-8 path"),
            delivery.to_str().expect("utf-8 path"),
            "--reference",
        ])
        .output()
        .expect("runs CLI");
    let (ok, stdout, stderr) = split(output);

    assert!(
        !ok,
        "an unreadable reference must not report success: {stdout}"
    );
    // And the refusal must say *why*, naming the file. A bare non-zero exit would
    // leave a reviewer unable to tell "the master is missing" from "the tool is
    // broken", which is the difference between acting on the answer and filing it.
    assert!(
        stderr.contains("no-such-master.mp4"),
        "the failure must name the reference that could not be read: {stderr}"
    );
    assert!(
        !stdout.contains("DIFFERS"),
        "no comparison may be emitted for an unidentified reference: {stdout}"
    );
}

#[test]
fn two_different_masters_produce_two_different_answers() {
    // The property that makes the digest worth carrying. If the recorded
    // reference did not vary with the master, then "what changed?" would be
    // unanswerable: the same output would be presented as evidence about two
    // different masters, and a reviewer would have no way to tell.
    let dir = tempfile::tempdir().expect("temp dir");
    let master_a = dir.path().join("MasterA.mp4");
    let master_b = dir.path().join("MasterB.mp4");
    let delivery = dir.path().join("Delivery.mp4");
    std::fs::write(
        &master_a,
        build_mp4(&TrackSpec::video_25fps(1920, 1080, 50)),
    )
    .expect("writes master A");
    // Same nominal shape, different bytes: a different encode of one master, which
    // is exactly the case a filename cannot distinguish.
    std::fs::write(
        &master_b,
        build_mp4(&TrackSpec::video_25fps(1920, 1080, 40)),
    )
    .expect("writes master B");
    std::fs::write(&delivery, build_mp4(&TrackSpec::video_25fps(1280, 720, 50)))
        .expect("writes delivery");

    let digest_for = |master: &std::path::Path| -> String {
        let output = cli()
            .args([
                "compare",
                master.to_str().expect("utf-8 path"),
                delivery.to_str().expect("utf-8 path"),
                "--reference",
                "--json",
            ])
            .output()
            .expect("runs CLI");
        let (ok, stdout, stderr) = split(output);
        assert!(ok, "compare --reference failed: {stderr}");
        let value: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("not JSON ({e}): {stdout}"));
        value["reference"]["sha256"]
            .as_str()
            .unwrap_or_else(|| panic!("the JSON must carry the reference digest: {stdout}"))
            .to_owned()
    };

    let a = digest_for(&master_a);
    let b = digest_for(&master_b);
    assert_ne!(
        a, b,
        "two different masters must record two different digests, or the digest is \
         not identifying the reference"
    );
}

#[test]
fn compare_emits_machine_readable_json() {
    // `compare --json` has to be one parseable document, like every other
    // machine-readable path in this CLI.
    let dir = tempfile::tempdir().expect("temp dir");
    let left = dir.path().join("left.mp4");
    let right = dir.path().join("right.mp4");
    std::fs::write(&left, build_mp4(&TrackSpec::video_25fps(640, 480, 30))).expect("writes left");
    std::fs::write(&right, build_mp4(&TrackSpec::video_25fps(320, 240, 30))).expect("writes right");

    let output = cli()
        .args([
            "compare",
            left.to_str().expect("utf-8 path"),
            right.to_str().expect("utf-8 path"),
            "--json",
        ])
        .output()
        .expect("runs CLI");
    let (ok, stdout, stderr) = split(output);

    assert!(ok, "compare failed: {stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {stdout}"));

    // The tolerances travel with the result so a reader can judge each claim
    // rather than take it on trust.
    assert!(
        value.get("tolerances").is_some(),
        "the tolerances must be reported alongside: {stdout}"
    );
    assert!(
        value.get("streams").is_some(),
        "the stream half must be present: {stdout}"
    );
}

#[test]
fn search_reads_back_the_findings_the_engine_wrote() {
    // The point of the command: the engine writes findings to the database, and
    // before it nothing read them back. `analysed_case` uses a fixture with a
    // known GOP change, so the search has something specific to find.
    let dir = tempfile::tempdir().expect("temp dir");
    let case = analysed_case(dir.path());

    let output = cli()
        .args([
            "search",
            "--case-dir",
            case.to_str().expect("utf-8 path"),
            "GOP",
        ])
        .output()
        .expect("runs CLI");
    let (ok, stdout, stderr) = split(output);

    assert!(ok, "search failed: {stderr}");
    assert!(
        stdout.contains("GOP"),
        "the finding the engine recorded must be findable: {stdout}"
    );
    assert!(
        stdout.contains("finding"),
        "hits must be labelled with the kind of record they are: {stdout}"
    );
}

#[test]
fn search_reports_how_many_matched_rather_than_only_how_many_are_shown() {
    // The property the search module exists to protect. `--limit 1` on a case
    // holding several findings must say the page is truncated and name the real
    // total, because "1 result" would read as "there was only one".
    let dir = tempfile::tempdir().expect("temp dir");
    let case = analysed_case(dir.path());

    let output = cli()
        .args([
            "search",
            "--case-dir",
            case.to_str().expect("utf-8 path"),
            "--limit",
            "1",
        ])
        .output()
        .expect("runs CLI");
    let (ok, stdout, stderr) = split(output);

    assert!(ok, "search failed: {stderr}");
    assert!(
        stdout.contains("of ") && stdout.contains("matches"),
        "a truncated page must name the total it truncated from: {stdout}"
    );
    assert!(
        stdout.contains("truncated"),
        "truncation must be stated, never left to be inferred from a short list: {stdout}"
    );
}

#[test]
fn a_search_that_matches_nothing_says_so_rather_than_looking_empty() {
    let dir = tempfile::tempdir().expect("temp dir");
    let case = analysed_case(dir.path());

    let output = cli()
        .args([
            "search",
            "--case-dir",
            case.to_str().expect("utf-8 path"),
            "no-such-text-98765",
        ])
        .output()
        .expect("runs CLI");
    let (ok, stdout, stderr) = split(output);

    assert!(ok, "search failed: {stderr}");
    assert!(
        stdout.contains("No matching records"),
        "an empty result must say so explicitly: {stdout}"
    );
    assert!(
        stdout.contains("0 match"),
        "the count must be zero, not absent: {stdout}"
    );
}

#[test]
fn search_emits_machine_readable_json_naming_the_query() {
    let dir = tempfile::tempdir().expect("temp dir");
    let case = analysed_case(dir.path());

    let output = cli()
        .args([
            "search",
            "--case-dir",
            case.to_str().expect("utf-8 path"),
            "GOP",
            "--json",
        ])
        .output()
        .expect("runs CLI");
    let (ok, stdout, stderr) = split(output);

    assert!(ok, "search failed: {stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {stdout}"));

    // The query is echoed, not just the results: a saved document listing rows is
    // ambiguous without knowing what was asked for.
    assert_eq!(value["term"], "GOP", "{stdout}");
    assert!(
        value.get("total").is_some(),
        "the total must be reported so truncation is detectable: {stdout}"
    );
    assert!(
        value.get("truncated").is_some(),
        "`truncated` must be an explicit field, never implied: {stdout}"
    );
}

#[test]
fn help_lists_every_documented_subcommand_including_compare() {
    let output = cli().arg("--help").output().expect("runs CLI");
    let (ok, stdout, _) = split(output);

    assert!(ok);
    assert!(
        stdout.contains("compare"),
        "`compare` is missing from the help output"
    );
    assert!(
        stdout.contains("search"),
        "`search` is missing from the help output"
    );
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
fn analyze_reports_stage_progress_without_polluting_stdout() {
    // Spec §55 asks for background workers and progress reporting, and a QC pass
    // over a long master is slow enough that a silent terminal reads as a hung
    // process. The progress therefore goes to stderr...
    //
    // ...and specifically *not* to stdout, because `analyze --json` is meant to be
    // piped. A progress line interleaved with the result document would corrupt
    // the output for every consumer that parses it, which is the opposite of what
    // machine-readable output is for.
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("sample.mp4");
    std::fs::write(&file, build_mp4(&TrackSpec::video_25fps(320, 240, 30)))
        .expect("writes fixture");

    let acquired = cli()
        .args([
            "acquire",
            file.to_str().expect("utf-8 path"),
            "--name",
            "Progress",
            "--parent",
            dir.path().to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("acquires a case");
    assert!(
        acquired.status.success(),
        "acquire failed: {}",
        String::from_utf8_lossy(&acquired.stderr)
    );
    let case = dir.path().join("case.tptcase");

    let output = cli()
        .args([
            "analyze",
            file.to_str().expect("utf-8 path"),
            "--case-dir",
            case.to_str().expect("utf-8 path"),
            "--json",
        ])
        .output()
        .expect("runs CLI");
    let (ok, stdout, stderr) = split(output);

    assert!(ok, "analyze failed: {stderr}");
    assert!(
        stderr.contains("acquisition"),
        "the run must report the stages it went through: {stderr}"
    );
    assert!(
        stderr.contains("concurrent analysis"),
        "the independent stages must report themselves: {stderr}"
    );
    assert!(
        !stdout.contains("acquisition"),
        "progress must not reach stdout; `--json` output has to stay parseable: {stdout}"
    );
    // And the JSON is still a single parseable document.
    assert!(
        serde_json::from_str::<serde_json::Value>(&stdout).is_ok(),
        "stdout must remain valid JSON: {stdout}"
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

/// Builds a real MP4, acquires it, and analyses it into a case directory.
fn analysed_case(dir: &Path) -> std::path::PathBuf {
    let bytes = tpt_app_media_forensics_container::fixture::build_mp4_stsd_gop_change();
    let file = dir.join("sample.mp4");
    std::fs::write(&file, bytes).expect("writes fixture");

    let parent = dir.join("out");
    let out = cli()
        .args([
            "acquire",
            file.to_str().expect("utf-8"),
            "--name",
            "Reported Case",
            "--parent",
            parent.to_str().expect("utf-8"),
        ])
        .output()
        .expect("acquires");
    assert!(out.status.success(), "acquire failed");

    let case_dir = parent.join("case.tptcase");
    let out = cli()
        .args([
            "analyze",
            file.to_str().expect("utf-8"),
            "--case-dir",
            case_dir.to_str().expect("utf-8"),
        ])
        .output()
        .expect("analyses");
    assert!(out.status.success(), "analyze failed");
    case_dir
}

#[test]
fn a_reference_designation_is_recorded_listed_and_cleared() {
    // Spec §67's "users should be able to define a reference asset", driven through
    // the real binary. The engine could have guessed which file was the master and
    // the point of the command is that it does not: which of two encodes is
    // authoritative is a decision about the job, and the designation has to survive
    // the process exiting for a reopened case to know it.
    let dir = tempfile::tempdir().expect("temp dir");
    let case_dir = analysed_case(dir.path());

    // Nothing designated yet, and saying so plainly rather than printing nothing.
    let listed = cli()
        .args([
            "reference",
            "--case-dir",
            case_dir.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("runs CLI");
    assert!(listed.status.success(), "listing failed");
    let empty = String::from_utf8_lossy(&listed.stdout).into_owned();
    assert!(
        empty.contains("No reference designated"),
        "an undesignated case must say so rather than print an empty list: {empty}"
    );

    let designated = cli()
        .args([
            "reference",
            "--case-dir",
            case_dir.to_str().expect("utf-8 path"),
            "sample.mp4",
        ])
        .output()
        .expect("runs CLI");
    assert!(
        designated.status.success(),
        "designation failed: {}",
        String::from_utf8_lossy(&designated.stderr)
    );
    let output = String::from_utf8_lossy(&designated.stdout).into_owned();
    assert!(
        output.contains("sample.mp4"),
        "the designation must name the file: {output}"
    );
    // And print the digest. A designation an analyst cannot see is a designation
    // they cannot check, and the digest is what a later comparison is bound to.
    assert!(
        output.contains("sha256"),
        "the designation must carry the reference's digest: {output}"
    );

    // Listing again must find it — this is the reopen-later path, proved through
    // the binary rather than by trusting the in-process store.
    let relisted = cli()
        .args([
            "reference",
            "--case-dir",
            case_dir.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("runs CLI");
    let after = String::from_utf8_lossy(&relisted.stdout).into_owned();
    assert!(
        after.contains("sample.mp4") && !after.contains("No reference designated"),
        "the designation must persist across invocations: {after}"
    );

    let cleared = cli()
        .args([
            "reference",
            "--case-dir",
            case_dir.to_str().expect("utf-8 path"),
            "sample.mp4",
            "--clear",
        ])
        .output()
        .expect("runs CLI");
    assert!(
        cleared.status.success(),
        "clearing failed: {}",
        String::from_utf8_lossy(&cleared.stderr)
    );
    let relisted = cli()
        .args([
            "reference",
            "--case-dir",
            case_dir.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("runs CLI");
    assert!(
        String::from_utf8_lossy(&relisted.stdout).contains("No reference designated"),
        "a cleared designation must not come back"
    );
}

#[test]
fn designating_an_asset_the_case_does_not_hold_is_refused() {
    // Silently succeeding would print a confirmation for a designation nothing
    // recorded — the one failure a reviewer could not detect from the report.
    let dir = tempfile::tempdir().expect("temp dir");
    let case_dir = analysed_case(dir.path());

    let out = cli()
        .args([
            "reference",
            "--case-dir",
            case_dir.to_str().expect("utf-8 path"),
            "no-such-file.mp4",
        ])
        .output()
        .expect("runs CLI");

    assert!(
        !out.status.success(),
        "an unknown asset must not report success"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no-such-file.mp4"),
        "the refusal must name what it could not find: {stderr}"
    );
}

#[test]
fn report_writes_each_supported_format() {
    let dir = tempfile::tempdir().expect("temp dir");
    let case_dir = analysed_case(dir.path());

    for (extension, check) in [
        ("json", "{"),
        ("html", "<!DOCTYPE html>"),
        ("csv", "finding_id,"),
        ("pdf", "%PDF-1.4"),
    ] {
        let out = dir.path().join(format!("report.{extension}"));
        let output = cli()
            .args([
                "report",
                "--case-dir",
                case_dir.to_str().expect("utf-8"),
                "--out",
                out.to_str().expect("utf-8"),
            ])
            .output()
            .expect("runs CLI");
        let (ok, stdout, stderr) = split(output);
        assert!(ok, "report .{extension} failed: {stderr}");

        let bytes = std::fs::read(&out).expect("report written");
        assert!(!bytes.is_empty(), "report .{extension} is empty");
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains(check),
            "report .{extension} does not contain `{check}`"
        );
        assert!(
            stdout.contains("SHA-256"),
            "report .{extension} does not state its own digest"
        );
    }
}

#[test]
fn report_carries_the_findings_the_analysis_found() {
    let dir = tempfile::tempdir().expect("temp dir");
    let case_dir = analysed_case(dir.path());
    let out = dir.path().join("report.json");

    let output = cli()
        .args([
            "report",
            "--case-dir",
            case_dir.to_str().expect("utf-8"),
            "--out",
            out.to_str().expect("utf-8"),
        ])
        .output()
        .expect("runs CLI");
    assert!(output.status.success(), "report failed");

    let text = std::fs::read_to_string(&out).expect("report readable");
    let value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    assert!(
        value["findings"]
            .as_array()
            .expect("findings array")
            .iter()
            .any(|f| f["rule_id"] == "VIDEO.GOP_LENGTH_CHANGE"),
        "the report should carry the GOP finding: {text}"
    );
    assert!(
        text.contains("must not be interpreted as proof"),
        "the disclaimer is mandatory on every report"
    );
    assert!(
        value["methodology"]["analysis_fingerprint"].is_string(),
        "the report must state its analysis fingerprint"
    );
}

#[test]
fn report_is_deterministic_for_the_same_case() {
    let dir = tempfile::tempdir().expect("temp dir");
    let case_dir = analysed_case(dir.path());

    let mut digests = Vec::new();
    for run in 0..2 {
        let out = dir.path().join(format!("report{run}.pdf"));
        let output = cli()
            .args([
                "report",
                "--case-dir",
                case_dir.to_str().expect("utf-8"),
                "--out",
                out.to_str().expect("utf-8"),
            ])
            .output()
            .expect("runs CLI");
        assert!(output.status.success(), "report failed");
        digests.push(std::fs::read(&out).expect("report written"));
    }

    assert_eq!(
        digests[0], digests[1],
        "two renders of one case must be byte-identical"
    );
}

#[test]
fn report_refuses_an_unknown_format() {
    let dir = tempfile::tempdir().expect("temp dir");
    let case_dir = analysed_case(dir.path());
    let out = dir.path().join("report.docx");

    let output = cli()
        .args([
            "report",
            "--case-dir",
            case_dir.to_str().expect("utf-8"),
            "--out",
            out.to_str().expect("utf-8"),
        ])
        .output()
        .expect("runs CLI");
    let (ok, _, stderr) = split(output);

    assert!(!ok, "an unsupported format must fail");
    assert!(
        stderr.contains(".json, .html, .csv, .pdf"),
        "the error should name the supported formats: {stderr}"
    );
    assert!(!out.exists(), "nothing should be written on failure");
}

#[test]
fn report_writes_a_self_verifying_bundle() {
    let dir = tempfile::tempdir().expect("temp dir");
    let case_dir = analysed_case(dir.path());
    // The format comes from the extension, so a `.bundle` path is a directory.
    let out = dir.path().join("bundle.bundle");

    let output = cli()
        .args([
            "report",
            "--case-dir",
            case_dir.to_str().expect("utf-8"),
            "--out",
            out.to_str().expect("utf-8"),
        ])
        .output()
        .expect("runs CLI");
    assert!(output.status.success(), "bundle failed");
    assert!(out.join("case-data.json").is_file());
    assert!(
        out.join("case-report.pdf").is_file(),
        "the bundle should carry the PDF"
    );
    assert!(
        out.join("bundle-manifest.json").is_file(),
        "the bundle needs a verifying manifest"
    );
}

/// Stages a small intake tree and returns the directory.
fn intake_tree(dir: &Path) -> std::path::PathBuf {
    let intake = dir.join("intake");
    std::fs::create_dir_all(intake.join("sub")).expect("creates intake");

    std::fs::write(
        intake.join("clean.mp4"),
        tpt_app_media_forensics_container::fixture::build_mp4(
            &tpt_app_media_forensics_container::fixture::TrackSpec::video_25fps(320, 240, 30),
        ),
    )
    .expect("writes clean");
    std::fs::write(
        intake.join("damaged.mp4"),
        tpt_app_media_forensics_container::fixture::build_mp4_stsd_gop_change(),
    )
    .expect("writes damaged");
    std::fs::write(
        intake.join("sub/nested.mp4"),
        tpt_app_media_forensics_container::fixture::build_mp4(
            &tpt_app_media_forensics_container::fixture::TrackSpec::video_25fps(320, 240, 30),
        ),
    )
    .expect("writes nested");
    std::fs::write(intake.join("notes.txt"), b"not media").expect("writes text");
    intake
}

#[test]
fn batch_analyses_a_whole_directory_including_subdirectories() {
    let dir = tempfile::tempdir().expect("temp dir");
    let intake = intake_tree(dir.path());
    let case_dir = dir.path().join("case.tptcase");

    // A case must exist before a batch can write into it.
    let seed = intake.join("clean.mp4");
    let output = cli()
        .args([
            "acquire",
            seed.to_str().expect("utf-8"),
            "--name",
            "Batch Case",
            "--parent",
            dir.path().to_str().expect("utf-8"),
        ])
        .output()
        .expect("acquires");
    assert!(output.status.success());

    let output = cli()
        .args([
            "batch",
            intake.to_str().expect("utf-8"),
            "--case-dir",
            case_dir.to_str().expect("utf-8"),
        ])
        .output()
        .expect("runs CLI");
    let (ok, stdout, stderr) = split(output);

    assert!(ok, "batch failed: {stderr}");
    assert!(stdout.contains("Analysed 3"), "stdout was:\n{stdout}");
    assert!(
        stdout.contains("nested.mp4"),
        "nested files must be analysed"
    );
    assert!(
        !stdout.contains("notes.txt"),
        "non-media must not be analysed"
    );
    assert!(
        stdout.contains("Fingerprint"),
        "a batch must state its analysis fingerprint"
    );
}

#[test]
fn batch_json_lists_every_file_analysed() {
    let dir = tempfile::tempdir().expect("temp dir");
    let intake = intake_tree(dir.path());
    let case_dir = dir.path().join("case.tptcase");

    let output = cli()
        .args([
            "acquire",
            intake.join("clean.mp4").to_str().expect("utf-8"),
            "--name",
            "Batch Case",
            "--parent",
            dir.path().to_str().expect("utf-8"),
        ])
        .output()
        .expect("acquires");
    assert!(output.status.success());

    let output = cli()
        .args([
            "batch",
            intake.to_str().expect("utf-8"),
            "--case-dir",
            case_dir.to_str().expect("utf-8"),
            "--json",
        ])
        .output()
        .expect("runs CLI");
    assert!(output.status.success());

    let text = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    assert_eq!(value["analysed"], 3);
    assert_eq!(value["failed"], 0);

    let files = value["files"].as_array().expect("files array");
    assert_eq!(files.len(), 3, "only real media is analysed");
    for file in files {
        assert_eq!(file["status"], "analysed");
    }
    assert!(
        !text.contains("notes.txt"),
        "non-media must not appear in the batch"
    );
}

#[test]
fn a_second_batch_hits_the_cache_for_every_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let intake = intake_tree(dir.path());
    let case_dir = dir.path().join("case.tptcase");

    let output = cli()
        .args([
            "acquire",
            intake.join("clean.mp4").to_str().expect("utf-8"),
            "--name",
            "Batch Case",
            "--parent",
            dir.path().to_str().expect("utf-8"),
        ])
        .output()
        .expect("acquires");
    assert!(output.status.success());

    for _ in 0..2 {
        let output = cli()
            .args([
                "batch",
                intake.to_str().expect("utf-8"),
                "--case-dir",
                case_dir.to_str().expect("utf-8"),
            ])
            .output()
            .expect("runs CLI");
        assert!(output.status.success());
    }

    let output = cli()
        .args([
            "batch",
            intake.to_str().expect("utf-8"),
            "--case-dir",
            case_dir.to_str().expect("utf-8"),
            "--json",
        ])
        .output()
        .expect("runs CLI");
    let text = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    let cached = value["files"]
        .as_array()
        .expect("files")
        .iter()
        .filter(|f| f["cache_hit"] == serde_json::Value::Bool(true))
        .count();
    assert_eq!(cached, 3, "every file is cached on a repeat run");
}

#[test]
fn batch_rejects_a_path_that_is_not_a_directory() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = fixture(dir.path(), "one.mp4", b"payload");
    let case_dir = dir.path().join("case.tptcase");

    let output = cli()
        .args([
            "acquire",
            file.to_str().expect("utf-8"),
            "--name",
            "C",
            "--parent",
            dir.path().to_str().expect("utf-8"),
        ])
        .output()
        .expect("acquires");
    assert!(output.status.success());

    let output = cli()
        .args([
            "batch",
            file.to_str().expect("utf-8"),
            "--case-dir",
            case_dir.to_str().expect("utf-8"),
        ])
        .output()
        .expect("runs CLI");
    let (ok, _, stderr) = split(output);

    assert!(!ok, "a file is not a directory");
    assert!(stderr.contains("not a directory"), "stderr was: {stderr}");
}

#[test]
fn batch_writes_a_bundle_over_every_file_in_the_case() {
    let dir = tempfile::tempdir().expect("temp dir");
    let intake = intake_tree(dir.path());
    let case_dir = dir.path().join("case.tptcase");

    let output = cli()
        .args([
            "acquire",
            intake.join("clean.mp4").to_str().expect("utf-8"),
            "--name",
            "Batch Case",
            "--parent",
            dir.path().to_str().expect("utf-8"),
        ])
        .output()
        .expect("acquires");
    assert!(output.status.success());

    let output = cli()
        .args([
            "batch",
            intake.to_str().expect("utf-8"),
            "--case-dir",
            case_dir.to_str().expect("utf-8"),
        ])
        .output()
        .expect("runs CLI");
    assert!(output.status.success());

    for file in [
        "case-data.json",
        "case-report.html",
        "case-report.pdf",
        "findings.csv",
        "bundle-manifest.json",
    ] {
        assert!(
            case_dir.join("reports").join(file).is_file(),
            "{file} is missing from the batch bundle"
        );
    }
}

/// Builds a minimal WebM document with one VP9 track.
fn webm_bytes() -> Vec<u8> {
    let mut track_entry = vec![0xD7, 0x81, 1, 0x83, 0x81, 1, 0x86, 0x80 | 5];
    track_entry.extend_from_slice(b"V_VP9");

    let mut tracks_body = vec![0xAE, 0x80 | track_entry.len() as u8];
    tracks_body.extend_from_slice(&track_entry);

    let mut cluster = vec![0xE7, 0x81, 0x00];
    for (index, is_key) in [true, false, true].into_iter().enumerate() {
        let mut block = vec![0x81];
        block.extend_from_slice(&((index as u16) * 33).to_be_bytes());
        block.push(u8::from(is_key) << 7);
        block.extend_from_slice(&[index as u8 + 1, 0xAA]);
        cluster.extend_from_slice(&[0xA3, 0x80 | block.len() as u8]);
        cluster.extend_from_slice(&block);
    }

    let mut segment = vec![0x16, 0x54, 0xAE, 0x6B, 0x80 | tracks_body.len() as u8];
    segment.extend_from_slice(&tracks_body);
    segment.extend_from_slice(&[0x1F, 0x43, 0xB6, 0x75, 0x80 | cluster.len() as u8]);
    segment.extend_from_slice(&cluster);

    let mut doc = vec![0x1A, 0x45, 0xDF, 0xA3, 0x80, 0x18, 0x53, 0x80, 0x67];
    doc.push(0x80 | segment.len() as u8);
    doc.extend_from_slice(&segment);
    doc
}

#[test]
fn inspect_reports_a_webm_container_and_its_streams() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let path = fixture(tmp.path(), "clip.webm", &webm_bytes());

    let output = cli().arg("inspect").arg(&path).output().expect("runs CLI");
    let (ok, stdout, stderr) = split(output);

    assert!(ok, "a WebM file must inspect cleanly: {stderr}");
    assert!(stdout.contains("matroska"), "{stdout}");
    assert!(
        stdout.contains("vp09"),
        "the VP9 tag must be reported: {stdout}"
    );
    // The decisive line: WebM is no longer an unintegrated format.
    assert!(
        !stdout.contains("not integrated yet"),
        "WebM has a demuxer now: {stderr}"
    );
}

#[test]
fn inspect_json_reports_the_webm_container_tag() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let path = fixture(tmp.path(), "clip.webm", &webm_bytes());

    let output = cli()
        .args(["inspect", "--json"])
        .arg(&path)
        .output()
        .expect("runs CLI");
    let (ok, stdout, stderr) = split(output);

    assert!(ok, "{stderr}");
    let value: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON: {e}\n{stdout}"));
    assert_eq!(value["container"], "matroska");
    assert_eq!(value["stream_count"], 1);
    assert_eq!(value["streams"][0]["codec"]["name"], "vp09");
}

#[test]
fn a_renamed_webm_file_is_reported_as_a_mismatch() {
    // The whole point of signature-based detection: a renamed file is a finding.
    let tmp = tempfile::tempdir().expect("temp dir");
    let path = fixture(tmp.path(), "actually_webm.mp4", &webm_bytes());

    let output = cli().arg("inspect").arg(&path).output().expect("runs CLI");
    let (_, stdout, stderr) = split(output);

    assert!(
        stdout.contains("matroska"),
        "the bytes decide, not the name: {stdout}"
    );
    assert!(
        stderr.contains("DOES NOT MATCH") || stdout.contains("DOES NOT MATCH"),
        "the rename must be reported: {stdout}{stderr}"
    );
}

/// Creates a case directory containing one real media file.
fn acquired_case(dir: &Path) -> std::path::PathBuf {
    let file = fixture(
        dir,
        "evidence.mp4",
        &build_mp4(&TrackSpec::video_25fps(64, 48, 4)),
    );
    let parent = dir.join("out");
    std::fs::create_dir_all(&parent).expect("creates parent");

    let output = cli()
        .args([
            "acquire",
            file.to_str().expect("utf-8 path"),
            "--name",
            "Note Case",
            "--parent",
            parent.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("runs CLI");
    assert!(output.status.success(), "acquire must succeed");

    parent.join("case.tptcase")
}

/// Reads back every note recorded on a case.
fn notes_in(case: &Path) -> Vec<tpt_app_media_forensics_core::store::StoredNote> {
    let store = tpt_app_media_forensics_core::store::Store::open(case).expect("opens");
    let case_id = store.only_case_id().expect("reads").expect("one case");
    store.notes_in_case(&case_id).expect("reads")
}

#[test]
fn a_note_is_recorded_and_read_back_verbatim() {
    let dir = tempfile::tempdir().expect("temp dir");
    let case = acquired_case(dir.path());

    // Multi-line and with a trailing space: the store keeps the body verbatim, and
    // routing prose through a shell argument risks mangling it.
    let body = "First line.\r\nSecond line.   ";
    let output = cli()
        .args([
            "note",
            "--case-dir",
            case.to_str().expect("utf-8 path"),
            "--body",
            body,
            "--subject-kind",
            "asset",
            "--subject",
            "asset-1",
        ])
        .output()
        .expect("runs CLI");
    let (ok, stdout, stderr) = split(output);
    assert!(ok, "the note must be recorded: {stderr}");
    assert!(
        stdout.contains("asset"),
        "the subject is reported: {stdout}"
    );

    let recorded = notes_in(&case);
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].body, body,
        "the body must survive byte for byte"
    );
    assert_eq!(recorded[0].subject_kind.as_deref(), Some("asset"));
    assert_eq!(recorded[0].subject_id.as_deref(), Some("asset-1"));
}

#[test]
fn a_case_level_note_needs_no_subject() {
    let dir = tempfile::tempdir().expect("temp dir");
    let case = acquired_case(dir.path());

    let output = cli()
        .args([
            "note",
            "--case-dir",
            case.to_str().expect("utf-8 path"),
            "--body",
            "Client disputes the timestamp.",
        ])
        .output()
        .expect("runs CLI");
    let (ok, _, stderr) = split(output);
    assert!(ok, "a case-level note must be accepted: {stderr}");

    let recorded = notes_in(&case);
    assert_eq!(recorded.len(), 1);
    assert!(
        !recorded[0].is_attached(),
        "no subject means a case-level note"
    );
}

#[test]
fn a_note_naming_only_half_its_subject_is_refused() {
    // Accepting it would let a later reader attach the analyst's conclusion to the
    // wrong thing.
    let dir = tempfile::tempdir().expect("temp dir");
    let case = acquired_case(dir.path());
    let case_str = case.to_str().expect("utf-8 path");

    for args in [
        vec![
            "note",
            "--case-dir",
            case_str,
            "--body",
            "half",
            "--subject-kind",
            "asset",
        ],
        vec![
            "note",
            "--case-dir",
            case_str,
            "--body",
            "half",
            "--subject",
            "asset-1",
        ],
    ] {
        let output = cli().args(&args).output().expect("runs CLI");
        let (ok, _, stderr) = split(output);
        assert!(!ok, "half a subject must be refused: {stderr}");
        assert!(
            stderr.to_lowercase().contains("subject"),
            "the error must name the problem: {stderr}"
        );
    }

    assert!(
        notes_in(&case).is_empty(),
        "a refused note must not be recorded"
    );
}

#[test]
fn an_empty_note_is_refused() {
    // An empty note is indistinguishable from "the analyst wrote nothing", which
    // is a different record.
    let dir = tempfile::tempdir().expect("temp dir");
    let case = acquired_case(dir.path());

    let output = cli()
        .args([
            "note",
            "--case-dir",
            case.to_str().expect("utf-8 path"),
            "--body",
            "   ",
        ])
        .output()
        .expect("runs CLI");
    let (ok, _, stderr) = split(output);
    assert!(!ok, "an empty note must be refused: {stderr}");
    assert!(notes_in(&case).is_empty());
}

#[test]
fn a_note_json_output_is_valid_json() {
    let dir = tempfile::tempdir().expect("temp dir");
    let case = acquired_case(dir.path());

    let output = cli()
        .args([
            "--json",
            "note",
            "--case-dir",
            case.to_str().expect("utf-8 path"),
            "--body",
            "recorded",
        ])
        .output()
        .expect("runs CLI");
    let (ok, stdout, stderr) = split(output);
    assert!(ok, "{stderr}");

    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(value["note_id"].is_i64(), "the note id is reported");
    assert_eq!(value["subject_kind"], serde_json::Value::Null);
}

#[test]
fn a_note_against_a_non_case_directory_fails_with_a_diagnosable_message() {
    let dir = tempfile::tempdir().expect("temp dir");
    let output = cli()
        .args([
            "note",
            "--case-dir",
            dir.path().to_str().expect("utf-8 path"),
            "--body",
            "nowhere to put this",
        ])
        .output()
        .expect("runs CLI");
    let (ok, _, stderr) = split(output);

    assert!(!ok, "a plain directory is not a case");
    assert!(
        stderr.contains("case"),
        "the error must say what was wrong: {stderr}"
    );
}

#[test]
fn notes_survive_into_a_generated_report() {
    // The end-to-end claim: a note written through the CLI appears in the report
    // produced afterwards. Each half was separately tested; this is what proves
    // they are the same path.
    let dir = tempfile::tempdir().expect("temp dir");
    let case = acquired_case(dir.path());
    let case_str = case.to_str().expect("utf-8 path");
    let media = dir.path().join("evidence.mp4");

    let analysed = cli()
        .args([
            "analyze",
            media.to_str().expect("utf-8 path"),
            "--case-dir",
            case_str,
        ])
        .output()
        .expect("runs CLI");
    assert!(
        analysed.status.success(),
        "analyze must succeed: {}",
        String::from_utf8_lossy(&analysed.stderr)
    );

    let noted = cli()
        .args([
            "note",
            "--case-dir",
            case_str,
            "--body",
            "Client says this is the wrong master.",
        ])
        .output()
        .expect("runs CLI");
    assert!(noted.status.success(), "the note must be recorded");

    let out = dir.path().join("report.html");
    let reported = cli()
        .args([
            "report",
            "--case-dir",
            case_str,
            "--out",
            out.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("runs CLI");
    assert!(
        reported.status.success(),
        "report must succeed: {}",
        String::from_utf8_lossy(&reported.stderr)
    );

    let html = std::fs::read_to_string(&out).expect("reads report");
    assert!(
        html.contains("Client says this is the wrong master."),
        "the note must reach the report"
    );
}

#[test]
fn validate_reports_a_verdict_derived_from_the_findings() {
    // Before this command, `ValidationResult::from_findings` and
    // `Severity::fails_validation` were both implemented, documented, and unit
    // tested, and nothing on earth called either. Every report carried
    // `validation: null` and the HTML header's PASS/FAIL block could never render.
    let dir = tempfile::tempdir().expect("temp dir");
    let case = analysed_case(dir.path());

    let output = cli()
        .args(["validate", "--case-dir", case.to_str().expect("utf-8 path")])
        .output()
        .expect("runs CLI");
    let (_, stdout, stderr) = split(output);

    assert!(
        ["PASS", "PASS WITH WARNINGS", "FAIL"]
            .iter()
            .any(|label| stdout.contains(label)),
        "validate must state one of the three spec §68 verdicts: {stdout}\n{stderr}"
    );
    // The verdict must be accompanied by the findings that drove it. A bare
    // "FAIL" is not actionable and, more importantly, cannot be checked against
    // the case by anyone reading the output.
    assert!(
        stdout.contains("Findings"),
        "the verdict must state how many findings it considered: {stdout}"
    );
}

#[test]
fn validate_exits_non_zero_when_the_verdict_is_fail() {
    // A delivery gate that always exits 0 is not a gate. The exit code is the
    // part a CI pipeline actually consumes.
    let dir = tempfile::tempdir().expect("temp dir");
    let case = analysed_case(dir.path());

    let output = cli()
        .args(["validate", "--case-dir", case.to_str().expect("utf-8 path")])
        .output()
        .expect("runs CLI");
    let code = output.status.code();
    let (_, stdout, _) = split(output);

    let expected = if stdout.contains("FAIL") && !stdout.contains("PASS WITH WARNINGS") {
        // FAIL
        2
    } else {
        0
    };
    assert_eq!(
        code,
        Some(expected),
        "exit code must match the verdict.\nstdout:\n{stdout}\nexpected {expected}"
    );
}

#[test]
fn validate_emits_machine_readable_json_naming_the_blocking_findings() {
    let dir = tempfile::tempdir().expect("temp dir");
    let case = analysed_case(dir.path());

    let output = cli()
        .args([
            "--json",
            "validate",
            "--case-dir",
            case.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("runs CLI");
    let (_, stdout, stderr) = split(output);

    let value: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON ({e}): {stdout}"));
    assert!(
        value["result"].is_string(),
        "the verdict must be present in the JSON: {stdout}"
    );
    assert!(
        value["blocking"].is_array(),
        "the blocking findings must be listed so a caller can act on them: {stdout}\n{stderr}"
    );
    assert!(
        value["warnings"].is_array(),
        "warnings must be distinguishable from blocking findings: {stdout}"
    );
}

#[test]
fn validate_writes_a_bundle_only_when_asked() {
    // Off by default. A verdict is a claim about delivery, and silently adding one
    // to a report bundle would change a record the analyst did not ask to change.
    let dir = tempfile::tempdir().expect("temp dir");
    let case = analysed_case(dir.path());

    let without = cli()
        .args(["validate", "--case-dir", case.to_str().expect("utf-8 path")])
        .output()
        .expect("runs CLI");
    let (_, _, _) = split(without);
    assert!(
        !case.join("reports").join("validated").exists(),
        "validate wrote a bundle without being asked"
    );

    let with = cli()
        .args([
            "validate",
            "--case-dir",
            case.to_str().expect("utf-8 path"),
            "--write",
        ])
        .output()
        .expect("runs CLI");
    let (_, stdout, stderr) = split(with);

    let bundle = case.join("reports").join("validated");
    assert!(
        bundle.exists(),
        "--write did not produce a bundle: {stdout}\n{stderr}"
    );
    // The written bundle must actually carry the verdict, or `--write` would be
    // a flag that produces a file indistinguishable from the one it replaced.
    let rendered = bundle.join("report.html");
    if rendered.exists() {
        let html = std::fs::read_to_string(&rendered).unwrap_or_default();
        assert!(
            ["PASS", "PASS WITH WARNINGS", "FAIL"]
                .iter()
                .any(|label| html.contains(label)),
            "the written report must render the verdict: {html}"
        );
    }
}

#[test]
fn validate_rejects_a_directory_that_is_not_a_case() {
    let dir = tempfile::tempdir().expect("temp dir");
    let not_a_case = dir.path().join("plain");
    std::fs::create_dir_all(&not_a_case).expect("creates dir");

    let output = cli()
        .args([
            "validate",
            "--case-dir",
            not_a_case.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("runs CLI");
    let (ok, _, stderr) = split(output);

    assert!(!ok, "validate accepted a directory that is not a case");
    assert!(
        stderr.contains("not an initialised case"),
        "the error must say what is wrong: {stderr}"
    );
}

/// Writes spec §68's example profile and returns its path.
fn write_delivery_profile(dir: &Path, version: u32, width: u32, height: u32) -> std::path::PathBuf {
    let path = dir.join(format!("profile-v{version}.json"));
    let json = serde_json::json!({
        "name": "Client X Delivery",
        "version": version,
        "requirements": [
            { "kind": "video_codec", "any_of": ["h264"] },
            { "kind": "video_resolution", "width": width, "height": height },
            { "kind": "frame_rate", "fps": 25.0, "tolerance": 0.5 },
            { "kind": "container_format", "any_of": ["mov"] }
        ]
    });
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&json).expect("serialises"),
    )
    .expect("writes profile");
    path
}

/// Writes a 1080p 25 fps MP4 and returns its path.
fn conforming_file(dir: &Path) -> std::path::PathBuf {
    let file = dir.join("delivery.mp4");
    std::fs::write(&file, build_mp4(&TrackSpec::video_25fps(1920, 1080, 50)))
        .expect("writes fixture");
    file
}

/// Writes a 720p 25 fps MP4 and returns its path.
fn non_conforming_file(dir: &Path) -> std::path::PathBuf {
    let file = dir.join("delivery.mp4");
    std::fs::write(&file, build_mp4(&TrackSpec::video_25fps(1280, 720, 50)))
        .expect("writes fixture");
    file
}

#[test]
fn profile_template_writes_a_profile_that_actually_parses() {
    // The template exists so a customer starts from something that works. A
    // template that does not load would turn the most likely first use of `profile`
    // into an error message.
    let dir = tempfile::tempdir().expect("temp dir");
    let out = dir.path().join("delivery.json");

    let output = cli()
        .args(["profile", "template", "--out", out.to_str().expect("utf-8")])
        .output()
        .expect("runs CLI");
    let (ok, _, stderr) = split(output);
    assert!(ok, "profile template failed: {stderr}");
    assert!(out.exists(), "the template was not written");

    // And it must load through the same parser a real profile goes through.
    let checked = cli()
        .args(["profile", "check", out.to_str().expect("utf-8")])
        .output()
        .expect("runs CLI");
    let (ok, stdout, stderr) = split(checked);
    assert!(
        ok,
        "the emitted template must pass `profile check`: {stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("delivery v1"),
        "the identifier must name the version (spec §70): {stdout}"
    );
}

#[test]
fn profile_template_refuses_to_overwrite_an_existing_profile() {
    // A profile is maintained across versions (§70). Overwriting one because
    // someone asked for a template would destroy what the previous version
    // required, and a delivery judged against it could no longer be explained.
    let dir = tempfile::tempdir().expect("temp dir");
    let out = dir.path().join("delivery.json");
    let first = cli()
        .args(["profile", "template", "--out", out.to_str().expect("utf-8")])
        .output()
        .expect("runs CLI");
    assert!(first.status.success());

    let second = cli()
        .args(["profile", "template", "--out", out.to_str().expect("utf-8")])
        .output()
        .expect("runs CLI");
    let (ok, _, stderr) = split(second);
    assert!(!ok, "the second template silently overwrote the first");
    assert!(
        stderr.contains("refusing to overwrite"),
        "the refusal must say why: {stderr}"
    );
}

#[test]
fn validate_checks_a_file_against_a_profile_and_reports_each_requirement() {
    // Spec §95's invocation, and the reason `validate` gained a file mode: a QC
    // pass starts with a file and a specification, not with an analysed case.
    let dir = tempfile::tempdir().expect("temp dir");
    let file = conforming_file(dir.path());
    let profile = write_delivery_profile(dir.path(), 1, 1920, 1080);

    let output = cli()
        .args([
            "validate",
            file.to_str().expect("utf-8"),
            "--profile",
            profile.to_str().expect("utf-8"),
        ])
        .output()
        .expect("runs CLI");
    let (ok, stdout, stderr) = split(output);

    assert!(
        ok,
        "a conforming delivery must not fail: {stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("PASS"),
        "the verdict must be stated: {stdout}"
    );
    for requirement in ["video.codec", "video.resolution", "video.frame_rate"] {
        assert!(
            stdout.contains(requirement),
            "{requirement} must be reported: {stdout}"
        );
    }
    assert!(
        stdout.contains("25 (+/- 0.5)"),
        "the tolerance must be printed, not merely applied: {stdout}"
    );
}

#[test]
fn validate_fails_a_non_conforming_file_and_says_exactly_which_requirement() {
    // The output a client disputes a rejection against. A bare `FAIL` tells nobody
    // which line of their specification was missed or what the file actually is.
    let dir = tempfile::tempdir().expect("temp dir");
    let file = non_conforming_file(dir.path());
    let profile = write_delivery_profile(dir.path(), 1, 1920, 1080);

    let output = cli()
        .args([
            "validate",
            file.to_str().expect("utf-8"),
            "--profile",
            profile.to_str().expect("utf-8"),
        ])
        .output()
        .expect("runs CLI");
    let code = output.status.code();
    let (ok, stdout, stderr) = split(output);

    assert!(!ok, "a 720p delivery must not pass a 1080p profile");
    assert_eq!(
        code,
        Some(2),
        "a delivery gate that always exits 0 is not a gate.\nstdout:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("FAIL"),
        "the verdict must be stated: {stdout}"
    );
    assert!(
        stdout.contains("Expected: 1920x1080") && stdout.contains("Observed: 1280x720"),
        "both sides of the mismatch must be shown: {stdout}"
    );
    // The requirement that passed must still be listed. A report showing only the
    // failure reads as though nothing else was checked.
    assert!(
        stdout.contains("video.frame_rate"),
        "passing requirements must appear too: {stdout}"
    );
}

#[test]
fn validate_reports_an_unmeasurable_requirement_as_not_measured_and_blocks() {
    // The claim the whole three-way outcome exists to defend. A file with no audio
    // track checked against a profile requiring channels cannot be shown to meet
    // it, so it must not report PASS.
    let dir = tempfile::tempdir().expect("temp dir");
    let file = conforming_file(dir.path());

    let profile = dir.path().join("with-audio.json");
    std::fs::write(
        &profile,
        r#"{"name":"Needs audio","version":1,"requirements":[
            {"kind":"audio_channels","channels":2}
        ]}"#,
    )
    .expect("writes profile");

    let output = cli()
        .args([
            "validate",
            file.to_str().expect("utf-8"),
            "--profile",
            profile.to_str().expect("utf-8"),
        ])
        .output()
        .expect("runs CLI");
    let code = output.status.code();
    let (ok, stdout, _) = split(output);

    assert!(!ok, "an unmeasured requirement must not pass");
    assert_eq!(code, Some(2));
    assert!(
        stdout.contains("NOT MEASURED"),
        "an unmeasured requirement must say so, not read as a pass: {stdout}"
    );
    assert!(
        !stdout.contains("Observed: 0"),
        "no value was measured, so none may be printed: {stdout}"
    );
}

#[test]
fn validate_emits_json_naming_the_profile_version_and_each_requirement() {
    // The machine-readable half. A pipeline consuming this needs the exact profile
    // version (§70) and per-requirement results, not a single word.
    let dir = tempfile::tempdir().expect("temp dir");
    let file = non_conforming_file(dir.path());
    let profile = write_delivery_profile(dir.path(), 3, 1920, 1080);

    let output = cli()
        .args([
            "--json",
            "validate",
            file.to_str().expect("utf-8"),
            "--profile",
            profile.to_str().expect("utf-8"),
        ])
        .output()
        .expect("runs CLI");
    let (_, stdout, stderr) = split(output);

    let value: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON ({e}): {stdout}"));

    assert_eq!(value["result"], "FAIL");
    assert_eq!(value["profile"]["version"], 3, "the version must travel");
    assert_eq!(value["profile"]["identifier"], "client-x-delivery v3");

    let requirements = value["requirements"]
        .as_array()
        .unwrap_or_else(|| panic!("requirements must be an array: {stdout}\n{stderr}"));
    let resolution = requirements
        .iter()
        .find(|r| r["id"] == "video.resolution")
        .unwrap_or_else(|| panic!("no video.resolution entry: {stdout}"));
    assert_eq!(resolution["outcome"], "NOT MET");
    assert_eq!(resolution["expected"], "1920x1080");
    assert_eq!(resolution["observed"], "1280x720");

    // A file check ran no analysis. Saying `0` findings would imply a clean one.
    assert!(
        value["findings_considered"].is_null(),
        "a file check must not report a finding count: {stdout}"
    );
}

#[test]
fn validate_writes_a_bundle_only_when_asked_and_the_bundle_carries_the_verdict() {
    // Off by default: a verdict is a claim about delivery, and silently adding one
    // to a report bundle would change a record the analyst did not ask to change.
    let dir = tempfile::tempdir().expect("temp dir");
    let file = non_conforming_file(dir.path());
    let profile = write_delivery_profile(dir.path(), 1, 1920, 1080);
    let bundle = file.with_extension("validated");

    let without = cli()
        .args([
            "validate",
            file.to_str().expect("utf-8"),
            "--profile",
            profile.to_str().expect("utf-8"),
        ])
        .output()
        .expect("runs CLI");
    let _ = split(without);
    assert!(
        !bundle.exists(),
        "validate wrote a bundle without being asked"
    );

    let with = cli()
        .args([
            "validate",
            file.to_str().expect("utf-8"),
            "--profile",
            profile.to_str().expect("utf-8"),
            "--write",
        ])
        .output()
        .expect("runs CLI");
    let (_, _, stderr) = split(with);

    assert!(bundle.exists(), "--write produced no bundle: {stderr}");
    let html = std::fs::read_to_string(bundle.join("case-report.html")).expect("reads HTML");
    assert!(
        html.contains("FAIL"),
        "the written report must render the verdict: {html}"
    );
    assert!(
        html.contains("video.resolution") && html.contains("1280x720"),
        "the written report must carry the requirement table: {html}"
    );
}

#[test]
fn a_case_validated_against_a_profile_combines_both_halves_of_the_verdict() {
    // A file can meet its specification and still carry a significant finding. The
    // verdict has to be the worse of the two, or a clean profile check would paper
    // over a damaged container.
    let dir = tempfile::tempdir().expect("temp dir");
    let case = analysed_case(dir.path());
    // A profile the analysed file comfortably meets, so the findings decide.
    let profile = write_delivery_profile(dir.path(), 1, 320, 240);

    let output = cli()
        .args([
            "--json",
            "validate",
            "--case-dir",
            case.to_str().expect("utf-8"),
            "--profile",
            profile.to_str().expect("utf-8"),
        ])
        .output()
        .expect("runs CLI");
    let (_, stdout, stderr) = split(output);

    let value: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON ({e}): {stdout}"));
    assert!(value["profile"].is_object(), "the profile must be applied");
    assert!(
        value["findings_considered"].is_number(),
        "the severity half must still be considered: {stdout}\n{stderr}"
    );
    // Whatever the findings are, the two halves must agree on one verdict: a
    // requirements table saying everything passed beside a FAIL from severities is
    // the confusion this combination exists to prevent.
    let requirements_all_met = value["requirements"]
        .as_array()
        .expect("requirements")
        .iter()
        .all(|r| r["outcome"] == "MET");
    let severity_failed = value["result"] == "FAIL";
    assert!(
        !(requirements_all_met && severity_failed),
        "requirements all met but the verdict is FAIL with no blocking finding: {stdout}"
    );
}

#[test]
fn validate_rejects_a_profile_that_does_not_parse_with_a_reason() {
    // The most likely thing to go wrong with a hand-written profile (§69), and the
    // error has to say more than "invalid".
    let dir = tempfile::tempdir().expect("temp dir");
    let file = conforming_file(dir.path());

    let profile = dir.path().join("broken.json");
    // A frame rate with no tolerance: rejected rather than defaulted.
    std::fs::write(
        &profile,
        r#"{"name":"x","version":1,"requirements":[{"kind":"frame_rate","fps":25.0}]}"#,
    )
    .expect("writes profile");

    let output = cli()
        .args([
            "validate",
            file.to_str().expect("utf-8"),
            "--profile",
            profile.to_str().expect("utf-8"),
        ])
        .output()
        .expect("runs CLI");
    let (ok, _, stderr) = split(output);

    assert!(
        !ok,
        "a profile with no tolerance must not load with an assumed one"
    );
    assert!(
        stderr.contains("tolerance"),
        "the error must say what is wrong: {stderr}"
    );
}

#[test]
fn help_lists_the_profile_subcommand() {
    let output = cli().arg("--help").output().expect("runs CLI");
    let (_, stdout, _) = split(output);
    assert!(
        stdout.contains("profile"),
        "`profile` is missing from the help output"
    );
}
