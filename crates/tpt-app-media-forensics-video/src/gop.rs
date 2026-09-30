//! GOP structure analysis (spec §15).
//!
//! # No decoding required
//!
//! GOP structure is a property of the *packet* layer, not of pixels. The
//! sync-sample table (`stss`) lists which packets are keyframes, and the
//! time-to-sample table (`stts`) gives their presentation times. Both come from
//! container metadata, so this analysis runs without decoding a single
//! macroblock.
//!
//! That matters beyond speed. A decoder is a large, failure-prone component
//! that can be defeated by a malformed stream; GOP structure is one of the
//! strongest structural signals available and should remain available even when
//! decoding fails.
//!
//! # The finding spec §15 asks for
//!
//! ```text
//! "GOP structure changes from approximately 60 frames to approximately 15
//!  frames at 00:37:21.120."
//! ```
//!
//! That is an observation, not proof of editing — an encoder can also change
//! GOP length on its own, and a scene-cut-driven encoder certainly does. The
//! change is recorded with its location and nothing more is claimed.

use tpt_app_media_forensics_model::MediaTime;

/// One group of pictures, delimited by two keyframes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gop {
    /// Zero-based index of this GOP within the track.
    pub index: usize,
    /// Frame index of the keyframe that opens this GOP.
    pub start_frame: u32,
    /// Number of frames in this GOP, excluding the opening keyframe.
    pub length: u32,
    /// Presentation time of the opening keyframe.
    pub start_time: MediaTime,
    /// Presentation time of the keyframe that closes this GOP.
    pub end_time: MediaTime,
}

impl Gop {
    /// Returns the wall-clock duration covered by this GOP.
    #[must_use]
    pub fn duration(&self) -> MediaTime {
        self.end_time.signed_diff(self.start_time)
    }
}

/// A point where GOP length departs from the track's dominant length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GopChange {
    /// Index of the GOP whose length departs from the dominant one.
    pub gop_index: usize,
    /// Presentation time where the change begins.
    pub at: MediaTime,
    /// The dominant GOP length, in frames.
    pub expected_length: u32,
    /// The observed GOP length, in frames.
    pub observed_length: u32,
}

/// The result of analysing a track's GOP structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GopReport {
    /// Every GOP found, in order.
    pub gops: Vec<Gop>,
    /// Points where the GOP length departs from the dominant length.
    pub changes: Vec<GopChange>,
    /// The most common GOP length, in frames.
    pub dominant_length: u32,
    /// Number of keyframes observed.
    pub keyframe_count: usize,
    /// Number of frames in the track.
    pub frame_count: usize,
}

impl GopReport {
    /// Returns `true` when the track uses a single constant GOP length.
    ///
    /// A perfectly regular GOP structure is what a single encoding pass
    /// produces. Irregularity is the interesting case, not the clean one.
    #[must_use]
    pub fn is_uniform(&self) -> bool {
        self.changes.is_empty()
    }

    /// Returns the shortest GOP length, if any GOP was found.
    #[must_use]
    pub fn shortest(&self) -> Option<u32> {
        self.gops.iter().map(|g| g.length).min()
    }

    /// Returns the longest GOP length, if any GOP was found.
    #[must_use]
    pub fn longest(&self) -> Option<u32> {
        self.gops.iter().map(|g| g.length).max()
    }
}

/// Analyses GOP structure from keyframe positions and frame timestamps.
///
/// `keyframes` holds frame indices of the sync samples, ascending. `timestamps`
/// holds the presentation time of every frame; it may be longer than the frame
/// range covered by `keyframes`, and the final GOP runs to the end of the
/// track.
///
/// `tolerance` is the relative deviation permitted before a GOP counts as
/// changed. Expressed in frames and supplied by the active profile, never
/// hard-coded: different encoders and delivery specifications use different
/// GOP structures, and a fixed threshold would flag one format as anomalous and
/// pass another.
#[must_use]
pub fn analyse(keyframes: &[u32], timestamps: &[MediaTime], tolerance_frames: u32) -> GopReport {
    let mut gops = Vec::new();

    for (index, window) in keyframes.windows(2).enumerate() {
        let start_frame = window[0];
        let end_frame = window[1];
        gops.push(Gop {
            index,
            start_frame,
            length: end_frame.saturating_sub(start_frame),
            start_time: timestamp_at(timestamps, start_frame),
            end_time: timestamp_at(timestamps, end_frame),
        });
    }

    // The final GOP runs from the last keyframe to the end of the track. It is
    // included so that a trailing short GOP is visible; a truncated final GOP is
    // normal and the caller can discount it.
    if let Some(&last_keyframe) = keyframes.last() {
        let end_frame = u32::try_from(timestamps.len()).unwrap_or(u32::MAX);
        if end_frame > last_keyframe {
            gops.push(Gop {
                index: gops.len(),
                start_frame: last_keyframe,
                length: end_frame.saturating_sub(last_keyframe),
                start_time: timestamp_at(timestamps, last_keyframe),
                end_time: timestamp_at(timestamps, end_frame.saturating_sub(1)),
            });
        }
    }

    let dominant_length = modal_gop_length(&gops);
    let changes = find_changes(&gops, tolerance_frames);

    GopReport {
        gops,
        changes,
        dominant_length,
        keyframe_count: keyframes.len(),
        frame_count: timestamps.len(),
    }
}

/// Returns the presentation time of a frame index, or zero if out of range.
fn timestamp_at(timestamps: &[MediaTime], frame: u32) -> MediaTime {
    usize::try_from(frame)
        .ok()
        .and_then(|i| timestamps.get(i).copied())
        .unwrap_or(MediaTime::ZERO)
}

/// Returns the most common GOP length.
///
/// On a tie, the length that appears **earliest** wins. The encoding's
/// established pattern precedes any later change, so when a file splits evenly
/// between two lengths the earlier one is the baseline and the later one is the
/// deviation. Preferring the shorter instead would let a file that starts at 60
/// frames and switches to 15 declare 15 as its dominant length — inverting the
/// finding spec §15 asks for.
fn modal_gop_length(gops: &[Gop]) -> u32 {
    // (length, count, index of first occurrence)
    let mut counts: std::collections::BTreeMap<u32, (u32, usize)> =
        std::collections::BTreeMap::new();
    for gop in gops {
        counts
            .entry(gop.length)
            .and_modify(|(count, _)| *count += 1)
            .or_insert((1, gop.index));
    }
    counts
        .into_iter()
        .max_by(|(_, (count_a, first_a)), (_, (count_b, first_b))| {
            // Highest count; on a tie, earliest first-occurrence.
            count_a.cmp(count_b).then_with(|| first_b.cmp(first_a))
        })
        .map_or(0, |(length, _)| length)
}
/// Finds points where the GOP length *transitions*.
///
/// Each entry is one transition, comparing a GOP against its predecessor
/// rather than against a global mode. Three reasons:
///
/// 1. **One change, one finding.** A track that runs at 60-frame GOPs then
///    switches to 15 produces a single transition. Reporting every
///    deviating GOP separately would emit a dozen near-identical findings for
///    one structural event, which is exactly the output spec §15's example
///    shows instead.
/// 2. **The direction is correct.** Comparing against a mode inverts the
///    report whenever the shorter regime is the more common one: a file that
///    spends most of its length at 15-frame GOPs would otherwise report
///    "dominant 15, changed to 50" and bury the actual edit.
/// 3. **Time is the boundary**, so the finding lands where a reviewer jumps
///    to (spec §81).
///
/// The trailing GOP is excluded: a file that ends mid-GOP always has a short
/// final group, and reporting that as a transition would be noise.
fn find_changes(gops: &[Gop], tolerance: u32) -> Vec<GopChange> {
    let last_index = gops.len().saturating_sub(1);
    let mut changes = Vec::new();

    for index in 1..gops.len() {
        if index == last_index {
            break;
        }
        let previous = &gops[index - 1];
        let current = &gops[index];
        if deviation(current.length, previous.length) > tolerance {
            changes.push(GopChange {
                gop_index: index,
                // The transition happens where the previous GOP ended.
                at: previous.end_time,
                expected_length: previous.length,
                observed_length: current.length,
            });
        }
    }

    changes
}

/// Returns the absolute difference between two frame counts.
fn deviation(observed: u32, expected: u32) -> u32 {
    observed.abs_diff(expected)
}
#[cfg(test)]
mod tests {
    use super::*;

    /// Builds `count` frame timestamps at 25 fps.
    fn frames(count: usize) -> Vec<MediaTime> {
        (0..count)
            .map(|i| MediaTime::from_micros(i as i64 * 40_000))
            .collect()
    }

    /// Keyframes every `gop` frames across `total` frames.
    fn uniform_keyframes(total: usize, gop: usize) -> Vec<u32> {
        (0..total).step_by(gop).map(|i| i as u32).collect()
    }

    /// Keyframe indices switching from `first_gop` to `second_gop` at `switch_at`.
    fn gop_change_keyframes(
        total: usize,
        first_gop: usize,
        switch_at: usize,
        second_gop: usize,
    ) -> Vec<u32> {
        let mut keyframes = Vec::new();
        let mut frame = 0usize;
        while frame < total {
            keyframes.push(frame as u32);
            frame += if frame < switch_at {
                first_gop
            } else {
                second_gop
            };
        }
        keyframes
    }

    #[test]
    fn uniform_structure_reports_no_changes() {
        let ts = frames(300);
        let report = analyse(&uniform_keyframes(300, 50), &ts, 5);

        assert_eq!(report.keyframe_count, 6);
        assert_eq!(report.dominant_length, 50);
        assert!(
            report.is_uniform(),
            "an unchanging GOP structure must not raise findings"
        );
    }

    #[test]
    fn a_gop_length_change_is_one_finding_at_the_transition() {
        // 50-frame GOPs, then 15-frame GOPs: spec §15's example, in miniature.
        let ts = frames(600);
        let keyframes = gop_change_keyframes(600, 50, 250, 15);
        let report = analyse(&keyframes, &ts, 5);

        // One structural event, one finding - not one per deviating GOP.
        assert_eq!(report.changes.len(), 1, "one transition, one finding");

        let change = &report.changes[0];
        assert_eq!(change.expected_length, 50);
        assert_eq!(change.observed_length, 15);
        // Located where the 50-frame GOP ended, so the finding is clickable.
        assert_eq!(change.at, MediaTime::from_micros(250 * 40_000));
        assert_eq!(change.at.to_timecode(), "00:00:10.000");
    }

    #[test]
    fn a_transition_is_reported_in_the_direction_it_occurred() {
        // The shorter regime being more common must not invert the report.
        let ts = frames(900);
        let keyframes = gop_change_keyframes(900, 60, 120, 10);
        let report = analyse(&keyframes, &ts, 5);

        assert_eq!(report.dominant_length, 10, "10-frame GOPs are more common");
        assert_eq!(report.changes.len(), 1);
        assert_eq!(report.changes[0].expected_length, 60);
        assert_eq!(
            report.changes[0].observed_length, 10,
            "the report must follow the file, not the frequency"
        );
    }
    #[test]
    fn tolerance_suppresses_minor_variation() {
        let ts = frames(300);
        // 50-frame GOPs with one 52-frame GOP: within a 5-frame tolerance.
        let keyframes = vec![0, 50, 102, 152, 202, 252];
        let report = analyse(&keyframes, &ts, 5);
        assert!(
            report.is_uniform(),
            "small deviations are normal encoder behaviour"
        );
    }

    #[test]
    fn a_single_keyframe_forms_one_gop_to_the_end() {
        // One keyframe at the head means one GOP running to the end of track.
        let report = analyse(&[0], &frames(100), 5);
        assert_eq!(report.gops.len(), 1);
        assert_eq!(report.gops[0].length, 100);
        assert_eq!(report.keyframe_count, 1);
    }

    #[test]
    fn no_keyframes_yields_no_gops_without_panicking() {
        let report = analyse(&[], &frames(100), 5);
        assert!(report.gops.is_empty());
        assert_eq!(report.frame_count, 100);
    }

    #[test]
    fn trailing_gop_runs_to_the_end_of_the_track() {
        let ts = frames(125);
        let report = analyse(&[0, 50, 100], &ts, 5);

        let last = report.gops.last().expect("trailing GOP");
        assert_eq!(last.start_frame, 100);
        assert_eq!(last.length, 25);
    }

    #[test]
    fn a_truncated_final_gop_is_not_reported_as_a_change() {
        // Every file ends mid-GOP somewhere; flagging that would be noise.
        let ts = frames(137);
        let report = analyse(&uniform_keyframes(137, 50), &ts, 5);
        assert!(
            report.is_uniform(),
            "the truncated tail is expected, not a structural change"
        );
    }

    #[test]
    fn shortest_and_longest_reflect_observed_gops() {
        let ts = frames(400);
        let keyframes = vec![0, 50, 100, 160, 210, 260, 310, 360];
        let report = analyse(&keyframes, &ts, 5);

        assert_eq!(report.shortest(), Some(40));
        assert_eq!(report.longest(), Some(60));
    }

    #[test]
    fn gop_duration_is_derived_from_timestamps() {
        let ts = frames(200);
        let report = analyse(&[0, 50, 100], &ts, 5);
        // 50 frames at 25 fps.
        assert_eq!(report.gops[0].duration(), MediaTime::from_millis(2_000));
    }

    #[test]
    fn keyframes_out_of_timestamp_range_do_not_panic() {
        let ts = frames(10);
        let report = analyse(&[0, 500, 100_000], &ts, 5);
        assert!(report.frame_count == 10);
    }

    #[test]
    fn analysis_is_deterministic() {
        let ts = frames(300);
        let keys = uniform_keyframes(300, 50);
        assert_eq!(analyse(&keys, &ts, 5), analyse(&keys, &ts, 5));
    }

    #[test]
    fn a_single_frame_track_does_not_panic() {
        let report = analyse(&[0], &frames(1), 5);
        assert_eq!(report.frame_count, 1);
    }
}
