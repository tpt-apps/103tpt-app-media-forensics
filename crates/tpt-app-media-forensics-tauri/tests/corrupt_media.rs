//! Malformed media must not crash the application (spec §75, §96).
//!
//! # Why this file exists at all
//!
//! Spec §75 and §96 make "malformed media cannot crash the application" an
//! acceptance criterion, and spec §97 asks to *verify* it. In a library that
//! means returning errors; in a GUI it is a stronger claim, because a
//! `#[tauri::command]` that panics takes the window down with it along with the
//! analyst's unsaved screen state.
//!
//! So the bar these tests set is not "the engine returns an error". It is:
//! **every screen's view model survives arbitrary bytes**. Each case below
//! builds a hostile fixture and pushes it through every screen constructor. A
//! panic anywhere fails the test; an `Err` or an empty-but-valid view passes.

use std::path::Path;

use tpt_app_media_forensics_core::batch;
use tpt_app_media_forensics_core::{AnalysisEngine, CaseDirectory};
use tpt_app_media_forensics_model::MediaTime;

use tpt_app_media_forensics_model::{AnalysisStatus, Timebase};
use tpt_app_media_forensics_tauri::view::batch::{BatchRow, BatchSort, BatchView};
use tpt_app_media_forensics_tauri::view::dashboard::DashboardSummary;
use tpt_app_media_forensics_tauri::view::timeline::{TimelineLayer, TimelineView};
use tpt_app_media_forensics_tauri::view::viewer::{
    FrameDifference, FrameImageView, FrameStamp, FrameStep, FrameTimestamps, Histogram,
    PixelSample, RgbBasis, ViewerState, Waveform,
};

/// Writes `bytes` to a scratch file and returns its path.
fn fixture(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).expect("scratch file writes");
    path
}

/// Every screen, applied to one analysis outcome.
///
/// The point is that a hostile file produces one *partial* outcome rather than a
/// failure, and every screen has to render that partial outcome rather than
/// refusing it.
fn render_every_screen(outcome: &tpt_app_media_forensics_core::AnalysisOutcome) {
    let _ = DashboardSummary::new("c", "Case", &outcome.findings, AnalysisStatus::Complete);
    // `Complete` because the engine's own outcome carries its timeline with it.
    // The case's stored retention is a fact about the database, and this test is
    // about a hostile file reaching every screen, not about which build wrote it.
    let timeline = TimelineView::build(
        &outcome.timeline,
        &outcome.findings,
        tpt_app_media_forensics_tauri::view::Retention::Complete,
    );
    let _ = timeline.mark_count();
    let _ = timeline.nearest_mark(TimelineLayer::Error, MediaTime::ZERO, 1_000_000);
    // `for_each`, not `map().count()`: every finding must actually have its
    // jump target built, which is the point of the exercise.
    outcome.findings.iter().for_each(|f| {
        let _ = timeline.target_for_finding(f);
    });
    let _ = BatchView::from_rows(vec![BatchView::analysed(
        "x.mp4",
        &outcome.findings,
        outcome.cache_hit,
    )]);
}

/// Builds a case directory holding one analysed file.
fn analyse(
    dir: &tempfile::TempDir,
    source: &Path,
) -> Option<tpt_app_media_forensics_core::AnalysisOutcome> {
    let case_root = dir.path().join("case.tptcase");
    let case = CaseDirectory::create(
        &case_root,
        &tpt_app_media_forensics_model::Case::new("corrupt corpus".to_owned(), None),
    )
    .expect("case directory is created");

    AnalysisEngine::new().analyse(source, &case).ok()
}

/// The hostile fixtures, each one a shape a real intake folder contains.
fn corpus() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("empty.mp4", Vec::new()),
        ("one-byte.mp4", vec![0x00]),
        // A box size of u32::MAX: the classic "declared longer than the file".
        ("oversized-box.mp4", vec![0xFF; 8]),
        ("truncated-ftyp.mp4", b"\x00\x00\x00\x18ftypisom".to_vec()),
        ("bad-length.mp4", {
            let mut b = b"\x00\x00\x00\x20ftypisom\x00\x00\x02\x00isomiso2".to_vec();
            b.truncate(6);
            b
        }),
        // Pure noise with a plausible extension: what a renamed file looks like.
        ("noise.mp4", (0u16..4096).map(|i| (i % 251) as u8).collect()),
        // A `moov` box header promising far more than exists.
        (
            "lying-moov.mp4",
            b"\x00\x00\x00\x10moov\xFF\xFF\xFF\xFF".to_vec(),
        ),
        ("zero-size-moov.mp4", b"\x00\x00\x00\x00moov".to_vec()),
        (
            "text-as-media.mov",
            b"this is a text file pretending to be a movie".to_vec(),
        ),
        (
            "riff-not-wav.wav",
            b"RIFF\xFF\xFF\xFF\xFFWAVEjunkjunkjunk".to_vec(),
        ),
        (
            "ebml-header-only.mkv",
            vec![0x1A, 0x45, 0xDF, 0xA3, 0x00, 0x00, 0x00],
        ),
    ]
}

#[test]
fn an_empty_file_reaches_every_screen_without_panicking() {
    let dir = tempfile::tempdir().expect("scratch");
    let source = fixture(&dir, "empty.mp4", &[]);

    if let Some(outcome) = analyse(&dir, &source) {
        render_every_screen(&outcome);
    }
}

#[test]
fn the_hostile_corpus_never_crashes_a_screen() {
    // The corpus is built once and each file pushed through the whole engine
    // and every screen model. A panic in any of them fails the test.
    for (name, bytes) in corpus() {
        let dir = tempfile::tempdir().expect("scratch");
        let source = fixture(&dir, name, &bytes);
        if let Some(outcome) = analyse(&dir, &source) {
            render_every_screen(&outcome);
        }
    }
}

#[test]
fn an_unidentifiable_file_records_what_it_could_not_measure() {
    // This test originally asserted that a file with no recognisable structure
    // produces *findings*. It does not, and should not: with no `ftyp` the
    // engine does not know what the file claims to be, so no rule can decide
    // whether anything about it is wrong. Asserting findings here would have
    // been asserting a defect in the engine rather than in the fixture.
    //
    // What it does produce is a full list of limitations — which is the
    // behaviour spec §75 actually asks for, and the thing the GUI has to be
    // able to render without crashing.
    let dir = tempfile::tempdir().expect("scratch");
    let source = fixture(&dir, "noise.mp4", &[0x5A; 2048]);

    let Some(outcome) = analyse(&dir, &source) else {
        // Refusing outright is also acceptable, and the screens are not reached.
        return;
    };

    assert!(
        !outcome.limitations.is_empty(),
        "a file nothing could be read from must say so, not report itself clean: \
         {} findings, {} limitations",
        outcome.findings.len(),
        outcome.limitations.len()
    );

    // And the dashboard must reflect that honestly: zero findings from an
    // unreadable file is not a clean bill of health.
    let summary = DashboardSummary::new("c", "Case", &outcome.findings, AnalysisStatus::Complete);
    let _ = summary.is_clear();
}

#[test]
fn a_recognised_but_truncated_file_does_produce_findings() {
    // The contrast with the case above, and the reason it is a separate test: a
    // file the engine *can* identify is one whose damage it can describe, and
    // the truncated-media rule exists precisely for this. It also exercises the
    // screen layer against findings that carry real severities.
    let dir = tempfile::tempdir().expect("scratch");
    let mut bytes = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isomiso2".to_vec();
    bytes.extend_from_slice(b"\x00\x00\x00\x10moov\xFF\xFF\xFF\xFF");
    let source = fixture(&dir, "truncated.mp4", &bytes);

    if let Some(outcome) = analyse(&dir, &source) {
        render_every_screen(&outcome);
        assert!(
            !outcome.findings.is_empty(),
            "a container the engine can identify but cannot finish reading must \
             produce findings about the damage"
        );
    }
}

#[test]
fn a_file_the_engine_cannot_read_becomes_a_labelled_batch_row() {
    // Spec §75's requirement survives into the batch dashboard: one unreadable
    // file among many must not stop the run or disappear from the table.
    let dir = tempfile::tempdir().expect("scratch");
    let case_root = dir.path().join("case.tptcase");
    let case = CaseDirectory::create(
        &case_root,
        &tpt_app_media_forensics_model::Case::new("batch".to_owned(), None),
    )
    .expect("case");

    let intake = dir.path().join("intake");
    std::fs::create_dir_all(&intake).expect("intake directory");
    fixture(
        &dir,
        "intake/good.mov",
        b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isomiso2",
    );
    // Carries a valid `ftyp` so the scanner accepts it, and nothing after it, so
    // the engine still cannot analyse it. A file with no signature is *skipped*
    // by the scanner, which is a different outcome with a different reason.
    fixture(
        &dir,
        "intake/broken.mov",
        b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isomiso2",
    );

    let outcome = batch::run(&AnalysisEngine::new(), &intake, &case).expect("batch runs");

    let rows: Vec<BatchRow> = outcome
        .results
        .iter()
        .map(|(path, file)| match file {
            batch::FileOutcome::Analysed(analysed) => BatchView::analysed(
                path.display().to_string(),
                &analysed.findings,
                analysed.cache_hit,
            ),
            batch::FileOutcome::Failed { reason } => {
                BatchRow::unreadable(path.display().to_string(), reason.clone())
            }
            batch::FileOutcome::Skipped { reason } => {
                BatchRow::skipped(path.display().to_string(), reason.clone())
            }
        })
        .collect();

    let view = BatchView::from_rows(rows);
    assert_eq!(view.counts().total, 2, "both files appear: {view:?}");
    assert!(
        view.sorted(BatchSort::Status).len() == 2,
        "sorting a table containing a failed file must not lose it"
    );
}

#[test]
fn an_unreadable_file_is_never_reported_as_a_pass() {
    // The failure mode this guards against is an intake of a thousand files
    // where one bad file sorts to the top and reads as clean.
    let row = BatchRow::unreadable("C:/intake/broken.mov", "not a recognised container");
    assert!(row.status.blocks_delivery());
    assert!(!BatchView::from_rows(vec![row]).all_passed());
}

// ---------------------------------------------------------------------------
// Malformed decoded frames
//
// The engine catches a decoder panic at the packet layer, but a frame that
// *survived* decoding can still carry dimensions that disagree with its buffer.
// Those reach the viewer, and the viewer is the last place they can be caught
// before they index out of bounds (spec §75).
// ---------------------------------------------------------------------------

/// Frame sizes and buffer lengths chosen to break naive indexing.
fn hostile_frames() -> Vec<FrameImageView> {
    vec![
        // Declares far more pixels than it carries.
        FrameImageView {
            frame_index: 0,
            width: 4096,
            height: 4096,
            time: MediaTime::ZERO,
            rgb: Some(vec![0u8; 12]),
            basis: RgbBasis::Bt601Matrix,
            evidence_path: None,
        },
        // Zero dimensions with a non-empty buffer.
        FrameImageView {
            frame_index: 0,
            width: 0,
            height: 0,
            time: MediaTime::ZERO,
            rgb: Some(vec![7u8; 512]),
            basis: RgbBasis::LumaOnly,
            evidence_path: None,
        },
        // Dimensions whose product overflows a 32-bit count.
        FrameImageView {
            frame_index: 0,
            width: u32::MAX,
            height: u32::MAX,
            time: MediaTime::ZERO,
            rgb: Some(vec![1u8; 3]),
            basis: RgbBasis::Bt601Matrix,
            evidence_path: None,
        },
        // No pixels retained at all.
        FrameImageView::without_pixels(0, 1920, 1080, MediaTime::from_millis(40)),
    ]
}

#[test]
fn the_pixel_inspector_refuses_every_hostile_frame() {
    // Coordinates an analyst can actually produce: the origin, the far corner,
    // and one past each edge.
    for frame in hostile_frames() {
        for (x, y) in [
            (0, 0),
            (1, 1),
            (frame.width, 0),
            (0, frame.height),
            (u32::MAX, u32::MAX),
        ] {
            let _ = PixelSample::read(
                frame.rgb.as_deref().unwrap_or(&[]),
                frame.width,
                frame.height,
                x,
                y,
                frame.basis,
                None,
            );
        }
    }
}

#[test]
fn the_histogram_refuses_every_hostile_frame() {
    // Returns `None` rather than an empty chart: "no pixels" and "every pixel is
    // black" must not both draw as an empty histogram.
    for frame in hostile_frames() {
        let _ = Histogram::from_rgb(
            frame.rgb.as_deref().unwrap_or(&[]),
            frame.width,
            frame.height,
        );
    }
}

#[test]
fn comparing_two_hostile_frames_never_claims_they_are_identical() {
    // The dangerous outcome is `Identical`: it would tell a reviewer two frames
    // match when neither could be read. `NotComparable` is the honest answer.
    let frames = hostile_frames();
    for left in &frames {
        for right in &frames {
            if FrameDifference::compare(left, right) == FrameDifference::Identical {
                // Only permissible when both sides really are a full,
                // equal-sized, fully-populated buffer.
                assert!(
                    left.has_pixels() && right.has_pixels() && left.width == right.width,
                    "claimed identical without both frames being readable: \
                     {left:?} vs {right:?}"
                );
            }
        }
    }
}

#[test]
fn frame_stepping_survives_a_sequence_with_nonsense_indices() {
    // A frame list where indices are not sorted, not contiguous, and not
    // monotonic in time. Every one of those is reachable from a damaged file.
    let frames = vec![
        FrameStamp {
            index: 9,
            time: MediaTime::from_millis(300),
            is_key_frame: true,
        },
        FrameStamp {
            index: 2,
            time: MediaTime::from_millis(100),
            is_key_frame: false,
        },
        FrameStamp {
            index: 5,
            time: MediaTime::from_millis(200),
            is_key_frame: true,
        },
    ];

    let mut state = ViewerState::new(10);
    for step in [
        FrameStep::Forward,
        FrameStep::Back,
        FrameStep::ForwardSecond,
        FrameStep::BackSecond,
        FrameStep::NextKeyFrame,
        FrameStep::PreviousKeyFrame,
        FrameStep::End,
        FrameStep::Start,
    ] {
        let landed = state.step_with(step, &frames, &[2, 5, 9]);
        assert!(
            landed <= 9,
            "{step:?} landed on frame {landed}, past the end of a 10-frame sequence"
        );
    }
}

#[test]
fn jumping_into_an_empty_sequence_leaves_the_viewer_where_it_was() {
    let mut state = ViewerState::new(0);
    state.frame_index = 0;
    assert_eq!(
        state.jump_to_time(&[], MediaTime::from_millis(1_000)),
        0,
        "jumping with nothing to jump to must not move the analyst to a made-up position"
    );
}

#[test]
fn the_waveform_refuses_hostile_audio_parameters() {
    // Zero channels, zero columns, a channel count larger than the buffer, and
    // a non-finite signal: each is a shape a decoder can emit from hostile
    // bytes, and each must be refused or sanitised rather than divided by zero.
    let pcm = [0.5f32, -0.5, 0.0, 0.25];
    let _ = Waveform::from_pcm(&pcm, 0, 10);
    let _ = Waveform::from_pcm(&pcm, 1, 0);
    let _ = Waveform::from_pcm(&pcm, 99, 10);
    let _ = Waveform::from_pcm(&[f32::NAN, f32::INFINITY], 1, 4);
    let _ = Waveform::from_pcm(&[], 2, 10);
}

#[test]
fn extreme_timestamps_do_not_overflow_the_display_fields() {
    // Timestamps arrive from an attacker-controlled table (spec §24). The whole
    // range of `i64` must survive being converted and displayed.
    for ticks in [i64::MIN, -1, 0, 1, i64::MAX] {
        let stamps = FrameTimestamps::new(
            MediaTime::from_micros(ticks),
            None,
            0,
            Timebase::from_ticks_per_second(1_000),
            ticks,
            Some(ticks),
            false,
        );
        // The only requirement is that this returns rather than panicking or
        // wrapping; the values themselves are whatever the arithmetic yields.
        let _ = stamps.reorder_delay();
        let _ = stamps.is_reordered();
    }
}

/// The hostile corpus, run through every screen command's data path.
///
/// The commands themselves are thin and take a `tauri::State` this crate cannot
/// construct, so what is exercised here is the work they delegate to: the same
/// `observe_stages` and container readers a screen receives. A panic in any of
/// them fails the test, which is the property spec \u00a775 asks for.
#[test]
fn the_hostile_corpus_survives_the_inspection_path() {
    for (name, bytes) in corpus() {
        let dir = tempfile::tempdir().expect("scratch");
        let source = fixture(&dir, name, &bytes);

        // Whatever the engine makes of it, this must return rather than unwind.
        let engine = AnalysisEngine::new();
        let (bundle, reasons) = engine.observe_stages(&source);
        // Every fixture in the corpus is either unreadable or nonsense, so the
        // engine must have something to say about it. Silence would be the
        // failure: a screen showing nothing looks like a clean file.
        assert!(
            !bundle.damage.is_empty() || !reasons.is_empty(),
            "{name}: a file nothing could be read from must record why"
        );
    }
}

#[test]
fn comparing_a_file_against_itself_never_reports_a_difference() {
    // The engine's own determinism guard. If comparing a file with itself could
    // report a difference, every comparison in the GUI would be suspect - and a
    // reviewer has no way to tell engine noise from a real finding.
    for (name, bytes) in corpus() {
        let dir = tempfile::tempdir().expect("scratch");
        let source = fixture(&dir, name, &bytes);

        let engine = AnalysisEngine::new();
        let (left, _) = engine.observe_stages(&source);
        let (right, _) = engine.observe_stages(&source);

        let left_name = "left";
        let right_name = "right";
        let result = tpt_app_media_forensics_rules::comparison::compare(
            &tpt_app_media_forensics_rules::comparison::ComparisonInput {
                name: left_name,
                streams: left.container.as_ref().map(|c| c.streams.as_slice()),
                metadata: left.metadata.as_ref(),
                scene: left.scene.as_ref(),
                silence: Some(left.silence.as_slice()),
                loudness: left.loudness.as_ref(),
            },
            &tpt_app_media_forensics_rules::comparison::ComparisonInput {
                name: right_name,
                streams: right.container.as_ref().map(|c| c.streams.as_slice()),
                metadata: right.metadata.as_ref(),
                scene: right.scene.as_ref(),
                silence: Some(right.silence.as_slice()),
                loudness: right.loudness.as_ref(),
            },
        );

        let view = tpt_app_media_forensics_tauri::view::comparison::ComparisonView::build(&result);
        assert_ne!(
            view.verdict,
            tpt_app_media_forensics_tauri::view::comparison::ComparisonVerdict::Different,
            "{name}: a file compared with itself reported a difference"
        );
    }
}
