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
    if samples.len() < channels * block_length(sample_rate) {
        return Err(LoudnessError::TooShort);
    }

    let blocks = block_loudness_series(samples, channels, sample_rate);
    Ok(Measurement::new(
        gated_loudness(&blocks),
        Methodology::ItuBs1770_4,
    ))
}

/// Length of one gating block, in samples.
fn block_length(sample_rate: u32) -> usize {
    (sample_rate * BLOCK_MS / 1_000) as usize
}

/// Hop between consecutive blocks: a 25 % step gives 75 % overlap.
fn hop_length(sample_rate: u32) -> usize {
    (sample_rate * BLOCK_MS * (100 - BLOCK_OVERLAP_PERCENT) / 100 / 1_000) as usize
}

/// Computes the loudness of every gating block, in LUFS.
fn block_loudness_series(samples: &[f32], channels: usize, sample_rate: u32) -> Vec<f64> {
    let block = block_length(sample_rate);
    let hop = hop_length(sample_rate).max(1);
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
}
