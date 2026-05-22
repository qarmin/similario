//! Video signature cache - bincode + blake3.
//!
//! Cache key: blake3(path + size + mtime + SignatureConfig) → .bin file
//! Location: ~/.cache/similario/signatures/
//! Atomic write: *.tmp → rename.
//! Versioning: CACHE_VERSION in header - old files are ignored.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::visual::{SignatureConfig, VideoSignature};

/// Cache format version. Changing this causes old files to be ignored.
const CACHE_VERSION: u8 = 1;

/// Cache file header (version byte + bincode payload).
#[derive(Serialize, Deserialize)]
struct CacheEntry {
    version: u8,
    signature: VideoSignature,
}

/// Returns the cache directory for signatures.
pub fn cache_dir() -> PathBuf {
    dirs_cache()
        .unwrap_or_else(|| PathBuf::from(".similario_cache"))
        .join("similario")
        .join("signatures")
}

fn dirs_cache() -> Option<PathBuf> {
    // Try XDG_CACHE_HOME, then ~/.cache
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
}

/// Computes the cache key for the given video file.
/// Includes `SignatureConfig` parameters that affect the computed hash,
/// so changing e.g. `window_count` or `skip_secs` produces a different key.
fn cache_key(path: &Path, size: u64, mtime_secs: u64, config: &SignatureConfig) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update(&size.to_le_bytes());
    hasher.update(&mtime_secs.to_le_bytes());
    hasher.update(&[CACHE_VERSION]);
    // Signature parameters that affect the computed hash.
    hasher.update(&config.skip_secs.to_le_bytes());
    hasher.update(&config.window_count.to_le_bytes());
    hasher.update(&config.window_secs.to_le_bytes());
    hasher.update(&[u8::from(config.cropdetect)]);
    hasher.update(&[u8::from(config.audio_fingerprint)]);
    hasher.finalize().to_hex().to_string()
}

fn cache_path(key: &str) -> PathBuf {
    cache_dir().join(format!("{key}.bin"))
}

/// Tries to load a signature from cache.
/// Returns `None` if the cache entry is missing or outdated.
pub fn load(path: &Path, config: &SignatureConfig) -> Option<VideoSignature> {
    let (size, mtime) = file_stats(path)?;
    let key = cache_key(path, size, mtime, config);
    let cache_file = cache_path(&key);

    let bytes = fs::read(&cache_file).ok()?;

    let entry: CacheEntry = bincode::deserialize(&bytes).ok()?;
    if entry.version != CACHE_VERSION {
        return None;
    }

    // Refresh cache file mtime (LRU-style).
    let _ = filetime::set_file_mtime(&cache_file, filetime::FileTime::now());

    Some(entry.signature)
}

/// Saves a signature to cache (atomic write via tmp file).
pub fn save(path: &Path, signature: &VideoSignature, config: &SignatureConfig) -> io::Result<()> {
    let (size, mtime) =
        file_stats(path).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cannot stat video file"))?;

    let dir = cache_dir();
    fs::create_dir_all(&dir)?;

    let key = cache_key(path, size, mtime, config);
    let final_path = cache_path(&key);
    let tmp_path = final_path.with_extension("tmp");

    let entry = CacheEntry {
        version: CACHE_VERSION,
        signature: signature.clone(),
    };
    let bytes = bincode::serialize(&entry).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    {
        let mut file = fs::File::create(&tmp_path)?;
        file.write_all(&bytes)?;
        file.flush()?;
    }

    fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

/// Returns (size in bytes, mtime as seconds since epoch) or None.
fn file_stats(path: &Path) -> Option<(u64, u64)> {
    let meta = fs::metadata(path).ok()?;
    let size = meta.len();
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((size, mtime))
}

/// Removes cache entries older than `max_age_days` days (LRU cleanup).
pub fn cleanup_old_entries(max_age_days: u64) {
    let dir = cache_dir();
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };

    let cutoff = SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(max_age_days * 86400))
        .unwrap_or(SystemTime::UNIX_EPOCH);

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("bin") {
            continue;
        }
        if let Ok(meta) = entry.metadata()
            && let Ok(mtime) = meta.modified()
            && mtime < cutoff
        {
            let _ = fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::NamedTempFile;

    use super::*;
    use crate::visual::VideoSignature;

    fn dummy_signature(path: &Path) -> VideoSignature {
        VideoSignature {
            path: path.to_path_buf(),
            duration_ms: 60_000,
            aspect_ratio: 1.777,
            visual_hashes: vec![],
            audio_fingerprint: None,
            metadata: None,
        }
    }

    #[test]
    fn save_and_load_roundtrip() {
        let tmp = NamedTempFile::new().unwrap();
        // Must have non-zero size so file_stats works.
        fs::write(tmp.path(), b"fake video content").unwrap();

        let sig = dummy_signature(tmp.path());

        // Test serialization logic directly without touching the real cache dir.
        let bytes = bincode::serialize(&CacheEntry {
            version: CACHE_VERSION,
            signature: sig,
        })
        .unwrap();
        let entry: CacheEntry = bincode::deserialize(&bytes).unwrap();

        assert_eq!(entry.version, CACHE_VERSION);
        assert_eq!(entry.signature.duration_ms, 60_000);
        assert!((entry.signature.aspect_ratio - 1.777).abs() < 0.001);
    }

    #[test]
    fn cache_key_changes_with_size() {
        let cfg = SignatureConfig::default();
        let k1 = cache_key(Path::new("/a/b.mp4"), 100, 0, &cfg);
        let k2 = cache_key(Path::new("/a/b.mp4"), 200, 0, &cfg);
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_changes_with_mtime() {
        let cfg = SignatureConfig::default();
        let k1 = cache_key(Path::new("/a/b.mp4"), 100, 1000, &cfg);
        let k2 = cache_key(Path::new("/a/b.mp4"), 100, 2000, &cfg);
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_changes_with_config() {
        let cfg1 = SignatureConfig::default();
        let cfg2 = SignatureConfig {
            window_count: 10,
            ..SignatureConfig::default()
        };
        let k1 = cache_key(Path::new("/a/b.mp4"), 100, 1000, &cfg1);
        let k2 = cache_key(Path::new("/a/b.mp4"), 100, 1000, &cfg2);
        assert_ne!(k1, k2, "different window_count should produce different keys");

        let cfg3 = SignatureConfig {
            skip_secs: 0.0,
            ..SignatureConfig::default()
        };
        let k3 = cache_key(Path::new("/a/b.mp4"), 100, 1000, &cfg3);
        assert_ne!(k1, k3, "different skip_secs should produce different keys");
    }
}
