//! sRGB transfer encode/decode.
//!
//! `SrgbEncode` quantizes a `Buffer<LinearSrgb>` (planar f32 in [0, 1] linear
//! light, with clamping) into an interleaved 8-bit `Srgb8` via a precomputed
//! 16-bit linear → 8-bit sRGB LUT.
//!
//! `SrgbDecode` is the inverse: `Srgb8` (8-bit display-referred) →
//! `Buffer<LinearSrgb>` (planar f32 linear light), via a precomputed 256-entry
//! u8 → f32 LUT.

use std::sync::OnceLock;

use wide::f32x8;

use crate::space::{Buffer, LinearSrgb, Srgb8};
use crate::transform::Transform;

/// `Buffer<LinearSrgb>` → `Srgb8`. Clamps to [0, 1], scales to 16-bit linear,
/// looks up the sRGB transfer LUT, interleaves RGB into the output bytes.
pub struct SrgbEncode;
impl Transform for SrgbEncode {
    type Input = Buffer<LinearSrgb>;
    type Output = Srgb8;
    fn apply(&self, src: Buffer<LinearSrgb>) -> Srgb8 {
        let (w, h) = src.dimensions();
        let plane = src.plane_size();
        let lut = encode_lut();

        // SIMD-quantize each plane to 16-bit indices, then scalar LUT lookup.
        let mut r_idx = vec![0u16; plane];
        let mut g_idx = vec![0u16; plane];
        let mut b_idx = vec![0u16; plane];
        quantize_to_u16(src.r(), &mut r_idx);
        quantize_to_u16(src.g(), &mut g_idx);
        quantize_to_u16(src.b(), &mut b_idx);

        let mut pixels = Vec::with_capacity(plane * 3);
        for i in 0..plane {
            pixels.push(lut[r_idx[i] as usize]);
            pixels.push(lut[g_idx[i] as usize]);
            pixels.push(lut[b_idx[i] as usize]);
        }
        Srgb8 {
            width: w,
            height: h,
            pixels,
        }
    }
}

/// `Srgb8` → `Buffer<LinearSrgb>`. Per-byte LUT into linear light, then
/// deinterleave R/G/B into planes.
pub struct SrgbDecode;
impl Transform for SrgbDecode {
    type Input = Srgb8;
    type Output = Buffer<LinearSrgb>;
    fn apply(&self, src: Srgb8) -> Buffer<LinearSrgb> {
        let plane = (src.width as usize) * (src.height as usize);
        debug_assert_eq!(src.pixels.len(), plane * 3);
        let lut = decode_lut();
        let mut data = vec![0.0f32; plane * 3];
        let (r, gb) = data.split_at_mut(plane);
        let (g, b) = gb.split_at_mut(plane);
        for (i, px) in src.pixels.chunks_exact(3).enumerate() {
            r[i] = lut[px[0] as usize];
            g[i] = lut[px[1] as usize];
            b[i] = lut[px[2] as usize];
        }
        Buffer::from_planar(data, src.width, src.height)
    }
}

fn quantize_to_u16(plane: &[f32], out: &mut [u16]) {
    debug_assert_eq!(plane.len(), out.len());
    let n = plane.len();
    let main = n - n % 8;
    let zero = f32x8::splat(0.0);
    let one = f32x8::splat(1.0);
    let scale = f32x8::splat(65535.0);
    let mut i = 0;
    while i < main {
        let v: [f32; 8] = plane[i..i + 8].try_into().expect("8 lanes");
        let v = f32x8::new(v).max(zero).min(one) * scale;
        let arr = v.round().to_array();
        for (k, &f) in arr.iter().enumerate() {
            out[i + k] = (f as u32).min(65535) as u16;
        }
        i += 8;
    }
    for k in main..n {
        let v = plane[k].clamp(0.0, 1.0) * 65535.0;
        out[k] = (v.round() as u32).min(65535) as u16;
    }
}

/// Precomputed 65536-entry LUT mapping 16-bit linear sRGB → 8-bit sRGB.
fn encode_lut() -> &'static [u8; 65536] {
    static LUT: OnceLock<Box<[u8; 65536]>> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = Box::new([0u8; 65536]);
        for v in 0..=65535u32 {
            let linear = v as f64 / 65535.0;
            let srgb = if linear <= 0.0031308 {
                linear * 12.92
            } else {
                1.055 * linear.powf(1.0 / 2.4) - 0.055
            };
            lut[v as usize] = (srgb.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
        lut
    })
}

/// Precomputed 256-entry LUT mapping 8-bit sRGB → linear light f32 in [0, 1].
fn decode_lut() -> &'static [f32; 256] {
    static LUT: OnceLock<Box<[f32; 256]>> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = Box::new([0.0f32; 256]);
        for v in 0..=255u32 {
            let s = v as f64 / 255.0;
            let linear = if s <= 0.04045 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            };
            lut[v as usize] = linear as f32;
        }
        lut
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_lut_endpoints() {
        let lut = encode_lut();
        assert_eq!(lut[0], 0);
        assert_eq!(lut[65535], 255);
    }

    #[test]
    fn decode_lut_endpoints() {
        let lut = decode_lut();
        assert_eq!(lut[0], 0.0);
        assert!((lut[255] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn srgb_encode_clamps_negative_and_out_of_range() {
        let data = vec![-0.5, 2.0, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5];
        // 2x2 = 4 pixels, planar (4 R, 4 G, 4 B).
        let buf: Buffer<LinearSrgb> = Buffer::from_planar(data, 2, 2);
        let out = SrgbEncode.apply(buf);
        // First R is -0.5 → clamped to 0 → encoded → 0.
        assert_eq!(out.pixels[0], 0);
        // Second R is 2.0 → clamped to 1 → encoded → 255.
        assert_eq!(out.pixels[3], 255);
    }

    #[test]
    fn srgb_encode_mid_gray_lands_at_srgb_midtone() {
        // Linear 0.5 → sRGB ≈ 0.7354 → ~188.
        let plane: Vec<f32> = vec![0.5; 16];
        let mut data = Vec::with_capacity(48);
        data.extend_from_slice(&plane);
        data.extend_from_slice(&plane);
        data.extend_from_slice(&plane);
        let buf: Buffer<LinearSrgb> = Buffer::from_planar(data, 4, 4);
        let out = SrgbEncode.apply(buf);
        for &b in &out.pixels {
            assert!((185..=190).contains(&b), "got {b}");
        }
    }

    #[test]
    fn srgb_decode_then_encode_is_identity() {
        // 0..255 in steps of 1, R-only image. Decode → encode should round-trip.
        let mut pixels = Vec::with_capacity(256 * 3);
        for v in 0..=255u8 {
            pixels.push(v);
            pixels.push(v);
            pixels.push(v);
        }
        let src = Srgb8 {
            width: 256,
            height: 1,
            pixels,
        };
        let lin = SrgbDecode.apply(src.clone());
        let back = SrgbEncode.apply(lin);
        for (i, (&a, &b)) in src.pixels.iter().zip(back.pixels.iter()).enumerate() {
            assert!(
                (a as i32 - b as i32).abs() <= 1,
                "drift at byte {i}: {a} -> {b}"
            );
        }
    }

    #[test]
    fn quantize_to_u16_handles_tail() {
        // 11 values: 8 main + 3 tail.
        let mut out = vec![0u16; 11];
        let plane: Vec<f32> = (0..11).map(|i| i as f32 / 11.0).collect();
        quantize_to_u16(&plane, &mut out);
        for (i, &v) in out.iter().enumerate() {
            let expected = ((i as f32 / 11.0).clamp(0.0, 1.0) * 65535.0).round() as u32;
            assert_eq!(v as u32, expected);
        }
    }
}
