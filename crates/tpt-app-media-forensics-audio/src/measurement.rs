//! Audio measurements that carry their own methodology (spec §21).
//!
//! # Why the method is part of the type
//!
//! Spec §21 is unambiguous: *"Measurements must specify the standard/method
//! used. Never report a number without identifying the methodology."*
//!
//! A bare `f64` cannot satisfy that — nothing stops a caller writing it into a
//! report without context. So every measurement here is a
//! [`Measurement`] carrying a [`Methodology`], and constructing one requires
//! naming the method. The type system enforces the rule that the prose cannot.
//!
//! Two figures computed differently are not comparable: a -23 LUFS integrated
//! loudness and a -23 dBFS RMS are the same digits and different quantities.
//! Printing them without labels is how an otherwise sound report becomes
//! indefensible.

use serde::{Deserialize, Serialize};

/// A named, citable method behind a measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Methodology {
    /// Integrated loudness per ITU-R BS.1770-4, gated, with K-weighting.
    /// Equivalent to EBU R128 integrated loudness.
    ItuBs1770_4,
    /// Loudness range per EBU Tech 3342: 10th to 95th percentile of 3-second
    /// short-term loudness.
    ///
    /// A separate variant from `ItuBs1770_4` rather than a reuse of it: LRA and
    /// integrated loudness are different quantities computed with different
    /// windows, and printing one under the other's name would make two
    /// incompatible numbers look comparable.
    EbuR128Lra,
    /// Sample-peak / true-peak style level measurement relative to full scale.
    Dbfs,
    /// A detection threshold on sample amplitude.
    AmplitudeThreshold,
    /// The arithmetic mean of signed samples.
    SampleMean,
    /// Ratio of the loudest to the quietest meaningful level.
    DynamicRange,
    /// Hann-windowed FFT, 2048-point frames with 50% overlap.
    ///
    /// The window length and overlap are part of the method: a spectrum computed
    /// with different parameters is a different measurement, not a rougher one,
    /// and its figures are not comparable.
    HannWindowFft,
    /// Sample amplitude on the normalised 0.0 to 1.0 scale.
    ///
    /// Distinct from `Dbfs` on purpose. A peak amplitude of `1.0` is 0 dBFS,
    /// and printing the raw amplitude with a "dBFS" unit would misstate the
    /// measurement by an order of magnitude in log terms.
    SampleAmplitude,
}

impl Methodology {
    /// Returns the citation printed alongside the figure.
    #[must_use]
    pub const fn citation(self) -> &'static str {
        match self {
            Self::ItuBs1770_4 => "ITU-R BS.1770-4 / EBU R128",
            Self::EbuR128Lra => {
                "EBU Tech 3342 loudness range, 10th-95th percentile of 3s short-term loudness"
            }
            Self::Dbfs => "dB relative to digital full scale",
            Self::AmplitudeThreshold => "sample amplitude threshold",
            Self::SampleMean => "arithmetic mean of signed samples",
            Self::DynamicRange => "ratio of peak to noise floor",
            Self::HannWindowFft => "2048-point Hann-windowed FFT, 50% frame overlap",
            Self::SampleAmplitude => "sample amplitude, normalised to full scale",
        }
    }
}

/// Converts a normalised amplitude (0.0 to 1.0) to dBFS.
///
/// Returns `None` for digital silence, which has no finite decibel value.
/// Reporting `-inf` as a number would be misleading; the caller reports
/// "digital silence" instead.
#[must_use]
pub fn amplitude_to_dbfs(amplitude: f64) -> Option<f64> {
    if amplitude <= 0.0 {
        return None;
    }
    Some(20.0 * amplitude.log10())
}

/// A measured value together with how it was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Measurement {
    /// The measured value.
    pub value: f64,
    /// The method that produced it.
    pub methodology: Methodology,
}

impl Measurement {
    /// Builds a measurement, naming its methodology.
    #[must_use]
    pub const fn new(value: f64, methodology: Methodology) -> Self {
        Self { value, methodology }
    }

    /// Renders the value with its citation, e.g.
    /// `-23.1 LUFS (ITU-R BS.1770-4 / EBU R128)`.
    ///
    /// The unit is part of the method, so it travels with it; a measurement
    /// cannot be rendered without the information needed to interpret it.
    #[must_use]
    pub fn describe(&self) -> String {
        let unit = match self.methodology {
            Methodology::ItuBs1770_4 => " LUFS",
            // LU, not LUFS: LRA is a *range* between two loudness figures, and
            // "LUFS" would present a span as if it were an absolute level.
            Methodology::EbuR128Lra => " LU",
            Methodology::Dbfs => " dBFS",
            Methodology::AmplitudeThreshold
            | Methodology::SampleMean
            | Methodology::SampleAmplitude => " (normalised 0.0-1.0)",
            Methodology::DynamicRange => " dB",
            // The unit is carried in the citation rather than appended, because
            // a dBFS figure from an FFT is not the same quantity as a dBFS
            // figure of sample peak — same unit, different measurement.
            Methodology::HannWindowFft => " dBFS",
        };
        format!(
            "{:.1}{} ({})",
            self.value,
            unit,
            self.methodology.citation()
        )
    }
}

impl std::fmt::Display for Measurement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

/// A contiguous region of near-silence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SilenceRegion {
    /// Index of the first sample in the region.
    pub start_frame: u64,
    /// Index of the last sample in the region.
    pub end_frame: u64,
    /// Length of the region in samples.
    pub length_frames: u64,
}

/// Where a level measurement applies in the signal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelStats {
    /// Highest absolute sample value, 0.0 to 1.0.
    pub peak: f64,
    /// Root-mean-square level, 0.0 to 1.0.
    pub rms: f64,
    /// Arithmetic mean of signed samples; non-zero indicates DC offset.
    pub mean: f64,
    /// Number of samples examined.
    pub sample_count: u64,
}

/// Adds `value` to `sum` with Kahan compensation.
///
/// Returns the compensation to carry forward and the new sum.
#[inline]
fn kahan_add(sum: f64, value: f64, compensation: f64) -> (f64, f64) {
    let adjusted = value - compensation;
    let next = sum + adjusted;
    (next - sum - adjusted, next)
}

/// Computes peak, RMS, and mean over interleaved `f32` samples.
///
/// Exact and allocation-free. Summation uses Kahan compensation so that a long
/// signal does not accumulate enough rounding error to shift the reported DC
/// offset — a float accumulator would make the result depend on sample count,
/// and therefore not reproducible (spec §77).
#[must_use]
pub fn level_stats(samples: &[f32]) -> LevelStats {
    let mut peak = 0.0f64;
    let mut sum_squares = 0.0f64;
    let mut sum = 0.0f64;
    let mut compensation_squares = 0.0f64;
    let mut compensation_mean = 0.0f64;

    for &sample in samples {
        let value = f64::from(sample);
        peak = peak.max(value.abs());

        // Kahan compensated summation. Two accumulators are needed: one for the
        // energy and one for the signed mean, because a long signal accumulates
        // enough rounding error to shift the reported DC offset.
        let (y, t) = kahan_add(sum_squares, value * value, compensation_squares);
        sum_squares = t;
        compensation_squares = y;

        let (y, t) = kahan_add(sum, value, compensation_mean);
        sum = t;
        compensation_mean = y;
    }
    let count = samples.len() as f64;
    LevelStats {
        peak,
        rms: if count > 0.0 {
            (sum_squares / count).sqrt()
        } else {
            0.0
        },
        mean: if count > 0.0 { sum / count } else { 0.0 },
        sample_count: samples.len() as u64,
    }
}

/// Finds contiguous regions whose peak level stays below `threshold`.
///
/// A fade is not silence. Requiring the *peak* to stay under the threshold
/// across the whole region, rather than the mean, means a fade-in or fade-out
/// is correctly excluded instead of being reported as a silent passage.
#[must_use]
pub fn find_silence(samples: &[f32], threshold: f64, min_length: u64) -> Vec<SilenceRegion> {
    let mut regions = Vec::new();
    let mut start: Option<usize> = None;

    for (index, &sample) in samples.iter().enumerate() {
        if sample.abs() < threshold as f32 {
            start.get_or_insert(index);
        } else if let Some(begin) = start.take() {
            push_region(&mut regions, begin, index, min_length);
        }
    }
    if let Some(begin) = start {
        push_region(&mut regions, begin, samples.len(), min_length);
    }

    regions
}

/// Records a silence region if it is long enough to be meaningful.
fn push_region(regions: &mut Vec<SilenceRegion>, begin: usize, end: usize, min_length: u64) {
    let length = (end.saturating_sub(begin)) as u64;
    if length >= min_length {
        regions.push(SilenceRegion {
            start_frame: begin as u64,
            end_frame: end.saturating_sub(1) as u64,
            length_frames: length,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dbs(rms: f64) -> Measurement {
        Measurement::new(rms, Methodology::Dbfs)
    }

    #[test]
    fn full_scale_amplitude_is_zero_dbfs() {
        // The conversion that prevents "Peak 1.0 dBFS": amplitude 1.0 is 0 dBFS.
        assert_eq!(amplitude_to_dbfs(1.0), Some(0.0));
        let half = amplitude_to_dbfs(0.5).expect("half scale has a level");
        assert!(
            (half + 6.02).abs() < 0.01,
            "expected about -6.02 dBFS, got {half}"
        );
    }

    #[test]
    fn silence_has_no_decibel_value() {
        assert_eq!(amplitude_to_dbfs(0.0), None);
        assert!(amplitude_to_dbfs(0.0).is_none());
    }

    #[test]
    fn amplitude_and_dbfs_carry_different_units() {
        let amplitude = Measurement::new(0.5, Methodology::SampleAmplitude);
        assert!(amplitude.describe().contains("normalised"));
        assert!(!amplitude.describe().contains("dBFS"));

        let level = Measurement::new(-6.02, Methodology::Dbfs);
        assert!(level.describe().contains("dBFS"));
    }

    #[test]
    fn a_measurement_always_reports_its_method() {
        let loudness = Measurement::new(-23.1, Methodology::ItuBs1770_4);
        let text = loudness.describe();
        assert!(text.contains("-23.1"));
        assert!(text.contains("LUFS"));
        assert!(
            text.contains("ITU-R BS.1770-4"),
            "a bare number must never be printed without its method"
        );
    }

    #[test]
    fn units_follow_the_method() {
        assert!(dbs(-3.0).describe().contains("dBFS"));
        assert!(Measurement::new(0.5, Methodology::SampleMean)
            .describe()
            .contains("normalised"));
    }

    #[test]
    fn level_stats_of_silence_are_zero() {
        let stats = level_stats(&[0.0; 100]);
        assert_eq!(stats.peak, 0.0);
        assert_eq!(stats.rms, 0.0);
        assert_eq!(stats.mean, 0.0);
    }

    #[test]
    fn level_stats_of_a_full_scale_square_are_unity() {
        let samples: Vec<f32> = (0..100)
            .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let stats = level_stats(&samples);
        assert!((stats.peak - 1.0).abs() < 1e-6);
        assert!((stats.rms - 1.0).abs() < 1e-6);
        assert!(
            stats.mean.abs() < 1e-6,
            "a symmetric signal has no DC offset"
        );
    }

    #[test]
    fn dc_offset_is_detected() {
        // A constant positive signal: the mean is non-zero, which is exactly
        // what DC offset means. Peak equals mean here because the signal never
        // changes sign or level.
        let stats = level_stats(&vec![0.5f32; 100]);
        assert!(
            (stats.mean - 0.5).abs() < 1e-6,
            "DC offset is a non-zero mean"
        );
        assert!((stats.peak - 0.5).abs() < 1e-6);
        assert!((stats.rms - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_symmetric_signal_has_no_dc_offset() {
        let samples: Vec<f32> = (0..100)
            .map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
            .collect();
        let stats = level_stats(&samples);
        assert!(stats.mean.abs() < 1e-9, "symmetric signals average to zero");
    }

    #[test]
    fn empty_input_does_not_divide_by_zero() {
        let stats = level_stats(&[]);
        assert_eq!(stats.rms, 0.0);
        assert_eq!(stats.sample_count, 0);
    }

    #[test]
    fn silence_is_found_between_loud_sections() {
        let mut samples = vec![0.8f32; 50];
        samples.extend(std::iter::repeat_n(0.0, 100));
        samples.extend(vec![0.8f32; 50]);

        let regions = find_silence(&samples, 0.01, 10);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].length_frames, 100);
        assert_eq!(regions[0].start_frame, 50);
    }

    #[test]
    fn a_fade_is_not_silence() {
        // Peak within the region exceeds the threshold, so this is not silence
        // even though its mean would be low.
        let mut samples: Vec<f32> = (0..50).map(|i| (i as f32 / 50.0) * 0.8).collect();
        samples.extend(std::iter::repeat_n(0.8f32, 50));
        assert!(find_silence(&samples, 0.01, 10).is_empty());
    }

    #[test]
    fn short_silence_below_the_minimum_is_ignored() {
        let mut samples = vec![0.0f32; 5];
        samples.extend(vec![0.8f32; 50]);
        assert!(find_silence(&samples, 0.01, 10).is_empty());
    }

    #[test]
    fn silence_at_the_very_end_is_found() {
        let mut samples = vec![0.8f32; 50];
        samples.extend(std::iter::repeat_n(0.0, 100));
        assert_eq!(find_silence(&samples, 0.01, 10).len(), 1);
    }

    #[test]
    fn all_silent_input_is_one_region() {
        assert_eq!(find_silence(&vec![0.0; 200], 0.01, 10).len(), 1);
    }

    #[test]
    fn stats_are_deterministic_for_long_input() {
        // A naive float accumulator drifts over this many samples; the
        // compensated one must not.
        let samples: Vec<f32> = (0..200_000).map(|i| (i % 7) as f32 * 0.1).collect();
        let a = level_stats(&samples);
        let b = level_stats(&samples);
        assert_eq!(a.rms, b.rms);
        assert_eq!(a.mean, b.mean);
    }
}
