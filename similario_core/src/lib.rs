pub mod audio;
pub mod cache;
pub mod compare;
pub mod metadata;
pub mod process_utils;
pub mod thumbnail;
pub mod visual;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub use compare::{CompareConfig, SimilarGroup, SimilarityKind, SimilarityResult};
pub use metadata::VideoMetadata;
use rayon::prelude::*;
pub use visual::{SignatureConfig, SignatureError, VideoSignature};

/// Result of scanning a single file.
pub enum ScanOutcome {
    /// Signature loaded from cache.
    Cached(VideoSignature),
    /// Signature computed and saved to cache.
    Computed(VideoSignature),
    /// Error (file skipped).
    Error(PathBuf, String),
}

/// Result of cache pre-check: separates cached signatures from paths that need processing.
pub struct CacheCheckResult {
    /// Signatures loaded from cache.
    pub cached: Vec<VideoSignature>,
    /// Paths that have no valid cache entry and need processing.
    pub uncached: Vec<PathBuf>,
}

/// Pre-checks the cache for all paths. Fast (no ffmpeg), single-threaded I/O.
/// Returns cached signatures and the list of paths that still need processing.
///
/// The cache key includes `SignatureConfig` parameters, so changing e.g.
/// `window_count` or `skip_secs` will treat all entries as uncached.
pub fn check_cache(paths: &[PathBuf], sig_config: &SignatureConfig) -> CacheCheckResult {
    let mut cached = Vec::new();
    let mut uncached = Vec::new();
    for path in paths {
        if let Some(sig) = cache::load(path, sig_config) {
            cached.push(sig);
        } else {
            uncached.push(path.clone());
        }
    }
    CacheCheckResult { cached, uncached }
}

/// Computes signatures for the given paths in parallel using rayon.
///
/// Calls `progress(done, total)` after each file.
/// Checks `stop_flag` before processing each file and during frame extraction.
pub fn compute_signatures<F>(
    paths: &[PathBuf],
    sig_config: &SignatureConfig,
    use_cache: bool,
    stop_flag: &Arc<AtomicBool>,
    progress: F,
) -> Vec<ScanOutcome>
where
    F: Fn(usize, usize) + Send,
{
    let total = paths.len();
    let done = AtomicUsize::new(0);
    let progress = Mutex::new(progress);

    paths
        .par_iter()
        .map(|path| {
            if stop_flag.load(Ordering::Relaxed) {
                return ScanOutcome::Error(path.clone(), "interrupted".into());
            }

            let start = Instant::now();
            let outcome = match VideoSignature::from_path(path, sig_config, stop_flag) {
                Ok(sig) => {
                    log_file_processed(path, &sig, start.elapsed());
                    if use_cache {
                        let _ = cache::save(path, &sig, sig_config);
                    }
                    ScanOutcome::Computed(sig)
                }
                Err(e) => {
                    log::warn!("Error processing {}: {e}", path.display());
                    ScanOutcome::Error(path.clone(), e.to_string())
                }
            };

            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            if let Ok(cb) = progress.lock() {
                cb(d, total);
            }
            outcome
        })
        .collect()
}

fn log_file_processed(path: &Path, sig: &VideoSignature, elapsed: std::time::Duration) {
    let file_size = std::fs::metadata(path).map_or(0, |m| m.len());
    let size_mb = file_size as f64 / (1024.0 * 1024.0);
    let dur_secs = sig.duration_ms as f64 / 1000.0;

    let resolution = sig
        .metadata
        .as_ref()
        .and_then(|m| Some(format!("{}x{}", m.width?, m.height?)))
        .unwrap_or_else(|| "?x?".into());

    log::info!(
        "Processed in {:.2}s: {} - {:.1} MB, {resolution}, {:.1}s duration",
        elapsed.as_secs_f64(),
        path.display(),
        size_mb,
        dur_secs,
    );
}

/// Recursively collects video files from a directory.
/// Calls `progress(found_count)` each time a new video file is found.
pub fn collect_video_files(dir: &Path, progress: impl FnMut(usize)) -> Vec<PathBuf> {
    const VIDEO_EXTS: &[&str] = &[
        "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "mpg", "mpeg", "ts", "mts", "m2ts", "vob", "3gp",
        "ogv", "rm", "rmvb", "divx",
    ];

    let mut result = Vec::new();
    let mut progress = progress;
    collect_recursive(dir, VIDEO_EXTS, &mut result, &mut progress);
    result
}

fn collect_recursive(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>, progress: &mut impl FnMut(usize)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        // Use entry.file_type() instead of path.is_dir() to avoid extra stat syscalls.
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if ft.is_dir() {
            collect_recursive(&path, exts, out, progress);
        } else if ft.is_file()
            && let Some(ext) = path.extension().and_then(|e| e.to_str())
            && exts.iter().any(|&e| e.eq_ignore_ascii_case(ext))
        {
            out.push(path);
            progress(out.len());
        }
    }
}
