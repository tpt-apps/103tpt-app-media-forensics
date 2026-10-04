//! Per-asset inspection screens: overview, streams, metadata, and the
//! analysis run itself (spec \u00a779).
//!
//! # These read what the engine measured
//!
//! An asset's container, streams and metadata are re-read on demand from the
//! source file through the same `-container` and `-metadata` entry points the
//! rules used. That is a deliberate choice over caching the inspection into the
//! case database: the acquisition record fixes the file's identity by hash
//! (spec \u00a711), so a re-inspection of a file whose hash still matches is the same
//! inspection, and caching it would mean a second copy of the truth to keep in
//! step.
//!
//! Where the engine cannot read something, the screen says so rather than
//! showing an empty table. "No metadata atoms were found" and "this container
//! could not be parsed" are different states of the same file, and a blank
//! screen conflates them.

use serde::{Deserialize, Serialize};

/// One stream as the Streams screen shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamView {
    /// Index within the container.
    pub index: usize,
    /// The stream's kind, e.g. `video`.
    pub kind: String,
    /// The codec tag exactly as the container declared it.
    ///
    /// Verbatim, never normalised: a stream tagged `avc1` in a file with no
    /// `avcC` box is itself the observation (spec \u00a726).
    pub codec: String,
    /// Long-form codec name, when recognised.
    pub codec_long_name: Option<String>,
    /// Declared language tag.
    pub language: Option<String>,
    /// Display dimensions, preferring the crop over the coded size.
    pub dimensions: Option<String>,
    /// Declared frame rate, rendered exactly as the rational it is.
    pub frame_rate: Option<String>,
    /// Declared sample rate, for audio.
    pub sample_rate: Option<u32>,
    /// Declared bit depth, for audio.
    pub bit_depth: Option<u16>,
    /// Declared channel count, or `None` when no layout was declared.
    pub channels: Option<u16>,
    /// Colour primaries, when the container recorded them.
    pub primaries: Option<String>,
    /// Colour transfer, when recorded.
    pub transfer: Option<String>,
    /// Colour matrix, when recorded.
    pub matrix: Option<String>,
    /// Whether the stream signals HDR.
    pub is_hdr: bool,
    /// Packets the container's sample index reports.
    pub packet_count: Option<u64>,
    /// Declared start time, in whole microseconds.
    pub start_micros: i64,
    /// Declared duration, in whole microseconds, when the container states one.
    pub declared_duration_micros: Option<i64>,
    /// Duration the sample tables actually add up to.
    ///
    /// Kept beside the declaration rather than reconciled with it, because
    /// their disagreement *is* the finding (spec \u00a726).
    pub measured_duration_micros: Option<i64>,
}

impl StreamView {
    /// Whether this stream's declared and measured durations disagree.
    ///
    /// Reported as a flag rather than left for the analyst to subtract two
    /// columns, and deliberately without a verdict: the rule that consumes the
    /// disagreement decides whether it is significant.
    #[must_use]
    pub fn durations_disagree(&self) -> bool {
        match (self.declared_duration_micros, self.measured_duration_micros) {
            (Some(declared), Some(measured)) => declared != measured,
            _ => false,
        }
    }
}

/// The container and stream summary for one asset (spec \u00a779).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverviewView {
    /// The asset's display name.
    pub asset_name: String,
    /// The container format actually parsed.
    pub format: String,
    /// Number of tracks the `moov` declares, counted independently of the
    /// demuxer.
    ///
    /// Carried beside `stream_count` because the two can disagree \u2014 a `trak`
    /// the demuxer skipped still sits in the file \u2014 and that disagreement is
    /// exactly what `CONTAINER.DECLARED_TRACK_MISMATCH` reports.
    pub declared_track_count: usize,
    /// Streams the demuxer recovered.
    pub stream_count: usize,
    /// The first video stream's dimensions, when there is one.
    pub dimensions: Option<String>,
    /// Declared frame rate of the first video stream.
    pub frame_rate: Option<String>,
    /// Problems found while parsing, rather than raised.
    pub anomalies: Vec<String>,
}

impl OverviewView {
    /// Whether the container declared more tracks than were recovered.
    ///
    /// A property of the file, not a verdict on it.
    #[must_use]
    pub fn has_track_mismatch(&self) -> bool {
        self.declared_track_count > self.stream_count
    }
}

/// One metadata value with its provenance (spec \u00a726, \u00a727).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataRow {
    /// Which part of the container this came from.
    pub scope: String,
    /// The key as written in the container.
    pub key: String,
    /// The value, exactly as read.
    ///
    /// Never trimmed or reformatted: the stored value is the evidence.
    pub value: String,
    /// The element it was read from, e.g. `mvhd`.
    pub source: String,
}

/// The metadata tree for one asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataView {
    /// Every entry, ordered by scope, track and key.
    pub entries: Vec<MetadataRow>,
    /// Keys whose values disagree across scopes.
    ///
    /// Surfaced rather than left for the analyst to spot, because a conflict is
    /// the whole point of the cross-check (spec \u00a726). The conflict itself is
    /// still only an observation; the rule grades it.
    pub conflicts: Vec<String>,
    /// Observed encoder indicators, each with what it does not establish.
    pub indicators: Vec<IndicatorView>,
}

/// One encoder indicator, with its stated limits (spec \u00a727).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndicatorView {
    /// The tool this points at.
    pub name: String,
    /// What was actually observed in the file.
    pub observation: String,
    /// How strongly the rule holds it.
    pub confidence: String,
    /// What it does *not* establish.
    ///
    /// Rendered beside the indicator, never below the fold. A declared `Lavf58`
    /// tag shown on its own invites a reader to treat it as proof of FFmpeg,
    /// which is exactly the inference spec \u00a727 rules out.
    pub limitations: String,
}
