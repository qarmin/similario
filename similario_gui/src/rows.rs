use std::collections::HashMap;
use std::path::PathBuf;

use similario_core::compare::SimilarGroup;
use similario_core::{SimilarityKind, VideoSignature};
use slint::{Image, Model, ModelRc, Rgb8Pixel, SharedPixelBuffer, SharedString, VecModel};

use crate::format::{format_bitrate, format_date, format_duration, format_size};
use crate::{FileRow, GroupKind, QualityTier};

#[derive(Clone)]
pub struct PlainRow {
    pub is_header: bool,
    pub group_kind: GroupKind,
    pub quality_tier: QualityTier,
    pub name: String,
    pub path: String,
    pub size: String,
    pub dimensions: String,
    pub duration: String,
    pub duration_secs: f64,
    pub bitrate: String,
    pub fps: String,
    pub codec: String,
    pub mod_date: String,
}

/// Score a codec: higher is better. Recognises modern formats (AV1, HEVC, VP9)
/// over older ones (h264, MPEG-2). Falls back to 0 for unknown codecs.
fn codec_rank(codec: &str) -> i32 {
    let c = codec.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| c.contains(n));
    if has(&["av1"]) {
        return 4;
    }
    if has(&["hevc", "h265", "265", "vp9"]) {
        return 3;
    }
    if has(&["h264", "264", "avc", "vp8"]) {
        return 2;
    }
    if has(&["mpeg", "xvid", "divx", "wmv"]) {
        return 1;
    }
    0
}

/// (resolution_pixels, codec_rank, bitrate_bps) - lexicographic order means
/// resolution dominates, then codec, then bitrate as a tiebreaker.
type QualityScore = (u64, i32, u64);

fn quality_score(width: Option<u32>, height: Option<u32>, codec: &str, bitrate: Option<u64>) -> QualityScore {
    let res = u64::from(width.unwrap_or(0)) * u64::from(height.unwrap_or(0));
    (res, codec_rank(codec), bitrate.unwrap_or(0))
}

pub fn groups_to_plain_rows(groups: &[SimilarGroup], sig_map: &HashMap<PathBuf, &VideoSignature>) -> Vec<PlainRow> {
    let mut rows = Vec::new();
    for group in groups {
        let kind = match &group.kind {
            SimilarityKind::Identical => GroupKind::Identical,
            SimilarityKind::SameContent => GroupKind::SameContent,
            SimilarityKind::SubClip { .. } => GroupKind::SubClip,
            SimilarityKind::Similar => GroupKind::Similar,
        };
        rows.push(PlainRow {
            is_header: true,
            group_kind: kind,
            quality_tier: QualityTier::Unknown,
            name: format!("{} file(s)", group.files.len()),
            path: String::new(),
            size: String::new(),
            dimensions: String::new(),
            duration: String::new(),
            duration_secs: 0.0,
            bitrate: String::new(),
            fps: String::new(),
            codec: String::new(),
            mod_date: String::new(),
        });

        // Score every file in the group to find the "best" - highest resolution,
        // then best codec, then highest bitrate. Single-file groups stay Unknown.
        let scores: Vec<QualityScore> = group
            .files
            .iter()
            .map(|p| {
                let meta = sig_map.get(p).and_then(|s| s.metadata.as_ref());
                let codec = meta.and_then(|m| m.codec.as_deref()).unwrap_or("");
                quality_score(
                    meta.and_then(|m| m.width),
                    meta.and_then(|m| m.height),
                    codec,
                    meta.and_then(|m| m.bitrate_bps),
                )
            })
            .collect();
        let max_score = scores.iter().max().copied();
        let differentiated = scores.iter().any(|s| Some(*s) != max_score);

        for (path, score) in group.files.iter().zip(scores.iter()) {
            let meta = sig_map.get(path).and_then(|s| s.metadata.as_ref());
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            let fs_meta = std::fs::metadata(path).ok();
            let size = fs_meta.as_ref().map(|m| format_size(m.len())).unwrap_or_default();
            let mod_date = fs_meta
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| format_date(d.as_secs()))
                .unwrap_or_default();
            let duration_secs = meta.map_or(0.0, |m| m.duration_secs);

            let quality_tier = if !differentiated || *score == (0, 0, 0) {
                QualityTier::Unknown
            } else if Some(*score) == max_score {
                QualityTier::Best
            } else {
                QualityTier::Worse
            };

            rows.push(PlainRow {
                is_header: false,
                group_kind: GroupKind::Identical, // unused for non-header rows
                quality_tier,
                name,
                path: path.display().to_string(),
                size,
                dimensions: meta
                    .and_then(|m| Some(format!("{}×{}", m.width?, m.height?)))
                    .unwrap_or_default(),
                duration: meta.map(|m| format_duration(m.duration_secs)).unwrap_or_default(),
                duration_secs,
                bitrate: meta
                    .and_then(|m| Some(format_bitrate(m.bitrate_bps?)))
                    .unwrap_or_default(),
                fps: meta.and_then(|m| Some(format!("{:.1}", m.fps?))).unwrap_or_default(),
                codec: meta.and_then(|m| m.codec.clone()).unwrap_or_default(),
                mod_date,
            });
        }
    }
    rows
}

pub fn plain_to_file_row(p: PlainRow) -> FileRow {
    FileRow {
        is_header: p.is_header,
        group_kind: p.group_kind,
        quality_tier: p.quality_tier,
        name: SharedString::from(p.name),
        path: SharedString::from(p.path),
        size: SharedString::from(p.size),
        dimensions: SharedString::from(p.dimensions),
        duration: SharedString::from(p.duration),
        bitrate: SharedString::from(p.bitrate),
        fps: SharedString::from(p.fps),
        codec: SharedString::from(p.codec),
        mod_date: SharedString::from(p.mod_date),
        thumbnail: placeholder_thumbnail(),
        checked: false,
    }
}

#[expect(clippy::indexing_slicing, reason = "chunks_exact_mut(3) guarantees 3 elements")]
pub fn placeholder_thumbnail() -> Image {
    let mut buf = SharedPixelBuffer::<Rgb8Pixel>::new(112, 63);
    for p in buf.make_mut_bytes().chunks_exact_mut(3) {
        p[0] = 0x33;
        p[1] = 0x33;
        p[2] = 0x44;
    }
    Image::from_rgb8(buf)
}

pub fn with_vec_model<F>(model: &ModelRc<FileRow>, f: F)
where
    F: FnOnce(&VecModel<FileRow>),
{
    if let Some(vm) = model.as_any().downcast_ref::<VecModel<FileRow>>() {
        f(vm);
    }
}
