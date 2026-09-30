//! Presentation and decode timestamp forensics (spec §24).
//!
//! Inspects PTS/DTS for monotonicity, gaps, overlaps, negative values, and
//! offset. These are the conditions spec §24 lists as findings, and each one is
//! reported here as a structured observation rather than being left for a rule
//! to rediscover from raw numbers.
//!
//! All arithmetic is exact integer work on [`MediaTime`] microseconds; no
//! float accumulation, because accumulated rounding error would manufacture the
//! kind of timestamp artefact this tool must not invent (spec §77).

use tpt_app_media_forensics_model::MediaTime;

/// One timestamp anomaly detected in a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Anomaly {
    /// A decode timestamp moved backwards.
    NonMonotonicDts {
        /// Index of the offending sample.
        index: usize,
        /// The previous value.
        previous: MediaTime,
        /// The offending value.
        observed: MediaTime,
    },
    /// A presentation timestamp moved backwards.
    NonMonotonicPts {
        /// Index of the offending sample.
        index: usize,
        /// The previous value.
        previous: MediaTime,
        /// The offending value.
        observed: MediaTime,
    },
    /// A gap larger than the tolerance between consecutive samples.
    Gap {
        /// Index of the sample after the gap.
        index: usize,
        /// Size of the gap.
        size: MediaTime,
    },
    /// Consecutive samples overlap.
    Overlap {
        /// Index of the overlapping sample.
        index: usize,
        /// Size of the overlap.
        size: MediaTime,
    },
    /// A negative timestamp, legitimate before an edit list is applied.
    NegativeTimestamp {
        /// Index of the offending sample.
        index: usize,
        /// The observed value.
        observed: MediaTime,
    },
}

impl Anomaly {
    /// Returns the sample index the anomaly was found at.
    #[must_use]
    pub fn index(&self) -> usize {
        match self {
            Self::NonMonotonicDts { index, .. }
            | Self::NonMonotonicPts { index, .. }
            | Self::Gap { index, .. }
            | Self::Overlap { index, .. }
            | Self::NegativeTimestamp { index, .. } => *index,
        }
    }
}

/// The result of scanning one stream's timestamps.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TimestampReport {
    /// Anomalies found, in sample order.
    pub anomalies: Vec<Anomaly>,
    /// Number of samples scanned.
    pub sample_count: usize,
}

impl TimestampReport {
    /// Returns `true` when nothing irregular was found.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.anomalies.is_empty()
    }

    /// Counts anomalies of a given kind.
    #[must_use]
    pub fn count_of(&self, predicate: impl Fn(&Anomaly) -> bool) -> usize {
        self.anomalies.iter().filter(|a| predicate(a)).count()
    }
}

/// Scans a stream's presentation timestamps for anomalies.
///
/// `tolerance` absorbs the small irregular spacing that legitimate variable
/// frame rate content produces. It is a deviation from the *expected* frame
/// duration, not an absolute spacing: comparing raw spacing against the
/// tolerance would flag every frame of a normal 25 fps stream as a gap. The
/// tolerance comes from the active profile, never from a constant here.
///
/// Expected duration is the **modal** inter-sample delta — the most common
/// spacing. Median and mean are both wrong for this: a track that is mostly 40 ms
/// with one 10-second gap has a median of 40 ms (fine) but a mean skewed by the
/// gap, and a min that collapses to the shortest frame. The mode is the frame
/// duration the encoder actually used.
#[must_use]
pub fn scan_presentation(timestamps: &[MediaTime], tolerance: MediaTime) -> TimestampReport {
    let mut anomalies = Vec::new();

    for (index, &time) in timestamps.iter().enumerate() {
        if time.as_micros() < 0 {
            anomalies.push(Anomaly::NegativeTimestamp {
                index,
                observed: time,
            });
        }

        let Some(&previous) = index.checked_sub(1).and_then(|i| timestamps.get(i)) else {
            continue;
        };

        let delta = time.signed_diff(previous);
        if delta.as_micros() < 0 {
            anomalies.push(Anomaly::NonMonotonicPts {
                index,
                previous,
                observed: time,
            });
        } else if delta.as_micros() == 0 {
            // Two samples claiming the same presentation time.
            anomalies.push(Anomaly::Overlap { index, size: delta });
        }
    }

    if let Some(expected) = modal_delta(timestamps) {
        let threshold = expected.saturating_add(tolerance);
        for (index, pair) in timestamps.windows(2).enumerate() {
            let delta = pair[1].signed_diff(pair[0]);
            // Only forward-moving gaps qualify; a regression was already
            // recorded above and must not also be counted as a gap.
            if delta > threshold {
                anomalies.push(Anomaly::Gap {
                    index: index + 1,
                    size: delta.saturating_sub(expected),
                });
            }
        }
    }

    // Sort so anomalies are in sample order regardless of detection order.
    anomalies.sort_by_key(Anomaly::index);

    TimestampReport {
        anomalies,
        sample_count: timestamps.len(),
    }
}

/// Returns the most common strictly positive inter-sample delta.
///
/// Ties break toward the smaller delta, which is deterministic and matches the
/// shorter frame of a variable-rate sequence.
fn modal_delta(timestamps: &[MediaTime]) -> Option<MediaTime> {
    let mut counts: std::collections::BTreeMap<i64, u32> = std::collections::BTreeMap::new();
    for pair in timestamps.windows(2) {
        let delta = pair[1].signed_diff(pair[0]).as_micros();
        if delta > 0 {
            *counts.entry(delta).or_default() += 1;
        }
    }
    counts
        .into_iter()
        // Highest count wins; on a tie prefer the *smaller* delta, which is
        // deterministic and matches the shorter frame of a variable-rate run.
        .max_by(|(delta_a, count_a), (delta_b, count_b)| {
            count_a.cmp(count_b).then_with(|| delta_b.cmp(delta_a))
        })
        .map(|(delta, _)| MediaTime::from_micros(delta))
}

/// Scans a stream's decode timestamps for backwards motion.
///
/// Decode order must be monotonic for the decoder to work; a violation means
/// either a broken muxer or a file assembled from reordered parts, and both
/// are worth surfacing.
#[must_use]
pub fn scan_decode(timestamps: &[MediaTime]) -> TimestampReport {
    let mut anomalies = Vec::new();

    for (index, &time) in timestamps.iter().enumerate() {
        if let Some(&previous) = index.checked_sub(1).and_then(|i| timestamps.get(i)) {
            if time < previous {
                anomalies.push(Anomaly::NonMonotonicDts {
                    index,
                    previous,
                    observed: time,
                });
            }
        }
    }

    TimestampReport {
        anomalies,
        sample_count: timestamps.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: i64) -> MediaTime {
        MediaTime::from_millis(value)
    }

    #[test]
    fn evenly_spaced_timestamps_are_clean() {
        let ts: Vec<MediaTime> = (0..50).map(|i| ms(i * 40)).collect();
        let report = scan_presentation(&ts, ms(1));
        assert!(report.is_clean());
        assert_eq!(report.sample_count, 50);
    }

    #[test]
    fn backwards_timestamps_are_flagged() {
        let ts = vec![ms(0), ms(40), ms(80), ms(40), ms(120)];
        let report = scan_presentation(&ts, ms(1));
        assert_eq!(
            report.count_of(|a| matches!(a, Anomaly::NonMonotonicPts { .. })),
            1
        );
    }

    #[test]
    fn large_gaps_are_flagged() {
        let ts = vec![ms(0), ms(40), ms(1_000)];
        let report = scan_presentation(&ts, ms(1));
        let gaps = report.count_of(|a| matches!(a, Anomaly::Gap { .. }));
        assert_eq!(gaps, 1);
    }

    #[test]
    fn tolerance_absorbs_ordinary_variation() {
        // Jittery but legitimate variable frame rate must not be flagged.
        let ts = vec![ms(0), ms(40), ms(81), ms(120), ms(161)];
        let report = scan_presentation(&ts, ms(5));
        assert!(
            report.is_clean(),
            "a zero tolerance would flag normal frame-rate variation"
        );
    }

    #[test]
    fn repeated_timestamps_are_flagged_as_overlap() {
        let ts = vec![ms(0), ms(40), ms(40), ms(80)];
        let report = scan_presentation(&ts, ms(1));
        assert_eq!(report.count_of(|a| matches!(a, Anomaly::Overlap { .. })), 1);
    }

    #[test]
    fn negative_timestamps_are_flagged_but_not_as_regressions() {
        // Pre-roll is legitimate (spec §24); it must not also be reported as
        // non-monotonic just because it is negative.
        let ts = vec![ms(-80), ms(-40), ms(0), ms(40)];
        let report = scan_presentation(&ts, ms(1));
        assert_eq!(
            report.count_of(|a| matches!(a, Anomaly::NegativeTimestamp { .. })),
            2
        );
        assert_eq!(
            report.count_of(|a| matches!(a, Anomaly::NonMonotonicPts { .. })),
            0
        );
    }

    #[test]
    fn decode_timestamps_must_not_move_backwards() {
        let dts = vec![ms(0), ms(80), ms(40), ms(120)];
        let report = scan_decode(&dts);
        assert_eq!(
            report.count_of(|a| matches!(a, Anomaly::NonMonotonicDts { .. })),
            1
        );
    }

    #[test]
    fn empty_and_single_sample_input_is_clean() {
        assert!(scan_presentation(&[], ms(1)).is_clean());
        assert!(scan_presentation(&[ms(0)], ms(1)).is_clean());
        assert!(scan_decode(&[]).is_clean());
    }

    #[test]
    fn anomalies_report_their_sample_index() {
        let ts = vec![ms(0), ms(40), ms(10)];
        let report = scan_presentation(&ts, ms(1));
        assert!(report.anomalies.iter().all(|a| a.index() == 2));
    }

    #[test]
    fn scanning_is_deterministic() {
        let ts: Vec<MediaTime> = (0..100).map(|i| ms(i * 37)).collect();
        assert_eq!(scan_presentation(&ts, ms(2)), scan_presentation(&ts, ms(2)));
    }
}
