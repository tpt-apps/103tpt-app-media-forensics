//! Side-by-side comparison of two assets (spec \u00a782).
//!
//! # The engine's comparison, arranged for a screen
//!
//! Spec \u00a782 shows two panes with their frame numbers and PTS side by side and
//! asks for differences to be highlighted. The comparison itself is already
//! computed by `-rules::comparison` against every measured axis, with
//! `NotComparable` kept distinct from `Equal` and numeric axes judged against a
//! reported tolerance. This module does not recompute any of it.
//!
//! # There is no similarity score
//!
//! A re-mux and a transcode produce byte-different files; only one of them
//! changed anything a reviewer would care about. A single "87% similar" number
//! would discard exactly the axis-by-axis information this screen exists to
//! show, which is the reason the engine's `Comparison` has no score either.
//!
//! # "Equivalent" is stricter than "no differences"
//!
//! [`ComparisonView::verdict`] reports three outcomes, not two. Two files with no
//! measured differences but an uncomparable axis are *not* equivalent, because
//! nothing established they match on that axis \u2014 and a bare "no" would leave a
//! reader unable to tell a real disagreement from an axis this build never
//! measured. Those are different sentences.

use serde::{Deserialize, Serialize};

use tpt_app_media_forensics_model::comparison::{ComparisonAxis, Difference, StreamComparison};
use tpt_app_media_forensics_rules::comparison::WithinTolerance;

/// How one property compares, as a closed set the UI can switch on.
///
/// A tag rather than the engine's `Difference` enum: `Difference` is not `Eq`
/// (its numbers are floats) and, more importantly, the frontend should switch on
/// a closed set of words rather than re-derive the four-way distinction from
/// rendered strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DifferenceTag {
    /// Both sides were measured and agree.
    Equal,
    /// Both sides were measured and disagree.
    Different,
    /// Only the left asset declared it.
    OnlyLeft,
    /// Only the right asset declared it.
    OnlyRight,
    /// The property could not be measured on at least one side.
    NotComparable,
}

impl From<&Difference> for DifferenceTag {
    fn from(difference: &Difference) -> Self {
        match difference {
            Difference::Equal => Self::Equal,
            Difference::Different { .. } => Self::Different,
            Difference::OnlyLeft { .. } => Self::OnlyLeft,
            Difference::OnlyRight { .. } => Self::OnlyRight,
            Difference::NotComparable { .. } => Self::NotComparable,
        }
    }
}

impl DifferenceTag {
    /// Whether the property was actually measured on both sides.
    ///
    /// The distinction spec \u00a738 is built on: `NotComparable` is not `Equal`, and
    /// a screen must not style it as agreement.
    #[must_use]
    pub const fn was_measured(self) -> bool {
        matches!(self, Self::Equal | Self::Different)
    }

    /// Whether this verdict should be highlighted in the table.
    #[must_use]
    pub const fn is_interesting(self) -> bool {
        !matches!(self, Self::Equal)
    }

    /// The label drawn in the verdict column.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Equal => "SAME",
            Self::Different => "DIFFERS",
            Self::OnlyLeft => "LEFT ONLY",
            Self::OnlyRight => "RIGHT ONLY",
            Self::NotComparable => "NOT COMPARED",
        }
    }
}

/// A measurement compared against a tolerance rather than for equality.
///
/// Two runs of the same file differ in the last bits of a float, so an exact
/// comparison of loudness or scene counts would report a difference on every
/// pair and train a reviewer to ignore the axis. The tolerance used is carried
/// with every result so a reader can judge the claim rather than take it on
/// trust.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ToleranceRow {
    /// The outcome.
    pub verdict: ToleranceVerdict,
    /// The left-hand value.
    pub left: Option<f64>,
    /// The right-hand value.
    pub right: Option<f64>,
    /// The absolute gap between them.
    pub delta: Option<f64>,
    /// The tolerance the gap was judged against.
    ///
    /// `0.0` when nothing was measured, because there was no tolerance to judge
    /// against - not a tolerance of zero, which would be a claim that the two
    /// must match exactly.
    pub tolerance: f64,
    /// Which side could not be measured, when one could not.
    pub unmeasured_side: Option<UnmeasuredSide>,
}

/// Which side of a tolerance comparison produced no measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnmeasuredSide {
    /// The left-hand asset had no measurement.
    Left,
    /// The right-hand asset had no measurement.
    Right,
    /// Neither asset had a measurement.
    Both,
}

impl From<tpt_app_media_forensics_model::ComparisonSide> for UnmeasuredSide {
    fn from(side: tpt_app_media_forensics_model::ComparisonSide) -> Self {
        use tpt_app_media_forensics_model::ComparisonSide;
        match side {
            ComparisonSide::Left => Self::Left,
            ComparisonSide::Right => Self::Right,
        }
    }
}

/// The outcome of a tolerance comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToleranceVerdict {
    /// Both were measured and agree within the tolerance.
    Agree,
    /// Both were measured and differ by more than the tolerance.
    Diverge,
    /// At least one side could not be measured.
    Unmeasured,
}

impl ToleranceVerdict {
    /// The label drawn beside the numbers.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Agree => "WITHIN TOLERANCE",
            Self::Diverge => "OUTSIDE TOLERANCE",
            Self::Unmeasured => "NOT COMPARED",
        }
    }

    /// Whether this outcome blocks a claim of equivalence.
    #[must_use]
    pub const fn is_agreement(self) -> bool {
        matches!(self, Self::Agree)
    }
}

impl From<&tpt_app_media_forensics_rules::comparison::WithinTolerance> for ToleranceRow {
    /// Converts an engine tolerance result for the screen.
    fn from(value: &tpt_app_media_forensics_rules::comparison::WithinTolerance) -> Self {
        use tpt_app_media_forensics_rules::comparison::WithinTolerance;
        match value {
            WithinTolerance::Agree {
                lower,
                upper,
                delta,
                tolerance,
            } => Self {
                verdict: ToleranceVerdict::Agree,
                // The pair is reported as lower/upper rather than as left/right
                // because `Agree` has already discarded which side was which:
                // the engine's own contract is that they are within tolerance of
                // each other, and inventing an order would claim more than it
                // measured.
                left: Some(*lower),
                right: Some(*upper),
                delta: Some(*delta),
                tolerance: *tolerance,
                unmeasured_side: None,
            },
            WithinTolerance::Diverge {
                left,
                right,
                delta,
                tolerance,
            } => Self {
                verdict: ToleranceVerdict::Diverge,
                left: Some(*left),
                right: Some(*right),
                delta: Some(*delta),
                tolerance: *tolerance,
                unmeasured_side: None,
            },
            // `Unmeasured` names *which side* had no measurement rather than a
            // tolerance, and that is worth carrying through: "the right-hand
            // file could not be decoded" and "both could not be decoded" are
            // different situations with the same verdict, and the reason the
            // engine collapsed them is not one the screen should repeat.
            WithinTolerance::Unmeasured { side } => Self {
                verdict: ToleranceVerdict::Unmeasured,
                left: None,
                right: None,
                delta: None,
                tolerance: 0.0,
                unmeasured_side: Some((*side).into()),
            },
        }
    }
}

/// One property compared between two assets, arranged for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldRow {
    /// Which axis this property belongs to.
    pub axis: ComparisonAxis,
    /// What was compared.
    pub field: String,
    /// How it compares.
    pub verdict: DifferenceTag,
    /// The left-hand value, when there is one.
    pub left: Option<String>,
    /// The right-hand value, when there is one.
    pub right: Option<String>,
    /// Why the comparison could not be made, when it could not.
    pub reason: Option<String>,
}

impl FieldRow {
    /// Builds a row from the engine's field comparison.
    #[must_use]
    pub fn new(axis: ComparisonAxis, field: impl Into<String>, difference: &Difference) -> Self {
        let (left, right) = match difference {
            Difference::Equal => (None, None),
            Difference::Different { left, right } => (Some(left.clone()), Some(right.clone())),
            Difference::OnlyLeft { value } => (Some(value.clone()), None),
            Difference::OnlyRight { value } => (None, Some(value.clone())),
            Difference::NotComparable { .. } => (None, None),
        };

        Self {
            axis,
            field: field.into(),
            verdict: DifferenceTag::from(difference),
            left,
            right,
            reason: match difference {
                Difference::NotComparable { reason } => Some(reason.clone()),
                _ => None,
            },
        }
    }

    /// Whether this row is worth drawing attention to.
    #[must_use]
    pub fn is_highlighted(&self) -> bool {
        self.verdict.is_interesting()
    }
}

/// One axis' worth of comparison, with its rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AxisGroup {
    /// Which axis.
    pub axis: ComparisonAxis,
    /// The rows, in the engine's report order.
    pub rows: Vec<FieldRow>,
    /// Rows that were measured and disagree.
    pub differing: usize,
    /// Rows that could not be compared.
    pub uncomparable: usize,
}

impl AxisGroup {
    /// Whether anything on this axis is worth reporting.
    #[must_use]
    pub fn is_interesting(&self) -> bool {
        self.differing > 0 || self.uncomparable > 0
    }
}

/// One stream pair, as the screen shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamPair {
    /// Container index on the left.
    pub left_index: usize,
    /// Container index on the right.
    pub right_index: usize,
    /// Stream kind both sides agreed on.
    pub kind: tpt_app_media_forensics_model::StreamKind,
    /// The properties compared.
    pub fields: Vec<FieldRow>,
}

impl StreamPair {
    /// Builds a pair row from the engine's stream comparison.
    #[must_use]
    pub fn new(pair: &StreamComparison) -> Self {
        Self {
            left_index: pair.left_index,
            right_index: pair.right_index,
            kind: pair.kind,
            fields: pair
                .fields
                .iter()
                .map(|f| FieldRow::new(f.axis, f.field.clone(), &f.difference))
                .collect(),
        }
    }

    /// Number of properties that were measured and disagree.
    #[must_use]
    pub fn differing(&self) -> usize {
        self.fields
            .iter()
            .filter(|f| f.verdict == DifferenceTag::Different)
            .count()
    }
}

/// A stream present on only one side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnmatchedView {
    /// Which side it was found on.
    pub side: tpt_app_media_forensics_model::ComparisonSide,
    /// Its container index.
    pub index: usize,
    /// Its kind.
    pub kind: tpt_app_media_forensics_model::StreamKind,
    /// Its codec name.
    pub codec: String,
}

/// The overall outcome of a comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ComparisonVerdict {
    /// Measured on both sides and identical throughout.
    Equivalent,
    /// At least one measured axis disagrees.
    Different,
    /// Nothing disagrees, but at least one axis could not be compared.
    ///
    /// Deliberately distinct from `Different`. Two files that match everywhere
    /// they could be measured, and nowhere it could not, are not the same claim
    /// as two files that disagree.
    Incomplete,
    /// Nothing was measured at all.
    Unmeasured,
}

impl ComparisonVerdict {
    /// The label drawn above the table.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Equivalent => "EQUIVALENT",
            Self::Different => "DIFFERENT",
            Self::Incomplete => "NO DIFFERENCES MEASURED, BUT INCOMPLETE",
            Self::Unmeasured => "NOT COMPARED",
        }
    }
}

/// The side-by-side comparison view (spec §82).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComparisonView {
    /// Display name of the asset under examination.
    pub left_name: String,
    /// Display name of the asset compared against it.
    pub right_name: String,
    /// The overall outcome.
    pub verdict: ComparisonVerdict,
    /// Per-stream property rows.
    pub axes: Vec<AxisGroup>,
    /// Stream pairs that exist on both sides.
    pub streams: Vec<StreamPair>,
    /// Streams with no counterpart.
    pub unmatched: Vec<UnmatchedView>,
    /// How the stream layout as a whole compares.
    pub layout: DifferenceTag,
    /// Metadata keys that differ, are one-sided, or are otherwise notable.
    pub metadata: Vec<FieldRow>,
    /// Scene-change counts, compared within tolerance.
    pub scene_changes: ToleranceRow,
    /// Total silence, compared within tolerance.
    pub silence_totals: ToleranceRow,
    /// Integrated loudness, compared within tolerance.
    pub loudness: ToleranceRow,
}

impl ComparisonView {
    /// Builds the view from the engine's whole-file comparison.
    ///
    /// Takes the aggregate rather than re-deriving per-axis results, so every
    /// tolerance the engine applied travels with the result it judged.
    #[must_use]
    pub fn build(result: &tpt_app_media_forensics_rules::comparison::Comparison) -> Self {
        // Per-stream rows are grouped by axis so the renderer can lay them out
        // in the spec's order rather than in whatever order the engine emitted.
        let mut axes: Vec<AxisGroup> = ComparisonAxis::ALL
            .iter()
            .map(|axis| {
                let rows: Vec<FieldRow> = result
                    .streams
                    .streams
                    .iter()
                    .flat_map(|pair| &pair.fields)
                    .filter(|f| f.axis == *axis)
                    .map(|f| FieldRow::new(f.axis, f.field.clone(), &f.difference))
                    .collect();
                AxisGroup {
                    axis: *axis,
                    differing: rows.iter().filter(|r| r.verdict.is_interesting()).count(),
                    uncomparable: rows
                        .iter()
                        .filter(|r| r.verdict == DifferenceTag::NotComparable)
                        .count(),
                    rows,
                }
            })
            .collect();
        // Empty axes are dropped rather than rendered blank, for the reason the
        // timeline does the same thing: a blank section and an absent one mean
        // different things, and only the first is a measurement.
        axes.retain(|group| !group.rows.is_empty());

        let metadata = result
            .metadata
            .iter()
            .map(|m| FieldRow::new(ComparisonAxis::Metadata, m.key.clone(), &m.difference))
            .collect();

        // Every source of "could not measure", counted together.
        //
        // The first version of this read `measured_differences()` alone, and that
        // is a list of *field* comparisons - it does not include the stream
        // layout, the metadata axes, or the tolerance axes. So a file whose
        // container could not be read produced no field comparisons at all,
        // counted zero uncomparables, and was reported EQUIVALENT. Two files, one
        // of which was never opened, described as interchangeable.
        //
        // The engine's own `is_equivalent` is strict about this and catches it;
        // the counts below exist to separate *which* of the two weaker outcomes
        // applies, and are only trusted once every source is included.
        let uncomparable = result
            .streams
            .streams
            .iter()
            .flat_map(|pair| &pair.fields)
            .filter(|f| f.difference.is_not_comparable())
            .count()
            + usize::from(result.streams.layout.is_not_comparable())
            + usize::from(result.scene.changes.is_not_comparable())
            + usize::from(result.silence.region_counts.is_not_comparable())
            + result
                .metadata
                .iter()
                .filter(|m| m.difference.is_not_comparable())
                .count()
            + usize::from(result.scene.changes.is_not_comparable())
            + usize::from(matches!(
                result.scene.change_counts,
                WithinTolerance::Unmeasured { .. }
            ))
            + usize::from(result.silence.region_counts.is_not_comparable())
            + usize::from(matches!(
                result.silence.totals,
                WithinTolerance::Unmeasured { .. }
            ))
            + usize::from(matches!(
                result.loudness,
                WithinTolerance::Unmeasured { .. }
            ));

        // `StreamComparison::differences()` filters on `is_different()`, but the
        // check is written out explicitly here because getting it wrong is
        // invisible: an uncomparable row counted as a disagreement reports two
        // identical files as DIFFERENT, and a reviewer reads that as a finding.
        // "Measured and disagree", never merely "not equal to Equal".
        //
        // `scene.changes` is `NotComparable` whenever neither side ran Tier-2
        // pixel analysis, which is the common case for the patent-encumbered
        // codecs this build does not decode. The first version of this checked
        // `!difference.is_equal()`, which is true for `NotComparable` - so two
        // files that were byte-identical and had no colour, no scene data and no
        // audio compared as DIFFERENT. Every clause below tests for a *measured*
        // disagreement, which is the only thing that makes that word true.
        let measured_disagreement = |difference: &Difference| difference.is_different();

        let has_difference = result
            .streams
            .streams
            .iter()
            .flat_map(|pair| &pair.fields)
            .any(|f| measured_disagreement(&f.difference))
            || !result.unmatched().is_empty()
            || result
                .metadata
                .iter()
                .any(|m| measured_disagreement(&m.difference))
            || measured_disagreement(&result.scene.changes)
            || measured_disagreement(&result.silence.region_counts)
            || matches!(result.scene.change_counts, WithinTolerance::Diverge { .. })
            || matches!(result.silence.totals, WithinTolerance::Diverge { .. })
            || matches!(result.loudness, WithinTolerance::Diverge { .. });

        // A real disagreement is the stronger statement, so it outranks an axis
        // nobody measured. `Incomplete` is deliberately distinct from
        // `Equivalent`: nothing disagrees, but nothing established agreement
        // either.
        let verdict = match (has_difference, uncomparable) {
            (true, _) => ComparisonVerdict::Different,
            (false, 0) => ComparisonVerdict::Equivalent,
            (false, _) => ComparisonVerdict::Incomplete,
        };

        Self {
            left_name: result.left_name.clone(),
            right_name: result.right_name.clone(),
            verdict,
            axes,
            streams: result.streams.streams.iter().map(StreamPair::new).collect(),
            unmatched: result
                .streams
                .unmatched
                .iter()
                .map(|u| UnmatchedView {
                    side: u.side,
                    index: u.index,
                    kind: u.kind,
                    codec: u.codec.clone(),
                })
                .collect(),
            layout: DifferenceTag::from(&result.streams.layout),
            metadata,
            scene_changes: ToleranceRow::from(&result.scene.change_counts),
            silence_totals: ToleranceRow::from(&result.silence.totals),
            loudness: ToleranceRow::from(&result.loudness),
        }
    }

    /// Returns the rows for one axis.
    #[must_use]
    pub fn row(&self, axis: ComparisonAxis) -> Option<&AxisGroup> {
        self.axes.iter().find(|group| group.axis == axis)
    }

    /// Total properties measured and found to disagree.
    #[must_use]
    pub fn total_differences(&self) -> usize {
        self.axes.iter().map(|a| a.differing).sum()
    }

    /// Whether the two sides agree on everything that was measured.
    #[must_use]
    pub fn agrees_on_everything_measured(&self) -> bool {
        self.verdict == ComparisonVerdict::Equivalent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_app_media_forensics_metadata::{MetadataTree, Scope};
    use tpt_app_media_forensics_model::{
        compare_streams, CodecInfo, StreamAnalysis, StreamKind, StreamTiming, Timebase,
    };
    use tpt_app_media_forensics_rules::comparison::{
        compare, ComparisonInput, MetadataComparison, SceneComparison, SilenceComparison,
        WithinTolerance,
    };

    fn stream(index: usize, kind: StreamKind, width: Option<u32>) -> StreamAnalysis {
        StreamAnalysis {
            index: u32::try_from(index).unwrap_or(u32::MAX),
            kind,
            language: None,
            codec: CodecInfo::new(if kind == StreamKind::Video {
                "av01"
            } else {
                "opus"
            }),
            timing: StreamTiming {
                timebase: Timebase::from_ticks_per_second(1_000),
                start_time: tpt_app_media_forensics_model::MediaTime::ZERO,
                duration: None,
                measured_duration: None,
                edit_list_offset: None,
            },
            video: width.map(|w| tpt_app_media_forensics_model::VideoFormat {
                coded_width: w,
                coded_height: 240,
                display_width: Some(w),
                display_height: Some(240),
                frame_rate: Some(tpt_app_media_forensics_model::Rational::from_integer(25)),
                sample_aspect_ratio: None,
                display_aspect_ratio: None,
                rotation_degrees: Some(0),
                pixel_format: tpt_app_media_forensics_model::PixelFormat::new(
                    "yuv420p",
                    tpt_app_media_forensics_model::ChromaSubsampling::Cs420,
                    8,
                ),
                colour: Default::default(),
                is_hdr: false,
            }),
            audio: None,
            packet_count: None,
        }
    }

    /// A comparison input over two explicitly-listed streams.
    fn input<'a>(name: &'a str, streams: &'a [StreamAnalysis]) -> ComparisonInput<'a> {
        ComparisonInput {
            name,
            streams: Some(streams),
            metadata: None,
            scene: None,
            silence: Some(&[]),
            loudness: None,
        }
    }

    #[test]
    fn an_uncomparable_property_is_never_reported_as_equal() {
        // Spec \u00a738's central distinction, carried all the way to the screen.
        // Styling `NotComparable` as agreement would tell a reviewer two files
        // matched on a property neither of them declared.
        let row = FieldRow::new(
            ComparisonAxis::Colour,
            "primaries",
            &Difference::NotComparable {
                reason: "neither asset reported this property".to_owned(),
            },
        );

        assert_eq!(row.verdict, DifferenceTag::NotComparable);
        assert!(!row.verdict.was_measured());
        assert_ne!(row.verdict, DifferenceTag::Equal);
        assert!(row.reason.is_some(), "the reason must reach the analyst");
        assert_eq!(row.verdict.label(), "NOT COMPARED");
    }

    #[test]
    fn a_property_only_one_side_declared_shows_which_side() {
        let row = FieldRow::new(
            ComparisonAxis::AudioFormat,
            "channel count",
            &Difference::OnlyLeft {
                value: "6".to_owned(),
            },
        );
        assert_eq!(row.verdict, DifferenceTag::OnlyLeft);
        assert_eq!(row.left.as_deref(), Some("6"));
        assert_eq!(row.right, None);
    }

    #[test]
    fn an_equal_property_is_not_highlighted() {
        // A comparison where everything agrees is meaningful, but highlighting
        // it would bury the real differences.
        let row = FieldRow::new(ComparisonAxis::Container, "format", &Difference::Equal);
        assert!(!row.is_highlighted());
        assert!(row.verdict.was_measured());
    }

    #[test]
    fn two_files_differing_in_resolution_report_it_and_not_an_equivalence() {
        let left = vec![stream(0, StreamKind::Video, Some(1920))];
        let right = vec![stream(0, StreamKind::Video, Some(1280))];

        let view = ComparisonView::build(&compare(&input("a.mov", &left), &input("b.mov", &right)));
        assert_eq!(view.verdict, ComparisonVerdict::Different);
        assert!(!view.agrees_on_everything_measured());
        assert_eq!(view.verdict.label(), "DIFFERENT");
    }

    #[test]
    fn identical_files_are_reported_as_equivalent() {
        let left = vec![stream(0, StreamKind::Video, Some(1920))];
        let right = vec![stream(0, StreamKind::Video, Some(1920))];

        let view = ComparisonView::build(&compare(&input("a.mov", &left), &input("b.mov", &right)));
        // Two files with no colour declared produce `NotComparable` rows on the
        // colour axis, so the honest verdict is INCOMPLETE rather than
        // EQUIVALENT: nothing disagrees, but nothing established agreement on
        // colour either. That is the engine's own strict reading, inherited.
        assert_eq!(view.verdict, ComparisonVerdict::Incomplete);
        assert!(!view.agrees_on_everything_measured());
        assert_eq!(
            view.verdict.label(),
            "NO DIFFERENCES MEASURED, BUT INCOMPLETE"
        );
    }

    #[test]
    fn two_byte_identical_files_are_not_reported_as_different() {
        // The regression this pins. The first version of the verdict checked
        // `!difference.is_equal()`, which is true for `NotComparable` as well as
        // for `Different`. Two files that agree on everything *and* declared
        // nothing comparable - no colour, no scene data, no audio, no Tier-2 run -
        // were therefore reported DIFFERENT.
        //
        // That is the worst possible failure for this screen: a reviewer told
        // two identical files differ, with a table of axes behind it, and no way
        // to see that every row actually said "not measured".
        let left = vec![stream(0, StreamKind::Video, Some(1920))];
        let view = ComparisonView::build(&compare(&input("a.mov", &left), &input("a.mov", &left)));

        assert_ne!(
            view.verdict,
            ComparisonVerdict::Different,
            "an uncomparable axis must never be reported as a disagreement"
        );
    }

    #[test]
    fn an_unmeasurable_axis_is_counted_as_incomparable_not_as_agreement() {
        // The mirror image: the same `NotComparable` rows that must not read as
        // a difference must also stop the verdict reading as equivalence.
        let left = vec![stream(0, StreamKind::Video, Some(1920))];
        let view = ComparisonView::build(&compare(&input("a.mov", &left), &input("a.mov", &left)));

        assert_eq!(view.verdict, ComparisonVerdict::Incomplete);
        assert!(
            !view.agrees_on_everything_measured(),
            "nothing was established about colour, so nothing may claim agreement"
        );
    }

    #[test]
    fn an_axis_the_containers_declared_nothing_for_is_reported_not_comparable() {
        // `compare_streams` emits a row for every axis on every paired stream,
        // including ones neither container declared - so a stream with no colour
        // information yields `NotComparable` rows rather than an absent axis.
        //
        // That is the correct shape, and the test originally asserted the
        // opposite. It would have been asserting a defect in the view: dropping
        // those rows would hide the fact that colour was never comparable, which
        // is exactly what a reviewer checking a suspicious grade needs to see.
        let left = vec![stream(0, StreamKind::Video, Some(1920))];
        let view = ComparisonView::build(&compare(&input("a.mov", &left), &input("a.mov", &left)));

        let colour = view.row(ComparisonAxis::Colour).expect("colour rows exist");
        assert!(
            colour
                .rows
                .iter()
                .all(|r| r.verdict.was_measured() || r.reason.is_some()),
            "an uncomparable row must carry its reason: {colour:?}"
        );
    }

    #[test]
    fn an_axis_with_no_streams_at_all_is_omitted() {
        // With nothing to compare there is nothing to render. A blank section
        // would mean "looked, found nothing" and an absent one means "there was
        // nothing to look at" - different states.
        let left = vec![stream(0, StreamKind::Video, Some(1920))];
        let view = ComparisonView::build(&compare(&input("a.mov", &left), &input("a.mov", &left)));
        assert!(
            view.axes.iter().all(|a| !a.rows.is_empty()),
            "an empty axis group must never be rendered"
        );
    }

    #[test]
    fn a_tolerance_agreement_is_not_reported_as_exact_equality() {
        // Two runs of the same file differ in the last bits of a float. The
        // tolerance and the gap travel with the result so a reader can judge
        // the claim rather than take it on trust.
        let row = ToleranceRow::from(&WithinTolerance::Agree {
            lower: -23.4,
            upper: -23.2,
            delta: 0.2,
            tolerance: 0.5,
        });

        assert_eq!(row.verdict, ToleranceVerdict::Agree);
        assert_eq!(row.tolerance, 0.5);
        assert_eq!(row.delta, Some(0.2));
        assert!(row.verdict.is_agreement());
    }

    #[test]
    fn a_tolerance_divergence_is_reported_with_both_values() {
        let row = ToleranceRow::from(&WithinTolerance::Diverge {
            left: -24.0,
            right: -20.0,
            delta: 4.0,
            tolerance: 0.5,
        });

        assert_eq!(row.verdict, ToleranceVerdict::Diverge);
        assert_eq!(row.left, Some(-24.0));
        assert_eq!(row.right, Some(-20.0));
        assert!(!row.verdict.is_agreement());
        assert_eq!(row.verdict.label(), "OUTSIDE TOLERANCE");
    }

    #[test]
    fn an_unmeasured_tolerance_is_not_agreement() {
        // The failure this guards against: "within tolerance" printed for a
        // measurement that was never taken.
        let row = ToleranceRow::from(&WithinTolerance::Unmeasured {
            side: tpt_app_media_forensics_model::ComparisonSide::Right,
        });
        assert_eq!(row.verdict, ToleranceVerdict::Unmeasured);
        assert!(!row.verdict.is_agreement());
        assert_eq!(row.left, None);
    }

    #[test]
    fn a_dropped_audio_track_is_one_unmatched_stream_not_three_differences() {
        // Streams pair by kind and position, which is the engine's rule; this
        // test pins that the view preserves it rather than zipping by index.
        let left = vec![
            stream(0, StreamKind::Video, Some(1920)),
            stream(1, StreamKind::Audio, None),
        ];
        let right = vec![stream(0, StreamKind::Video, Some(1920))];

        let view = ComparisonView::build(&compare(&input("a.mov", &left), &input("b.mov", &right)));
        assert_eq!(view.unmatched.len(), 1);
        assert_eq!(view.unmatched[0].kind, StreamKind::Audio);
        assert_eq!(view.verdict, ComparisonVerdict::Different);
    }

    #[test]
    fn streams_that_could_not_be_read_are_not_reported_as_a_layout_difference() {
        // "Unreadable" and "different" are different states of the same file.
        let mut left = input("a.mov", &[]);
        left.streams = None;
        let right = vec![stream(0, StreamKind::Video, Some(1920))];

        let view = ComparisonView::build(&compare(&left, &input("b.mov", &right)));
        assert_eq!(view.layout, DifferenceTag::NotComparable);
        assert!(view.streams.is_empty());
        // `is_equivalent` is strict in the engine for exactly this case, and the
        // verdict inherits that: an unreadable side must never read as
        // equivalent, whatever else agreed.
        assert!(
            !view.agrees_on_everything_measured(),
            "an unreadable side must never read as equivalent"
        );
    }

    #[test]
    fn metadata_differences_reach_the_screen() {
        let left_tree = MetadataTree::new(vec![
            tpt_app_media_forensics_metadata::MetadataEntry::container("encoder", "Lavf58", "meta"),
        ]);
        let right_tree = MetadataTree::new(vec![
            tpt_app_media_forensics_metadata::MetadataEntry::container("encoder", "Lavf60", "meta"),
        ]);

        let mut left = input("a.mov", &[]);
        left.metadata = Some(&left_tree);
        let mut right = input("b.mov", &[]);
        right.metadata = Some(&right_tree);

        let view = ComparisonView::build(&compare(&left, &right));
        assert_eq!(view.verdict, ComparisonVerdict::Different);
        assert!(
            view.metadata
                .iter()
                .any(|m| m.verdict == DifferenceTag::Different),
            "the encoder difference must be reported: {view:?}"
        );
    }

    #[test]
    fn the_tolerance_axes_survive_the_conversion() {
        // Every tolerance the engine applied travels with the result it judged.
        // A screen that dropped them would be claiming an exactness the engine
        // never asserted.
        let result = tpt_app_media_forensics_rules::comparison::Comparison {
            left_name: "a.mov".to_owned(),
            right_name: "b.mov".to_owned(),
            streams: compare_streams(None, None),
            metadata: vec![MetadataComparison {
                key: "encoder".to_owned(),
                scope: Scope::Container,
                track_index: None,
                difference: Difference::Equal,
            }],
            scene: SceneComparison {
                changes: Difference::Equal,
                change_counts: WithinTolerance::Agree {
                    lower: 9.0,
                    upper: 10.0,
                    delta: 1.0,
                    tolerance: 1.0,
                },
            },
            silence: SilenceComparison {
                totals: WithinTolerance::Unmeasured {
                    side: tpt_app_media_forensics_model::ComparisonSide::Left,
                },
                region_counts: Difference::NotComparable {
                    reason: "neither side decoded".to_owned(),
                },
            },
            loudness: WithinTolerance::Diverge {
                left: -24.0,
                right: -20.0,
                delta: 4.0,
                tolerance: 0.5,
            },
            tolerances: tpt_app_media_forensics_rules::comparison::Tolerances::default(),
        };

        let view = ComparisonView::build(&result);
        assert_eq!(view.scene_changes.verdict, ToleranceVerdict::Agree);
        assert_eq!(view.scene_changes.tolerance, 1.0);
        assert_eq!(view.silence_totals.verdict, ToleranceVerdict::Unmeasured);
        assert_eq!(view.loudness.verdict, ToleranceVerdict::Diverge);
        assert_eq!(view.loudness.delta, Some(4.0));
    }

    #[test]
    fn the_view_round_trips_through_the_ipc_boundary() {
        let left = vec![stream(0, StreamKind::Video, Some(1920))];
        let right = vec![stream(0, StreamKind::Video, Some(1280))];
        let view = ComparisonView::build(&compare(&input("a.mov", &left), &input("b.mov", &right)));

        let json = serde_json::to_string(&view).expect("encodes");
        let decoded: ComparisonView = serde_json::from_str(&json).expect("decodes");
        assert_eq!(decoded, view);
    }

    #[test]
    fn every_difference_tag_has_a_distinct_label() {
        let tags = [
            DifferenceTag::Equal,
            DifferenceTag::Different,
            DifferenceTag::OnlyLeft,
            DifferenceTag::OnlyRight,
            DifferenceTag::NotComparable,
        ];
        let mut labels: Vec<&str> = tags.iter().map(|t| t.label()).collect();
        let before = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), before, "labels must be unique: {labels:?}");
    }
}
