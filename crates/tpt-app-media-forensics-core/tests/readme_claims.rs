//! The README must not claim more than the engine does.
//!
//! # Why this file exists
//!
//! The README listed six capabilities the code did not have: a comparison engine,
//! BLAKE3 hashing, spectral audio analysis, Vorbis-in-Matroska decoding, a
//! corrupt-media corpus held on disk, and rule definitions under `rules/`. Three
//! of the directories it pointed at were empty. All of it read as shipped
//! behaviour.
//!
//! That is the failure this project exists to prevent, pointed the other way.
//! The README already says *"a tool that quietly omits a measurement is worse
//! than one that names the gap"* \u2014 and then quietly invented measurements. For
//! forensic work the cost of a false capability claim is not a missing feature:
//! it is a report claiming to have checked something it never checked.
//!
//! Correcting the prose would leave it free to drift back, so the claims are
//! checked against the code instead.

use tpt_app_media_forensics_audio::decode::is_decodable;

const README: &str = include_str!("../../../README.md");

/// The text between two markers.
///
/// Returns an empty string when the opening marker is absent, which every test
/// below treats as a failure: a section that moved is a test to update, not a
/// check to skip silently.
fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let Some(at) = text.find(start) else {
        return "";
    };
    let body = &text[at + start.len()..];
    match body.find(end) {
        Some(i) => &body[..i],
        None => body,
    }
}

/// Capabilities the README must not list as available, with the reason.
///
/// Each of these is a *negative* claim, so it cannot be checked by calling the
/// engine — there is nothing to call. They are maintained by hand, which is why
/// [`every_claimed_capability_exists_in_the_source`] exists alongside: that test
/// checks the positive claims against the code, so a stale entry here cannot
/// silently become wrong the way the BLAKE3 entry did. That entry asserted
/// BLAKE3 was never called, and stopped being true the moment
/// `acquisition.rs` hashed with it.
const NOT_IMPLEMENTED: [(&str, &str); 4] = [
    ("reference master", "no comparison engine exists"),
    ("comparison", "no comparison engine exists"),
    ("spectrum", "no spectral analysis exists"),
    ("spectral", "no spectral analysis exists"),
];

/// Builds the claim table, embedding each file's source at compile time.
///
/// A macro rather than a plain array because `include_str!` takes a literal:
/// written directly, the paths would have to be spelled out twice, and a path
/// that no longer resolves is a compile error rather than a silently empty
/// check — which is the property that makes this worth keeping.
macro_rules! claimed_exist {
    ($(($term:expr, $file:expr, $marker:expr)),* $(,)?) => {
        [$(($term, include_str!($file), $marker)),*]
    };
}

/// Capabilities the README claims, each paired with the source that proves it.
///
/// The term is matched case-insensitively against the "What it does" section.
/// The marker is a substring that must appear in the embedded source — chosen to
/// be the actual implementation, not a mention of it, so a doc comment cannot
/// satisfy the check.
///
/// This is the check that would have caught the stale claims found by hand:
/// colour/HDR (no reader populates `ColourInfo`), spectral (no FFT in the audio
/// crate), and progress/cancellation (no such code in `-core`).
const CLAIMED_EXIST: [(&str, &str, &str); 17] = claimed_exist![
    (
        "container inspection",
        "../../tpt-app-media-forensics-container/src/mp4.rs",
        "pub fn inspect_bytes"
    ),
    // Claimed as "colour signalling" in the README. `parse_track_colour` is what
    // reads `colr`; before it existed, `ColourInfo` was `Default::default()` and
    // `is_hdr` a hardcoded `false` on every file.
    (
        "colour signalling",
        "../../tpt-app-media-forensics-container/src/colr.rs",
        "pub fn parse_track_colour"
    ),
    // Claimed as "Encoder fingerprinting". The `Confidence` enum having no top
    // grade is the mechanism: no indicator *can* claim more than its evidence
    // supports, so the claim cannot drift into a verdict.
    (
        "fingerprinting",
        "../../tpt-app-media-forensics-metadata/src/fingerprint.rs",
        "pub fn identify_encoders"
    ),
    (
        "gop",
        "../../tpt-app-media-forensics-video/src/gop.rs",
        "pub fn analyse"
    ),
    (
        "duplicate",
        "../../tpt-app-media-forensics-video/src/duplicate.rs",
        "find_repeated_runs"
    ),
    (
        "near-duplicate",
        "../../tpt-app-media-forensics-video/src/near_duplicate.rs",
        "PerceptualHash"
    ),
    (
        "scene",
        "../../tpt-app-media-forensics-video/src/scene.rs",
        "pub fn analyse"
    ),
    // Claimed as "bitrate and compression anomalies" in the README.
    (
        "bitrate",
        "../../tpt-app-media-forensics-video/src/bitrate.rs",
        "pub fn analyse"
    ),
    (
        "loudness",
        "../../tpt-app-media-forensics-audio/src/loudness.rs",
        "integrated_loudness"
    ),
    (
        "silence",
        "../../tpt-app-media-forensics-audio/src/measurement.rs",
        "find_silence"
    ),
    (
        "a/v",
        "../../tpt-app-media-forensics-timing/src/av_sync.rs",
        "pub fn analyse"
    ),
    (
        "pts/dts",
        "../../tpt-app-media-forensics-timing/src/pts_dts.rs",
        "scan_presentation"
    ),
    // Claimed as "Error timeline" in the README. `locate` is what turns a byte
    // offset into a media time, and `CONTAINER.TRUNCATED_MEDIA` is what uses it.
    (
        "error timeline",
        "../../tpt-app-media-forensics-container/src/damage.rs",
        "pub fn locate"
    ),
    // Claimed as "Edit lists" in the README. `parse_edit_lists` is what reads
    // `elst`; before it existed, `edit_list_offset` was `None` for every file.
    (
        "edit lists",
        "../../tpt-app-media-forensics-container/src/elst.rs",
        "pub fn parse_edit_lists"
    ),
    ("sha-256", "../src/acquisition.rs", "sha2::Sha256::new"),
    ("blake3", "../src/acquisition.rs", "blake3::Hasher::new"),
    (
        "pdf",
        "../../tpt-app-media-forensics-report/src/pdf.rs",
        "pub fn to_pdf"
    ),
];

#[test]
fn every_claimed_capability_exists_in_the_source() {
    // The positive half of the guard. `NOT_IMPLEMENTED` above is a hand-kept
    // list of things that must be absent; this checks the things that must be
    // present actually are, in the code rather than in the prose.
    //
    // A claim is only checked when the README makes it. Renaming a capability in
    // the README without renaming it here leaves it unchecked, which is the
    // failure this test is weakest to — but strictly better than a denylist
    // alone, which cannot detect a claim going stale in the other direction.
    let claims = between(README, "## What it does", "### Planned, not built");
    assert!(
        !claims.is_empty(),
        "the capability section moved; update this test"
    );
    let lowered = claims.to_lowercase();

    let mut missing: Vec<&str> = CLAIMED_EXIST
        .iter()
        .filter(|(term, _, _)| lowered.contains(term))
        .filter(|(_, source, marker)| !source.contains(marker))
        .map(|(term, _, _)| *term)
        .collect();
    missing.sort_unstable();

    assert!(
        missing.is_empty(),
        "the README claims {}, but no implementing code was found. Either implement it, or \
         move it to 'Planned, not built' where the gap is visible. A report that says it checked \
         something it never checked is the one failure this project exists to prevent.",
        missing.join(", ")
    );
}

#[test]
fn the_capability_list_names_nothing_the_engine_lacks() {
    // Scoped to the "What it does" list. The "Planned, not built" table below
    // it has to name these in order to record them as absent.
    let claims = between(README, "## What it does", "### Planned, not built");
    assert!(
        !claims.is_empty(),
        "the capability section moved; update this test"
    );

    let mut found: Vec<&str> = NOT_IMPLEMENTED
        .iter()
        .filter(|(term, _)| claims.to_lowercase().contains(term))
        .map(|(term, _)| *term)
        .collect();
    found.sort_unstable();

    assert!(
        found.is_empty(),
        "the README lists {} under 'What it does', which this build does not have. Either \
         implement it, or move it to 'Planned, not built' where the gap is visible.",
        found.join(", ")
    );
}

#[test]
fn the_capability_list_and_the_gap_list_do_not_overlap() {
    // Listed as both available and planned is worse than either alone: the
    // reader has no way to tell which to believe.
    let claims = between(README, "## What it does", "### Planned, not built").to_lowercase();
    let planned = between(README, "### Planned, not built", "## Three principles").to_lowercase();
    assert!(
        !claims.is_empty(),
        "the capability section moved; update this test"
    );
    assert!(
        !planned.is_empty(),
        "the gap section moved; update this test"
    );

    for (term, _) in NOT_IMPLEMENTED {
        assert!(
            !(claims.contains(term) && planned.contains(term)),
            "{term} is listed as both available and planned"
        );
    }
}

#[test]
fn every_codec_the_status_calls_decoded_really_is() {
    // Scoped to the status paragraph. The "will not do" list below it names
    // patent-encumbered codecs precisely because they are never decoded, so
    // scanning the whole section would flag the sentences that are honest.
    let status = between(README, "**Phase 1", "### What this build will not do");
    assert!(
        !status.is_empty(),
        "the status paragraph moved; update this test"
    );
    let lowered = status.to_lowercase();

    // A codec name counts as a claim only when the word "decod" follows it
    // closely, which is how the sentence is phrased ("Opus is decoded from ...").
    // Without that window, merely mentioning a codec would be read as a promise.
    let mut claimed: Vec<&str> = [
        "opus", "vorbis", "aac", "flac", "mp3", "speex", "theora", "vp9", "av1",
    ]
    .into_iter()
    .filter(|&codec| decode_is_claimed(&lowered, codec))
    .collect();
    claimed.sort_unstable();

    let mut undecodable: Vec<&str> = claimed
        .iter()
        .copied()
        .filter(|codec| !is_decodable(codec))
        .collect();
    undecodable.sort_unstable();

    assert!(
        undecodable.is_empty(),
        "the README names {} as decoded, but the audio crate refuses them. A reader who \
         believes this build decodes them will draw a conclusion from a measurement \
         that never happened.",
        undecodable.join(", ")
    );
}

/// Whether `lowered` presents `codec` as something this build decodes.
fn decode_is_claimed(lowered: &str, codec: &str) -> bool {
    let window = 40;
    let mut from = 0;
    while let Some(rel) = lowered[from..].find(codec) {
        let at = from + rel;
        let end = (at + codec.len() + window).min(lowered.len());
        if lowered[at..end].contains("decod") {
            return true;
        }
        from = at + codec.len();
    }
    false
}
