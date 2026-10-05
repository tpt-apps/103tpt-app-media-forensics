//! Property tests for the frame-ordering analysers (spec §75–§77).
//!
//! # Why these two
//!
//! `gop::analyse` and `duplicate::find_repeated_runs` are pure functions over
//! sequences — frame indices, digests, and times. They are also the two analysers
//! whose *output shape* carries most of the meaning: a GOP report is a partition
//! of the track, and a repeated-run list is a claim about maximality. A report
//! that double-counted a frame or missed half a freeze would look entirely
//! plausible.
//!
//! Example tests can show one freeze is found. They cannot show that the runs
//! *partition* the samples they claim, that no run overlaps another, or that every
//! maximal run is found — which are the properties a reviewer is actually relying
//! on when they read a frame count off the report.
//!
//! So the properties below are structural rather than per-example: they assert
//! relationships between the output and the input that must hold for *every*
//! sequence, and they include an independent oracle for the duplicate detector so
//! completeness is checked against something other than itself.

use proptest::prelude::*;

use tpt_app_media_forensics_model::MediaTime;
use tpt_app_media_forensics_video::duplicate::{find_repeated_runs, SampleDigest};
use tpt_app_media_forensics_video::gop::analyse;

/// Keyframe indices as a strictly increasing sequence, which is what an
/// `stss` box actually contains.
fn keyframes(max_frame: u32) -> impl Strategy<Value = Vec<u32>> {
    prop::collection::vec(0u32..=max_frame, 0..12).prop_map(|mut frames| {
        frames.sort_unstable();
        frames.dedup();
        frames
    })
}

/// Arbitrary digests, over a small alphabet so runs actually occur.
///
/// A large digest space would make almost every run a run of one and the
/// completeness properties vacuous, which is the failure mode property tests are
/// most prone to.
fn digests() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(0u8..4, 0..30).prop_map(|values| {
        values
            .into_iter()
            .map(|v| format!("d{v}"))
            .collect::<Vec<String>>()
    })
}

/// Presentation times, one per frame, at a regular cadence.
fn times(count: usize) -> Vec<MediaTime> {
    (0..count)
        .map(|i| MediaTime::from_micros(i as i64 * 40_000))
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// The GOPs partition the track: consecutive GOPs abut exactly, with no gap
    /// and no frame counted twice.
    ///
    /// The load-bearing property of the whole analyser. A report renders GOP
    /// lengths and frame positions beside each other, and a reader adding them up
    /// is entitled to get the track length. If GOP *n* ended anywhere but where GOP
    /// *n+1* began, some frames would be double-counted and others silently
    /// dropped — and neither shows up as an error, only as a plausible total.
    #[test]
    fn gops_partition_the_track_without_gap_or_overlap(
        keyframes in keyframes(60),
        frames in 0usize..80,
    ) {
        let stamps = times(frames);
        let report = analyse(&keyframes, &stamps, 2);

        for pair in report.gops.windows(2) {
            let previous = &pair[0];
            let current = &pair[1];
            prop_assert_eq!(
                previous.start_frame + previous.length,
                current.start_frame,
                "GOP {} ends at {} but GOP {} starts at {}: frames are counted twice or \
                 dropped between them",
                previous.index,
                previous.start_frame + previous.length,
                current.index,
                current.start_frame
            );
        }
    }

    /// Every GOP starts at a declared keyframe, and no two start at the same one.
    ///
    /// The analyser's input is a keyframe list; a GOP opening anywhere else would
    /// mean the report is describing a structure the container never declared.
    #[test]
    fn every_gop_starts_at_a_declared_keyframe(
        keyframes in keyframes(60),
        frames in 0usize..80,
    ) {
        let stamps = times(frames);
        let report = analyse(&keyframes, &stamps, 2);

        let starts: Vec<u32> = report.gops.iter().map(|g| g.start_frame).collect();
        for start in &starts {
            prop_assert!(
                keyframes.contains(start),
                "GOP starts at frame {start}, which the container never declared as a \
                 keyframe: {starts:?}"
            );
        }
        let mut unique = starts.clone();
        unique.sort_unstable();
        unique.dedup();
        prop_assert_eq!(unique.len(), starts.len(), "two GOPs share an opening frame");
    }

    /// The GOPs cover the whole track from the first keyframe onward.
    ///
    /// Asserted as a total, because partial coverage is the failure the first
    /// property cannot see: correct abutment everywhere, but the last GOP
    /// stopping short, which loses frames without any discontinuity in the
    /// partition.
    #[test]
    fn gops_cover_every_frame_after_the_first_keyframe(
        raw_keyframes in prop::collection::vec(0u32..80, 0..12),
        frames in 1usize..80,
    ) {
        // Keyframes beyond the end of the track are dropped rather than clamped.
        // A hostile `stss` can name a frame that does not exist, and the engine
        // handles that with saturating arithmetic — but "the analyser coped" is a
        // different property from "the GOPs tile the track", and asserting the
        // latter over input where it does not hold would be asserting nothing.
        let mut keyframes = raw_keyframes;
        keyframes.retain(|&frame| (frame as usize) < frames);
        keyframes.sort_unstable();
        keyframes.dedup();

        let stamps = times(frames);
        let report = analyse(&keyframes, &stamps, 2);

        prop_assume!(!keyframes.is_empty());
        let first = keyframes[0];
        let covered: u32 = report.gops.iter().map(|g| g.length).sum();
        prop_assert_eq!(
            covered,
            frames as u32 - first,
            "the GOPs must cover every frame from the first keyframe to the end"
        );
        prop_assert_eq!(report.frame_count, frames, "the frame count is what was given");
        prop_assert_eq!(
            report.keyframe_count,
            keyframes.len(),
            "the keyframe count is what was given"
        );
    }

    /// A track with no keyframes produces no GOPs at all.
    ///
    /// Worth pinning because "no keyframes declared" and "one GOP spanning the
    /// whole track" are different claims, and only one of them is true. A reader
    /// shown a single full-length GOP would conclude the file is all-intra, which
    /// is the opposite of what an absent `stss` means.
    #[test]
    fn no_keyframes_means_no_gops(frames in 0usize..40) {
        let report = analyse(&[], &times(frames), 2);
        prop_assert!(
            report.gops.is_empty(),
            "a track declaring no keyframes has no GOP structure to report, got {:?}",
            report.gops
        );
    }

    /// A perfectly regular GOP structure reports no changes.
    ///
    /// The property that keeps `VIDEO.GOP_LENGTH_CHANGE` quiet on healthy files.
    /// The final GOP is excluded from the comparison by construction — a truncated
    /// tail is normal — so a regular structure must produce nothing at all.
    #[test]
    fn a_regular_gop_structure_reports_no_changes(period in 1u32..12, gops in 2u32..8) {
        let keyframes: Vec<u32> = (0..gops).map(|g| g * period).collect();
        let frames = (gops * period) as usize;
        let report = analyse(&keyframes, &times(frames), 2);

        prop_assert_eq!(
            report.dominant_length, period,
            "the dominant GOP length is the one repeated"
        );
        prop_assert!(
            report.changes.is_empty(),
            "a constant GOP length has no transitions, got {:?}",
            report.changes
        );
    }

    /// Every maximal run is found, and every reported run is a real maximal run.
    ///
    /// Completeness and soundness in one property, checked against an oracle
    /// written independently of the implementation. Checking the detector against
    /// itself would prove nothing; the reference below walks maximal runs directly
    /// and never looks at [`find_repeated_runs`].
    ///
    /// Both directions matter. A detector that stopped early would pass every
    /// example test in the crate while missing the freeze in the second half of a
    /// file; one that over-reported would fill a report with runs that are not
    /// there, and a reviewer shown runs that do not exist learns to distrust the
    /// ones that do.
    #[test]
    fn every_maximal_repeated_run_is_found_exactly_once(
        values in digests(),
        min_run in 0u32..6,
        key_flags in prop::collection::vec(any::<bool>(), 0..30),
    ) {
        let samples: Vec<SampleDigest> = values
            .iter()
            .enumerate()
            .map(|(index, digest)| SampleDigest {
                digest: digest.clone(),
                time: MediaTime::from_micros(index as i64 * 40_000),
                is_key_frame: key_flags.get(index).copied().unwrap_or(index == 0),
            })
            .collect();

        let found = find_repeated_runs(&samples, min_run);

        // The oracle: maximal runs, walked directly.
        let mut expected: Vec<(usize, usize)> = Vec::new();
        let mut index = 0usize;
        while index < samples.len() {
            let mut end = index;
            while end + 1 < samples.len() && samples[end + 1].digest == samples[index].digest {
                end += 1;
            }
            // `min_run.max(2)`: a run of one is an isolated repeat, which says
            // nothing, so the analyser is documented as never reporting one.
            if end - index + 1 >= min_run.max(2) as usize {
                expected.push((index, end));
            }
            index = end + 1;
        }

        let reported: Vec<(usize, usize)> = found
            .iter()
            .map(|run| {
                let start = run.first_frame as usize;
                (start, start + run.length as usize - 1)
            })
            .collect();

        prop_assert_eq!(
            reported, expected,
            "the runs found must be exactly the maximal repeated runs"
        );
    }

    /// Reported runs never overlap, are ordered, and cannot cover more than the
    /// track.
    ///
    /// A reader adding up run lengths to get "how many frames were frozen" is
    /// entitled to an answer no larger than the track. Two runs covering one frame
    /// would break that in a way nothing else in the report would reveal.
    #[test]
    fn reported_runs_are_ordered_disjoint_and_bounded(values in digests(), min_run in 2u32..6) {
        let samples: Vec<SampleDigest> = values
            .iter()
            .enumerate()
            .map(|(index, digest)| SampleDigest {
                digest: digest.clone(),
                time: MediaTime::from_micros(index as i64 * 40_000),
                is_key_frame: index == 0,
            })
            .collect();

        let found = find_repeated_runs(&samples, min_run);
        for pair in found.windows(2) {
            let previous = &pair[0];
            let current = &pair[1];
            prop_assert!(
                previous.first_frame + previous.length <= current.first_frame,
                "runs overlap: {}..{} and {}..{}",
                previous.first_frame,
                previous.first_frame + previous.length,
                current.first_frame,
                current.first_frame + current.length
            );
        }

        let covered: u64 = found.iter().map(|r| u64::from(r.length)).sum();
        prop_assert!(
            covered <= samples.len() as u64,
            "reported runs cover {covered} of {} samples, which is impossible",
            samples.len()
        );
    }

    /// A run of one is never reported, whatever `min_run` says.
    ///
    /// `min_run` is clamped to 2 rather than honoured literally, and the reason is
    /// evidentiary: a single repeated frame is indistinguishable from ordinary
    /// encoder behaviour on a static scene, so reporting it would put a finding in
    /// front of an examiner that the data cannot support. A caller passing
    /// `min_run: 1` asks for something the analyser declines to do, and this says
    /// so rather than quietly serving it.
    #[test]
    fn a_run_of_one_is_never_reported(values in digests(), min_run in 0u32..2) {
        let samples: Vec<SampleDigest> = values
            .iter()
            .enumerate()
            .map(|(index, digest)| SampleDigest {
                digest: digest.clone(),
                time: MediaTime::from_micros(index as i64 * 40_000),
                is_key_frame: index == 0,
            })
            .collect();

        for run in find_repeated_runs(&samples, min_run) {
            prop_assert!(
                run.length >= 2,
                "a run of {} frame(s) was reported at min_run {min_run}: one repeated frame \
                 is not evidence of a freeze",
                run.length
            );
        }
    }

    /// A run spans exactly the samples it covers, and the times of those samples.
    ///
    /// Small, but it is what places a freeze on the timeline: a report putting the
    /// interval anywhere else would show the defect at the wrong moment.
    #[test]
    fn a_run_spans_the_samples_it_covers(values in digests(), min_run in 2u32..6) {
        let samples: Vec<SampleDigest> = values
            .iter()
            .enumerate()
            .map(|(index, digest)| SampleDigest {
                digest: digest.clone(),
                time: MediaTime::from_micros(index as i64 * 40_000),
                is_key_frame: index == 0,
            })
            .collect();

        for run in find_repeated_runs(&samples, min_run) {
            let start = run.first_frame as usize;
            let end = start + run.length as usize - 1;
            prop_assert!(
                start < samples.len() && end < samples.len(),
                "run at {start}..{end} runs past a {} sample track",
                samples.len()
            );
            prop_assert_eq!(
                run.start_time, samples[start].time,
                "a run starts at the time of its first sample"
            );
            prop_assert_eq!(
                run.end_time, samples[end].time,
                "a run ends at the time of its last sample"
            );
            prop_assert!(
                samples[start].digest == samples[end].digest,
                "a run must cover one digest throughout"
            );
        }
    }
}
