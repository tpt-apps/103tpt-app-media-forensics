//! Property tests for the timestamp scanners (spec §24, §75-§77).
//!
//! # Why these scanners
//!
//! `scan_presentation` and `scan_decode` are the only analysis in the engine whose
//! input is an *arbitrary sequence of times* rather than a file. Every other
//! stage has to survive hostile bytes; these have to be right about hostile
//! *numbers* — a negative time, a repeated instant, a regression, an enormous
//! gap, a zero-length sequence.
//!
//! Example tests cannot establish the properties that matter here. They can show
//! that one backwards pair is reported, which a two-line unit test already does.
//! What they cannot show is the **completeness** direction: that *every* condition
//! in the input is reported. A scanner that reported one anomaly correctly and
//! then stopped scanning would pass every example test in the crate and be
//! useless in a forensic report, because the finding would be missing from a file
//! that genuinely has the defect.
//!
//! Both directions are asserted below. Soundness — nothing reported that the file
//! does not show — matters just as much, because a false anomaly is a claim put
//! in front of an examiner that the evidence does not support.
//!
//! # Determinism
//!
//! No randomness beyond `proptest`'s own seeded generator, and no tolerance
//! derived from the input: a property that varied its own threshold could pass by
//! never being challenged. Every tolerance here is a fixed constant, so a failure
//! is reproducible from the seed `proptest` prints.

use proptest::prelude::*;

use tpt_app_media_forensics_model::MediaTime;
use tpt_app_media_forensics_timing::pts_dts::{
    scan_decode, scan_presentation, Anomaly, TimestampReport,
};

/// Tolerance every property below is run against.
///
/// Constant rather than derived: a threshold taken from the sequence under test
/// would make "no gap is reported" true by construction for any input it chose.
/// One millisecond is far below the 20-100 ms frame durations used as sequences
/// and far above the jitter a single step accumulates.
const TOLERANCE: MediaTime = MediaTime::from_micros(1_000);

/// Arbitrary timestamps, including negatives and repeats.
fn times() -> impl Strategy<Value = Vec<MediaTime>> {
    // `prop_map`, not `map`: on a `VecStrategy`, `map` maps the *strategy* rather
    // than the value it produces, so `map` here would build a strategy of
    // strategies and never compile usefully.
    prop::collection::vec(-2_000_000i64..2_000_000, 0..40)
        .prop_map(|micros: Vec<i64>| micros.into_iter().map(MediaTime::from_micros).collect())
}

/// Indices of every anomaly satisfying `wanted`, in the order reported.
fn indices_of(report: &TimestampReport, wanted: fn(&Anomaly) -> bool) -> Vec<usize> {
    report
        .anomalies
        .iter()
        .filter(|a| wanted(a))
        .map(Anomaly::index)
        .collect()
}

fn is_non_monotonic_pts(a: &Anomaly) -> bool {
    matches!(a, Anomaly::NonMonotonicPts { .. })
}

fn is_overlap(a: &Anomaly) -> bool {
    matches!(a, Anomaly::Overlap { .. })
}

fn is_negative(a: &Anomaly) -> bool {
    matches!(a, Anomaly::NegativeTimestamp { .. })
}

fn is_gap(a: &Anomaly) -> bool {
    matches!(a, Anomaly::Gap { .. })
}

fn is_non_monotonic_dts(a: &Anomaly) -> bool {
    matches!(a, Anomaly::NonMonotonicDts { .. })
}

/// Indices of every backwards step in `sequence`.
fn backwards_steps(sequence: &[MediaTime]) -> Vec<usize> {
    (1..sequence.len())
        .filter(|&i| sequence[i] < sequence[i - 1])
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// A regularly spaced track is clean.
    ///
    /// The property that keeps the rules quiet on healthy files. A 20-100 ms
    /// cadence is what a real track looks like, and any scanner reporting a gap,
    /// an overlap or a regression on one would fill a forensic report with noise.
    #[test]
    fn a_regular_track_reports_no_anomalies(step in 20_000i64..100_000, count in 2usize..40) {
        let sequence: Vec<MediaTime> = (0..count)
            .map(|i| MediaTime::from_micros(i as i64 * step))
            .collect();

        let report = scan_presentation(&sequence, TOLERANCE);
        prop_assert!(
            report.is_clean(),
            "a strictly increasing, evenly spaced track must report nothing, got {:?}",
            report.anomalies
        );

        let decode = scan_decode(&sequence);
        prop_assert!(
            decode.is_clean(),
            "the same track is clean in decode order too, got {:?}",
            decode.anomalies
        );
    }

    /// Every reported presentation anomaly is backed by the input.
    ///
    /// Soundness. A regression reported where there is none would put a false
    /// claim in front of an examiner, which is worse than reporting nothing: the
    /// whole product rests on a finding meaning something was observed.
    #[test]
    fn every_reported_presentation_anomaly_is_backed_by_the_input(sequence in times()) {
        let report = scan_presentation(&sequence, TOLERANCE);

        for anomaly in &report.anomalies {
            let index = anomaly.index();
            prop_assert!(
                index < sequence.len(),
                "anomaly at {index} but only {} samples were given",
                sequence.len()
            );

            match *anomaly {
                Anomaly::NonMonotonicPts { index, previous, observed } => {
                    prop_assert_eq!(observed, sequence[index], "the reported value is the file's");
                    prop_assert_eq!(previous, sequence[index - 1], "the reported value is the file's");
                    prop_assert!(
                        sequence[index] < sequence[index - 1],
                        "a regression was reported but {index} does not go backwards"
                    );
                }
                Anomaly::Overlap { index, size } => {
                    // Two samples sharing one instant overlap for no duration at
                    // all, which is what `size` is carrying. Asserting it is zero
                    // pins the meaning of the field: a reader seeing `size` must
                    // not take it as "how far they overlap", which would be a
                    // different number entirely.
                    prop_assert_eq!(
                        size,
                        MediaTime::ZERO,
                        "an overlap is two samples at one instant, so its size is zero"
                    );
                    prop_assert_eq!(
                        sequence[index].signed_diff(sequence[index - 1]),
                        size,
                        "the reported size must be the difference between the two samples"
                    );
                }
                Anomaly::NegativeTimestamp { index, observed } => {
                    prop_assert_eq!(observed, sequence[index], "the reported value is the file's");
                    prop_assert!(
                        observed.as_micros() < 0,
                        "a negative time was reported but {observed:?} is not negative"
                    );
                }
                Anomaly::Gap { index, size } => {
                    prop_assert!(size.as_micros() > 0, "a gap of no size was reported");
                    prop_assert!(
                        index >= 1,
                        "a gap is named by the sample after it, so it cannot be the first"
                    );
                }
                // `scan_decode` is a separate function; it must never produce this.
                Anomaly::NonMonotonicDts { .. } => {
                    prop_assert!(false, "scan_presentation must not report a decode anomaly");
                }
            }
        }
    }

    /// Every backwards step and every repeated instant is reported, and nothing
    /// else is.
    ///
    /// Completeness — the direction example tests cannot reach. For each pair of
    /// neighbours exactly one of three things holds: the times go backwards, they
    /// are identical, or they advance. The first two must produce an anomaly and
    /// the third must not produce an overlap. A scanner that stopped after the
    /// first few samples would satisfy every example test in the crate while
    /// dropping the defect from the back half of a long file.
    #[test]
    fn every_backwards_or_duplicated_pair_is_reported(sequence in times()) {
        let report = scan_presentation(&sequence, TOLERANCE);

        let expected_overlaps: Vec<usize> = (1..sequence.len())
            .filter(|&i| sequence[i] == sequence[i - 1])
            .collect();

        prop_assert_eq!(
            indices_of(&report, is_non_monotonic_pts),
            backwards_steps(&sequence),
            "every backwards step must be reported, and nothing else"
        );
        prop_assert_eq!(
            indices_of(&report, is_overlap),
            expected_overlaps,
            "every repeated instant must be reported, and nothing else"
        );
    }

    /// Negative times are reported exactly, in both directions.
    ///
    /// Stated both ways because spec §24 treats a negative presentation time as
    /// *legitimate* before an edit list is applied rather than as damage. It is
    /// therefore an observation and not a grade, and reporting one for a file
    /// whose times are all positive would be inventing a finding.
    #[test]
    fn negative_times_are_reported_exactly(sequence in times()) {
        let report = scan_presentation(&sequence, TOLERANCE);

        let expected: Vec<usize> = sequence
            .iter()
            .enumerate()
            .filter(|(_, t)| t.as_micros() < 0)
            .map(|(i, _)| i)
            .collect();
        prop_assert_eq!(
            indices_of(&report, is_negative),
            expected,
            "a negative time must be reported for every sample that is negative, and \
             for no sample that is not"
        );
    }

    /// Decode order: a regression is reported, and a non-regression never is.
    ///
    /// Kept as its own property because `scan_decode` and `scan_presentation` are
    /// separate functions that must not be confused. The decode scanner reports
    /// only one kind of anomaly: decode order has no concept of a gap, because
    /// every packet must be decodable in sequence, so uneven spacing says nothing
    /// about validity.
    #[test]
    fn decode_regressions_are_reported_exactly(sequence in times()) {
        let report = scan_decode(&sequence);

        prop_assert_eq!(
            indices_of(&report, is_non_monotonic_dts),
            backwards_steps(&sequence),
            "every backwards decode step must be reported, and nothing else"
        );
        prop_assert!(
            indices_of(&report, is_non_monotonic_pts).is_empty(),
            "the decode scanner reported a presentation anomaly"
        );
    }

    /// A gap is only ever reported when there really is one.
    ///
    /// Gaps use a *modal* delta threshold rather than an absolute spacing, which is
    /// the subtlest part of this scanner: the expected frame duration is a
    /// property of the sequence being scanned. That is deliberate — a track that
    /// is mostly 40 ms with one 10-second gap should not have its expected
    /// duration skewed by the gap — but it also means a threshold computed wrongly
    /// would report a gap on every frame of a variable-frame-rate file.
    ///
    /// So the property is not "no gaps here", which any sequence that legitimately
    /// drifts would fail; it is that every gap reported is a real, non-empty
    /// discontinuity that the input contains.
    #[test]
    fn every_gap_reported_is_a_real_discontinuity(sequence in times()) {
        let report = scan_presentation(&sequence, TOLERANCE);

        for anomaly in report.anomalies.iter().filter(|a| is_gap(a)) {
            let Anomaly::Gap { index, size } = *anomaly else {
                unreachable!("filtered to gaps above")
            };
            prop_assert!(size.as_micros() > 0, "a gap of no size was reported");
            prop_assert!(
                index >= 1 && index < sequence.len(),
                "a gap is named by the sample after it, so {index} is out of range"
            );
            // The pair the gap describes genuinely advances, and by more than the
            // gap itself: otherwise the number is a restatement of the step rather
            // than the excess over the expected duration.
            let step = sequence[index].signed_diff(sequence[index - 1]);
            prop_assert!(
                step.as_micros() > 0,
                "a gap was reported for a pair that does not advance"
            );
            prop_assert!(
                size.as_micros() < step.as_micros(),
                "a gap of {size:?} cannot exceed the {step:?} step that produced it"
            );
        }
    }

    /// Anomalies come back in sample order.
    ///
    /// The scanners sort before returning, and the report renders and persists the
    /// list exactly as given. An unsorted list would print an error timeline that
    /// runs backwards, which an examiner would read as the file being wrong in a
    /// second way it is not.
    #[test]
    fn anomalies_are_returned_in_sample_order(sequence in times()) {
        let indices: Vec<usize> = scan_presentation(&sequence, TOLERANCE)
            .anomalies
            .iter()
            .map(Anomaly::index)
            .collect();
        let mut sorted = indices.clone();
        sorted.sort_unstable();
        prop_assert_eq!(indices, sorted, "anomalies must be in sample order");
    }

    /// The reported sample count is the length of what was scanned.
    ///
    /// The count is rendered as "scanned N samples", so a scanner that lost or
    /// invented one would misstate how much of the file was examined.
    #[test]
    fn the_sample_count_is_what_was_scanned(sequence in times()) {
        prop_assert_eq!(
            scan_presentation(&sequence, TOLERANCE).sample_count,
            sequence.len()
        );
        prop_assert_eq!(scan_decode(&sequence).sample_count, sequence.len());
    }

    /// Appending a sample can never hide a regression already reported.
    ///
    /// Monotonicity under extension, and the reason it is worth asserting: the gap
    /// threshold is the *mode* of the inter-sample deltas, so appending a sample
    /// can change the expected frame duration and move that threshold. A threshold
    /// that could move far enough to stop reporting a real discontinuity would be
    /// a defect visible only on long files — exactly the length a forensic
    /// examination usually involves.
    #[test]
    fn extending_a_sequence_never_hides_an_earlier_regression(
        prefix in times(),
        extra in -2_000_000i64..2_000_000,
    ) {
        let mut sequence = prefix;
        prop_assume!(sequence.len() >= 2);

        let before = indices_of(&scan_presentation(&sequence, TOLERANCE), is_non_monotonic_pts);
        sequence.push(MediaTime::from_micros(extra));
        let after = indices_of(&scan_presentation(&sequence, TOLERANCE), is_non_monotonic_pts);

        for index in before {
            prop_assert!(
                after.contains(&index),
                "regression at {index} disappeared when a sample was appended"
            );
        }
    }
}
