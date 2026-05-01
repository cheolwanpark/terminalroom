//! Detail / texture knobs.
//!
//! - `Clarity` — local-contrast (unsharp mask) on OKLCh L.
//! - `Grain` — additive deterministic noise on `LinearSrgb` just before
//!   `SrgbEncode`, so the noise lands in the perceptual encode rather than
//!   being compressed by the sRGB curve.

use crate::control::Control;
use crate::primitive::blur::gaussian_blur_1ch;
use crate::primitive::noise::noise_at;
use crate::space::{Buffer, LinearSrgb, Oklch};

fn iso_attenuation(iso: f32) -> f32 {
    if iso > 1600.0 {
        (1600.0 / iso).clamp(0.4, 1.0)
    } else {
        1.0
    }
}

/// Local-contrast / micro-contrast. Operates on the L channel of OKLCh
/// (already in the buffer at the detail stage). `value > 0` adds clarity
/// (crisper midtones); `value < 0` softens. Capped at high ISO.
#[derive(Debug, Clone, Copy)]
pub struct Clarity {
    pub value: f32,
    pub iso: f32,
}

impl Default for Clarity {
    fn default() -> Self {
        Self {
            value: 0.0,
            iso: 100.0,
        }
    }
}

impl Control for Clarity {
    type Space = Oklch;
    fn apply(&self, image: &mut Buffer<Oklch>) {
        if self.value == 0.0 {
            return;
        }
        let amount = self.value * iso_attenuation(self.iso);
        let (w, h) = image.dimensions();
        let plane = image.plane_size();
        let l_clone = image.data()[..plane].to_vec();
        let blurred = gaussian_blur_1ch(&l_clone, w, h, 2.0);
        let l_mut = &mut image.data_mut()[..plane];
        for i in 0..plane {
            let lv = l_clone[i];
            let detail = lv - blurred[i];
            let mask = clarity_mask(lv);
            l_mut[i] = (lv + amount * detail * mask).max(0.0).min(1.5);
        }
    }
}

/// Bell-curve mask over the OKLab L domain. Peaks broadly in midtones
/// (L ≈ 0.4–0.7) and rolls off into deep shadows (L < 0.15) and clipped
/// highlights (L > 0.9), where unsharp masking is undesirable.
fn clarity_mask(l: f32) -> f32 {
    let lo = smoothstep(0.10, 0.35, l);
    let hi = 1.0 - smoothstep(0.70, 0.95, l);
    (lo * hi).clamp(0.0, 1.0)
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge1 == edge0 {
        return if x < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Additive deterministic luminance noise, applied just before sRGB encode.
/// `amount` in [0, 1]. `iso` attenuates at high ISO where sensor noise
/// already dominates.
#[derive(Debug, Clone, Copy)]
pub struct Grain {
    pub amount: f32,
    pub iso: f32,
    pub seed: u64,
}

impl Default for Grain {
    fn default() -> Self {
        Self {
            amount: 0.0,
            iso: 100.0,
            seed: 0,
        }
    }
}

impl Control for Grain {
    type Space = LinearSrgb;
    fn apply(&self, image: &mut Buffer<LinearSrgb>) {
        let amount = self.amount.clamp(0.0, 1.0);
        if amount == 0.0 {
            return;
        }
        let iso_atten = if self.iso > 1600.0 {
            (1600.0 / self.iso).clamp(0.5, 1.0)
        } else {
            1.0
        };
        let strength = amount * iso_atten * 0.05;
        let (w, h) = image.dimensions();
        let (r, g, b) = image.rgb_planes_mut();
        for y in 0..h {
            for x in 0..w {
                let i = (y as usize) * (w as usize) + (x as usize);
                let n = noise_at(x, y, self.seed) * strength;
                r[i] += n;
                g[i] += n;
                b[i] += n;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clarity_zero_is_noop() {
        let data: Vec<f32> = (0..12).map(|i| i as f32 * 0.05).collect();
        let mut buf: Buffer<Oklch> = Buffer::from_planar(data.clone(), 4, 1);
        Clarity {
            value: 0.0,
            iso: 100.0,
        }
        .apply(&mut buf);
        for (a, b) in data.iter().zip(buf.data().iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn clarity_amplifies_local_contrast_in_midtones() {
        // Image with midtone variation (L plane: alternating 0.3 / 0.5 / ...).
        let mut l = vec![0.3_f32; 64];
        for i in 0..64 {
            l[i] = if i % 2 == 0 { 0.3 } else { 0.5 };
        }
        let plane: usize = 64;
        let mut data = vec![0.0_f32; plane * 3];
        data[..plane].copy_from_slice(&l);
        // C and h zero.
        let mut buf: Buffer<Oklch> = Buffer::from_planar(data, 8, 8);
        let original = buf.data().to_vec();
        Clarity {
            value: 1.0,
            iso: 100.0,
        }
        .apply(&mut buf);
        let mut diff = 0.0_f32;
        for (a, b) in original.iter().zip(buf.data().iter()) {
            diff += (a - b).abs();
        }
        assert!(diff > 1e-3, "clarity should change L plane: diff {diff}");
    }

    #[test]
    fn grain_zero_is_noop() {
        let data = vec![0.5_f32; 24];
        let mut buf: Buffer<LinearSrgb> = Buffer::from_planar(data.clone(), 4, 2);
        Grain {
            amount: 0.0,
            iso: 100.0,
            seed: 0,
        }
        .apply(&mut buf);
        assert_eq!(buf.data(), data.as_slice());
    }

    #[test]
    fn grain_perturbs_within_bounded_range() {
        let mut buf: Buffer<LinearSrgb> = Buffer::from_planar(vec![0.5_f32; 48], 4, 4);
        Grain {
            amount: 1.0,
            iso: 100.0,
            seed: 42,
        }
        .apply(&mut buf);
        for &v in buf.data() {
            // Default strength is 1.0 * 1.0 * 0.05 = 0.05. So values in [0.45, 0.55].
            assert!((0.44..=0.56).contains(&v), "out of range: {v}");
        }
    }

    #[test]
    fn grain_deterministic_across_calls() {
        let mut a: Buffer<LinearSrgb> = Buffer::from_planar(vec![0.5_f32; 48], 4, 4);
        let mut b: Buffer<LinearSrgb> = Buffer::from_planar(vec![0.5_f32; 48], 4, 4);
        Grain {
            amount: 1.0,
            iso: 100.0,
            seed: 7,
        }
        .apply(&mut a);
        Grain {
            amount: 1.0,
            iso: 100.0,
            seed: 7,
        }
        .apply(&mut b);
        assert_eq!(a.data(), b.data());
    }
}
