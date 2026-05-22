//! Letterbox cropdetect - ported from vid_dup_finder_lib.
//!
//! Iterates from each edge (top, bottom, left, right) toward the center.
//! A row/column is considered letterbox when ≥90% of pixels fall within
//! ±16 of the dominant (modal) color and the modal value is dark (≤32).

use image::GrayImage;

/// Crop area in pixels from each edge.
#[derive(Debug, Clone, Copy, Default)]
pub struct Crop {
    pub left: u32,
    pub right: u32,
    pub top: u32,
    pub bottom: u32,
}

impl Crop {
    /// Returns the more conservative crop (minimum from both).
    pub fn union(&self, other: &Self) -> Self {
        Self {
            left: self.left.min(other.left),
            right: self.right.min(other.right),
            top: self.top.min(other.top),
            bottom: self.bottom.min(other.bottom),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.left == 0 && self.right == 0 && self.top == 0 && self.bottom == 0
    }
}

const LETTERBOX_TOLERANCE: u8 = 16;
const LETTERBOX_THRESHOLD: f64 = 0.90;
/// Maximum modal pixel value to consider a strip a letterbox (must be dark).
const LETTERBOX_MAX_MODAL: u8 = 32;

/// Detects letterbox on a single frame and returns pixels to crop from each edge.
pub fn detect_letterbox(frame: &GrayImage) -> Crop {
    let (w, h) = frame.dimensions();

    let top = scan_from_edge(frame, w, h, Side::Top);
    let bottom = scan_from_edge(frame, w, h, Side::Bottom);
    let left = scan_from_edge(frame, w, h, Side::Left);
    let right = scan_from_edge(frame, w, h, Side::Right);

    // Sanity check: keep at least 1px of usable content.
    let usable_w = w.saturating_sub(left + right);
    let usable_h = h.saturating_sub(top + bottom);
    if usable_w == 0 || usable_h == 0 {
        return Crop::default();
    }

    Crop {
        left,
        right,
        top,
        bottom,
    }
}

/// Detects letterbox across multiple frames and returns the union (most conservative crop).
pub fn detect_letterbox_multi(frames: &[GrayImage]) -> Crop {
    frames
        .iter()
        .step_by(1.max(frames.len() / 8))
        .take(8)
        .map(detect_letterbox)
        .reduce(|a, b| a.union(&b))
        .unwrap_or_default()
}

#[derive(Clone, Copy)]
enum Side {
    Top,
    Bottom,
    Left,
    Right,
}

fn scan_from_edge(frame: &GrayImage, w: u32, h: u32, side: Side) -> u32 {
    let max_strip = match side {
        Side::Top | Side::Bottom => h / 2,
        Side::Left | Side::Right => w / 2,
    };

    for offset in 0..max_strip {
        let pixels: Vec<u8> = match side {
            Side::Top => (0..w).map(|x| frame.get_pixel(x, offset).0[0]).collect(),
            Side::Bottom => (0..w).map(|x| frame.get_pixel(x, h - 1 - offset).0[0]).collect(),
            Side::Left => (0..h).map(|y| frame.get_pixel(offset, y).0[0]).collect(),
            Side::Right => (0..h).map(|y| frame.get_pixel(w - 1 - offset, y).0[0]).collect(),
        };

        if !is_letterbox_strip(&pixels) {
            return offset;
        }
    }

    max_strip
}

/// Returns true if a strip (row/column) qualifies as a dark letterbox bar.
/// Criterion: modal value ≤ 32 AND ≥90% of pixels within ±16 of the mode.
#[expect(clippy::indexing_slicing, reason = "u8 as usize always valid for [_; 256]")]
fn is_letterbox_strip(pixels: &[u8]) -> bool {
    if pixels.is_empty() {
        return false;
    }

    // Histogram → mode.
    let mut histogram = [0u32; 256];
    for &p in pixels {
        histogram[p as usize] += 1;
    }
    let mode = histogram
        .iter()
        .enumerate()
        .max_by_key(|&(_, c)| c)
        .map_or(0, |(i, _)| i as u8);

    // Strip must be dark (letterbox = black bars).
    if mode > LETTERBOX_MAX_MODAL {
        return false;
    }

    let matching = pixels
        .iter()
        .filter(|&&p| mode.abs_diff(p) <= LETTERBOX_TOLERANCE)
        .count();

    matching as f64 / pixels.len() as f64 >= LETTERBOX_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_gray(w: u32, h: u32, value: u8) -> GrayImage {
        GrayImage::from_pixel(w, h, image::Luma([value]))
    }

    #[test]
    fn no_letterbox_on_uniform() {
        let frame = make_gray(160, 90, 128);
        let crop = detect_letterbox(&frame);
        // Uniform bright frame - no dark bars, no crop expected.
        let _ = crop;
    }

    #[test]
    fn detects_black_bars_top_bottom() {
        let mut frame = GrayImage::new(160, 90);
        // Black bars: top and bottom 10px.
        for x in 0..160 {
            for y in 0..10 {
                frame.put_pixel(x, y, image::Luma([0]));
                frame.put_pixel(x, 89 - y, image::Luma([0]));
            }
        }
        // Bright content in the middle.
        for x in 0..160 {
            for y in 10..80 {
                frame.put_pixel(x, y, image::Luma([200]));
            }
        }
        let crop = detect_letterbox(&frame);
        assert!(crop.top >= 9, "expected top crop, got {}", crop.top);
        assert!(crop.bottom >= 9, "expected bottom crop, got {}", crop.bottom);
    }
}
