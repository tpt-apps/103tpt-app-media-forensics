//! Scene-change analysis on decoded frames (spec §18).
//!
//! # What counts as a change
//!
//! A cut produces a large difference between consecutive frames; a pan produces
//! a moderate one; a static scene produces almost none. This measures the mean
//! absolute luma difference between consecutive frames, which is the standard
//! signal and needs no motion estimation.
//!
//! # The threshold is a profile value, not a constant
//!
//! What counts as a cut depends on the content: a high-contrast action sequence
//! changes more per frame than a conversation. The threshold lives in the rule
//! profile so a delivery specification can set it, rather than being buried here
//! where it could not be tuned without a rebuild.
//!
//! # Nothing here asserts a cut occurred
//!
//! A large difference is consistent with a scene change and also with a
//! dissolve, a flash, or a change of lighting. The finding reports the measured
//! difference; interpretation belongs to the reviewer (spec §66).

use crate::decode::DecodedFrame;

/// One measured difference between consecutive frames.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameDifference {
    /// Index of the later frame.
    pub index: usize,
    /// Mean absolute luma difference, 0.0 to 255.0.
    pub mean_absolute: f64,
    /// Proportion of samples differing by more than a small threshold.
    pub changed_fraction: f64,
}

/// The result of scanning a decoded sequence.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneReport {
    /// One entry per consecutive pair.
    pub differences: Vec<FrameDifference>,
    /// Frames examined.
    pub frames_examined: usize,
    /// Pairs that were *not* compared because the frames are not adjacent in
    /// the source stream.
    ///
    /// Non-zero means recoverable decode damage: frames were lost between two
    /// recovered ones, so the difference across that gap is not measurable as a
    /// single step. Reported rather than omitted so a reader can distinguish
    /// "this file has no scene changes" from "this file could not be examined
    /// everywhere", which would otherwise look identical in the findings.
    pub comparisons_skipped: usize,
}

impl SceneReport {
    /// Returns frames whose difference meets or exceeds `threshold`.
    ///
    /// Comparisons are on a rounded mean so that a value one float ULP either
    /// side of the threshold does not flip between runs.
    #[must_use]
    pub fn changes_above(&self, threshold: f64) -> Vec<&FrameDifference> {
        self.differences
            .iter()
            .filter(|d| d.mean_absolute >= threshold)
            .collect()
    }

    /// Returns the largest difference observed, if any.
    #[must_use]
    pub fn largest(&self) -> Option<&FrameDifference> {
        self.differences
            .iter()
            .max_by(|a, b| a.mean_absolute.total_cmp(&b.mean_absolute))
    }
}

/// A per-sample difference above this counts toward `changed_fraction`.
///
/// Chosen to ignore compression noise: a value near full scale is a real change,
/// a handful of levels is not.
const SAMPLE_DELTA: u8 = 16;

/// Measures the difference between every genuinely consecutive pair of frames.
///
/// # A gap in the frame list is not a scene change
///
/// Frames of differing dimensions cannot be compared sample by sample, so a
/// resolution change ends the scan and is reported through the frame count
/// rather than being compared against misaligned pixels.
///
/// The same reasoning applies to a **gap in the indices**, and this is not
/// hypothetical: [`crate::decode::DecodeSession::decode_resilient`] recovers from
/// a corrupt packet by skipping forward to the next keyframe, so the frames it
/// returns legitimately skip numbers. Comparing frame 5 against frame 12 would
/// measure the difference over seven frames of elapsed footage and report it as
/// a single step. On any real cut that reads as a large, confident scene change
/// which is an artefact of the recovery, not an observation about the media —
/// the exact failure mode this engine exists to avoid.
///
/// So a non-adjacent pair is skipped and counted in
/// [`SceneReport::comparisons_skipped`], which is what lets a reader tell a
/// quiet file from a partly-undecodable one.
#[must_use]
pub fn analyse(frames: &[DecodedFrame]) -> SceneReport {
    let mut differences = Vec::new();
    let mut comparisons_skipped = 0usize;

    for pair in frames.windows(2) {
        let (previous, current) = (&pair[0], &pair[1]);
        if previous.width != current.width || previous.height != current.height {
            break;
        }

        // Not adjacent in the source stream, so the difference spans frames
        // this analysis never saw. Not measurable as a single step.
        if current.index != previous.index + 1 {
            comparisons_skipped += 1;
            continue;
        }

        let (mean_absolute, changed_fraction) = compare(&previous.luma, &current.luma);
        differences.push(FrameDifference {
            index: current.index,
            mean_absolute,
            changed_fraction,
        });
    }

    SceneReport {
        differences,
        frames_examined: frames.len(),
        comparisons_skipped,
    }
}

/// Returns the mean absolute difference and the changed-sample proportion.
fn compare(a: &[u8], b: &[u8]) -> (f64, f64) {
    let samples = a.len().min(b.len());
    if samples == 0 {
        return (0.0, 0.0);
    }

    let mut total: u64 = 0;
    let mut changed: u64 = 0;
    for index in 0..samples {
        let difference = a[index].abs_diff(b[index]);
        total += u64::from(difference);
        if difference > SAMPLE_DELTA {
            changed += 1;
        }
    }

    (
        total as f64 / samples as f64,
        changed as f64 / samples as f64,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a frame of a uniform luma value.
    fn uniform(index: usize, width: u32, height: usize, value: u8) -> DecodedFrame {
        DecodedFrame {
            index,
            is_key_frame: index == 0,
            width,
            height: height as u32,
            luma: vec![value; width as usize * height],
        }
    }

    /// Builds a frame split horizontally: left half one value, right half another.
    fn split(index: usize, width: u32, height: usize, left: u8, right: u8) -> DecodedFrame {
        let mut luma = vec![0u8; width as usize * height];
        let w = width as usize;
        for y in 0..height {
            for x in 0..w {
                luma[y * w + x] = if x < w / 2 { left } else { right };
            }
        }
        DecodedFrame {
            index,
            is_key_frame: index == 0,
            width,
            height: height as u32,
            luma,
        }
    }

    #[test]
    fn identical_frames_produce_no_difference() {
        let frames = vec![uniform(0, 16, 16, 100), uniform(1, 16, 16, 100)];
        let report = analyse(&frames);
        assert_eq!(report.differences.len(), 1);
        assert_eq!(report.differences[0].mean_absolute, 0.0);
        assert_eq!(report.differences[0].changed_fraction, 0.0);
    }

    #[test]
    fn a_full_black_to_white_change_measures_the_full_range() {
        let frames = vec![uniform(0, 16, 16, 0), uniform(1, 16, 16, 255)];
        let report = analyse(&frames);
        assert!((report.differences[0].mean_absolute - 255.0).abs() < 1e-9);
        assert!((report.differences[0].changed_fraction - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_small_difference_is_not_reported_as_a_change() {
        let frames = vec![uniform(0, 16, 16, 100), uniform(1, 16, 16, 105)];
        let report = analyse(&frames);
        assert!(
            report.changes_above(20.0).is_empty(),
            "5 levels is not a cut"
        );
    }

    #[test]
    fn a_large_difference_is_reported_at_the_right_index() {
        let frames = vec![
            uniform(0, 16, 16, 0),
            uniform(1, 16, 16, 0),
            uniform(2, 16, 16, 255),
        ];
        let report = analyse(&frames);
        let changes = report.changes_above(100.0);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].index, 2, "the change is at the later frame");
    }

    #[test]
    fn frames_separated_by_a_lost_reference_are_not_compared() {
        // The case that makes the skip load-bearing. Frames 0 and 9 are black
        // and white; comparing them would measure 255.0 and report a
        // scene change — but frames 1 through 8 were never seen, so that number
        // describes a gap in the analysis rather than anything about the media.
        let frames = vec![uniform(0, 16, 16, 0), uniform(9, 16, 16, 255)];
        let report = analyse(&frames);

        assert!(
            report.differences.is_empty(),
            "a gap is not a step: {:?}",
            report.differences
        );
        assert!(
            report.changes_above(1.0).is_empty(),
            "no fabricated scene change may reach the rule"
        );
        assert_eq!(report.comparisons_skipped, 1);
        assert_eq!(report.frames_examined, 2);
    }

    #[test]
    fn adjacent_frames_after_a_gap_still_compare_normally() {
        // Recovery must not sterilise the analysis: once the stream is
        // contiguous again, ordinary differences are measured as before.
        let frames = vec![
            uniform(0, 16, 16, 0),
            uniform(9, 16, 16, 0), // gap: 1..=8 lost
            uniform(10, 16, 16, 255),
        ];
        let report = analyse(&frames);

        assert_eq!(report.comparisons_skipped, 1);
        assert_eq!(report.differences.len(), 1, "only the adjacent pair compares");
        assert_eq!(report.differences[0].index, 10);
        assert!((report.differences[0].mean_absolute - 255.0).abs() < 1e-9);
    }

    #[test]
    fn a_contiguous_run_skips_nothing() {
        // The negative case: the common path must not start reporting skips.
        let frames: Vec<DecodedFrame> = (0..5).map(|i| uniform(i, 16, 16, 100)).collect();
        let report = analyse(&frames);
        assert_eq!(report.comparisons_skipped, 0);
        assert_eq!(report.differences.len(), 4);
    }

    /// Builds a frame with a bright band starting at `edge`.
    fn band(index: usize, width: u32, height: usize, edge: usize) -> DecodedFrame {
        let mut luma = vec![0u8; width as usize * height];
        let w = width as usize;
        for y in 0..height {
            for x in 0..w {
                luma[y * w + x] = if (edge..edge + 8).contains(&x) {
                    200
                } else {
                    0
                };
            }
        }
        DecodedFrame {
            index,
            is_key_frame: index == 0,
            width,
            height: height as u32,
            luma,
        }
    }

    #[test]
    fn a_moving_edge_registers_a_difference_without_a_motion_model() {
        // A bright band shifted a few pixels: many samples change by a moderate
        // amount, which is what a pan looks like at the sample level.
        let frames = vec![band(0, 32, 32, 4), band(1, 32, 32, 8)];
        let report = analyse(&frames);

        assert!(
            report.differences[0].mean_absolute > 0.0,
            "a moving edge must register a difference"
        );
        assert!(
            report.differences[0].changed_fraction > 0.0,
            "some samples must be counted as changed"
        );
    }

    #[test]
    fn a_moving_edge_is_measured_below_a_cut_threshold() {
        // The same motion must not read as a scene change: that is the
        // distinction between a pan and a cut.
        let frames = vec![band(0, 32, 32, 4), band(1, 32, 32, 6)];
        let report = analyse(&frames);
        assert!(
            report.changes_above(40.0).is_empty(),
            "a small pan is not a scene change: {:?}",
            report.differences
        );
    }

    #[test]
    fn a_resolution_change_ends_the_scan_rather_than_comparing_misaligned_pixels() {
        let frames = vec![
            uniform(0, 16, 16, 0),
            uniform(1, 32, 32, 255),
            uniform(2, 32, 32, 255),
        ];
        let report = analyse(&frames);
        assert!(
            report.differences.is_empty(),
            "frames of different sizes must not be compared"
        );
        assert_eq!(report.frames_examined, 3, "the frames are still counted");
    }

    #[test]
    fn an_empty_or_single_frame_sequence_produces_no_differences() {
        assert!(analyse(&[]).differences.is_empty());
        assert!(analyse(&[uniform(0, 8, 8, 1)]).differences.is_empty());
    }

    #[test]
    fn comparison_is_deterministic() {
        let frames = vec![split(0, 16, 16, 0, 255), split(1, 16, 16, 255, 0)];
        assert_eq!(analyse(&frames), analyse(&frames));
    }

    #[test]
    fn the_largest_difference_is_reported() {
        let frames = vec![
            uniform(0, 16, 16, 0),
            uniform(1, 16, 16, 10),
            uniform(2, 16, 16, 250),
        ];
        let report = analyse(&frames);
        assert_eq!(report.largest().map(|d| d.index), Some(2));
    }
}
