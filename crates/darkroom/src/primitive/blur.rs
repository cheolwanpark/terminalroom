//! Separable Gaussian blur on a 1-channel f32 plane.
//!
//! Used by Clarity (unsharp mask on OKLCh L). Two passes (horizontal then
//! vertical) so cost is O(w·h·r) instead of O(w·h·r²).
//!
//! Boundaries are clamped (replicate edge). For MVP this is correct and
//! visually fine; if Clarity halos appear at edges later, switch to mirror or
//! a guided filter.

/// Gaussian blur with the given sigma. Returns a fresh `Vec<f32>` of length
/// `width * height`.
pub fn gaussian_blur_1ch(plane: &[f32], width: u32, height: u32, sigma: f32) -> Vec<f32> {
    debug_assert_eq!(plane.len() as u32, width * height);
    if sigma <= 0.0 || width == 0 || height == 0 {
        return plane.to_vec();
    }
    let kernel = build_kernel(sigma);
    let mut tmp = vec![0.0f32; plane.len()];
    let mut out = vec![0.0f32; plane.len()];
    horizontal(plane, &mut tmp, width, height, &kernel);
    vertical(&tmp, &mut out, width, height, &kernel);
    out
}

fn build_kernel(sigma: f32) -> Vec<f32> {
    let radius = ((sigma * 3.0).ceil() as i32).max(1);
    let len = (2 * radius + 1) as usize;
    let mut k = vec![0.0f32; len];
    let inv_2_sigma_sq = 1.0 / (2.0 * sigma * sigma);
    let mut sum = 0.0_f32;
    for i in 0..len {
        let x = (i as i32 - radius) as f32;
        let v = (-x * x * inv_2_sigma_sq).exp();
        k[i] = v;
        sum += v;
    }
    for v in k.iter_mut() {
        *v /= sum;
    }
    k
}

fn horizontal(src: &[f32], dst: &mut [f32], width: u32, height: u32, kernel: &[f32]) {
    let radius = (kernel.len() / 2) as i32;
    let w = width as i32;
    let h = height as i32;
    for y in 0..h {
        let row = (y * w) as usize;
        for x in 0..w {
            let mut acc = 0.0_f32;
            for (k, &kv) in kernel.iter().enumerate() {
                let xi = (x + k as i32 - radius).clamp(0, w - 1) as usize;
                acc += kv * src[row + xi];
            }
            dst[row + x as usize] = acc;
        }
    }
}

fn vertical(src: &[f32], dst: &mut [f32], width: u32, height: u32, kernel: &[f32]) {
    let radius = (kernel.len() / 2) as i32;
    let w = width as i32;
    let h = height as i32;
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0_f32;
            for (k, &kv) in kernel.iter().enumerate() {
                let yi = (y + k as i32 - radius).clamp(0, h - 1);
                acc += kv * src[(yi * w + x) as usize];
            }
            dst[(y * w + x) as usize] = acc;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_sigma_is_identity() {
        let src: Vec<f32> = (0..16).map(|i| i as f32).collect();
        let out = gaussian_blur_1ch(&src, 4, 4, 0.0);
        assert_eq!(out, src);
    }

    #[test]
    fn constant_input_constant_output() {
        let src = vec![0.5_f32; 64];
        let out = gaussian_blur_1ch(&src, 8, 8, 1.5);
        for v in out {
            assert!((v - 0.5).abs() < 1e-4);
        }
    }

    #[test]
    fn impulse_response_sums_to_one() {
        // Single bright pixel in a sea of zeros: total energy preserved.
        let mut src = vec![0.0_f32; 81];
        src[40] = 1.0;
        let out = gaussian_blur_1ch(&src, 9, 9, 1.0);
        let total: f32 = out.iter().sum();
        assert!((total - 1.0).abs() < 1e-3, "energy {total}");
    }

    #[test]
    fn build_kernel_normalized() {
        let k = build_kernel(2.0);
        let s: f32 = k.iter().sum();
        assert!((s - 1.0).abs() < 1e-6);
    }
}
