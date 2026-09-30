//! Audio/video synchronisation analysis (spec §23).
//!
//! # Why this is implemented here rather than via `tpt-av-sync`
//!
//! `spec.txt` §23 names `tpt-av-sync` for this. The real crate is a CRDT
//! collaboration engine for multi-user timeline editing; its `playhead` module
//! measures *peer network* clocks with NTP-style round trips. Nothing in the
//! foundation measures the offset between an audio and a video track — see
//! `docs/foundation.md`.
//!
//! The offset-and-drift model is nonetheless what §23 asks for, and is
//! reimplemented here over media timestamps.
//!
//! # The measurement
//!
//! Rather than saying "audio is out of sync", the engine reports the offset at
//! the start, the offset at the end, and the drift between them:
//!
//! ```text
//! Initial offset: +42 ms
//! Final offset:   +117 ms
//! Estimated drift: +75 ms / 90 min
//! ```
//!
//! That is far more useful than a single number, because a constant offset and
//! a drifting offset have different causes: one is an edit, the other is a
//! clock-rate mismatch between the encoder and the device that captured it.
//!
//! # Exact arithmetic
//!
//! Offsets are [`MediaTime`] microseconds. Drift is a rational rate, not a
//! float, so the same input always yields the same output (spec §77).

use tpt_app_media_forensics_model::MediaTime;

use crate::error::TimingError;

/// A video track's presentation samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoSamples {
    /// Presentation times of the video track's frames, in order.
    pub timestamps: Vec<MediaTime>,
}

/// An audio track's presentation samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSamples {
    /// Presentation times of the audio track's packet or block boundaries.
    pub timestamps: Vec<MediaTime>,
}

/// The result of comparing audio and video presentation timing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// Offset of audio relative to video at the start of the timeline.
    ///
    /// Positive means audio runs later than video.
    pub initial_offset: MediaTime,
    /// Offset at the end of the compared region.
    pub final_offset: MediaTime,
    /// Difference between the final and initial offsets, over the elapsed span.
    ///
    /// This is the clock-rate mismatch, not the offset itself.
    pub drift: MediaTime,
    /// Elapsed media time the drift was measured over.
    pub measured_over: MediaTime,
    /// Number of correspondence points used.
    pub sample_count: usize,
}

impl SyncReport {
    /// Returns `true` when the offset does not change across the timeline.
    ///
    /// A constant offset points at an edit or a start-time difference. A
    /// changing offset points at a clock-rate problem. Reporting them the same
    /// way would hide the distinction that matters.
    #[must_use]
    pub fn is_constant_offset(&self) -> bool {
        self.drift.as_micros() == 0
    }

    /// Returns `true` when the drift exceeds the supplied tolerance.
    ///
    /// Compares magnitude, so a large negative drift counts as drift too.
    #[must_use]
    pub fn drift_exceeds(&self, tolerance: MediaTime) -> bool {
        let limit = tolerance.as_micros().unsigned_abs();
        self.drift.as_micros().unsigned_abs() > limit
    }
}

/// Analyses audio/video synchronisation over a set of correspondence points.
///
/// # Errors
///
/// Returns [`TimingError::InsufficientSamples`] when either stream has fewer
/// than two samples, because neither an offset nor a drift can be estimated
/// from a single point. The caller should report that rather than substitute a
/// guess.
pub fn analyse(video: &VideoSamples, audio: &AudioSamples) -> Result<SyncReport, TimingError> {
    if video.timestamps.len() < 2 || audio.timestamps.len() < 2 {
        let available = video.timestamps.len().min(audio.timestamps.len());
        return Err(TimingError::InsufficientSamples { available });
    }

    let midpoint = video.timestamps.len() / 2;
    let (head, tail) = video.timestamps.split_at(midpoint);

    let initial_offset = estimate_offset(head, &audio.timestamps);
    let final_offset = estimate_offset(tail, &audio.timestamps);

    let first = video.timestamps[0];
    let last = video.timestamps[video.timestamps.len() - 1];
    let span = last.signed_diff(first);

    Ok(SyncReport {
        initial_offset,
        final_offset,
        drift: final_offset.signed_diff(initial_offset),
        measured_over: if span.as_micros() > 0 {
            span
        } else {
            MediaTime::ZERO
        },
        sample_count: video.timestamps.len(),
    })
}

/// Estimates the offset of `audio` relative to `video` over one region.
///
/// # Method
///
/// The offset is the **median** of `nearest_audio(v) - v` across the region.
///
/// A median rather than a mean because the residuals are quantised: when the
/// true offset is not a whole number of frame intervals, the nearest audio
/// sample alternates between the one before and the one after, so individual
/// residuals alternate between roughly `-offset` and `frame - offset`. The mean
/// of that alternation is badly biased; the median is not.
fn estimate_offset(video: &[MediaTime], audio: &[MediaTime]) -> MediaTime {
    let mut residuals: Vec<i64> = Vec::with_capacity(video.len());
    let mut cursor = 0usize;

    for &v in video {
        // Advance while the current candidate is earlier than `v`; the
        // final cursor is the first audio sample at or after `v`.
        while cursor < audio.len() && audio[cursor] < v {
            cursor += 1;
        }
        let index = cursor.min(audio.len() - 1);
        residuals.push(audio[index].signed_diff(v).as_micros());
    }

    median_micros(&mut residuals)
}

/// Returns the median of `values`, sorting in place.
///
/// Operates on raw microseconds so the even-count case can average exactly.
fn median_micros(values: &mut [i64]) -> MediaTime {
    if values.is_empty() {
        return MediaTime::ZERO;
    }
    values.sort_unstable();
    let mid = values.len() / 2;
    let median = if values.len() % 2 == 0 {
        // Values are sorted and `mid - 1 <= mid`, so the sum cannot overflow.
        values[mid - 1].saturating_add(values[mid]) / 2
    } else {
        values[mid]
    };
    MediaTime::from_micros(median)
}
