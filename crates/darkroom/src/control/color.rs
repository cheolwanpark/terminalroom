//! Color fine-tune knobs operating in OKLab / OKLCh.

use crate::control::Control;
use crate::primitive::protect::skin_protection;
use crate::space::{Buffer, Oklab, Oklch};

/// Creative warm/cool bias along the OKLab b-axis. `value > 0` warms (yellow),
/// `value < 0` cools (blue). Stronger in highlights, weaker in shadows.
#[derive(Debug, Clone, Copy)]
pub struct Warmth {
    pub value: f32,
}

impl Default for Warmth {
    fn default() -> Self {
        Self { value: 0.0 }
    }
}

impl Control for Warmth {
    type Space = Oklab;
    fn apply(&self, image: &mut Buffer<Oklab>) {
        if self.value == 0.0 {
            return;
        }
        let plane = image.plane_size();
        let amount = self.value * 0.05;
        let (l, ab) = image.data_mut().split_at_mut(plane);
        let (_a, b) = ab.split_at_mut(plane);
        for i in 0..plane {
            let weight = (l[i] - 0.2).clamp(0.0, 1.0);
            b[i] += amount * weight;
        }
    }
}

/// Vibrance-aware chroma scale. Internally:
/// - low chroma gets a larger boost (vibrance);
/// - high chroma gets a smaller boost (compression-aware);
/// - skin hues attenuated to avoid over-saturation;
/// - bright highlights attenuated;
/// - high ISO caps the maximum boost.
#[derive(Debug, Clone, Copy)]
pub struct Color {
    pub value: f32,
    pub iso: f32,
}

impl Default for Color {
    fn default() -> Self {
        Self {
            value: 0.0,
            iso: 100.0,
        }
    }
}

impl Control for Color {
    type Space = Oklch;
    fn apply(&self, image: &mut Buffer<Oklch>) {
        if self.value == 0.0 {
            return;
        }
        let amount = self.value;
        let iso_cap = if self.iso > 3200.0 { 0.5 } else { 1.0 };
        let plane = image.plane_size();
        let (l, ch) = image.data_mut().split_at_mut(plane);
        let (c, h) = ch.split_at_mut(plane);
        for i in 0..plane {
            let lv = l[i];
            let cv = c[i];
            let hv = h[i];
            let vib_weight = (1.0 - (cv / 0.3).min(1.0)) * 0.6 + 0.4;
            let skin = skin_protection(lv, cv, hv);
            let hl_guard = 1.0 - ((lv - 0.85) / 0.15).clamp(0.0, 1.0) * 0.5;
            let factor = 1.0 + amount * vib_weight * skin * hl_guard * iso_cap;
            c[i] = (cv * factor).max(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warmth_zero_is_noop() {
        let data: Vec<f32> = (0..12).map(|i| i as f32 * 0.05).collect();
        let mut buf: Buffer<Oklab> = Buffer::from_planar(data.clone(), 4, 1);
        Warmth { value: 0.0 }.apply(&mut buf);
        assert_eq!(buf.data(), data.as_slice());
    }

    #[test]
    fn warmth_positive_increases_b_axis() {
        // 4 pixels, L=0.5, a=0, b=0.
        let data: Vec<f32> = vec![0.5, 0.5, 0.5, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let mut buf: Buffer<Oklab> = Buffer::from_planar(data, 4, 1);
        Warmth { value: 1.0 }.apply(&mut buf);
        for &b in buf.b() {
            assert!(b > 0.0, "expected positive b, got {b}");
        }
    }

    #[test]
    fn color_zero_is_noop() {
        let data: Vec<f32> = (0..12).map(|i| i as f32 * 0.05).collect();
        let mut buf: Buffer<Oklch> = Buffer::from_planar(data.clone(), 4, 1);
        Color {
            value: 0.0,
            iso: 100.0,
        }
        .apply(&mut buf);
        assert_eq!(buf.data(), data.as_slice());
    }

    #[test]
    fn color_positive_boosts_low_chroma() {
        // L=0.5, C=0.05 (low chroma), h=0.
        let data: Vec<f32> = vec![
            0.5, 0.5, 0.5, 0.5, 0.05, 0.05, 0.05, 0.05, 0.0, 0.0, 0.0, 0.0,
        ];
        let mut buf: Buffer<Oklch> = Buffer::from_planar(data, 4, 1);
        Color {
            value: 1.0,
            iso: 100.0,
        }
        .apply(&mut buf);
        for &c in buf.g() {
            assert!(c > 0.05, "low chroma should boost, got {c}");
        }
    }

    #[test]
    fn color_high_iso_caps_boost() {
        let data: Vec<f32> = vec![0.5; 12];
        let mut low: Buffer<Oklch> = Buffer::from_planar(data.clone(), 4, 1);
        let mut high: Buffer<Oklch> = Buffer::from_planar(data, 4, 1);
        // Set C to a low non-zero so we can detect the boost.
        for v in &mut low.data_mut()[4..8] {
            *v = 0.1;
        }
        for v in &mut high.data_mut()[4..8] {
            *v = 0.1;
        }
        Color {
            value: 1.0,
            iso: 100.0,
        }
        .apply(&mut low);
        Color {
            value: 1.0,
            iso: 12800.0,
        }
        .apply(&mut high);
        let low_c: f32 = low.g().iter().sum();
        let high_c: f32 = high.g().iter().sum();
        assert!(
            low_c > high_c,
            "ISO cap: low {low_c} should exceed high {high_c}"
        );
    }
}
