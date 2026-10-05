//! Golden tests: what a *correct* file must produce (spec §77).
//!
//! # Why these exist alongside the property and fuzz tests
//!
//! The robustness harness asserts that nothing panics. That is necessary and it is
//! not sufficient: a reader can never crash and still quietly misread a perfectly
//! valid file, and that is the failure mode that costs a forensic product its
//! credibility. The VINT bug the harness found is exactly that shape — in release
//! it did not crash at all, it read every 8-byte VINT as carrying a full byte of
//! value bits.
//!
//! A golden test answers the complementary question: given a file we *know* is
//! well-formed, does the engine report what we expect? It pins the answer, so a
//! change in reading becomes a visible diff rather than a silent re-interpretation
//! of somebody's evidence.
//!
//! # Why three categories, and why they are rendered by hand
//!
//! `todo.md` named "metadata, structure, findings". Each is pinned, and each is
//! rendered by an explicit function rather than by `Debug` or `serde`:
//!
//! - `serde` would pin the *struct layout*. Adding a field would fail every
//!   golden for a reason that is not a behaviour change, which trains a reviewer
//!   to accept golden churn — the same way a gap list full of solved problems
//!   trains a reader to stop reading it.
//! - `Debug` would pin formatting, including anything derived from a path or a
//!   clock.
//!
//! So each renderer lists the fields it considers part of the claim. A field that
//! is not yet in a golden is a decision not yet made, which is visible as an
//! omission rather than hidden inside a blob.
//!
//! # What is deliberately *not* pinned
//!
//! Filesystem timestamps, source paths, cache keys, and evidence file locations.
//! Those vary between machines and runs; a golden containing them would be
//! permanently red and would eventually be ignored. The fixtures themselves are
//! deterministic by construction (spec §77), which is what makes the rest of it
//! possible.
//!
//! # Updating a golden
//!
//! `UPDATE_GOLDENS=1 cargo test --test golden`. The test still *runs* and still
//! asserts nothing about correctness in that mode, so a regeneration can never be
//! mistaken for a pass. A reviewer reads the diff: a changed golden is a claim
//! about behaviour that somebody has to agree with.

use std::path::{Path, PathBuf};

use tpt_app_media_forensics_container::fixture::{
    build_mp4, build_mp4_av, build_mp4_with_bitrate_drop, build_mp4_with_colour,
    build_mp4_with_metadata, build_mp4_with_reordered_frames, build_mp4_with_repeated_frames,
    build_mp4_with_wrong_declared_duration, build_webm, TrackSpec,
};
use tpt_app_media_forensics_core::case_dir::CaseDirectory;
use tpt_app_media_forensics_core::AnalysisEngine;
use tpt_app_media_forensics_model::Case;

/// Directory holding the committed expectations.
fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// Whether this run should rewrite the goldens rather than compare against them.
fn updating() -> bool {
    std::env::var("UPDATE_GOLDENS").is_ok_and(|v| v == "1")
}
/// Compares `actual` against the committed golden for `name`, or writes it.
///
/// The failure message prints the first differing line rather than both files in
/// full: a golden diff is a list of claims, and the line that changed *is* the
/// claim that changed. Dumping four kilobytes of structure to find one altered
/// resolution is how a reviewer ends up skimming.
fn check(name: &str, actual: &str) {
    let path = golden_dir().join(format!("{name}.txt"));
    if updating() {
        std::fs::create_dir_all(golden_dir()).expect("creates the golden directory");
    }
    // Existence, not emptiness. A golden that is legitimately empty — a clean
    // file producing no findings — is a real and important claim, and treating an
    // empty file as "missing" would leave those two tests permanently red, which is
    // how a test gets ignored rather than fixed.
    //
    // Two goldens here *are* empty on purpose: `findings-clean` and
    // `findings-sdr-colour`, both asserting that a healthy file produces nothing.
    if !path.exists() {
        panic!(
            "no golden for `{name}`. Run `UPDATE_GOLDENS=1 cargo test --test golden` to \
             create it, then read the result: a new golden is a claim, not a formality."
        );
    }

    let expected = std::fs::read_to_string(&path).expect("reads the golden");

    if updating() {
        std::fs::write(&path, actual).expect("writes the golden");
        return;
    }

    if expected != actual {
        let first_difference = expected
            .lines()
            .zip(actual.lines())
            .enumerate()
            .find(|(_, (want, got))| want != got)
            .map(|(line, (want, got))| {
                format!(
                    "  first difference at line {}:\n    expected: {want}\n      actual: {got}",
                    line + 1
                )
            })
            .unwrap_or_else(|| {
                format!(
                    "  same lines but different content ({} expected vs {} actual)",
                    expected.lines().count(),
                    actual.lines().count()
                )
            });
        panic!("`{name}` no longer matches its golden.\n{first_difference}");
    }
}

/// Renders what the container reader claims about a file.
///
/// Structure only, and only the fields a report would print. Deliberately absent:
/// byte offsets and `mdat` contents — those are the reader's business, not a claim
/// about the file's structure.
fn render_structure(bytes: Vec<u8>) -> String {
    // Dispatch on the detected format rather than assuming ISO-BMFF. The first
    // version of this renderer called the MP4 reader unconditionally, which made
    // the Matroska golden pin "the MP4 reader rejects a WebM file" — technically
    // true, worth nothing, and the exact failure this file exists to prevent: a
    // golden that passes without testing anything.
    let inspection = match tpt_app_media_forensics_container::probe::detect(&bytes) {
        tpt_app_media_forensics_container::probe::ContainerFormat::Matroska => {
            match tpt_app_media_forensics_container::inspect_matroska_bytes(bytes) {
                Ok(inspection) => inspection,
                Err(error) => return format!("unreadable: {error}\n"),
            }
        }
        _ => match tpt_app_media_forensics_container::inspect_bytes(bytes) {
            Ok(inspection) => inspection,
            Err(error) => return format!("unreadable: {error}\n"),
        },
    };

    let mut out = format!("format {}\n", inspection.format.tag());
    out.push_str(&format!(
        "declared_track_count {}\n",
        inspection.declared_track_count
    ));
    out.push_str(&format!(
        "declared_next_track_id {}\n",
        inspection
            .declared_next_track_id
            .map_or_else(|| "none".to_owned(), |id| id.to_string())
    ));

    for stream in &inspection.streams {
        out.push_str(&format!(
            "stream {} kind={} codec={} packets={} language={}\n",
            stream.index,
            stream.kind.tag(),
            stream.codec.name,
            stream
                .packet_count
                .map_or_else(|| "none".to_owned(), |count| count.to_string()),
            stream.language.as_deref().unwrap_or("none"),
        ));
        if let Some(video) = stream.video_format() {
            // Coded dimensions and the display size after crop are both pinned:
            // a 1920x1088 coded frame displayed as 1080p is the case the reader
            // exists to get right, and collapsing it to one number would hide
            // which of the two it reported.
            out.push_str(&format!(
                "  video coded={}x{} display={}x{} fps={} sar={} rotation={} hdr={}\n",
                video.coded_width,
                video.coded_height,
                video.display_width.unwrap_or(video.coded_width),
                video.display_height.unwrap_or(video.coded_height),
                video
                    .frame_rate
                    .map_or_else(|| "none".to_owned(), |r| r.to_string()),
                video
                    .sample_aspect_ratio
                    .map_or_else(|| "none".to_owned(), |r| r.to_string()),
                video
                    .rotation_degrees
                    .map_or_else(|| "none".to_owned(), |d| d.to_string()),
                video.is_hdr
            ));
        }
        if let Some(audio) = stream.audio_format() {
            out.push_str(&format!(
                "  audio {} Hz, {}-bit, layout={:?} channels={}\n",
                audio.sample_rate,
                audio.bit_depth,
                audio.channel_layout,
                audio.channel_count()
            ));
        }
    }

    for anomaly in &inspection.anomalies {
        out.push_str(&format!("anomaly {anomaly}\n"));
    }
    out
}

/// Renders the metadata tree, in the order the store would persist it.
///
/// Scope, key, value, and the element the value was read from. The source element
/// is pinned because it is the difference between "the file says 2024" and "the
/// `mvhd` box says 2024" — a reader that started taking creation time from a
/// different box would otherwise be invisible here.
fn render_metadata(bytes: Vec<u8>) -> String {
    let dir = tempfile::tempdir().expect("scratch");
    let path = dir.path().join("sample.mp4");
    std::fs::write(&path, bytes).expect("writes source");

    let (bundle, _limitations) = AnalysisEngine::new().observe_stages(&path);
    let mut out = String::new();
    let Some(tree) = bundle.metadata.as_ref() else {
        return "no metadata tree was produced\n".to_owned();
    };
    for entry in &tree.entries {
        out.push_str(&format!(
            "{} {} = {:?} from {}\n",
            entry.scope.tag(),
            entry.key,
            entry.value,
            entry.source
        ));
    }
    out
}

/// Renders what the engine concluded, without the measurements behind it.
///
/// Rule, severity, confidence, and timeline placement — the four things a reader
/// acts on. Measurement strings are excluded because they embed formatted floats,
/// and a change in how a number prints is a rendering change, not a change in what
/// was found. The float *values* are pinned by the existing rule tests.
fn render_findings(name: &str, bytes: Vec<u8>) -> String {
    let dir = tempfile::tempdir().expect("scratch");
    let source = dir.path().join(name);
    std::fs::write(&source, bytes).expect("writes source");

    let case_root = dir.path().join("case.tptcase");
    CaseDirectory::create(&case_root, &Case::new("Golden".to_owned(), None))
        .expect("creates the case");

    let opened = CaseDirectory::open(&case_root).expect("opens the case");
    let outcome = match AnalysisEngine::new().analyse(&source, &opened) {
        Ok(outcome) => outcome,
        Err(error) => return format!("unanalysable: {error}\n"),
    };

    let mut out = String::new();
    for finding in &outcome.findings {
        out.push_str(&format!(
            "{} {} {} {}\n",
            finding.rule_id,
            finding.severity.tag(),
            finding.confidence.tag(),
            finding
                .timeline_start
                .map_or_else(|| "-".to_owned(), |t| t.to_timecode())
        ));
    }
    out
}

/// One WebM document with six distinct blocks, for the structure golden.
fn webm() -> Vec<u8> {
    let blocks: Vec<(u16, bool, Vec<u8>)> = (0..6u16)
        .map(|index| {
            let mut payload = vec![0u8; 32];
            payload[0] = index as u8;
            payload[1] = (index as u8).wrapping_mul(7);
            (index * 40, index % 4 == 0, payload)
        })
        .collect();
    build_webm("V_VP9", 1, &blocks)
}

// ---------------------------------------------------------------------------
// Structure
// ---------------------------------------------------------------------------

#[test]
fn a_plain_video_track_structure_is_stable() {
    check(
        "structure-plain-video",
        &render_structure(build_mp4(&TrackSpec::video_25fps(640, 480, 50))),
    );
}

#[test]
fn an_audio_video_structure_is_stable() {
    // The only fixture with two streams and a real audio sample entry, so it is the
    // one that would catch the reader reading audio declarations at the video
    // entry's offsets — the defect the audio sample entry was written to fix.
    check(
        "structure-audio-video",
        &render_structure(build_mp4_av(
            &TrackSpec::video_25fps(640, 480, 100),
            &TrackSpec::audio_48khz(2_400),
            40,
        )),
    );
}

#[test]
fn a_matroska_structure_is_stable() {
    // The VINT reader. The harness proved it no longer panics; this proves it still
    // reads ordinary documents correctly, which the panic test cannot show.
    check("structure-matroska", &render_structure(webm()));
}

#[test]
fn a_reordered_structure_is_stable() {
    // Composition offsets change what the reader reports about timing without
    // changing the file's shape, so the structure must not move.
    check(
        "structure-reordered",
        &render_structure(build_mp4_with_reordered_frames(40)),
    );
}

#[test]
fn a_wrong_declared_duration_structure_is_stable() {
    // A duration the sample table does not support. The reader must report what the
    // header says, not what it infers.
    check(
        "structure-wrong-duration",
        &render_structure(build_mp4_with_wrong_declared_duration()),
    );
}

#[test]
fn an_unreadable_file_is_stable() {
    // What the engine says about bytes that are not media at all. This is a claim
    // too: "unreadable" and "readable but empty" are different, and a reviewer
    // relies on the distinction.
    check(
        "structure-unreadable",
        &render_structure(b"not media at all".to_vec()),
    );
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

#[test]
fn the_metadata_tree_is_stable() {
    check(
        "metadata-text-atoms",
        &render_metadata(build_mp4_with_metadata(&TrackSpec::video_25fps(
            640, 480, 50,
        ))),
    );
}

#[test]
fn a_file_with_no_metadata_reads_as_empty() {
    // Pinned separately from the positive case because "no metadata" and "metadata
    // we failed to read" both render as an empty tree, and that is a distinction
    // the report makes elsewhere. Pinning the empty case makes the coincidence
    // visible in review instead of latent.
    check(
        "metadata-empty",
        &render_metadata(build_mp4(&TrackSpec::video_25fps(640, 480, 50))),
    );
}

// ---------------------------------------------------------------------------
// Findings
// ---------------------------------------------------------------------------

#[test]
fn a_clean_file_produces_no_findings() {
    // The strongest single golden in this file. A file with no defects must produce
    // nothing, and every rule added later has to keep this true.
    check(
        "findings-clean",
        &render_findings(
            "clean.mp4",
            build_mp4(&TrackSpec::video_25fps(640, 480, 50)),
        ),
    );
}

#[test]
fn a_repeated_frame_finding_is_stable() {
    check(
        "findings-repeated-frames",
        &render_findings("repeated.mp4", build_mp4_with_repeated_frames(10, 15)),
    );
}

#[test]
fn a_bitrate_drop_finding_is_stable() {
    check(
        "findings-bitrate-drop",
        &render_findings("bitrate.mp4", build_mp4_with_bitrate_drop(60, 40)),
    );
}

#[test]
fn a_reordering_finding_is_stable() {
    check(
        "findings-reordered",
        &render_findings("reordered.mp4", build_mp4_with_reordered_frames(40)),
    );
}

#[test]
fn an_hdr_finding_is_stable() {
    check(
        "findings-hdr-missing",
        &render_findings(
            "hdr.mp4",
            tpt_app_media_forensics_container::build_mp4_with_hdr_signalling_only(),
        ),
    );
}

#[test]
fn a_colour_complete_file_produces_no_findings() {
    // The negative control for the HDR case. Both files are colour-signalled; only
    // one is incomplete, and the rule must tell them apart.
    check(
        "findings-sdr-colour",
        &render_findings("sdr.mp4", build_mp4_with_colour()),
    );
}
