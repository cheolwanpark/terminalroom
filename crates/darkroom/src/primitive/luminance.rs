//! Luminance extraction and the `Y'/Y` rescale used by hue-preserving tone
//! controls.
//!
//! Tone work happens in linear RGB by extracting Y, modifying Y, and scaling
//! the RGB triple by `Y'/Y`. This preserves hue (chromaticity) — applying a
//! tone curve to each channel independently would shift hue.

use wide::f32x8;

use crate::space::{Buffer, LinearRec2020, LinearSrgb};

/// BT.2020 luminance weights for linear Rec.2020.
pub const REC2020_LUMA: [f32; 3] = [0.2627, 0.6780, 0.0593];
/// BT.709 luminance weights for linear sRGB.
pub const REC709_LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// Compute per-pixel luminance Y for a planar Rec.2020 buffer.
pub fn luminance_rec2020(buf: &Buffer<LinearRec2020>) -> Vec<f32> {
    luminance_planar(buf.r(), buf.g(), buf.b(), REC2020_LUMA)
}

/// Compute per-pixel luminance Y for a planar linear sRGB buffer.
pub fn luminance_srgb(buf: &Buffer<LinearSrgb>) -> Vec<f32> {
    luminance_planar(buf.r(), buf.g(), buf.b(), REC709_LUMA)
}

fn luminance_planar(r: &[f32], g: &[f32], b: &[f32], w: [f32; 3]) -> Vec<f32> {
    debug_assert_eq!(r.len(), g.len());
    debug_assert_eq!(g.len(), b.len());
    let n = r.len();
    let mut y = vec![0.0f32; n];
    let wr = f32x8::splat(w[0]);
    let wg = f32x8::splat(w[1]);
    let wb = f32x8::splat(w[2]);
    let main = n - n % 8;
    let mut i = 0;
    while i < main {
        let vr = f32x8::new(r[i..i + 8].try_into().expect("8 lanes"));
        let vg = f32x8::new(g[i..i + 8].try_into().expect("8 lanes"));
        let vb = f32x8::new(b[i..i + 8].try_into().expect("8 lanes"));
        let vy = wr * vr + wg * vg + wb * vb;
        y[i..i + 8].copy_from_slice(&vy.to_array());
        i += 8;
    }
    for k in main..n {
        y[k] = w[0] * r[k] + w[1] * g[k] + w[2] * b[k];
    }
    y
}

/// Apply the ratio `y_new / y_old` per pixel to a Rec.2020 RGB buffer.
/// `y_old` and `y_new` must each be `plane_size()` long.
///
/// Tone curves modify Y in log-luminance space; this brings the result back
/// to linear RGB without a hue shift.
pub fn rescale_rec2020_to_luma(buf: &mut Buffer<LinearRec2020>, y_old: &[f32], y_new: &[f32]) {
    let plane = buf.plane_size();
    debug_assert_eq!(y_old.len(), plane);
    debug_assert_eq!(y_new.len(), plane);
    let (r, g, b) = buf.rgb_planes_mut();
    rescale_planar(r, g, b, y_old, y_new);
}

/// Same as `rescale_rec2020_to_luma` but for a linear sRGB buffer.
pub fn rescale_srgb_to_luma(buf: &mut Buffer<LinearSrgb>, y_old: &[f32], y_new: &[f32]) {
    let plane = buf.plane_size();
    debug_assert_eq!(y_old.len(), plane);
    debug_assert_eq!(y_new.len(), plane);
    let (r, g, b) = buf.rgb_planes_mut();
    rescale_planar(r, g, b, y_old, y_new);
}

fn rescale_planar(r: &mut [f32], g: &mut [f32], b: &mut [f32], y_old: &[f32], y_new: &[f32]) {
    let n = r.len();
    let eps = f32x8::splat(1e-6);
    let main = n - n % 8;
    let mut i = 0;
    while i < main {
        let yo = f32x8::new(y_old[i..i + 8].try_into().expect("8 lanes"));
        let yn = f32x8::new(y_new[i..i + 8].try_into().expect("8 lanes"));
        let ratio = yn / yo.max(eps);
        let vr = f32x8::new(r[i..i + 8].try_into().expect("8 lanes")) * ratio;
        let vg = f32x8::new(g[i..i + 8].try_into().expect("8 lanes")) * ratio;
        let vb = f32x8::new(b[i..i + 8].try_into().expect("8 lanes")) * ratio;
        r[i..i + 8].copy_from_slice(&vr.to_array());
        g[i..i + 8].copy_from_slice(&vg.to_array());
        b[i..i + 8].copy_from_slice(&vb.to_array());
        i += 8;
    }
    for k in main..n {
        let ratio = y_new[k] / y_old[k].max(1e-6);
        r[k] *= ratio;
        g[k] *= ratio;
        b[k] *= ratio;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luminance_white_is_one() {
        let buf: Buffer<LinearRec2020> = Buffer::from_planar(vec![1.0; 12], 2, 2);
        let y = luminance_rec2020(&buf);
        for v in y {
            assert!((v - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn luminance_black_is_zero() {
        let buf: Buffer<LinearRec2020> = Buffer::from_planar(vec![0.0; 12], 2, 2);
        let y = luminance_rec2020(&buf);
        assert!(y.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn rescale_with_equal_ratios_is_noop() {
        let mut buf: Buffer<LinearRec2020> = Buffer::from_planar(vec![0.5; 24], 4, 2);
        let y_old = vec![0.5; 8];
        let y_new = vec![0.5; 8];
        let before = buf.data().to_vec();
        rescale_rec2020_to_luma(&mut buf, &y_old, &y_new);
        assert_eq!(buf.data(), before.as_slice());
    }

    #[test]
    fn rescale_doubles_when_ratio_is_two() {
        let mut buf: Buffer<LinearRec2020> = Buffer::from_planar(vec![0.25; 24], 4, 2);
        let y_old = vec![0.25; 8];
        let y_new = vec![0.5; 8];
        rescale_rec2020_to_luma(&mut buf, &y_old, &y_new);
        for &v in buf.data() {
            assert!((v - 0.5).abs() < 1e-6);
        }
    }

    #[test]
    fn rescale_preserves_hue_chromaticity() {
        // A non-neutral pixel: R=0.8, G=0.4, B=0.2. Ratio R:G:B should survive.
        let mut buf: Buffer<LinearRec2020> = Buffer::from_planar(
            vec![0.8, 0.8, 0.4, 0.4, 0.2, 0.2], // 2x1 planar
            2,
            1,
        );
        let y0 = luminance_rec2020(&buf);
        let y1: Vec<f32> = y0.iter().map(|y| y * 1.5).collect();
        rescale_rec2020_to_luma(&mut buf, &y0, &y1);
        for i in 0..2 {
            let r = buf.r()[i];
            let g = buf.g()[i];
            let b = buf.b()[i];
            // Original chromaticity was 0.8 : 0.4 : 0.2 = 4 : 2 : 1.
            assert!((r / g - 2.0).abs() < 1e-4, "R/G shifted: {r}/{g}");
            assert!((g / b - 2.0).abs() < 1e-4, "G/B shifted: {g}/{b}");
        }
    }
}
