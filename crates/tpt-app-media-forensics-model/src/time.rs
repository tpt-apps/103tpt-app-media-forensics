//! Exact time representation for forensic timing work.
//!
//! Media timing is *not* floating point. Frame timestamps are integers in a
//! container-defined timebase, and rounding error in PTS handling is exactly
//! the kind of artefact a forensic tool must not invent. Everything in this
//! module is integer or fixed-point rational arithmetic.
//!
//! See spec §24 (timestamp forensics) and §15 (GOP analysis).

use core::fmt;

use serde::{Deserialize, Serialize};

/// An exact rational number, used for frame rates and sample rates.
///
/// Stored as a reduced fraction so that `30000/1001` (NTSC 29.97) survives a
/// round trip without becoming `29.97` and losing the distinction between
/// 29.97 and 30.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rational {
    /// Numerator. Positive by construction.
    numerator: u64,
    /// Denominator. Never zero by construction.
    denominator: u64,
}

/// Error returned when constructing a [`Rational`] with a zero denominator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a rational denominator must be non-zero")]
pub struct ZeroDenominatorError;

impl Rational {
    /// Builds a rational, reducing it to lowest terms.
    ///
    /// # Errors
    ///
    /// Returns [`ZeroDenominatorError`] if `denominator` is zero, rather than
    /// panicking, because denominators arrive from untrusted container data.
    pub fn new(numerator: u64, denominator: u64) -> Result<Self, ZeroDenominatorError> {
        if denominator == 0 {
            return Err(ZeroDenominatorError);
        }
        let divisor = gcd(numerator, denominator);
        Ok(Self {
            numerator: numerator / divisor,
            denominator: denominator / divisor,
        })
    }

    /// Builds a rational from an integer value.
    #[must_use]
    pub const fn from_integer(value: u64) -> Self {
        Self {
            numerator: value,
            denominator: 1,
        }
    }

    /// Returns the reduced numerator.
    #[must_use]
    pub const fn numerator(self) -> u64 {
        self.numerator
    }

    /// Returns the reduced denominator.
    #[must_use]
    pub const fn denominator(self) -> u64 {
        self.denominator
    }

    /// Returns the value as `f64`, for display and non-critical comparison.
    ///
    /// Never use this for timestamp arithmetic — use [`MediaTime`].
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn to_f64(self) -> f64 {
        self.numerator as f64 / self.denominator as f64
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.denominator == 1 {
            write!(f, "{}", self.numerator)
        } else {
            write!(f, "{}/{}", self.numerator, self.denominator)
        }
    }
}

/// Greatest common divisor, used to reduce rationals to lowest terms.
const fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    // Guard against gcd(0, 0), which would make `Rational::new` divide by zero.
    if a == 0 {
        1
    } else {
        a
    }
}

/// A container's timestamp timebase: ticks per second.
///
/// For example an MP4 video track commonly uses a timebase of `1/30000` with
/// sample durations of `1001`, which is how 29.97 fps is expressed exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Timebase {
    /// Numerator of the timebase.
    numerator: u32,
    /// Denominator of the timebase: ticks per second.
    ticks_per_second: u32,
}

impl Timebase {
    /// Builds a timebase of `1 / ticks_per_second`, the common case.
    #[must_use]
    pub const fn from_ticks_per_second(ticks_per_second: u32) -> Self {
        Self {
            numerator: 1,
            ticks_per_second,
        }
    }

    /// Builds a timebase from an explicit numerator and denominator.
    ///
    /// Returns [`None`] if either component is zero, since a zero timebase
    /// cannot convert timestamps to seconds.
    #[must_use]
    pub const fn new(numerator: u32, denominator: u32) -> Option<Self> {
        if numerator == 0 || denominator == 0 {
            return None;
        }
        Some(Self {
            numerator,
            ticks_per_second: denominator,
        })
    }

    /// Returns the number of ticks per second in this timebase.
    #[must_use]
    pub const fn ticks_per_second(self) -> u32 {
        self.ticks_per_second
    }

    /// Converts a signed tick count to a [`MediaTime`].
    ///
    /// Timestamps may legitimately be negative before an edit list is applied
    /// (spec §24, "negative timestamps"), so the input is signed.
    #[must_use]
    pub fn ticks_to_media_time(self, ticks: i64) -> MediaTime {
        // Work in i128 to keep the multiply exact; a 32-bit timebase and a
        // 64-bit tick count overflows i64 on adversarial input.
        let micros = (i128::from(ticks) * 1_000_000) / i128::from(self.ticks_per_second);
        MediaTime::from_micros(clamp_to_i64(micros))
    }

    /// Converts a [`MediaTime`] back to an integer tick count.
    #[must_use]
    pub fn media_time_to_ticks(self, time: MediaTime) -> i64 {
        let ticks = (i128::from(time.as_micros()) * i128::from(self.ticks_per_second)) / 1_000_000;
        clamp_to_i64(ticks)
    }
}

impl fmt::Display for Timebase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.numerator, self.ticks_per_second)
    }
}

/// A media presentation time, in whole microseconds from the stream start.
///
/// Microseconds are the common denominator the engine uses to align audio and
/// video for A/V sync measurement (spec §23). Sub-microsecond precision is
/// unnecessary for that purpose and would invite float drift.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct MediaTime {
    micros: i64,
}

impl MediaTime {
    /// The zero point of a timeline.
    pub const ZERO: Self = Self { micros: 0 };

    /// Builds a time from whole microseconds.
    #[must_use]
    pub const fn from_micros(micros: i64) -> Self {
        Self { micros }
    }

    /// Builds a time from whole milliseconds.
    #[must_use]
    pub const fn from_millis(millis: i64) -> Self {
        Self {
            micros: millis.saturating_mul(1_000),
        }
    }

    /// Returns the time in whole microseconds.
    #[must_use]
    pub const fn as_micros(self) -> i64 {
        self.micros
    }

    /// Returns the time in whole milliseconds, truncating toward zero.
    #[must_use]
    pub const fn as_millis(self) -> i64 {
        self.micros / 1_000
    }

    /// Returns the time in whole seconds, truncating toward zero.
    #[must_use]
    pub const fn as_secs(self) -> i64 {
        self.micros / 1_000_000
    }

    /// Returns the signed distance from `self` to `other`, saturating on overflow.
    ///
    /// Saturating rather than wrapping: a wrapped difference would report a
    /// tiny offset where the truth is "enormous", which is precisely the kind
    /// of false conclusion a forensic report must not make.
    #[must_use]
    pub fn signed_diff(self, other: Self) -> Self {
        Self {
            micros: self.micros.saturating_sub(other.micros),
        }
    }

    /// Returns `self` shifted forward by `delta`, saturating on overflow.
    #[must_use]
    pub fn saturating_add(self, delta: Self) -> Self {
        Self {
            micros: self.micros.saturating_add(delta.micros),
        }
    }

    /// Returns `self` shifted backward by `delta`, saturating on overflow.
    #[must_use]
    pub fn saturating_sub(self, delta: Self) -> Self {
        Self {
            micros: self.micros.saturating_sub(delta.micros),
        }
    }

    /// Formats as `HH:MM:SS.mmm`, the timecode shown throughout the UI and reports.
    ///
    /// Negative values are rendered with a leading `-` so that pre-roll
    /// timestamps remain visible rather than being silently clamped.
    #[must_use]
    pub fn to_timecode(self) -> String {
        let sign = if self.micros < 0 { "-" } else { "" };
        let total = self.micros.unsigned_abs();
        let millis = (total % 1_000_000) / 1_000;
        let total_secs = total / 1_000_000;
        let seconds = total_secs % 60;
        let total_mins = total_secs / 60;
        let minutes = total_mins % 60;
        let hours = total_mins / 60;
        format!("{sign}{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
    }
}

impl fmt::Display for MediaTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_timecode())
    }
}

/// Saturating cast helper: malformed media must never panic the engine
/// (spec §75, "no malformed media should be able to crash the application").
///
/// Not a `const fn`: `i128::from` is not yet const-callable, and this only
/// needs to be fast, not compile-time evaluable.
fn clamp_to_i64(value: i128) -> i64 {
    if value > i128::from(i64::MAX) {
        i64::MAX
    } else if value < i128::from(i64::MIN) {
        i64::MIN
    } else {
        value as i64
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rational_is_reduced() {
        let r = Rational::new(60, 120).unwrap();
        assert_eq!((r.numerator(), r.denominator()), (1, 2));
    }

    #[test]
    fn rational_preserves_ntsc_rate_exactly() {
        // 30000/1001 must not collapse to 30000/1000.
        let r = Rational::new(30_000, 1_001).unwrap();
        assert_eq!((r.numerator(), r.denominator()), (30_000, 1_001));
        assert!((r.to_f64() - 29.97).abs() < 0.001);
    }

    #[test]
    fn rational_rejects_zero_denominator() {
        // Must not panic: container data is untrusted (spec §75).
        assert!(Rational::new(1, 0).is_err());
    }

    #[test]
    fn rational_zero_value_is_valid() {
        let z = Rational::new(0, 100).unwrap();
        assert_eq!((z.numerator(), z.denominator()), (0, 1));
        assert_eq!(z.to_f64(), 0.0);
    }

    #[test]
    fn timebase_round_trips_ticks() {
        let tb = Timebase::from_ticks_per_second(1_000);
        let time = tb.ticks_to_media_time(1_500);
        assert_eq!(time.as_millis(), 1_500);
        assert_eq!(tb.media_time_to_ticks(time), 1_500);
    }

    #[test]
    fn timebase_rejects_zero_components() {
        assert!(Timebase::new(0, 25).is_none());
        assert!(Timebase::new(25, 0).is_none());
    }

    #[test]
    fn timebase_handles_negative_ticks() {
        // Negative pre-roll timestamps are legitimate input (spec §24).
        let tb = Timebase::from_ticks_per_second(1_000);
        assert_eq!(tb.ticks_to_media_time(-2_500).as_millis(), -2_500);
    }

    #[test]
    fn timebase_conversion_saturates_instead_of_overflowing() {
        let tb = Timebase::from_ticks_per_second(1_000);
        assert_eq!(
            tb.ticks_to_media_time(i64::MAX).as_micros(),
            i64::MAX,
            "must saturate, not wrap"
        );
    }

    #[test]
    fn timebase_ntsc_frame_duration_is_exact() {
        // 29.97 fps as a 1/30000 timebase with 1001-tick frames.
        let tb = Timebase::from_ticks_per_second(30_000);
        assert_eq!(tb.ticks_to_media_time(1_001).as_micros(), 33_366);
    }

    #[test]
    fn signed_diff_reports_direction() {
        let a = MediaTime::from_micros(117_000);
        let b = MediaTime::from_micros(42_000);
        assert_eq!(a.signed_diff(b).as_millis(), 75);
        assert_eq!(b.signed_diff(a).as_millis(), -75);
    }

    #[test]
    fn signed_diff_saturates_on_overflow() {
        let a = MediaTime::from_micros(i64::MAX);
        let b = MediaTime::from_micros(i64::MIN);
        assert_eq!(a.signed_diff(b).as_micros(), i64::MAX);
    }

    #[test]
    fn timecode_formats_and_keeps_sign() {
        assert_eq!(
            MediaTime::from_millis(3_723_456).to_timecode(),
            "01:02:03.456"
        );
        assert_eq!(
            MediaTime::from_millis(-1_500).to_timecode(),
            "-00:00:01.500"
        );
    }

    #[test]
    fn timecode_handles_long_runtimes() {
        // 100+ hours must widen the hours field rather than wrap into minutes.
        assert!(MediaTime::from_micros(360_000_000_000)
            .to_timecode()
            .starts_with("100:"));
    }
}
