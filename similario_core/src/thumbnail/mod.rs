//! Thumbnail extraction via ffmpeg - single frame as PNG piped to stdout.

use std::path::Path;
use std::process::Command;

use image::RgbImage;

/// Extracts a single RGB thumbnail from a video at the given timestamp (seconds).
/// Optionally scales down to fit within `max_w × max_h` preserving aspect ratio.
pub fn extract_thumbnail(path: &Path, timestamp_secs: f32, max_w: u32, max_h: u32) -> Result<RgbImage, String> {
    let vf = format!("scale='min({max_w},iw)':'min({max_h},ih)':force_original_aspect_ratio=decrease");

    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-threads", "1"])
        .arg("-ss")
        .arg(format!("{timestamp_secs:.3}"))
        .arg("-i")
        .arg(path)
        .arg("-vf")
        .arg(&vf)
        .args(["-vframes", "1", "-f", "image2pipe", "-vcodec", "png", "pipe:1"])
        .output()
        .map_err(|e| format!("ffmpeg spawn: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg failed: {stderr}"));
    }

    let img = image::load_from_memory(&output.stdout).map_err(|e| format!("decode thumbnail: {e}"))?;
    Ok(img.into_rgb8())
}
