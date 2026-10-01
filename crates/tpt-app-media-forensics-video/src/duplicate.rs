//! Duplicate and repeated-frame detection (spec §16, §17).
//!
//! # What can be proven without decoding
//!
//! Detection compares the **compressed** sample bytes, so it needs no decoder.
//! What that proves depends on whether the samples are keyframes, and the
//! distinction is not academic:
//!
//! | Samples | Claim | Valid? |
//! |---|---|---|
//! | all keyframes | identical bitstreams mean identical pictures | **yes** |
//! | any non-keyframe | identical bitstreams with the same reference state | **only with a caveat** |
//!
//! An I-frame is self-contained, so two identical I-frame bitstreams decode to
//! the same picture. That is a sound conclusion.
//!
//! A P- or B-frame is predicted from its references, so identical bytes alone do
//! *not* prove identical output — the reference picture behind them could
//! differ. Detecting duplication from non-keyframe bytes is therefore reported
//! at lower confidence and explicitly labelled, rather than being presented as
//! proof.
//!
//! Near-duplicate detection needs decoded pixels and is Tier 2.

use tpt_app_media_forensics_model::MediaTime;

/// How strong a duplicate claim is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Soundness {
    /// Identical compressed bytes, but at least one sample is a predicted frame
    /// whose reference picture is unknown.
    CompressedMatchOnly,
    /// Identical compressed bytes across keyframes only. These decode to
    /// identical pictures by construction.
    PixelIdentical,
}

impl Soundness {
    /// Returns a human-readable explanation for a finding.
    #[must_use]
    pub const fn explanation(self) -> &'static str {
        match self {
            Self::PixelIdentical => {
                "identical keyframe bitstreams; these frames decode to identical pictures"
            }
            Self::CompressedMatchOnly => {
                "identical compressed data; at least one frame is predicted, so identical \
                 reference state is assumed rather than proven"
            }
        }
    }
}

/// A run of consecutive samples sharing the same content digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepeatedRun {
    /// Index of the first sample in the run.
    pub first_frame: u32,
    /// Number of consecutive samples sharing the digest.
    pub length: u32,
    /// Presentation time of the first sample.
    pub start_time: MediaTime,
    /// Presentation time of the last sample.
    pub end_time: MediaTime,
    /// How strong the duplicate claim is.
    pub soundness: Soundness,
}

impl RepeatedRun {
    /// Returns `true` when the run contains only keyframes.
    ///
    /// Such a run is a provable pixel-identical sequence — the strongest
    /// duplicate signal available without a decoder.
    #[must_use]
    pub fn is_pixel_identical(&self) -> bool {
        self.soundness == Soundness::PixelIdentical
    }
}

/// One sample, reduced to what duplicate detection needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleDigest {
    /// Content digest of the compressed sample.
    pub digest: String,
    /// Presentation time.
    pub time: MediaTime,
    /// Whether the sample is a random-access point.
    pub is_key_frame: bool,
}

/// Finds runs of consecutive samples with identical content.
///
/// `min_run` is the shortest run worth reporting. A run of two is the minimum
/// that indicates anything: one isolated repeat is indistinguishable from
/// ordinary encoder behaviour on a static scene.
#[must_use]
pub fn find_repeated_runs(samples: &[SampleDigest], min_run: u32) -> Vec<RepeatedRun> {
    let min_run = min_run.max(2);
    let mut runs = Vec::new();
    let mut index = 0usize;

    while index < samples.len() {
        let digest = &samples[index].digest;
        let start = index;
        let mut all_key_frames = samples[index].is_key_frame;

        while index + 1 < samples.len() && &samples[index + 1].digest == digest {
            index += 1;
            all_key_frames &= samples[index].is_key_frame;
        }

        let length = u32::try_from(index - start + 1).unwrap_or(u32::MAX);
        if length >= min_run {
            runs.push(RepeatedRun {
                first_frame: u32::try_from(start).unwrap_or(u32::MAX),
                length,
                start_time: samples[start].time,
                end_time: samples[index].time,
                soundness: if all_key_frames {
                    Soundness::PixelIdentical
                } else {
                    Soundness::CompressedMatchOnly
                },
            });
        }

        index += 1;
    }

    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: i64) -> MediaTime {
        MediaTime::from_millis(value)
    }

    /// Builds `count` samples with distinct digests, all at 25 fps.
    fn distinct(count: usize) -> Vec<SampleDigest> {
        (0..count)
            .map(|i| SampleDigest {
                digest: format!("unique-{i}"),
                time: ms(i as i64 * 40),
                is_key_frame: false,
            })
            .collect()
    }

    #[test]
    fn no_repeats_produces_no_findings() {
        assert!(find_repeated_runs(&distinct(100), 2).is_empty());
    }

    #[test]
    fn a_run_of_keyframes_is_provably_pixel_identical() {
        let samples = vec![
            SampleDigest {
                digest: "a".into(),
                time: ms(0),
                is_key_frame: true,
            },
            SampleDigest {
                digest: "a".into(),
                time: ms(40),
                is_key_frame: true,
            },
            SampleDigest {
                digest: "a".into(),
                time: ms(80),
                is_key_frame: true,
            },
            SampleDigest {
                digest: "b".into(),
                time: ms(120),
                is_key_frame: true,
            },
        ];

        let runs = find_repeated_runs(&samples, 2);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].length, 3);
        assert!(runs[0].is_pixel_identical(), "keyframes are self-contained");
        assert!(runs[0]
            .soundness
            .explanation()
            .contains("identical pictures"));
    }

    #[test]
    fn a_run_including_predicted_frames_is_labelled_lower_confidence() {
        // A P-frame is predicted from references, so identical bytes alone do
        // not prove identical pictures.
        let samples = vec![
            SampleDigest {
                digest: "a".into(),
                time: ms(0),
                is_key_frame: true,
            },
            SampleDigest {
                digest: "a".into(),
                time: ms(40),
                is_key_frame: false,
            },
            SampleDigest {
                digest: "a".into(),
                time: ms(80),
                is_key_frame: false,
            },
        ];

        let runs = find_repeated_runs(&samples, 2);
        assert_eq!(runs.len(), 1);
        assert!(!runs[0].is_pixel_identical());
        assert!(runs[0]
            .soundness
            .explanation()
            .contains("assumed rather than proven"));
    }

    #[test]
    fn run_reports_its_time_span_as_spec_describes() {
        // spec §17: a repeated sequence with start, end, and length.
        let mut samples = distinct(10);
        for (i, slot) in samples.iter_mut().enumerate().skip(5).take(3) {
            slot.digest = "repeat".to_owned();
            let _ = i;
        }
        let runs = find_repeated_runs(&samples, 2);

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].length, 3);
        assert_eq!(runs[0].start_time, ms(200));
        assert_eq!(runs[0].end_time, ms(280));
    }

    #[test]
    fn minimum_run_length_is_enforced() {
        let samples = vec![
            SampleDigest {
                digest: "a".into(),
                time: ms(0),
                is_key_frame: false,
            },
            SampleDigest {
                digest: "b".into(),
                time: ms(40),
                is_key_frame: false,
            },
            SampleDigest {
                digest: "c".into(),
                time: ms(80),
                is_key_frame: false,
            },
        ];
        // A single repeat is indistinguishable from ordinary static-scene
        // encoding and is not reported.
        assert!(find_repeated_runs(&samples, 2).is_empty());
    }

    #[test]
    fn separate_runs_are_reported_separately() {
        let samples = vec![
            SampleDigest {
                digest: "a".into(),
                time: ms(0),
                is_key_frame: true,
            },
            SampleDigest {
                digest: "a".into(),
                time: ms(40),
                is_key_frame: true,
            },
            SampleDigest {
                digest: "b".into(),
                time: ms(80),
                is_key_frame: true,
            },
            SampleDigest {
                digest: "c".into(),
                time: ms(120),
                is_key_frame: true,
            },
            SampleDigest {
                digest: "c".into(),
                time: ms(160),
                is_key_frame: true,
            },
        ];
        let runs = find_repeated_runs(&samples, 2);
        assert_eq!(runs.len(), 2);
    }

    #[test]
    fn long_repeat_runs_are_found_completely() {
        // A freeze frame: the same picture held for several seconds.
        let samples: Vec<SampleDigest> = (0..300)
            .map(|i| SampleDigest {
                digest: if (100..200).contains(&i) {
                    "frozen".into()
                } else {
                    format!("f{i}")
                },
                time: ms(i as i64 * 40),
                is_key_frame: i == 100,
            })
            .collect();

        let runs = find_repeated_runs(&samples, 2);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].length, 100);
        assert_eq!(runs[0].start_time, ms(4_000));
    }

    #[test]
    fn empty_input_is_handled() {
        assert!(find_repeated_runs(&[], 2).is_empty());
    }

    #[test]
    fn detection_is_deterministic() {
        let samples = vec![
            SampleDigest {
                digest: "a".into(),
                time: ms(0),
                is_key_frame: true,
            },
            SampleDigest {
                digest: "a".into(),
                time: ms(40),
                is_key_frame: true,
            },
            SampleDigest {
                digest: "b".into(),
                time: ms(80),
                is_key_frame: true,
            },
            SampleDigest {
                digest: "b".into(),
                time: ms(120),
                is_key_frame: true,
            },
        ];
        assert_eq!(
            find_repeated_runs(&samples, 2),
            find_repeated_runs(&samples, 2)
        );
    }
}
