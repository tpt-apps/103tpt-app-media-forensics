//! Command-line interface for TPT Media Forensics (spec §51).
//!
//! The CLI is a thin front end: every command delegates to
//! [`tpt_app_media_forensics_core`], the same engine the desktop GUI uses, so
//! results are identical whichever way a case is examined.
//!
//! Commands:
//!
//! * `hash`    — compute SHA-256 and BLAKE3 digests of a file
//! * `acquire` — record an acquisition manifest for a file
//! * `inspect` — print container and stream structure for a file
//! * `analyze` — run the analysis engine over an asset
//! * `report`  — generate a report from stored results
//! * `batch`   — analyse every media file in a directory
//!
//! # Offline by design
//!
//! No command performs network access. Analysis of an evidentiary file must
//! not depend on connectivity (spec §96).
//!
//! # Read-only sources
//!
//! Commands that touch source media open it read-only and say so. Nothing in
//! the CLI writes to a source file (spec §11).

mod output;

use std::process::ExitCode;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use tpt_app_media_forensics_container::{
    detect_file, extension_matches, inspect_file, read_samples, ContainerFormat, TrackFrameInfo,
};
use tpt_app_media_forensics_core::{acquire, CaseDirectory};
use tpt_app_media_forensics_metadata::{find_conflicts, MetadataEntry, Scope};
use tpt_app_media_forensics_model::{Case, MediaType};
use tpt_app_media_forensics_rules::builtin_rules;
use tpt_app_media_forensics_video::duplicate::{find_repeated_runs, SampleDigest};
use tpt_app_media_forensics_video::gop;

use crate::output::{render_acquisition, AcquisitionJson};

/// Professional media inspection, forensic analysis, and evidence preservation.
#[derive(Debug, Parser)]
#[command(name = "tpt-media-forensics", version, about, long_about = None)]
struct Cli {
    /// Emit machine-readable JSON instead of human-readable text.
    #[arg(long, global = true)]
    json: bool,

    /// Increase log verbosity (repeat for more detail).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

/// The available subcommands.
#[derive(Debug, Subcommand)]
enum Command {
    /// Compute SHA-256 and BLAKE3 digests of a file.
    Hash {
        /// Path to the file. Opened read-only.
        path: std::path::PathBuf,
    },

    /// Create a case directory and record a source file within it.
    Acquire {
        /// Path to the media file. Opened read-only.
        path: std::path::PathBuf,
        /// Case name.
        #[arg(long)]
        name: String,
        /// Directory to create the case in. A `case.tptcase` folder is added.
        #[arg(long)]
        parent: std::path::PathBuf,
    },
    /// Analyse a raw audio file: levels, silence, DC offset, and loudness.
    Audio {
        /// Path to the audio file. Opened read-only.
        path: std::path::PathBuf,
    },

    /// Extract metadata and cross-check it for consistency.
    Metadata {
        /// Path to the media file. Opened read-only.
        path: std::path::PathBuf,
    },

    /// Print container and stream structure for a media file.
    Inspect {
        /// Path to the media file. Opened read-only.
        path: std::path::PathBuf,
    },

    /// Run the analysis engine over a media file.
    Analyze {
        /// Path to the media file. Opened read-only.
        path: std::path::PathBuf,
        /// Case directory to write results and evidence into.
        #[arg(long)]
        case_dir: std::path::PathBuf,
    },

    /// Generate a report from a previously analysed case.
    Report {
        /// Case directory produced by `acquire` or `analyze`.
        #[arg(long)]
        case_dir: std::path::PathBuf,
        /// Output file path. The format is inferred from the extension.
        #[arg(long)]
        out: std::path::PathBuf,
    },

    /// Analyse every media file found beneath a directory.
    Batch {
        /// Directory to scan recursively.
        directory: std::path::PathBuf,
        /// Case directory to write results and evidence into.
        #[arg(long)]
        case_dir: std::path::PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Log level is derived from repeated -v flags, never from an environment
    // variable that an unrelated process could have set.
    let level = match cli.verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    tracing_subscriber::fmt()
        .with_env_filter(level)
        .with_writer(std::io::stderr)
        .init();

    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // `{error:#}` prints the full anyhow cause chain, so the analyst
            // sees which path failed rather than a generic message.
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Dispatches a parsed command to the engine.
///
/// # Errors
///
/// Returns a descriptive error when the input cannot be read or the case
/// directory cannot be written. Every path in a batch failure is labelled so
/// the analyst knows which file caused it.
fn run(cli: &Cli) -> anyhow::Result<()> {
    match &cli.command {
        Command::Hash { path } => {
            let record = acquire(path)?;
            emit(
                cli.json,
                &AcquisitionJson::from(&record),
                &render_acquisition(&record),
            );
            Ok(())
        }

        Command::Acquire { path, name, parent } => {
            let record = acquire(path)?;
            let case = Case::new(name.clone(), None);
            let root = parent.join("case.tptcase");
            let dir = CaseDirectory::create(&root, &case)?;

            let asset = tpt_app_media_forensics_core::acquire_asset(path, MediaType::Container)?;
            let mut case = case;
            case.add_asset(&asset);
            dir.write_manifest(&case)?;

            if cli.json {
                let value = serde_json::json!({
                    "case": case.id.to_string(),
                    "case_dir": dir.root(),
                    "asset": AcquisitionJson::from(&record),
                });
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                println!("Case          {name}");
                println!("Case ID       {}", case.id);
                println!("Case directory {}", dir.root().display());
                println!();
                print!("{}", render_acquisition(&record));
            }
            Ok(())
        }

        Command::Audio { path } => audio_report(path, cli.json),
        Command::Metadata { path } => metadata_report(path, cli.json),
        Command::Inspect { path } => inspect(path, cli.json),

        Command::Analyze { path, case_dir } => analyse(path, case_dir, cli.json),
        Command::Report { case_dir, out } => generate_report(case_dir, out),

        Command::Batch {
            directory,
            case_dir,
        } => run_batch(directory, case_dir, cli.json),
    }
}

/// Default GOP-length tolerance, in frames.
///
/// Placeholder until rule profiles (spec §37) carry per-profile tolerances. The
/// value is deliberately visible and documented rather than buried in a rule.
const DEFAULT_GOP_TOLERANCE_FRAMES: u32 = 5;

/// Shortest repeated run worth reporting; a single repeat is indistinguishable
/// from ordinary static-scene encoding.
const DEFAULT_MIN_DUPLICATE_RUN: u32 = 2;

/// Cap on duplicate runs printed to the console.
const MAX_REPORTED_RUNS: usize = 20;

/// Inspects a media file's container structure.
///
/// Reports what the file *is* (by signature), then what each stream declares
/// about itself. The declared format is compared against the file extension,
/// because a mismatch is a finding rather than an error.
///
/// # Errors
///
/// Returns an error if the file cannot be read or the container cannot be
/// parsed. Damaged tracks are reported as anomalies, not errors.
fn inspect(path: &std::path::Path, json: bool) -> anyhow::Result<()> {
    let format = detect_file(path).with_context(|| format!("cannot read {}", path.display()))?;
    let extension_ok = extension_matches(path, format);
    let mut frame_info: Vec<Option<TrackFrameInfo>> = Vec::new();
    let mut repeated_runs = String::new();

    let streams = match format {
        ContainerFormat::IsoBmff => {
            let inspection =
                inspect_file(path).with_context(|| format!("cannot inspect {}", path.display()))?;
            for anomaly in &inspection.anomalies {
                eprintln!("anomaly: {anomaly}");
            }
            frame_info = inspection.frame_info.clone();
            repeated_runs = report_duplicate_runs(path);
            inspection.streams
        }
        // An unrecognised signature is a finding about the evidence, not a gap
        // in the tool. Conflating the two would tell an analyst their file is
        // fine once a format is added, when in fact it is not a container.
        ContainerFormat::Unknown => {
            anyhow::bail!(
                "{} is not a recognised media container (no known signature match)",
                path.display()
            );
        }
        other => {
            // A known format whose demuxer is not integrated yet. Naming the
            // format keeps the result honest about what the file actually is.
            eprintln!(
                "inspect: identified as {}; deep parsing for this format is not \
                 integrated yet",
                other.tag()
            );
            Vec::new()
        }
    };

    if !extension_ok {
        eprintln!(
            "finding: extension does not match detected container ({})",
            format.tag()
        );
    }

    if json {
        let value = serde_json::json!({
            "source_path": path.display().to_string(),
            "container": format.tag(),
            "extension_matches_container": extension_ok,
            "stream_count": streams.len(),
            "streams": streams,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("Source        {}", path.display());
        println!("Container     {}", format.tag());
        println!(
            "Extension     {}",
            if extension_ok {
                "matches"
            } else {
                "DOES NOT MATCH"
            }
        );
        println!("Streams       {}", streams.len());
        if !repeated_runs.is_empty() {
            println!("{}", repeated_runs.trim_end());
        }
        for stream in &streams {
            let codec = stream.codec.long_name.as_deref().unwrap_or("unrecognised");
            println!(
                "  [{}] {}  {}  {}",
                stream.index,
                stream.kind.tag(),
                stream.codec.name,
                codec
            );
            if let Some(video) = stream.video_format() {
                println!(
                    "        {}x{}  frame rate: {}",
                    video.coded_width,
                    video.coded_height,
                    video
                        .frame_rate
                        .map_or_else(|| "not measurable".to_owned(), |r| r.to_string())
                );
            }
            println!("        timescale: {}", stream.timing.timebase);
            println!("        samples:   {}", stream.packet_count.unwrap_or(0));

            // GOP structure needs no decoding: it comes from the container's
            // sync-sample table (spec §15).
            if stream.kind.tag() == "video" {
                if let Some(Some(info)) = frame_info.get(stream.index as usize) {
                    let report = gop::analyse(
                        &info.keyframes,
                        &info.frame_times,
                        DEFAULT_GOP_TOLERANCE_FRAMES,
                    );
                    if info.all_frames_are_keyframes {
                        println!("        GOP:       every frame is a keyframe (no stss box)");
                    } else {
                        println!(
                            "        GOP:       {} keyframes, dominant length {} frames",
                            report.keyframe_count, report.dominant_length
                        );
                        if report.is_uniform() {
                            println!("        GOP:       uniform structure");
                        } else {
                            for change in &report.changes {
                                println!(
                                    "        finding:   GOP length {} -> {} frames at {}",
                                    change.expected_length, change.observed_length, change.at
                                );
                            }
                        }
                    }
                }
            }
        }
        println!("Source was opened read-only; it has not been modified.");
    }
    Ok(())
}

/// Emits a result in the requested format.
///
/// If JSON rendering fails the human-readable form is printed instead: failing
/// to serialise the engine's own output is a bug the analyst cannot act on,
/// and emitting nothing would hide a successful acquisition entirely.
fn emit<T: serde::Serialize>(json: bool, value: &T, text: &str) {
    if json {
        match serde_json::to_string_pretty(value) {
            Ok(encoded) => println!("{encoded}"),
            Err(error) => {
                eprintln!("warning: could not render JSON ({error}); using text");
                print!("{text}");
            }
        }
    } else {
        print!("{text}");
    }
}

/// Reports repeated-sample runs detected at the packet layer (spec §17).
///
/// Reads every access unit and compares compressed digests. No decoding, so
/// this works even when the video stream itself cannot be decoded.
///
/// Returns rendered text rather than printing, so the caller controls where it
/// appears in the report.
fn report_duplicate_runs(path: &std::path::Path) -> String {
    let Ok(samples) = read_samples(std::fs::read(path).unwrap_or_default()) else {
        return String::new();
    };

    let records: Vec<SampleDigest> = samples
        .iter()
        .map(|s| SampleDigest {
            digest: s.digest.clone(),
            time: s.time,
            is_key_frame: s.is_key_frame,
        })
        .collect();

    let runs = find_repeated_runs(&records, DEFAULT_MIN_DUPLICATE_RUN);
    if runs.is_empty() {
        return String::new();
    }

    let mut out = format!(
        "Repeated runs   {} (compressed-sample comparison, no decoding)\n",
        runs.len()
    );
    for run in runs.iter().take(MAX_REPORTED_RUNS) {
        out.push_str(&format!(
            "    {} - {}  {} frames\n        {}\n",
            run.start_time,
            run.end_time,
            run.length,
            run.soundness.explanation()
        ));
    }
    if runs.len() > MAX_REPORTED_RUNS {
        out.push_str(&format!(
            "    ... {} more runs not shown\n",
            runs.len() - MAX_REPORTED_RUNS
        ));
    }
    out
}
/// Analyses a raw audio file (spec §19-§22).
///
/// Decodes to PCM via `tpt-av-cadence`, then reports levels, silence regions,
/// DC offset, and integrated loudness. Every figure is printed with the
/// methodology that produced it, and a measurement that cannot be taken
/// correctly is reported as unavailable rather than approximated (spec §21).
fn audio_report(path: &std::path::Path, json: bool) -> anyhow::Result<()> {
    use tpt_app_media_forensics_audio::{
        amplitude_to_dbfs, find_silence, integrated_loudness, level_stats, Measurement, Methodology,
    };
    let file = std::fs::File::open(path)?;
    use tpt_av_cadence_core::FormatReader as _;
    let source = Box::new(std::io::BufReader::new(file));
    let mut reader = tpt_av_cadence_wav::WavReader::open(source)
        .with_context(|| format!("cannot parse WAV: {}", path.display()))?;

    let sample_rate = reader.info().sample_rate;
    let channels = reader.info().channels;

    // Decode into a bounded buffer; refuse to silently truncate a long file.
    let mut pcm: Vec<f32> = Vec::new();
    let mut block = vec![0.0f32; 8192];
    while let Ok(read) = reader.decoder().decode(&mut block) {
        if read == 0 {
            break;
        }
        pcm.truncate(pcm.len() + read);
        pcm.extend_from_slice(&block[..read]);
        if pcm.len() > MAX_AUDIO_SAMPLES {
            pcm.truncate(MAX_AUDIO_SAMPLES);
            eprintln!(
                "audio: truncated to {} samples; loudness covers a prefix only",
                MAX_AUDIO_SAMPLES
            );
            break;
        }
    }

    let stats = level_stats(&pcm);
    let silence = find_silence(&pcm, SILENCE_THRESHOLD, MIN_SILENCE_FRAMES);
    let loudness = integrated_loudness(&pcm, channels, sample_rate);

    // Peak and RMS are reported as levels *and* as normalised amplitudes: the
    // two are different quantities, and printing an amplitude with a "dBFS"
    // unit would misstate the measurement.
    let peak_amplitude = Measurement::new(stats.peak, Methodology::SampleAmplitude);
    let peak_level = amplitude_to_dbfs(stats.peak).map(|v| Measurement::new(v, Methodology::Dbfs));
    let rms_level = amplitude_to_dbfs(stats.rms).map(|v| Measurement::new(v, Methodology::Dbfs));
    let dc = Measurement::new(stats.mean, Methodology::SampleMean);

    if json {
        let value = serde_json::json!({
            "source_path": path.display().to_string(),
            "sample_rate": sample_rate,
            "channels": channels,
            "frames": stats.sample_count,
            "peak_amplitude": peak_amplitude,
            "peak_dbfs": peak_level,
            "rms_dbfs": rms_level,
            "dc_offset": dc,
            "silence_regions": silence.len(),
            "integrated_loudness": loudness.as_ref().ok().map(ToString::to_string),
            "loudness_unavailable_reason": loudness.as_ref().err().map(ToString::to_string),
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("Source        {}", path.display());
        println!("Sample rate   {sample_rate} Hz");
        println!("Channels      {channels}");
        println!("Frames        {}", stats.sample_count);
        println!("Peak          {}", peak_amplitude.describe());
        match &peak_level {
            Some(level) => println!("Peak level    {}", level.describe()),
            None => println!("Peak level    digital silence; no decibel value exists"),
        }
        match &rms_level {
            Some(level) => println!("RMS level     {}", level.describe()),
            None => println!("RMS level     digital silence; no decibel value exists"),
        }
        println!("DC offset     {}", dc.describe());
        println!(
            "Silence       {} region(s) below {} over {} frames",
            silence.len(),
            SILENCE_THRESHOLD,
            MIN_SILENCE_FRAMES
        );
        match &loudness {
            Ok(measured) => println!("Loudness      {}", measured.describe()),
            Err(reason) => println!(
                "Loudness      NOT MEASURED: {reason}\n\
                               No figure is reported rather than one computed with a method the\n\
                               standard does not define for this sample rate."
            ),
        }
        println!("Source was opened read-only; it has not been modified.");
    }
    Ok(())
}

/// Largest number of samples decoded for one analysis pass.
const MAX_AUDIO_SAMPLES: usize = 200_000_000;

/// Peak amplitude below which a sample counts as silent.
const SILENCE_THRESHOLD: f64 = 0.001;

/// Shortest run that counts as a silence region.
const MIN_SILENCE_FRAMES: u64 = 1_000;

/// Extracts metadata and cross-checks it for consistency (spec §25, §26).
///
/// Entries retain the scope and source element they came from, because a
/// conflict is only visible when both competing values survive.
fn metadata_report(path: &std::path::Path, json: bool) -> anyhow::Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let entries = extract_metadata(&bytes);
    let tree = tpt_app_media_forensics_metadata::MetadataTree::new(entries);
    let conflicts = find_conflicts(&tree);

    if json {
        let value = serde_json::json!({
            "source_path": path.display().to_string(),
            "entry_count": tree.len(),
            "entries": tree.entries,
            "conflicts": conflicts,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("Source        {}", path.display());
        println!("Entries       {}", tree.len());
        for entry in &tree.entries {
            let track = entry
                .track_index
                .map_or_else(String::new, |i| format!("[track {i}] "));
            println!(
                "  {:<9} {}{:<16} = {}  ({})",
                entry.scope.tag(),
                track,
                entry.key,
                entry.value,
                entry.source
            );
        }
        println!("Conflicts     {}", conflicts.len());
        for conflict in &conflicts {
            println!("  finding: {}", conflict.describe());
        }
        if conflicts.is_empty() {
            println!("  no cross-scope inconsistencies found");
        }
        println!("Source was opened read-only; it has not been modified.");
    }

    Ok(())
}

/// Reads top-level metadata atoms from a container (spec §25).
///
/// Deliberately conservative: it reads the well-known free-text atoms without
/// claiming to fully parse every box. Values it cannot interpret are omitted
/// rather than invented, so the report never asserts something the file did not
/// state.
fn extract_metadata(bytes: &[u8]) -> Vec<MetadataEntry> {
    let mut entries = Vec::new();
    let mut offset = 0usize;

    while offset + 8 <= bytes.len() {
        let size = read_box_size(&bytes[offset..]);
        let Some(size) = size else { break };
        if size > bytes.len() - offset {
            break;
        }
        let kind = &bytes[offset + 4..offset + 8];
        let payload = &bytes[offset + 8..offset + size];

        if kind == b"moov" {
            collect_free_text(payload, Scope::Container, None, &mut entries);

            // Track-scoped atoms live inside `trak`, one box per track. The
            // index is what makes two tracks' same-key values distinguishable.
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
                    collect_free_text(
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

    entries
}

/// Reads a box size, rejecting the degenerate forms.
fn read_box_size(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 8 {
        return None;
    }
    let size = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    // A size below 8 cannot describe a box, and size 0 means "to end of file",
    // which we do not accept: it makes an off-by-one unbounded.
    (size >= 8).then_some(size)
}

/// Reads text atoms from a box payload.
fn collect_free_text(
    payload: &[u8],
    scope: Scope,
    track_index: Option<u32>,
    entries: &mut Vec<MetadataEntry>,
) {
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
            // The value begins after a 4-byte version/flags field and a
            // 4-byte locale field.
            let value_start = offset + 16;
            if value_start < offset + size {
                let raw = &payload[value_start..offset + size];
                let text: String = raw
                    .iter()
                    .take_while(|&&b| b != 0)
                    .map(|&b| char::from(b))
                    .collect();
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    entries.push(MetadataEntry {
                        scope,
                        track_index,
                        key: escape_atom(kind),
                        value: trimmed.to_owned(),
                        source: if scope == Scope::Container {
                            "moov".to_owned()
                        } else {
                            "trak".to_owned()
                        },
                    });
                }
            }
        }
        offset += size;
    }
}

/// Renders a four-character atom name for display.
///
/// MP4 metadata atoms begin with `0xA9` and render as `©` followed by the
/// key. Printing the raw byte makes the key illegible in a terminal, so the
/// copyright sign is written as `u+a9` and the result stays pure ASCII — which
/// also keeps the output stable across encodings.
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

/// Analyses a media file and writes the result into a case (spec §97).
///
/// Runs the same engine the desktop app uses, so a finding means the same
/// thing however it was reached (spec §51). The source is opened read-only.
fn analyse(path: &std::path::Path, case_dir: &std::path::Path, json: bool) -> anyhow::Result<()> {
    use tpt_app_media_forensics_core::{AnalysisEngine, CaseDirectory};
    use tpt_app_media_forensics_report::{write_bundle, AssetSummary, Methodology, Report};

    let directory = CaseDirectory::open(case_dir)
        .with_context(|| format!("{} is not an initialised case", case_dir.display()))?;

    let engine = AnalysisEngine::new();
    let outcome = engine.analyse(path, &directory)?;

    let profile = &outcome.profile;
    let fingerprint = engine.analysis_fingerprint(&outcome.cache_key);

    let methodology = Methodology {
        application_version: env!("CARGO_PKG_VERSION").to_owned(),
        analysis_version: outcome.cache_key.analysis_version.to_string(),
        profile: profile.identifier(),
        profile_fingerprint: outcome.cache_key.profile.as_hex().to_owned(),
        enabled_rules: builtin_rules().iter().map(|r| r.id().to_owned()).collect(),
        rule_set_fingerprint: outcome.cache_key.rules.as_hex().to_owned(),
        input_hashes: outcome
            .asset
            .sha256()
            .map(|h| vec![(outcome.asset.name.clone(), h.to_owned())])
            .unwrap_or_default(),
        analysis_timestamp_unix: tpt_app_media_forensics_core::pipeline::analysis_timestamp(),
        applicable_standards: vec!["ITU-R BS.1770-4 (loudness)".to_owned()],
        analysis_fingerprint: fingerprint.clone(),
    };

    let report = Report {
        schema_version: 1,
        case_name: directory_manifest_name(&directory),
        case_id: directory.root().display().to_string(),
        case_description: None,
        assets: vec![AssetSummary {
            name: outcome.asset.name.clone(),
            source_path: outcome.asset.acquisition.source_path.clone(),
            sha256: outcome.asset.sha256().map(ToOwned::to_owned),
            blake3: outcome.asset.blake3().map(ToOwned::to_owned),
            size_bytes: outcome.asset.size_bytes(),
            stream_count: outcome.asset.acquisition.size_bytes as usize,
        }],
        findings: outcome.findings.clone(),
        evidence: Vec::new(),
        limitations: outcome.limitations.clone(),
        methodology,
        validation: None,
    };

    let bundle_dir = directory.root().join("reports");
    let manifest = write_bundle(&report, &bundle_dir)?;

    if json {
        let value = serde_json::json!({
            "source_path": path.display().to_string(),
            "sha256": outcome.asset.sha256(),
            "blake3": outcome.asset.blake3(),
            "cache_hit": outcome.cache_hit,
            "analysis_fingerprint": fingerprint,
            "finding_count": outcome.findings.len(),
            "findings": outcome.findings,
            "limitations": outcome.limitations,
            "bundle_files": manifest.files,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("Source        {}", path.display());
        println!(
            "SHA-256       {}",
            outcome.asset.sha256().unwrap_or("(not computed)")
        );
        println!(
            "BLAKE3        {}",
            outcome.asset.blake3().unwrap_or("(not computed)")
        );
        println!(
            "Result        {}",
            if outcome.cache_hit {
                "served from the analysis cache"
            } else {
                "analysed"
            }
        );
        println!("Findings      {}", outcome.findings.len());
        for finding in &outcome.findings {
            println!(
                "  [{}] {}  {}",
                finding.severity.tag(),
                finding.rule_id,
                finding.observation.summary
            );
        }
        if !outcome.limitations.is_empty() {
            println!("Limitations");
            for limitation in &outcome.limitations {
                println!("  - {limitation}");
            }
        }
        println!("Fingerprint   {fingerprint}");
        println!("Bundle        {}", bundle_dir.display());
        for entry in &manifest.files {
            println!("  {}  sha256 {}", entry.name, entry.sha256);
        }
        println!("Source was opened read-only; it has not been modified.");
    }

    Ok(())
}

/// Reads the case name from its manifest, falling back to the directory name.
fn directory_manifest_name(directory: &CaseDirectory) -> String {
    directory
        .read_manifest()
        .map(|m| m.name)
        .unwrap_or_else(|_| {
            directory
                .root()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "case".to_owned())
        })
}

/// Renders a report from a previously analysed case.
///
/// The format is inferred from the output extension, so `report --out r.pdf`
/// produces a PDF and `--out r.html` produces HTML. This reads what the engine
/// recorded; it never re-analyses, because a report built from stored findings
/// states what was actually observed at the time.
fn generate_report(case_dir: &std::path::Path, out: &std::path::Path) -> anyhow::Result<()> {
    use tpt_app_media_forensics_core::pipeline::load_report;
    use tpt_app_media_forensics_report::{
        asset_hashes_to_csv, findings_to_csv, measurements_to_csv, to_html, to_json, to_pdf,
        write_bundle,
    };

    let directory = CaseDirectory::open(case_dir)
        .with_context(|| format!("{} is not an initialised case", case_dir.display()))?;
    let loaded = load_report(&directory)?;
    let report = &loaded.report;

    let bytes = match out
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("json") => write_bytes(out, to_json(report)?.into_bytes())?,
        Some("html") | Some("htm") => write_bytes(out, to_html(report).into_bytes())?,
        Some("csv") => write_bytes(out, findings_to_csv(report).into_bytes())?,
        Some("pdf") => write_bytes(out, to_pdf(report)?)?,
        Some("bundle") => {
            // A directory bundle: every deliverable plus a verifying manifest.
            let manifest = write_bundle(report, out)?;
            println!("Bundle        {}", out.display());
            for entry in &manifest.files {
                println!("  {}  sha256 {}", entry.name, entry.sha256);
            }
            println!(
                "Measurements  {}",
                measurements_to_csv(report)
                    .lines()
                    .count()
                    .saturating_sub(1)
            );
            println!(
                "Asset hashes  {}",
                asset_hashes_to_csv(report)
                    .lines()
                    .count()
                    .saturating_sub(1)
            );
            return Ok(());
        }
        other => anyhow::bail!(
            "cannot infer a report format from {}; use .json, .html, .csv, .pdf, or .bundle",
            other.unwrap_or("(no extension)")
        ),
    };

    use sha2::Digest as _;
    let hash = tpt_app_media_forensics_model::asset::to_hex(&sha2::Sha256::digest(&bytes));
    println!("Report        {}", out.display());
    println!(
        "Format        {}",
        out.extension().and_then(|e| e.to_str()).unwrap_or("?")
    );
    println!("Findings      {}", report.finding_count());
    println!("Bytes         {}", bytes.len());
    println!("SHA-256       {hash}");
    println!("Fingerprint   {}", report.methodology.analysis_fingerprint);
    if !report.limitations.is_empty() {
        println!("Limitations");
        for limitation in &report.limitations {
            println!("  - {limitation}");
        }
    }
    Ok(())
}
/// Writes bytes to `path`, returning them for hashing.
fn write_bytes(path: &std::path::Path, bytes: Vec<u8>) -> std::io::Result<Vec<u8>> {
    std::fs::write(path, &bytes)?;
    Ok(bytes)
}

/// Analyses every media file beneath a directory into one case.
///
/// Each file is reported individually, so a corrupt item is visible as a
/// labelled failure rather than silently dropping out of a total. The exit code
/// reflects whether any file could not be analysed, so a batch over a folder with
/// one bad file still fails loudly for a scripted caller.
fn run_batch(
    directory: &std::path::Path,
    case_dir: &std::path::Path,
    json: bool,
) -> anyhow::Result<()> {
    use tpt_app_media_forensics_core::batch::{self, FileOutcome};
    use tpt_app_media_forensics_core::{AnalysisEngine, CaseDirectory};
    use tpt_app_media_forensics_report::write_bundle;

    if !directory.is_dir() {
        anyhow::bail!("{} is not a directory", directory.display());
    }
    let case = CaseDirectory::open(case_dir)
        .with_context(|| format!("{} is not an initialised case", case_dir.display()))?;

    let outcome = batch::run(&AnalysisEngine::new(), directory, &case)?;
    let (analysed, failed, skipped) = outcome.counts();

    // Register the run as one report over every file in the case.
    let loaded = tpt_app_media_forensics_core::pipeline::load_report(&case)?;
    let bundle = write_bundle(&loaded.report, &case.reports_dir())?;

    if json {
        let files: Vec<serde_json::Value> = outcome
            .results
            .iter()
            .map(|(path, outcome)| {
                serde_json::json!({
                    "path": path.display().to_string(),
                    "status": match outcome {
                        FileOutcome::Analysed(_) => "analysed",
                        FileOutcome::Failed { .. } => "failed",
                        FileOutcome::Skipped { .. } => "skipped",
                    },
                    "reason": match outcome {
                        FileOutcome::Analysed(_) => serde_json::Value::Null,
                        FileOutcome::Failed { reason } | FileOutcome::Skipped { reason } => {
                            serde_json::Value::String(reason.clone())
                        }
                    },
                    "finding_count": outcome.findings().len(),
                    "cache_hit": matches!(
                        outcome,
                        FileOutcome::Analysed(o) if o.cache_hit
                    ),
                })
            })
            .collect();

        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "directory": directory.display().to_string(),
                "case_dir": case.root().display().to_string(),
                "analysed": analysed,
                "failed": failed,
                "skipped": skipped,
                "finding_count": loaded.report.finding_count(),
                "analysis_fingerprint": loaded.report.methodology.analysis_fingerprint,
                "bundle_files": bundle.files,
                "files": files,
            }))?
        );
    } else {
        println!("Directory      {}", directory.display());
        println!("Case           {}", case.root().display());
        println!();
        for (path, outcome) in &outcome.results {
            match outcome {
                FileOutcome::Analysed(o) => {
                    println!(
                        "  {}  {} findings{}",
                        path.display(),
                        o.findings.len(),
                        if o.cache_hit { "  (cached)" } else { "" }
                    );
                }
                FileOutcome::Failed { reason } => println!("  {reason}"),
                FileOutcome::Skipped { reason } => {
                    println!("  {}  skipped: {reason}", path.display());
                }
            }
        }
        println!();
        println!("Analysed {analysed}   Failed {failed}   Skipped {skipped}");
        println!("Findings {}", loaded.report.finding_count());
        println!(
            "Fingerprint {}",
            loaded.report.methodology.analysis_fingerprint
        );
        println!("Bundle        {}", case.reports_dir().display());
        for entry in &bundle.files {
            println!("  {}  sha256 {}", entry.name, entry.sha256);
        }
        if failed > 0 {
            println!();
            println!("{failed} file(s) could not be analysed; see the case database.");
        }
    }

    // A batch that could not read some of its input has not fully succeeded.
    if failed > 0 {
        std::process::exit(2);
    }
    Ok(())
}
