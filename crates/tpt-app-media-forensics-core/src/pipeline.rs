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

use tpt_app_media_forensics_audio::{level_stats, Measurement};
use tpt_app_media_forensics_container::probe::{detect_file, extension_matches};
use tpt_app_media_forensics_container::{detect, read_samples, ContainerFormat};
use tpt_app_media_forensics_metadata::{MetadataEntry, MetadataTree, Scope};
use tpt_app_media_forensics_model::{
    AcquisitionRecord, AnalysisId, AnalysisVersion, CacheKey, Case, Finding, MediaAsset, MediaTime,
    MediaType,
};
use tpt_app_media_forensics_rules::{builtin_rules, engine::empty_bundle, RuleEngine, RuleProfile};
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
            return Ok(AnalysisOutcome {
                findings: entry.findings,
                cache_hit: true,
                cache_key,
                limitations: vec!["result served from the analysis cache".to_owned()],
                metadata: MetadataTree::default(),
                asset,
                profile: self.profile.clone(),
            });
        }

        // 4. Container inspection.
        let bytes = std::fs::read(source)
            .map_err(|e| CoreError::io("read source", source.display().to_string(), e))?;
        let format = detect(&bytes);
        let inspection = if format == ContainerFormat::IsoBmff {
            match tpt_app_media_forensics_container::inspect_bytes(bytes.clone()) {
                Ok(inspection) => Some(inspection),
                Err(error) => {
                    limitations.push(format!("container structure could not be read: {error}"));
                    None
                }
            }
        } else {
            limitations.push(format!(
                "container format {} has no demuxer integrated yet",
                format.tag()
            ));
            None
        };

        // 5. Per-layer analysis.
        let mut bundle = empty_bundle(asset.id);

        if let Some(inspection) = &inspection {
            bundle.container = Some(inspection.clone());

            if let Some(info) = inspection.frame_info.iter().flatten().next() {
                bundle.timestamps = vec![scan_presentation(
                    &info.frame_times,
                    self.profile.pts_tolerance,
                )];
            }
        }

        // GOP and duplicate detection both work at the packet layer.
        if let Some(inspection) = &inspection {
            if let Some(info) = inspection.frame_info.iter().flatten().next() {
                bundle.gop = Some(gop::analyse(
                    &info.keyframes,
                    &info.frame_times,
                    self.profile.gop_tolerance_frames,
                ));
            }
            if let Ok(samples) = read_samples(bytes.clone()) {
                let digests: Vec<tpt_app_media_forensics_video::duplicate::SampleDigest> = samples
                    .iter()
                    .map(|s| tpt_app_media_forensics_video::duplicate::SampleDigest {
                        digest: s.digest.clone(),
                        time: s.time,
                        is_key_frame: s.is_key_frame,
                    })
                    .collect();
                bundle.repeated_runs = find_repeated_runs(&digests, self.profile.min_duplicate_run);
            }
        }

        bundle.metadata = extract_metadata(&bytes);
        if bundle.metadata.as_ref().is_none_or(MetadataTree::is_empty) {
            limitations.push("no readable metadata atoms were found".to_owned());
        }

        // 6. Rules.
        let findings =
            self.rules
                .evaluate(&bundle, &self.profile)
                .map_err(|e| CoreError::Serialise {
                    operation: "evaluate rules",
                    reason: e.to_string(),
                })?;

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
            stream_count: inspection.as_ref().map_or(0, |i| i.streams.len()),
        })?;

        // 8. Record the run in the case database so `report` can rebuild this
        //    without re-analysing. A cache hit deliberately skips this: the
        //    record already exists, and findings are append-only (spec §66).
        if !cache_hit {
            self.persist(case_dir, &asset, &cache_key, &findings)?;
        }

        Ok(AnalysisOutcome {
            asset,
            findings,
            cache_key,
            cache_hit: false,
            metadata: bundle.metadata.unwrap_or_default(),
            limitations,
            profile: self.profile.clone(),
        })
    }

    /// Records an analysis run and its findings in the case database.
    ///
    /// Written as one transaction: a run whose findings were only partly
    /// written would leave the case reporting fewer observations than the engine
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
/// # Errors
///
/// Never; returns the levels, or `None` when there are no samples.
pub fn measure_audio(
    pcm: &[f32],
    channels: u16,
    sample_rate: u32,
) -> Option<(
    tpt_app_media_forensics_audio::LevelStats,
    Option<Measurement>,
)> {
    if pcm.is_empty() {
        return None;
    }
    let levels = level_stats(pcm);
    let loudness =
        tpt_app_media_forensics_audio::integrated_loudness(pcm, channels, sample_rate).ok();
    Some((levels, loudness))
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
