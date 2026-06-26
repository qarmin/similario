//! Signature comparison and group clustering.
//!
//! Pipeline:
//! 1. Duration buckets (±20%) - reduces O(n²) to O(n·k)
//! 2. Pre-filter: compare middle window
//! 3. Full visual comparison (all windows)
//! 4. Sliding window → SubClip detection
//! 5. Grouping with representative (anti-daisy-chain)

use std::path::PathBuf;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::visual::{TemporalHash, VideoSignature};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SimilarityKind {
    /// Identical hashes - both files are likely copies of the same content.
    Identical,
    /// Same content, different format/codec/resolution.
    SameContent,
    /// One file is a subclip of the other.
    SubClip {
        /// Offset in ms - where in the source the clip starts.
        offset_ms: u64,
        /// Which file is the shorter clip.
        clip_is: WhichFile,
    },
    /// General visual similarity.
    Similar,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WhichFile {
    A,
    B,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimilarityResult {
    pub path_a: PathBuf,
    pub path_b: PathBuf,
    pub kind: SimilarityKind,
    /// Visual similarity score: 0.0 (different) - 1.0 (identical).
    pub visual_score: f32,
}

/// A group of similar files (≥2 members).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimilarGroup {
    pub files: Vec<PathBuf>,
    pub kind: SimilarityKind,
}

#[derive(Debug, Clone)]
pub struct CompareConfig {
    /// Hamming tolerance (0.0-1.0). Default: 0.30 (30% differing bits).
    pub tolerance: f32,
    /// Duration filter tolerance (±%). Default: 20%.
    pub duration_tolerance_pct: f64,
    /// Minimum fraction of windows that must match (SameContent). Default: 0.6.
    pub min_matching_windows: f32,
    /// Minimum fraction of clip windows that must match (SubClip). Default: 0.5.
    pub subclip_min_match: f32,
    /// Whether to use audio fingerprints in comparison (when available).
    pub use_audio: bool,
    /// Chromaprint maximum segment score to consider a match (lower = stricter). Default: 3.0.
    pub audio_max_difference: f64,
    /// Minimum matching segment duration in seconds. Default: 5.0.
    pub audio_min_segment_duration: f32,
}

impl Default for CompareConfig {
    fn default() -> Self {
        Self {
            tolerance: 0.30,
            duration_tolerance_pct: 20.0,
            min_matching_windows: 0.6,
            subclip_min_match: 0.5,
            use_audio: false,
            audio_max_difference: 3.0,
            audio_min_segment_duration: 5.0,
        }
    }
}

/// Normalized Hamming distance (0.0 = identical, 1.0 = completely different).
fn hamming_norm(a: &TemporalHash, b: &TemporalHash) -> f32 {
    a.normalized_distance(b)
}

#[derive(Debug)]
struct PairResult {
    visual_score: f32,
    kind: SimilarityKind,
}

/// Compares two signatures. Returns None if they don't match.
fn compare_pair(a: &VideoSignature, b: &VideoSignature, cfg: &CompareConfig) -> Option<PairResult> {
    if cfg.use_audio {
        compare_pair_audio(a, b, cfg)
    } else {
        compare_pair_visual(a, b, cfg)
    }
}

/// Audio-primary comparison: match by audio fingerprint, visual is ignored.
fn compare_pair_audio(a: &VideoSignature, b: &VideoSignature, cfg: &CompareConfig) -> Option<PairResult> {
    let audio_score = compare_audio(a, b, cfg)?;
    if audio_score <= 0.0 {
        return None;
    }

    let kind = if audio_score >= 0.95 {
        SimilarityKind::Identical
    } else {
        SimilarityKind::SameContent
    };

    Some(PairResult {
        visual_score: audio_score,
        kind,
    })
}

/// Visual-primary comparison: match by visual hashes.
#[expect(clippy::indexing_slicing, reason = "indices bounded by is_empty/len checks")]
fn compare_pair_visual(a: &VideoSignature, b: &VideoSignature, cfg: &CompareConfig) -> Option<PairResult> {
    let ha = &a.visual_hashes;
    let hb = &b.visual_hashes;

    if ha.is_empty() || hb.is_empty() {
        return None;
    }

    // Pre-filter: compare middle window.
    let mid_a = &ha[ha.len() / 2];
    let mid_b = &hb[hb.len() / 2];
    if hamming_norm(mid_a, mid_b) > cfg.tolerance * 1.5 {
        return None;
    }

    // Check SameContent: how many windows match (when both have the same window count)?
    if ha.len() == hb.len() {
        let matching = ha
            .iter()
            .zip(hb.iter())
            .filter(|(x, y)| hamming_norm(x, y) <= cfg.tolerance)
            .count();
        let ratio = matching as f32 / ha.len() as f32;

        if ratio >= cfg.min_matching_windows {
            let avg_score = ha
                .iter()
                .zip(hb.iter())
                .map(|(x, y)| 1.0 - hamming_norm(x, y))
                .sum::<f32>()
                / ha.len() as f32;

            let kind = if avg_score >= 0.97 {
                SimilarityKind::Identical
            } else {
                SimilarityKind::SameContent
            };
            return Some(PairResult {
                visual_score: avg_score,
                kind,
            });
        }
    }

    // SubClip: one file is shorter - check sliding window.
    let (shorter_sig, longer_sig, clip_is) = if a.duration_ms <= b.duration_ms {
        (a, b, WhichFile::A)
    } else {
        (b, a, WhichFile::B)
    };

    // Minimum clip length: 10% of source duration.
    let ratio = shorter_sig.duration_ms as f64 / longer_sig.duration_ms.max(1) as f64;
    if ratio >= 0.10
        && let Some((score, offset_ms)) = sliding_window_match(
            &shorter_sig.visual_hashes,
            &longer_sig.visual_hashes,
            cfg.tolerance,
            cfg.subclip_min_match,
        )
    {
        return Some(PairResult {
            visual_score: score,
            kind: SimilarityKind::SubClip { offset_ms, clip_is },
        });
    }

    // General similarity (pre-filter passed but nothing specific matched).
    let mid_score = 1.0 - hamming_norm(mid_a, mid_b);
    if mid_score >= 1.0 - cfg.tolerance {
        return Some(PairResult {
            visual_score: mid_score,
            kind: SimilarityKind::Similar,
        });
    }

    None
}

/// Compares audio fingerprints of two signatures using chromaprint segment matching.
/// Returns similarity 0.0-1.0 or None.
fn compare_audio(a: &VideoSignature, b: &VideoSignature, cfg: &CompareConfig) -> Option<f32> {
    let fp_a = a.audio_fingerprint.as_deref()?;
    let fp_b = b.audio_fingerprint.as_deref()?;
    if fp_a.is_empty() || fp_b.is_empty() {
        return None;
    }

    let config = rusty_chromaprint::Configuration::preset_test1();
    let segments = rusty_chromaprint::match_fingerprints(fp_a, fp_b, &config).ok()?;

    // Filter segments by duration and score threshold (like czkawka).
    let matching: Vec<_> = segments
        .iter()
        .filter(|s| s.duration(&config) > cfg.audio_min_segment_duration && s.score < cfg.audio_max_difference)
        .collect();

    if matching.is_empty() {
        return Some(0.0);
    }

    // Best segment score → normalize to 0.0-1.0 (lower chromaprint score = better match).
    let best = matching
        .iter()
        .map(|s| s.score)
        .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(f64::MAX);

    Some((1.0 - (best / cfg.audio_max_difference).min(1.0)) as f32)
}

/// Sliding window: finds the best alignment of clip windows within source windows.
/// Returns (score, offset_ms) or None if threshold not met.
#[expect(clippy::indexing_slicing, reason = "offset bounded by sn-cn loop range")]
fn sliding_window_match(
    clip_hashes: &[TemporalHash],
    source_hashes: &[TemporalHash],
    tolerance: f32,
    min_match: f32,
) -> Option<(f32, u64)> {
    let cn = clip_hashes.len();
    let sn = source_hashes.len();
    if cn == 0 || sn < cn {
        return None;
    }

    let mut best_score = 0.0f32;
    let mut best_offset_ms = 0u64;

    for offset in 0..=(sn - cn) {
        let matching = clip_hashes
            .iter()
            .zip(&source_hashes[offset..offset + cn])
            .filter(|(c, s)| hamming_norm(c, s) <= tolerance)
            .count();
        let ratio = matching as f32 / cn as f32;

        if ratio >= min_match {
            let score = clip_hashes
                .iter()
                .zip(&source_hashes[offset..offset + cn])
                .map(|(c, s)| 1.0 - hamming_norm(c, s))
                .sum::<f32>()
                / cn as f32;

            if score > best_score {
                best_score = score;
                best_offset_ms = source_hashes[offset].start_ms;
            }
        }
    }

    if best_score > 0.0 {
        Some((best_score, best_offset_ms))
    } else {
        None
    }
}

/// Groups signatures into duration buckets (±tolerance_pct%).
/// Returns Vec<Vec<usize>> - indices of signatures per bucket.
#[expect(clippy::indexing_slicing, reason = "indices from valid range 0..sigs.len()")]
fn build_duration_buckets(sigs: &[VideoSignature], tolerance_pct: f64) -> Vec<Vec<usize>> {
    if sigs.is_empty() {
        return vec![];
    }

    let mut order: Vec<usize> = (0..sigs.len()).collect();
    order.sort_by_key(|&i| sigs[i].duration_ms);

    let tol = tolerance_pct / 100.0;
    let mut buckets: Vec<Vec<usize>> = vec![];
    let mut current: Vec<usize> = vec![order[0]];
    let mut bucket_center = sigs[order[0]].duration_ms as f64;

    for &idx in &order[1..] {
        let dur = sigs[idx].duration_ms as f64;
        if (dur - bucket_center).abs() / bucket_center.max(1.0) <= tol {
            current.push(idx);
        } else {
            buckets.push(current.clone());
            current = vec![idx];
            bucket_center = dur;
        }
    }
    buckets.push(current);

    buckets
}

/// Compares all signatures and returns groups of similar files.
#[expect(clippy::indexing_slicing, reason = "bucket indices from valid range")]
pub fn find_similar(sigs: &[VideoSignature], cfg: &CompareConfig) -> Vec<SimilarGroup> {
    let buckets = build_duration_buckets(sigs, cfg.duration_tolerance_pct);

    // Collect pairs (idx_a, idx_b, PairResult) in parallel.
    let pairs: Vec<(usize, usize, PairResult)> = buckets
        .par_iter()
        .flat_map(|bucket| {
            let mut bucket_pairs = vec![];
            for i in 0..bucket.len() {
                for j in (i + 1)..bucket.len() {
                    let ia = bucket[i];
                    let ib = bucket[j];
                    if let Some(result) = compare_pair(&sigs[ia], &sigs[ib], cfg) {
                        bucket_pairs.push((ia, ib, result));
                    }
                }
            }
            bucket_pairs
        })
        .collect();

    // Cluster with representative per group (anti-daisy-chain).
    cluster_into_groups(sigs, pairs, cfg)
}

#[expect(clippy::indexing_slicing, reason = "group/sig indices verified against usize::MAX")]
fn cluster_into_groups(
    sigs: &[VideoSignature],
    pairs: Vec<(usize, usize, PairResult)>,
    cfg: &CompareConfig,
) -> Vec<SimilarGroup> {
    // group_id[i] = group ID for sigs[i] (usize::MAX if unassigned).
    let mut group_id: Vec<usize> = vec![usize::MAX; sigs.len()];
    // Representative of each group (signature index).
    let mut representatives: Vec<usize> = vec![];
    let mut groups: Vec<(Vec<usize>, SimilarityKind)> = vec![];

    for (ia, ib, result) in pairs {
        let ga = group_id[ia];
        let gb = group_id[ib];

        match (ga == usize::MAX, gb == usize::MAX) {
            // Both new - create a new group.
            (true, true) => {
                let gid = groups.len();
                group_id[ia] = gid;
                group_id[ib] = gid;
                representatives.push(ia);
                groups.push((vec![ia, ib], result.kind));
            }
            // ia already in a group - add ib (validate against representative).
            (false, true) => {
                let rep = representatives[ga];
                if rep != ia {
                    if let Some(r) = compare_pair(&sigs[rep], &sigs[ib], cfg) {
                        if r.visual_score < 1.0 - cfg.tolerance {
                            continue; // Daisy chain - reject.
                        }
                    } else {
                        continue;
                    }
                }
                group_id[ib] = ga;
                groups[ga].0.push(ib);
            }
            // ib already in a group - add ia (validate against representative).
            (true, false) => {
                let rep = representatives[gb];
                if rep != ib {
                    if let Some(r) = compare_pair(&sigs[rep], &sigs[ia], cfg) {
                        if r.visual_score < 1.0 - cfg.tolerance {
                            continue;
                        }
                    } else {
                        continue;
                    }
                }
                group_id[ia] = gb;
                groups[gb].0.push(ia);
            }
            // Both already in groups - skip merge to avoid daisy chains.
            (false, false) => {
                let _ = result;
            }
        }
    }

    groups
        .into_iter()
        .filter(|(members, _)| members.len() >= 2)
        .map(|(members, kind)| SimilarGroup {
            files: members.iter().map(|&i| sigs[i].path.clone()).collect(),
            kind,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual::{HASH_BITS, HashBits, TemporalHash, VideoSignature};

    fn make_hash(val: bool) -> TemporalHash {
        let mut bits = HashBits::ZERO;
        if val {
            // Only set the 1000 used bits, not all 1024.
            for i in 0..HASH_BITS {
                bits.set(i, true);
            }
        }
        TemporalHash {
            start_ms: 0,
            end_ms: 6000,
            bits,
        }
    }

    fn make_sig(path: &str, duration_ms: u64, hash_val: bool) -> VideoSignature {
        VideoSignature {
            path: PathBuf::from(path),
            duration_ms,
            aspect_ratio: 1.777,
            visual_hashes: vec![
                make_hash(hash_val),
                make_hash(hash_val),
                make_hash(hash_val),
                make_hash(hash_val),
                make_hash(hash_val),
            ],
            audio_fingerprint: None,
            metadata: None,
        }
    }

    #[test]
    fn identical_hashes_found() {
        let sigs = vec![
            make_sig("/a.mp4", 60_000, false),
            make_sig("/b.mp4", 60_000, false),
            make_sig("/c.mp4", 60_000, false),
        ];
        let cfg = CompareConfig::default();
        let groups = find_similar(&sigs, &cfg);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 3);
    }

    #[test]
    fn different_duration_not_grouped() {
        let sigs = vec![
            make_sig("/a.mp4", 60_000, false),
            make_sig("/b.mp4", 3_600_000, false), // 1 hour - outside ±20%
        ];
        let cfg = CompareConfig::default();
        let groups = find_similar(&sigs, &cfg);
        assert!(groups.is_empty(), "different durations should not match");
    }

    #[test]
    fn hamming_distance_zero_for_same() {
        let h = make_hash(false);
        assert_eq!(h.hamming_distance(&h), 0);
    }

    #[test]
    fn hamming_distance_max_for_inverse() {
        let h0 = make_hash(false);
        let h1 = make_hash(true);
        assert_eq!(h0.hamming_distance(&h1), 1000);
        assert!((h0.normalized_distance(&h1) - 1.0).abs() < 0.001);
    }

    #[test]
    fn duration_buckets_groups_by_tolerance() {
        let sigs = vec![
            make_sig("/a.mp4", 60_000, false),
            make_sig("/b.mp4", 65_000, false),  // +8% - within ±20%
            make_sig("/c.mp4", 600_000, false), // 10× longer - separate bucket
        ];
        let buckets = build_duration_buckets(&sigs, 20.0);
        assert_eq!(buckets.len(), 2);
    }
}
