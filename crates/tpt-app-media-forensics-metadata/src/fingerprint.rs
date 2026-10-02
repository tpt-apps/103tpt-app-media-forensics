//! Encoder fingerprinting (spec §27).
//!
//! # An indicator is not an identification
//!
//! Spec §27 asks for encoder signatures "where technically defensible", and the
//! distinction that makes it defensible is the one this module is built around: an
//! encoder *signature* is a string the encoder writes into the file, and anyone
//! can write any string into any file. `too=Lavf58.45.100` is evidence that a
//! file contains those characters. It is not evidence that FFmpeg produced it.
//!
//! So every [`EncoderIndicator`] carries a [`Confidence`] reflecting where the
//! signal came from. A declared tag is `Low`: it is self-reported and trivially
//! forged. Only a structural property that cannot be written into a string — an
//! all-intra GOP — reaches `Medium`, and nothing here claims `High`, because
//! nothing measured here establishes which program ran.
//!
//! # Why no bitstream analysis
//!
//! Spec §27 lists quantisation characteristics and bitstream features as
//! evidence. This build does not parse coded slice data, so those inputs do not
//! exist to be combined. Rather than infer an encoder from the absence of
//! evidence, the report states what it could not measure. A fingerprint that
//! silently omits its inputs reads as a complete answer.

use serde::{Deserialize, Serialize};

use crate::tree::{MetadataEntry, MetadataTree, Scope};

/// How much weight an indicator carries.
///
/// Deliberately narrower than the model's general-purpose scale: the top grade is
/// unreachable here by construction, and saying so in the type is clearer than
/// leaving a variant that never appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    /// A structural property of the file that cannot be expressed as a string.
    Medium,
    /// A self-reported tag, or a pattern too common to be diagnostic.
    Low,
}

/// What kind of evidence produced an indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// A string the encoder wrote into a metadata atom, e.g. `©too`.
    DeclaredTag,
    /// A property of the track's structure.
    Structure,
    /// A codec fourcc or profile string.
    CodecIdentity,
}
/// One observed indicator of the tool that wrote a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderIndicator {
    /// The tool this indicator points at, e.g. `FFmpeg`.
    ///
    /// This is what the evidence *matches*, not a determination of what produced
    /// the file. The distinction is carried by [`EncoderIndicator::limitations`].
    pub name: String,
    /// What was actually observed, verbatim.
    pub observation: String,
    /// Where the observation came from.
    pub evidence: EvidenceKind,
    /// How much weight it carries.
    pub confidence: Confidence,
    /// What this indicator does not establish.
    ///
    /// Present on every indicator, without exception. A tag written by one
    /// program and copied by another is the ordinary case, not an exotic one.
    pub limitations: String,
}

/// The fingerprinting result for one asset.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FingerprintReport {
    /// Every indicator found, in deterministic order.
    pub indicators: Vec<EncoderIndicator>,
    /// Evidence spec §27 lists that this build does not have.
    ///
    /// Recorded rather than omitted, so a reader can tell an asset with no
    /// fingerprint from one fingerprinted with limited inputs.
    pub not_measured: Vec<String>,
}

impl FingerprintReport {
    /// Returns true when no indicator was found.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.indicators.is_empty()
    }

    /// Renders the indicators as report lines.
    #[must_use]
    pub fn describe(&self) -> Vec<String> {
        self.indicators
            .iter()
            .map(|indicator| {
                format!(
                    "{} ({:?}, {:?}): {}",
                    indicator.name, indicator.evidence, indicator.confidence, indicator.observation
                )
            })
            .collect()
    }
}
/// Known encoder tags and the tool each names.
///
/// The needle is lowercase so matching is case-insensitive: real muxers write
/// `Lavf58`, `Lavc58`, and `Lavf60` across versions, and casing is a convention
/// rather than a fact about the encoder.
const DECLARED_TAGS: &[(&str, &str)] = &[
    ("lavf", "FFmpeg (libavformat)"),
    ("lavc", "FFmpeg (libavcodec)"),
    ("ffmpeg", "FFmpeg"),
    ("x264", "x264"),
    ("x265", "x265"),
    ("vlc", "VLC"),
    ("handbrake", "HandBrake"),
    ("mencoder", "MEncoder"),
    ("nvenc", "NVIDIA NVENC"),
    ("qsv", "Intel Quick Sync"),
    ("amf", "AMD AMF"),
];

/// Identifies encoder indicators from metadata and structure.
///
/// `tree` supplies the declared tags; `all_intra` says whether every sample is a
/// keyframe, which is the one structural property measurable without a decoder.
///
/// # Panics
///
/// Never.
#[must_use]
pub fn identify_encoders(tree: Option<&MetadataTree>, all_intra: bool) -> FingerprintReport {
    let mut indicators = Vec::new();

    for entry in tree.into_iter().flat_map(|tree| tree.entries.iter()) {
        // Only the encoder tag. `©nam` is a title and `©cmt` a comment: a file
        // named after a camera is not evidence about the program that wrote it,
        // and matching one would fire on most files in existence.
        if !is_encoder_tag(&entry.key) {
            continue;
        }
        indicators.extend(matches_for(&entry.value));
    }

    if all_intra {
        indicators.push(structural_indicator());
    }

    // Sorted so two runs over the same file agree (spec §77).
    indicators.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.observation.cmp(&b.observation))
            .then_with(|| a.evidence.cmp(&b.evidence))
    });
    indicators.dedup();

    FingerprintReport {
        indicators,
        not_measured: vec![
            "quantisation characteristics: coded slice data is not parsed by this build".to_owned(),
            "bitstream features: coded slice data is not parsed by this build".to_owned(),
            "closed versus open GOP: only the keyframe table is read, not reference structure"
                .to_owned(),
        ],
    }
}

/// Whether an atom names the encoder.
fn is_encoder_tag(key: &str) -> bool {
    // `©too` arrives as `\xa9too` before escaping and as `u{a9}too` after, so the
    // `©` prefix cannot be matched literally here.
    key.ends_with("too")
}

/// Matches a declared string against the known tags.
///
/// The whole string is kept as the observation even when only part of it matched:
/// a report showing "matches Lavf" beside the full tag lets a reviewer see what
/// the file actually says, including any suffix this build does not interpret.
fn matches_for(value: &str) -> Vec<EncoderIndicator> {
    let lowered = value.to_lowercase();
    DECLARED_TAGS
        .iter()
        .filter(|(needle, _)| lowered.contains(needle))
        .map(|(_, name)| EncoderIndicator {
            name: (*name).to_owned(),
            observation: value.to_owned(),
            evidence: EvidenceKind::DeclaredTag,
            // Low, and always: this is a string in the file, written by whatever
            // wrote the file and copied unchanged by everything downstream.
            confidence: Confidence::Low,
            limitations: "a declared tag is self-reported: any program can write any \
                          string into any file, and transcoders copy these strings unchanged"
                .to_owned(),
        })
        .collect()
}

/// The one structural indicator this build can measure.
fn structural_indicator() -> EncoderIndicator {
    EncoderIndicator {
        // Not a tool name: all-intra encoding is a configuration, not an
        // identity, and naming it as a tool would claim more than was measured.
        name: "all-intra encoding".to_owned(),
        observation: "every sample in the track is a sync sample".to_owned(),
        evidence: EvidenceKind::Structure,
        confidence: Confidence::Medium,
        limitations: "this is an encoding configuration, not an identification: encoders \
                      from many vendors can be told to emit only keyframes"
            .to_owned(),
    }
}

/// Metadata entries belonging to one track, for callers building a report.
#[must_use]
pub fn track_entries(tree: &MetadataTree, track: u32) -> Vec<&MetadataEntry> {
    tree.entries
        .iter()
        .filter(|e| e.scope == Scope::Track && e.track_index == Some(track))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{identify_encoders, Confidence, EvidenceKind};
    use crate::tree::{MetadataEntry, MetadataTree};

    /// A tree carrying one container-level atom.
    fn tree_with(key: &str, value: &str) -> MetadataTree {
        MetadataTree::new(vec![MetadataEntry::container(key, value, "moov")])
    }

    #[test]
    fn a_declared_ffmpeg_tag_is_reported_at_low_confidence() {
        let report = identify_encoders(Some(&tree_with("\u{a9}too", "Lavf58.45.100")), false);

        assert_eq!(report.indicators.len(), 1);
        let indicator = &report.indicators[0];
        assert!(indicator.name.contains("FFmpeg"), "{:?}", indicator.name);
        assert_eq!(indicator.evidence, EvidenceKind::DeclaredTag);
        assert_eq!(
            indicator.confidence,
            Confidence::Low,
            "a self-reported tag cannot carry more weight than this"
        );
        // The whole tag is preserved, so a reviewer sees what the file says.
        assert_eq!(indicator.observation, "Lavf58.45.100");
    }

    #[test]
    fn no_indicator_ever_claims_high_confidence() {
        // Nothing measured here can establish which program ran, so the top grade
        // must be unreachable rather than merely unused.
        let report = identify_encoders(Some(&tree_with("\u{a9}too", "Lavf58")), true);
        assert!(report
            .indicators
            .iter()
            .all(|i| matches!(i.confidence, Confidence::Low | Confidence::Medium)));
        assert!(report.indicators.iter().all(|i| !i.limitations.is_empty()));
    }

    #[test]
    fn a_title_tag_is_not_an_encoder_indicator() {
        // A file named after its camera is not evidence about what wrote it;
        // matching `©nam` would fire on most files in existence.
        let report = identify_encoders(Some(&tree_with("\u{a9}nam", "Lavf58.45.100")), false);
        assert!(report.is_empty(), "{:?}", report.indicators);
    }

    #[test]
    fn a_comment_tag_is_not_an_encoder_indicator() {
        let report = identify_encoders(Some(&tree_with("\u{a9}cmt", "encoded by x264")), false);
        assert!(report.is_empty(), "{:?}", report.indicators);
    }

    #[test]
    fn an_unknown_tag_produces_no_indicator() {
        let report = identify_encoders(Some(&tree_with("\u{a9}too", "SomeOtherTool 1.0")), false);
        assert!(report.is_empty());
        // And the absence is stated rather than left blank.
        assert!(!report.not_measured.is_empty());
    }

    #[test]
    fn an_all_intra_track_is_reported_as_a_configuration_not_a_tool() {
        let report = identify_encoders(None, true);
        assert_eq!(report.indicators.len(), 1);
        assert_eq!(report.indicators[0].confidence, Confidence::Medium);
        assert_eq!(report.indicators[0].evidence, EvidenceKind::Structure);
        assert!(
            !report.indicators[0].name.contains("FFmpeg"),
            "all-intra is a configuration, not an identification"
        );
    }

    #[test]
    fn no_metadata_and_no_structure_yields_an_empty_report() {
        let report = identify_encoders(None, false);
        assert!(report.is_empty());
        assert!(report.describe().is_empty());
    }

    #[test]
    fn an_empty_tree_is_handled_without_panicking() {
        let report = identify_encoders(Some(&MetadataTree::default()), false);
        assert!(report.is_empty());
    }

    #[test]
    fn indicators_are_ordered_deterministically() {
        let tree = MetadataTree::new(vec![
            MetadataEntry::container("\u{a9}too", "Lavf58.45.100", "moov"),
            MetadataEntry::track(0, "\u{a9}too", "x264 core 164", "trak"),
        ]);
        let first = identify_encoders(Some(&tree), true);
        let second = identify_encoders(Some(&tree), true);
        assert_eq!(first, second, "spec §77 requires reproducible output");

        let names: Vec<_> = first.indicators.iter().map(|i| i.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
    }

    #[test]
    fn an_indicator_is_produced_for_each_matching_tag_not_just_the_first() {
        // `Lavf` and `x264` both appear in real tags, and each names a different
        // component. Reporting only the first would hide one.
        let report = identify_encoders(Some(&tree_with("\u{a9}too", "Lavf58 x264")), false);
        assert_eq!(report.indicators.len(), 2, "{:?}", report.indicators);
    }

    #[test]
    fn the_report_states_what_it_could_not_measure() {
        let report = identify_encoders(None, false);
        let text = report.not_measured.join(" ").to_lowercase();
        assert!(text.contains("quantisation"), "{text}");
        assert!(text.contains("bitstream"), "{text}");
    }

    #[test]
    fn a_tag_is_matched_case_insensitively() {
        // Real muxers vary in casing across versions.
        let report = identify_encoders(Some(&tree_with("\u{a9}too", "LAVF58.45.100")), false);
        assert_eq!(report.indicators.len(), 1);
    }

    #[test]
    fn describe_renders_every_indicator() {
        let report = identify_encoders(Some(&tree_with("\u{a9}too", "Lavf58")), true);
        let lines = report.describe();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines.iter().all(|l| !l.trim().is_empty()));
    }
}
