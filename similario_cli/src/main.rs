use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::{Parser, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use similario_core::compare::{CompareConfig, find_similar};
use similario_core::{ScanOutcome, SignatureConfig, check_cache, collect_video_files, compute_signatures};

#[derive(Parser)]
#[command(name = "similario", version, about = "Detects similar video files")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scans directories and prints groups of similar files.
    Scan {
        /// Directories to search (recursively).
        #[arg(required = true)]
        dirs: Vec<PathBuf>,

        /// Hamming tolerance (0.0–1.0). Default: 0.30.
        #[arg(long, default_value_t = 0.30)]
        tolerance: f32,

        /// Output format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,

        /// Number of temporal windows per file.
        #[arg(long, default_value_t = 5)]
        windows: usize,

        /// Seconds to skip at the start of each file.
        #[arg(long, default_value_t = 15.0)]
        skip: f64,

        /// Do not use the signature cache.
        #[arg(long)]
        no_cache: bool,

        /// Disable letterbox (black bar) detection and cropping.
        #[arg(long)]
        no_cropdetect: bool,
    },

    /// Removes cached signatures older than the given number of days.
    CleanCache {
        #[arg(long, default_value_t = 30)]
        older_than_days: u64,
    },
}

#[derive(ValueEnum, Clone, Copy)]
enum OutputFormat {
    Text,
    Json,
}

#[expect(clippy::print_stdout, reason = "CLI output")]
fn main() {
    env_logger::init();
    let cli = Cli::parse();

    // Ctrl+C handler.
    let interrupted = Arc::new(AtomicBool::new(false));
    {
        let flag = interrupted.clone();
        ctrlc::set_handler(move || {
            flag.store(true, Ordering::SeqCst);
        })
        .expect("Failed to set Ctrl+C handler");
    }

    match cli.command {
        Command::Scan {
            dirs,
            tolerance,
            output,
            windows,
            skip,
            no_cache,
            no_cropdetect,
        } => {
            run_scan(
                dirs,
                tolerance,
                output,
                windows,
                skip,
                no_cache,
                no_cropdetect,
                interrupted,
            );
        }
        Command::CleanCache { older_than_days } => {
            similario_core::cache::cleanup_old_entries(older_than_days);
            println!("Cache cleaned (entries older than {older_than_days} days removed).");
        }
    }
}

#[expect(
    clippy::print_stderr,
    clippy::needless_pass_by_value,
    clippy::unwrap_used,
    reason = "CLI function"
)]
fn run_scan(
    dirs: Vec<PathBuf>,
    tolerance: f32,
    output: OutputFormat,
    window_count: usize,
    skip_secs: f64,
    no_cache: bool,
    no_cropdetect: bool,
    interrupted: Arc<AtomicBool>,
) {
    // 1. Collect files.
    eprint!("Collecting files... ");
    let mut paths: Vec<PathBuf> = dirs
        .iter()
        .flat_map(|d| {
            collect_video_files(d, |n| {
                eprint!("\rCollecting files... {n}");
            })
        })
        .collect();
    paths.sort();
    paths.dedup();
    eprintln!("\rCollecting files... {} video files.", paths.len());

    if paths.is_empty() {
        eprintln!("No video files to process.");
        return;
    }

    // 2. Check cache first.
    let sig_config = SignatureConfig {
        skip_secs,
        window_count,
        window_secs: 6.0,
        cropdetect: !no_cropdetect,
        audio_fingerprint: false,
    };

    let use_cache = !no_cache;
    let (mut signatures, uncached) = if use_cache {
        eprint!("Checking cache... ");
        let cache_result = check_cache(&paths, &sig_config);
        eprintln!(
            "{} cached, {} to process.",
            cache_result.cached.len(),
            cache_result.uncached.len()
        );
        (cache_result.cached, cache_result.uncached)
    } else {
        (Vec::new(), paths)
    };

    // 3. Compute signatures for uncached files.
    let pb = ProgressBar::new(uncached.len() as u64);
    pb.set_style(ProgressStyle::with_template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {msg}").unwrap());

    let outcomes = compute_signatures(&uncached, &sig_config, use_cache, &interrupted, |done, _total| {
        pb.set_position(done as u64);
    });

    pb.finish_and_clear();

    if interrupted.load(Ordering::SeqCst) {
        eprintln!("Interrupted by user.");
        return;
    }

    let mut errors = 0usize;
    for outcome in outcomes {
        match outcome {
            ScanOutcome::Computed(sig) | ScanOutcome::Cached(sig) => signatures.push(sig),
            ScanOutcome::Error(path, err) => {
                eprintln!("ERROR: {} - {}", path.display(), err);
                errors += 1;
            }
        }
    }

    eprintln!("Signatures: {} OK, {} errors.", signatures.len(), errors);

    if signatures.len() < 2 {
        eprintln!("Not enough files to compare.");
        return;
    }

    // 3. Compare.
    eprint!("Comparing... ");
    let compare_cfg = CompareConfig {
        tolerance,
        ..Default::default()
    };
    let groups = find_similar(&signatures, &compare_cfg);
    eprintln!("{} groups of similar files.", groups.len());

    // 4. Print results.
    match output {
        OutputFormat::Text => print_text(&groups),
        OutputFormat::Json => print_json(&groups),
    }
}

#[expect(clippy::print_stdout, reason = "CLI output")]
fn print_text(groups: &[similario_core::compare::SimilarGroup]) {
    for (i, group) in groups.iter().enumerate() {
        let kind = match &group.kind {
            similario_core::SimilarityKind::Identical => "IDENTICAL",
            similario_core::SimilarityKind::SameContent => "SAME CONTENT",
            similario_core::SimilarityKind::SubClip { .. } => "SUB-CLIP",
            similario_core::SimilarityKind::Similar => "SIMILAR",
        };
        println!("--- Group {} [{kind}] ---", i + 1);
        for path in &group.files {
            println!("  {}", path.display());
        }
    }
}

#[expect(clippy::print_stdout, clippy::print_stderr, reason = "CLI output")]
fn print_json(groups: &[similario_core::compare::SimilarGroup]) {
    match serde_json::to_string_pretty(groups) {
        Ok(json) => println!("{json}"),
        Err(e) => eprintln!("JSON serialization error: {e}"),
    }
}
