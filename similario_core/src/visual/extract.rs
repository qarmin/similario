//! Frame extraction via ffmpeg - one process per temporal window.
//!
//! Each window gets its own ffmpeg process with `-ss` before `-i` for fast
//! keyframe-based seeking, avoiding full decode from the start of the file.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use image::GrayImage;
use thiserror::Error;

use super::watchdog::{self, KillReason};
use super::{DCT_SIZE, FfmpegTimeout, cropdetect, is_black_frame};

#[derive(Debug, Error)]
pub enum FrameExtractError {
    #[error("ffmpeg not found in PATH")]
    FfmpegNotFound,
    #[error("ffmpeg failed: {0}")]
    FfmpegFailed(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Not enough valid frames in window {window} (got {got})")]
    InsufficientFrames { window: usize, got: usize },
    #[error("Extraction stopped")]
    Stopped,
    #[error("ffmpeg timed out after {seconds}s - file likely corrupt or stuck")]
    TimedOut { seconds: u64 },
}

const FRAME_W: u32 = DCT_SIZE as u32;
const FRAME_H: u32 = DCT_SIZE as u32;
const FRAME_BYTES: usize = (FRAME_W * FRAME_H) as usize; // 256

/// Extracts frames for N windows - one ffmpeg process per window with fast seek.
///
/// Returns `windows.len() * frames_per_window` grayscale 16×16 frames,
/// laid out sequentially: [win0_fr0..win0_fr15, win1_fr0..win1_fr15, ...].
///
/// Black frames are replaced by neighboring frames (or zero frames if none exist).
pub fn extract_frames_multi_window(
    path: &Path,
    windows: &[(f64, f64)],
    frames_per_window: usize,
    cropdetect_enabled: bool,
    timeout: FfmpegTimeout,
    stop_flag: &Arc<AtomicBool>,
) -> Result<Vec<GrayImage>, FrameExtractError> {
    let n_windows = windows.len();
    let total_frames = n_windows * frames_per_window;

    // Optional letterbox detection - pre-scan a few frames at moderate resolution.
    let crop_filter = if cropdetect_enabled {
        detect_crop(path, windows, timeout, stop_flag)?
    } else {
        String::new()
    };

    let mut all_frames: Vec<GrayImage> = Vec::with_capacity(total_frames);

    for (win_idx, (start, end)) in windows.iter().enumerate() {
        if stop_flag.load(Ordering::Relaxed) {
            return Err(FrameExtractError::Stopped);
        }

        let duration = end - start;
        let fps = frames_per_window as f64 / duration.max(0.1);

        let filter = format!("fps={fps:.4},{crop_filter}scale={FRAME_W}:{FRAME_H}:flags=bilinear,format=gray");

        let raw = run_ffmpeg_seeked(path, *start, duration, &filter, frames_per_window, timeout, stop_flag)?;

        for chunk in raw.chunks(FRAME_BYTES) {
            if chunk.len() < FRAME_BYTES {
                break;
            }
            let img = GrayImage::from_raw(FRAME_W, FRAME_H, chunk.to_vec()).expect("raw gray image from correct size");
            all_frames.push(img);
        }

        // Pad if ffmpeg returned fewer frames than expected for this window.
        let expected = (win_idx + 1) * frames_per_window;
        while all_frames.len() < expected {
            all_frames.push(GrayImage::new(FRAME_W, FRAME_H));
        }
    }

    // Replace black frames with neighbors (per window).
    replace_black_frames(&mut all_frames, frames_per_window);

    Ok(all_frames)
}

/// Resolution used for the cropdetect pre-scan.
const PRESCAN_W: u32 = 160;
const PRESCAN_H: u32 = 120;

/// Extracts a handful of moderate-resolution frames and runs letterbox
/// detection on them. Returns an ffmpeg `crop=…,` filter fragment
/// (with trailing comma) or an empty string if no letterbox was found.
#[expect(clippy::indexing_slicing, reason = "windows.len()/2 valid when non-empty")]
fn detect_crop(
    path: &Path,
    windows: &[(f64, f64)],
    timeout: FfmpegTimeout,
    stop_flag: &Arc<AtomicBool>,
) -> Result<String, FrameExtractError> {
    let (start, end) = windows[windows.len() / 2];
    let duration = end - start;
    let prescan_frames: usize = 8;
    let fps = prescan_frames as f64 / duration.max(0.1);

    let filter = format!("fps={fps:.4},scale={PRESCAN_W}:{PRESCAN_H}:flags=bilinear,format=gray");

    let raw = run_ffmpeg_seeked(path, start, duration, &filter, prescan_frames, timeout, stop_flag)?;
    let prescan_bytes = (PRESCAN_W * PRESCAN_H) as usize;

    let frames: Vec<GrayImage> = raw
        .chunks_exact(prescan_bytes)
        .filter_map(|c| GrayImage::from_raw(PRESCAN_W, PRESCAN_H, c.to_vec()))
        .collect();

    if frames.is_empty() {
        return Ok(String::new());
    }

    let crop = cropdetect::detect_letterbox_multi(&frames);
    if crop.is_empty() {
        return Ok(String::new());
    }

    let cx = crop.left as f64 / PRESCAN_W as f64;
    let cy = crop.top as f64 / PRESCAN_H as f64;
    let cw = 1.0 - (crop.left + crop.right) as f64 / PRESCAN_W as f64;
    let ch = 1.0 - (crop.top + crop.bottom) as f64 / PRESCAN_H as f64;

    Ok(format!("crop=iw*{cw:.4}:ih*{ch:.4}:iw*{cx:.4}:ih*{cy:.4},",))
}

/// Spawns ffmpeg with `-ss` BEFORE `-i` for fast keyframe seek, registers it
/// with the global watchdog, then reads its output.
fn run_ffmpeg_seeked(
    path: &Path,
    seek_secs: f64,
    duration: f64,
    vf_filter: &str,
    max_frames: usize,
    timeout: FfmpegTimeout,
    stop_flag: &Arc<AtomicBool>,
) -> Result<Vec<u8>, FrameExtractError> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-threads", "1"])
        // Fast seek BEFORE -i: jumps to nearest keyframe, near-instant.
        .arg("-ss")
        .arg(format!("{seek_secs:.3}"))
        .arg("-t")
        .arg(format!("{duration:.3}"))
        .arg("-i")
        .arg(path)
        .args([
            "-vf",
            vf_filter,
            "-vframes",
            &max_frames.to_string(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::process_utils::disable_windows_console_window(&mut cmd);
    let child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            FrameExtractError::FfmpegNotFound
        } else {
            FrameExtractError::Io(e)
        }
    })?;

    let timeout = timeout.for_duration(duration);
    let watched = watchdog::watch(child, timeout, stop_flag);

    let result = read_stdout_checked(watched.child(), max_frames * FRAME_BYTES);

    match watched.outcome() {
        Some(KillReason::Stopped) => Err(FrameExtractError::Stopped),
        Some(KillReason::TimedOut) => {
            log::warn!(
                "ffmpeg timed out after {}s extracting frames from {} (seek={seek_secs:.3}s, dur={duration:.3}s) - \
                 file is likely corrupt or got the process stuck",
                timeout.as_secs(),
                path.display(),
            );
            Err(FrameExtractError::TimedOut {
                seconds: timeout.as_secs(),
            })
        }
        None => result,
    }
}

/// Reads child stdout to EOF. Cancellation/timeout is handled externally by
/// the global watchdog (see `watchdog::watch`), which kills the child -
/// closing the pipe and unblocking the read - rather than this loop polling
/// a flag itself.
#[expect(clippy::indexing_slicing, reason = "buf[..n] bounded by read return value")]
fn read_stdout_checked(child: &Arc<Mutex<Child>>, capacity: usize) -> Result<Vec<u8>, FrameExtractError> {
    let mut stdout = child
        .lock()
        .expect("watchdog mutex poisoned")
        .stdout
        .take()
        .expect("stdout piped at spawn");
    let mut raw = Vec::with_capacity(capacity);
    let mut buf = [0u8; 4096];

    loop {
        match stdout.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
            Err(e) => return Err(FrameExtractError::Io(e)),
        }
    }
    drop(stdout);

    let mut child = child.lock().expect("watchdog mutex poisoned");
    let status = child.wait()?;
    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut stderr);
        }
        return Err(FrameExtractError::FfmpegFailed(stderr));
    }

    Ok(raw)
}

/// Replaces black frames with neighboring frames within each window.
#[expect(clippy::indexing_slicing, reason = "indices bounded by window/frame counts")]
fn replace_black_frames(frames: &mut [GrayImage], frames_per_window: usize) {
    let n_windows = frames.len() / frames_per_window;

    for w in 0..n_windows {
        let base = w * frames_per_window;
        let window_frames = &mut frames[base..base + frames_per_window];

        let fallback_idx = window_frames.iter().position(|f| !is_black_frame(f.as_raw()));

        for i in 0..frames_per_window {
            if !is_black_frame(window_frames[i].as_raw()) {
                continue;
            }
            let replacement_idx = (i + 1..frames_per_window)
                .find(|&j| !is_black_frame(window_frames[j].as_raw()))
                .or_else(|| (0..i).rev().find(|&j| !is_black_frame(window_frames[j].as_raw())))
                .or(fallback_idx);

            if let Some(src) = replacement_idx
                && src != i
            {
                let src_data = window_frames[src].as_raw().clone();
                window_frames[i] = GrayImage::from_raw(FRAME_W, FRAME_H, src_data).expect("same dimensions");
            }
        }
    }
}
