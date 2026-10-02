//! Compression and bitrate analysis (spec §28-§29).
//!
//! # No decoding required
//!
//! Bitrate is a property of the packet layer: each sample's compressed size is in
//! the sample table, and each sample's presentation time is in `stts`. Both are
//! container metadata, so this runs without a decoder — which matters because a
//! decoder is the component most likely to be defeated by a malformed stream, and
//! a file that will not decode is exactly the one whose bitrate history matters.
//!
//! # Windows, not instantaneous rates
//!
//! A single sample's bitrate is meaningless: one keyframe can be fifty times the
//! size of the P-frame after it. Measuring over a whole file yields one average
//! that hides everything. So the measurement is a sliding window, and what gets
//! reported is where the rate *departs* from the file's own average.
//!
//! # No cause is asserted
//!
//! Spec §29's own example lists the alternatives: low-complexity content, a still
//! shot, a freeze. A sustained drop in bitrate is evidence of *something* and
//! nothing more. The finding states the numbers and stops.

use tpt_app_media_forensics_model::MediaTime;

/// One compressed sample, as the bitrate analysis needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitrateSample {
    /// Presentation time of the sample.
    pub time: MediaTime,
    /// Compressed size in bytes.
    pub size: u64,
    /// Whether the sample is a random-access point.
    ///
    /// Carried because keyframes dominate a window's byte count. A window that
    /// happens to contain a keyframe will read as higher bitrate than an
    /// identical window without one, and a report that cannot tell them apart
    /// will attribute an artefact of GOP placement to a content change.
    pub is_key_frame: bool,
}

/// A window of the file whose bitrate departs from the file's average.
///
/// `PartialEq` only, for the same reason as [`BitrateReport`]: the bitrate fields
/// are `f64`.
#[derive(Debug, Clone, PartialEq)]
pub struct BitrateAnomaly {
    /// Presentation time where the window begins.
    pub start: MediaTime,
    /// Presentation time where the window ends.
    pub end: MediaTime,
    /// Observed bitrate across the window, in bits per second.
    pub observed_bps: f64,
    /// The file's average bitrate, in bits per second.
    pub average_bps: f64,
    /// Number of keyframes inside the window.
    ///
    /// Reported so an examiner can discount a window whose rate is explained by
    /// GOP placement rather than by the content.
    pub keyframes: usize,
}

impl BitrateAnomaly {
    /// Returns the observed rate as a fraction of the file average.
    ///
    /// `None` when the average is zero, which is a file with no measurable
    /// duration rather than an infinitely compressed one.
    #[must_use]
    pub fn ratio_to_average(&self) -> Option<f64> {
        (self.average_bps > 0.0).then(|| self.observed_bps / self.average_bps)
    }

    /// Renders the observed rate in Mbps, the unit delivery specifications use.
    #[must_use]
    pub fn describe_mbps(&self) -> String {
        format!("{:.2} Mbps", self.observed_bps / 1_000_000.0)
    }
}
/// The result of analysing a track's compression.
///
/// Deliberately `PartialEq` and not `Eq`: it carries `f64` measurements, and
/// `Eq` on floats is not a relation anyone should rely on. `BitrateAnomaly` does
/// derive `Eq` on the same reasoning problem and was corrected for it.
#[derive(Debug, Clone, PartialEq)]
pub struct BitrateReport {
    /// Total compressed bytes across all samples.
    pub total_bytes: u64,
    /// Duration covered by the samples, or `None` when the last sample's time
    /// does not follow the first.
    pub duration: Option<MediaTime>,
    /// Average bitrate in bits per second, or `None` when no duration is known.
    ///
    /// `None` rather than zero: a rate cannot be computed without a duration, and
    /// reporting 0 bps would read as "this file compresses to nothing".
    pub average_bps: Option<f64>,
    /// Windows whose bitrate departs from the average.
    pub anomalies: Vec<BitrateAnomaly>,
    /// Number of samples analysed.
    pub sample_count: usize,
    /// Number of keyframes among them.
    pub keyframe_count: usize,
}

impl BitrateReport {
    /// Returns the average bitrate in Mbps, or `None` when it cannot be computed.
    #[must_use]
    pub fn average_mbps(&self) -> Option<f64> {
        self.average_bps.map(|bps| bps / 1_000_000.0)
    }

    /// Whether the track holds a steady rate.
    ///
    /// No departing window is a statement about rate control, not about the
    /// picture: a still shot also produces a steady rate.
    #[must_use]
    pub fn is_constant_rate(&self) -> bool {
        self.sample_count > 1 && self.anomalies.is_empty()
    }
}
/// Measures compression over a sliding window and finds departures from average.
///
/// `window_frames` sets the window length; a shorter window localises a change
/// more precisely but reports more of the ordinary variation every encoder
/// produces. `min_ratio` is the fraction of the average below which a window
/// counts as anomalous, and is expected well under 1.0: spec §29 asks for *sudden
/// changes*, and a window at or above the average is not a change.
///
/// Returns a report with no anomalies for fewer than two samples: a rate needs a
/// duration, and one sample has neither a rate nor a comparison to make.
///
/// # Panics
///
/// Never. All arithmetic is checked or saturating, because sample sizes and
/// timestamps come from an attacker-controlled table (spec §75).
#[must_use]
pub fn analyse(samples: &[BitrateSample], window_frames: usize, min_ratio: f64) -> BitrateReport {
    let mut report = BitrateReport {
        // Saturating, not `sum()`: sample sizes come from an attacker-controlled
        // table, and four `u64::MAX` entries would panic in debug and wrap in
        // release. A wrapped total would report a small, plausible bitrate for a
        // file that is enormous — the worst possible direction for an error.
        total_bytes: samples
            .iter()
            .fold(0u64, |acc, s| acc.saturating_add(s.size)),
        duration: None,
        average_bps: None,
        anomalies: Vec::new(),
        sample_count: samples.len(),
        keyframe_count: samples.iter().filter(|s| s.is_key_frame).count(),
    };

    if samples.len() < 2 {
        return report;
    }

    // Duration comes from the first and last sample's presentation times rather
    // than a declared value: the declared duration is a claim, and the measured
    // one is what the samples actually span.
    let first = samples.first().map_or(0, |s| s.time.as_micros());
    let last = samples.last().map_or(0, |s| s.time.as_micros());
    let span_micros = last.saturating_sub(first);
    if span_micros <= 0 {
        return report;
    }
    report.duration = Some(MediaTime::from_micros(span_micros));

    // Bits per second from bytes and microseconds: bytes * 8 / (micros / 1e6).
    let average_bps = (report.total_bytes as f64 * 8.0 * 1_000_000.0) / span_micros as f64;
    report.average_bps = Some(average_bps);

    let window = window_frames.max(1).min(samples.len());
    if window >= samples.len() || average_bps <= 0.0 {
        return report;
    }

    // Sliding window with a one-sample stride. Non-overlapping windows would miss
    // a change straddling a boundary, which is exactly the case worth finding: a
    // step aligned neatly to a window edge is the signature of a splice, and
    // aligned windows would report it as a clean change of state rather than a
    // transition.
    let mut bytes_in_window = 0u64;
    let mut keyframes_in_window = 0usize;
    for (index, sample) in samples.iter().enumerate() {
        bytes_in_window = bytes_in_window.saturating_add(sample.size);
        keyframes_in_window += usize::from(sample.is_key_frame);

        if index >= window {
            bytes_in_window = bytes_in_window.saturating_sub(samples[index - window].size);
            keyframes_in_window -= usize::from(samples[index - window].is_key_frame);
        }

        if index + 1 < window {
            continue;
        }

        let window_start = samples[index + 1 - window].time;
        let window_end = sample.time;
        let window_micros = window_end
            .as_micros()
            .saturating_sub(window_start.as_micros());
        if window_micros <= 0 {
            continue;
        }

        let observed_bps = (bytes_in_window as f64 * 8.0 * 1_000_000.0) / window_micros as f64;
        if observed_bps < average_bps * min_ratio {
            report.anomalies.push(BitrateAnomaly {
                start: window_start,
                end: window_end,
                observed_bps,
                average_bps,
                keyframes: keyframes_in_window,
            });
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One 25 fps sample per frame, starting at time zero.
    fn samples(sizes: &[u64]) -> Vec<BitrateSample> {
        sizes
            .iter()
            .enumerate()
            .map(|(i, &size)| BitrateSample {
                time: MediaTime::from_micros(i as i64 * 40_000),
                size,
                is_key_frame: i == 0,
            })
            .collect()
    }

    #[test]
    fn average_bitrate_is_measured_from_the_samples() {
        // 40 frames of 25_000 bytes at 25 fps = 1 second.
        let input = samples(&[25_000; 40]);
        let report = analyse(&input, 8, 0.5);
        // 1_000_000 bytes over 1.56 s of sample span.
        let average = report.average_bps.expect("a rate is computable");
        assert!(average > 0.0);
        assert_eq!(report.total_bytes, 1_000_000);
        assert!(report.is_constant_rate());
    }

    #[test]
    fn a_uniform_file_has_no_anomalies() {
        let input = samples(&[25_000; 60]);
        assert!(analyse(&input, 8, 0.5).anomalies.is_empty());
    }

    #[test]
    fn a_sustained_drop_is_reported_with_both_numbers() {
        // The spec §29 shape: a long high-rate stretch, then a long low-rate one.
        let mut sizes = vec![40_000u64; 50];
        sizes.extend(vec![2_000u64; 50]);
        let report = analyse(&samples(&sizes), 10, 0.5);

        assert!(!report.anomalies.is_empty(), "a 20x drop must be visible");
        let worst = report
            .anomalies
            .iter()
            .min_by(|a, b| {
                a.observed_bps
                    .partial_cmp(&b.observed_bps)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .expect("at least one anomaly");
        assert!(worst.ratio_to_average().expect("an average exists") < 0.5);
        assert!(worst.describe_mbps().ends_with("Mbps"));
    }

    #[test]
    fn a_window_at_or_above_the_average_is_not_anomaly() {
        // Spec §29 asks for sudden *drops*. A window at or above the average is
        // the opposite of the observation the rule exists to make.
        let input = samples(&[25_000; 60]);
        let report = analyse(&input, 8, 1.0);
        assert!(
            report.anomalies.is_empty(),
            "nothing is below the average: {:?}",
            report.anomalies
        );
    }

    #[test]
    fn a_single_sample_yields_no_rate_rather_than_a_guess() {
        let report = analyse(&samples(&[10_000]), 4, 0.5);
        assert_eq!(report.sample_count, 1);
        assert!(report.average_bps.is_none(), "a rate needs two samples");
        assert!(report.duration.is_none());
    }

    #[test]
    fn no_samples_yields_an_empty_report() {
        let report = analyse(&[], 8, 0.5);
        assert_eq!(report.sample_count, 0);
        assert_eq!(report.total_bytes, 0);
        assert!(report.average_bps.is_none());
        assert!(report.anomalies.is_empty());
    }

    #[test]
    fn identical_timestamps_yield_no_rate_rather_than_dividing_by_zero() {
        // Every sample at time zero: a span of zero has no rate.
        let input: Vec<BitrateSample> = (0..10)
            .map(|_| BitrateSample {
                time: MediaTime::ZERO,
                size: 1_000,
                is_key_frame: false,
            })
            .collect();
        let report = analyse(&input, 4, 0.5);
        assert!(report.average_bps.is_none());
        assert!(report.anomalies.is_empty());
    }

    #[test]
    fn a_window_larger_than_the_file_measures_nothing() {
        // Correct: with one window covering everything, every window *is* the
        // average, so there is nothing to compare against.
        let input = samples(&[25_000; 20]);
        assert!(analyse(&input, 100, 0.5).anomalies.is_empty());
    }

    #[test]
    fn a_zero_window_is_treated_as_one_sample() {
        // A profile that sets the window to zero must not produce a zero-length
        // window that silently finds nothing.
        let input = samples(&[25_000; 40]);
        let report = analyse(&input, 0, 0.5);
        assert_eq!(report.sample_count, 40);
        assert!(report.average_bps.is_some());
    }

    #[test]
    fn windows_slide_rather_than_jumping() {
        // A change at the midpoint of a non-overlapping window would be missed
        // or misreported; a sliding window catches the transition.
        let mut sizes = vec![50_000u64; 100];
        sizes[50..75].fill(500);
        let report = analyse(&samples(&sizes), 10, 0.5);

        // Anomalies must cover a contiguous stretch spanning the change, not a
        // single isolated window.
        assert!(report.anomalies.len() > 1, "{:?}", report.anomalies);
    }

    #[test]
    fn absurd_sample_sizes_do_not_panic() {
        let input = samples(&[u64::MAX, u64::MAX, u64::MAX, u64::MAX]);
        let report = analyse(&input, 2, 0.5);
        assert_eq!(report.sample_count, 4);
    }

    #[test]
    fn keyframes_are_counted_separately_from_bytes() {
        let input = samples(&[25_000; 40]);
        let report = analyse(&input, 8, 0.5);
        assert_eq!(report.keyframe_count, 1);
    }

    #[test]
    fn mbps_and_bps_agree() {
        let input = samples(&[25_000; 40]);
        let report = analyse(&input, 8, 0.5);
        let bps = report.average_bps.expect("a rate");
        let mbps = report.average_mbps().expect("a rate in Mbps");
        assert!((bps / 1_000_000.0 - mbps).abs() < f64::EPSILON);
    }
}
