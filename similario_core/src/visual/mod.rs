pub(crate) mod cropdetect;
mod dct;
mod extract;
mod watchdog;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bitvec::array::BitArray;
use bitvec::order::Lsb0;
pub use extract::FrameExtractError;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::metadata::VideoMetadata;

/// DCT cube side length (16×16×16).
pub const DCT_SIZE: usize = 16;
/// Sub-cube side length (10×10×10 = 1000 hash bits).
pub const HASH_SIZE: usize = 10;
/// Number of bits in one window hash.
pub const HASH_BITS: usize = HASH_SIZE * HASH_SIZE * HASH_SIZE; // 1000

/// Default number of temporal windows spread across the video.
pub const DEFAULT_WINDOW_COUNT: usize = 5;
/// Seconds to skip at the start (intro, credits).
pub const DEFAULT_SKIP_SECS: f64 = 15.0;
/// Seconds extracted per window.
pub const DEFAULT_WINDOW_SECS: f64 = 6.0;
/// Frames per window (fed into 3D-DCT).
pub const FRAMES_PER_WINDOW: usize = 16;

/// Default per-call ffmpeg timeout policy values (see [`FfmpegTimeout`]).
pub const DEFAULT_FFMPEG_TIMEOUT_BASE_SECS: f64 = 10.0;
pub const DEFAULT_FFMPEG_TIMEOUT_FACTOR: f64 = 5.0;
pub const DEFAULT_FFMPEG_TIMEOUT_MIN_SECS: f64 = 60.0;
pub const DEFAULT_FFMPEG_TIMEOUT_MAX_SECS: f64 = 360.0;

/// Threshold for classifying a frame as black (≥80% pixels ≤ 0x20).
const BLACK_PIXEL_LIMIT: u8 = 0x20;
const BLACK_FRAME_THRESHOLD: f64 = 0.80;

/// 1000-bit hash of one temporal window. Stored in 128 bytes (1024 bits, 1000 used).
pub type HashBits = BitArray<[u8; 128], Lsb0>;

/// Visual hash of one temporal segment of a video.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemporalHash {
    /// Segment start position in ms.
    pub start_ms: u64,
    /// Segment end position in ms.
    pub end_ms: u64,
    /// 1000-bit 3D-DCT hash.
    #[serde(with = "bitvec_serde")]
    pub bits: HashBits,
}

impl TemporalHash {
    /// Hamming distance between two hashes (0-1000).
    pub fn hamming_distance(&self, other: &Self) -> u32 {
        hamming_bitwise_fast::hamming_bitwise_fast(self.bits.as_raw_slice(), other.bits.as_raw_slice()) as u32
    }

    /// Normalized Hamming distance (0.0 = identical, 1.0 = completely different).
    pub fn normalized_distance(&self, other: &Self) -> f32 {
        self.hamming_distance(other) as f32 / HASH_BITS as f32
    }
}

/// Visual + audio signature of one video file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoSignature {
    pub path: PathBuf,
    pub duration_ms: u64,
    pub aspect_ratio: f32,
    /// Visual hashes - one per temporal window.
    pub visual_hashes: Vec<TemporalHash>,
    /// Audio fingerprint - one u32 per second (optional).
    pub audio_fingerprint: Option<Vec<u32>>,
    /// Cached metadata from ffprobe (avoids re-probing).
    #[serde(default)]
    pub metadata: Option<crate::metadata::VideoMetadata>,
}

#[derive(Debug, Error)]
pub enum SignatureError {
    #[error("Frame extraction error: {0}")]
    FrameExtract(#[from] FrameExtractError),
    #[error("Not enough frames extracted (got {got}, need {need})")]
    NotEnoughFrames { got: usize, need: usize },
    #[error("Metadata error: {0}")]
    Metadata(#[from] crate::metadata::MetadataError),
}

/// Per-ffmpeg-call timeout policy used by the frame-extraction watchdog.
///
/// Each window's ffmpeg call is killed if it runs longer than
/// `window_duration * factor + base_secs`, clamped to `[min_secs, max_secs]`.
/// `-ss` before `-i` makes seeking near-instant, so decoding a window at
/// `-threads 1` should rarely take more than a few times real-time; the
/// floor/ceiling protect both very short and very long windows against a
/// wedged ffmpeg.
#[derive(Debug, Clone, Copy)]
pub struct FfmpegTimeout {
    /// Constant overhead added on top of the duration-scaled part.
    pub base_secs: f64,
    /// Multiplier applied to the window duration.
    pub factor: f64,
    /// Lower bound on the effective timeout.
    pub min_secs: f64,
    /// Upper bound on the effective timeout (the hard cancel-after-N-seconds cap).
    pub max_secs: f64,
}

impl Default for FfmpegTimeout {
    fn default() -> Self {
        Self {
            base_secs: DEFAULT_FFMPEG_TIMEOUT_BASE_SECS,
            factor: DEFAULT_FFMPEG_TIMEOUT_FACTOR,
            min_secs: DEFAULT_FFMPEG_TIMEOUT_MIN_SECS,
            max_secs: DEFAULT_FFMPEG_TIMEOUT_MAX_SECS,
        }
    }
}

impl FfmpegTimeout {
    /// Effective timeout for a window of `duration` seconds.
    pub fn for_duration(&self, duration: f64) -> Duration {
        let lo = self.min_secs.max(0.0);
        let hi = self.max_secs.max(lo);
        Duration::from_secs_f64((duration * self.factor + self.base_secs).clamp(lo, hi))
    }
}

/// Configuration for building a visual signature.
#[derive(Debug, Clone)]
pub struct SignatureConfig {
    /// Seconds to skip at the start.
    pub skip_secs: f64,
    /// Number of temporal windows (evenly spaced).
    pub window_count: usize,
    /// Duration of each window in seconds.
    pub window_secs: f64,
    /// Whether to apply letterbox cropdetect.
    pub cropdetect: bool,
    /// Whether to compute audio fingerprint (Chromaprint).
    pub audio_fingerprint: bool,
    /// Per-ffmpeg-call timeout policy for the extraction watchdog. Does not
    /// affect the computed hash, so it is intentionally excluded from the cache key.
    pub ffmpeg_timeout: FfmpegTimeout,
}

impl Default for SignatureConfig {
    fn default() -> Self {
        Self {
            skip_secs: DEFAULT_SKIP_SECS,
            window_count: DEFAULT_WINDOW_COUNT,
            window_secs: DEFAULT_WINDOW_SECS,
            cropdetect: true,
            audio_fingerprint: false,
            ffmpeg_timeout: FfmpegTimeout::default(),
        }
    }
}

impl VideoSignature {
    /// Builds a video signature from the given file path.
    #[expect(
        clippy::indexing_slicing,
        reason = "i bounded by windows.len(), frames pre-allocated"
    )]
    pub fn from_path(
        path: &Path,
        config: &SignatureConfig,
        stop_flag: &Arc<AtomicBool>,
    ) -> Result<Self, SignatureError> {
        if stop_flag.load(Ordering::Relaxed) {
            return Err(SignatureError::FrameExtract(extract::FrameExtractError::Stopped));
        }

        // 1. Fetch metadata.
        let meta = VideoMetadata::from_path(path)?;
        let duration_ms = (meta.duration_secs * 1000.0) as u64;
        let aspect_ratio = meta.aspect_ratio().unwrap_or(16.0 / 9.0);

        // 2. Compute window positions (in seconds).
        let windows = compute_window_positions(meta.duration_secs, config);

        // 3. Extract frames via one ffmpeg process for all windows.
        let all_frames = extract::extract_frames_multi_window(
            path,
            &windows,
            FRAMES_PER_WINDOW,
            config.cropdetect,
            config.ffmpeg_timeout,
            stop_flag,
        )?;

        // 4. Build one TemporalHash per window.
        let mut visual_hashes = Vec::with_capacity(windows.len());
        for (i, (start_secs, end_secs)) in windows.iter().enumerate() {
            let frames = &all_frames[i * FRAMES_PER_WINDOW..(i + 1) * FRAMES_PER_WINDOW];
            let hash_bits = dct::compute_hash_from_frames(frames);
            visual_hashes.push(TemporalHash {
                start_ms: (*start_secs * 1000.0) as u64,
                end_ms: (*end_secs * 1000.0) as u64,
                bits: hash_bits,
            });
        }

        // 5. Optional audio fingerprint.
        let audio_fingerprint = if config.audio_fingerprint {
            match crate::audio::compute_fingerprint(path, stop_flag) {
                Ok(Some(fp)) if !fp.is_empty() => Some(fp),
                Ok(_) => None,
                Err(e) => {
                    log::debug!("Audio fingerprint failed for {}: {e}", path.display());
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            path: path.to_path_buf(),
            duration_ms,
            aspect_ratio,
            visual_hashes,
            audio_fingerprint,
            metadata: Some(meta),
        })
    }
}

/// Computes window positions (start_secs, end_secs) evenly distributed across
/// the video, skipping the first `skip_secs`.
fn compute_window_positions(duration_secs: f64, config: &SignatureConfig) -> Vec<(f64, f64)> {
    let usable_start = config.skip_secs.min(duration_secs * 0.15);
    let usable_end = (duration_secs - 0.5).max(usable_start + 1.0);
    let usable = usable_end - usable_start;

    let n = config.window_count;
    let window_secs = config.window_secs.min(usable);
    let step = if n > 1 {
        (usable - window_secs) / (n - 1) as f64
    } else {
        0.0
    };

    (0..n)
        .map(|i| {
            let start = usable_start + i as f64 * step;
            let end = (start + window_secs).min(duration_secs - 0.1);
            // Ensure end > start (at least 0.5s window).
            let end = end.max(start + 0.5);
            (start, end)
        })
        .collect()
}

/// Returns true if ≥80% of frame pixels are at or below the black threshold.
pub(crate) fn is_black_frame(data: &[u8]) -> bool {
    let dark = data.iter().filter(|&&p| p <= BLACK_PIXEL_LIMIT).count();
    dark as f64 / data.len() as f64 >= BLACK_FRAME_THRESHOLD
}

/// Serde helper for HashBits (BitArray does not implement Serialize directly).
mod bitvec_serde {
    use bitvec::prelude::*;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::HashBits;

    pub fn serialize<S: Serializer>(bits: &HashBits, s: S) -> Result<S::Ok, S::Error> {
        bits.as_raw_slice().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<HashBits, D::Error> {
        let bytes = <Vec<u8>>::deserialize(d)?;
        let arr: [u8; 128] = bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("expected 128 bytes"))?;
        Ok(BitArray::new(arr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_positions_skip() {
        let cfg = SignatureConfig {
            skip_secs: 15.0,
            window_count: 5,
            window_secs: 6.0,
            cropdetect: false,
            audio_fingerprint: false,
            ..Default::default()
        };
        // 120s video: usable_start = min(15, 18) = 15, usable_end = 118
        let windows = compute_window_positions(120.0, &cfg);
        assert_eq!(windows.len(), 5);
        // First window starts after 15s
        assert!(windows[0].0 >= 14.9);
        // No window exceeds the video end
        for (_, end) in &windows {
            assert!(*end < 120.0);
        }
    }

    #[test]
    fn window_positions_short_video() {
        let cfg = SignatureConfig {
            skip_secs: 15.0,
            window_count: 3,
            window_secs: 4.0,
            cropdetect: false,
            audio_fingerprint: false,
            ..Default::default()
        };
        // Short 20s video: skip reduced to 3s (15% of 20s)
        let windows = compute_window_positions(20.0, &cfg);
        assert_eq!(windows.len(), 3);
        for (start, end) in &windows {
            assert!(start < end);
            assert!(*end < 20.0);
        }
    }

    #[test]
    #[expect(clippy::float_cmp, reason = "exact zero expected for identical hashes")]
    fn hamming_identical() {
        let h = TemporalHash {
            start_ms: 0,
            end_ms: 6000,
            bits: HashBits::ZERO,
        };
        assert_eq!(h.hamming_distance(&h), 0);
        assert_eq!(h.normalized_distance(&h), 0.0);
    }

    #[test]
    fn ffmpeg_timeout_scales_and_clamps() {
        let t = FfmpegTimeout {
            base_secs: 10.0,
            factor: 5.0,
            min_secs: 20.0,
            max_secs: 120.0,
        };
        // Below the floor: 1*5+10 = 15 -> clamped up to min.
        assert_eq!(t.for_duration(1.0), Duration::from_secs(20));
        // Within range: 6*5+10 = 40.
        assert_eq!(t.for_duration(6.0), Duration::from_secs(40));
        // Above the ceiling: 100*5+10 = 510 -> clamped down to max.
        assert_eq!(t.for_duration(100.0), Duration::from_secs(120));
    }

    #[test]
    fn ffmpeg_timeout_handles_inverted_bounds() {
        // max < min should not panic; the floor wins.
        let t = FfmpegTimeout {
            base_secs: 0.0,
            factor: 0.0,
            min_secs: 30.0,
            max_secs: 10.0,
        };
        assert_eq!(t.for_duration(5.0), Duration::from_secs(30));
    }

    #[test]
    fn is_black_frame_detection() {
        let black = vec![0x10u8; 256];
        let bright = vec![0xFFu8; 256];
        assert!(is_black_frame(&black));
        assert!(!is_black_frame(&bright));
    }
}
