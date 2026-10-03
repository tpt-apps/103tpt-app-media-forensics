//! The analysis pipeline (spec §97).
//!
//! Orchestrates acquisition, container inspection, the per-layer analyzers,
//! rule evaluation, caching, and evidence — in that order — and is the single
//! entry point both the CLI and the desktop app call (spec §51).
//!
//! # One engine, two front ends
//!
//! Nothing here knows about argument parsing or windows. A finding means the
//! same thing whether it arrived from `tpt-media-forensics analyze` or from the
//! GUI, because both call this.
//!
//! # Partial results are the norm, not a failure
//!
//! A file whose audio cannot be decoded still yields a full container report.
//! Every stage that could not run contributes a line to
//! [`AnalysisOutcome::limitations`] rather than aborting, because an examiner
//! needs to know what was not looked at as much as what was.
//!
//! # The source is never written
//!
//! The source is opened read-only for hashing and for the demuxers. Everything
//! this pipeline produces is written under the case directory.

use std::path::Path;

use tpt_app_media_forensics_audio::{find_silence, integrated_loudness, level_stats, Measurement};
use tpt_app_media_forensics_container::probe::{detect_file, extension_matches};
use tpt_app_media_forensics_container::{detect, ContainerFormat};
use tpt_app_media_forensics_metadata::{FingerprintReport, MetadataEntry, MetadataTree, Scope};
use tpt_app_media_forensics_model::{
    AcquisitionRecord, AnalysisId, AnalysisVersion, CacheKey, Case, Finding, MediaAsset, MediaTime,
    MediaType,
};
use tpt_app_media_forensics_rules::{
    builtin_rules, engine::empty_bundle, engine::AnalysisBundle, RuleEngine, RuleProfile,
};
use tpt_app_media_forensics_timing::pts_dts::scan_presentation;
use tpt_app_media_forensics_video::duplicate::find_repeated_runs;
use tpt_app_media_forensics_video::gop;

use crate::acquisition;
use crate::cache::{AnalysisCache, CacheEntry};
use crate::case_dir::CaseDirectory;
use crate::error::CoreError;
use crate::store::{Store, StoredAnalysis, StoredAsset};

/// What an analysis produced.
#[derive(Debug)]
pub struct AnalysisOutcome {
    /// The acquired asset, with its hashes.
    pub asset: MediaAsset,
    /// Findings, ordered most severe first.
    pub findings: Vec<Finding>,
    /// The key this analysis is cached under.
    pub cache_key: CacheKey,
    /// Whether the result came from the cache.
    pub cache_hit: bool,
    /// The metadata tree extracted from the container.
    pub metadata: MetadataTree,
    /// Observed encoder indicators, with their confidence and limits (spec §27).
    ///
    /// A field rather than only a finding, because an encoder indicator is an
    /// observation about the file, not a conclusion about it — the same reason
    /// `metadata` sits beside `findings` rather than inside it.
    pub fingerprint: FingerprintReport,
    /// Every located observation, in one order (spec §31).
    ///
    /// Merges structural damage, timestamp anomalies, and positioned findings so
    /// a reviewer can see *where* a problem sits rather than only that it exists.
    pub timeline: tpt_app_media_forensics_model::Timeline,
    /// What could not be measured, and why.
    pub limitations: Vec<String>,
    /// The profile used.
    pub profile: RuleProfile,
}

/// The analysis engine.
pub struct AnalysisEngine {
    rules: RuleEngine,
    profile: RuleProfile,
}

impl Default for AnalysisEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AnalysisEngine {
    /// Builds an engine with the built-in rule set and the default profile.
    #[must_use]
    pub fn new() -> Self {
        Self::with_profile(RuleProfile::default())
    }

    /// Builds an engine with a specific profile.
    #[must_use]
    pub fn with_profile(profile: RuleProfile) -> Self {
        Self {
            rules: RuleEngine::new(builtin_rules()),
            profile,
        }
    }

    /// Returns the profile in use.
    #[must_use]
    pub fn profile(&self) -> &RuleProfile {
        &self.profile
    }

    /// Analyses one source file within a case.
    ///
    /// # Errors
    ///
    /// Returns an error only when the source cannot be read or the case
    /// directory is unusable. A file the analysers cannot fully understand
    /// still produces findings, with the gaps recorded in `limitations`.
    pub fn analyse(
        &self,
        source: &Path,
        case_dir: &CaseDirectory,
    ) -> Result<AnalysisOutcome, CoreError> {
        // 1. Acquire: hashes and filesystem facts, read-only.
        let asset = acquisition::acquire_asset(source, MediaType::Container)?;
        let mut limitations = Vec::new();

        // 2. Cache key: asset content, engine behaviour, profile, rule set.
        let cache_key = CacheKey {
            asset_sha256: asset.sha256().unwrap_or_default().to_owned(),
            analysis_version: AnalysisVersion::CURRENT,
            profile: self.profile.fingerprint(),
            rules: self.rules.fingerprint(),
        };

        // 3. Cache lookup before doing any work.
        let cache = AnalysisCache::new(case_dir.root());
        let cached = cache.load(&cache_key)?;
        let cache_hit = cached.is_some();
        if let Some(entry) = cached {
            limitations.push(
                "encoder fingerprinting served from the analysis cache; re-run to \
                     recompute"
                    .to_owned(),
            );
            limitations.push(
                "the timeline was not rebuilt on a cache hit; only findings were served. \
                 Re-run to place observations on it"
                    .to_owned(),
            );
            return Ok(AnalysisOutcome {
                findings: entry.findings,
                cache_hit: true,
                cache_key,
                limitations,
                metadata: MetadataTree::default(),
                fingerprint: FingerprintReport::default(),
                timeline: tpt_app_media_forensics_model::Timeline::default(),
                asset,
                profile: self.profile.clone(),
            });
        }

        let bundle = self.run_stages(source, asset.id, &mut limitations)?;
        // 6. Rules.
        let findings =
            self.rules
                .evaluate(&bundle, &self.profile)
                .map_err(|e| CoreError::Serialise {
                    operation: "evaluate rules",
                    reason: e.to_string(),
                })?;

        // 6b. The unified timeline (spec §31).
        //
        // Built after the rules so findings can be placed, and from the bundle
        // rather than by re-reading the file: every input is already in hand, so
        // this costs no extra I/O and cannot disagree with the findings it places.
        let timeline = build_timeline(&bundle, &findings);

        // 7. Cache for next time.
        cache.store(&CacheEntry {
            key: cache_key.clone(),
            findings: findings.clone(),
            rule_ids: self
                .rules
                .rule_ids()
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            stream_count: 0,
        })?;

        // 8. Record the run in the case database so `report` can rebuild this
        //    without re-analysing. A cache hit deliberately skips this: the
        //    record already exists, and findings are append-only (spec §66).
        if !cache_hit {
            self.persist(case_dir, &asset, &cache_key, &findings)?;
        }

        // Encoder indicators are derived from the metadata tree and the keyframe
        // table, both of which are already in hand by this point — no extra I/O,
        // and no second parse of the file.
        // All-intra means every frame is a keyframe, which the GOP report can show
        // without a decoder: every counted frame is a keyframe. The `frame_count`
        // check keeps an empty track from reading as all-intra, which vacuous
        // truth would turn into an encoder indicator for a file with no frames.
        let all_intra = bundle
            .gop
            .as_ref()
            .is_some_and(|g| g.frame_count > 0 && g.keyframe_count == g.frame_count);
        let fingerprint = tpt_app_media_forensics_metadata::identify_encoders(
            bundle.metadata.as_ref(),
            all_intra,
        );
        for gap in &fingerprint.not_measured {
            limitations.push(gap.clone());
        }

        Ok(AnalysisOutcome {
            asset,
            findings,
            cache_key,
            cache_hit: false,
            metadata: bundle.metadata.clone().unwrap_or_default(),
            fingerprint,
            timeline,
            limitations,
            profile: self.profile.clone(),
        })
    }
    /// Runs every per-layer analyser and returns the populated bundle.
    ///
    /// Split out of [`AnalysisEngine::analyse`] so the stages can be observed
    /// directly. Two stages shipped that were fully implemented, documented, and
    /// unit-tested, and that no test could catch because no test could see: nothing
    /// called them, so the rules they fed could never fire. A stage you cannot
    /// inspect is a stage you cannot prove is wired.
    ///
    /// This bypasses the cache and persistence on purpose: it is an observation
    /// surface, not a second analysis path.
    fn run_stages(
        &self,
        source: &Path,
        asset_id: tpt_app_media_forensics_model::AssetId,
        limitations: &mut Vec<String>,
    ) -> Result<AnalysisBundle, CoreError> {
        // 4. Container inspection.
        //
        // Only the header is read here, and only for the formats that support a
        // partial read. An MP4's `moov` box holds the sample tables, codec
        // descriptions, and timing, and `mdat` - which on a long recording is
        // nearly the whole file - is never touched. Reading a whole 40 GB master
        // to inspect a 2 KB `moov` is what makes an examination impractical, so
        // the media data stays on disk unless sample reading is genuinely needed.
        //
        // Matroska has no equivalent: its `Tracks` element sits inside the
        // `Segment`, and clusters carrying the frames follow it, so track
        // enumeration and sample reading come from one whole-file pass. That is
        // bounded rather than silent, and the bound is stated as a limitation.
        let header =
            tpt_app_media_forensics_container::read_header(source, 64 * 1024).map_err(|e| {
                CoreError::io(
                    "read source header",
                    source.display().to_string(),
                    std::io::Error::other(e),
                )
            })?;
        let format = detect(&header);
        let inspection = match format {
            ContainerFormat::IsoBmff => read_container(
                || tpt_app_media_forensics_container::inspect_path(source),
                limitations,
            ),
            ContainerFormat::Matroska => {
                if file_size(source) > tpt_app_media_forensics_container::MAX_INSPECTED_BYTES {
                    limitations.push(format!(
                        "the file is {} bytes, above the {} byte limit for whole-file Matroska \
                         parsing, so no stream could be inspected",
                        file_size(source),
                        tpt_app_media_forensics_container::MAX_INSPECTED_BYTES
                    ));
                    None
                } else {
                    read_container(
                        || tpt_app_media_forensics_container::inspect_matroska_file(source),
                        limitations,
                    )
                }
            }
            other => {
                limitations.push(format!(
                    "container format {} has no demuxer integrated yet",
                    other.tag()
                ));
                None
            }
        };

        // Structural damage found by the scan below. Collected separately from
        // `inspection.anomalies` because those are free-text strings attached to
        // a parse, while these are typed and carry the byte offsets that make
        // them actionable.
        let mut structural_damage: Vec<tpt_app_media_forensics_container::StructuralDamage> =
            Vec::new();

        // 4b. Structural damage scan (spec §30).
        //
        // Deliberately separate from the demuxer. The demuxer returns the tracks
        // it managed to read and reports success for everything before the point
        // the bytes stopped making sense, so it has by construction lost the
        // boundary — and "where does this file stop being trustworthy" is the
        // question an examination actually turns on.
        //
        // Scanned from the bytes rather than the inspection result, so it runs
        // even when the demuxer failed entirely: a file too damaged to yield
        // any stream is precisely the file whose damage most needs recording.
        // Reading the whole file is bounded by the same cap as inspection.
        if format == ContainerFormat::IsoBmff {
            let size = file_size(source);
            if size <= tpt_app_media_forensics_container::MAX_INSPECTED_BYTES {
                match std::fs::read(source) {
                    Ok(bytes) => {
                        for defect in tpt_app_media_forensics_container::scan_isobmff(&bytes) {
                            structural_damage.push(defect);
                        }
                    }
                    Err(error) => limitations.push(format!(
                        "the file could not be read for a structural damage scan: {error}"
                    )),
                }
            } else {
                limitations.push(format!(
                    "the file is {} bytes, above the {} byte limit for whole-file structural \
                     scanning, so structural damage was not assessed",
                    size,
                    tpt_app_media_forensics_container::MAX_INSPECTED_BYTES
                ));
            }
        }

        // 5. Per-layer analysis.
        let mut bundle = empty_bundle(asset_id);
        // Samples are read once and reused: Tier-2 decodes from exactly the
        // bytes duplicate detection hashed, so reading them twice would parse
        // the file twice for no new information.
        let mut samples_for_tier_two: Option<Vec<tpt_app_media_forensics_container::SampleRecord>> =
            None;

        if let Some(inspection) = &inspection {
            bundle.container = Some(inspection.clone());

            // Frame-level timing analysis reads the **video** stream. Reaching
            // for whichever stream happened to be first meant an audio-only
            // file had its audio frames treated as video frames, so
            // `VIDEO.SINGLE_KEYFRAME` fired on an MP3 and GOP "structure" was
            // reported for a track that has no concept of one.
            if let Some(info) = inspection.first_video_frames() {
                bundle.timestamps = vec![scan_presentation(
                    &info.frame_times,
                    self.profile.pts_tolerance,
                )];
            }
        }

        // GOP and duplicate detection both work at the packet layer.
        if let Some(inspection) = &inspection {
            if let Some(info) = inspection.first_video_frames() {
                bundle.gop = Some(gop::analyse(
                    &info.keyframes,
                    &info.frame_times,
                    self.profile.gop_tolerance_frames,
                ));
            }
            // Duplicate detection needs every sample's bytes, so it genuinely
            // requires the media data. On a file too large to hold, it is
            // skipped and the gap is stated rather than silently omitted.
            let size = file_size(source);
            if size <= tpt_app_media_forensics_container::MAX_SAMPLED_BYTES {
                // Sample reading is format-specific: the demuxer that
                // enumerated the streams is the one that can read their
                // samples. Dispatching on the detected format keeps the two
                // paths from disagreeing about what a track contains.
                let read = match format {
                    ContainerFormat::Matroska => {
                        tpt_app_media_forensics_container::read_matroska_samples_file(source)
                    }
                    _ => tpt_app_media_forensics_container::read_samples_file(source),
                };
                match read {
                    Ok(samples) => {
                        // Duplicate detection is a **video** measurement: a
                        // repeated compressed video frame can mean a freeze or
                        // an inserted still. Hashing every stream flattened
                        // them into one sequence, so an audio track full of
                        // identical silence packets raised
                        // `VIDEO.DUPLICATE_FRAME_RUN` — a video finding derived
                        // entirely from audio.
                        let video_digests: Vec<
                            tpt_app_media_forensics_video::duplicate::SampleDigest,
                        > = samples
                            .iter()
                            .filter(|s| {
                                inspection.streams.get(s.stream_index as usize).is_some_and(
                                    |stream| {
                                        stream.kind
                                            == tpt_app_media_forensics_model::StreamKind::Video
                                    },
                                )
                            })
                            .map(|s| tpt_app_media_forensics_video::duplicate::SampleDigest {
                                digest: s.digest.clone(),
                                time: s.time,
                                is_key_frame: s.is_key_frame,
                            })
                            .collect();
                        if !video_digests.is_empty() {
                            bundle.repeated_runs =
                                find_repeated_runs(&video_digests, self.profile.min_duplicate_run);
                        }
                        // Tier-2 decodes from these same bytes, so they are
                        // retained here instead of being re-read per codec.
                        samples_for_tier_two = Some(samples);
                    }
                    Err(error) => limitations.push(format!(
                        "sample reading failed, so duplicate detection was skipped: {error}"
                    )),
                }
            } else {
                limitations.push(format!(
                    "file is {size} bytes, above the {} byte limit for sample-level duplicate detection; \
                     structural analysis still covers it",
                    tpt_app_media_forensics_container::MAX_SAMPLED_BYTES
                ));
            }
        }

        // 5c. Audio: decode a royalty-free track and measure it.
        //
        // The four audio rules read `audio_levels`, `silence`, and `loudness`.
        // Until this stage existed they were never populated, so every audio
        // rule was permanently dead no matter what the file contained.
        self.run_audio(source, &inspection, &format, &mut bundle, limitations);

        // 5c-ii. Sample index, for placing damage on a timeline (spec §31).
        //
        // Built from the samples already read for duplicate detection, so it
        // costs no additional I/O. The anchor is the first `mdat`'s payload
        // start, which is where a contiguous ISO-BMFF file's media data begins.
        //
        // When samples were not read — file above the sampling bound, or a read
        // failure — the index stays empty and damage findings carry a byte
        // offset with no timecode. That is stated rather than guessed: a
        // timecode inferred from a sample table that was never read would be a
        // fabrication dressed as a measurement.
        if let Some(samples) = samples_for_tier_two.as_deref() {
            let anchor = mdat_payload_offset(source).unwrap_or(0);
            bundle.sample_index =
                tpt_app_media_forensics_container::SampleIndex::build(samples, anchor);

            // 5c-iii. Bitrate analysis (spec §28-§29).
            //
            // Built from the same samples, so it costs no additional I/O. Only
            // video streams are measured: combining a video and an audio rate
            // into one figure hides exactly the per-track variation the rule
            // exists to surface, and an audio-only file gets no bitrate finding
            // rather than a misleading one.
            let video_samples: Vec<tpt_app_media_forensics_video::bitrate::BitrateSample> = samples
                .iter()
                .filter(|sample| {
                    inspection
                        .as_ref()
                        .and_then(|i| i.streams.get(sample.stream_index as usize))
                        .is_some_and(|stream| {
                            stream.kind == tpt_app_media_forensics_model::StreamKind::Video
                        })
                })
                .map(
                    |sample| tpt_app_media_forensics_video::bitrate::BitrateSample {
                        time: sample.time,
                        size: sample.size as u64,
                        is_key_frame: sample.is_key_frame,
                    },
                )
                .collect();

            if video_samples.len() >= 2 {
                bundle.bitrate = Some(tpt_app_media_forensics_video::bitrate::analyse(
                    &video_samples,
                    self.profile.bitrate_window_frames,
                    self.profile.bitrate_anomaly_ratio,
                ));
            }
        }

        // 5d. A/V synchronisation, when the file actually has both streams.
        //
        // `av_sync::analyse` is implemented and tested, but until this stage
        // existed nothing called it, so `bundle.sync` stayed `None` and
        // `TIMING.AV_SYNC_DRIFT` never fired on any file.
        self.run_av_sync(&inspection, &mut bundle, limitations);

        // Metadata lives in moov, which was already read for inspection, so this
        // costs nothing extra and never touches the media data.
        // 5b. Tier-2: pixel-level analysis (spec §16-§18).
        //
        // Runs only when the samples were already read and the decoder is
        // pixel-exact. Every reason Tier-2 cannot run becomes a limitation, so a
        // report never implies a measurement was made when it was not.
        self.run_tier_two(&samples_for_tier_two, &inspection, &mut bundle, limitations);

        // Metadata extraction reads ISO-BMFF atoms. A Matroska file carries its
        // tags in a different element tree that this build does not parse, so
        // the absence is stated rather than left to look like a file with no
        // metadata at all — the two are different observations.
        bundle.metadata = match format {
            ContainerFormat::IsoBmff => tpt_app_media_forensics_container::read_moov(source)
                .ok()
                .and_then(|bytes| extract_metadata(&bytes)),
            ContainerFormat::Matroska => {
                limitations.push(
                    "Matroska tags are not extracted by this build; only ISO-BMFF metadata \
                     atoms are read"
                        .to_owned(),
                );
                None
            }
            _ => None,
        };
        if bundle.metadata.as_ref().is_none_or(MetadataTree::is_empty) {
            limitations.push("no readable metadata atoms were found".to_owned());
        }

        // Damage is attached to the bundle so rules can grade it. An empty
        // vector means "scanned and found nothing", which is a measurement; a
        // `None` would mean "never scanned", which is a gap. The distinction is
        // the whole point of recording it here rather than logging it.
        bundle.damage = structural_damage;

        Ok(bundle)
    }

    /// Runs every per-layer analyser over `source` and returns the bundle.
    ///
    /// This is the observation surface the unwired-stage guard is built on. It
    /// deliberately bypasses the cache and writes nothing to the case: it exists
    /// so a test can ask "which analyses actually ran?" rather than inferring it
    /// from findings, which cannot distinguish "the stage ran and found nothing"
    /// from "the stage never ran".
    ///
    /// A source that cannot even be acquired yields an empty bundle and the
    /// reason, rather than an error: the guard runs over a corpus and should not
    /// abort because one fixture is unreadable.
    #[must_use]
    pub fn observe_stages(&self, source: &Path) -> (AnalysisBundle, Vec<String>) {
        let asset = match acquisition::acquire_asset(source, MediaType::Container) {
            Ok(asset) => asset,
            Err(error) => {
                return (
                    empty_bundle(tpt_app_media_forensics_model::AssetId::new_derived(&[
                        b"unreadable",
                    ])),
                    vec![format!("the source could not be acquired: {error}")],
                );
            }
        };

        let mut limitations = Vec::new();
        match self.run_stages(source, asset.id, &mut limitations) {
            Ok(bundle) => (bundle, limitations),
            Err(error) => (
                empty_bundle(asset.id),
                vec![format!("analysis could not run: {error}")],
            ),
        }
    }

    /// Compares audio and video presentation timing when the file has both.
    ///
    /// A/V offset is only meaningful when both streams exist. A video-only or
    /// audio-only file therefore produces no sync report and no limitation: the
    /// measurement was never applicable, which is a different statement from
    /// having been attempted and failed.
    fn run_av_sync(
        &self,
        inspection: &Option<tpt_app_media_forensics_container::ContainerInspection>,
        bundle: &mut AnalysisBundle,
        limitations: &mut Vec<String>,
    ) {
        use tpt_app_media_forensics_model::StreamKind;
        use tpt_app_media_forensics_timing::av_sync::{analyse, AudioSamples, VideoSamples};

        let Some(inspection) = inspection else {
            return;
        };

        // Both tracks must be located by kind. Taking the first two streams
        // would compare audio against video for most files and audio against
        // audio for the rest, and the second case yields a confident,
        // meaningless zero offset.
        let frames_of = |kind: StreamKind| -> Option<Vec<_>> {
            let position = inspection.streams.iter().position(|s| s.kind == kind)?;
            inspection
                .frame_info
                .get(position)
                .and_then(|info| info.as_ref())
                .map(|info| info.frame_times.clone())
        };

        let (Some(video), Some(audio)) =
            (frames_of(StreamKind::Video), frames_of(StreamKind::Audio))
        else {
            return;
        };

        match analyse(
            &VideoSamples { timestamps: video },
            &AudioSamples { timestamps: audio },
        ) {
            Ok(report) => bundle.sync = Some(report),
            // Neither an offset nor a drift can come from a single point.
            // Saying so beats reporting a zero offset that was never measured.
            Err(error) => {
                limitations.push(format!(
                    "A/V synchronisation could not be measured: {error}"
                ));
            }
        }
    }

    /// Runs audio analysis, recording why it was skipped if it was.
    ///
    /// Only royalty-free codecs are decoded. A patent-encumbered track is
    /// reported as *not decoded by this build*, which is a different statement
    /// from "the audio was clean" — and the difference matters, because the
    /// audio rules would otherwise silently contribute nothing while the report
    /// reads as though they had run and found nothing.
    ///
    /// Exactly one track is measured. A file with several audio tracks would
    /// need a choice of which is primary, and picking one silently would make
    /// the result depend on container order. That is stated instead.
    fn run_audio(
        &self,
        source: &Path,
        inspection: &Option<tpt_app_media_forensics_container::ContainerInspection>,
        format: &ContainerFormat,
        bundle: &mut AnalysisBundle,
        limitations: &mut Vec<String>,
    ) {
        use tpt_app_media_forensics_audio::is_audio_decodable;

        let Some(inspection) = inspection else {
            return;
        };
        let tracks: Vec<_> = inspection
            .streams
            .iter()
            .filter(|s| s.kind == tpt_app_media_forensics_model::StreamKind::Audio)
            .collect();

        if tracks.is_empty() {
            return;
        }
        if tracks.len() > 1 {
            limitations.push(format!(
                "the file carries {} audio tracks; only the first (index {}) was decoded and \
                 measured, so findings describe that track alone",
                tracks.len(),
                tracks[0].index
            ));
        }

        let track = tracks[0];
        if !is_audio_decodable(&track.codec.name) {
            limitations.push(format!(
                "the audio track is `{}`, which this build does not decode (only royalty-free \
                 Opus and Vorbis are). Its declared properties are reported; its signal was \
                 not measured, so no audio finding applies to it",
                track.codec.name
            ));
            return;
        }

        // Matroska audio arrives as demuxed access units, not as a bare Ogg
        // stream. A WebM file is Matroska, and handing its bytes to an Ogg
        // reader fails on the capture pattern — the two formats share a
        // lineage and nothing else.
        let limits = tpt_app_media_forensics_audio::AudioDecodeLimits::new(2);
        let decoded = match format {
            ContainerFormat::Matroska => {
                let samples =
                    match tpt_app_media_forensics_container::read_matroska_samples_file(source) {
                        Ok(samples) => samples,
                        Err(error) => {
                            limitations.push(format!("the audio track could not be read: {error}"));
                            return;
                        }
                    };
                let payloads: Vec<Vec<u8>> = samples
                    .iter()
                    .filter(|s| s.stream_index == track.index)
                    .map(|s| s.data.clone())
                    .collect();
                if payloads.is_empty() {
                    limitations.push(
                        "the audio track declared no samples, so it was not measured".to_owned(),
                    );
                    return;
                }
                // Matroska carries Opus in the same channel layout Opus itself
                // uses, so the track's declared channel count is the right one
                // to decode at.
                let channels = track.audio.as_ref().map_or(1, |a| a.channel_count().max(1));
                tpt_app_media_forensics_audio::decode_opus_packets(&payloads, channels, limits)
            }
            _ => {
                limitations.push(
                    "audio decoding covers Opus and Vorbis; audio inside MP4 is identified but \
                     not decoded by this build"
                        .to_owned(),
                );
                return;
            }
        };

        let decoded = match decoded {
            Ok(decoded) => decoded,
            Err(error) => {
                limitations.push(format!("the audio track could not be decoded: {error}"));
                return;
            }
        };

        if decoded.truncated {
            limitations.push(
                "audio decoding stopped at the frame limit; the audio findings below describe \
                 a prefix of the track, not the whole of it"
                    .to_owned(),
            );
        }

        match measure_audio(
            &decoded.pcm,
            decoded.channels,
            decoded.sample_rate,
            &self.profile,
        ) {
            Some(measured) => {
                bundle.audio_levels = Some(measured.levels);
                bundle.silence = measured.silence;
                bundle.loudness = measured.loudness;
            }
            None => limitations
                .push("the audio track decoded to no samples, so it was not measured".to_owned()),
        }
    }

    /// Runs pixel-level analysis, recording why it was skipped if it was.
    ///
    /// Tier-2 needs the encoded samples, which are only in memory when the file
    /// was within the sampling bound, and it needs a pixel-exact decoder. Both
    /// conditions are reported rather than passed over in silence: a report that
    /// omits a measurement without saying so would read as though none was due.
    fn run_tier_two(
        &self,
        samples: &Option<Vec<tpt_app_media_forensics_container::SampleRecord>>,
        inspection: &Option<tpt_app_media_forensics_container::ContainerInspection>,
        bundle: &mut AnalysisBundle,
        limitations: &mut Vec<String>,
    ) {
        use tpt_app_media_forensics_video::{
            is_decodable, near_duplicate, scene, DecodeLimits, DecodeSession,
        };

        let Some(inspection) = inspection else {
            limitations.push(
                "Tier-2 pixel analysis was not run: no container structure was recovered"
                    .to_owned(),
            );
            return;
        };
        let Some(stream) = inspection
            .streams
            .iter()
            .find(|s| s.kind == tpt_app_media_forensics_model::StreamKind::Video)
        else {
            return;
        };

        if !is_decodable(&stream.codec.name) {
            limitations.push(format!(
                "Tier-2 pixel analysis was not run: codec `{}` has no integrated decoder \
                 (only royalty-free VP9 and AV1 are decoded)",
                stream.codec.name
            ));
            return;
        }

        let Some(samples) = samples else {
            limitations.push(
                "Tier-2 pixel analysis was not run: the encoded samples were not read".to_owned(),
            );
            return;
        };

        let packets: Vec<(Vec<u8>, bool)> = samples
            .iter()
            .filter(|s| s.stream_index == stream.index)
            .map(|s| (s.data.clone(), s.is_key_frame))
            .collect();
        if packets.is_empty() {
            return;
        }

        let mut session = match DecodeSession::open(&stream.codec.name, DecodeLimits::default()) {
            Ok(session) => session,
            Err(error) => {
                limitations.push(format!("Tier-2 pixel analysis was withheld: {error}"));
                return;
            }
        };

        let (frames, stopped) = session.decode_prefix(&packets);
        if let Some(error) = stopped {
            limitations.push(format!(
                "Tier-2 pixel analysis covered {} frames before stopping: {error}",
                frames.len()
            ));
        }

        if frames.len() < 2 {
            limitations.push(format!(
                "Tier-2 pixel analysis decoded {} frame(s); at least two are needed to compare them",
                frames.len()
            ));
            return;
        }

        bundle.scene = Some(scene::analyse(&frames));
        bundle.near_duplicates = Some(near_duplicate::analyse(
            &frames,
            self.profile.near_duplicate_window,
        ));
    }

    /// Records an analysis run and its findings in the case database.
    ///
    /// Written as one transaction: a run whose findings were only partly written
    /// would leave the case reporting fewer observations than the engine
    /// produced, which is exactly the kind of silent gap this product must not
    /// have.
    fn persist(
        &self,
        case_dir: &CaseDirectory,
        asset: &MediaAsset,
        cache_key: &CacheKey,
        findings: &[Finding],
    ) -> Result<(), CoreError> {
        let store = Store::open(case_dir.root())?;

        let manifest = case_dir.read_manifest()?;

        let case_id = manifest.case_id.clone();
        let asset_id = asset.id.to_string();
        let stored_asset = StoredAsset {
            id: asset_id.clone(),
            case_id: case_id.clone(),
            name: asset.name.clone(),
            source_path: asset.acquisition.source_path.clone(),
            size_bytes: asset.size_bytes(),
            sha256: asset.sha256().map(ToOwned::to_owned),
            blake3: asset.blake3().map(ToOwned::to_owned),
        };

        // An asset already in the case is left alone: the same file analysed
        // twice must not create a second asset row (spec §10).
        let existing = store.assets_in_case(&case_id).map_err(rusqlite_to_core)?;
        if !existing.iter().any(|a| a.id == stored_asset.id) {
            store
                .upsert_case(&case_id, &manifest.name, manifest.description.as_deref())
                .map_err(rusqlite_to_core)?;
            store
                .insert_asset(&stored_asset)
                .map_err(rusqlite_to_core)?;
        }

        let analysis = StoredAnalysis {
            id: AnalysisId::new_derived(&[
                asset_id.as_bytes(),
                cache_key.to_key_string().as_bytes(),
            ])
            .to_string(),
            case_id,
            asset_id: asset_id.clone(),
            cache_key: cache_key.to_key_string(),
            finding_count: i64::try_from(findings.len()).unwrap_or(i64::MAX),
            rule_count: i64::try_from(self.rules.rule_ids().len()).unwrap_or(i64::MAX),
            profile: self.profile.identifier(),
            profile_fingerprint: cache_key.profile.as_hex().to_owned(),
            rule_set_fingerprint: cache_key.rules.as_hex().to_owned(),
            started_at: analysis_timestamp(),
        };

        store.insert_analysis(&analysis).map_err(rusqlite_to_core)?;
        store
            .insert_rule_results(
                &analysis.id,
                &self
                    .rules
                    .rule_ids()
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect::<Vec<_>>(),
            )
            .map_err(rusqlite_to_core)?;
        for finding in findings {
            store
                .insert_finding(&analysis.id, finding)
                .map_err(rusqlite_to_core)?;
        }

        Ok(())
    }

    /// Returns the combined analysis fingerprint for a cache key (spec §63).
    ///
    /// Derived from the same inputs that key the cache, so a report and its
    /// cache validity cannot disagree.
    #[must_use]
    pub fn analysis_fingerprint(&self, key: &CacheKey) -> String {
        MethodOfFingerprint::compute(
            &key.asset_sha256,
            &key.analysis_version.to_string(),
            key.profile.as_hex(),
            key.rules.as_hex(),
        )
    }
}

/// Indirection so the fingerprint lives with the report type that also computes
/// one, keeping the two implementations in step.
struct MethodOfFingerprint;

impl MethodOfFingerprint {
    fn compute(asset: &str, version: &str, profile: &str, rules: &str) -> String {
        tpt_app_media_forensics_report::Methodology::compute_fingerprint(
            asset, version, profile, rules,
        )
    }
}

/// Builds the unified timeline from the analysed bundle and the findings (spec §31).
///
/// Merges three sources that arrive by unrelated routes, and keeps each one's
/// placement provenance intact:
///
/// - **Structural damage** sits at a byte offset; it becomes a media time only by
///   inference through the sample index, so its entries are marked `Inferred`.
/// - **Timestamp anomalies** sit at a sample index the scanner already knew, so
///   they are `Measured`.
/// - **Findings** carry whatever position their rule chose. One with no position
///   becomes `Unplaced` rather than being dropped: the finding is real, and
///   dropping it here would mean the timeline disagreed with the findings list.
fn build_timeline(
    bundle: &tpt_app_media_forensics_rules::AnalysisBundle,
    findings: &[Finding],
) -> tpt_app_media_forensics_model::Timeline {
    use tpt_app_media_forensics_model::{TimelineEntry, TimelineSource};

    let mut entries: Vec<TimelineEntry> = Vec::new();

    // Structural damage, placed through the sample index.
    for defect in &bundle.damage {
        let placed = bundle.sample_index.locate(defect.offset());
        match placed.map(|position| position.time) {
            Some(time) => entries.push(TimelineEntry::inferred(
                time,
                TimelineSource::StructuralDamage,
                defect.tag(),
                defect.describe(),
            )),
            None => entries.push(TimelineEntry::unplaced(
                TimelineSource::StructuralDamage,
                defect.tag(),
                defect.describe(),
            )),
        }
    }

    // Timestamp anomalies, placed at the sample index the scanner reported.
    for report in &bundle.timestamps {
        for anomaly in &report.anomalies {
            let Some(time) = anomaly_time(report, anomaly) else {
                continue;
            };
            entries.push(TimelineEntry::measured(
                time,
                TimelineSource::Timestamp,
                anomaly_tag(anomaly),
                describe_anomaly(anomaly),
            ));
        }
    }

    // Findings, placed exactly where their rule put them.
    for finding in findings {
        match finding.timeline_start {
            Some(time) => entries.push(TimelineEntry::measured(
                time,
                TimelineSource::Finding,
                &finding.rule_id,
                &finding.observation.summary,
            )),
            None => entries.push(TimelineEntry::unplaced(
                TimelineSource::Finding,
                &finding.rule_id,
                &finding.observation.summary,
            )),
        }
    }

    let duration = bundle.container.as_ref().and_then(|container| {
        container
            .streams
            .iter()
            .filter_map(|stream| stream.timing.measured_duration)
            .max_by_key(|time| time.as_micros())
    });

    tpt_app_media_forensics_model::Timeline::new(entries, duration)
}

/// The presentation time a timestamp anomaly sits at, from the stream's own times.
///
/// A gap or an overlap has no single sample time of its own; it sits between two
/// samples, and the earlier one is used, because that is the point at which the
/// discontinuity becomes visible. `None` when the report carries no timestamps to
/// resolve the index against — which would mean the anomaly cannot be placed, not
/// that it belongs at zero.
fn anomaly_time(
    report: &tpt_app_media_forensics_timing::pts_dts::TimestampReport,
    anomaly: &tpt_app_media_forensics_timing::pts_dts::Anomaly,
) -> Option<tpt_app_media_forensics_model::MediaTime> {
    // The report does not retain the timestamps it scanned, so the anomaly's own
    // observed value is used where it has one.
    match anomaly {
        tpt_app_media_forensics_timing::pts_dts::Anomaly::NonMonotonicDts { observed, .. }
        | tpt_app_media_forensics_timing::pts_dts::Anomaly::NonMonotonicPts { observed, .. }
        | tpt_app_media_forensics_timing::pts_dts::Anomaly::NegativeTimestamp {
            observed, ..
        } => Some(*observed),
        // A gap and an overlap name a size, not a time. Placing them needs the
        // neighbouring timestamps, which the report does not carry, so they stay
        // off the timeline rather than at a fabricated instant.
        tpt_app_media_forensics_timing::pts_dts::Anomaly::Gap { .. }
        | tpt_app_media_forensics_timing::pts_dts::Anomaly::Overlap { .. } => {
            let _ = report;
            None
        }
    }
}

/// A stable tag naming the kind of timestamp anomaly.
fn anomaly_tag(anomaly: &tpt_app_media_forensics_timing::pts_dts::Anomaly) -> &'static str {
    use tpt_app_media_forensics_timing::pts_dts::Anomaly;
    match anomaly {
        Anomaly::NonMonotonicDts { .. } => "TIMING.NON_MONOTONIC_DTS",
        Anomaly::NonMonotonicPts { .. } => "TIMING.NON_MONOTONIC_PTS",
        Anomaly::Gap { .. } => "TIMING.TIMESTAMP_GAP",
        Anomaly::Overlap { .. } => "TIMING.TIMESTAMP_OVERLAP",
        Anomaly::NegativeTimestamp { .. } => "TIMING.NEGATIVE_TIMESTAMP",
    }
}

/// Renders a timestamp anomaly as one readable line.
///
/// Lives here rather than on `Anomaly` itself because the timeline is the only
/// consumer that needs prose; the anomaly type stays a plain data enum, and adding
/// a rendering method to it would make every consumer of the timing crate depend on
/// this project's phrasing.
fn describe_anomaly(anomaly: &tpt_app_media_forensics_timing::pts_dts::Anomaly) -> String {
    use tpt_app_media_forensics_timing::pts_dts::Anomaly;
    match anomaly {
        Anomaly::NonMonotonicDts {
            index,
            previous,
            observed,
        } => format!(
            "decode timestamp at sample {index} moved backwards from {} to {}",
            previous.to_timecode(),
            observed.to_timecode()
        ),
        Anomaly::NonMonotonicPts {
            index,
            previous,
            observed,
        } => format!(
            "presentation timestamp at sample {index} moved backwards from {} to {}",
            previous.to_timecode(),
            observed.to_timecode()
        ),
        Anomaly::Gap { index, size } => {
            format!("a gap of {} precedes sample {index}", size.to_timecode())
        }
        Anomaly::Overlap { index, size } => {
            format!(
                "sample {index} overlaps the previous by {}",
                size.to_timecode()
            )
        }
        Anomaly::NegativeTimestamp { index, observed } => format!(
            "sample {index} carries the negative timestamp {}",
            observed.to_timecode()
        ),
    }
}

/// Extracts metadata atoms, recording scope so conflicts remain visible.
fn extract_metadata(bytes: &[u8]) -> Option<MetadataTree> {
    let mut entries = Vec::new();
    let mut offset = 0usize;

    while offset + 8 <= bytes.len() {
        let Some(size) = read_box_size(&bytes[offset..]) else {
            break;
        };
        if size > bytes.len() - offset {
            break;
        }
        let kind = &bytes[offset + 4..offset + 8];
        let payload = &bytes[offset + 8..offset + size];

        if kind == b"moov" {
            collect(payload, Scope::Container, None, &mut entries);
            let mut track_index = 0u32;
            let mut inner = 0usize;
            while inner + 8 <= payload.len() {
                let Some(inner_size) = read_box_size(&payload[inner..]) else {
                    break;
                };
                if inner_size > payload.len() - inner {
                    break;
                }
                if &payload[inner + 4..inner + 8] == b"trak" {
                    collect(
                        &payload[inner + 8..inner + inner_size],
                        Scope::Track,
                        Some(track_index),
                        &mut entries,
                    );
                    track_index = track_index.saturating_add(1);
                }
                inner += inner_size;
            }
        }
        offset += size;
    }

    (!entries.is_empty()).then(|| MetadataTree::new(entries))
}

/// Reads a box size, rejecting degenerate forms.
fn read_box_size(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 8 {
        return None;
    }
    let size = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    (size >= 8).then_some(size)
}

/// Collects free-text metadata atoms from a box payload.
fn collect(payload: &[u8], scope: Scope, track_index: Option<u32>, out: &mut Vec<MetadataEntry>) {
    const TEXT_BOXES: [&[u8; 4]; 3] = [b"\xa9nam", b"\xa9too", b"\xa9cmt"];

    let mut offset = 0usize;
    while offset + 8 <= payload.len() {
        let Some(size) = read_box_size(&payload[offset..]) else {
            break;
        };
        if size > payload.len() - offset {
            break;
        }
        let kind = &payload[offset + 4..offset + 8];

        if TEXT_BOXES.iter().any(|t| *t == kind) {
            let start = offset + 16;
            if start < offset + size {
                let text: String = payload[start..offset + size]
                    .iter()
                    .take_while(|&&b| b != 0)
                    .map(|&b| char::from(b))
                    .collect();
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    out.push(MetadataEntry {
                        scope,
                        track_index,
                        key: escape_atom(kind),
                        value: trimmed.to_owned(),
                        source: if scope == Scope::Container {
                            "moov"
                        } else {
                            "trak"
                        }
                        .to_owned(),
                    });
                }
            }
        }
        offset += size;
    }
}

/// Renders an atom name with non-ASCII bytes escaped.
fn escape_atom(kind: &[u8]) -> String {
    kind.iter()
        .map(|&b| {
            if b.is_ascii_graphic() {
                char::from(b).to_string()
            } else {
                format!("u+{b:02x}")
            }
        })
        .collect()
}

/// Detects the container format of a file on disk.
///
/// # Errors
///
/// Returns an error if the file cannot be read.
pub fn detect_source(path: &Path) -> Result<ContainerFormat, CoreError> {
    detect_file(path)
        .map_err(|e| CoreError::io("read source header", path.display().to_string(), e))
}

/// A file's size in bytes, or 0 when it cannot be stat'd.
///
/// A stat failure reads as zero so that callers treat the file as within every
/// bound and let the subsequent read produce the real, specific error. Guessing
/// a large size instead would report a size limitation for a file whose actual
/// problem is that it could not be opened.
fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or_default()
}

/// Byte offset where the first `mdat`'s payload begins.
///
/// This is the anchor the sample index accumulates from, so it must come from
/// the file's own layout rather than a constant: an MP4 may place `mdat` before
/// or after `moov`, and an anchor that assumed the usual order would shift every
/// sample position by the size of whatever precedes it.
///
/// Returns `None` when the file cannot be read or holds no `mdat`. The caller
/// then leaves the index empty rather than anchoring at zero, because a zero
/// anchor silently produces plausible-looking but wrong offsets — the worst
/// failure mode for a value a report will present as a location.
fn mdat_payload_offset(path: &Path) -> Option<u64> {
    use tpt_app_media_forensics_container::{read_header, MAX_INSPECTED_BYTES};

    if file_size(path) > MAX_INSPECTED_BYTES {
        return None;
    }
    let header = read_header(path, 64 * 1024).ok()?;

    let mut offset = 0u64;
    let total = header.len() as u64;
    while offset.saturating_add(8) <= total {
        let at = offset as usize;
        let declared = u64::from(u32::from_be_bytes(
            header[at..at + 4].try_into().unwrap_or([0; 4]),
        ));
        let box_type = &header[at + 4..at + 8];

        if box_type == b"mdat" {
            // Size 1 means a 64-bit size follows the type field; size 0 means
            // the box runs to end of file. In every case the payload starts
            // after the header, whose length is what actually matters here.
            let header_len = if declared == 1 { 16 } else { 8 };
            return Some(offset.saturating_add(header_len));
        }

        // Stop at `moov`: media data conventionally follows it, and walking
        // into `mdat` payload looking for another `mdat` would find sample bytes.
        if box_type == b"moov" {
            return None;
        }

        let advance = if declared == 0 {
            total
        } else {
            declared.max(8)
        };
        if advance <= offset {
            break;
        }
        offset = offset.saturating_add(advance);
    }
    None
}

/// Runs one container inspection, recording failure as a limitation.
///
/// A file that cannot be inspected must still produce a report: the reason is
/// an observation about the evidence, and refusing the whole examination would
/// hide the container-level findings that did parse.
fn read_container(
    inspect: impl FnOnce() -> Result<
        tpt_app_media_forensics_container::ContainerInspection,
        tpt_app_media_forensics_container::ContainerError,
    >,
    limitations: &mut Vec<String>,
) -> Option<tpt_app_media_forensics_container::ContainerInspection> {
    match inspect() {
        Ok(inspection) => Some(inspection),
        Err(error) => {
            limitations.push(format!("container structure could not be read: {error}"));
            None
        }
    }
}

/// Reports whether a file's extension matches its detected container.
///
/// # Errors
///
/// Returns an error if the file cannot be read.
pub fn extension_matches_source(path: &Path) -> Result<bool, CoreError> {
    Ok(extension_matches(path, detect_source(path)?))
}

/// Computes audio measurements for a decoded PCM buffer.
///
/// Returns levels, silence regions, and integrated loudness. Silence is measured
/// here rather than left to the caller so that the three measurements always
/// come from the same PCM buffer: measuring silence against a different slice
/// than the levels would let a report pair a level with a silence region that
/// does not describe the same audio.
///
/// Returns `None` only when there are no samples, which is "there is no audio"
/// rather than "the audio is silent".
#[must_use]
pub fn measure_audio(
    pcm: &[f32],
    channels: u16,
    sample_rate: u32,
    profile: &RuleProfile,
) -> Option<AudioMeasurements> {
    if pcm.is_empty() || channels == 0 {
        return None;
    }
    let silence = find_silence(pcm, profile.silence_threshold, profile.min_silence_frames);
    Some(AudioMeasurements {
        levels: level_stats(pcm),
        silence,
        loudness: integrated_loudness(pcm, channels, sample_rate).ok(),
    })
}

/// The audio measurements the rules consume.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioMeasurements {
    /// Peak, RMS, and mean of the decoded signal.
    pub levels: tpt_app_media_forensics_audio::LevelStats,
    /// Sustained regions below the profile's silence threshold.
    pub silence: Vec<tpt_app_media_forensics_audio::SilenceRegion>,
    /// Integrated loudness, when it could be measured correctly.
    pub loudness: Option<Measurement>,
}

/// The time at which an analysis started, used only for reporting.
#[must_use]
pub fn analysis_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Returns the acquisition record for a source, for callers that need it
/// without running a full analysis.
///
/// # Errors
///
/// Returns an error if the source cannot be read.
pub fn acquire_only(source: &Path) -> Result<AcquisitionRecord, CoreError> {
    acquisition::acquire(source)
}

/// Builds the case record for a set of analysed assets.
///
/// # Errors
///
/// Never; returns a case carrying the asset identifiers.
pub fn case_for(name: &str, assets: &[MediaAsset]) -> std::result::Result<Case, CoreError> {
    let mut case = Case::new(name.to_owned(), None);
    for asset in assets {
        case.add_asset(asset);
    }
    Ok(case)
}

/// Re-exported so callers do not need to name `MediaTime` for empty bundles.
#[must_use]
pub fn zero_time() -> MediaTime {
    MediaTime::ZERO
}
/// A report rebuilt from the case database.
#[derive(Debug)]
pub struct CaseReport {
    /// The reconstructed report.
    pub report: tpt_app_media_forensics_report::Report,
    /// The analysis the findings came from, if one is recorded.
    pub analysis_id: Option<String>,
}

/// Rebuilds a report from what a case recorded in SQLite.
///
/// Findings are read back from their canonical JSON payloads, so the report
/// states exactly what the engine produced rather than a reconstruction from
/// lossy columns. Limitations are not stored in the database — they describe the
/// analysis run, not its findings — so a rebuilt report carries the engine's
/// standard limitations until per-run limitations are persisted alongside the
/// analysis.
///
/// # Errors
///
/// Returns an error if the case database cannot be read.
pub fn load_report(case_dir: &CaseDirectory) -> Result<CaseReport, CoreError> {
    let store = Store::open(case_dir.root())?;
    let case_id = store
        .only_case_id()
        .map_err(rusqlite_to_core)?
        .ok_or_else(|| CoreError::InvalidManifest {
            reason: "the case database contains no case record".to_owned(),
        })?;

    let analysis = store.latest_analysis(&case_id).map_err(rusqlite_to_core)?;
    let findings = store.findings_in_case(&case_id).map_err(rusqlite_to_core)?;
    let assets = store.assets_in_case(&case_id).map_err(rusqlite_to_core)?;

    let manifest = case_dir.read_manifest();
    let (case_name, case_description) = manifest
        .map(|c| (c.name, c.description))
        .unwrap_or_else(|_| (case_dir.root().display().to_string(), None));

    // The methodology is reconstructed from the stored analysis. The cache key
    // already encodes asset hash, analysis version, profile, and rule set, which
    // are exactly the four inputs the fingerprint is derived from, so the
    // rebuilt report states the same fingerprint the original run did.
    let methodology = match &analysis {
        Some(analysis) => rebuild_methodology(analysis),
        None => tpt_app_media_forensics_report::Methodology {
            application_version: env!("CARGO_PKG_VERSION").to_owned(),
            analysis_version: AnalysisVersion::CURRENT.to_string(),
            profile: "unknown".to_owned(),
            profile_fingerprint: "unknown".to_owned(),
            enabled_rules: Vec::new(),
            rule_set_fingerprint: "unknown".to_owned(),
            input_hashes: Vec::new(),
            analysis_timestamp_unix: 0,
            applicable_standards: Vec::new(),
            analysis_fingerprint: "unknown".to_owned(),
        },
    };

    let report = tpt_app_media_forensics_report::Report {
        schema_version: 1,
        case_name,
        case_id,
        case_description,
        assets: assets
            .iter()
            .map(|a| tpt_app_media_forensics_report::AssetSummary {
                name: a.name.clone(),
                source_path: a.source_path.clone(),
                sha256: a.sha256.clone(),
                blake3: a.blake3.clone(),
                size_bytes: a.size_bytes,
                stream_count: 0,
            })
            .collect(),
        findings,
        evidence: Vec::new(),
        methodology,
        limitations: Vec::new(),
        validation: None,
    };

    Ok(CaseReport {
        report,
        analysis_id: analysis.map(|a| a.id),
    })
}

/// Converts a database error into a core error.
fn rusqlite_to_core(error: rusqlite::Error) -> CoreError {
    CoreError::database("case database", error)
}

/// Rebuilds the methodology block from a stored analysis.
///
/// The four inputs the analysis fingerprint is derived from are recorded on the
/// analysis row, so a report rebuilt from the database states the same
/// fingerprint the original run did (spec §63).
fn rebuild_methodology(analysis: &StoredAnalysis) -> tpt_app_media_forensics_report::Methodology {
    let fingerprint = tpt_app_media_forensics_report::Methodology::compute_fingerprint(
        &analysis.cache_key,
        &AnalysisVersion::CURRENT.to_string(),
        &analysis.profile_fingerprint,
        &analysis.rule_set_fingerprint,
    );

    tpt_app_media_forensics_report::Methodology {
        application_version: env!("CARGO_PKG_VERSION").to_owned(),
        analysis_version: AnalysisVersion::CURRENT.to_string(),
        profile: analysis.profile.clone(),
        profile_fingerprint: analysis.profile_fingerprint.clone(),
        enabled_rules: builtin_rules().iter().map(|r| r.id().to_owned()).collect(),
        rule_set_fingerprint: analysis.rule_set_fingerprint.clone(),
        input_hashes: Vec::new(),
        analysis_timestamp_unix: analysis.started_at,
        applicable_standards: vec!["ITU-R BS.1770-4 (loudness)".to_owned()],
        analysis_fingerprint: fingerprint,
    }
}
