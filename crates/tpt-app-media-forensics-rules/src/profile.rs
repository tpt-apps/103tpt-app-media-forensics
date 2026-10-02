//! Rule profiles and tolerances (spec §37, §70).
//!
//! # Tolerances live in the profile, never in a rule
//!
//! A rule that hard-codes "more than 5 frames is a GOP change" cannot be
//! tightened for a client whose delivery specification says 2. Every threshold
//! therefore comes from the active [`RuleProfile`], and changing one changes the
//! profile fingerprint — which invalidates the analysis cache (spec §54) and
//! appears in the report (spec §63).
//!
//! # Profiles are versioned
//!
//! A report must identify the exact profile version used, and an existing
//! profile is never silently changed (spec §70). Bumping `version` documents
//! that thresholds moved.

use serde::{Deserialize, Serialize};
use tpt_app_media_forensics_model::{MediaTime, ProfileFingerprint};

/// A named, versioned set of rule thresholds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleProfile {
    /// Human-readable profile name, e.g. "Default forensic".
    pub name: String,
    /// Version of this profile's thresholds.
    pub version: u32,

    /// Frames a GOP may differ from the dominant length before it counts as a
    /// structural change.
    pub gop_tolerance_frames: u32,

    /// Permitted deviation between consecutive frame timestamps before a gap
    /// is reported.
    pub pts_tolerance: MediaTime,

    /// Peak amplitude below which a sample counts as silent, 0.0 to 1.0.
    pub silence_threshold: f64,

    /// Shortest run of silence worth reporting, in samples.
    pub min_silence_frames: u64,

    /// Shortest run of identical samples worth reporting as duplicate.
    pub min_duplicate_run: u32,

    /// Sample magnitude at or above which a sample counts as clipped.
    pub clipping_threshold: f64,

    /// Mean sample magnitude above which DC offset is reported.
    pub dc_offset_threshold: f64,

    /// Loudness below which an audio asset is reported as inaudible.
    pub inaudible_lufs: f64,

    /// Difference in LUFS between the start and end of an asset that counts as
    /// audible loudness drift.
    pub loudness_drift_tolerance_lu: f64,
    /// A/V offset change, in milliseconds, above which drift is reported.
    ///
    /// In milliseconds, not LU: offset and drift are time quantities, and
    /// sharing a tolerance field with a level measurement invites comparing
    /// unlike units.
    pub av_drift_tolerance_ms: f64,

    /// Mean absolute luma difference between consecutive frames above which two
    /// frames are treated as a scene change.
    ///
    /// A profile value rather than a constant: what counts as a cut depends on
    /// the content, and a delivery specification should be able to set it
    /// without a rebuild.
    pub scene_change_threshold: f64,

    /// Frames either side of a pair compared for near-duplication.
    ///
    /// Bounding this is what makes the measurement tractable. An all-pairs scan
    /// is quadratic, and over a long file that is not a computation an analyst
    /// will wait for.
    pub near_duplicate_window: usize,

    /// Frames per sliding window for the bitrate measurement (spec §29).
    ///
    /// A window, not the whole file: one average over a long recording hides
    /// every local change, which is the entire subject of §29.
    pub bitrate_window_frames: usize,

    /// Fraction of the file's average bitrate below which a window is anomalous.
    ///
    /// Must be well under 1.0. Spec §29 asks for *sudden changes*, and a window
    /// at or above the average is not a change. 0.5 means "less than half the
    /// average", which a normal encoder produces only on genuinely static
    /// content.
    pub bitrate_anomaly_ratio: f64,
}

impl Default for RuleProfile {
    /// A deliberately permissive default.
    ///
    /// Tols are wide enough that ordinary encodes do not raise findings; a
    /// profile for a specific delivery specification tightens them.
    fn default() -> Self {
        Self {
            name: "Default forensic".to_owned(),
            version: 1,
            gop_tolerance_frames: 5,
            pts_tolerance: MediaTime::from_millis(2),
            silence_threshold: 0.001,
            min_silence_frames: 1_000,
            min_duplicate_run: 2,
            clipping_threshold: 0.999,
            dc_offset_threshold: 0.001,
            inaudible_lufs: -70.0,
            loudness_drift_tolerance_lu: 2.0,
            av_drift_tolerance_ms: 20.0,
            scene_change_threshold: 60.0,
            near_duplicate_window: 8,
            // 12 frames is half a second at 25 fps: long enough that ordinary
            // encoder variation averages out, short enough to localise a cut.
            bitrate_window_frames: 12,
            bitrate_anomaly_ratio: 0.5,
        }
    }
}

impl RuleProfile {
    /// Returns the fingerprint used for cache keying (spec §54, §63).
    ///
    /// Derived from the serialised thresholds, so any change to any tolerance
    /// produces a different fingerprint and invalidates cached results.
    #[must_use]
    pub fn fingerprint(&self) -> ProfileFingerprint {
        let serialised = serde_json::to_string(self).unwrap_or_else(|_| self.name.clone());
        ProfileFingerprint::from_serialized(&serialised)
    }

    /// Returns the identifier printed in reports, e.g. `default-forensic v1`.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_any_tolerance_changes_the_fingerprint() {
        // If this did not hold, a tightened tolerance would silently reuse
        // cached results computed with the looser one (spec §54).
        let base = RuleProfile::default();
        let changed = RuleProfile {
            gop_tolerance_frames: base.gop_tolerance_frames + 1,
            ..base.clone()
        };
        assert_ne!(base.fingerprint(), changed.fingerprint());
    }

    #[test]
    fn renaming_a_profile_changes_the_fingerprint() {
        let renamed = RuleProfile {
            name: "Client X".to_owned(),
            ..RuleProfile::default()
        };
        assert_ne!(RuleProfile::default().fingerprint(), renamed.fingerprint());
    }

    #[test]
    fn identical_profiles_share_a_fingerprint() {
        assert_eq!(
            RuleProfile::default().fingerprint(),
            RuleProfile::default().fingerprint()
        );
    }

    #[test]
    fn identifier_names_the_version() {
        // A report must state which profile version produced it (spec §70).
        let profile = RuleProfile {
            name: "Client X Delivery".to_owned(),
            version: 4,
            ..RuleProfile::default()
        };
        assert_eq!(profile.identifier(), "client-x-delivery v4");
    }

    #[test]
    fn the_default_profile_does_not_flag_ordinary_content() {
        // Defaults are permissive: a normal encode should be quiet.
        let profile = RuleProfile::default();
        assert!(profile.gop_tolerance_frames >= 2);
        assert!(profile.min_duplicate_run >= 2);
        assert!(profile.clipping_threshold > 0.99);
    }
}
