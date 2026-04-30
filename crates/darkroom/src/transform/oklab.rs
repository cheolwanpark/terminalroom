//! OKLab color-space conversions.
//!
//! OKLab is perceptually uniform; Cartesian (L, a, b). OKLCh is its polar
//! form (L, C, h).
//!
//! - `LinearToOklab`: `Buffer<LinearSrgb>` → `Buffer<Oklab>` (M1 matrix +
//!   per-component cbrt + M2 matrix).
//! - `OklabToLinear`: inverse (M2⁻¹ + per-component cube + M1⁻¹).
//! - `OklabToOklch` / `OklchToOklab`: same-layout polar/Cartesian swap, in
//!   place via [`InPlaceTransform`].
//!
//! References: Björn Ottosson, "A perceptual color space for image processing",
//! 2020. Constants are from the public OKLab reference.

use wide::f32x8;

use crate::space::{Buffer, LinearSrgb, Oklab, Oklch};
use crate::transform::{InPlaceTransform, Transform};

// Forward: linear sRGB → LMS, then LMS' (cbrt) → L,a,b.
const M1: [[f32; 3]; 3] = [
    [0.4122214708, 0.5363325363, 0.0514459929],
    [0.2119034982, 0.6806995451, 0.1073969566],
    [0.0883024619, 0.2817188376, 0.6299787005],
];
const M2: [[f32; 3]; 3] = [
    [0.2104542553, 0.7936177850, -0.0040720468],
    [1.9779984951, -2.4285922050, 0.4505937099],
    [0.0259040371, 0.7827717662, -0.8086757660],
];

// Inverse: L,a,b → LMS' (cube), then LMS → linear sRGB.
const M2_INV: [[f32; 3]; 3] = [
    [1.0, 0.3963377774, 0.2158037573],
    [1.0, -0.1055613458, -0.0638541728],
    [1.0, -0.0894841775, -1.2914855480],
];
const M1_INV: [[f32; 3]; 3] = [
    [4.0767416621, -3.3077115913, 0.2309699292],
    [-1.2684380046, 2.6097574011, -0.3413193965],
    [-0.0041960863, -0.7034186147, 1.7076147010],
];

pub struct LinearToOklab;
impl Transform for LinearToOklab {
    type Input = Buffer<LinearSrgb>;
    type Output = Buffer<Oklab>;
    fn apply(&self, mut src: Buffer<LinearSrgb>) -> Buffer<Oklab> {
        let (r, g, b) = src.rgb_planes_mut();
        // Step 1: M1 * (r, g, b) -> (l, m, s) in place.
        apply_3x3_simd(r, g, b, M1);
        // Step 2: cube root each.
        cbrt_signed_inplace(r);
        cbrt_signed_inplace(g);
        cbrt_signed_inplace(b);
        // Step 3: M2 * (l_, m_, s_) -> (L, a, b) in place.
        apply_3x3_simd(r, g, b, M2);
        src.into_space()
    }
}

pub struct OklabToLinear;
impl Transform for OklabToLinear {
    type Input = Buffer<Oklab>;
    type Output = Buffer<LinearSrgb>;
    fn apply(&self, mut src: Buffer<Oklab>) -> Buffer<LinearSrgb> {
        let (r, g, b) = src.rgb_planes_mut();
        // Step 1: M2⁻¹ * (L, a, b) -> (l_, m_, s_) in place.
        apply_3x3_simd(r, g, b, M2_INV);
        // Step 2: cube each.
        cube_inplace(r);
        cube_inplace(g);
        cube_inplace(b);
        // Step 3: M1⁻¹ * (l, m, s) -> (R, G, B) in place.
        apply_3x3_simd(r, g, b, M1_INV);
        src.into_space()
    }
}

/// OKLab → OKLCh: (L, a, b) → (L, C, h). Same layout, in place.
pub struct OklabToOklch;
impl InPlaceTransform for OklabToOklch {
    type In = Oklab;
    type Out = Oklch;
    fn apply(&self, mut src: Buffer<Oklab>) -> Buffer<Oklch> {
        // The struct exposes `rgb_planes_mut` which is generic over the planes;
        // here the channels are L, a, b but the layout is the same, so we
        // reuse the slot names.
        let plane_size = src.plane_size();
        let (l, ab) = src.data_mut().split_at_mut(plane_size);
        let (a, b) = ab.split_at_mut(plane_size);
        let _ = l; // L unchanged
        let n = a.len();
        let main = n - n % 8;
        let mut i = 0;
        while i < main {
            let va = f32x8::new(a[i..i + 8].try_into().expect("8 lanes"));
            let vb = f32x8::new(b[i..i + 8].try_into().expect("8 lanes"));
            let vc = (va * va + vb * vb).sqrt();
            // atan2 is not in wide; lane-extract.
            let aa = va.to_array();
            let ba = vb.to_array();
            let mut h = [0.0f32; 8];
            for k in 0..8 {
                h[k] = ba[k].atan2(aa[k]);
            }
            a[i..i + 8].copy_from_slice(&vc.to_array());
            b[i..i + 8].copy_from_slice(&h);
            i += 8;
        }
        for k in main..n {
            let av = a[k];
            let bv = b[k];
            a[k] = (av * av + bv * bv).sqrt();
            b[k] = bv.atan2(av);
        }
        src.into_space()
    }
}

/// OKLCh → OKLab: (L, C, h) → (L, a, b). Same layout, in place.
pub struct OklchToOklab;
impl InPlaceTransform for OklchToOklab {
    type In = Oklch;
    type Out = Oklab;
    fn apply(&self, mut src: Buffer<Oklch>) -> Buffer<Oklab> {
        let plane_size = src.plane_size();
        let (l, ch) = src.data_mut().split_at_mut(plane_size);
        let (c, h) = ch.split_at_mut(plane_size);
        let _ = l;
        let n = c.len();
        // sin/cos lane-extracted.
        for k in 0..n {
            let cv = c[k];
            let hv = h[k];
            c[k] = cv * hv.cos();
            h[k] = cv * hv.sin();
        }
        src.into_space()
    }
}

fn apply_3x3_simd(r: &mut [f32], g: &mut [f32], b: &mut [f32], m: [[f32; 3]; 3]) {
    crate::simd::apply_3x3_planar(r, g, b, m);
}

/// Cube root preserving sign. `cbrt(-x) = -cbrt(x)`. Lane-extracted scalar
/// `cbrt` (no SIMD cube root in `wide`); full SIMD cbrt is a Phase-C+ optimization.
fn cbrt_signed_inplace(plane: &mut [f32]) {
    for v in plane.iter_mut() {
        *v = v.cbrt();
    }
}

fn cube_inplace(plane: &mut [f32]) {
    let n = plane.len();
    let main = n - n % 8;
    let mut i = 0;
    while i < main {
        let v = f32x8::new(plane[i..i + 8].try_into().expect("8 lanes"));
        let v3 = v * v * v;
        plane[i..i + 8].copy_from_slice(&v3.to_array());
        i += 8;
    }
    for k in main..n {
        let v = plane[k];
        plane[k] = v * v * v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_linear_oklab_linear_is_identity() {
        // Use a non-trivial set of in-gamut linear sRGB values.
        let mut data = Vec::new();
        for r in [0.05, 0.2, 0.5, 0.8, 0.95] {
            for g in [0.05, 0.5, 0.95] {
                for b in [0.05, 0.5, 0.95] {
                    data.extend_from_slice(&[r, g, b]);
                }
            }
        }
        let n_pixels = data.len() / 3;
        let buf: Buffer<LinearSrgb> = Buffer::from_interleaved(&data, n_pixels as u32, 1);
        let original: Vec<f32> = buf.data().to_vec();
        let lab = LinearToOklab.apply(buf);
        let back = OklabToLinear.apply(lab);
        for (a, b) in original.iter().zip(back.data().iter()) {
            assert!((a - b).abs() < 1e-3, "drift: {a} -> {b}");
        }
    }

    #[test]
    fn oklab_oklch_roundtrip_preserves_lab() {
        // Random-ish lab values.
        let lab: Vec<f32> = vec![
            // L plane (4 values)
            0.5, 0.6, 0.4, 0.7, // a plane (4 values)
            0.1, -0.2, 0.05, 0.0, // b plane (4 values)
            -0.1, 0.2, 0.0, 0.15,
        ];
        let buf: Buffer<Oklab> = Buffer::from_planar(lab.clone(), 4, 1);
        let lch = OklabToOklch.apply(buf);
        let back = OklchToOklab.apply(lch);
        for (a, b) in lab.iter().zip(back.data().iter()) {
            assert!((a - b).abs() < 1e-5, "lab roundtrip: {a} -> {b}");
        }
    }

    #[test]
    fn oklab_white_has_zero_chroma() {
        // Linear sRGB (1, 1, 1) → OKLab L≈1, a≈0, b≈0.
        let buf: Buffer<LinearSrgb> = Buffer::from_planar(vec![1.0; 12], 4, 1);
        let lab = LinearToOklab.apply(buf);
        for &a in lab.g() {
            assert!(a.abs() < 1e-3, "a not zero: {a}");
        }
        for &b in lab.b() {
            assert!(b.abs() < 1e-3, "b not zero: {b}");
        }
        for &l in lab.r() {
            assert!((l - 1.0).abs() < 1e-3, "L not 1: {l}");
        }
    }

    #[test]
    fn oklab_black_is_zero() {
        let buf: Buffer<LinearSrgb> = Buffer::from_planar(vec![0.0; 12], 4, 1);
        let lab = LinearToOklab.apply(buf);
        for v in lab.data() {
            assert!(v.abs() < 1e-4, "expected ~0, got {v}");
        }
    }
}
