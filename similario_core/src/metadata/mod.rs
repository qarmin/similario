use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MetadataError {
    #[error("ffprobe not found in PATH")]
    FfprobeNotFound,
    #[error("ffprobe failed on '{path}': {stderr}")]
    FfprobeFailed { path: String, stderr: String },
    #[error("Failed to parse ffprobe output: {0}")]
    ParseError(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Video metadata fetched via ffprobe.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VideoMetadata {
    /// Duration in seconds.
    pub duration_secs: f64,
    /// Frames per second (None if unknown).
    pub fps: Option<f64>,
    /// Codec name (e.g. "h264", "hevc", "vp9").
    pub codec: Option<String>,
    /// Bitrate in bits/s (None if unknown).
    pub bitrate_bps: Option<u64>,
    /// Frame width in pixels.
    pub width: Option<u32>,
    /// Frame height in pixels.
    pub height: Option<u32>,
}

impl VideoMetadata {
    /// Fetches video metadata via the ffprobe CLI.
    pub fn from_path(path: &Path) -> Result<Self, MetadataError> {
        let mut cmd = Command::new("ffprobe");
        cmd.args([
            "-v",
            "error",
            "-show_format",
            "-show_streams",
            "-select_streams",
            "v:0",
            "-print_format",
            "json",
        ])
        .arg(path);
        crate::process_utils::disable_windows_console_window(&mut cmd);
        let output = cmd.output().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                MetadataError::FfprobeNotFound
            } else {
                MetadataError::Io(e)
            }
        })?;

        if !output.status.success() {
            return Err(MetadataError::FfprobeFailed {
                path: path.display().to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }

        let json: Value =
            serde_json::from_slice(&output.stdout).map_err(|e| MetadataError::ParseError(e.to_string()))?;

        Self::parse_ffprobe_json(&json)
    }

    #[expect(
        clippy::indexing_slicing,
        reason = "serde_json indexing returns Value::Null for missing keys"
    )]
    fn parse_ffprobe_json(json: &Value) -> Result<Self, MetadataError> {
        // Duration: prefer video stream, fall back to format.
        let duration_secs = parse_duration_secs(
            json["streams"][0]["duration"]
                .as_str()
                .or_else(|| json["format"]["duration"].as_str()),
        )
        .ok_or_else(|| MetadataError::ParseError("no duration found".into()))?;

        // FPS: avg_frame_rate or r_frame_rate ("num/den" format).
        let fps = json["streams"][0]["avg_frame_rate"]
            .as_str()
            .or_else(|| json["streams"][0]["r_frame_rate"].as_str())
            .and_then(parse_fps_fraction);

        // Video codec.
        let codec = json["streams"][0]["codec_name"].as_str().map(str::to_owned);

        // Bitrate: from stream, fallback to format.
        let bitrate_bps = json["streams"][0]["bit_rate"]
            .as_str()
            .or_else(|| json["format"]["bit_rate"].as_str())
            .and_then(|s| s.parse::<u64>().ok());

        // Dimensions capped at 16384 px.
        const MAX_DIM: u32 = 16_384;
        let width = json["streams"][0]["width"].as_u64().map(|w| (w as u32).min(MAX_DIM));
        let height = json["streams"][0]["height"].as_u64().map(|h| (h as u32).min(MAX_DIM));

        Ok(Self {
            duration_secs,
            fps,
            codec,
            bitrate_bps,
            width,
            height,
        })
    }

    /// Duration as a [`Duration`].
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.duration_secs)
    }

    /// Aspect ratio (width / height), or None if dimensions are unknown.
    pub fn aspect_ratio(&self) -> Option<f32> {
        match (self.width, self.height) {
            (Some(w), Some(h)) if h > 0 => Some(w as f32 / h as f32),
            _ => None,
        }
    }
}

/// Parses duration from ffprobe: float seconds or "HH:MM:SS.mmm".
fn parse_duration_secs(s: Option<&str>) -> Option<f64> {
    let s = s?.trim();

    // Try plain float (typical ffprobe format: "3723.123456").
    if let Ok(v) = s.parse::<f64>() {
        return if v.is_finite() && v >= 0.0 { Some(v) } else { None };
    }

    // HH:MM:SS.mmm or HH:MM:SS
    let parts: Vec<&str> = s.split(':').collect();
    match parts.as_slice() {
        [hh, mm, ss] => {
            let h = hh.parse::<f64>().ok()?;
            let m = mm.parse::<f64>().ok()?;
            let s = ss.parse::<f64>().ok()?;
            Some(h * 3600.0 + m * 60.0 + s)
        }
        [mm, ss] => {
            let m = mm.parse::<f64>().ok()?;
            let s = ss.parse::<f64>().ok()?;
            Some(m * 60.0 + s)
        }
        _ => None,
    }
}

/// Parses FPS from "num/den" format (e.g. "30000/1001" → 29.97).
fn parse_fps_fraction(s: &str) -> Option<f64> {
    let mut parts = s.split('/');
    let num = parts.next()?.trim().parse::<f64>().ok()?;
    let den = parts.next()?.trim().parse::<f64>().ok()?;
    if den == 0.0 || !num.is_finite() {
        return None;
    }
    let fps = num / den;
    if fps > 0.0 && fps < 1000.0 { Some(fps) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_float() {
        assert_eq!(parse_duration_secs(Some("3723.456")), Some(3723.456));
    }

    #[test]
    fn parse_duration_hhmmss() {
        // 1h 2m 3.5s = 3723.5
        assert_eq!(parse_duration_secs(Some("01:02:03.500")), Some(3723.5));
    }

    #[test]
    fn parse_duration_mmss() {
        assert_eq!(parse_duration_secs(Some("02:30.000")), Some(150.0));
    }

    #[test]
    fn parse_duration_none() {
        assert_eq!(parse_duration_secs(None), None);
        assert_eq!(parse_duration_secs(Some("N/A")), None);
    }

    #[test]
    fn parse_fps_normal() {
        assert!((parse_fps_fraction("30000/1001").unwrap() - 29.97).abs() < 0.01);
        assert_eq!(parse_fps_fraction("25/1"), Some(25.0));
        assert_eq!(parse_fps_fraction("0/0"), None);
    }

    #[test]
    fn parse_json_full() {
        let json = serde_json::json!({
            "streams": [{
                "codec_name": "h264",
                "width": 1920,
                "height": 1080,
                "duration": "120.5",
                "avg_frame_rate": "30000/1001",
                "bit_rate": "5000000"
            }],
            "format": {}
        });
        let meta = VideoMetadata::parse_ffprobe_json(&json).unwrap();
        assert!((meta.duration_secs - 120.5).abs() < 0.001);
        assert_eq!(meta.codec.as_deref(), Some("h264"));
        assert_eq!(meta.width, Some(1920));
        assert_eq!(meta.height, Some(1080));
        assert_eq!(meta.bitrate_bps, Some(5_000_000));
        assert!((meta.fps.unwrap() - 29.97).abs() < 0.01);
    }

    #[test]
    #[expect(clippy::float_cmp, reason = "exact float comparison intended in test")]
    fn duration_fallback_to_format() {
        let json = serde_json::json!({
            "streams": [{}],
            "format": { "duration": "60.0" }
        });
        let meta = VideoMetadata::parse_ffprobe_json(&json).unwrap();
        assert_eq!(meta.duration_secs, 60.0);
    }
}
