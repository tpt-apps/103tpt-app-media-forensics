//! The unified error and anomaly timeline (spec §31).
//!
//! # What a timeline is for
//!
//! Spec §31 asks for anomalies on one timeline, so a reviewer can see *where* in
//! a file a problem sits and jump to it. That requires merging observations that
//! arrive from three unrelated places — structural damage found by a byte scan,
//! timestamp anomalies found in a decode-order sequence, and per-frame findings
//! from the rule set — into one ordered list.
//!
//! # Why the entries record where they came from
//!
//! The three sources disagree about confidence in *placement*, and collapsing
//! them would misrepresent all three:
//!
//! - A timestamp anomaly sits at a sample index, which is exact.
//! - Structural damage sits at a byte offset, which becomes a media time only by
//!   inference from the sample table.
//! - A finding may carry no time at all, and must then be absent from an ordered
//!   timeline rather than parked at `00:00:00`.
//!
//! So [`TimelineEntry`] keeps the placement *and* how the placement was reached.
//! An entry whose origin is inferred must not be mistaken for one whose origin is
//! measured, and a timecode a reader can check is worth more than one that merely
//! looks precise.
//!
//! # Ordering is total, and deterministic
//!
//! Entries sort by time, then by source, then by reference. A gap and a
//! structural defect at the same instant need a stable order, or two runs over
//! one file would produce different reports — which spec §77 forbids. Placement is
//! carried on each entry as data but is deliberately not part of the sort order.

use serde::{Deserialize, Serialize};

use crate::time::MediaTime;

/// How an entry's position on the timeline was arrived at.
///
/// Carried on every entry because the weakest link in the chain decides how much
/// a timecode can be trusted. An entry whose time was measured is different from
/// one inferred from neighbouring samples, even when both read `00:00:10`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// The position was read directly from the file: a sample's own timestamp.
    Measured,
    /// Derived by accumulating sample sizes from an anchor to reach a byte
    /// offset, then mapping that to the last sample before it.
    ///
    /// Sound for a single contiguous `mdat`. Not sound across interleaved
    /// streams, where the running total includes another track's bytes.
    Inferred,
    /// The observation has no position in the file, only an order relative to
    /// other observations.
    ///
    /// A defect in the file header has no media time. Parking it at zero would be
    /// a fabricated timecode.
    Unplaced,
}

/// Which analysis a timeline entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineSource {
    /// Structural damage from the container byte scan (spec §30).
    StructuralDamage,
    /// A timestamp anomaly from the PTS/DTS scan (spec §24).
    Timestamp,
    /// A rule finding carrying a timeline position (spec §34).
    Finding,
}

/// One event on the error and anomaly timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineEntry {
    /// When the event occurs, when it has a position.
    pub time: Option<MediaTime>,
    /// How `time` was arrived at.
    pub placement: Placement,
    /// Which analysis produced this entry.
    pub source: TimelineSource,
    /// Rule ID, finding ID, or defect tag identifying the event.
    pub reference: String,
    /// Human-readable one-line description.
    pub summary: String,
}

impl TimelineEntry {
    /// Builds an entry whose position was read directly from the file.
    #[must_use]
    pub fn measured(
        time: MediaTime,
        source: TimelineSource,
        reference: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            time: Some(time),
            placement: Placement::Measured,
            source,
            reference: reference.into(),
            summary: summary.into(),
        }
    }

    /// Builds an entry whose position was inferred rather than read.
    #[must_use]
    pub fn inferred(
        time: MediaTime,
        source: TimelineSource,
        reference: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            time: Some(time),
            placement: Placement::Inferred,
            source,
            reference: reference.into(),
            summary: summary.into(),
        }
    }

    /// Builds an entry that has no media position.
    ///
    /// Used for observations about the file as a whole — a declared-versus-
    /// measured duration mismatch, a missing duration — which are real but are not
    /// events *at* a moment. Giving them a time would invent one.
    pub fn unplaced(
        source: TimelineSource,
        reference: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            time: None,
            placement: Placement::Unplaced,
            source,
            reference: reference.into(),
            summary: summary.into(),
        }
    }

    /// Whether this entry sits at a real position.
    #[must_use]
    pub fn is_placed(&self) -> bool {
        self.time.is_some()
    }

    /// The total order key: time first, then a deterministic tiebreak.
    ///
    /// Unplaced entries sort last rather than first. They have no position, so
    /// putting them at the head would suggest they happen before everything else,
    /// which is a claim the observation cannot support.
    ///
    /// `placement` is deliberately *not* part of this key. Its discriminant orders
    /// `Measured` before `Inferred` before `Unplaced`, which at an equal time would
    /// put a weaker placement claim ahead of an exactly-measured one — for no reason
    /// a reader could infer. Placement is data carried on the entry, not a ranking
    /// of it, and the first version of this key did rank it. The tests caught the
    /// inversion: an inferred entry at 100 ms displaced a measured one to second
    /// place.
    fn sort_key(&self) -> (i128, u8, &str, &str) {
        (
            self.time.map_or(i128::MAX, |t| i128::from(t.as_micros())),
            self.source as u8,
            &self.reference,
            &self.summary,
        )
    }
}

/// Every located observation about one asset, in one order (spec §31).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Timeline {
    /// Entries in timeline order, unplaced ones last.
    pub entries: Vec<TimelineEntry>,
    /// Total run time, when known, so a renderer can draw the axis.
    pub duration: Option<MediaTime>,
}

impl Timeline {
    /// Builds a timeline, sorting entries into their canonical order.
    #[must_use]
    pub fn new(mut entries: Vec<TimelineEntry>, duration: Option<MediaTime>) -> Self {
        entries.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
        Self { entries, duration }
    }

    /// Returns true when nothing was observed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Entries that sit at a real position, in order.
    ///
    /// Unplaced entries are excluded. A renderer drawing a scrub bar cannot place
    /// them, and offering them would invite drawing one at zero.
    pub fn placed(&self) -> impl Iterator<Item = &TimelineEntry> {
        self.entries.iter().filter(|entry| entry.is_placed())
    }

    /// Entries with no media position.
    pub fn unplaced(&self) -> impl Iterator<Item = &TimelineEntry> {
        self.entries.iter().filter(|entry| !entry.is_placed())
    }

    /// Entries from one source.
    pub fn from_source(&self, source: TimelineSource) -> impl Iterator<Item = &TimelineEntry> {
        self.entries
            .iter()
            .filter(move |entry| entry.source == source)
    }

    /// Counts entries whose position is exactly known.
    ///
    /// Distinct from [`Timeline::len`]: an entry can be real while its position is
    /// inferred, and a report saying "12 events" without saying how many are
    /// pinned would overstate what the analysis established.
    #[must_use]
    pub fn measured_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.placement == Placement::Measured)
            .count()
    }

    /// Renders the timeline as report lines.
    #[must_use]
    pub fn describe(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| {
                let position = match entry.time {
                    Some(time) => time.to_timecode(),
                    None => "(no media position)".to_owned(),
                };
                // The placement travels with the timecode. Printing `00:00:10`
                // with no indication of whether that was read or inferred is how
                // an estimate ends up quoted as a measurement.
                let qualifier = match entry.placement {
                    Placement::Measured => "",
                    Placement::Inferred => " inferred",
                    Placement::Unplaced => "",
                };
                format!(
                    "{position}{qualifier}  [{:?}] {}  {}",
                    entry.source, entry.reference, entry.summary
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{Placement, Timeline, TimelineEntry, TimelineSource};
    use crate::time::MediaTime;

    fn at(millis: i64) -> MediaTime {
        MediaTime::from_millis(millis)
    }

    #[test]
    fn entries_sort_by_time_regardless_of_input_order() {
        let entries = vec![
            TimelineEntry::measured(
                at(3000),
                TimelineSource::Timestamp,
                "TIMING.TIMESTAMP_GAP",
                "gap",
            ),
            TimelineEntry::inferred(
                at(1000),
                TimelineSource::StructuralDamage,
                "CONTAINER.TRUNCATED_MEDIA",
                "truncated",
            ),
            TimelineEntry::measured(at(2000), TimelineSource::Finding, "VIDEO.X", "x"),
        ];

        let timeline = Timeline::new(entries, Some(at(10_000)));
        let times: Vec<_> = timeline.placed().map(|e| e.time.unwrap()).collect();
        assert_eq!(times, vec![at(1000), at(2000), at(3000)]);
    }

    #[test]
    fn unplaced_entries_sort_last_not_first() {
        // Parking a header defect at the head of a timeline would claim it happens
        // before everything else, which the observation does not support.
        let timeline = Timeline::new(
            vec![
                TimelineEntry::unplaced(TimelineSource::Finding, "METADATA.X", "file-level"),
                TimelineEntry::measured(at(500), TimelineSource::Timestamp, "T", "gap"),
            ],
            None,
        );

        // Look the entries up rather than indexing: sorting has legitimately reordered
        // them, so position 0 is not "what was passed in first". Asserting on the
        // index would test the input order, not the sort.
        assert!(
            timeline.entries.last().expect("an entry").time.is_none(),
            "the unplaced entry must sort after the placed one: {:?}",
            timeline.entries
        );
        assert_eq!(timeline.len(), 2);
        assert_eq!(timeline.placed().count(), 1);
        assert_eq!(timeline.unplaced().count(), 1);
    }

    #[test]
    fn entries_at_the_same_instant_have_a_stable_order() {
        // spec §77: two runs over one file must produce identical output, so the
        // order cannot depend on which source was appended first.
        let build = || {
            vec![
                TimelineEntry::measured(at(1000), TimelineSource::Timestamp, "T", "gap"),
                TimelineEntry::inferred(
                    at(1000),
                    TimelineSource::StructuralDamage,
                    "CONTAINER.TRUNCATED_MEDIA",
                    "truncated",
                ),
                TimelineEntry::measured(at(1000), TimelineSource::Finding, "VIDEO.X", "x"),
            ]
        };

        let first = Timeline::new(build(), None);
        let second = Timeline::new(build(), None);
        assert_eq!(first, second, "ordering must be deterministic");
        assert_eq!(first.entries.len(), 3);
    }

    #[test]
    fn ordering_does_not_depend_on_input_order() {
        let entries = vec![
            TimelineEntry::measured(at(2000), TimelineSource::Finding, "B", "b"),
            TimelineEntry::measured(at(1000), TimelineSource::Timestamp, "A", "a"),
        ];
        let forwards = Timeline::new(entries.clone(), None);
        let mut reversed = entries;
        reversed.reverse();
        let backwards = Timeline::new(reversed, None);
        assert_eq!(forwards, backwards);
    }

    #[test]
    fn the_placement_is_reported_alongside_the_timecode() {
        // An inferred time printed bare is how an estimate gets quoted as a
        // measurement, so the qualifier must appear in the rendered line.
        let timeline = Timeline::new(
            vec![
                TimelineEntry::inferred(
                    at(280),
                    TimelineSource::StructuralDamage,
                    "CONTAINER.TRUNCATED_MEDIA",
                    "truncated",
                ),
                TimelineEntry::measured(at(100), TimelineSource::Timestamp, "T", "gap"),
            ],
            None,
        );

        // The measured 100 ms entry sorts first; the inferred 280 ms entry second.
        // Matched by content rather than index, because the assertion is about
        // what the renderer says, not about where sorting happened to put it.
        let lines = timeline.describe();
        let inferred = lines
            .iter()
            .find(|line| line.contains("truncated"))
            .expect("the inferred entry is present");
        assert!(
            inferred.contains("inferred"),
            "an inferred entry must say so: {inferred:?}"
        );

        let measured = lines
            .iter()
            .find(|line| line.contains("gap"))
            .expect("the measured entry is present");
        assert!(
            !measured.contains("inferred"),
            "a measured entry must not be marked inferred: {measured:?}"
        );
    }

    #[test]
    fn measured_count_distinguishes_placed_from_exact() {
        // An entry can be present and real while its position is inferred.
        let timeline = Timeline::new(
            vec![
                TimelineEntry::measured(at(100), TimelineSource::Timestamp, "A", "a"),
                TimelineEntry::inferred(at(200), TimelineSource::StructuralDamage, "B", "b"),
                TimelineEntry::unplaced(TimelineSource::Finding, "C", "c"),
            ],
            None,
        );

        assert_eq!(timeline.len(), 3);
        assert_eq!(timeline.measured_count(), 1, "only one position is exact");
    }

    #[test]
    fn an_empty_timeline_is_empty_rather_than_absent() {
        let timeline = Timeline::default();
        assert!(timeline.is_empty());
        assert_eq!(timeline.len(), 0);
        assert!(timeline.describe().is_empty());
        assert_eq!(timeline.measured_count(), 0);
        assert_eq!(timeline.duration, None);
    }

    #[test]
    fn entries_can_be_filtered_by_source() {
        let timeline = Timeline::new(
            vec![
                TimelineEntry::measured(at(100), TimelineSource::Timestamp, "A", "a"),
                TimelineEntry::inferred(at(200), TimelineSource::StructuralDamage, "B", "b"),
                TimelineEntry::measured(at(300), TimelineSource::Timestamp, "C", "c"),
            ],
            None,
        );

        assert_eq!(timeline.from_source(TimelineSource::Timestamp).count(), 2);
        assert_eq!(
            timeline
                .from_source(TimelineSource::StructuralDamage)
                .count(),
            1
        );
        assert_eq!(timeline.from_source(TimelineSource::Finding).count(), 0);
    }

    #[test]
    fn an_entry_with_no_time_never_reports_one() {
        let entry = TimelineEntry::unplaced(TimelineSource::Finding, "X", "file-level");
        assert!(entry.time.is_none());
        assert_eq!(entry.placement, Placement::Unplaced);
        assert!(!entry.is_placed());
    }

    #[test]
    fn the_timeline_carries_the_run_duration_for_rendering() {
        let timeline = Timeline::new(Vec::new(), Some(at(90_000)));
        assert_eq!(timeline.duration, Some(at(90_000)));
    }
}
