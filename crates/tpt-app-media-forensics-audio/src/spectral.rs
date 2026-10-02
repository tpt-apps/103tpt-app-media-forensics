//! Spectral analysis (spec §22).
//!
//! # What this measures, and what it deliberately does not
//!
//! An FFT of an audio file gives a *frequency-domain view of the samples*. It
//! does not tell you what was heard, what the recording site contained, or
//! whether a sound was edited in. Every figure below is reported as an
//! observation about the signal, and each carries the window length and overlap
//! that produced it — a spectrum is meaningless without them, because the same
//! transform with a 1024- and a 4096-point window resolves frequency to
//! different precision.
//!
//! # Windows, and why the choice is reported
//!
//! A rectangular window — no window at all — is the obvious thing to write and
//! the wrong one. It leaks energy from a strong tone across the whole spectrum
//! (spectral leakage), which would make a pure tone look like broadband noise
//! and could make a recording look like it has content where it has none. A Hann
//! window is used instead, and its 1.5-bin equivalent noise bandwidth is stated
//! rather than left implicit, because it is the number that says how finely this
//! analysis can actually distinguish two adjacent tones.
//!
//! # Normalisation
//!
//! Magnitudes are normalised so a full-scale sine at bin centre reads 0 dBFS.
//! That makes the figures comparable across window sizes, which raw FFT output
//! is not: an unnormalised transform's magnitude scales with the window length.

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;
use serde::{Deserialize, Serialize};

use crate::measurement::Methodology;

/// Samples per analysis frame.
///
/// 2048 is the common figure for speech and general-purpose spectral work: at
/// 48 kHz it is 42.7 ms, long enough to resolve a low tone and short enough that
/// a change in the signal is localised in time.
pub const FRAME_SIZE: usize = 2048;

/// Fraction of each frame overlapped with the previous one, 0.0 to 1.0.
///
/// 0.5 gives a hop of 1024 samples. This is the value the analysis reports, not
/// an internal detail: two analysts using different hops will compute different
/// maxima over the same file.
pub const OVERLAP: f64 = 0.5;

/// Hop between successive frames, in samples.
#[must_use]
pub fn hop_size() -> usize {
    ((FRAME_SIZE as f64) * (1.0 - OVERLAP)) as usize
}

/// The frequency spacing of one bin, given a sample rate.
#[must_use]
pub fn frequency_resolution(sample_rate: u32) -> f64 {
    f64::from(sample_rate) / FRAME_SIZE as f64
}

/// The window function applied before the transform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Window {
    /// Hann window: the default, because a rectangular window leaks badly.
    Hann,
}

impl Window {
    /// The window's equivalent noise bandwidth, in bins.
    ///
    /// 1.5 for Hann. This is the resolution limit of the analysis: two tones
    /// closer together than this cannot be told apart by it, whatever the frame
    /// length. Reported so a reader knows what "one bin" is worth.
    #[must_use]
    pub const fn equivalent_noise_bandwidth_bins(self) -> f64 {
        match self {
            Self::Hann => 1.5,
        }
    }
}
/// One analysed frame's magnitude spectrum, in dBFS.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spectrum {
    /// Magnitude in dBFS for each bin, 0 Hz to Nyquist exclusive.
    pub bins_dbfs: Vec<f64>,
    /// Sample rate the frame was taken at, in Hz.
    pub sample_rate: u32,
    /// Samples per frame.
    pub frame_size: usize,
    /// Frequency of the first bin, in Hz.
    pub bin_width_hz: f64,
    /// Window applied before the transform.
    pub window: Window,
}

/// A summary of the frequency content of a whole signal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpectralProfile {
    /// Loudest bin across all frames, and its frequency.
    ///
    /// `None` for a signal with no energy: digital silence has no peak, and
    /// reporting 0 Hz for it would invent one.
    pub peak: Option<PeakFrequency>,
    /// Where the spectral energy sits, in Hz.
    pub centroid_hz: Option<f64>,
    /// Flatness, 0.0 to 1.0.
    ///
    /// Near 0 for a tone, near 1 for white noise. It is a *shape* measure and
    /// says nothing about level.
    pub flatness: Option<f64>,
    /// Fraction of energy below 200 Hz, 0.0 to 1.0.
    ///
    /// A crude low-frequency-energy indicator, stated as a fraction rather than a
    /// band level so it is comparable across signals.
    pub low_frequency_ratio: Option<f64>,
    /// Fraction of energy above 5 kHz.
    pub high_frequency_ratio: Option<f64>,
    /// Frames actually analysed.
    ///
    /// A signal shorter than one frame produces zero, and the ratios are then
    /// `None` rather than computed over an empty set.
    pub frames_analysed: usize,
    /// The methodology these figures were produced by.
    pub methodology: Methodology,
}

/// One frequency and the level measured at it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PeakFrequency {
    /// The frequency of the loudest bin, in Hz.
    pub hz: f64,
    /// Its level, in dBFS.
    pub dbfs: f64,
}

/// Why a spectral analysis could not be performed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpectralError {
    /// The signal is shorter than one analysis frame.
    ///
    /// A distinct error rather than an empty profile: a 100-sample file is not
    /// "silence with no peak", it is a file this method cannot measure, and
    /// conflating the two is the failure mode this crate keeps guarding against.
    #[error("signal of {samples} samples is shorter than one {required}-sample analysis frame")]
    TooShort {
        /// Samples the signal holds.
        samples: usize,
        /// Samples one frame requires.
        required: usize,
    },
    /// The sample rate is zero, so no frequency can be named.
    #[error("a sample rate of zero cannot yield a frequency")]
    ZeroSampleRate,
}
/// Computes the magnitude spectrum of one frame.
///
/// # Errors
///
/// Returns [`SpectralError::TooShort`] if `samples` is shorter than
/// [`FRAME_SIZE`], and [`SpectralError::ZeroSampleRate`] if `sample_rate` is 0.
///
/// # Panics
///
/// Never.
pub fn frame_spectrum(samples: &[f32], sample_rate: u32) -> Result<Spectrum, SpectralError> {
    if sample_rate == 0 {
        return Err(SpectralError::ZeroSampleRate);
    }
    if samples.len() < FRAME_SIZE {
        return Err(SpectralError::TooShort {
            samples: samples.len(),
            required: FRAME_SIZE,
        });
    }

    let mut buffer: Vec<Complex<f32>> = samples[..FRAME_SIZE]
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            // Hann window: `0.5 - 0.5*cos(2*pi*n/(N-1))`, using `N-1` so the
            // window reaches exactly zero at both ends. Dividing by `N` instead
            // leaves a non-zero sample at the final index, and that single
            // uncancelled sample is enough to leak the whole frame's energy into
            // the low bins.
            let n = i as f64;
            let denominator = (FRAME_SIZE - 1) as f64;
            let hann = 0.5 - 0.5 * (std::f64::consts::TAU * n / denominator).cos();
            Complex::new((s as f64 * hann) as f32, 0.0)
        })
        .collect();

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FRAME_SIZE);
    fft.process(&mut buffer);

    // Only bins 0..N/2 are kept: the rest are the negative-frequency mirror and
    // carry no independent information. Keeping them would let a caller average
    // a bin with its own conjugate and understate the level.
    let half = FRAME_SIZE / 2;
    let scale = 2.0 / FRAME_SIZE as f64;

    let bins_dbfs = buffer[..half]
        .iter()
        .enumerate()
        .map(|(index, value)| {
            // Bin 0 has no negative-frequency twin, so it is not doubled.
            let doubled = if index == 0 { 1.0 } else { 2.0 };
            dbfs(value.norm() as f64 * scale * doubled)
        })
        .collect();

    Ok(Spectrum {
        bins_dbfs,
        sample_rate,
        frame_size: FRAME_SIZE,
        bin_width_hz: frequency_resolution(sample_rate),
        window: Window::Hann,
    })
}

/// Converts a linear magnitude to dBFS, floored at a finite minimum.
///
/// The floor matters: an exact digital-silent frame yields exact zeros, and
/// Analyses the frequency content of a whole signal.
///
/// Frames every [`hop_size`] samples with [`FRAME_SIZE`]-point windows and
/// aggregates them into one profile. The peak is the loudest bin seen in *any*
/// frame, which is why the hop is reported: a transient falling between frame
/// boundaries would be missed entirely by a longer hop.
///
/// # Errors
///
/// Returns [`SpectralError::TooShort`] if the signal is shorter than one frame,
/// and [`SpectralError::ZeroSampleRate`] if `sample_rate` is 0.
///
/// # Panics
///
/// Never.
pub fn analyse(samples: &[f32], sample_rate: u32) -> Result<SpectralProfile, SpectralError> {
    if sample_rate == 0 {
        return Err(SpectralError::ZeroSampleRate);
    }
    if samples.len() < FRAME_SIZE {
        return Err(SpectralError::TooShort {
            samples: samples.len(),
            required: FRAME_SIZE,
        });
    }

    let hop = hop_size();
    let bin_width = frequency_resolution(sample_rate);
    let low_cutoff_bin = (200.0 / bin_width) as usize;
    let high_cutoff_bin = (5_000.0 / bin_width) as usize;

    let mut peak: Option<PeakFrequency> = None;
    // Accumulated in linear power, not dB: summing decibel values is not
    // meaningful, because their sum depends on the number of bins added.
    let mut energy_total = 0.0f64;
    let mut energy_low = 0.0f64;
    let mut energy_high = 0.0f64;
    let mut weighted_hz = 0.0f64;
    let mut log_sum = 0.0f64;
    // Bins holding real energy, as distinct from every bin visited. A frame of
    // silence visits 1024 bins and contains no energy in any of them.
    let mut measured_bins = 0usize;
    let mut frames = 0usize;

    let mut start = 0usize;
    while start + FRAME_SIZE <= samples.len() {
        let spectrum = frame_spectrum(&samples[start..], sample_rate)?;
        frames += 1;

        for (index, &db) in spectrum.bins_dbfs.iter().enumerate() {
            let magnitude = db_to_linear(db);
            let power = magnitude * magnitude;

            energy_total += power;
            if index <= low_cutoff_bin {
                energy_low += power;
            }
            if index >= high_cutoff_bin {
                energy_high += power;
            }
            weighted_hz += power * index as f64 * bin_width;
            if !is_floored(db) {
                log_sum += magnitude.ln();
                measured_bins += 1;
            }

            // Bin 0 is DC, excluded from the peak search: a signal carrying any
            // DC offset would otherwise report 0 Hz as its loudest frequency,
            // which is a property of the waveform rather than anything audible.
            //
            // Floored bins are excluded too: they are the absence of a
            // measurement, and a frame of pure silence would otherwise report
            // its peak as the first bin holding the floor.
            if index > 0 && !is_floored(db) && peak.is_none_or(|p| db > p.dbfs) {
                peak = Some(PeakFrequency {
                    hz: index as f64 * bin_width,
                    dbfs: db,
                });
            }
        }

        start += hop;
    }

    // Digital silence has no energy, and every derived figure would be a
    // division by zero or an arbitrary convention. `None` says the measurement
    // does not exist, which is the honest reading.
    let has_energy = energy_total > 1e-20;
    let centroid = has_energy.then(|| weighted_hz / energy_total);

    // Flatness counts only bins that carry energy. Including the floored ones
    // would put thousands of identical values into the geometric mean, pulling
    // every signal's flatness toward the same number and making a pure tone look
    // flatter than it is.
    let flatness = (has_energy && measured_bins > 0).then(|| {
        let geometric_mean = (log_sum / measured_bins as f64).exp();
        let arithmetic_mean = energy_total.sqrt() / measured_bins as f64;
        if arithmetic_mean <= 0.0 {
            0.0
        } else {
            (geometric_mean / arithmetic_mean).clamp(0.0, 1.0)
        }
    });

    Ok(SpectralProfile {
        peak,
        centroid_hz: centroid,
        flatness,
        low_frequency_ratio: has_energy.then_some((energy_low / energy_total).clamp(0.0, 1.0)),
        high_frequency_ratio: has_energy.then_some((energy_high / energy_total).clamp(0.0, 1.0)),
        frames_analysed: frames,
        methodology: Methodology::HannWindowFft,
    })
}

/// Converts a linear magnitude to dBFS, floored at a finite minimum.
///
/// The floor matters: an exact digital-silent frame yields exact zeros, and
/// `log10(0)` is `-inf`. A `-inf` in a spectrum vector propagates into any sum or
/// mean computed from it, and a report printing `-inf dBFS` looks like a defect
/// in the tool rather than a property of the signal.
fn dbfs(magnitude: f64) -> f64 {
    if magnitude <= LINEAR_FLOOR {
        FLOOR_DBFS
    } else {
        (20.0 * magnitude.log10()).max(FLOOR_DBFS)
    }
}

/// Lowest level a bin is reported at, in dBFS.
const FLOOR_DBFS: f64 = -200.0;

/// Linear magnitude corresponding to [`FLOOR_DBFS`].
const LINEAR_FLOOR: f64 = 1e-10;

/// Whether a bin carries measurable energy rather than sitting at the floor.
///
/// A binned magnitude at the floor is the absence of a measurement, not a
/// measurement of something very quiet. Converting it back to a linear
/// magnitude and summing it — the obvious way to compute a centroid — invents
/// energy from nothing: 1024 floored bins sum to enough power to clear any
/// "is this silent?" threshold, and digital silence then reports a spectral
/// centroid. This function exists so that cannot happen.
fn is_floored(db: f64) -> bool {
    db <= FLOOR_DBFS
}

/// Converts dBFS back to a linear magnitude, treating the floor as no energy.
fn db_to_linear(db: f64) -> f64 {
    if is_floored(db) {
        0.0
    } else {
        10f64.powf(db / 20.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        analyse, frame_spectrum, frequency_resolution, hop_size, SpectralError, Window, FRAME_SIZE,
    };
    use crate::measurement::Methodology;

    const RATE: u32 = 48_000;

    /// A pure sine at `hz`, `seconds` long, at `amplitude` full scale.
    fn sine(hz: f64, seconds: f64, amplitude: f32) -> Vec<f32> {
        let count = (RATE as f64 * seconds) as usize;
        (0..count)
            .map(|i| {
                (amplitude as f64 * (std::f64::consts::TAU * hz * i as f64 / RATE as f64).sin())
                    as f32
            })
            .collect()
    }

    /// Noise of `count` samples, from a deterministic generator.
    ///
    /// Deterministic rather than random so a failure is reproducible: a test that
    /// fails once and never again teaches nothing.
    fn noise(count: usize, seed: u64) -> Vec<f32> {
        let mut state = seed;
        (0..count)
            .map(|_| {
                // xorshift64*, so no dependency and no platform variance.
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let unit = (state >> 40) as f32 / (1u32 << 24) as f32;
                unit * 2.0 - 1.0
            })
            .collect()
    }

    #[test]
    fn a_pure_tone_is_located_at_its_own_frequency() {
        // 1000 Hz is not a bin centre at 48 kHz / 2048, so the peak lands on the
        // nearest bin. The tolerance is the bin width, because claiming to
        // resolve better than the transform allows would be the same
        // overstatement this crate keeps correcting elsewhere.
        let profile = analyse(&sine(1_000.0, 0.2, 0.5), RATE).expect("analyses");
        let peak = profile.peak.expect("a tone has a peak");

        let bin_hz = frequency_resolution(RATE);
        assert!(
            (peak.hz - 1_000.0).abs() <= bin_hz,
            "peak at {} Hz, expected within {bin_hz} of 1000 Hz",
            peak.hz
        );
        // A half-scale tone is 6 dB below full scale. Reading it as 0 dBFS would
        // mean the normalisation silently doubled the signal; reading it as 0
        // would mean a full-scale tone could never be reported at all.
        assert!(
            (peak.dbfs + 6.02).abs() < 1.0,
            "a half-scale tone should read near -6 dBFS, got {}",
            peak.dbfs
        );
    }

    #[test]
    fn a_full_scale_tone_reads_near_zero_dbfs() {
        // The calibration claim the normalisation exists to support.
        let profile = analyse(&sine(1_000.0, 0.2, 1.0), RATE).expect("analyses");
        let peak = profile.peak.expect("a tone has a peak");
        assert!(
            peak.dbfs.abs() < 3.0,
            "a full-scale tone should read near 0 dBFS, got {}",
            peak.dbfs
        );
    }

    #[test]
    fn a_pure_tone_is_not_flat() {
        // Flatness separates a tone from noise. If this ever read high, the
        // measure would report shape the signal does not have.
        let tone = analyse(&sine(1_000.0, 0.2, 0.5), RATE).expect("analyses");
        let noise_profile = analyse(&noise(RATE as usize, 0x1234), RATE).expect("analyses");

        let tone_flatness = tone.flatness.expect("flatness");
        let noise_flatness = noise_profile.flatness.expect("flatness");
        assert!(
            tone_flatness < 0.5,
            "a pure tone should not read as flat: {tone_flatness}"
        );
        assert!(
            noise_flatness > tone_flatness,
            "noise ({noise_flatness}) should read flatter than a tone ({tone_flatness})"
        );
    }

    #[test]
    fn a_low_tone_puts_its_energy_below_200_hz() {
        let profile = analyse(&sine(80.0, 0.2, 0.5), RATE).expect("analyses");
        let low = profile.low_frequency_ratio.expect("a tone has energy");
        assert!(low > 0.5, "an 80 Hz tone is mostly low frequency: {low}");
        let high = profile.high_frequency_ratio.expect("energy");
        assert!(high < 0.05, "an 80 Hz tone is not high frequency: {high}");
    }

    #[test]
    fn a_high_tone_puts_its_energy_above_5_khz() {
        let profile = analyse(&sine(8_000.0, 0.2, 0.5), RATE).expect("analyses");
        let high = profile.high_frequency_ratio.expect("energy");
        assert!(high > 0.5, "an 8 kHz tone is mostly high frequency: {high}");
    }

    #[test]
    fn the_centroid_sits_at_the_content() {
        let profile = analyse(&sine(1_000.0, 0.2, 0.5), RATE).expect("analyses");
        let centroid = profile.centroid_hz.expect("a tone has a centroid");
        assert!(
            (centroid - 1_000.0).abs() < 200.0,
            "centroid {centroid} should sit at the tone"
        );
    }

    #[test]
    fn digital_silence_reports_no_figures_rather_than_zeroes() {
        // The distinction this crate keeps making: "nothing there" is not "zero".
        let profile = analyse(&vec![0.0f32; RATE as usize], RATE).expect("analyses");

        assert!(profile.peak.is_none(), "silence has no peak frequency");
        assert!(profile.centroid_hz.is_none());
        assert!(profile.flatness.is_none());
        assert!(profile.low_frequency_ratio.is_none());
        assert!(profile.frames_analysed > 0, "frames were still analysed");
    }

    #[test]
    fn a_signal_shorter_than_one_frame_is_an_error_not_a_result() {
        // Not an empty profile: a file this method cannot measure is a different
        // thing from a file with no content, and reporting the second for the
        // first is the failure this project exists to prevent.
        let result = analyse(&vec![0.1f32; 100], RATE);
        assert!(matches!(result, Err(SpectralError::TooShort { .. })));
    }

    #[test]
    fn a_zero_sample_rate_is_rejected() {
        let result = analyse(&sine(1_000.0, 0.2, 0.5), 0);
        assert_eq!(result.unwrap_err(), SpectralError::ZeroSampleRate);
    }

    #[test]
    fn no_bin_is_ever_negative_infinity() {
        // -inf propagates into any sum computed from the vector, and prints as a
        // tool defect rather than a property of the signal.
        let spectrum = frame_spectrum(&vec![0.0f32; FRAME_SIZE], RATE).expect("analyses");
        let non_finite: Vec<_> = spectrum
            .bins_dbfs
            .iter()
            .filter(|db| !db.is_finite())
            .collect();
        assert!(non_finite.is_empty(), "{non_finite:?}");
    }

    #[test]
    fn a_dc_offset_is_not_reported_as_a_peak_frequency() {
        // A constant signal is entirely bin 0. Reporting 0 Hz as its loudest
        // frequency would be a property of the waveform, not anything audible.
        let dc = vec![0.5f32; RATE as usize];
        let profile = analyse(&dc, RATE).expect("analyses");
        if let Some(peak) = profile.peak {
            assert!(peak.hz > 0.0, "a DC signal must not peak at 0 Hz: {peak:?}");
        }
    }

    #[test]
    fn the_spectrum_has_half_the_bins_and_stops_below_nyquist() {
        let spectrum = frame_spectrum(&sine(1_000.0, 0.2, 0.5), RATE).expect("analyses");
        assert_eq!(spectrum.bins_dbfs.len(), FRAME_SIZE / 2);
        let highest = (spectrum.bins_dbfs.len() - 1) as f64 * spectrum.bin_width_hz;
        assert!(highest < f64::from(RATE) / 2.0);
    }

    #[test]
    fn the_reported_parameters_are_the_ones_used() {
        let spectrum = frame_spectrum(&sine(1_000.0, 0.2, 0.5), RATE).expect("analyses");
        assert_eq!(spectrum.frame_size, FRAME_SIZE);
        assert_eq!(spectrum.sample_rate, RATE);
        assert_eq!(spectrum.window, Window::Hann);
        assert!((spectrum.bin_width_hz - frequency_resolution(RATE)).abs() < 1e-9);
        assert_eq!(hop_size(), FRAME_SIZE / 2, "50% overlap as documented");
    }

    #[test]
    fn the_profile_names_its_methodology() {
        // Spec §21: a number may not be reported without the method behind it.
        let profile = analyse(&sine(1_000.0, 0.2, 0.5), RATE).expect("analyses");
        assert_eq!(profile.methodology, Methodology::HannWindowFft);
        let citation = profile.methodology.citation();
        assert!(citation.contains("Hann"), "{citation}");
        assert!(citation.contains("2048"), "{citation}");
    }

    #[test]
    fn the_window_reports_its_resolution_limit() {
        // 1.5 bins is what a Hann window resolves. A reader needs it to know what
        // one bin is worth.
        assert!((Window::Hann.equivalent_noise_bandwidth_bins() - 1.5).abs() < 1e-9);
    }

    #[test]
    fn analysis_is_deterministic() {
        // Spec §77: two runs over the same input must agree exactly.
        let signal = sine(440.0, 0.2, 0.7);
        assert_eq!(
            analyse(&signal, RATE).expect("analyses"),
            analyse(&signal, RATE).expect("analyses")
        );
        assert_eq!(
            analyse(&noise(48_000, 99), RATE).expect("analyses"),
            analyse(&noise(48_000, 99), RATE).expect("analyses")
        );
    }

    #[test]
    fn a_quiet_tone_reads_lower_than_a_loud_one() {
        let quiet = analyse(&sine(1_000.0, 0.2, 0.1), RATE).expect("analyses");
        let loud = analyse(&sine(1_000.0, 0.2, 0.8), RATE).expect("analyses");
        let difference = loud.peak.expect("loud").dbfs - quiet.peak.expect("quiet").dbfs;
        // 8x in amplitude is about 18 dB.
        assert!(
            (difference - 18.0).abs() < 1.5,
            "an 8x amplitude difference should read near 18 dB, got {difference}"
        );
    }

    #[test]
    fn arbitrary_short_signals_are_handled_without_panicking() {
        for len in 0..FRAME_SIZE {
            let signal: Vec<f32> = (0..len).map(|i| (i as f32 * 0.01).sin()).collect();
            let _ = analyse(&signal, RATE);
            let _ = frame_spectrum(&signal, RATE);
        }
    }
}
