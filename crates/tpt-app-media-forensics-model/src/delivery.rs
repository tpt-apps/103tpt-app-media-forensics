//! Delivery specifications: a profile's declared requirements and the result of
//! checking one against a file (spec §68, §69, §70, §95).
//!
//! # Why this is domain data and not engine logic
//!
//! The engine already had a verdict — `Severity::fails_validation` in
//! [`crate::finding`] answers "do the findings permit delivery". What it did not
//! have was the *specification* a delivery is judged against. Spec §68's example
//! is a declared profile: `codec: h264`, `width: 1920`, `frame_rate: 25`,
//! `channels: 2`, `sample_rate: 48000`, `format: mov`. Nothing in the codebase
//! could express those requirements, let alone check them.
//!
//! So these types live in `-model`, the crate every other one depends on, rather
//! than beside the checker. The checker in `-rules` and the verdict in `-report`
//! then read the same types, and a profile written once is understood by the
//! engine, the CLI, the GUI, and the report format alike.
//!
//! # "Not measured" is not "met"
//!
//! The three-way [`RequirementOutcome`] is the load-bearing decision here. A
//! file whose audio this build never decodes, or whose container declares
//! nothing, has not satisfied a requirement — it has left it unmeasured. Folding
//! that into `Met` would let a delivery profile print `PASS` for a file it never
//! checked, which is the precise failure this product exists to prevent (spec
//! §75, §96).
//!
//! # Tolerances are explicit, never defaulted
//!
//! Every numeric requirement carries its own tolerance, and none of them has a
//! `serde` default. A profile that silently supplied one would be a profile whose
//! verdict could change because a field was omitted — and an omitted field is
//! exactly what a customer with a hand-written specification gets wrong. Making
//! the tolerance mandatory moves that mistake to authoring time, where it is
//! visible.

use serde::{Deserialize, Serialize};

/// A declared delivery requirement and the tolerance it is checked with.
///
/// Serialises with a `kind` tag, so a profile file reads as
/// `{"kind": "frame_rate", "fps": 25, "tolerance": 0.5}` rather than as an
/// externally tagged enum. That choice is what makes a profile a readable
/// document instead of a Rust value dump.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Requirement {
    /// The video codec must be one of a named family.
    ///
    /// Families, not four-character codes: a profile says `h264` because that is
    /// what a customer's specification says, and the engine knows that `avc1` and
    /// `avc3` are both H.264. The declared tag is still what the file is checked
    /// against — see [`crate::media::CodecInfo`], which preserves it verbatim.
    VideoCodec {
        /// Accepted codec family names, e.g. `h264`.
        any_of: Vec<String>,
    },

    /// The video frame size must be exactly this.
    VideoResolution {
        /// Required width in pixels, after any crop the container declares.
        width: u32,
        /// Required height in pixels, after any crop the container declares.
        height: u32,
    },

    /// The video frame rate must be within `tolerance` frames per second of
    /// `fps`.
    ///
    /// The tolerance exists because 25 fps and 24000/1001 fps are the same rate to
    /// every encoder that has ever existed, and a profile demanding exactly 25.0
    /// would reject NTSC-derived material it should accept. Zero tolerance is
    /// legitimate and is expressible; it is simply not assumed.
    FrameRate {
        /// Required rate in frames per second.
        fps: f64,
        /// Permitted absolute deviation, in frames per second.
        tolerance: f64,
    },
    /// The audio codec must be one of a named family.
    AudioCodec {
        /// Accepted codec family names, e.g. `pcm`.
        any_of: Vec<String>,
    },

    /// The audio channel count must be exactly this.
    AudioChannels {
        /// Required number of discrete channels.
        channels: u16,
    },

    /// The audio sample rate must be exactly this, in Hz.
    AudioSampleRate {
        /// Required rate in Hz.
        sample_rate: u32,
    },

    /// The container format must be one of the named families.
    ContainerFormat {
        /// Accepted format tags, e.g. `isobmff`.
        any_of: Vec<String>,
    },

    /// The A/V offset must not exceed this, in milliseconds (spec §95).
    ///
    /// An upper bound rather than an equality: there is no such thing as an
    /// exactly-zero offset that a viewer would actually watch.
    MaxAvOffsetMs {
        /// Largest permitted absolute offset, in milliseconds.
        limit_ms: f64,
    },
}

impl Requirement {
    /// Returns the stable rule-shaped identifier this requirement reports under.
    ///
    /// These are the same identifiers spec §69's example profile lists under
    /// `rules`, and they appear in the report beside the finding they produced,
    /// so a reviewer can match a line of the specification to a line of the
    /// result without translating between two vocabularies.
    #[must_use]
    pub fn id(&self) -> &'static str {
        match self {
            Self::VideoCodec { .. } => "video.codec",
            Self::VideoResolution { .. } => "video.resolution",
            Self::FrameRate { .. } => "video.frame_rate",
            Self::AudioCodec { .. } => "audio.codec",
            Self::AudioChannels { .. } => "audio.channels",
            Self::AudioSampleRate { .. } => "audio.sample_rate",
            Self::ContainerFormat { .. } => "container.format",
            Self::MaxAvOffsetMs { .. } => "timing.max_av_offset_ms",
        }
    }

    /// Renders what the profile demands, as spec §95 prints it.
    #[must_use]
    pub fn expected_text(&self) -> String {
        match self {
            Self::VideoCodec { any_of } | Self::AudioCodec { any_of } => any_of.join(" or "),
            Self::ContainerFormat { any_of } => any_of.join(" or "),
            Self::VideoResolution { width, height } => format!("{width}x{height}"),
            Self::FrameRate { fps, tolerance } => {
                // The tolerance is printed with the rate, never dropped. "Expected:
                // 25" beside a check that actually allowed 24.5 to 25.5 is a
                // claim the report cannot support, and a reviewer reading it
                // would have no way to know slack was being granted.
                if *tolerance == 0.0 {
                    format!("{} (exact)", trim_float(*fps))
                } else {
                    format!("{} (+/- {})", trim_float(*fps), trim_float(*tolerance))
                }
            }
            Self::AudioChannels { channels } => channels.to_string(),
            Self::AudioSampleRate { sample_rate } => sample_rate.to_string(),
            Self::MaxAvOffsetMs { limit_ms } => format!("<= {} ms", trim_float(*limit_ms)),
        }
    }
}
/// Whether one requirement was met, missed, or never measured.
///
/// Three states, not two, and the third is the one that matters most here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RequirementOutcome {
    /// The file satisfies the requirement.
    Met,
    /// The file was measured and does not satisfy the requirement.
    NotMet,
    /// The file could not be measured for this requirement.
    ///
    /// Never rendered as `Met`, and never silently dropped. A delivery profile
    /// that reports `PASS` for a requirement it could not check is making a
    /// claim about the file that nothing in the run supports.
    Undetermined,
}

impl RequirementOutcome {
    /// Returns the label printed in a validation report.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Met => "MET",
            Self::NotMet => "NOT MET",
            Self::Undetermined => "NOT MEASURED",
        }
    }

    /// Whether this outcome blocks delivery.
    ///
    /// `Undetermined` blocks, and that is the deliberate part. "We could not look
    /// at it" is not "it was fine" — the same rule `view/batch.rs` applies when
    /// it gives an unreadable file its own `UNREADABLE` status rather than folding
    /// it into `PASS`.
    #[must_use]
    pub const fn blocks_delivery(self) -> bool {
        matches!(self, Self::NotMet | Self::Undetermined)
    }
}

/// One requirement checked against one file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequirementCheck {
    /// The requirement's identifier, e.g. `video.frame_rate`.
    pub requirement_id: String,
    /// What the profile demanded.
    pub expected: String,
    /// What the file was measured as.
    ///
    /// `None` when the outcome is [`RequirementOutcome::Undetermined`]: there is
    /// no observed value to print, and printing one would invent it.
    pub observed: Option<String>,
    /// The outcome.
    pub outcome: RequirementOutcome,
    /// Why the outcome is what it is, in one sentence.
    ///
    /// Spec §71's rule for findings applies unchanged to a requirement: a
    /// `NOT MEASURED` line that does not say what was missing sends the reader
    /// looking for a defect in the file rather than in the analysis.
    pub detail: String,
}

/// The result of checking one profile against one file (spec §68, §95).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeliveryReport {
    /// The profile's declared name, e.g. `Client X Delivery`.
    pub profile_name: String,
    /// The profile's version (spec §70).
    ///
    /// Reported alongside the fingerprint rather than instead of it: a version is
    /// what a human maintains, and the fingerprint is what proves the file was
    /// checked against those exact requirements.
    pub profile_version: u32,
    /// The profile's identifier, e.g. `client-x-delivery v3`.
    pub profile_identifier: String,
    /// Fingerprint of the profile's requirements.
    pub profile_fingerprint: String,
    /// One entry per requirement, in profile order.
    ///
    /// Never empty and never filtered: an unmeasured requirement appears with
    /// `NOT MEASURED`, because a list that quietly drops what it could not check
    /// cannot be reconciled against the profile that asked.
    pub checks: Vec<RequirementCheck>,
}

impl DeliveryReport {
    /// Returns the checks that did not pass.
    #[must_use]
    pub fn blocking(&self) -> Vec<&RequirementCheck> {
        self.checks
            .iter()
            .filter(|c| c.outcome.blocks_delivery())
            .collect()
    }

    /// Returns `true` when every requirement was met.
    ///
    /// Not the same as "nothing failed": a profile with one unmeasured
    /// requirement is not a pass, which is why this and [`Self::blocking`] are
    /// separate questions rather than one.
    #[must_use]
    pub fn is_fully_met(&self) -> bool {
        self.checks
            .iter()
            .all(|c| c.outcome == RequirementOutcome::Met)
    }
}

/// Renders a float without a trailing `.0`, for profile text.
///
/// A named, versioned set of declared delivery requirements (spec §68, §69).
///
/// # Versioning is not optional
///
/// Spec §70: *"A report must identify the exact profile version used. Never
/// silently change an existing profile."* Both halves are enforced here. The
/// version travels in the identifier a report prints, and [`Self::fingerprint`]
/// is derived from the requirements themselves, so a profile edited without a
/// version bump produces a different fingerprint and the two disagree visibly
/// rather than passing as the same specification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeliveryProfile {
    /// Human-readable profile name, e.g. `Client X Delivery`.
    pub name: String,
    /// Version of these requirements.
    pub version: u32,
    /// What the profile demands, checked in this order.
    ///
    /// Order is the profile's, not sorted: the sequence a customer wrote their
    /// specification in is the sequence they will read the result in.
    pub requirements: Vec<Requirement>,
}

impl DeliveryProfile {
    /// Builds a profile with no requirements.
    ///
    /// Present for the same reason [`crate::RuleProfile::default`] exists: a
    /// profile with no requirements is a real state (it declares nothing, so it
    /// can never fail), and it is the natural starting point for a caller that
    /// appends requirements one at a time.
    #[must_use]
    pub fn new(name: impl Into<String>, version: u32) -> Self {
        Self {
            name: name.into(),
            version,
            requirements: Vec::new(),
        }
    }

    /// Returns the identifier printed in reports, e.g. `client-x-delivery v3`.
    ///
    /// The same slug-and-version form [`crate::analysis::ProfileFingerprint`]
    /// uses for rule profiles, so a report's profile line reads the same way
    /// whichever kind of profile produced it.
    #[must_use]
    pub fn identifier(&self) -> String {
        let slug: String = self
            .name
            .to_ascii_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        format!("{slug} v{}", self.version)
    }

    /// Returns the fingerprint identifying these exact requirements (spec §70).
    ///
    /// Derived from the name, version, and every requirement — so editing a
    /// requirement without bumping the version changes the fingerprint, and the
    /// mismatch between an unchanged version and a changed fingerprint is what
    /// makes "never silently change an existing profile" checkable rather than
    /// merely stated.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        // `serde_json` is not a dependency of `-model`, so the canonical form is
        // built by hand from the fields. Order is fixed and the requirement order
        // is the profile's own, so two identical profiles fingerprint alike and
        // a reordered profile does not — a reordering that changes the order a
        // reviewer reads results in is a change worth detecting.
        let mut canonical = String::new();
        canonical.push_str(&self.name);
        canonical.push('\u{1f}');
        canonical.push_str(&self.version.to_string());
        for requirement in &self.requirements {
            canonical.push('\u{1f}');
            canonical.push_str(requirement.id());
            canonical.push('\u{1f}');
            canonical.push_str(&requirement.expected_text());
            if let Requirement::FrameRate { fps, tolerance } = requirement {
                canonical.push('\u{1f}');
                canonical.push_str(&trim_float(*fps));
                canonical.push('\u{1f}');
                canonical.push_str(&trim_float(*tolerance));
            }
        }
        tpt_app_media_forensics_model_asset_hash(&canonical)
    }
}

/// Renders a float without trailing zero noise, for profile text.
///
/// `25` reads as the frame rate a specification states; `25.0` reads as a value
/// this engine computed. Both are the same number, and only one of them is what
/// the profile author typed.
///
/// Two decimal places is what this actually needs, and the reasoning is worth
/// keeping: a frame rate tolerance is never finer than a thousandth of a frame
/// per second, and a fixed three-place format renders a declared `0.5` as
/// `0.500` � which is not a different number, but *is* a different string, and a
/// report that prints the specification back to the reader should print it as
/// they wrote it.
#[must_use]
pub fn trim_float(value: f64) -> String {
    if !value.is_finite() {
        // A profile carrying NaN or infinity is a file this build will refuse,
        // but the text form must still be defined rather than panicking.
        return format!("{value}");
    }
    if value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{}", value as i64);
    }
    let text = format!("{value:.3}");
    let text = text.trim_end_matches('0');
    text.trim_end_matches('.').to_owned()
}

/// Content-addresses a canonical profile description to a short hex digest.
///
/// `sha2` is deliberately not a dependency of `-model`: this crate is the
/// dependency root and must stay free of hashing machinery it does not otherwise
/// need. The digest reuses the same [`crate::id::EntityId`] derivation every
/// other identifier in this engine uses, which is what keeps the value stable
/// across builds — a fingerprint that changed between releases would defeat the
/// purpose of printing it beside a profile version.
///
/// This detects that a profile changed. It makes no claim about a profile's
/// authenticity, which is not what spec §70 asks for and not what an FNV digest
/// could support.
fn tpt_app_media_forensics_model_asset_hash(value: &str) -> String {
    use crate::id::{EntityId, EntityKind};
    let id = EntityId::new_derived(EntityKind::Report, &[value.as_bytes()]);
    crate::asset::to_hex(&id.value().to_be_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A profile covering every requirement kind, so a change to any one of them
    /// is observable in the fingerprint.
    fn full_profile() -> DeliveryProfile {
        DeliveryProfile {
            name: "Client X Delivery".to_owned(),
            version: 3,
            requirements: vec![
                Requirement::VideoCodec {
                    any_of: vec!["h264".to_owned()],
                },
                Requirement::VideoResolution {
                    width: 1920,
                    height: 1080,
                },
                Requirement::FrameRate {
                    fps: 25.0,
                    tolerance: 0.5,
                },
                Requirement::AudioCodec {
                    any_of: vec!["pcm".to_owned()],
                },
                Requirement::AudioChannels { channels: 2 },
                Requirement::AudioSampleRate {
                    sample_rate: 48_000,
                },
                Requirement::ContainerFormat {
                    any_of: vec!["mov".to_owned()],
                },
                Requirement::MaxAvOffsetMs { limit_ms: 40.0 },
            ],
        }
    }

    #[test]
    fn an_unchanged_profile_keeps_its_fingerprint() {
        // The property spec §70 rests on: the same specification is always the
        // same fingerprint, or the value printed beside a version means nothing.
        assert_eq!(full_profile().fingerprint(), full_profile().fingerprint());
    }

    #[test]
    fn changing_a_requirement_changes_the_fingerprint() {
        // Without this, "never silently change an existing profile" would be a
        // sentence rather than a check.
        let base = full_profile();
        let changed = DeliveryProfile {
            requirements: vec![Requirement::VideoResolution {
                width: 1280,
                height: 720,
            }],
            ..base.clone()
        };
        assert_ne!(base.fingerprint(), changed.fingerprint());
    }

    #[test]
    fn changing_a_tolerance_alone_changes_the_fingerprint() {
        // A tolerance that does not appear in the fingerprint would let a
        // profile be relaxed without the report's evidence changing at all.
        let base = DeliveryProfile {
            requirements: vec![Requirement::FrameRate {
                fps: 25.0,
                tolerance: 0.5,
            }],
            ..full_profile()
        };
        let relaxed = DeliveryProfile {
            requirements: vec![Requirement::FrameRate {
                fps: 25.0,
                tolerance: 2.0,
            }],
            ..full_profile()
        };
        assert_ne!(base.fingerprint(), relaxed.fingerprint());
    }

    #[test]
    fn bumping_the_version_changes_the_fingerprint() {
        let base = full_profile();
        let bumped = DeliveryProfile {
            version: 4,
            ..full_profile()
        };
        assert_ne!(base.fingerprint(), bumped.fingerprint());
    }

    #[test]
    fn a_profile_with_no_requirements_has_a_stable_fingerprint() {
        // An empty profile is a real state, and two of them must agree.
        let a = DeliveryProfile::new("Broadcast", 1);
        let b = DeliveryProfile::new("Broadcast", 1);
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert_ne!(
            a.fingerprint(),
            DeliveryProfile::new("Broadcast", 2).fingerprint()
        );
    }

    #[test]
    fn the_identifier_names_the_version() {
        // Spec §70: a report must identify the exact profile version used.
        assert_eq!(full_profile().identifier(), "client-x-delivery v3");
    }

    #[test]
    fn requirement_ids_match_the_names_a_specification_uses() {
        // Spec §69 lists rules as `video.resolution`, `audio.channels`. The
        // identifier is what a reviewer matches on, so it must be that spelling
        // and not an internal variant name.
        assert_eq!(
            Requirement::VideoResolution {
                width: 1,
                height: 1
            }
            .id(),
            "video.resolution"
        );
        assert_eq!(
            Requirement::AudioChannels { channels: 2 }.id(),
            "audio.channels"
        );
        assert_eq!(
            Requirement::MaxAvOffsetMs { limit_ms: 40.0 }.id(),
            "timing.max_av_offset_ms"
        );
    }

    #[test]
    fn expected_text_prints_the_whole_frame_rate_tolerance_not_just_the_rate() {
        // `expected_text` is what lands in the report. A line reading "Expected:
        // 25" beside a check that actually allowed 24.5 to 25.5 is a claim the
        // report cannot support.
        let requirement = Requirement::FrameRate {
            fps: 25.0,
            tolerance: 0.5,
        };
        assert_eq!(requirement.expected_text(), "25 (+/- 0.5)");
    }

    #[test]
    fn a_zero_tolerance_is_printed_rather_than_omitted() {
        // A profile author writing exactly 25.0 means it, and the report must
        // not imply slack that was not declared.
        let requirement = Requirement::FrameRate {
            fps: 25.0,
            tolerance: 0.0,
        };
        assert_eq!(requirement.expected_text(), "25 (exact)");
    }

    #[test]
    fn an_unmeasured_requirement_blocks_delivery() {
        // The load-bearing claim of this module. If this ever returns false, a
        // delivery profile can print PASS for a file it never checked.
        assert!(RequirementOutcome::Undetermined.blocks_delivery());
        assert!(RequirementOutcome::NotMet.blocks_delivery());
        assert!(!RequirementOutcome::Met.blocks_delivery());
    }

    #[test]
    fn outcome_labels_are_distinct_in_the_report() {
        // `NOT MEASURED` must not read as a near-miss of `NOT MET`; a reviewer
        // acts on the first by fixing the analysis and on the second by fixing
        // the file.
        let labels = [
            RequirementOutcome::Met.label(),
            RequirementOutcome::NotMet.label(),
            RequirementOutcome::Undetermined.label(),
        ];
        for (i, a) in labels.iter().enumerate() {
            for b in &labels[i + 1..] {
                assert_ne!(a, b, "two outcomes must not share a label");
            }
        }
    }

    #[test]
    fn a_profile_round_trips_through_json() {
        // §69 asks customers to write their own specifications, so the on-disk
        // form has to survive the round trip a report bundle does.
        let profile = full_profile();
        let json = serde_json::to_string(&profile).expect("serialises");
        let parsed: DeliveryProfile = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(profile, parsed);
    }

    #[test]
    fn a_profile_read_from_spec_style_json_matches_the_built_one() {
        // The literal shape a customer writes. If this changes, the documented
        // format and the parser have drifted apart.
        let json = r#"{
            "name": "Client X Delivery",
            "version": 3,
            "requirements": [
                {"kind": "video_codec", "any_of": ["h264"]},
                {"kind": "video_resolution", "width": 1920, "height": 1080},
                {"kind": "frame_rate", "fps": 25.0, "tolerance": 0.5},
                {"kind": "audio_channels", "channels": 2},
                {"kind": "audio_sample_rate", "sample_rate": 48000},
                {"kind": "container_format", "any_of": ["mov"]}
            ]
        }"#;
        let parsed: DeliveryProfile = serde_json::from_str(json).expect("deserialises");
        assert_eq!(parsed.name, "Client X Delivery");
        assert_eq!(parsed.version, 3);
        assert_eq!(parsed.requirements.len(), 6);
        assert_eq!(parsed.requirements[0].id(), "video.codec");
        assert_eq!(parsed.requirements[2].id(), "video.frame_rate");
    }

    #[test]
    fn a_frame_rate_without_a_tolerance_is_rejected_rather_than_defaulted() {
        // The reason tolerances carry no serde default. A profile that silently
        // supplied one would change its verdict because a field was omitted.
        let json = r#"{
            "name": "x", "version": 1,
            "requirements": [{"kind": "frame_rate", "fps": 25.0}]
        }"#;
        assert!(
            serde_json::from_str::<DeliveryProfile>(json).is_err(),
            "a frame rate with no tolerance must not load with an assumed one"
        );
    }

    #[test]
    fn an_unknown_requirement_kind_is_rejected() {
        // A typo in a hand-written profile must fail loudly at load time rather
        // than being ignored — a silently dropped requirement is a delivery that
        // passes without ever having been checked against it.
        let json = r#"{
            "name": "x", "version": 1,
            "requirements": [{"kind": "video_resolutionn", "width": 1920, "height": 1080}]
        }"#;
        assert!(serde_json::from_str::<DeliveryProfile>(json).is_err());
    }

    #[test]
    fn an_unmeasured_check_carries_no_observed_value() {
        // `observed: Some("0")` beside `NOT MEASURED` would be a fabricated
        // measurement wearing the label of an honest one.
        let check = RequirementCheck {
            requirement_id: "audio.channels".to_owned(),
            expected: "2".to_owned(),
            observed: None,
            outcome: RequirementOutcome::Undetermined,
            detail: "the container declares no audio sample entry".to_owned(),
        };
        assert!(check.observed.is_none());
        assert!(check.outcome.blocks_delivery());
    }

    #[test]
    fn a_report_is_not_fully_met_when_one_requirement_was_unmeasured() {
        let report = DeliveryReport {
            profile_name: "x".to_owned(),
            profile_version: 1,
            profile_identifier: "x v1".to_owned(),
            profile_fingerprint: "f".to_owned(),
            checks: vec![
                RequirementCheck {
                    requirement_id: "video.resolution".to_owned(),
                    expected: "1920x1080".to_owned(),
                    observed: Some("1920x1080".to_owned()),
                    outcome: RequirementOutcome::Met,
                    detail: "met".to_owned(),
                },
                RequirementCheck {
                    requirement_id: "audio.channels".to_owned(),
                    expected: "2".to_owned(),
                    observed: None,
                    outcome: RequirementOutcome::Undetermined,
                    detail: "not measured".to_owned(),
                },
            ],
        };
        assert!(!report.is_fully_met());
        assert_eq!(report.blocking().len(), 1);
    }
}
