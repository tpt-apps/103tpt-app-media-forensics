//! Integrated loudness per ITU-R BS.1770-4 (spec §21).
//!
//! # What is implemented
//!
//! The BS.1770-4 integrated-loudness algorithm: K-weighting (a high-shelf
//! filter modelling the head, followed by an RLB high-pass), 400 ms blocks
//! with 75 % overlap, an absolute gate at -70 LUFS, a relative gate at -10 LU
//! below the ungated mean, then the mean of the surviving blocks.
//!
//! # The sample-rate limitation is real and stated
//!
//! BS.1770-4 publishes its K-weighting coefficients **only for 48 kHz**. The
//! standard defines none for other rates, so this module does not guess them.
//!
//! For any other rate it returns [`LoudnessError::UnsupportedSampleRate`]
//! rather than a plausible-looking number. A loudness figure produced from the
//! wrong filter is worse than no figure, because it looks authoritative and
//! nobody can tell it is wrong.
//!
//! Measuring at 44.1 kHz requires redesigning the filter for that rate and
//! validating it against a reference implementation — not resampling.

use crate::measurement::{Measurement, Methodology};

/// The sample rate BS.1770-4 defines K-weighting coefficients for.
pub const SPECIFIED_SAMPLE_RATE: u32 = 48_000;

/// Block length for BS.1770-4 gating, in milliseconds.
const BLOCK_MS: u32 = 400;

/// Short-term window for loudness range, in milliseconds.
///
/// 3 seconds per EBU Tech 3342. Long enough that a short-term value is stable
/// enough to take a percentile over, which is the whole basis of LRA.
const SHORT_TERM_MS: u32 = 3_000;

/// Lower percentile bounding loudness range, per EBU Tech 3342.
///
/// 10 rather than 0: a single silent instant should not define the bottom of a
/// programme.
const LRA_LOW_PERCENTILE: f64 = 10.0;

/// Upper percentile bounding loudness range, per EBU Tech 3342.
///
/// 95 rather than 100, for the same reason at the other end.
const LRA_HIGH_PERCENTILE: f64 = 95.0;

/// Overlap between consecutive blocks, as a percentage.
const BLOCK_OVERLAP_PERCENT: u32 = 75;

/// Absolute gate: blocks at or below this loudness are discarded.
const ABSOLUTE_GATE_LUFS: f64 = -70.0;

/// Relative gate: blocks this far below the ungated mean are discarded.
const RELATIVE_GATE_LU_DB: f64 = -10.0;

/// Why a loudness measurement could not be produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LoudnessError {
    /// The sample rate has no published K-weighting coefficients.
    #[error(
        "ITU-R BS.1770-4 specifies K-weighting only for 48 kHz; {0} Hz has no \
         defined coefficients, so no loudness figure is reported"
    )]
    UnsupportedSampleRate(u32),

    /// The signal is shorter than one 400 ms gating block.
    #[error("signal is shorter than the 400 ms gating block")]
    TooShort,

    /// The signal is shorter than one 3 s short-term window.
    ///
    /// A distinct variant from [`LoudnessError::TooShort`] rather than a shared
    /// one: a 2-second file is long enough for integrated loudness and too short
    /// for loudness range, and reporting the same message for both would hide
    /// that the two measurements need different amounts of audio.
    #[error("signal is shorter than the 3 s short-term window loudness range requires")]
    TooShortForShortTerm,
}

/// Computes integrated gated loudness in LUFS.
///
/// `samples` is interleaved `f32` PCM normalised to +/-1.0.
///
/// # Errors
///
/// Returns an error when the sample rate is not one BS.1770-4 defines, or the
/// signal is shorter than a single gating block. Both cases yield no number
/// rather than an approximation.
pub fn integrated_loudness(
    samples: &[f32],
    channels: u16,
    sample_rate: u32,
) -> Result<Measurement, LoudnessError> {
    if sample_rate != SPECIFIED_SAMPLE_RATE {
        return Err(LoudnessError::UnsupportedSampleRate(sample_rate));
    }

    let channels = channels.max(1) as usize;
    if samples.len() < channels * block_length_for(sample_rate, BLOCK_MS) {
        return Err(LoudnessError::TooShort);
    }

    let blocks = block_loudness_series(samples, channels, sample_rate, BLOCK_MS);
    Ok(Measurement::new(
        gated_loudness(&blocks),
        Methodology::ItuBs1770_4,
    ))
}

/// Computes loudness range in LU, per EBU Tech 3342 (spec §22).
///
/// # What loudness range is
///
/// LRA describes **how much the level moves across a programme**, which is a
/// different question from how loud it is. Integrated loudness says "this is -23
/// LUFS on average"; loudness range says "the quiet parts sit 12 LU below the
/// loud parts". The second is what tells a delivery engineer whether a
/// downstream system must handle a wide dynamic span.
///
/// It is the difference between the 10th and 95th percentile of *short-term*
/// loudness, where short-term means a 3-second window. The percentiles are the
/// whole method: 10 rather than 0 because a single silent instant should not
/// define the bottom of a programme, and 95 rather than 100 for the same reason
/// at the top.
///
/// # What it does not tell you
///
/// LRA is computed from 3-second windows, so it cannot see anything shorter than
/// that — a click, a single quiet frame, or an edit under 3 seconds. A file with
/// a large LRA has genuine level movement across seconds; it does not have
/// "more dynamic range" in any perceptual sense. The 3-second window is stated
/// with the figure for that reason.
///
/// # Errors
///
/// Same limitations as [`integrated_loudness`]: the K-weighting coefficients
/// BS.1770-4 defines exist only at 48 kHz, and a signal shorter than one
/// short-term window yields no figure rather than an approximation.
pub fn loudness_range(
    samples: &[f32],
    channels: u16,
    sample_rate: u32,
) -> Result<Measurement, LoudnessError> {
    if sample_rate != SPECIFIED_SAMPLE_RATE {
        return Err(LoudnessError::UnsupportedSampleRate(sample_rate));
    }

    let channels = channels.max(1) as usize;
    if samples.len() < channels * short_term_length(sample_rate) {
        return Err(LoudnessError::TooShortForShortTerm);
    }

    let blocks = block_loudness_series(samples, channels, sample_rate, SHORT_TERM_MS);
    Ok(Measurement::new(
        loudness_range_lu(&blocks),
        Methodology::EbuR128Lra,
    ))
}

/// Computes the range figure from a series of block loudness values.
///
/// Split out so the percentile behaviour is testable without synthesising a
/// multi-second signal for every case, which is what makes the gating rules
/// reviewable at all.
fn loudness_range_lu(blocks: &[f64]) -> f64 {
    // The same absolute gate as integrated loudness. Without it, digital silence
    // between passages would drag the 10th percentile to the floor and report a
    // range of 70 LU for a programme whose actual dynamics are modest.
    let mut above_gate: Vec<f64> = blocks
        .iter()
        .copied()
        .filter(|l| *l > ABSOLUTE_GATE_LUFS)
        .collect();

    if above_gate.is_empty() {
        return 0.0;
    }

    above_gate.sort_by(|a, b| a.total_cmp(b));
    let low = percentile(&above_gate, LRA_LOW_PERCENTILE);
    let high = percentile(&above_gate, LRA_HIGH_PERCENTILE);
    // Never negative: a single repeated value gives two identical percentiles,
    // and floating-point arithmetic around them could otherwise produce -0.0 or a
    // hair below zero, which a report would print as a negative range.
    (high - low).max(0.0)
}

/// Linearly interpolated percentile of an already-sorted slice.
///
/// Linear interpolation rather than nearest-rank, because the nearest-rank form
/// returns an input value and so quantises the result to whatever the block
/// spacing happened to be. The interpolated form moves smoothly as a signal
/// changes, which is what a measurement of a continuous quantity should do.
fn percentile(sorted: &[f64], percent: f64) -> f64 {
    if sorted.is_empty() {
        return ABSOLUTE_GATE_LUFS;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = percent / 100.0 * (sorted.len() - 1) as f64;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    if lower == upper {
        return sorted[lower];
    }
    let weight = rank - lower as f64;
    sorted[lower] * (1.0 - weight) + sorted[upper] * weight
}

/// Length of one short-term window, in samples.
///
/// 3 seconds per EBU Tech 3342. Much longer than the 400 ms integrated block,
/// because a short-term value is meant to be stable enough to percentile.
fn short_term_length(sample_rate: u32) -> usize {
    (sample_rate * SHORT_TERM_MS / 1_000) as usize
}

/// Length of one gating block, in samples, for a window of `window_ms`.
fn block_length_for(sample_rate: u32, window_ms: u32) -> usize {
    (sample_rate * window_ms / 1_000) as usize
}

/// Hop between consecutive blocks: a 25 % step gives 75 % overlap.
///
/// Held at 75 % for both windows. EBU Tech 3342's short-term blocks use the same
/// overlap, and sharing the constant keeps the two measurements stepping
/// through the same signal at the same rate.
fn hop_length_for(sample_rate: u32, window_ms: u32) -> usize {
    (sample_rate * window_ms * (100 - BLOCK_OVERLAP_PERCENT) / 100 / 1_000) as usize
}

/// Computes the loudness of every block of `window_ms`, in LUFS.
fn block_loudness_series(
    samples: &[f32],
    channels: usize,
    sample_rate: u32,
    window_ms: u32,
) -> Vec<f64> {
    let block = block_length_for(sample_rate, window_ms);
    let hop = hop_length_for(sample_rate, window_ms).max(1);
    let frames = samples.len() / channels;
    let mut out = Vec::new();

    let mut start = 0usize;
    while start + block <= frames {
        out.push(block_loudness(samples, start, block, channels));
        start += hop;
    }
    out
}

/// Computes the loudness of a single block, in LUFS.
fn block_loudness(samples: &[f32], start_frame: usize, block: usize, channels: usize) -> f64 {
    // BS.1770-4 K-weighting at 48 kHz: high-shelf (head), then RLB high-pass.
    const STAGE1: [f64; 5] = [
        1.53512485958697,
        -2.69169618940638,
        1.19839281085285,
        -1.69065929318241,
        0.73248077421585,
    ];
    const STAGE2: [f64; 5] = [1.0, -2.0, 1.0, -1.99004745483398, 0.99007225036621];

    let mut stage1 = Biquad::new(STAGE1, channels);
    let mut stage2 = Biquad::new(STAGE2, channels);
    let mut energy = 0.0f64;

    for i in 0..block {
        let frame = start_frame + i;
        for channel in 0..channels {
            let raw = samples[frame * channels + channel] as f64;
            let filtered = stage2.process(channel, stage1.process(channel, raw));
            energy += channel_weight() * filtered * filtered;
        }
    }

    let mean_square = energy / block as f64;
    if mean_square <= 0.0 {
        ABSOLUTE_GATE_LUFS
    } else {
        -0.691 + 10.0 * mean_square.log10()
    }
}

/// Returns the BS.1770-4 channel weight.
///
/// Mono and stereo weigh 1.0 per channel, which is exact. Surround layouts
/// assign reduced weights to LFE and surround channels, but that depends on the
/// channel layout, which is not known at this layer. Assuming a layout would
/// emit a figure that looks compliant but is not, so every channel weighs 1.0
/// and the limitation is recorded with the measurement.
const fn channel_weight() -> f64 {
    1.0
}

/// Applies the BS.1770-4 absolute and relative gates, then averages.
fn gated_loudness(blocks: &[f64]) -> f64 {
    let above_absolute: Vec<f64> = blocks
        .iter()
        .copied()
        .filter(|l| *l > ABSOLUTE_GATE_LUFS)
        .collect();

    if above_absolute.is_empty() {
        return ABSOLUTE_GATE_LUFS;
    }

    let ungated_mean = mean_loudness(&above_absolute);
    let relative_threshold = ungated_mean + RELATIVE_GATE_LU_DB;

    let gated: Vec<f64> = above_absolute
        .iter()
        .copied()
        .filter(|l| *l > relative_threshold)
        .collect();

    if gated.is_empty() {
        ungated_mean
    } else {
        mean_loudness(&gated)
    }
}

/// Mean of block loudness values, in LUFS.
///
/// Gating operates on loudness values in the log domain, so this is the
/// arithmetic mean of the LUFS figures rather than a mean of linear energies.
fn mean_loudness(values: &[f64]) -> f64 {
    if values.is_empty() {
        return ABSOLUTE_GATE_LUFS;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

/// A direct-form-I biquad with per-channel delay lines.
///
/// Per-channel state matters: sharing delay lines across channels would mix
/// them, producing a figure that is neither stereo nor mono.
struct Biquad {
    coefficients: [f64; 5],
    x1: Vec<f64>,
    x2: Vec<f64>,
    y1: Vec<f64>,
    y2: Vec<f64>,
}

impl Biquad {
    /// Builds a filter over `channels` independent delay lines.
    fn new(coefficients: [f64; 5], channels: usize) -> Self {
        Self {
            coefficients,
            x1: vec![0.0; channels],
            x2: vec![0.0; channels],
            y1: vec![0.0; channels],
            y2: vec![0.0; channels],
        }
    }

    /// Processes one sample for one channel.
    fn process(&mut self, channel: usize, input: f64) -> f64 {
        let [b0, b1, b2, a1, a2] = self.coefficients;
        let output = b0 * input + b1 * self.x1[channel] + b2 * self.x2[channel]
            - a1 * self.y1[channel]
            - a2 * self.y2[channel];
        self.x2[channel] = self.x1[channel];
        self.x1[channel] = input;
        self.y2[channel] = self.y1[channel];
        self.y1[channel] = output;
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// Generates `seconds` of a full-scale sine, interleaved over `channels`.
    fn sine(seconds: f64, freq: f64, channels: u16) -> Vec<f32> {
        let n = (RATE as f64 * seconds) as usize;
        let channels = channels as usize;
        (0..n)
            .flat_map(|i| {
                let value = (std::f64::consts::TAU * freq * i as f64 / RATE as f64).sin() as f32;
                std::iter::repeat_n(value, channels)
            })
            .collect()
    }

    #[test]
    fn a_full_scale_sine_measures_near_minus_three_lufs() {
        // BS.1770-4 test signal: a full-scale 1 kHz sine reads about -3.01 LUFS.
        let result = integrated_loudness(&sine(3.0, 1_000.0, 1), 1, RATE).expect("measures");
        assert!(
            (result.value - (-3.01)).abs() < 0.5,
            "expected about -3 LUFS, got {}",
            result.value
        );
    }

    #[test]
    fn the_result_names_its_standard() {
        let result = integrated_loudness(&sine(3.0, 1_000.0, 1), 1, RATE).expect("measures");
        assert_eq!(result.methodology, Methodology::ItuBs1770_4);
        assert!(result.describe().contains("ITU-R BS.1770-4"));
    }

    #[test]
    fn half_amplitude_is_six_db_lower() {
        let full = sine(3.0, 1_000.0, 1);
        let half: Vec<f32> = full.iter().map(|s| s * 0.5).collect();

        let a = integrated_loudness(&full, 1, RATE).expect("measures");
        let b = integrated_loudness(&half, 1, RATE).expect("measures");
        let drop = a.value - b.value;
        assert!(
            (drop - 6.0).abs() < 0.5,
            "halving amplitude should drop loudness by about 6 dB, got {drop}"
        );
    }

    #[test]
    fn unsupported_sample_rates_report_no_number() {
        let samples = sine(3.0, 1_000.0, 1);
        let result = integrated_loudness(&samples, 1, 44_100);
        assert!(matches!(
            result,
            Err(LoudnessError::UnsupportedSampleRate(44_100))
        ));
        assert!(result.unwrap_err().to_string().contains("48 kHz"));
    }

    #[test]
    fn a_signal_shorter_than_one_block_is_refused() {
        assert!(matches!(
            integrated_loudness(&sine(0.1, 1_000.0, 1), 1, RATE),
            Err(LoudnessError::TooShort)
        ));
    }

    #[test]
    fn digital_silence_lands_at_the_absolute_gate() {
        let samples = vec![0.0f32; (RATE as usize) * 2];
        let result = integrated_loudness(&samples, 1, RATE).expect("measures");
        assert!(result.value <= ABSOLUTE_GATE_LUFS);
    }

    #[test]
    fn measurement_is_deterministic() {
        // spec §77 requires identical results across runs.
        let samples = sine(3.0, 1_000.0, 2);
        let a = integrated_loudness(&samples, 2, RATE).expect("measures");
        let b = integrated_loudness(&samples, 2, RATE).expect("measures");
        assert_eq!(a.value, b.value);
    }

    #[test]
    fn stereo_is_measured() {
        let result = integrated_loudness(&sine(3.0, 1_000.0, 2), 2, RATE).expect("measures");
        assert!(result.value > ABSOLUTE_GATE_LUFS);
    }

    #[test]
    fn a_quiet_passage_is_gated_out() {
        // Three seconds loud, three seconds very quiet. The relative gate should
        // exclude the quiet part, so integrated loudness reflects the loud part.
        let mut samples = sine(3.0, 1_000.0, 1);
        let quiet: Vec<f32> = sine(3.0, 1_000.0, 1).iter().map(|s| s * 0.001).collect();
        samples.extend(quiet);

        let result = integrated_loudness(&samples, 1, RATE).expect("measures");
        let loud_only = integrated_loudness(&sine(3.0, 1_000.0, 1), 1, RATE).expect("measures");
        assert!(
            (result.value - loud_only.value).abs() < 1.0,
            "gating should make the quiet tail contribute little: {} vs {}",
            result.value,
            loud_only.value
        );
    }

    #[test]
    fn a_constant_level_signal_has_no_loudness_range() {
        // The reference case: a tone at one amplitude throughout. Every
        // short-term block reads the same, so the spread is zero.
        let result = loudness_range(&sine(12.0, 1_000.0, 1), 1, RATE).expect("measures");
        assert!(
            result.value < 2.0,
            "a constant signal should have near-zero range, got {}",
            result.value
        );
    }

    #[test]
    fn a_signal_that_steps_between_two_levels_reports_their_difference() {
        // Four seconds loud, then twelve seconds 20 dB quieter. The 10th and 95th
        // percentiles should straddle that 20 dB step.
        let mut samples = sine(4.0, 1_000.0, 1);
        let quiet: Vec<f32> = sine(12.0, 1_000.0, 1).iter().map(|s| s * 0.1).collect();
        samples.extend(quiet);

        let result = loudness_range(&samples, 1, RATE).expect("measures");
        assert!(
            (result.value - 20.0).abs() < 4.0,
            "a 20 dB step should read near 20 LU of range, got {}",
            result.value
        );
    }

    #[test]
    fn a_louder_signal_does_not_change_its_range() {
        // Range is a spread, not a level. Scaling the whole signal must not move
        // it — a figure that rose with level would be measuring loudness again.
        let mut mixed = sine(4.0, 1_000.0, 1);
        let tail: Vec<f32> = sine(10.0, 1_000.0, 1).iter().map(|s| s * 0.1).collect();
        mixed.extend(tail);
        let scaled: Vec<f32> = mixed.iter().map(|s| s * 0.5).collect();

        let a = loudness_range(&mixed, 1, RATE).expect("measures");
        let b = loudness_range(&scaled, 1, RATE).expect("measures");
        assert!(
            (a.value - b.value).abs() < 0.5,
            "range should be level-independent: {} vs {}",
            a.value,
            b.value
        );
    }

    #[test]
    fn a_loudness_range_names_its_standard_and_unit() {
        // A range printed as "LUFS" would present a span as an absolute level.
        let result = loudness_range(&sine(12.0, 1_000.0, 1), 1, RATE).expect("measures");
        assert_eq!(result.methodology, Methodology::EbuR128Lra);

        let described = result.describe();
        assert!(described.contains(" LU"), "{described}");
        assert!(!described.contains("LUFS"), "{described}");
        assert!(described.contains("EBU Tech 3342"), "{described}");
    }

    #[test]
    fn a_signal_shorter_than_one_short_term_window_is_refused() {
        // Two seconds is long enough for integrated loudness and too short for
        // LRA. A distinct error, because the two measurements need different
        // amounts of audio and saying so is the point.
        let result = loudness_range(&sine(2.0, 1_000.0, 1), 1, RATE);
        assert!(matches!(result, Err(LoudnessError::TooShortForShortTerm)));
        assert!(result.unwrap_err().to_string().contains("3 s"));
    }

    #[test]
    fn loudness_range_refuses_an_unsupported_sample_rate() {
        let result = loudness_range(&sine(12.0, 1_000.0, 1), 1, 44_100);
        assert!(matches!(
            result,
            Err(LoudnessError::UnsupportedSampleRate(44_100))
        ));
    }

    #[test]
    fn digital_silence_reports_zero_range_rather_than_a_spike() {
        // Without the absolute gate, silent blocks would drag the 10th percentile
        // to the floor and a silent file would report a huge range.
        let samples = vec![0.0f32; (RATE as usize) * 6];
        let result = loudness_range(&samples, 1, RATE).expect("measures");
        assert_eq!(result.value, 0.0, "silence has no level movement");
    }

    #[test]
    fn a_range_is_never_negative() {
        // A single repeated value makes both percentiles identical; floating
        // point around them must not produce a negative span.
        for value in [0.0, -3.0, -20.0] {
            let range = loudness_range_lu(&[value; 8]);
            assert!(range >= 0.0, "range {range} for uniform level {value}");
        }
    }

    #[test]
    fn the_percentile_interpolates_between_neighbours() {
        // Nearest-rank would return an input value and quantise the result;
        // interpolation is what lets the figure move smoothly.
        assert!((percentile(&[0.0, 10.0], 0.0) - 0.0).abs() < 1e-9);
        assert!((percentile(&[0.0, 10.0], 100.0) - 10.0).abs() < 1e-9);
        assert!((percentile(&[0.0, 10.0], 50.0) - 5.0).abs() < 1e-9);
        assert!((percentile(&[10.0], 50.0) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn blocks_below_the_absolute_gate_are_excluded() {
        // -80 LUFS is below the -70 gate. If it counted, this would report a wide
        // range that a real programme's gating would not.
        let with_gate = loudness_range_lu(&[-20.0, -20.0, -20.0, -20.0]);
        let without_gate = loudness_range_lu(&[-80.0, -20.0, -20.0, -20.0]);
        assert!((with_gate - 0.0).abs() < 1e-9);
        assert!(
            (with_gate - without_gate).abs() < 1e-9,
            "a gated-out block should not change the range"
        );
    }

    #[test]
    fn loudness_range_is_deterministic() {
        // spec §77 requires identical results across runs.
        let mut samples = sine(4.0, 1_000.0, 1);
        let tail: Vec<f32> = sine(9.0, 1_000.0, 1).iter().map(|s| s * 0.2).collect();
        samples.extend(tail);

        let a = loudness_range(&samples, 1, RATE).expect("measures");
        let b = loudness_range(&samples, 1, RATE).expect("measures");
        assert_eq!(a.value, b.value);
    }

    #[test]
    fn integrated_loudness_and_loudness_range_measure_different_things() {
        // The pair that most needs separating: a file with high range can be
        // entirely ordinary in absolute terms.
        let mut varied = sine(4.0, 1_000.0, 1);
        let tail: Vec<f32> = sine(12.0, 1_000.0, 1).iter().map(|s| s * 0.05).collect();
        varied.extend(tail);

        let level = integrated_loudness(&varied, 1, RATE).expect("measures");
        let range = loudness_range(&varied, 1, RATE).expect("measures");
        assert_ne!(
            level.methodology, range.methodology,
            "two different quantities must not share a methodology"
        );
    }
}
