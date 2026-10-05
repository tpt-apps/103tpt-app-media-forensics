//! Robustness harness: arbitrary bytes through every reader (spec §75, §77).
//!
//! # What this is, and what it is not
//!
//! This is **not** coverage-guided fuzzing. `cargo-fuzz` and libFuzzer need clang,
//! and neither is available on the machine this project is built on, so a real
//! fuzzing target could not run in CI or be reproduced by a contributor. What this
//! is instead: arbitrary bytes driven through every parser and the whole analysis
//! pipeline by `proptest`'s seeded generator, which runs on every build, on every
//! platform, with a failure reproducible from a printed seed.
//!
//! The trade-off is real and worth stating. libFuzzer would find deeper bugs
//! faster, because coverage feedback steers it at new paths. This harness explores
//! the *same* input class — bytes an attacker controls — on a schedule, and it
//! will find shallow memory-safety and bounds bugs reliably. Genuine coverage-
//! guided fuzzing remains undone, and `todo.md` records it as such.
//!
//! # Why this matters more here than in most projects
//!
//! Spec §75 makes "malformed media cannot crash the application" an acceptance
//! criterion, and this product's entire premise is that a hostile file is *data*.
//! Every reader below takes bytes from evidence. A panic in any one of them takes
//! down the process holding the analyst's unsaved work.
//!
//! # The property
//!
//! One: **no input panics, hangs, or exhausts memory.** A reader may return an
//! error, return nothing, or return a finding — all three are legitimate answers
//! to a malformed file. Panicking is not.

use proptest::prelude::*;

use tpt_app_media_forensics_core::AnalysisEngine;
use tpt_app_media_forensics_rules::{builtin_rules, RuleEngine, RuleProfile};

/// Bytes an attacker controls, bounded so a case cannot take unbounded time.
///
/// The cap matters as much as the assertions: a fuzz target that occasionally
/// generates a gigabyte is a fuzz target that gets disabled. 64 KiB is well past
/// any real `moov`, and the engine's own readers already refuse larger inputs.
fn arbitrary_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..65_536)
}

/// A well-formed box header wrapped around `body`.
///
/// Mixing a real container skeleton into the input class is what stops this
/// harness from only ever testing "random noise, rejected immediately". A file
/// that is a valid `moov` around hostile inner bytes reaches every reader; a file
/// of noise is rejected by the first length check.
fn plausible_container() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..4096).prop_map(|body| {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(b"moov");
        out.extend_from_slice(&body);
        out
    })
}

/// Runs arbitrary bytes through the whole analysis pipeline.
fn observe(bytes: &[u8]) {
    let dir = tempfile::tempdir().expect("scratch");
    let path = dir.path().join("fuzz.bin");
    std::fs::write(&path, bytes).expect("writes the hostile file");

    // `observe_stages` is the read-only surface, and it reaches every reader:
    // container, metadata, timing, packet scan, and — where the bytes parse
    // well enough to carry one — the Tier-2 decoder. Whatever it returns must be
    // a value, not an unwind.
    let (bundle, _limitations) = AnalysisEngine::new().observe_stages(&path);

    // Then every rule, because a rule matching on attacker-controlled numbers is
    // as capable of panicking as the parser that produced them, and the rules are
    // a separate trust boundary from the readers.
    let _ = RuleEngine::new(builtin_rules()).evaluate(&bundle, &RuleProfile::default());
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        max_shrink_iters: 4096,
        ..ProptestConfig::default()
    })]

    /// Arbitrary bytes must not take the pipeline down.
    ///
    /// The end-to-end version, and the one that matters: it reaches every reader
    /// transitively, including ones no individual test knows to call.
    #[test]
    fn arbitrary_bytes_never_panic_the_pipeline(bytes in arbitrary_bytes()) {
        observe(&bytes);
    }

    /// A plausible container wrapped around hostile bytes must not panic either.
    ///
    /// The complement, and it is the one that actually reaches the parsers. Noise
    /// is rejected by the first length check; a valid `moov` header around
    /// arbitrary inner bytes walks the box tree, the sample tables, the colour and
    /// edit-list readers, and the metadata scan. Without this half, the harness
    /// would mostly be measuring the top-level length check.
    #[test]
    fn a_plausible_container_never_panics_the_pipeline(bytes in plausible_container()) {
        observe(&bytes);
    }

    /// The individual readers, called directly.
    ///
    /// The pipeline test above routes through whatever the format sniffer
    /// decides. Calling the readers themselves means a defect is attributed to the
    /// reader that has it rather than to "the pipeline", which is the difference
    /// between a fixable bug report and a mystery.
    #[test]
    fn no_reader_panics_on_arbitrary_bytes(bytes in arbitrary_bytes()) {
        // ISO-BMFF: the box walk, inspection, the `moov`-only reader, and sample
        // extraction. `Err` is a fine answer for all four.
        let _ = tpt_app_media_forensics_container::scan_isobmff(&bytes);
        let _ = tpt_app_media_forensics_container::inspect_bytes(bytes.clone());
        let _ = tpt_app_media_forensics_container::read_samples(bytes.clone());

        // `read_moov` takes a path: it exists so a 40 GB asset's 2 KB `moov` can
        // be read without loading the media, so it is exercised against a file
        // rather than a buffer. That makes it the one reader this loop cannot
        // cover, and it is covered separately below instead of skipped.
        let dir = tempfile::tempdir().expect("scratch");
        let path = dir.path().join("fuzz.bin");
        std::fs::write(&path, &bytes).expect("writes the hostile file");
        let _ = tpt_app_media_forensics_container::read_moov(&path);

        // Matroska: a different reader entirely, with VINT decoding that is
        // different arithmetic from ISO-BMFF's fixed-width boxes. Testing only one
        // would leave the other's length handling unexercised.
        let _ = tpt_app_media_forensics_container::inspect_matroska_bytes(bytes.clone());

        // The sub-readers the box readers delegate to. Each has its own offsets
        // into the sample entry, and reading one at another's offsets is the
        // defect class that already bit the audio entry once.
        let _ = tpt_app_media_forensics_container::colr::parse_track_colour(&bytes);
        let _ = tpt_app_media_forensics_container::elst::parse_edit_lists(&bytes);
        let _ = tpt_app_media_forensics_container::parse_track_audio(&bytes);
        let _ = tpt_app_media_forensics_container::boxes::parse_composition_offsets(&bytes);

        // The Matroska sample reader takes the buffer by value, so it goes last.
        let _ = tpt_app_media_forensics_container::read_matroska_samples(bytes);
    }

    /// Truncating a file can never make the pipeline panic.
    ///
    /// Truncation is the corruption an analyst actually meets — a failed copy, an
    /// interrupted upload — so it deserves its own sweep rather than relying on
    /// the generator to happen to produce a short file. Cutting at several
    /// fractions also reaches the readers' partial-input paths, which the
    /// whole-file case never takes.
    #[test]
    fn a_truncated_prefix_never_panics(bytes in plausible_container()) {
        for cut in [
            0,
            1,
            bytes.len() / 4,
            bytes.len() / 2,
            bytes.len().saturating_sub(1),
        ] {
            observe(&bytes[..cut.min(bytes.len())]);
        }
    }
}
