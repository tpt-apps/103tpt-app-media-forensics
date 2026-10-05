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
//! * `compare` — compare two files across every measured axis
//! * `search`  — search a case's findings, assets, and evidence
//! * `report`  — generate a report from stored results
//! * `note`    — record an analyst note against a case, asset, or finding
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
    detect_file, extension_matches, inspect_file, inspect_matroska_file, read_matroska_samples,
    read_samples, ContainerFormat, TrackFrameInfo,
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

    /// Record an analyst note against a case, asset, or finding.
    Note {
        /// Case directory produced by `acquire` or `analyze`.
        #[arg(long)]
        case_dir: std::path::PathBuf,
        /// The note text. Read from stdin when omitted or given as `-`, so a
        /// multi-paragraph note does not have to survive shell quoting.
        #[arg(long, default_value = "-")]
        body: String,
        /// Kind of subject this note is about, e.g. `asset` or `finding`.
        ///
        /// Must be given together with `--subject`; neither alone is accepted.
        #[arg(long)]
        subject_kind: Option<String>,
        /// Identifier of the subject, e.g. an asset id.
        #[arg(long)]
        subject: Option<String>,
    },

    /// Designate, clear, or list a case's reference asset (spec §67).
    ///
    /// "This file is the master" is a decision somebody made about a file.
    /// Nothing in the bytes can recover it, so it is recorded against the asset
    /// rather than derived — the engine can guess which of two encodes is
    /// authoritative, and which one is authoritative is a question about the job,
    /// not the media.
    Reference {
        /// Case directory produced by `acquire` or `analyze`.
        #[arg(long)]
        case_dir: std::path::PathBuf,

        /// The asset to designate, by file name or asset id.
        ///
        /// Omit to list the current designations instead of changing them.
        asset: Option<String>,

        /// Clear this asset's designation rather than set it.
        ///
        /// Takes the asset, unlike most clear-flags: "clear *which* one" is a
        /// question with no default, and clearing every designation because nobody
        /// named one would be a destructive action taken by omission.
        #[arg(long)]
        clear: bool,
    },

    /// Compare two media files across every measured axis (spec §38–40).
    ///
    /// Both files are analysed directly. No case directory is involved: a
    /// comparison is a read-only question about two files, and writing an
    /// analysis record for each would put evidence in the case that nobody
    /// examined.
    ///
    /// With `--reference`, the **first** file is treated as a declared master: it
    /// is hashed, and the digest travels with the result. That is what turns
    /// "these two files differ" into spec §67's "what changed since the master".
    Compare {
        /// The first file. Opened read-only.
        ///
        /// The declared reference when `--reference` is given.
        left: std::path::PathBuf,

        /// The second file. Opened read-only.
        right: std::path::PathBuf,

        /// Treat the first file as a declared reference for this comparison.
        ///
        /// Its SHA-256 is computed from the bytes and recorded with the result, so
        /// the answer stays bound to these exact bytes even after the file is
        /// renamed or replaced. The output also relabels the two sides as
        /// "Reference" and "Delivery".
        #[arg(long)]
        reference: bool,
    },

    /// Search a case's findings, assets, and evidence (spec §41).
    Search {
        /// Case directory produced by `acquire` or `analyze`.
        #[arg(long)]
        case_dir: std::path::PathBuf,

        /// Text to look for. Omitted lists everything in scope.
        term: Option<String>,

        /// Which records to search.
        #[arg(long, value_enum, default_value = "all")]
        scope: SearchScopeArg,

        /// Only findings at this severity or above.
        #[arg(long, value_enum)]
        min_severity: Option<SeverityArg>,

        /// Maximum rows to return.
        #[arg(long)]
        limit: Option<usize>,
    },

    /// Report whether a file meets a delivery specification (spec §68, §69, §95).
    ///
    /// Two ways in, because a QC pass and a forensic review start from different
    /// places:
    ///
    /// * a file plus `--profile` — spec §95's invocation. The file is inspected
    ///   read-only and nothing is written.
    /// * `--case-dir` with no profile — the verdict over findings already
    ///   recorded, which is what the previous revision of this command did and
    ///   what a reviewer reaches for when auditing a past case.
    ///
    /// The verdict is always derived by `ValidationResult`, which is the one
    /// place that decides what blocks a delivery. Nothing here restates that
    /// rule: a second copy in the command layer would be free to disagree with
    /// the one the report renders.
    ///
    /// Exits 2 on `FAIL` so the command composes in a pipeline. A delivery gate
    /// that always exits 0 is not a gate.
    Validate {
        /// File to validate. Opened read-only. Mutually exclusive with
        /// `--case-dir`.
        path: Option<std::path::PathBuf>,

        /// Case directory produced by `analyze`.
        #[arg(long, conflicts_with = "path")]
        case_dir: Option<std::path::PathBuf>,

        /// Delivery profile to check the file against (spec §68, §69).
        ///
        /// Required with a file. Without it, a case directory is validated
        /// against its findings instead — which is a different, weaker claim,
        /// and one the output says so.
        #[arg(long, value_name = "PROFILE")]
        profile: Option<std::path::PathBuf>,

        /// Also write the verdict into a report bundle.
        ///
        /// Off by default: a verdict is a claim about delivery, and silently
        /// adding one to an existing report bundle would change a record the
        /// analyst has not asked to change.
        #[arg(long)]
        write: bool,
    },

    /// Inspect, list, and render delivery profiles (spec §69, §70).
    ///
    /// Profiles are data, not code: a customer writes one, ships it with the
    /// job, and the exact version used travels with the report.
    Profile {
        /// What to do with profiles.
        #[command(subcommand)]
        action: ProfileAction,
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

/// What `profile` does.
#[derive(Debug, Clone, Subcommand)]
enum ProfileAction {
    /// Show a profile's identity, fingerprint, and requirements.
    Show {
        /// Profile file to read.
        path: std::path::PathBuf,
    },

    /// Check that a profile file parses, without validating any media.
    ///
    /// Exists because a hand-written profile (spec §69) that fails to load is
    /// otherwise discovered at the end of a long QC pass, when the media has
    /// already been analysed and the rejection has already been promised.
    Check {
        /// Profile file to read.
        path: std::path::PathBuf,
    },

    /// Write an example profile to start from.
    ///
    /// Emits spec §68's example specification. A customer writes their profile
    /// from a blank file far less reliably than from one that already parses and
    /// already names every field correctly.
    Template {
        /// Where to write it. Refuses to overwrite an existing file.
        #[arg(long)]
        out: std::path::PathBuf,
    },
}

/// Which records `search` looks at, as a command-line value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum SearchScopeArg {
    /// Findings only.
    Findings,
    /// Assets only.
    Assets,
    /// Evidence only.
    Evidence,
    /// Everything.
    All,
}

impl From<SearchScopeArg> for tpt_app_media_forensics_core::store::SearchScope {
    fn from(value: SearchScopeArg) -> Self {
        use tpt_app_media_forensics_core::store::SearchScope;
        match value {
            SearchScopeArg::Findings => SearchScope::Findings,
            SearchScopeArg::Assets => SearchScope::Assets,
            SearchScopeArg::Evidence => SearchScope::Evidence,
            SearchScopeArg::All => SearchScope::All,
        }
    }
}

/// A severity floor for `search`, as a command-line value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum SeverityArg {
    /// Findings at Critical or above.
    Critical,
    /// Findings at Significant or above.
    Significant,
    /// Findings at Warning or above.
    Warning,
    /// Every finding.
    Info,
}

impl From<SeverityArg> for tpt_app_media_forensics_model::Severity {
    fn from(value: SeverityArg) -> Self {
        use tpt_app_media_forensics_model::Severity;
        match value {
            SeverityArg::Critical => Severity::Critical,
            SeverityArg::Significant => Severity::Significant,
            SeverityArg::Warning => Severity::Warning,
            SeverityArg::Info => Severity::Info,
        }
    }
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

        Command::Note {
            case_dir,
            body,
            subject_kind,
            subject,
        } => add_note(
            case_dir,
            body,
            subject_kind.as_deref(),
            subject.as_deref(),
            cli.json,
        ),

        Command::Batch {
            directory,
            case_dir,
        } => run_batch(directory, case_dir, cli.json),

        Command::Reference {
            case_dir,
            asset,
            clear,
        } => run_reference(case_dir, asset.as_deref(), *clear),

        Command::Compare {
            left,
            right,
            reference,
        } => compare(left, right, *reference, cli.json),

        Command::Validate {
            path,
            case_dir,
            profile,
            write,
        } => validate_delivery(
            path.as_deref(),
            case_dir.as_deref(),
            profile.as_deref(),
            *write,
            cli.json,
        ),

        Command::Profile { action } => run_profile(action, cli.json),

        Command::Search {
            case_dir,
            term,
            scope,
            min_severity,
            limit,
            // Copied out of the `&self.cli` match binding. `search_case` takes values
            // rather than references because it hands them straight to `SearchQuery`,
            // which is built rather than borrowed — a reference here would have to
            // outlive the query for no gain.
        } => search_case(
            case_dir,
            term.as_deref(),
            *scope,
            *min_severity,
            *limit,
            cli.json,
        ),
    }
}

/// Records an analyst note (spec §65).
///
/// Reads the body from stdin by default: a note is prose, often several
/// paragraphs, and routing it through shell quoting risks silently mangling it.
/// The store keeps the bytes verbatim, so nothing between this function and the
/// database may normalise them.
fn add_note(
    case_dir: &std::path::Path,
    body: &str,
    subject_kind: Option<&str>,
    subject: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    use std::io::Read as _;
    use tpt_app_media_forensics_core::store::Store;

    let text = if body == "-" {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .context("reading the note from stdin")?;
        buffer
    } else {
        body.to_owned()
    };

    if text.trim().is_empty() {
        // An empty note would be indistinguishable from "the analyst wrote
        // nothing", which is a different record from "something was written".
        anyhow::bail!("the note body is empty; refusing to record a note with no content");
    }

    let store = Store::open(case_dir)
        .with_context(|| format!("{} is not a case directory", case_dir.display()))?;
    let case_id = store
        .only_case_id()
        .context("reading the case record")?
        .ok_or_else(|| anyhow::anyhow!("{} contains no case record", case_dir.display()))?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let id = store
        .add_note(&case_id, subject_kind, subject, &text, now)
        .context("recording the note")?;

    if json {
        let value = serde_json::json!({
            "note_id": id,
            "case_id": case_id,
            "subject_kind": subject_kind,
            "subject_id": subject,
            "bytes": text.len(),
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        match (subject_kind, subject) {
            (Some(kind), Some(id)) => println!("Recorded note {id} on {kind}"),
            _ => println!("Recorded note {id} on the case"),
        }
    }
    Ok(())
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
        ContainerFormat::IsoBmff | ContainerFormat::Matroska => {
            // Both formats go through the same inspection result type; only the
            // reader differs. A format that reaches this arm is genuinely
            // parseable, so a failure here is a damaged file, not a gap.
            let inspection = match format {
                ContainerFormat::IsoBmff => inspect_file(path),
                _ => inspect_matroska_file(path),
            }
            .with_context(|| format!("cannot inspect {}", path.display()))?;
            for anomaly in &inspection.anomalies {
                eprintln!("anomaly: {anomaly}");
            }
            frame_info = inspection.frame_info.clone();
            repeated_runs = report_duplicate_runs(path, format);
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
                // Colour is what the container declares, not what the pixels are.
                // A file with no `colr` prints "not declared" rather than a
                // default: BT.709 would be a guess, and a guessed value here
                // reads as a measurement in everything downstream.
                let colour = &video.colour;
                let declared = colour
                    .primaries
                    .as_deref()
                    .unwrap_or("not declared")
                    .to_owned();
                let transfer = colour
                    .transfer
                    .as_deref()
                    .unwrap_or("not declared")
                    .to_owned();
                let range = match colour.full_range {
                    Some(true) => "full",
                    Some(false) => "limited",
                    None => "not declared",
                };
                println!(
                    "        colour:    primaries {declared}, transfer {transfer}, range {range}"
                );
                if video.is_hdr {
                    println!("        HDR:       signalled (BT.2020 or PQ/HLG)");
                }
                if let Some(metadata) = &colour.hdr_metadata {
                    println!("        HDR data:  {metadata}");
                }
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
fn report_duplicate_runs(path: &std::path::Path, format: ContainerFormat) -> String {
    // Sample reading is format-specific; using the MP4 reader on a WebM file
    // would fail to parse and silently report "no duplicates" for a file that
    // was never examined.
    let read = match format {
        ContainerFormat::Matroska => read_matroska_samples(std::fs::read(path).unwrap_or_default()),
        _ => read_samples(std::fs::read(path).unwrap_or_default()),
    };
    let Ok(samples) = read else {
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
/// Identifies an audio codec from the file's leading bytes.
///
/// Ogg carries its codec name in a header packet rather than in a magic number,
/// so the leading bytes are searched for the two identification strings this
/// build decodes. Searching the header region rather than the whole file keeps
/// probing cheap and avoids matching a codec name that appears in audio data.
fn detect_codec(bytes: &[u8]) -> String {
    // RIFF/WAVE is unambiguous from the first twelve bytes.
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WAVE" {
        return "wav".to_owned();
    }
    // OggS then the identification header. 256 bytes is enough for both
    // `OpusHead` and `\x01vorbis`, which appear immediately after the page
    // header.
    let head = &bytes[..bytes.len().min(256)];
    if head.starts_with(b"OggS") {
        if find_subslice(head, b"OpusHead").is_some() {
            return "Opus".to_owned();
        }
        if find_subslice(head, b"vorbis").is_some() {
            return "vorbis".to_owned();
        }
        return "ogg".to_owned();
    }
    if bytes.starts_with(b"fLaC") {
        return "fLaC".to_owned();
    }
    "unknown".to_owned()
}

/// Returns the offset of `needle` within `haystack`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Decodes a WAV file through the foundation's WAV reader.
///
/// WAV is not compressed and needs no codec choice, so it keeps its own path
/// rather than being folded into the Ogg decoder.
fn decode_wav(bytes: Vec<u8>) -> Result<tpt_app_media_forensics_audio::AudioDecode, String> {
    use tpt_app_media_forensics_audio::AudioDecode;
    use tpt_av_cadence_core::FormatReader as _;

    let source = Box::new(std::io::Cursor::new(bytes));
    let mut reader = tpt_av_cadence_wav::WavReader::open(source).map_err(|e| e.to_string())?;

    let channels = reader.info().channels;
    let sample_rate = reader.info().sample_rate;
    if channels == 0 {
        return Err("the WAV header declares zero channels".to_owned());
    }

    // Bounded decode: refuse to silently truncate a long file, and say so when
    // the cap is hit rather than presenting a prefix as the whole track.
    let cap = MAX_AUDIO_SAMPLES.min(48_000 * 600 * usize::from(channels));
    let mut pcm: Vec<f32> = Vec::new();
    let mut block = vec![0.0f32; 8192];
    let mut truncated = false;
    loop {
        let read = reader
            .decoder()
            .decode(&mut block)
            .map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        let take = read.min(cap.saturating_sub(pcm.len()));
        pcm.extend_from_slice(&block[..take]);
        if take < read {
            truncated = true;
            break;
        }
    }

    Ok(AudioDecode {
        pcm,
        channels,
        sample_rate,
        truncated,
    })
}

/// Analyses a raw audio file (spec §19-§22).
///
/// Decodes to PCM via `tpt-av-cadence`, then reports levels, silence regions,
/// DC offset, and integrated loudness. Every figure is printed with the
/// methodology that produced it, and a measurement that cannot be taken
/// correctly is reported as unavailable rather than approximated (spec §21).
fn audio_report(path: &std::path::Path, json: bool) -> anyhow::Result<()> {
    use tpt_app_media_forensics_audio::{
        amplitude_to_dbfs, decode_audio, find_silence, integrated_loudness, is_audio_decodable,
        level_stats, AudioDecodeLimits, Measurement, Methodology,
    };
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;

    // The codec is chosen from the file's own signature, never its extension:
    // an extension is a claim by whoever named the file, and this tool exists
    // to check claims.
    let codec = detect_codec(&bytes);
    let decoded = if is_audio_decodable(&codec) {
        decode_audio(bytes, &codec, AudioDecodeLimits::new(2))
            .map_err(|e| anyhow::anyhow!("{e}"))?
    } else if codec == "wav" {
        decode_wav(bytes).map_err(|e| anyhow::anyhow!("{e}"))?
    } else {
        anyhow::bail!(
            "{}: `{codec}` is not decoded here; this build decodes WAV, Opus, and Vorbis",
            path.display()
        );
    };

    if decoded.truncated {
        eprintln!("audio: decode stopped at the frame limit; these figures cover a prefix only");
    }

    let pcm = decoded.pcm;
    let sample_rate = decoded.sample_rate;
    let channels = decoded.channels;

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

/// Reads the analyst notes recorded on a case, for inclusion in a report.
///
/// Empty when the case database cannot be read rather than an error: `analyse` is
/// in the middle of writing to that database, and a report that failed to generate
/// because a note lookup failed would be a worse outcome than one without notes.
/// The limitation is stated in the report's own limitations list.
fn notes_for(
    directory: &tpt_app_media_forensics_core::CaseDirectory,
) -> Vec<tpt_app_media_forensics_report::Note> {
    use tpt_app_media_forensics_core::store::Store;

    let Ok(store) = Store::open(directory.root()) else {
        return Vec::new();
    };
    let Ok(Some(case_id)) = store.only_case_id() else {
        return Vec::new();
    };
    store
        .notes_in_case(&case_id)
        .unwrap_or_default()
        .into_iter()
        .map(|n| tpt_app_media_forensics_report::Note {
            subject_kind: n.subject_kind,
            subject_id: n.subject_id,
            body: n.body,
        })
        .collect()
}

/// Analyses a media file and writes the result into a case (spec §97).
///
/// Runs the same engine the desktop app uses, so a finding means the same thing
/// however it was reached (spec §51). The source is opened read-only.
///
/// The analysis runs on a background worker (spec §55) and reports each stage to
/// stderr as it completes, because a QC pass over a feature-length master takes
/// long enough that a silent terminal looks like a hung process. It goes to
/// stderr rather than stdout so that `--json` output stays machine-readable and
/// pipeable: a progress line interleaved with the result document would corrupt
/// both for anything parsing the output.
///
/// The result is identical whatever progress is printed — progress is a report
/// about the run, never a measurement of the media.
fn analyse(path: &std::path::Path, case_dir: &std::path::Path, json: bool) -> anyhow::Result<()> {
    use std::sync::{Arc, Mutex};

    use tpt_app_media_forensics_core::{AnalysisEngine, AnalysisJob, CaseDirectory, Progress};
    use tpt_app_media_forensics_report::{write_bundle, AssetSummary, Methodology, Report};

    let directory = CaseDirectory::open(case_dir)
        .with_context(|| format!("{} is not an initialised case", case_dir.display()))?;

    let engine = Arc::new(AnalysisEngine::new());

    // The last line printed, so a stage that reports twice does not print twice.
    let seen = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&seen);
    let progress = tpt_app_media_forensics_core::ProgressTracker::reporting(move |event| {
        let line = match event {
            Progress::Started { stage, .. } => format!("{} ...", stage.tag()),
            Progress::Finished { stage } => format!("{} done", stage.tag()),
            Progress::BranchFinished {
                completed, total, ..
            } => format!("concurrent analysis: {completed} of {total} independent stages done"),
        };

        let mut seen = sink.lock().expect("progress lock");
        if *seen == line {
            return;
        }
        *seen = line.clone();
        drop(seen);
        eprintln!("  {line}");
    });

    let job = AnalysisJob::spawn(
        Arc::clone(&engine),
        path.to_path_buf(),
        directory.clone(),
        progress,
    )?;
    let outcome = job.join()?;

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
        schema_version: tpt_app_media_forensics_report::REPORT_SCHEMA_VERSION,
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
        // The artefacts this run wrote into the case, each verified by re-reading
        // it from disk after writing. Previously hardcoded empty, so the report's
        // evidence table rendered no rows and `referenced_evidence` could only ever
        // return nothing.
        evidence: outcome.evidence.clone(),
        limitations: outcome.limitations.clone(),
        // Notes already on the case travel into the bundle. Reading them back means
        // re-running `analyse` on a case an analyst has already annotated produces a
        // report that still carries their conclusions, instead of quietly dropping
        // them the second time the file is analysed.
        notes: notes_for(&directory),
        methodology,
        validation: None,
        // No delivery profile was checked. `analyze` produces a forensic report;
        // `validate` attaches a verdict and a requirement table to one.
        // Defaulting this to an empty report would print a "Delivery validation"
        // section with no requirements in it, implying a specification was
        // applied when none was.
        delivery: None,
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
            // The artefacts this run wrote, so a machine consumer can locate them
            // without re-reading the case. Previously absent from the JSON entirely,
            // which meant evidence was written and then invisible to any script.
            "evidence_count": outcome.evidence.len(),
            "evidence": outcome.evidence,
            "timeline": outcome.timeline,
            "encoder_indicators": outcome.fingerprint,
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
        // Evidence is stated even when there is none. An analyst who sees no line
        // here cannot tell "no frames were decoded" from "nothing was written",
        // and those are very different conclusions about the strength of the
        // examination — so the count is always printed and the artefacts listed
        // when they exist.
        println!("Evidence      {}", outcome.evidence.len());
        for artefact in &outcome.evidence {
            println!(
                "  {}  {}  {} bytes",
                artefact.kind.tag(),
                artefact.relative_path,
                artefact.integrity.size_bytes
            );
        }
        // The placement qualifier travels with each timecode, so an inferred
        // position is never read as a measured one.
        if !outcome.timeline.is_empty() {
            println!("Timeline       {} observations", outcome.timeline.len());
            for line in outcome.timeline.describe() {
                println!("  {line}");
            }
        }
        // Each indicator is printed with what it does not establish. A declared
        // tag shown on its own invites a reader to treat "Lavf58" as proof of
        // FFmpeg, which is precisely the inference spec §27's "where technically
        // defensible" rules out.
        if !outcome.fingerprint.indicators.is_empty() {
            println!("Encoder indicators");
            for indicator in &outcome.fingerprint.indicators {
                println!(
                    "  {} [{:?}/{:?}] observed: {}",
                    indicator.name, indicator.evidence, indicator.confidence, indicator.observation
                );
                println!("      does not establish: {}", indicator.limitations);
            }
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

/// Designates, clears, or lists a case's reference asset (spec §67).
///
/// Prints the designation with the reference's SHA-256, because the point of
/// recording it is that a later `compare --reference` can name the exact bytes it
/// measured against. A designation an analyst cannot see is a designation they
/// cannot check.
fn run_reference(
    case_dir: &std::path::Path,
    asset: Option<&str>,
    clear: bool,
) -> anyhow::Result<()> {
    use tpt_app_media_forensics_core::store::{AssetRole, Store};

    let store = Store::open(case_dir)?;
    let case_id = store.only_case_id()?.ok_or_else(|| {
        anyhow::anyhow!(
            "{} holds no case, so there is no asset to designate",
            case_dir.display()
        )
    })?;

    // No asset named: list. Listing is the read-only mode and deliberately needs
    // neither an asset nor a flag, so "what does this case think its master is?"
    // is one command rather than a flag combination someone has to remember.
    let Some(selector) = asset else {
        let references = store.reference_assets(&case_id)?;
        if references.is_empty() {
            println!("No reference designated.");
        }
        for asset in &references {
            println!(
                "{}  sha256 {}",
                asset.name,
                asset.sha256.as_deref().unwrap_or("not recorded")
            );
        }
        return Ok(());
    };

    let found = store.find_asset(&case_id, selector)?.ok_or_else(|| {
        anyhow::anyhow!(
            "no asset named or identified by `{selector}` in this case; run without \
                 an argument to see what the case holds"
        )
    })?;

    let role = if clear {
        None
    } else {
        Some(AssetRole::Reference)
    };
    store.set_asset_role(&case_id, &found.id, role)?;

    if clear {
        println!("Cleared the reference designation for {}.", found.name);
        return Ok(());
    }

    println!(
        "Designated {} as the reference.\n  sha256 {}",
        found.name,
        found.sha256.as_deref().unwrap_or("not recorded")
    );
    // Naming the others is not decoration: replacing a master silently would leave
    // a reviewer unable to tell which of two files the report meant.
    let designated = store.reference_assets(&case_id)?;
    let others: Vec<&str> = designated
        .iter()
        .filter(|a| a.id != found.id)
        .map(|a| a.name.as_str())
        .collect();
    if !others.is_empty() {
        println!(
            "  (other assets are also designated: {})",
            others.join(", ")
        );
    }
    Ok(())
}

/// Compares two media files across every measured axis (spec §38–40, §67).
///
/// Analyses both files directly rather than reading a case, because a comparison
/// is a question *about* two files rather than a finding *about* an asset: it
/// creates no case, writes no analysis record, and touches nothing. Both sources
/// are opened read-only.
///
/// # What the output does and does not claim
///
/// The result reports differences and, separately, axes it could not compare.
/// Those are kept apart because "we could not read it" and "it matches" are
/// different claims, and a comparison that collapsed them would tell a reviewer
/// two files are equivalent when in fact nothing was measured.
///
/// There is deliberately no overall verdict and no similarity score. A transcode
/// to a lower bitrate and a re-mux with reordered atoms produce byte-different
/// files, but only the first changed anything a reviewer would care about; a
/// single number would discard exactly the information the command exists to
/// surface.
///
/// # The declared reference
///
/// With `--reference`, the left side is a *declared master* rather than just a
/// second path, and its content digest travels with the result. That is the whole
/// of spec §67: the axes are identical, and what changes is that the answer is
/// bound to specific bytes rather than to two filenames a reader cannot verify.
///
/// A reference that cannot be hashed is refused rather than compared and
/// unlabelled. Silently falling back to a plain comparison would produce exactly
/// the output the flag exists to prevent — "what changed since the master?" with
/// nothing recording which master.
fn compare(
    left: &std::path::Path,
    right: &std::path::Path,
    declare_reference: bool,
    json: bool,
) -> anyhow::Result<()> {
    use tpt_app_media_forensics_rules::comparison::compare as compare_inputs;
    use tpt_app_media_forensics_rules::comparison::compare_against_reference;

    if !declare_reference {
        return compare_pair(left, right, json, compare_inputs);
    }

    // Hashing happens before the comparison, not after: a reference that cannot be
    // identified must not produce a comparison at all. Falling back to the
    // unlabelled form would emit exactly the output the flag exists to prevent —
    // "what changed since the master?" with nothing recording which master.
    let identity = read_reference_identity(left)?;
    compare_pair(left, right, json, |a, b| {
        compare_against_reference(a, b, identity.clone())
    })
}

/// Runs the engine over both paths and emits the comparison.
///
/// Split out of [`compare`] so the two entry points — plain and reference —
/// cannot drift on how a file is analysed, how it is named, or how the result is
/// emitted. Only the comparison itself differs between them, which is the point.
fn compare_pair(
    left: &std::path::Path,
    right: &std::path::Path,
    json: bool,
    run: impl Fn(
        &tpt_app_media_forensics_rules::comparison::ComparisonInput<'_>,
        &tpt_app_media_forensics_rules::comparison::ComparisonInput<'_>,
    ) -> tpt_app_media_forensics_rules::comparison::Comparison,
) -> anyhow::Result<()> {
    use tpt_app_media_forensics_core::AnalysisEngine;

    let engine = AnalysisEngine::new();
    let left_bundle = engine.observe_stages(left).0;
    let right_bundle = engine.observe_stages(right).0;

    let left_name = display_name(left);
    let right_name = display_name(right);
    let left_input = comparison_input(&left_bundle, &left_name);
    let right_input = comparison_input(&right_bundle, &right_name);
    let result = run(&left_input, &right_input);

    emit(json, &result, &render_comparison(&result));
    Ok(())
}

/// Hashes the declared reference so the comparison is bound to these bytes.
///
/// Read separately from the analysis rather than reused from it: the bundle
/// deliberately does not carry the acquisition hashes, and re-hashing a media
/// file the engine has just read is cheaper than the extra plumbing that would
/// thread them through.
///
/// Fails rather than returning a partial identity. A reference named but not
/// identified would put "what changed?" in a report with nothing recording which
/// master it was measured against.
fn read_reference_identity(
    path: &std::path::Path,
) -> anyhow::Result<tpt_app_media_forensics_model::ReferenceIdentity> {
    let record = tpt_app_media_forensics_core::acquisition::acquire(path)?;
    let sha256 = record.hashes.sha256().ok_or_else(|| {
        anyhow::anyhow!(
            "{}: no SHA-256 could be computed for the reference",
            path.display()
        )
    })?;

    Ok(tpt_app_media_forensics_model::ReferenceIdentity {
        name: display_name(path),
        sha256: sha256.to_owned(),
        blake3: record.hashes.blake3().map(ToOwned::to_owned),
    })
}

/// The file's name, so a report identifies two files by more than a full path.
fn display_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Builds the comparison input for one analysed asset.
///
/// Every field is `Option` and stays `None` when the stage that would have
/// measured it did not run. Passing a zero for an unmeasured axis would make
/// "not measured" indistinguishable from "measured as zero", which is the same
/// conflation the comparison types exist to prevent.
fn comparison_input<'a>(
    bundle: &'a tpt_app_media_forensics_rules::AnalysisBundle,
    name: &'a str,
) -> tpt_app_media_forensics_rules::comparison::ComparisonInput<'a> {
    use tpt_app_media_forensics_rules::comparison::ComparisonInput;

    ComparisonInput {
        name,
        streams: bundle.container.as_ref().map(|c| c.streams.as_slice()),
        metadata: bundle.metadata.as_ref(),
        scene: bundle.scene.as_ref(),
        silence: Some(bundle.silence.as_slice()),
        loudness: bundle.loudness.as_ref(),
    }
}

/// One-line rendering of a per-property comparison.
///
/// The three that matter are kept visibly distinct: a difference, a property only
/// one side has, and a property neither side could measure.
fn describe_difference(
    difference: &tpt_app_media_forensics_model::comparison::Difference,
) -> String {
    use tpt_app_media_forensics_model::comparison::Difference;
    match difference {
        Difference::Equal => "same on both sides".to_owned(),
        Difference::Different { left, right } => format!("{left} vs {right}"),
        Difference::OnlyLeft { value } => format!("only on the left: {value}"),
        Difference::OnlyRight { value } => format!("only on the right: {value}"),
        Difference::NotComparable { reason } => format!("not compared: {reason}"),
    }
}

/// One-line rendering of a tolerance-based comparison.
fn describe_within_tolerance(
    result: &tpt_app_media_forensics_rules::comparison::WithinTolerance,
) -> String {
    use tpt_app_media_forensics_rules::comparison::WithinTolerance;
    match result {
        WithinTolerance::Agree { lower, upper, .. } => {
            format!("{lower:.1} to {upper:.1} LU, within tolerance")
        }
        WithinTolerance::Diverge { left, right, .. } => {
            format!("{left:.1} LU vs {right:.1} LU, outside tolerance")
        }
        WithinTolerance::Unmeasured { .. } => "not measured on at least one side".to_owned(),
    }
}

/// Renders a comparison as text.
///
/// Grouped by axis and ordered by [`ComparisonAxis::ALL`], so two runs of the
/// same comparison read in the same order — a report whose ordering varies is
/// hard to diff and easy to misread.
///
/// Per-property results are labelled `same`, `DIFFERS`, or `NOT COMPARED` rather
/// than only the interesting ones being printed. A reader shown only the
/// differences would conclude the files match on everything else, which is the
/// one inference this command must not invite.
fn render_comparison(result: &tpt_app_media_forensics_rules::comparison::Comparison) -> String {
    use tpt_app_media_forensics_model::comparison::{ComparisonAxis, ComparisonSide};

    let mut out = String::new();
    // When a reference is declared, the sides are relabelled. "Left" and "Right"
    // describe a mechanism; "Reference" and "Delivery" describe the question the
    // reader is actually asking (spec §67), and a difference then reads in the
    // direction that matters — what the delivery did to the master.
    let (left_label, right_label) = match result.reference() {
        Some(_) => ("Reference", "Delivery"),
        None => ("Left", "Right"),
    };
    out.push_str(&format!("{left_label:<11} {}\n", result.left_name));
    out.push_str(&format!("{right_label:<11} {}\n", result.right_name));

    // The reference's digest, printed in full.
    //
    // A name is not evidence: `Master.mov` survives being overwritten by a
    // different encode. Printing the digest is what lets a reader confirm which
    // master produced this answer, or discover that the master has since been
    // swapped — which is the whole reason the flag exists.
    if let Some(reference) = result.reference() {
        out.push_str(&format!("             sha256 {}\n", reference.sha256));
    }

    // Three outcomes, not two. "Equivalent" is deliberately strict — it is false
    // whenever any axis went uncomparable even if nothing differs — so printing a
    // bare "no" would leave a reader unable to tell a real disagreement from an
    // axis this build never measured. Those are different sentences.
    let differences = result.measured_differences();
    out.push_str(&format!(
        "Result      {}\n",
        match (differences.is_empty(), result.is_equivalent()) {
            (false, _) => format!("{} measured difference(s) — see below", differences.len()),
            (true, true) => "equivalent on every axis measured".to_owned(),
            (true, false) => {
                "no measured differences, but not every axis could be compared".to_owned()
            }
        }
    ));
    out.push_str(&format!(
        "Tolerances  loudness {:.2} LU, scene changes {:.1}\n",
        result.tolerances.loudness_lu, result.tolerances.scene_changes
    ));

    for axis in ComparisonAxis::ALL {
        let mut lines: Vec<String> = Vec::new();

        for stream in &result.streams.streams {
            for field in stream.fields.iter().filter(|f| f.axis == *axis) {
                let label = if field.difference.is_different() {
                    "DIFFERS"
                } else if field.difference.is_not_comparable() {
                    "NOT COMPARED"
                } else {
                    "same"
                };
                lines.push(format!(
                    "  {label:<13} {} — {}",
                    field.field,
                    describe_difference(&field.difference)
                ));
            }
        }

        // Whole-file axes live outside the per-stream list, so they are rendered
        // from their own summaries. Without this an axis measured only once —
        // silence, with no video stream to attach it to — would never appear.
        match axis {
            ComparisonAxis::SceneStructure => lines.push(format!(
                "  {:<13} {}",
                "scene changes",
                describe_difference(&result.scene.changes)
            )),
            ComparisonAxis::Silence => {
                lines.push(format!(
                    "  {:<13} {}",
                    "silent regions",
                    describe_difference(&result.silence.region_counts)
                ));
                lines.push(format!(
                    "  {:<13} {}",
                    "silent frames",
                    describe_within_tolerance(&result.silence.totals)
                ));
            }
            ComparisonAxis::Loudness => lines.push(format!(
                "  {:<13} {}",
                "integrated loudness",
                describe_within_tolerance(&result.loudness)
            )),
            _ => {}
        }

        if lines.is_empty() {
            continue;
        }
        out.push_str(&format!("{}\n", axis.tag()));
        for line in lines {
            out.push_str(&line);
            out.push('\n');
        }
        out.push('\n');
    }

    if !result.unmatched().is_empty() {
        out.push_str("Unmatched streams\n");
        for stream in result.unmatched() {
            // Relabelled with the sides. "Left stream 1: no counterpart" tells a
            // reviewer nothing about which file is missing the track; "reference
            // stream 1" tells them the master has it and the delivery dropped it,
            // which is the finding.
            let side = match stream.side {
                ComparisonSide::Left => left_label,
                ComparisonSide::Right => right_label,
            };
            out.push_str(&format!(
                "  {side} stream {} ({}): no counterpart\n",
                stream.index, stream.codec
            ));
        }
        out.push('\n');
    }

    if !result.metadata.is_empty() {
        out.push_str("Metadata\n");
        for entry in &result.metadata {
            let track = match entry.track_index {
                Some(index) => format!(" (track {index})"),
                None => String::new(),
            };
            out.push_str(&format!(
                "  {}{track}: {}\n",
                entry.key,
                describe_difference(&entry.difference)
            ));
        }
        out.push('\n');
    }

    out
}

/// Searches a case's findings, assets, and evidence (spec §41).
///
/// The point of this command is that the engine has been writing findings to the
/// database and, before it, nothing read them back out. An examiner who wanted
/// one finding out of five thousand had no way to name it.
///
/// # Truncation is never implied by a short list
///
/// `--limit` bounds how many rows are returned, and the count of everything that
/// matched is reported alongside it. Showing 200 of 5,000 rows and printing a
/// bare "200 results" would be a false statement about the case — and worse, one
/// a reviewer could not detect from the output alone.
///
/// # A severity floor constrains findings only
///
/// `--min-severity` filters findings. Assets and evidence carry no severity, so
/// the floor does not hide them; dropping them would mean searching a case for
/// its assets at WARNING and being told there were none.
fn search_case(
    case_dir: &std::path::Path,
    term: Option<&str>,
    scope: SearchScopeArg,
    min_severity: Option<SeverityArg>,
    limit: Option<usize>,
    json: bool,
) -> anyhow::Result<()> {
    use tpt_app_media_forensics_core::store::search::search as run_search;
    use tpt_app_media_forensics_core::store::{SearchQuery, SeverityFilter, Store};

    let directory = CaseDirectory::open(case_dir)
        .with_context(|| format!("{} is not an initialised case", case_dir.display()))?;
    let store = Store::open(directory.root())?;
    let case_id = store.only_case_id()?.ok_or_else(|| {
        anyhow::anyhow!(
            "{} holds no case, so there is nothing to search",
            case_dir.display()
        )
    })?;

    let mut query = match term {
        Some(text) => SearchQuery::text(text),
        None => SearchQuery::all(),
    };
    query.scope = Some(scope.into());
    query.severity = min_severity.map(|s| SeverityFilter::AtLeast(s.into()));
    query.limit = limit;

    let result = run_search(store.connection(), &case_id, &query)
        .map_err(|e| anyhow::anyhow!("search failed for {}: {e}", case_dir.display()))?;

    emit(
        json,
        &SearchJson::new(&query, &result),
        &render_search(&query, &result),
    );
    Ok(())
}

/// The search result in a shape that serialises usefully.
///
/// A thin wrapper rather than a `#[derive(Serialize)]` on `SearchResult` so the
/// JSON gains a name for what was searched *for*, not only what it found — a
/// document listing 200 rows is ambiguous without it.
#[derive(serde::Serialize)]
struct SearchJson {
    term: String,
    scope: &'static str,
    matched: usize,
    total: usize,
    truncated: bool,
    hits: Vec<SearchHitJson>,
}

/// One row, in the shape the CLI prints.
#[derive(serde::Serialize)]
struct SearchHitJson {
    scope: &'static str,
    severity: Option<&'static str>,
    label: String,
    id: String,
}

impl SearchJson {
    /// Builds the JSON view of a search.
    ///
    /// Takes the query as well as the result so `term` and `scope` describe what
    /// was actually asked for. Taken from the result alone they would have to be
    /// invented, and a document that names the wrong search is worse than one that
    /// omits the name.
    fn new(
        query: &tpt_app_media_forensics_core::store::SearchQuery,
        result: &tpt_app_media_forensics_core::store::search::SearchResult,
    ) -> Self {
        Self {
            term: query.trimmed().to_owned(),
            scope: scope_tag(query.effective_scope()),
            matched: result.hits.len(),
            total: result.total,
            truncated: result.truncated,
            hits: result
                .hits
                .iter()
                .map(|hit| SearchHitJson {
                    scope: scope_tag(hit.scope),
                    severity: hit.severity.map(|s| s.tag()),
                    label: hit.label.clone(),
                    id: hit.id.clone(),
                })
                .collect(),
        }
    }
}

/// A stable lowercase tag for a search scope.
fn scope_tag(scope: tpt_app_media_forensics_core::store::SearchScope) -> &'static str {
    use tpt_app_media_forensics_core::store::SearchScope;
    match scope {
        SearchScope::Findings => "finding",
        SearchScope::Assets => "asset",
        SearchScope::Evidence => "evidence",
        SearchScope::All => "all",
    }
}

/// Renders search results as text.
///
/// The count line always states both the rows returned *and* the total that
/// matched. A truncated page says so in words rather than leaving a short list to
/// imply there was nothing more — the difference between "200 findings" and
/// "200 of 5,000 findings" is the difference between a report and a fabrication.
fn render_search(
    query: &tpt_app_media_forensics_core::store::SearchQuery,
    result: &tpt_app_media_forensics_core::store::SearchResult,
) -> String {
    let mut out = String::new();

    // Echo what was searched for, not only what was found, so a saved output can
    // be read months later without reference to the command that produced it.
    let term = query.trimmed();
    if term.is_empty() {
        out.push_str("Term        (none — every record in scope)\n");
    } else {
        out.push_str(&format!("Term        {term}\n"));
    }
    out.push_str(&format!(
        "Scope       {}\n",
        scope_tag(query.effective_scope())
    ));

    if result.truncated {
        out.push_str(&format!(
            "Results     {} of {} matches (truncated by --limit)\n",
            result.hits.len(),
            result.total
        ));
    } else {
        out.push_str(&format!("Results     {} match(es)\n", result.total));
    }

    if result.is_empty() {
        // Distinguishes "nothing matched" from "nothing was looked at", which a
        // whitespace-only term or a wrong scope could otherwise blur.
        out.push_str("\nNo matching records.\n");
        return out;
    }

    out.push('\n');
    for hit in &result.hits {
        let severity = hit
            .severity
            .map(|s| format!("[{}] ", s.tag()))
            .unwrap_or_default();
        out.push_str(&format!(
            "  {:<9} {severity}{}\n",
            scope_tag(hit.scope),
            hit.label
        ));
        out.push_str(&format!("            {}\n", hit.id));
    }

    if result.truncated {
        out.push_str(&format!(
            "\n{} more match(es) exist; raise --limit to see them.\n",
            result.total - result.hits.len()
        ));
    }

    out
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

/// Reports whether a file meets a delivery specification (spec §68, §69, §95).
///
/// The verdict is derived by `ValidationResult`, which is the single place that
/// decides what blocks a delivery. Nothing here restates that rule: a second copy
/// in the command layer would be free to disagree with the one the report
/// renders.
///
/// A case directory is read rather than re-analysed, for the same reason
/// `generate_report` does — a verdict computed from a fresh run would be a
/// statement about that run, not about the findings in the case.
///
/// # Errors
///
/// Returns an error when the profile cannot be read or parsed, or when the file
/// cannot be inspected. A file that inspects but fails its requirements is not an
/// error: it is a `FAIL` with exit code 2.
fn validate_delivery(
    path: Option<&std::path::Path>,
    case_dir: Option<&std::path::Path>,
    profile_path: Option<&std::path::Path>,
    write: bool,
    json: bool,
) -> anyhow::Result<()> {
    let outcome = match (path, case_dir) {
        // A file needs a specification to be judged against. Validating one with
        // no profile would print a verdict about a file against a standard nobody
        // named, which is the claim this command exists to avoid.
        (Some(path), None) => {
            let profile_path = profile_path.context(
                "validating a file needs a specification: pass --profile <file>, or \
                 use --case-dir to judge a case's recorded findings instead",
            )?;
            validate_file(path, profile_path, write)?
        }
        // A case with a profile still works: the profile is checked against the
        // media the case recorded, and the findings come from the database. The
        // two answers are combined rather than one replacing the other.
        (None, Some(case_dir)) => validate_case(case_dir, profile_path, write)?,
        // clap's `conflicts_with` rejects this, but the match must be total.
        (Some(_), Some(_)) => anyhow::bail!("pass either a file or --case-dir, not both"),
        (None, None) => anyhow::bail!(
            "nothing to validate: pass a file path, or --case-dir <case> for a recorded case"
        ),
    };

    print_validation(&outcome, json)
}

/// Validates a single file against a profile (spec §95).
///
/// Reads the source read-only and writes nothing unless `--write` is given. No
/// case directory is involved: a delivery check is a question about one file
/// against one specification, and requiring an analysed case to ask it would make
/// the QC path redo the forensic path's work.
fn validate_file(
    path: &std::path::Path,
    profile_path: &std::path::Path,
    write: bool,
) -> anyhow::Result<ValidationOutcome> {
    use tpt_app_media_forensics_report::ValidationResult;

    let profile = load_profile(profile_path)?;
    let inspection = inspect_for_validation(path)?;

    // No A/V offset is supplied: this path inspects the container and does not run
    // the timing analysis. A profile asking for `timing.max_av_offset_ms` therefore
    // reports NOT MEASURED, which blocks — the honest answer, because "we could not
    // look" is not "it was fine", and the requirement is reported rather than
    // quietly skipped.
    let delivery = tpt_app_media_forensics_rules::delivery::check_profile(&profile, &inspection);

    let bundle = if write {
        let report = build_validation_report(path, &delivery);
        let at = path.with_extension("validated");
        Some((
            at.clone(),
            tpt_app_media_forensics_report::write_bundle(&report, &at)?,
        ))
    } else {
        None
    };

    Ok(ValidationOutcome {
        verdict: ValidationResult::from_delivery(&delivery),
        source: path.display().to_string(),
        case_dir: None,
        findings_considered: None,
        delivery: Some(delivery),
        blocking_findings: Vec::new(),
        warning_findings: Vec::new(),
        bundle,
    })
}
/// Validates a recorded case, optionally against a profile (spec §68).
fn validate_case(
    case_dir: &std::path::Path,
    profile_path: Option<&std::path::Path>,
    write: bool,
) -> anyhow::Result<ValidationOutcome> {
    use tpt_app_media_forensics_core::pipeline::load_report;
    use tpt_app_media_forensics_report::ValidationResult;

    let directory = CaseDirectory::open(case_dir)
        .with_context(|| format!("{} is not an initialised case", case_dir.display()))?;
    let loaded = load_report(&directory)?;
    let report = &loaded.report;

    // When a profile is supplied, the media the case recorded is inspected against
    // it. Reading that path from the case rather than taking one on the command
    // line keeps the two in agreement: a verdict about a different file than the
    // one analysed would be worse than no verdict at all.
    let delivery = match profile_path {
        Some(profile_path) => {
            let profile = load_profile(profile_path)?;
            let source = report
                .assets
                .first()
                .map(|a| &a.source_path)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "{} records no asset to validate against {}",
                        case_dir.display(),
                        profile_path.display()
                    )
                })?;
            Some(tpt_app_media_forensics_rules::delivery::check_profile(
                &profile,
                &inspect_for_validation(std::path::Path::new(source))?,
            ))
        }
        None => None,
    };

    let verdict = ValidationResult::combine(delivery.as_ref(), &report.findings);

    // The blocking findings, named rather than counted. "FAIL" on its own tells an
    // analyst nothing about what to fix; the rule id and summary are what they act
    // on, and a verdict that does not carry them is not actionable.
    //
    // Cloned rather than borrowed: `ValidationOutcome` outlives the loaded report
    // it was built from, and a finding is a small value next to the database it
    // came out of.
    let blocking: Vec<tpt_app_media_forensics_model::Finding> = report
        .findings
        .iter()
        .filter(|f| f.severity.fails_validation())
        .cloned()
        .collect();
    let warnings: Vec<tpt_app_media_forensics_model::Finding> = report
        .findings
        .iter()
        .filter(|f| f.severity == tpt_app_media_forensics_model::Severity::Warning)
        .cloned()
        .collect();

    let bundle = if write {
        let mut validated = report.clone();
        validated.validation = Some(verdict);
        validated.delivery = delivery.clone();
        let at = directory.root().join("reports").join("validated");
        Some((
            at.clone(),
            tpt_app_media_forensics_report::write_bundle(&validated, &at)?,
        ))
    } else {
        None
    };

    Ok(ValidationOutcome {
        verdict,
        source: report
            .assets
            .first()
            .map_or_else(String::new, |a| a.source_path.clone()),
        case_dir: Some(directory.root().display().to_string()),
        findings_considered: Some(report.findings.len()),
        delivery,
        blocking_findings: blocking,
        warning_findings: warnings,
        bundle,
    })
}

/// Everything one validation run produced, before it is rendered.
struct ValidationOutcome {
    /// The single verdict both halves were combined into.
    verdict: tpt_app_media_forensics_report::ValidationResult,
    /// The media that was checked.
    source: String,
    /// The case the findings came from, when a case was read.
    case_dir: Option<String>,
    /// How many findings informed the severity half, when any did.
    ///
    /// `None` for a bare file check, where no analysis ran. Printing a finding
    /// count of zero there would imply a clean analysis rather than no analysis.
    findings_considered: Option<usize>,
    /// The requirement results, when a profile was applied.
    delivery: Option<tpt_app_media_forensics_model::DeliveryReport>,
    blocking_findings: Vec<tpt_app_media_forensics_model::Finding>,
    warning_findings: Vec<tpt_app_media_forensics_model::Finding>,
    /// Where the bundle was written, with its manifest, when `--write` was given.
    bundle: Option<(
        std::path::PathBuf,
        tpt_app_media_forensics_report::BundleManifest,
    )>,
}
/// Reads and parses a delivery profile file (spec §69).
///
/// A parse failure names the file and says what a valid profile looks like,
/// because a hand-written profile that does not load is the most likely thing to
/// go wrong here and "invalid profile" alone sends the reader nowhere.
fn load_profile(
    path: &std::path::Path,
) -> anyhow::Result<tpt_app_media_forensics_model::DeliveryProfile> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading the delivery profile {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| {
        format!(
            "{} is not a valid delivery profile: requirements are objects tagged with \
             \"kind\", and every numeric requirement states its own tolerance",
            path.display()
        )
    })
}

/// Inspects a file for validation, refusing anything this build cannot read.
fn inspect_for_validation(
    path: &std::path::Path,
) -> anyhow::Result<tpt_app_media_forensics_container::ContainerInspection> {
    use tpt_app_media_forensics_container::{
        detect_file, inspect_file, inspect_matroska_file, ContainerFormat,
    };

    let format = detect_file(path).with_context(|| format!("cannot read {}", path.display()))?;
    match format {
        ContainerFormat::IsoBmff => {
            inspect_file(path).with_context(|| format!("cannot inspect {}", path.display()))
        }
        ContainerFormat::Matroska => inspect_matroska_file(path)
            .with_context(|| format!("cannot inspect {}", path.display())),
        // An unrecognised signature is a fact about the evidence, not a gap in the
        // tool. Conflating the two would tell an analyst their file is fine once a
        // format is added, when in fact it is not a container this build reads.
        other => anyhow::bail!(
            "{} is a {} file; this build cannot check a delivery profile against it",
            path.display(),
            other.tag()
        ),
    }
}

/// Builds the report a file-only validation writes, when `--write` is given.
///
/// A validation report is a real forensic report with a verdict and a requirement
/// table attached, not a separate document format: a client disputing a rejection
/// needs the hashes and the methodology beside the verdict, and a second format
/// would be one more thing that can disagree with the first.
fn build_validation_report(
    path: &std::path::Path,
    delivery: &tpt_app_media_forensics_model::DeliveryReport,
) -> tpt_app_media_forensics_report::Report {
    use tpt_app_media_forensics_report::{AssetSummary, Methodology, Report, ValidationResult};

    let verdict = ValidationResult::from_delivery(delivery);

    Report {
        schema_version: tpt_app_media_forensics_report::REPORT_SCHEMA_VERSION,
        case_name: delivery.profile_name.clone(),
        case_id: path.display().to_string(),
        case_description: Some(format!(
            "Delivery validation of {} against {}",
            path.display(),
            delivery.profile_identifier
        )),
        assets: vec![AssetSummary {
            name: path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
            source_path: path.display().to_string(),
            sha256: file_sha256(path),
            blake3: None,
            size_bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            stream_count: 0,
        }],
        // No analysis ran, so there are no findings to carry. A validation report
        // listing findings would be claiming an examination this path did not
        // perform.
        findings: Vec::new(),
        evidence: Vec::new(),
        methodology: Methodology {
            application_version: env!("CARGO_PKG_VERSION").to_owned(),
            analysis_version: tpt_app_media_forensics_model::AnalysisVersion::CURRENT.to_string(),
            profile: delivery.profile_identifier.clone(),
            profile_fingerprint: delivery.profile_fingerprint.clone(),
            enabled_rules: Vec::new(),
            rule_set_fingerprint: "none (no forensic rules ran)".to_owned(),
            input_hashes: Vec::new(),
            analysis_timestamp_unix: tpt_app_media_forensics_core::pipeline::analysis_timestamp(),
            applicable_standards: vec![delivery.profile_name.clone()],
            analysis_fingerprint: delivery.profile_fingerprint.clone(),
        },
        limitations: vec![
            "This is a delivery-specification check. No forensic rules were run, so the \
             report contains no findings and makes no claim about the file's integrity \
             beyond the requirements listed above."
                .to_owned(),
        ],
        notes: Vec::new(),
        validation: Some(verdict),
        delivery: Some(delivery.clone()),
    }
}

/// The SHA-256 of a file, lowercase hex.
///
/// `None` rather than a fabricated value if the file cannot be read: a report
/// claiming a digest nobody computed is worse than one admitting it has none.
/// Unreachable in practice, because the inspection that already succeeded proves
/// the file is readable � which is exactly why it must not be an `unwrap`.
fn file_sha256(path: &std::path::Path) -> Option<String> {
    use sha2::Digest as _;
    std::fs::read(path)
        .ok()
        .map(|bytes| tpt_app_media_forensics_model::asset::to_hex(&sha2::Sha256::digest(&bytes)))
}

/// Renders a validation outcome and sets the exit code.
///
/// # Errors
///
/// Returns an error only if the output cannot be written. The exit code is set
/// here rather than by each caller, because a delivery gate that always exits 0
/// is not a gate — and a second place deciding the exit code is a second place
/// that can disagree about the verdict.
fn print_validation(outcome: &ValidationOutcome, json: bool) -> anyhow::Result<()> {
    use tpt_app_media_forensics_report::ValidationResult;

    let payload = serde_json::json!({
        "source": outcome.source,
        "case_dir": outcome.case_dir,
        "result": outcome.verdict.label(),
        "profile": outcome.delivery.as_ref().map(|d| serde_json::json!({
            "name": d.profile_name,
            "version": d.profile_version,
            "identifier": d.profile_identifier,
            "fingerprint": d.profile_fingerprint,
        })),
        "requirements": outcome.delivery.as_ref().map(|d| d.checks.iter()
            .map(|c| serde_json::json!({
                "id": c.requirement_id,
                "expected": c.expected,
                "observed": c.observed,
                "outcome": c.outcome.label(),
                "detail": c.detail,
            }))
            .collect::<Vec<_>>()),
        "findings_considered": outcome.findings_considered,
        "blocking": outcome.blocking_findings.iter().map(|f| serde_json::json!({
            "rule_id": f.rule_id,
            "severity": f.severity.tag(),
            "summary": f.observation.summary,
        })).collect::<Vec<_>>(),
        "warnings": outcome.warning_findings.iter().map(|f| serde_json::json!({
            "rule_id": f.rule_id,
            "summary": f.observation.summary,
        })).collect::<Vec<_>>(),
        "bundle": outcome.bundle.as_ref().map(|(at, manifest)| serde_json::json!({
            "directory": at.display().to_string(),
            "files": manifest.files,
        })),
    });

    let mut text = String::new();
    text.push_str(&format!("Result        {}\n", outcome.verdict.label()));
    text.push_str(&format!("Source        {}\n", outcome.source));

    if let Some(delivery) = &outcome.delivery {
        text.push_str(&format!("Profile       {}\n", delivery.profile_identifier));
        text.push_str(&format!("Fingerprint   {}\n", delivery.profile_fingerprint));
        text.push('\n');
        for check in &delivery.checks {
            text.push_str(&format!("{}\n", check.requirement_id));
            text.push_str(&format!("  Expected: {}\n", check.expected));
            // "not measured" rather than a blank, matching the HTML and PDF. An
            // empty line beside a verdict reads as a value that was zero.
            text.push_str(&format!(
                "  Observed: {}\n",
                check.observed.as_deref().unwrap_or("not measured")
            ));
            text.push_str(&format!("  Result:    {}\n", check.outcome.label()));
            text.push_str(&format!("  {}\n", check.detail));
        }
    }

    match outcome.findings_considered {
        Some(count) => text.push_str(&format!("\nFindings      {count}\n")),
        // Said explicitly rather than omitted: the absence of a finding count is
        // the difference between "the analysis found nothing" and "no analysis ran",
        // and a reader of this output is entitled to know which.
        None => text.push_str("\nNo forensic analysis was run for this check.\n"),
    }

    if !outcome.blocking_findings.is_empty() {
        text.push_str("Blocking\n");
        for finding in &outcome.blocking_findings {
            text.push_str(&format!(
                "  [{}] {}  {}\n",
                finding.severity.tag(),
                finding.rule_id,
                finding.observation.summary
            ));
        }
    }
    if !outcome.warning_findings.is_empty() {
        text.push_str("Warnings\n");
        for finding in &outcome.warning_findings {
            text.push_str(&format!(
                "  [{}] {}  {}\n",
                finding.severity.tag(),
                finding.rule_id,
                finding.observation.summary
            ));
        }
    }
    if let Some((at, manifest)) = &outcome.bundle {
        text.push_str(&format!("Bundle        {}\n", at.display()));
        for entry in &manifest.files {
            text.push_str(&format!("  {}  sha256 {}\n", entry.name, entry.sha256));
        }
    }

    emit(json, &payload, &text);

    if outcome.verdict == ValidationResult::Fail {
        std::process::exit(2);
    }

    Ok(())
}

/// Shows or checks a delivery profile (spec §69, §70).
fn run_profile(action: &ProfileAction, json: bool) -> anyhow::Result<()> {
    match action {
        ProfileAction::Show { path } | ProfileAction::Check { path } => {
            let profile = load_profile(path)?;
            let payload = serde_json::json!({
                "name": profile.name,
                "version": profile.version,
                "identifier": profile.identifier(),
                "fingerprint": profile.fingerprint(),
                "requirements": profile.requirements.iter().map(|r| serde_json::json!({
                    "id": r.id(),
                    "expected": r.expected_text(),
                })).collect::<Vec<_>>(),
            });

            let mut text = String::new();
            text.push_str(&format!("Profile       {}\n", profile.identifier()));
            text.push_str(&format!("Name          {}\n", profile.name));
            text.push_str(&format!("Version       {}\n", profile.version));
            text.push_str(&format!("Fingerprint   {}\n", profile.fingerprint()));
            text.push_str(&format!("Requirements  {}\n", profile.requirements.len()));
            for requirement in &profile.requirements {
                text.push_str(&format!(
                    "  {:<24} {}\n",
                    requirement.id(),
                    requirement.expected_text()
                ));
            }

            emit(json, &payload, &text);
            Ok(())
        }

        ProfileAction::Template { out } => {
            // Refuses rather than overwrites. A profile is a specification a
            // customer maintains across versions (spec §70), and overwriting one
            // because someone asked for a template would destroy the record of
            // what the previous version actually required.
            if out.exists() {
                anyhow::bail!(
                    "{} already exists; refusing to overwrite an existing profile. Write a \
                     new file, or bump the version of the existing one deliberately.",
                    out.display()
                );
            }

            // Parsed before writing, so a template that does not load is caught
            // here rather than by the analyst who later runs it against a delivery.
            let _: tpt_app_media_forensics_model::DeliveryProfile =
                serde_json::from_str(DELIVERY_PROFILE_TEMPLATE)
                    .context("the built-in template does not parse")?;

            std::fs::write(out, DELIVERY_PROFILE_TEMPLATE)
                .with_context(|| format!("writing {}", out.display()))?;
            println!("Profile       {}", out.display());
            println!(
                "Edit the requirements, bump `version` when they change (spec §70), then:\n  \
                 tpt-media-forensics validate <file> --profile {}",
                out.display()
            );
            Ok(())
        }
    }
}

/// Spec §68's example specification, as a starting point for `profile template`.
///
/// JSON rather than the YAML the spec illustrates. `serde_json` is already a
/// dependency of every crate here, and a parser added for one config file would be
/// the one dependency in this project not pinned to a revision — which is the
/// trade spec §63 and §77 exist to prevent. The structure is identical either way.
const DELIVERY_PROFILE_TEMPLATE: &str = r#"{
  "name": "Delivery",
  "version": 1,
  "requirements": [
    { "kind": "video_codec", "any_of": ["h264"] },
    { "kind": "video_resolution", "width": 1920, "height": 1080 },
    { "kind": "frame_rate", "fps": 25.0, "tolerance": 0.5 },
    { "kind": "audio_channels", "channels": 2 },
    { "kind": "audio_sample_rate", "sample_rate": 48000 },
    { "kind": "container_format", "any_of": ["mov"] }
  ]
}
"#;

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
