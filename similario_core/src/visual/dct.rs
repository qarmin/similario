//! 3D-DCT on a 16×16×16 frame cube → 1000-bit hash.
//!
//! Algorithm (ported from vid_dup_finder_lib):
//! 1. Center pixels: u8 → f64, [0,255] → [-128,127]
//! 2. Tensor [frame, x, y] = [16, 16, 16]
//! 3. DCT-II three times (per axis) with transpositions
//! 4. Extract 10×10×10 sub-tensor (lowest frequencies)
//! 5. Binarize: coefficient > 0.0 → bit=1

use bitvec::prelude::*;
use image::GrayImage;
use ndarray::Array3;
use rustdct::DctPlanner;

use super::{DCT_SIZE, FRAMES_PER_WINDOW, HASH_SIZE, HashBits};

/// Computes a 1000-bit hash from `FRAMES_PER_WINDOW` frames of 16×16 px.
#[expect(clippy::indexing_slicing, reason = "indices bounded by DCT_SIZE/HASH_SIZE constants")]
pub fn compute_hash_from_frames(frames: &[GrayImage]) -> HashBits {
    assert!(frames.len() >= FRAMES_PER_WINDOW);

    // 1. Build tensor [frame_idx, x, y] with centered f64 values.
    let mut tensor = Array3::<f64>::zeros((DCT_SIZE, DCT_SIZE, DCT_SIZE));
    for (fi, frame) in frames.iter().take(DCT_SIZE).enumerate() {
        for y in 0..DCT_SIZE {
            for x in 0..DCT_SIZE {
                let pixel = frame.get_pixel(x as u32, y as u32).0[0];
                // Center: [0,255] → [-128.0, 127.0]
                tensor[[fi, x, y]] = f64::from(pixel) - 128.0;
            }
        }
    }

    // 2. 3D-DCT (three rounds of 1D-DCT with transpositions).
    let dct_result = dct_3d(tensor);

    // 3. Binarize: extract 10×10×10 corner and apply threshold 0.0.
    let mut bits: HashBits = BitArray::ZERO;
    let mut bit_idx = 0usize;

    for fi in 0..HASH_SIZE {
        for xi in 0..HASH_SIZE {
            for yi in 0..HASH_SIZE {
                if bit_idx >= 1000 {
                    break;
                }
                bits.set(bit_idx, dct_result[[fi, xi, yi]] > 0.0);
                bit_idx += 1;
            }
        }
    }

    bits
}

/// 3D-DCT-II on an N×N×N cube (separable: 3× 1D-DCT per axis).
#[expect(clippy::indexing_slicing, reason = "shape indices are valid")]
fn dct_3d(mut mat: Array3<f64>) -> Array3<f64> {
    let n = mat.shape()[0];
    let mut planner = DctPlanner::new();
    let dct = planner.plan_dct2(n);

    // Round 1: DCT along axis 0 (frame index).
    for mut row in mat.rows_mut() {
        let s = row.as_slice_mut().expect("contiguous");
        dct.process_dct2(s);
    }
    // Transpose axes 0↔1.
    mat = swap_axes_0_1(mat);

    // Round 2: DCT along axis 1 (x).
    for mut row in mat.rows_mut() {
        let s = row.as_slice_mut().expect("contiguous");
        dct.process_dct2(s);
    }
    // Transpose axes 0↔2.
    mat = swap_axes_0_2(mat);

    // Round 3: DCT along axis 2 (y).
    for mut row in mat.rows_mut() {
        let s = row.as_slice_mut().expect("contiguous");
        dct.process_dct2(s);
    }
    // Restore original orientation: 0↔2, 0↔1.
    mat = swap_axes_0_2(mat);
    mat = swap_axes_0_1(mat);

    mat
}

/// Copies a tensor with axes 0 and 1 swapped into a new contiguous row-major array.
/// NOTE: Uses original shape - correct only for cubic tensors (all dims equal).
#[expect(clippy::needless_pass_by_value, reason = "takes ownership for view conversion")]
fn swap_axes_0_1(mat: Array3<f64>) -> Array3<f64> {
    let mut view = mat.view();
    view.swap_axes(0, 1);
    view.as_standard_layout().into_owned()
}

/// Copies a tensor with axes 0 and 2 swapped into a new contiguous row-major array.
/// NOTE: Uses original shape - correct only for cubic tensors (all dims equal).
#[expect(clippy::needless_pass_by_value, reason = "takes ownership for view conversion")]
fn swap_axes_0_2(mat: Array3<f64>) -> Array3<f64> {
    let mut view = mat.view();
    view.swap_axes(0, 2);
    view.as_standard_layout().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_frame(value: u8) -> GrayImage {
        GrayImage::from_pixel(DCT_SIZE as u32, DCT_SIZE as u32, image::Luma([value]))
    }

    #[test]
    fn hash_identical_frames_is_zero_distance() {
        let frames: Vec<GrayImage> = (0..FRAMES_PER_WINDOW).map(|_| make_frame(128)).collect();
        let h1 = compute_hash_from_frames(&frames);
        let h2 = compute_hash_from_frames(&frames);
        // Identical input → identical hash → distance = 0.
        let dist = h1.iter().zip(h2.iter()).filter(|(a, b)| a != b).count();
        assert_eq!(dist, 0);
    }

    #[test]
    fn hash_different_content_differs() {
        let dark: Vec<GrayImage> = (0..FRAMES_PER_WINDOW).map(|_| make_frame(30)).collect();
        let bright: Vec<GrayImage> = (0..FRAMES_PER_WINDOW).map(|_| make_frame(220)).collect();
        let h1 = compute_hash_from_frames(&dark);
        let h2 = compute_hash_from_frames(&bright);
        let dist = h1.iter().zip(h2.iter()).filter(|(a, b)| a != b).count();
        // Completely different brightness → at least 1 bit must differ.
        assert!(dist > 0, "expected non-zero distance, got {dist}");
    }

    #[test]
    fn dct_3d_produces_finite_values() {
        let mat = Array3::<f64>::ones((DCT_SIZE, DCT_SIZE, DCT_SIZE));
        let result = dct_3d(mat);
        for &v in &result {
            assert!(v.is_finite(), "non-finite DCT coefficient: {v}");
        }
    }
}
