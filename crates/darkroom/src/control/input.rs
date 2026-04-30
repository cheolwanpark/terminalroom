//! Input-correction knobs. Operate before the camera→working transform
//! (Temperature, Tint on `CameraLinear`) or just after it (Exposure on
//! `LinearRec2020`).

use wide::f32x8;

use crate::control::Control;
use crate::simd::map_f32x8;
use crate::space::{Buffer, CameraLinear, LinearRec2020};

/// Exposure compensation in EV. `rgb *= 2^ev`. Applies in the working space
/// (linear Rec.2020) so the gain is uniform per channel after WB+matrix.
#[derive(Debug, Clone, Copy)]
pub struct Exposure {
    pub ev: f32,
}

impl Default for Exposure {
    fn default() -> Self {
        Self { ev: 0.0 }
    }
}

impl Control for Exposure {
    type Space = LinearRec2020;
    fn apply(&self, image: &mut Buffer<LinearRec2020>) {
        if self.ev == 0.0 {
            return;
        }
        let gain = 2.0_f32.powf(self.ev);
        let g = f32x8::splat(gain);
        let (r, gp, b) = image.rgb_planes_mut();
        for plane in [r, gp, b] {
            map_f32x8(plane, |v| v * g);
        }
    }
}

/// White-balance temperature in Kelvin. Adjusts R/B gain in camera-linear:
/// warmer (lower K) boosts R / drops B; cooler (higher K) the inverse.
///
/// MVP model: linear deviation from a 5500 K reference. Real WB curves
/// (Planckian locus) refine this later.
#[derive(Debug, Clone, Copy)]
pub struct Temperature {
    pub kelvin: f32,
}

impl Default for Temperature {
    fn default() -> Self {
        Self { kelvin: 5500.0 }
    }
}

impl Control for Temperature {
    type Space = CameraLinear;
    fn apply(&self, image: &mut Buffer<CameraLinear>) {
        let dev = (self.kelvin - 5500.0) / 1000.0;
        if dev == 0.0 {
            return;
        }
        let r_gain = (1.0 - dev * 0.05).max(0.0);
        let b_gain = (1.0 + dev * 0.05).max(0.0);
        let wr = f32x8::splat(r_gain);
        let wb = f32x8::splat(b_gain);
        let (r, _g, b) = image.rgb_planes_mut();
        map_f32x8(r, |v| v * wr);
        map_f32x8(b, |v| v * wb);
    }
}

/// Tint along the green ↔ magenta axis. `value > 0` adds magenta (reduces G),
/// `value < 0` adds green (boosts G). Range typically -1..1.
#[derive(Debug, Clone, Copy)]
pub struct Tint {
    pub value: f32,
}

impl Default for Tint {
    fn default() -> Self {
        Self { value: 0.0 }
    }
}

impl Control for Tint {
    type Space = CameraLinear;
    fn apply(&self, image: &mut Buffer<CameraLinear>) {
        if self.value == 0.0 {
            return;
        }
        let g_gain = (1.0 - self.value * 0.1).max(0.0);
        let wg = f32x8::splat(g_gain);
        let (_r, g, _b) = image.rgb_planes_mut();
        map_f32x8(g, |v| v * wg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposure_zero_is_noop() {
        let mut buf: Buffer<LinearRec2020> = Buffer::from_planar(vec![0.5; 24], 4, 2);
        let original = buf.data().to_vec();
        Exposure { ev: 0.0 }.apply(&mut buf);
        assert_eq!(buf.data(), original.as_slice());
    }

    #[test]
    fn exposure_plus_one_doubles_rgb() {
        let mut buf: Buffer<LinearRec2020> = Buffer::from_planar(vec![0.25; 24], 4, 2);
        Exposure { ev: 1.0 }.apply(&mut buf);
        for &v in buf.data() {
            assert!((v - 0.5).abs() < 1e-5);
        }
    }

    #[test]
    fn exposure_minus_one_halves_rgb() {
        let mut buf: Buffer<LinearRec2020> = Buffer::from_planar(vec![0.4; 24], 4, 2);
        Exposure { ev: -1.0 }.apply(&mut buf);
        for &v in buf.data() {
            assert!((v - 0.2).abs() < 1e-5);
        }
    }

    #[test]
    fn temperature_warmer_increases_r_and_decreases_b() {
        let mut buf: Buffer<CameraLinear> = Buffer::from_planar(vec![0.5; 24], 4, 2);
        Temperature { kelvin: 4500.0 }.apply(&mut buf);
        let r_avg: f32 = buf.r().iter().sum::<f32>() / buf.r().len() as f32;
        let b_avg: f32 = buf.b().iter().sum::<f32>() / buf.b().len() as f32;
        assert!(r_avg > 0.5);
        assert!(b_avg < 0.5);
    }

    #[test]
    fn temperature_cooler_decreases_r_and_increases_b() {
        let mut buf: Buffer<CameraLinear> = Buffer::from_planar(vec![0.5; 24], 4, 2);
        Temperature { kelvin: 6500.0 }.apply(&mut buf);
        let r_avg: f32 = buf.r().iter().sum::<f32>() / buf.r().len() as f32;
        let b_avg: f32 = buf.b().iter().sum::<f32>() / buf.b().len() as f32;
        assert!(r_avg < 0.5);
        assert!(b_avg > 0.5);
    }

    #[test]
    fn tint_zero_is_noop() {
        let mut buf: Buffer<CameraLinear> = Buffer::from_planar(vec![0.5; 24], 4, 2);
        let original = buf.data().to_vec();
        Tint { value: 0.0 }.apply(&mut buf);
        assert_eq!(buf.data(), original.as_slice());
    }

    #[test]
    fn tint_positive_reduces_g() {
        let mut buf: Buffer<CameraLinear> = Buffer::from_planar(vec![0.5; 24], 4, 2);
        Tint { value: 1.0 }.apply(&mut buf);
        for &v in buf.g() {
            assert!(v < 0.5);
        }
        for &v in buf.r() {
            assert_eq!(v, 0.5);
        }
        for &v in buf.b() {
            assert_eq!(v, 0.5);
        }
    }

    #[test]
    fn tint_negative_boosts_g() {
        let mut buf: Buffer<CameraLinear> = Buffer::from_planar(vec![0.5; 24], 4, 2);
        Tint { value: -1.0 }.apply(&mut buf);
        for &v in buf.g() {
            assert!(v > 0.5);
        }
    }
}
