//! SIMD helpers built on `wide::f32x8`.
//!
//! These operate on planar f32 channel slices (the layout of `Buffer<S>`).
//! Because the data is planar, an `f32x8` load pulls 8 same-channel pixels in
//! one instruction with no shuffle, which is the entire point.
//!
//! `wide` is compile-time dispatched (NEON on aarch64, SSE2 baseline on
//! x86_64; AVX2 only with `-C target-feature=+avx2` or similar). For runtime
//! AVX2 selection on shipped binaries we use `pulp` in the heavier kernels
//! (Phase C: OKLab cbrt, Gaussian blur).

use wide::f32x8;

/// In-place per-element f(x) on a single channel plane.
///
/// The tail (length not divisible by 8) is padded into an 8-lane buffer,
/// processed once, and only the relevant prefix is written back.
pub fn map_f32x8<F>(plane: &mut [f32], mut f: F)
where
    F: FnMut(f32x8) -> f32x8,
{
    let n = plane.len();
    let main = n - n % 8;
    for chunk in plane[..main].chunks_exact_mut(8) {
        let arr: [f32; 8] = chunk.try_into().expect("chunk length is exactly 8");
        let r = f(f32x8::new(arr));
        chunk.copy_from_slice(&r.to_array());
    }
    let rem = n - main;
    if rem > 0 {
        let tail = &mut plane[main..];
        let mut buf = [0.0f32; 8];
        buf[..rem].copy_from_slice(tail);
        let r = f(f32x8::new(buf)).to_array();
        tail.copy_from_slice(&r[..rem]);
    }
}

/// In-place per-pixel f(r, g, b) → (r', g', b') on three planar channels.
///
/// All three slices must be the same length. Like `map_f32x8`, the tail is
/// handled by padding into 8-lane buffers and writing back the prefix.
pub fn map_pixel_f32x8<F>(r: &mut [f32], g: &mut [f32], b: &mut [f32], mut f: F)
where
    F: FnMut(f32x8, f32x8, f32x8) -> (f32x8, f32x8, f32x8),
{
    debug_assert_eq!(r.len(), g.len());
    debug_assert_eq!(g.len(), b.len());
    let n = r.len();
    let main = n - n % 8;
    let mut i = 0;
    while i < main {
        let rv = f32x8::new(r[i..i + 8].try_into().expect("8-lane slice"));
        let gv = f32x8::new(g[i..i + 8].try_into().expect("8-lane slice"));
        let bv = f32x8::new(b[i..i + 8].try_into().expect("8-lane slice"));
        let (nr, ng, nb) = f(rv, gv, bv);
        r[i..i + 8].copy_from_slice(&nr.to_array());
        g[i..i + 8].copy_from_slice(&ng.to_array());
        b[i..i + 8].copy_from_slice(&nb.to_array());
        i += 8;
    }
    let rem = n - main;
    if rem > 0 {
        let mut rb = [0.0f32; 8];
        let mut gb = [0.0f32; 8];
        let mut bb = [0.0f32; 8];
        rb[..rem].copy_from_slice(&r[main..]);
        gb[..rem].copy_from_slice(&g[main..]);
        bb[..rem].copy_from_slice(&b[main..]);
        let (nr, ng, nb) = f(f32x8::new(rb), f32x8::new(gb), f32x8::new(bb));
        let nra = nr.to_array();
        let nga = ng.to_array();
        let nba = nb.to_array();
        r[main..].copy_from_slice(&nra[..rem]);
        g[main..].copy_from_slice(&nga[..rem]);
        b[main..].copy_from_slice(&nba[..rem]);
    }
}

/// In-place 3×3 matrix multiply over three planar RGB channels.
///
/// `[r', g', b']ᵀ = m * [r, g, b]ᵀ`. No clamping — the caller decides whether
/// to clamp afterwards (e.g. `LinearSrgb` may legitimately go negative for
/// out-of-gamut Rec.2020 content; the encode step clamps).
pub fn apply_3x3_planar(r: &mut [f32], g: &mut [f32], b: &mut [f32], m: [[f32; 3]; 3]) {
    let m00 = f32x8::splat(m[0][0]);
    let m01 = f32x8::splat(m[0][1]);
    let m02 = f32x8::splat(m[0][2]);
    let m10 = f32x8::splat(m[1][0]);
    let m11 = f32x8::splat(m[1][1]);
    let m12 = f32x8::splat(m[1][2]);
    let m20 = f32x8::splat(m[2][0]);
    let m21 = f32x8::splat(m[2][1]);
    let m22 = f32x8::splat(m[2][2]);
    map_pixel_f32x8(r, g, b, |vr, vg, vb| {
        let nr = m00 * vr + m01 * vg + m02 * vb;
        let ng = m10 * vr + m11 * vg + m12 * vb;
        let nb = m20 * vr + m21 * vg + m22 * vb;
        (nr, ng, nb)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_f32x8_doubles_full_lanes_and_tail() {
        // 19 elements: 16 main + 3 tail.
        let mut buf: Vec<f32> = (0..19).map(|i| i as f32).collect();
        let expected: Vec<f32> = buf.iter().map(|&v| v * 2.0).collect();
        map_f32x8(&mut buf, |v| v + v);
        assert_eq!(buf, expected);
    }

    #[test]
    fn map_f32x8_handles_empty_slice() {
        let mut buf: Vec<f32> = Vec::new();
        map_f32x8(&mut buf, |v| v * f32x8::splat(2.0));
        assert!(buf.is_empty());
    }

    #[test]
    fn map_f32x8_handles_short_slice() {
        let mut buf: Vec<f32> = vec![1.0, 2.0, 3.0];
        map_f32x8(&mut buf, |v| v + f32x8::splat(10.0));
        assert_eq!(buf, vec![11.0, 12.0, 13.0]);
    }

    #[test]
    fn map_pixel_f32x8_swaps_channels() {
        // 11 pixels: 8 main + 3 tail.
        let mut r: Vec<f32> = (0..11).map(|i| i as f32).collect();
        let mut g: Vec<f32> = (0..11).map(|i| 100.0 + i as f32).collect();
        let mut b: Vec<f32> = (0..11).map(|i| 200.0 + i as f32).collect();
        let r0 = r.clone();
        let g0 = g.clone();
        let b0 = b.clone();
        map_pixel_f32x8(&mut r, &mut g, &mut b, |vr, vg, vb| (vb, vr, vg));
        assert_eq!(r, b0);
        assert_eq!(g, r0);
        assert_eq!(b, g0);
    }

    #[test]
    fn apply_3x3_planar_identity_is_noop() {
        let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let mut r: Vec<f32> = (0..17).map(|i| i as f32).collect();
        let mut g: Vec<f32> = (0..17).map(|i| 50.0 + i as f32).collect();
        let mut b: Vec<f32> = (0..17).map(|i| 100.0 + i as f32).collect();
        let r0 = r.clone();
        let g0 = g.clone();
        let b0 = b.clone();
        apply_3x3_planar(&mut r, &mut g, &mut b, identity);
        assert_eq!(r, r0);
        assert_eq!(g, g0);
        assert_eq!(b, b0);
    }

    #[test]
    fn apply_3x3_planar_white_preserves_white_with_summing_rows() {
        // A matrix whose rows each sum to 1 must map white (1,1,1) to white.
        // BT.2020→BT.709 is one such example.
        let m = [
            [1.66049, -0.58764, -0.07285],
            [-0.12455, 1.13290, -0.00835],
            [-0.01815, -0.10058, 1.11873],
        ];
        let mut r = vec![1.0; 8];
        let mut g = vec![1.0; 8];
        let mut b = vec![1.0; 8];
        apply_3x3_planar(&mut r, &mut g, &mut b, m);
        for v in r.iter().chain(g.iter()).chain(b.iter()) {
            assert!((*v - 1.0).abs() < 1e-4, "expected ≈1, got {v}");
        }
    }

    #[test]
    fn apply_3x3_planar_swap_rb_via_matrix() {
        // Pure permutation: swap R and B.
        let m = [[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]];
        let mut r: Vec<f32> = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0];
        let mut g: Vec<f32> = vec![10.0; 9];
        let mut b: Vec<f32> = vec![
            100.0, 200.0, 300.0, 400.0, 500.0, 600.0, 700.0, 800.0, 900.0,
        ];
        let r0 = r.clone();
        let b0 = b.clone();
        apply_3x3_planar(&mut r, &mut g, &mut b, m);
        assert_eq!(r, b0);
        assert_eq!(b, r0);
        assert!(g.iter().all(|&v| (v - 10.0).abs() < 1e-5));
    }
}
