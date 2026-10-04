//! The timeline as central navigation (spec §42, §81).
//!
//! # The timeline is how an analyst moves, not how a file is drawn
//!
//! Spec §81 makes the timeline the primary navigation mechanism: selecting
//! Finding #17 should jump to the relevant timestamp, frame, audio region,
//! stream, and evidence. That is a stronger requirement than "render a strip",
//! and it is the reason this module produces a [`JumpTarget`] rather than a
//! picture of one. The frontend's job is to draw layers and call
//! [`TimelineView::target_for`]; which view a jump lands on, and to what, is
//! decided here where it can be tested.
//!
//! # Five layers, and an unplaced entry belongs to none of them
//!
//! ```text
//! VIDEO
//! â–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆ
//! AUDIO
//! â–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆâ–ˆ
//! SCENE CHANGES
//!    â”‚    â”‚        â”‚
//! ERRORS
//!         â–²            â–²
//! FINDINGS
//!               â—
//! ```
//!
//! Every layer is positioned by media time. Spec §31 established that some
//! observations have *no* media position — a header defect, a declared-versus-
//! measured duration mismatch — and that parking them at `00:00:00` would be a
//! fabricated timecode. A drawn strip has the same temptation: put the marker at
//! the left edge and it looks like a real observation at the start of the file.
//!
//! So [`TimelineLayer::events`] holds only placed entries, and everything
//! without a position is listed separately in [`TimelineView::unplaced`] with a
//! count. An analyst can still find them; they cannot mistake them for
//! something that happened at a moment.
//!
//! # Placement travels with the marker
//!
//! A measured time and an inferred one can both read `00:00:10`. Spec §31
//! insists the distinction is carried, so [`TimelineMark`] reports
//! [`super::TimelinePlacement`] on every marker and the renderer draws inferred
//! positions differently. A timeline that quietly upgrades an inference to a
//! measurement is how an estimate ends up quoted as fact.

use serde::{Deserialize, Serialize};

use tpt_app_media_forensics_model::{
    EvidenceId, Finding, FindingId, MediaTime, Severity, TimelineEntry, TimelineSource,
};

use super::{Retention, TimelinePlacement};

/// Which layer an event is drawn on (spec §42).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TimelineLayer {
    /// Video frame positions.
    Video,
    /// Audio regions: silence, level excursions, loudness.
    Audio,
    /// Scene-change positions.
    Scene,
    /// Errors and structural damage.
    Error,
    /// Findings raised by the rule set.
    Finding,
}

impl TimelineLayer {
    /// Every layer, in the order spec §42 lists them.
    ///
    /// One ordered list so the strip's row order cannot vary between renders.
    pub const ALL: [Self; 5] = [
        Self::Video,
        Self::Audio,
        Self::Scene,
        Self::Error,
        Self::Finding,
    ];

    /// Returns the row label drawn beside the layer.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Video => "VIDEO",
            Self::Audio => "AUDIO",
            Self::Scene => "SCENE CHANGES",
            Self::Error => "ERRORS",
            Self::Finding => "FINDINGS",
        }
    }
}

/// One positioned marker on a layer.
///
/// `time` is always present. That is the whole point of this type: an entry
/// with no media position is not a marker, and the model that holds markers
/// therefore cannot hold one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineMark {
    /// When the event occurs.
    pub time: MediaTime,
    /// End of a spanning region, when the observation covers one.
    pub end: Option<MediaTime>,
    /// How the position was arrived at.
    ///
    /// A renderer draws `Inferred` differently from `Measured`. Collapsing them
    /// would let a position derived from a byte offset be quoted as a
    /// timestamp.
    pub placement: super::TimelinePlacement,
    /// What the event is, in one line.
    pub summary: String,
    /// Which analysis produced it.
    pub source: TimelineSource,
    /// The finding this marker belongs to, when it came from the rule set.
    pub finding_id: Option<FindingId>,
    /// Severity, for colouring a finding marker.
    pub severity: Option<Severity>,
    /// Evidence artefacts anchored at this position.
    pub evidence: Vec<EvidenceId>,
}

impl TimelineMark {
    /// Builds a mark from a positioned timeline entry.
    #[must_use]
    pub fn from_entry(entry: &TimelineEntry) -> Self {
        Self {
            time: entry.time.unwrap_or(MediaTime::ZERO),
            end: None,
            placement: entry.placement,
            summary: entry.summary.clone(),
            source: entry.source,
            finding_id: None,
            severity: None,
            evidence: Vec::new(),
        }
    }

    /// Whether this mark covers a span rather than an instant.
    #[must_use]
    pub fn is_span(&self) -> bool {
        self.end.is_some_and(|end| end > self.time)
    }
}

/// One row of the timeline strip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerRow {
    /// Which row this is.
    pub layer: TimelineLayer,
    /// Marks on the row, ordered by time.
    ///
    /// Only positioned events. See the module documentation.
    pub events: Vec<TimelineMark>,
}

impl LayerRow {
    /// Number of markers on this row.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the row has no markers.
    ///
    /// Distinct from "the row was not examined": the caller decides that by
    /// whether the row exists at all, which is why [`TimelineView::layers`]
    /// omits rows nothing was observed on rather than filling them with
    /// placeholder empties.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// Which screen a jump lands on (spec §81).
///
/// Spec §81 lists five things a selection should reach: timestamp, frame,
/// audio region, stream, evidence. This is the screen half of that list; the
/// rest travel on [`JumpTarget`] itself. The frontend switches on this rather
/// than guessing from which panel was clicked, so a marker drawn on the error
/// row opens the same destination as the equivalent finding in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum JumpScreen {
    /// The video viewer, positioned at a frame.
    Video,
    /// The audio view, positioned at a region.
    Audio,
    /// The findings list, scrolled to the finding.
    Findings,
    /// The evidence list, scrolled to the artefacts.
    Evidence,
    /// The streams screen, scrolled to the stream.
    Streams,
    /// Nowhere: the observation has no media position to jump to.
    ///
    /// Not an error. Spec §31 established that some observations genuinely have
    /// no time, and the honest response to clicking one is to say so and stay
    /// put — not to jump to `00:00:00` and imply the event happened there.
    Nowhere,
}

impl JumpScreen {
    /// Returns the stable tag used in JSON output.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Video => "VIDEO",
            Self::Audio => "AUDIO",
            Self::Findings => "FINDINGS",
            Self::Evidence => "EVIDENCE",
            Self::Streams => "STREAMS",
            Self::Nowhere => "NOWHERE",
        }
    }
}

/// Everything a click on the timeline should reach (spec §81).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JumpTarget {
    /// Where to navigate.
    pub screen: JumpScreen,
    /// The media time, when the observation has one.
    ///
    /// `None` together with `screen: Nowhere`. Present whenever there is
    /// anything to navigate to.
    pub time: Option<MediaTime>,
    /// The decoded frame nearest this position, when frames were decoded.
    pub frame_index: Option<usize>,
    /// The stream this belongs to, when it is stream-scoped.
    pub stream_id: Option<tpt_app_media_forensics_model::StreamId>,
    /// The finding selected, when the click came from the findings row.
    pub finding_id: Option<FindingId>,
    /// Evidence artefacts anchored here.
    pub evidence: Vec<EvidenceId>,
    /// A sentence explaining the destination, shown after the jump.
    ///
    /// Present so the analyst can see *where* they landed and *why*, rather
    /// than watching a panel change with no account of it. Says "no media
    /// position" when there is none.
    pub explanation: String,
}

impl JumpTarget {
    /// The destination for an observation that has no media position.
    ///
    /// Its own constructor rather than a general fallback so that every call
    /// site has to decide, visibly, what to do about an unplaced event. The
    /// string is the point: spec §31's refusal to invent a timecode has to
    /// survive all the way to what the analyst reads.
    #[must_use]
    pub fn unplaced(summary: impl AsRef<str>) -> Self {
        Self {
            screen: JumpScreen::Nowhere,
            time: None,
            frame_index: None,
            stream_id: None,
            finding_id: None,
            evidence: Vec::new(),
            explanation: format!(
                "{} has no media position, so there is nothing to jump to. It is \
                 recorded as an observation about the file as a whole.",
                summary.as_ref()
            ),
        }
    }
}

/// The timeline strip for one asset (spec §42, §81).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineView {
    /// Rows carrying at least one marker, in [`TimelineLayer::ALL`] order.
    pub layers: Vec<LayerRow>,
    /// Observations with no media position.
    ///
    /// Listed rather than drawn, because they cannot be drawn honestly.
    pub unplaced: Vec<TimelineEntry>,
    /// Total run duration, when known, so the frontend can scale the axis.
    pub duration: Option<MediaTime>,
    /// Markers whose position was read from the file rather than inferred.
    ///
    /// Distinct from the total marker count. A strip reporting "12 events"
    /// without saying how many are pinned overstates what the analysis
    /// established, and the distinction is one the model already knows.
    pub measured_marks: usize,
    /// What the strip's completeness is, in words, when it is not complete.
    ///
    /// `None` when every run in the case retained its timeline, which is the
    /// only case where "nothing is shown here" means "nothing was found".
    ///
    /// This is what lets the screen say "this case predates timeline retention"
    /// rather than drawing an empty strip that reads as "this run found
    /// nothing". An empty strip and an incomplete one are different findings,
    /// and in a forensic tool only one of them is evidence of absence.
    pub retention_notice: Option<String>,
}

impl TimelineView {
    /// Builds the strip from the engine's unified timeline and the case's
    /// findings.
    ///
    /// Findings are layered separately from the timeline entries rather than
    /// being read out of them, because the engine's timeline merges positioned
    /// findings into one list and the strip needs them on their own row,
    /// coloured by severity and carrying their evidence references.
    ///
    /// `retention` is carried rather than derived here: whether a run recorded
    /// its strip is a fact about the database, and this layer does not get to
    /// infer it (spec §78).
    #[must_use]
    pub fn build(
        timeline: &tpt_app_media_forensics_model::Timeline,
        findings: &[Finding],
        retention: Retention,
    ) -> Self {
        let mut by_layer: Vec<(TimelineLayer, Vec<TimelineMark>)> = TimelineLayer::ALL
            .iter()
            .map(|layer| (*layer, Vec::new()))
            .collect();

        // Placed engine observations. Unplaced ones are collected separately
        // below, never defaulted to time zero.
        let mut unplaced = Vec::new();
        for entry in &timeline.entries {
            let Some(time) = entry.time else {
                unplaced.push(entry.clone());
                continue;
            };
            let mut mark = TimelineMark::from_entry(entry);
            mark.time = time;
            if let Some(row) = by_layer
                .iter_mut()
                .find(|(l, _)| *l == layer_for(entry.source))
            {
                row.1.push(mark);
            }
        }

        // Findings, placed or not.
        for finding in findings {
            let Some(start) = finding.timeline_start else {
                // A finding with no position is still an observation the
                // analyst must be able to find. It joins the unplaced list with
                // the same wording spec §31 uses.
                unplaced.push(TimelineEntry::unplaced(
                    TimelineSource::Finding,
                    finding.rule_id.clone(),
                    finding.observation.summary.clone(),
                ));
                continue;
            };
            let mark = TimelineMark {
                time: start,
                end: finding.timeline_end,
                placement: TimelinePlacement::Measured,
                summary: finding.observation.summary.clone(),
                source: TimelineSource::Finding,
                finding_id: Some(finding.id),
                severity: Some(finding.severity),
                evidence: finding.evidence.clone(),
            };
            if let Some(row) = by_layer
                .iter_mut()
                .find(|(l, _)| *l == TimelineLayer::Finding)
            {
                row.1.push(mark);
            }
        }

        // Each row sorted by time so the frontend can lay markers out without
        // re-sorting, and so two renders of one case agree on order (spec §77).
        for (_, marks) in &mut by_layer {
            marks.sort_by_key(|m| (m.time.as_micros(), m.source as u8, m.summary.clone()));
        }

        let measured_marks = by_layer
            .iter()
            .flat_map(|(_, marks)| marks.iter())
            .filter(|m| m.placement == TimelinePlacement::Measured)
            .count();

        // Empty rows are omitted rather than rendered blank. A blank row and an
        // absent row mean different things: the first says "looked, found
        // nothing", the second says the caller never asked. Rendering all five
        // unconditionally would make the two indistinguishable, which is the
        // same conflation the engine refuses everywhere else.
        let layers = by_layer
            .into_iter()
            .filter(|(_, marks)| !marks.is_empty())
            .map(|(layer, events)| LayerRow { layer, events })
            .collect();

        Self {
            layers,
            unplaced,
            duration: timeline.duration,
            measured_marks,
            retention_notice: retention_notice(retention),
        }
    }

    /// Total markers across every row.
    #[must_use]
    pub fn mark_count(&self) -> usize {
        self.layers.iter().map(LayerRow::len).sum()
    }

    /// Returns the markers on one row.
    #[must_use]
    pub fn row(&self, layer: TimelineLayer) -> Option<&LayerRow> {
        self.layers.iter().find(|row| row.layer == layer)
    }
    /// Resolves a click on a finding into a destination (spec §81).
    ///
    /// Prefers the finding's own frame index when it has one: for a pixel rule
    /// that is the frame the measurement was taken from, which is exactly what
    /// an analyst needs to see. Falling back to "the frame nearest this time"
    /// would be the wrong one for a scene change, where the observation is
    /// about the difference between two frames.
    #[must_use]
    pub fn target_for_finding(&self, finding: &Finding) -> JumpTarget {
        let Some(time) = finding.timeline_start else {
            return JumpTarget::unplaced(&finding.observation.summary);
        };

        // Evidence first: if the finding carries artefacts, showing them is the
        // most useful landing spot, since they are what a reviewer verifies.
        let screen = if !finding.evidence.is_empty() {
            JumpScreen::Evidence
        } else if finding.frame_index.is_some() {
            JumpScreen::Video
        } else if finding.stream_id.is_some() {
            JumpScreen::Streams
        } else {
            JumpScreen::Findings
        };

        JumpTarget {
            screen,
            time: Some(time),
            frame_index: finding.frame_index,
            stream_id: finding.stream_id,
            finding_id: Some(finding.id),
            evidence: finding.evidence.clone(),
            explanation: format!("{} at {}", finding.rule_id, time.to_timecode()),
        }
    }

    /// Resolves a click on an unplaced observation (spec §31, §81).
    ///
    /// Returns `JumpScreen::Nowhere` with an explanation, rather than an error:
    /// clicking a file-level observation is a normal thing to do, and the answer
    /// is that this particular observation has no moment to jump to.
    #[must_use]
    pub fn target_for_unplaced(&self, entry: &TimelineEntry) -> JumpTarget {
        JumpTarget::unplaced(&entry.summary)
    }

    /// Finds the marker nearest a clicked position on one row.
    ///
    /// Returns the marker and how far the click was from it, so the frontend
    /// can reject a click that landed nowhere near anything instead of snapping
    /// to the only marker on the row and implying the analyst chose it.
    ///
    /// `tolerance` is in microseconds and is required rather than defaulted: a
    /// default would be a pixel-to-time conversion that belongs to the renderer,
    /// where the strip's actual width is known.
    #[must_use]
    pub fn nearest_mark(
        &self,
        layer: TimelineLayer,
        time: MediaTime,
        tolerance: i64,
    ) -> Option<(&TimelineMark, i64)> {
        let row = self.row(layer)?;
        row.events
            .iter()
            .map(|mark| (mark, (mark.time.as_micros() - time.as_micros()).abs()))
            .filter(|(_, distance)| *distance <= tolerance)
            .min_by_key(|(mark, distance)| (*distance, mark.time.as_micros()))
    }
}

/// The sentence the screen must show above the strip, if any.
///
/// Returns `None` when the strip is a complete record, which is the only case
/// where "nothing is shown here" means "nothing was found".
///
/// This lives in the model rather than the frontend because the wording *is* the
/// forensic claim. An empty strip and an unrecorded one produce identical
/// pixels, so the only thing separating "this run found nothing" from "this
/// run's observations were never recorded" is a sentence — and a sentence
/// assembled in a renderer is a sentence no Rust test can reach. By the time it
/// reaches the renderer the claim has already been made, unverified.
fn retention_notice(retention: Retention) -> Option<String> {
    match retention {
        Retention::Complete => None,
        Retention::NoRuns => Some(
            "No analysis has been run for this case, so there is no timeline to show. \
             Run an analysis to record one."
                .to_owned(),
        ),
        Retention::Partial {
            unrecorded_runs,
            total_runs,
        } => Some(format!(
            "{unrecorded_runs} of {total_runs} recorded runs predate timeline retention \
             and did not store one. The markers below are only what the remaining runs \
             observed; an absent track is not evidence that nothing was there."
        )),
    }
}

/// Maps a timeline source onto the row spec §42 draws it on.
fn layer_for(source: TimelineSource) -> TimelineLayer {
    match source {
        // Findings are layered from the findings list rather than from the
        // merged timeline, so the engine's own finding entries route there too
        // instead of being dropped from the strip.
        TimelineSource::Finding => TimelineLayer::Finding,
        // A timestamp anomaly is not a picture and not a sound; it is a defect
        // in the sequence, which is what the error row reports.
        TimelineSource::StructuralDamage
        | TimelineSource::Timestamp
        | TimelineSource::PacketDamage
        | TimelineSource::DecodeDamage => TimelineLayer::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::testing::{finding, finding_at, finding_with_evidence};
    use tpt_app_media_forensics_model::{Placement, StreamId};

    fn at(ms: i64) -> MediaTime {
        MediaTime::from_millis(ms)
    }

    fn timeline(
        entries: Vec<TimelineEntry>,
        duration_ms: i64,
    ) -> tpt_app_media_forensics_model::Timeline {
        tpt_app_media_forensics_model::Timeline::new(entries, Some(at(duration_ms)))
    }

    #[test]
    fn an_unplaced_entry_is_never_given_a_time() {
        // The whole point of the module. Spec §31 established that a header
        // defect has no media position; parking it at zero would be a fabricated
        // timecode that reads on the strip as "this happened at the start".
        let view = TimelineView::build(
            &timeline(
                vec![
                    TimelineEntry::measured(at(500), TimelineSource::Timestamp, "T", "gap"),
                    TimelineEntry::unplaced(
                        TimelineSource::StructuralDamage,
                        "CONTAINER.MISSING_MOOV",
                        "no moov box",
                    ),
                ],
                10_000,
            ),
            &[],
            Retention::Complete,
        );

        assert_eq!(view.unplaced.len(), 1);
        assert_eq!(view.mark_count(), 1, "only the positioned entry is drawn");
        assert!(
            view.layers
                .iter()
                .flat_map(|row| &row.events)
                .all(|mark| mark.time != MediaTime::ZERO),
            "nothing may be drawn at the origin unless it is really there"
        );
    }

    #[test]
    fn clicking_an_unplaced_observation_goes_nowhere_and_says_why() {
        let view = TimelineView::build(&tpt_app_media_forensics_model::Timeline::default(), &[], Retention::Complete);
        let entry = TimelineEntry::unplaced(
            TimelineSource::StructuralDamage,
            "CONTAINER.MISSING_MOOV",
            "no moov box",
        );

        let target = view.target_for_unplaced(&entry);
        assert_eq!(target.screen, JumpScreen::Nowhere);
        assert_eq!(target.time, None);
        assert!(
            target.explanation.contains("no media position"),
            "the analyst must be told why nothing happened: {target:?}"
        );
    }

    #[test]
    fn a_finding_with_no_position_joins_the_unplaced_list_rather_than_being_dropped() {
        // It is still an observation the analyst must be able to find. Dropping
        // it would hide a finding from the navigation the spec makes central.
        let findings = [finding(
            Severity::Critical,
            tpt_app_media_forensics_model::FindingStatus::New,
        )];
        let view = TimelineView::build(
            &tpt_app_media_forensics_model::Timeline::default(),
            &findings,
            Retention::Complete,
        );

        assert!(view.layers.is_empty(), "no marker was fabricated");
        assert_eq!(view.unplaced.len(), 1);
        assert_eq!(view.unplaced[0].source, TimelineSource::Finding);
    }

    #[test]
    fn clicking_a_positionless_finding_does_not_move_the_viewer() {
        let findings = [finding(
            Severity::Warning,
            tpt_app_media_forensics_model::FindingStatus::New,
        )];
        let view = TimelineView::build(
            &tpt_app_media_forensics_model::Timeline::default(),
            &findings,
            Retention::Complete,
        );

        let target = view.target_for_finding(&findings[0]);
        assert_eq!(target.screen, JumpScreen::Nowhere);
        assert_eq!(target.time, None);
    }

    #[test]
    fn inferred_and_measured_placements_are_both_carried() {
        // Both read `00:00:10`. A strip that showed them identically would let
        // an inference be quoted as a timestamp.
        let view = TimelineView::build(
            &timeline(
                vec![
                    TimelineEntry::inferred(
                        at(280),
                        TimelineSource::StructuralDamage,
                        "D",
                        "truncated",
                    ),
                    TimelineEntry::measured(at(100), TimelineSource::Timestamp, "T", "gap"),
                ],
                10_000,
            ),
            &[],
            Retention::Complete,
        );

        assert_eq!(view.measured_marks, 1, "one of the two is exact");
        let placements: Vec<Placement> = view
            .layers
            .iter()
            .flat_map(|row| &row.events)
            .map(|mark| mark.placement)
            .collect();
        assert!(placements.contains(&Placement::Inferred));
        assert!(placements.contains(&Placement::Measured));
    }

    #[test]
    fn findings_and_errors_land_on_their_own_rows() {
        let findings = [finding_at(Severity::Significant, 3_000, None)];
        let view = TimelineView::build(
            &timeline(
                vec![TimelineEntry::measured(
                    at(1_000),
                    TimelineSource::PacketDamage,
                    "CONTAINER.UNREADABLE_PACKET",
                    "empty access unit",
                )],
                10_000,
            ),
            &findings,
            Retention::Complete,
        );

        assert_eq!(view.row(TimelineLayer::Error).map(LayerRow::len), Some(1));
        assert_eq!(view.row(TimelineLayer::Finding).map(LayerRow::len), Some(1));
    }

    #[test]
    fn a_row_nothing_was_observed_on_is_omitted_not_drawn_blank() {
        // A blank row says "looked, found nothing"; an absent row says the
        // caller never asked. Rendering all five would conflate them.
        let view = TimelineView::build(&tpt_app_media_forensics_model::Timeline::default(), &[], Retention::Complete);
        assert!(view.layers.is_empty());
        assert!(view.row(TimelineLayer::Video).is_none());
    }

    #[test]
    fn a_finding_with_evidence_jumps_to_the_evidence() {
        // Evidence is what a reviewer verifies, so it is the most useful landing
        // spot when the finding carries artefacts.
        let findings = [finding_with_evidence()];
        let view = TimelineView::build(
            &tpt_app_media_forensics_model::Timeline::default(),
            &findings,
            Retention::Complete,
        );

        let target = view.target_for_finding(&findings[0]);
        assert_eq!(target.screen, JumpScreen::Evidence);
        assert!(!target.evidence.is_empty());
    }

    #[test]
    fn a_finding_with_a_frame_jumps_to_that_exact_frame() {
        // Not "the frame nearest this time": for a pixel rule, `frame_index` is
        // the frame the measurement was taken from.
        let mut f = finding_at(Severity::Warning, 5_000, None);
        f.frame_index = Some(1_042);
        let view = TimelineView::build(
            &tpt_app_media_forensics_model::Timeline::default(),
            &[f.clone()],
            Retention::Complete,
        );

        let target = view.target_for_finding(&f);
        assert_eq!(target.screen, JumpScreen::Video);
        assert_eq!(target.frame_index, Some(1_042));
    }

    #[test]
    fn a_positioned_finding_carries_its_stream() {
        let mut f = finding_at(Severity::Warning, 1_000, None);
        f.stream_id = Some(StreamId::new_derived(&["s"]));
        let view = TimelineView::build(
            &tpt_app_media_forensics_model::Timeline::default(),
            &[f.clone()],
            Retention::Complete,
        );

        let target = view.target_for_finding(&f);
        assert_eq!(target.screen, JumpScreen::Streams);
        assert!(target.stream_id.is_some());
    }

    #[test]
    fn a_finding_covering_a_span_is_drawn_as_one() {
        let f = finding_at(Severity::Warning, 1_000, Some(4_000));
        let view = TimelineView::build(&tpt_app_media_forensics_model::Timeline::default(), &[f], Retention::Complete);

        let mark = &view.row(TimelineLayer::Finding).expect("row").events[0];
        assert!(mark.is_span());
        assert_eq!(mark.end, Some(at(4_000)));
    }

    #[test]
    fn a_zero_length_finding_is_not_drawn_as_a_span() {
        let f = finding_at(Severity::Warning, 1_000, Some(1_000));
        let view = TimelineView::build(&tpt_app_media_forensics_model::Timeline::default(), &[f], Retention::Complete);

        let mark = &view.row(TimelineLayer::Finding).expect("row").events[0];
        assert!(!mark.is_span(), "an instant is not a range");
    }

    #[test]
    fn a_click_near_a_marker_selects_it() {
        let view = TimelineView::build(
            &timeline(
                vec![TimelineEntry::measured(
                    at(2_000),
                    TimelineSource::Timestamp,
                    "T",
                    "gap",
                )],
                10_000,
            ),
            &[],
            Retention::Complete,
        );

        let (mark, distance) = view
            .nearest_mark(TimelineLayer::Error, at(2_100), 500_000)
            .expect("a marker is in range");
        assert_eq!(mark.summary, "gap");
        assert_eq!(distance, 100_000);
    }

    #[test]
    fn a_click_far_from_every_marker_selects_nothing() {
        // Snapping to the only marker on the row would imply the analyst chose
        // it, which they did not.
        let view = TimelineView::build(
            &timeline(
                vec![TimelineEntry::measured(
                    at(2_000),
                    TimelineSource::Timestamp,
                    "T",
                    "gap",
                )],
                10_000,
            ),
            &[],
            Retention::Complete,
        );

        assert!(view
            .nearest_mark(TimelineLayer::Error, at(9_000), 1_000)
            .is_none());
    }

    #[test]
    fn the_nearest_of_several_markers_wins() {
        let view = TimelineView::build(
            &timeline(
                vec![
                    TimelineEntry::measured(at(1_000), TimelineSource::Timestamp, "A", "first"),
                    TimelineEntry::measured(at(2_000), TimelineSource::Timestamp, "B", "second"),
                    TimelineEntry::measured(at(3_000), TimelineSource::Timestamp, "C", "third"),
                ],
                10_000,
            ),
            &[],
            Retention::Complete,
        );

        let (mark, _) = view
            .nearest_mark(TimelineLayer::Error, at(2_400), 1_000_000)
            .expect("one is in range");
        assert_eq!(mark.summary, "second");
    }

    #[test]
    fn markers_are_ordered_by_time_within_a_row() {
        // The engine already sorts; re-sorting here means the frontend lays out
        // in the order the model declares, and two renders agree (spec §77).
        let view = TimelineView::build(
            &timeline(
                vec![
                    TimelineEntry::measured(at(3_000), TimelineSource::Timestamp, "C", "c"),
                    TimelineEntry::measured(at(1_000), TimelineSource::Timestamp, "A", "a"),
                    TimelineEntry::measured(at(2_000), TimelineSource::Timestamp, "B", "b"),
                ],
                10_000,
            ),
            &[],
            Retention::Complete,
        );

        let row = view.row(TimelineLayer::Error).expect("row");
        let times: Vec<i64> = row.events.iter().map(|m| m.time.as_micros()).collect();
        assert!(times.windows(2).all(|w| w[0] <= w[1]), "{times:?}");
    }

    #[test]
    fn the_duration_is_carried_so_the_axis_can_be_scaled() {
        let view = TimelineView::build(
            &timeline(
                vec![TimelineEntry::measured(
                    at(1_000),
                    TimelineSource::Timestamp,
                    "T",
                    "gap",
                )],
                90_000,
            ),
            &[],
            Retention::Complete,
        );
        assert_eq!(view.duration, Some(at(90_000)));
    }

    #[test]
    fn an_empty_timeline_produces_an_empty_strip_rather_than_failing() {
        // A case analysed from a cache has no timeline entries at all; the
        // screen must render as empty, not as an error.
        let view = TimelineView::build(&tpt_app_media_forensics_model::Timeline::default(), &[], Retention::Complete);
        assert_eq!(view.mark_count(), 0);
        assert_eq!(view.measured_marks, 0);
        assert!(view.unplaced.is_empty());
    }

    #[test]
    fn every_layer_has_a_distinct_label() {
        let mut labels: Vec<&str> = TimelineLayer::ALL.iter().map(|l| l.label()).collect();
        let before = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), before, "labels must be unique: {labels:?}");
        assert_eq!(TimelineLayer::ALL.len(), 5, "spec §42 lists five layers");
    }

    #[test]
    fn a_case_predating_timeline_retention_says_so_rather_than_showing_an_empty_strip() {
        // The whole point of the retention field. A case written before schema v4
        // has no `timeline_entries` and never will, so its strip is empty — and an
        // empty strip with no explanation reads as "this run found nothing", which
        // is the opposite of the truth and the more dangerous of the two.
        let view = TimelineView::build(
            &tpt_app_media_forensics_model::Timeline::default(),
            &[],
            Retention::Partial {
                unrecorded_runs: 1,
                total_runs: 3,
            },
        );

        let notice = view
            .retention_notice
            .expect("an incomplete strip must say so");
        assert!(
            notice.contains("predate timeline retention"),
            "the notice must name the cause, not just report an empty strip: {notice}"
        );
        assert!(
            notice.contains("1 of 3"),
            "the notice must say how much of the record is missing: {notice}"
        );
        assert!(
            notice.contains("not evidence that nothing was there"),
            "the notice must say which way the reading is wrong: {notice}"
        );
    }

    #[test]
    fn a_complete_strip_carries_no_notice() {
        // A notice that appears unconditionally is a banner nobody reads, and it
        // would blunt the notice that matters. Silence here is the signal.
        let view = TimelineView::build(
            &tpt_app_media_forensics_model::Timeline::default(),
            &[],
            Retention::Complete,
        );
        assert_eq!(view.retention_notice, None);
    }

    #[test]
    fn a_case_with_no_runs_is_not_a_run_that_found_nothing() {
        let view = TimelineView::build(
            &tpt_app_media_forensics_model::Timeline::default(),
            &[],
            Retention::NoRuns,
        );
        let notice = view.retention_notice.expect("must say nothing was run");
        assert!(
            notice.contains("No analysis has been run"),
            "{notice}"
        );
    }

    #[test]
    fn a_partial_case_with_a_full_strip_still_carries_the_notice() {
        // The strip drawing is not the signal. One legacy run and one current run
        // can leave a populated strip that is still not a record of the whole
        // case, and the notice has to survive that.
        let findings = [finding_at(Severity::Warning, 2_000, None)];
        let view = TimelineView::build(
            &tpt_app_media_forensics_model::Timeline::default(),
            &findings,
            Retention::Partial {
                unrecorded_runs: 1,
                total_runs: 2,
            },
        );

        assert_eq!(view.mark_count(), 1, "the later run's marker is drawn");
        assert!(
            view.retention_notice.is_some(),
            "a drawn strip is not a complete one"
        );
    }

    #[test]
    fn the_strip_round_trips_through_the_ipc_boundary() {
        let findings = [finding_at(Severity::Warning, 1_500, Some(2_500))];
        let view = TimelineView::build(
            &timeline(
                vec![TimelineEntry::measured(
                    at(1_000),
                    TimelineSource::Timestamp,
                    "T",
                    "gap",
                )],
                10_000,
            ),
            &findings,
            Retention::Complete,
        );

        let json = serde_json::to_string(&view).expect("encodes");
        let decoded: TimelineView = serde_json::from_str(&json).expect("decodes");
        assert_eq!(decoded, view);
    }
}
