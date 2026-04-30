//! Linear-light primaries conversion between Rec.2020 (BT.2020) and sRGB
//! (BT.709), both at D65 white point. The math is a 3×3 matrix multiply per
//! pixel and is applied in place on the planar `Buffer<S>`; the underlying
//! `Vec<f32>` is reused and only the phantom type is rebound.
//!
//! These are `InPlaceTransform` impls. No clamping here — the caller decides
//! whether to clamp (e.g. `SrgbEncode` clamps to [0, 1] before quantization).

use crate::simd::apply_3x3_planar;
use crate::space::{Buffer, LinearRec2020, LinearSrgb};
use crate::transform::InPlaceTransform;

/// BT.2020 → BT.709 primaries matrix, D65 to D65, linear light. Each row sums
/// to 1, so neutral preserves to neutral.
pub(crate) const REC2020_TO_SRGB: [[f32; 3]; 3] = [
    [1.66049, -0.58764, -0.07285],
    [-0.12455, 1.13290, -0.00835],
    [-0.01815, -0.10058, 1.11873],
];

/// BT.709 → BT.2020 primaries matrix, D65 to D65, linear light. Inverse of
/// `REC2020_TO_SRGB`.
pub(crate) const SRGB_TO_REC2020: [[f32; 3]; 3] = [
    [0.62740, 0.32928, 0.04332],
    [0.06909, 0.91955, 0.01136],
    [0.01639, 0.08801, 0.89559],
];

pub struct Rec2020ToSrgb;
impl InPlaceTransform for Rec2020ToSrgb {
    type In = LinearRec2020;
    type Out = LinearSrgb;
    fn apply(&self, mut buf: Buffer<Self::In>) -> Buffer<Self::Out> {
        let (r, g, b) = buf.rgb_planes_mut();
        apply_3x3_planar(r, g, b, REC2020_TO_SRGB);
        buf.into_space()
    }
}

pub struct SrgbToRec2020;
impl InPlaceTransform for SrgbToRec2020 {
    type In = LinearSrgb;
    type Out = LinearRec2020;
    fn apply(&self, mut buf: Buffer<Self::In>) -> Buffer<Self::Out> {
        let (r, g, b) = buf.rgb_planes_mut();
        apply_3x3_planar(r, g, b, SRGB_TO_REC2020);
        buf.into_space()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rec2020_to_srgb_white_stays_white() {
        // Each row of REC2020_TO_SRGB sums to 1.
        for row in REC2020_TO_SRGB {
            let s: f32 = row.iter().sum();
            assert!((s - 1.0).abs() < 1e-3, "row sum {s} != 1");
        }
    }

    #[test]
    fn srgb_to_rec2020_white_stays_white() {
        for row in SRGB_TO_REC2020 {
            let s: f32 = row.iter().sum();
            assert!((s - 1.0).abs() < 1e-3, "row sum {s} != 1");
        }
    }

    #[test]
    fn rec2020_to_srgb_roundtrip_is_identity() {
        // Apply REC2020 → SRGB → REC2020 and expect to recover input.
        let input: Vec<f32> = (0..30).map(|i| i as f32 / 30.0).collect();
        let buf: Buffer<LinearRec2020> = Buffer::from_planar(input.clone(), 5, 2);
        let mid = Rec2020ToSrgb.apply(buf);
        let back = SrgbToRec2020.apply(mid);
        let out = back.into_data();
        for (a, b) in input.iter().zip(out.iter()) {
            assert!((a - b).abs() < 1e-3, "roundtrip drift: {a} -> {b}");
        }
    }

    #[test]
    fn rec2020_to_srgb_pure_red_clips_other_channels_negative() {
        // Saturated Rec.2020 red maps to slightly out-of-gamut sRGB. R blows
        // past 1, G and B go small-negative. We don't clamp here — that's the
        // encode step's job.
        let mut data = vec![0.0f32; 6]; // 2 pixels, planar
        data[0] = 1.0;
        data[1] = 1.0;
        let buf: Buffer<LinearRec2020> = Buffer::from_planar(data, 2, 1);
        let out = Rec2020ToSrgb.apply(buf);
        assert!(out.r()[0] > 1.0, "expected R > 1, got {}", out.r()[0]);
        assert!(out.g()[0] < 0.0, "expected G < 0, got {}", out.g()[0]);
        assert!(out.b()[0] < 0.0, "expected B < 0, got {}", out.b()[0]);
    }
}
