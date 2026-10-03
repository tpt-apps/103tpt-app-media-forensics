//! Whole-file comparison across two analysed assets (spec §38–40).
//!
//! # Where this sits and why
//!
//! `-model/src/comparison.rs` holds the vocabulary — [`Difference`],
//! [`ComparisonAxis`], and the per-stream pairing — because the model crate is
//! the dependency root and every layer above it needs those types. This module
//! holds the aggregate, because feeding it requires `MetadataTree`,
//! `SceneReport`, and `SilenceRegion`, and those live in crates the model must
//! not depend on. Splitting vocabulary from aggregation is what keeps the
//! model's "no I/O, no workspace dependencies" rule intact while still letting a
//! report compare two completed analyses.
//!
//! # Whole-file axes need a tolerance, and it is not the same for all of them
//!
//! Per-stream properties either match or do not. Audio levels and loudness are
//! measurements: two runs of the same file differ in the last bits of a float,
//! and a faithful re-encode shifts integrated loudness slightly. Comparing
//! those exactly would report a difference on every pair of files, which trains a
//! reviewer to ignore the axis — the comparison would be worse than useless. So
//! whole-file numeric axes compare against a tolerance and report which
//! tolerance they used, rather than claiming exact equality.

use serde::{Deserialize, Serialize};

use tpt_app_media_forensics_audio::{Measurement, Methodology, SilenceRegion};
use tpt_app_media_forensics_metadata::{MetadataEntry, MetadataTree, Scope};
use tpt_app_media_forensics_model::comparison::{
    ComparisonAxis, ComparisonSide, Difference, FieldComparison, StreamComparisonResult,
    UnmatchedStream,
};
use tpt_app_media_forensics_model::StreamAnalysis;
use tpt_app_media_forensics_video::scene::SceneReport;

/// How closely two whole-file measurements must agree to count as the same.
///
/// Defaults are deliberately loose enough to survive a faithful re-encode and
/// tight enough that a real change shows. They are reported alongside every
/// result so a reader can judge the claim rather than take it on trust.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Tolerances {
    /// Integrated loudness, in LU.
    ///
    /// A lossy re-encode routinely moves integrated loudness by a few tenths.
    pub loudness_lu: f64,
    /// Scene-change count, as an absolute count.
    ///
    /// An absolute count rather than a ratio: a two-cut file and a twenty-cut
    /// file have no meaningful "5% of cuts", and a ratio tolerance there would
    /// pass any one-frame disagreement while failing a large file over a cut a
    /// reviewer would not notice.
    pub scene_changes: f64,
}

impl Default for Tolerances {
    fn default() -> Self {
        Self {
            loudness_lu: 0.5,
            scene_changes: 1.0,
        }
    }
}

/// How two whole-file measurements compare, within a tolerance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WithinTolerance {
    /// Both were measured and agree to within the tolerance.
    Agree {
        /// The smaller of the two values.
        lower: f64,
        /// The larger of the two values.
        upper: f64,
        /// The absolute gap between them.
        delta: f64,
        /// The tolerance that gap was judged against.
        tolerance: f64,
    },
    /// Both were measured and disagree by more than the tolerance.
    Diverge {
        /// The left-hand value.
        left: f64,
        /// The right-hand value.
        right: f64,
        /// The absolute gap between them.
        delta: f64,
        /// The tolerance they were judged against.
        tolerance: f64,
    },
    /// At least one side could not be measured.
    Unmeasured {
        /// Which side had no measurement.
        side: ComparisonSide,
    },
}

impl WithinTolerance {
    /// Compares two optional measurements against a tolerance.
    #[must_use]
    pub fn compare(left: Option<f64>, right: Option<f64>, tolerance: f64) -> Self {
        match (left, right) {
            (Some(l), Some(r)) => {
                let delta = (l - r).abs();
                if delta <= tolerance {
                    Self::Agree {
                        lower: l.min(r),
                        upper: l.max(r),
                        delta,
                        tolerance,
                    }
                } else {
                    Self::Diverge {
                        left: l,
                        right: r,
                        delta,
                        tolerance,
                    }
                }
            }
            (None, Some(_)) => Self::Unmeasured {
                side: ComparisonSide::Left,
            },
            (Some(_), None) => Self::Unmeasured {
                side: ComparisonSide::Right,
            },
            // Neither measured is reported as unmeasured rather than as agreement:
            // two files with no loudness measurement have not been shown to match,
            // only shown to both lack the data.
            (None, None) => Self::Unmeasured {
                side: ComparisonSide::Left,
            },
        }
    }

    /// Whether the two values agreed within tolerance.
    #[must_use]
    pub fn agreed(&self) -> bool {
        matches!(self, Self::Agree { .. })
    }
}

/// A metadata key compared between two assets.
///
/// Keyed rather than positional: metadata is an unordered set of labelled
/// values, and comparing entry *3* of one file to entry *3* of another compares
/// whatever happened to sort into that position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetadataComparison {
    /// The key as written in the container.
    pub key: String,
    /// Which part of the container it came from.
    pub scope: Scope,
    /// Which track it belonged to, for track-scoped keys.
    pub track_index: Option<u32>,
    /// How the values compare.
    pub difference: Difference,
}

/// Silence structure compared between two assets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SilenceComparison {
    /// Total silent duration in frames on each side, when measured.
    pub totals: WithinTolerance,
    /// Number of distinct silent regions on each side.
    pub region_counts: Difference,
}

/// Scene-change structure compared between two assets.
///
/// A count rather than the frame-by-frame positions, because two encodes of the
/// same footage rarely agree on exactly which frame a cut lands on while
/// agreeing closely on how many cuts there are. Comparing positions would
/// report a difference on nearly every pair of related files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneComparison {
    /// Number of detected scene changes on each side.
    pub changes: Difference,
    /// The counts compared within tolerance.
    pub change_counts: WithinTolerance,
}

/// A whole-file comparison of two analysed assets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comparison {
    /// Display name of the left asset.
    pub left_name: String,
    /// Display name of the right asset.
    pub right_name: String,
    /// The per-stream half, computed by the model crate.
    pub streams: StreamComparisonResult,
    /// Metadata keys that differ, are one-sided, or are otherwise notable.
    pub metadata: Vec<MetadataComparison>,
    /// Scene-change structure.
    pub scene: SceneComparison,
    /// Silence structure.
    pub silence: SilenceComparison,
    /// Integrated loudness.
    pub loudness: WithinTolerance,
    /// The tolerances used, recorded so a reader can judge each result.
    pub tolerances: Tolerances,
}

impl Comparison {
    /// Streams with no counterpart, in either direction.
    #[must_use]
    pub fn unmatched(&self) -> &[UnmatchedStream] {
        &self.streams.unmatched
    }

    /// Whether the two assets are interchangeable on every axis measured.
    ///
    /// False when anything is uncomparable, even if nothing differs: "they match
    /// on everything we looked at" is a weaker claim than this, and conflating
    /// the two is the failure this whole design exists to prevent.
    #[must_use]
    pub fn is_equivalent(&self) -> bool {
        self.differences().iter().all(|f| f.difference.is_equal()) && self.unmatched().is_empty()
    }

    /// Axes that were measured on both sides and disagree.
    ///
    /// Distinct from [`Self::differences`], which also surfaces axes that could
    /// not be compared at all. An axis nobody measured is not evidence of a
    /// difference between two files, so a caller deciding whether two assets
    /// *agree* wants this; a caller building a report of everything notable wants
    /// [`Self::differences`].
    #[must_use]
    pub fn measured_differences(&self) -> Vec<FieldComparison> {
        self.differences()
            .into_iter()
            .filter(|f| f.difference.is_different())
            .collect()
    }
}

/// The input a whole-file comparison needs from one analysed asset.
///
/// A struct of `Option`s rather than a borrow of `AnalysisBundle`, so a caller
/// comparing two runs of the engine, or two bundles pulled from a case database,
/// supplies the same thing either way. Every field is optional because any stage
/// may have been skipped, and a missing measurement must not be silently read as
/// a zero.
#[derive(Debug, Clone, Default)]
pub struct ComparisonInput<'a> {
    /// Display name of the asset.
    pub name: &'a str,
    /// Container streams, when the container could be read.
    pub streams: Option<&'a [StreamAnalysis]>,
    /// The metadata tree, when metadata was extracted.
    pub metadata: Option<&'a MetadataTree>,
    /// The scene report, when Tier-2 decoding ran.
    pub scene: Option<&'a SceneReport>,
    /// Silence regions, when audio was decoded.
    pub silence: Option<&'a [SilenceRegion]>,
    /// Integrated loudness, when it could be measured.
    pub loudness: Option<&'a Measurement>,
}

/// Compares two analysed assets across every whole-file axis.
///
/// Never fails. An asset whose stages were skipped compares as
/// [`WithinTolerance::Unmeasured`] or [`Difference::NotComparable`] on those axes
/// rather than erroring, because a partial analysis is still a usable input and
/// refusing to compare would hide the axes that *were* measured.
#[must_use]
pub fn compare(left: &ComparisonInput<'_>, right: &ComparisonInput<'_>) -> Comparison {
    compare_with(left, right, Tolerances::default())
}

/// Compares two analysed assets using explicit tolerances.
#[must_use]
pub fn compare_with(
    left: &ComparisonInput<'_>,
    right: &ComparisonInput<'_>,
    tolerances: Tolerances,
) -> Comparison {
    Comparison {
        left_name: left.name.to_owned(),
        right_name: right.name.to_owned(),
        streams: tpt_app_media_forensics_model::compare_streams(left.streams, right.streams),
        metadata: compare_metadata(left.metadata, right.metadata),
        scene: compare_scene(left.scene, right.scene, tolerances.scene_changes),
        silence: compare_silence(left.silence, right.silence),
        loudness: WithinTolerance::compare(
            loudness_lufs(left.loudness),
            loudness_lufs(right.loudness),
            tolerances.loudness_lu,
        ),
        tolerances,
    }
}

/// Loudness in LUFS, but only for the integrated-loudness methodology.
///
/// A loudness *range* measurement is a different quantity computed over a
/// different window. Comparing an LRA number against an integrated number would
/// produce a plausible-looking figure that means nothing, so a non-integrated
/// measurement is treated as absent rather than compared.
fn loudness_lufs(measurement: Option<&Measurement>) -> Option<f64> {
    let m = measurement?;
    (m.methodology == Methodology::ItuBs1770_4).then_some(m.value)
}

/// Position of an axis in [`ComparisonAxis::ALL`], for deterministic ordering.
fn axis_order(axis: ComparisonAxis) -> usize {
    ComparisonAxis::ALL
        .iter()
        .position(|a| *a == axis)
        .expect("every axis is listed in ALL")
}

impl Comparison {
    /// Every property that was measured and is worth reporting.
    ///
    /// Ordered by [`ComparisonAxis::ALL`] then by field name, so two runs over
    /// the same pair of files produce identical output (spec §77).
    #[must_use]
    pub fn differences(&self) -> Vec<FieldComparison> {
        let mut out: Vec<FieldComparison> = self
            .streams
            .streams
            .iter()
            .flat_map(|s| s.differences().cloned())
            .collect();

        for entry in &self.metadata {
            if entry.difference.is_interesting() {
                out.push(FieldComparison::new(
                    ComparisonAxis::Metadata,
                    entry.key.clone(),
                    entry.difference.clone(),
                ));
            }
        }

        out.push(FieldComparison::new(
            ComparisonAxis::SceneStructure,
            "scene_changes",
            self.scene.changes.clone(),
        ));
        out.push(FieldComparison::new(
            ComparisonAxis::Silence,
            "silent_regions",
            self.silence.region_counts.clone(),
        ));
        out.push(FieldComparison::new(
            ComparisonAxis::Loudness,
            "integrated_loudness",
            loudness_difference(&self.loudness),
        ));
        out.push(FieldComparison::new(
            ComparisonAxis::StreamLayout,
            "stream_layout",
            self.streams.layout.clone(),
        ));

        out.sort_by(|a, b| {
            axis_order(a.axis)
                .cmp(&axis_order(b.axis))
                .then_with(|| a.field.cmp(&b.field))
        });
        out
    }
}

/// Renders a loudness comparison for a report.
///
/// Agreement within tolerance is reported as `Equal`, *not* as a difference with
/// a size. The alternative — reporting the 0.2 LU gap — contradicts
/// [`WithinTolerance::agreed`], which just told the caller the two agree, and
/// reintroduces exactly the noise the tolerance exists to remove. A reviewer who
/// wants the margin can read it from [`WithinTolerance::Agree::delta`] and the
/// tolerance beside it.
fn loudness_difference(loudness: &WithinTolerance) -> Difference {
    match loudness {
        WithinTolerance::Agree { .. } => Difference::Equal,
        WithinTolerance::Diverge { left, right, .. } => Difference::Different {
            left: format!("{left:.2} LUFS"),
            right: format!("{right:.2} LUFS"),
        },
        WithinTolerance::Unmeasured { .. } => Difference::NotComparable {
            reason: "loudness was not measured on at least one side".to_owned(),
        },
    }
}

/// Compares metadata by key rather than by position.
///
/// Pairs on `(scope, track, key)` because metadata is a set of labelled values,
/// not a sequence. Pairing positionally would compare entry 3 of one file to
/// entry 3 of another — whatever happened to sort there — and report differences
/// that do not exist while missing ones that do.
fn compare_metadata(
    left: Option<&MetadataTree>,
    right: Option<&MetadataTree>,
) -> Vec<MetadataComparison> {
    let (Some(left), Some(right)) = (left, right) else {
        return Vec::new();
    };

    // Collect the union of keys, sorted, so output order does not depend on
    // which tree was passed first or on container iteration order (spec §77).
    let mut keys: Vec<(Scope, String, Option<u32>)> = Vec::new();
    for entry in left.entries.iter().chain(right.entries.iter()) {
        let key = (entry.scope, entry.key.clone(), entry.track_index);
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys.sort();

    keys.into_iter()
        .filter_map(|(scope, key, track)| {
            let l_entry = find_entry(left, scope, &key, track);
            let r_entry = find_entry(right, scope, &key, track);

            let difference = match (l_entry, r_entry) {
                (Some(l), Some(r)) if l.value == r.value => Difference::Equal,
                (Some(l), Some(r)) => Difference::Different {
                    left: l.value.clone(),
                    right: r.value.clone(),
                },
                (Some(l), None) => Difference::OnlyLeft {
                    value: l.value.clone(),
                },
                (None, Some(r)) => Difference::OnlyRight {
                    value: r.value.clone(),
                },
                (None, None) => return None,
            };

            Some(MetadataComparison {
                key,
                scope,
                track_index: track,
                difference,
            })
        })
        .collect()
}

/// Finds one metadata entry by its full identity.
fn find_entry<'a>(
    tree: &'a MetadataTree,
    scope: Scope,
    key: &str,
    track: Option<u32>,
) -> Option<&'a MetadataEntry> {
    tree.entries
        .iter()
        .find(|e| e.scope == scope && e.key == key && e.track_index == track)
}

/// Compares two analyses of the same file and requires them to agree.
///
/// Exists because a comparison engine that cannot tell "these two files differ"
/// from "our analysis is not reproducible" is not much use: the second is a bug
/// in the engine, and reporting it as a difference between the files would send
/// a reviewer looking for a problem in the media that is not there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelfComparison {
    /// Axes that were measured and disagreed with themselves, which indicate an
    /// engine bug.
    pub unstable_axes: Vec<ComparisonAxis>,

    /// The full comparison, for the detail behind each unstable axis.
    pub comparison: Comparison,
}

impl SelfComparison {
    /// Whether the asset agreed with itself on every axis *measured*.
    ///
    /// An axis nobody measured is not instability: it is a gap, and it is
    /// reported by [`SelfComparison::comparison`] rather than counted as the
    /// engine contradicting itself.
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        self.comparison.measured_differences().is_empty() && self.comparison.unmatched().is_empty()
    }
}

/// Compares two analyses of the same file.
///
/// A self-comparison whose axes are all `NotComparable` is *vacuously*
/// consistent: nothing was measured, so nothing disagreed. That is the honest
/// reading, and reporting it as instability would make a partial analysis look
/// like a broken engine.
#[must_use]
pub fn compare_self(input: &ComparisonInput<'_>, other: &ComparisonInput<'_>) -> SelfComparison {
    let comparison = compare(input, other);
    let unstable_axes: Vec<ComparisonAxis> = comparison
        .measured_differences()
        .into_iter()
        .map(|f| f.axis)
        .collect();
    let mut unique = unstable_axes.clone();
    unique.sort();
    unique.dedup();

    SelfComparison {
        unstable_axes: unique,
        comparison,
    }
}

fn compare_scene(
    left: Option<&SceneReport>,
    right: Option<&SceneReport>,
    tolerance: f64,
) -> SceneComparison {
    let left_count = left.map(|r| r.differences.len() as f64);
    let right_count = right.map(|r| r.differences.len() as f64);
    let counts = WithinTolerance::compare(left_count, right_count, tolerance);

    let changes = match (left_count, right_count) {
        (Some(l), Some(r)) if l == r => Difference::Equal,
        (Some(l), Some(r)) => Difference::Different {
            left: format!("{l:.0} scene changes"),
            right: format!("{r:.0} scene changes"),
        },
        // Same reasoning as silence: a side that never measured has not been
        // shown to have zero scene changes, so this is not a one-sided value.
        (Some(_), None) | (None, Some(_)) | (None, None) => Difference::NotComparable {
            reason: "scene structure was not measured on at least one side".to_owned(),
        },
    };

    SceneComparison {
        changes,
        change_counts: counts,
    }
}

fn compare_silence(
    left: Option<&[SilenceRegion]>,
    right: Option<&[SilenceRegion]>,
) -> SilenceComparison {
    let left_total = left.map(|r| r.iter().map(|s| s.length_frames).sum::<u64>());
    let right_total = right.map(|r| r.iter().map(|s| s.length_frames).sum::<u64>());

    // An exact tolerance on an integer frame count: silence regions sit on sample
    // boundaries, so anything short of an exact match means the audio genuinely
    // differs rather than that a float wobbled.
    let totals = WithinTolerance::compare(
        left_total.map(|t| t as f64),
        right_total.map(|t| t as f64),
        0.0,
    );

    // A side that never measured is `NotComparable`, not a one-sided value. The
    // alternative — `OnlyLeft { value: "not measured" }` — reads as "only this
    // file has silence", which is a claim about the media that was never made.
    let region_counts = match (left, right) {
        (Some(l), Some(r)) if l.len() == r.len() => Difference::Equal,
        (Some(l), Some(r)) => Difference::Different {
            left: format!("{} region(s)", l.len()),
            right: format!("{} region(s)", r.len()),
        },
        (Some(_), None) | (None, Some(_)) | (None, None) => Difference::NotComparable {
            reason: "silence was not measured on at least one side".to_owned(),
        },
    };

    SilenceComparison {
        totals,
        region_counts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_app_media_forensics_metadata::MetadataEntry;

    fn input<'a>(
        name: &'a str,
        metadata: Option<&'a MetadataTree>,
        loudness: Option<&'a Measurement>,
        silence: Option<&'a [SilenceRegion]>,
    ) -> ComparisonInput<'a> {
        ComparisonInput {
            name,
            streams: None,
            metadata,
            scene: None,
            silence,
            loudness,
        }
    }

    fn tree(entries: Vec<MetadataEntry>) -> MetadataTree {
        MetadataTree::new(entries)
    }

    fn integrated(lufs: f64) -> Measurement {
        Measurement::new(lufs, Methodology::ItuBs1770_4)
    }

    fn silence_region(length: u64) -> SilenceRegion {
        SilenceRegion {
            start_frame: 0,
            end_frame: length,
            length_frames: length,
        }
    }

    #[test]
    fn an_identical_pair_agrees_on_every_axis_measured() {
        let l_meta = tree(vec![MetadataEntry::container("title", "A", "mvhd")]);
        let r_meta = tree(vec![MetadataEntry::container("title", "A", "mvhd")]);
        let l_loud = integrated(-23.0);
        let r_loud = integrated(-23.0);

        let result = compare(
            &input("a.mp4", Some(&l_meta), Some(&l_loud), None),
            &input("b.mp4", Some(&r_meta), Some(&r_loud), None),
        );

        assert!(
            result.measured_differences().is_empty(),
            "measured axes must agree: {:?}",
            result.differences()
        );
        assert!(result.unmatched().is_empty());
        // `is_equivalent` stays false because streams, scene, and silence were
        // never measured. That is the intended strictness, not a defect.
        assert!(
            !result.is_equivalent(),
            "unmeasured axes must not read as equivalence"
        );
    }

    #[test]
    fn a_loudness_difference_within_tolerance_does_not_make_the_pair_differ() {
        // The whole point of a tolerance: a re-encode moves integrated loudness
        // slightly, and reporting that on every pair of related files would train
        // a reviewer to ignore the axis.
        let l_loud = integrated(-23.0);
        let r_loud = integrated(-23.2);

        let result = compare(
            &input("a.mp4", None, Some(&l_loud), None),
            &input("b.mp4", None, Some(&r_loud), None),
        );

        assert!(
            result.loudness.agreed(),
            "0.2 LU is inside the default tolerance"
        );
        assert!(
            result.measured_differences().is_empty(),
            "a within-tolerance gap must not be reported as a difference: {:?}",
            result.differences()
        );
    }

    #[test]
    fn a_loudness_difference_beyond_tolerance_is_reported_with_its_size() {
        let l_loud = integrated(-23.0);
        let r_loud = integrated(-30.0);

        let result = compare(
            &input("a.mp4", None, Some(&l_loud), None),
            &input("b.mp4", None, Some(&r_loud), None),
        );

        assert!(!result.loudness.agreed());
        let loudness = result
            .differences()
            .into_iter()
            .find(|f| f.axis == ComparisonAxis::Loudness)
            .expect("loudness is always reported");
        assert!(
            loudness.difference.is_different(),
            "a 7 LU gap must be reported as a difference"
        );
        assert!(!result.is_equivalent());
    }

    #[test]
    fn a_loudness_range_measurement_is_not_compared_as_integrated_loudness() {
        // LRA and integrated loudness are different quantities. Comparing them
        // would produce a plausible-looking number that means nothing.
        let l_loud = integrated(-23.0);
        let lra = Measurement::new(6.0, Methodology::EbuR128Lra);

        let result = compare(
            &input("a.mp4", None, Some(&l_loud), None),
            &input("b.mp4", None, Some(&lra), None),
        );

        assert!(
            !result.loudness.agreed(),
            "a range measurement must not satisfy an integrated comparison"
        );
        assert!(matches!(
            result.loudness,
            WithinTolerance::Unmeasured { .. }
        ));
    }

    #[test]
    fn unmeasured_loudness_is_not_reported_as_agreement() {
        // Two files with no loudness measurement have not been shown to match.
        let result = compare(
            &input("a.mp4", None, None, None),
            &input("b.mp4", None, None, None),
        );

        assert!(matches!(
            result.loudness,
            WithinTolerance::Unmeasured { .. }
        ));
        assert!(
            !result.is_equivalent(),
            "nothing measured must not read as equivalent"
        );
    }

    #[test]
    fn metadata_is_paired_by_key_not_by_position() {
        // Same keys, different order. A positional comparison would report every
        // key as different.
        let l_meta = tree(vec![
            MetadataEntry::container("title", "A", "mvhd"),
            MetadataEntry::container("author", "B", "mvhd"),
        ]);
        let r_meta = tree(vec![
            MetadataEntry::container("author", "B", "mvhd"),
            MetadataEntry::container("title", "A", "mvhd"),
        ]);

        let result = compare(
            &input("a.mp4", Some(&l_meta), None, None),
            &input("b.mp4", Some(&r_meta), None, None),
        );

        let reported = result.differences();
        let differences: Vec<&str> = reported
            .iter()
            .filter(|f| f.axis == ComparisonAxis::Metadata)
            .map(|f| f.field.as_str())
            .collect();
        assert!(
            differences.is_empty(),
            "metadata order must not count as a difference: {differences:?}"
        );
    }

    #[test]
    fn a_metadata_value_change_is_reported_with_both_values() {
        let l_meta = tree(vec![MetadataEntry::container("title", "A", "mvhd")]);
        let r_meta = tree(vec![MetadataEntry::container("title", "B", "mvhd")]);

        let result = compare(
            &input("a.mp4", Some(&l_meta), None, None),
            &input("b.mp4", Some(&r_meta), None, None),
        );

        let title = result
            .metadata
            .iter()
            .find(|m| m.key == "title")
            .expect("title is compared");
        assert_eq!(
            title.difference,
            Difference::Different {
                left: "A".to_owned(),
                right: "B".to_owned(),
            }
        );
    }

    #[test]
    fn metadata_present_on_one_side_only_is_reported_as_one_sided() {
        let l_meta = tree(vec![MetadataEntry::container("title", "A", "mvhd")]);
        let r_meta = tree(vec![]);

        let result = compare(
            &input("a.mp4", Some(&l_meta), None, None),
            &input("b.mp4", Some(&r_meta), None, None),
        );

        let title = result
            .metadata
            .iter()
            .find(|m| m.key == "title")
            .expect("title is compared");
        assert!(
            matches!(title.difference, Difference::OnlyLeft { .. }),
            "a key only one file carries must be one-sided, got {:?}",
            title.difference
        );
    }

    #[test]
    fn silence_differences_are_reported_by_region_count() {
        let l = vec![silence_region(1_000), silence_region(500)];
        let r = vec![silence_region(1_000)];

        let result = compare(
            &input("a.mp4", None, None, Some(&l)),
            &input("b.mp4", None, None, Some(&r)),
        );

        assert!(result.silence.region_counts.is_different());
        assert!(
            !result.measured_differences().is_empty(),
            "a differing region count must appear as a measured difference"
        );
    }

    #[test]
    fn unmeasured_silence_is_not_reported_as_zero_silence() {
        let l = vec![silence_region(1_000)];

        let result = compare(
            &input("a.mp4", None, None, Some(&l)),
            &input("b.mp4", None, None, None),
        );

        assert!(
            result.silence.region_counts.is_not_comparable(),
            "silence never measured must not read as 'no silence'"
        );
    }

    #[test]
    fn a_comparison_of_the_same_input_is_consistent() {
        // Reproducibility (spec §77), checked through the comparison engine
        // itself rather than assumed.
        let meta = tree(vec![MetadataEntry::container("title", "A", "mvhd")]);
        let loud = integrated(-23.0);
        let l = input("a.mp4", Some(&meta), Some(&loud), None);
        let r = input("a.mp4", Some(&meta), Some(&loud), None);

        let result = compare_self(&l, &r);
        assert!(
            result.is_consistent(),
            "an asset must agree with itself: {:?}",
            result.unstable_axes
        );
        assert!(result.unstable_axes.is_empty());
    }

    #[test]
    fn a_self_comparison_flags_an_unstable_axis() {
        // The engine disagreeing with itself is a bug in the engine, and it must
        // be distinguishable from the two files genuinely differing.
        let l_meta = tree(vec![MetadataEntry::container("title", "A", "mvhd")]);
        let r_meta = tree(vec![MetadataEntry::container("title", "Z", "mvhd")]);

        let result = compare_self(
            &input("a.mp4", Some(&l_meta), None, None),
            &input("a.mp4", Some(&r_meta), None, None),
        );

        assert!(
            !result.is_consistent(),
            "the engine must not disagree with itself"
        );
        assert!(result.unstable_axes.contains(&ComparisonAxis::Metadata));
    }

    #[test]
    fn comparison_output_order_is_deterministic() {
        let l_meta = tree(vec![
            MetadataEntry::container("zebra", "1", "mvhd"),
            MetadataEntry::container("apple", "2", "mvhd"),
        ]);
        let r_meta = tree(vec![
            MetadataEntry::container("apple", "CHANGED", "mvhd"),
            MetadataEntry::container("zebra", "CHANGED", "mvhd"),
        ]);

        let first = compare(
            &input("a.mp4", Some(&l_meta), None, None),
            &input("b.mp4", Some(&r_meta), None, None),
        );
        let second = compare(
            &input("a.mp4", Some(&l_meta), None, None),
            &input("b.mp4", Some(&r_meta), None, None),
        );

        assert_eq!(first, second, "the same inputs must give the same output");

        // Sorted by axis first, then by field name within an axis: two runs must
        // emit the same list in the same order, and an unordered `HashMap` or an
        // unsorted `Vec` would break that.
        let axes: Vec<ComparisonAxis> = first.differences().iter().map(|f| f.axis).collect();
        let positions: Vec<usize> = axes.iter().map(|a| axis_order(*a)).collect();
        let mut sorted_positions = positions.clone();
        sorted_positions.sort_unstable();
        assert_eq!(
            positions, sorted_positions,
            "axes must be emitted in ComparisonAxis::ALL order"
        );

        let reported = first.differences();
        let by_axis = |axis: ComparisonAxis| -> Vec<String> {
            reported
                .iter()
                .filter(|f| f.axis == axis)
                .map(|f| f.field.clone())
                .collect()
        };
        let metadata = by_axis(ComparisonAxis::Metadata);
        let mut sorted_metadata = metadata.clone();
        sorted_metadata.sort();
        assert_eq!(metadata, sorted_metadata, "fields must be sorted by name");
    }

    #[test]
    fn the_tolerance_used_is_recorded_with_the_result() {
        // A reader must be able to judge "these agreed" rather than take it on
        // trust, which means knowing what was compared against.
        let l_loud = integrated(-23.0);
        let r_loud = integrated(-23.2);

        let result = compare(
            &input("a.mp4", None, Some(&l_loud), None),
            &input("b.mp4", None, Some(&r_loud), None),
        );

        assert_eq!(
            result.tolerances.loudness_lu,
            Tolerances::default().loudness_lu
        );
        match &result.loudness {
            WithinTolerance::Agree { tolerance, .. } => {
                assert_eq!(*tolerance, 0.5, "the tolerance must travel with the result");
            }
            other => panic!("expected agreement, got {other:?}"),
        }
    }

    #[test]
    fn a_strict_tolerance_turns_a_within_tolerance_gap_into_a_difference() {
        let l_loud = integrated(-23.0);
        let r_loud = integrated(-23.2);

        let result = compare_with(
            &input("a.mp4", None, Some(&l_loud), None),
            &input("b.mp4", None, Some(&r_loud), None),
            Tolerances {
                loudness_lu: 0.01,
                ..Tolerances::default()
            },
        );

        assert!(
            !result.loudness.agreed(),
            "a strict tolerance must be honoured"
        );
    }
}
