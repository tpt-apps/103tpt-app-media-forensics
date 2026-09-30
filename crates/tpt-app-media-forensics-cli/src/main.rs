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
    ContainerFormat, detect_file, extension_matches, inspect_file,
};
use tpt_app_media_forensics_core::{acquire, CaseDirectory};
use tpt_app_media_forensics_model::{Case, MediaType};

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

        Command::Inspect { path } => {
            inspect(path, cli.json)
        }

        Command::Analyze { path, case_dir } => {
            // Fail fast if the case directory is not a case, so the analyst is
            // told now rather than after a long run.
            CaseDirectory::open(case_dir)
                .with_context(|| format!("{} is not an initialised case", case_dir.display()))?;
            let record = acquire(path)?;
            eprintln!("analyze: acquired {}", record.source_path);
            eprintln!(
                "analyze: the analysis engine is not implemented yet \
                 (Phase 1, spec \u{a7}97)"
            );
            Ok(())
        }

        Command::Report { case_dir, out } => {
            CaseDirectory::open(case_dir)
                .with_context(|| format!("{} is not an initialised case", case_dir.display()))?;
            eprintln!("report: writing {} ", out.display());
            eprintln!("report: rendering is not implemented yet (Phase 1, spec \u{a7}59-63)");
            Ok(())
        }

        Command::Batch {
            directory,
            case_dir,
        } => {
            CaseDirectory::open(case_dir)
                .with_context(|| format!("{} is not an initialised case", case_dir.display()))?;
            if !directory.is_dir() {
                anyhow::bail!("{} is not a directory", directory.display());
            }
            eprintln!("batch: scanning {}", directory.display());
            eprintln!(
                "batch: the analysis engine is not implemented yet \
                 (Phase 1, spec \u{a7}48-49)"
            );
            Ok(())
        }
    }
}

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
    let format =
        detect_file(path).with_context(|| format!("cannot read {}", path.display()))?;
    let extension_ok = extension_matches(path, format);

    let streams = match format {
        ContainerFormat::IsoBmff => {
            let inspection = inspect_file(path)
                .with_context(|| format!("cannot inspect {}", path.display()))?;
            for anomaly in &inspection.anomalies {
                eprintln!("anomaly: {anomaly}");
            }
            inspection.streams
        }
        other => {
            // Detection works for every supported signature; deep parsing is
            // implemented per-format as each demuxer is integrated. Say which
            // format was seen rather than reporting an empty inspection.
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
            if extension_ok { "matches" } else { "DOES NOT MATCH" }
        );
        println!("Streams       {}", streams.len());
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
