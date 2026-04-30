//! Tone-fine-tune knobs. Hue-preserving: extract Y, modify Y in log-luminance
//! space, scale RGB by `Y'/Y` to preserve chromaticity. The four tone-domain
//! controls (Contrast, Shadows, Blacks, SoftHighlightsTone) operate on
//! `LinearRec2020`. SoftHighlightsChroma operates in OKLCh after color stage.

use crate::control::Control;
use crate::primitive::curve::{MIDDLE_GRAY, ToneCurve, apply_curve_to_luma};
use crate::primitive::luminance::{luminance_rec2020, rescale_rec2020_to_luma};
use crate::primitive::mask::{highlight_mask, near_black_mask, shadow_mask};
use crate::space::{Buffer, LinearRec2020, Oklch};

fn iso_attenuation(iso: f32) -> f32 {
    if iso > 1600.0 {
        (1600.0 / iso).clamp(0.4, 1.0)
    } else {
        1.0
    }
}

/// Midtone contrast. `value` in [-1, 1]. `value > 0` increases slope
/// (steeper midtone), `value < 0` flattens.
#[derive(Debug, Clone, Copy)]
pub struct Contrast {
    pub value: f32,
}

impl Default for Contrast {
    fn default() -> Self {
        Self { value: 0.0 }
    }
}

impl Control for Contrast {
    type Space = LinearRec2020;
    fn apply(&self, image: &mut Buffer<LinearRec2020>) {
        if self.value == 0.0 {
            return;
        }
        let slope = 2.0_f32.powf(self.value);
        let curve = ToneCurve {
            pivot: 0.0,
            slope,
            toe: 0.0,
            shoulder: 0.0,
        };
        let y_old = luminance_rec2020(image);
        let mut y_new = vec![0.0; y_old.len()];
        apply_curve_to_luma(&curve, &y_old, &mut y_new);
        rescale_rec2020_to_luma(image, &y_old, &y_new);
    }
}

/// Shadow lift / deepen. `value > 0` lifts (brightens) shadows; `value < 0`
/// deepens. Capped at high ISO to avoid amplifying noise.
#[derive(Debug, Clone, Copy)]
pub struct Shadows {
    pub value: f32,
    pub iso: f32,
}

impl Default for Shadows {
    fn default() -> Self {
        Self {
            value: 0.0,
            iso: 100.0,
        }
    }
}

impl Control for Shadows {
    type Space = LinearRec2020;
    fn apply(&self, image: &mut Buffer<LinearRec2020>) {
        if self.value == 0.0 {
            return;
        }
        let amount = self.value * iso_attenuation(self.iso);
        let inv_mid = 1.0 / MIDDLE_GRAY;
        let y_old = luminance_rec2020(image);
        let mut y_new = y_old.clone();
        for i in 0..y_old.len() {
            if y_old[i] <= 0.0 {
                continue;
            }
            let log_y = (y_old[i] * inv_mid).log2();
            let mask = shadow_mask(log_y);
            y_new[i] = (log_y + amount * mask).exp2() * MIDDLE_GRAY;
        }
        rescale_rec2020_to_luma(image, &y_old, &y_new);
    }
}

/// Black-point shift. Acts on the deepest part of the image (near-black mask).
/// `value > 0` lifts blacks (filmy), `value < 0` deepens.
#[derive(Debug, Clone, Copy)]
pub struct Blacks {
    pub value: f32,
}

impl Default for Blacks {
    fn default() -> Self {
        Self { value: 0.0 }
    }
}

impl Control for Blacks {
    type Space = LinearRec2020;
    fn apply(&self, image: &mut Buffer<LinearRec2020>) {
        if self.value == 0.0 {
            return;
        }
        let inv_mid = 1.0 / MIDDLE_GRAY;
        let y_old = luminance_rec2020(image);
        let mut y_new = y_old.clone();
        for i in 0..y_old.len() {
            if y_old[i] <= 0.0 {
                continue;
            }
            let log_y = (y_old[i] * inv_mid).log2();
            let mask = near_black_mask(log_y);
            y_new[i] = (log_y + self.value * mask).exp2() * MIDDLE_GRAY;
        }
        rescale_rec2020_to_luma(image, &y_old, &y_new);
    }
}

/// Soft-Highlights tone stage: shoulder compression on the highlight region.
/// `value` in [0, 1]. Larger = more roll-off.
#[derive(Debug, Clone, Copy)]
pub struct SoftHighlightsTone {
    pub value: f32,
}

impl Default for SoftHighlightsTone {
    fn default() -> Self {
        Self { value: 0.0 }
    }
}

impl Control for SoftHighlightsTone {
    type Space = LinearRec2020;
    fn apply(&self, image: &mut Buffer<LinearRec2020>) {
        let strength = self.value.clamp(0.0, 1.0);
        if strength == 0.0 {
            return;
        }
        let inv_mid = 1.0 / MIDDLE_GRAY;
        let y_old = luminance_rec2020(image);
        let mut y_new = y_old.clone();
        for i in 0..y_old.len() {
            if y_old[i] <= 0.0 {
                continue;
            }
            let log_y = (y_old[i] * inv_mid).log2();
            let mask = highlight_mask(log_y);
            // Compression: subtract a fraction of the EV-above-mid-gray.
            let compression = strength * mask * log_y.max(0.0) * 0.5;
            y_new[i] = (log_y - compression).exp2() * MIDDLE_GRAY;
        }
        rescale_rec2020_to_luma(image, &y_old, &y_new);
    }
}

/// Soft-Highlights chroma stage: highlight desaturation in OKLCh. `value` in
/// [0, 1]. Larger = more desaturation as L approaches 1.
#[derive(Debug, Clone, Copy)]
pub struct SoftHighlightsChroma {
    pub value: f32,
}

impl Default for SoftHighlightsChroma {
    fn default() -> Self {
        Self { value: 0.0 }
    }
}

impl Control for SoftHighlightsChroma {
    type Space = Oklch;
    fn apply(&self, image: &mut Buffer<Oklch>) {
        let strength = self.value.clamp(0.0, 1.0);
        if strength == 0.0 {
            return;
        }
        let plane = image.plane_size();
        let (l, ch) = image.data_mut().split_at_mut(plane);
        let (c, _h) = ch.split_at_mut(plane);
        for i in 0..plane {
            // Mask: rises with L > 0.6, peaks at L = 1.
            let mask = ((l[i] - 0.6) / 0.4).clamp(0.0, 1.0);
            let factor = 1.0 - strength * mask * 0.5;
            c[i] *= factor;
        }
    }
}

/// User-facing Soft-Highlights knob. Expands to two pipeline stages
/// (`tone` in linear Rec.2020, `chroma` in OKLCh).
#[derive(Debug, Clone, Copy)]
pub struct SoftHighlights {
    pub value: f32,
}

impl Default for SoftHighlights {
    fn default() -> Self {
        Self { value: 0.0 }
    }
}

impl SoftHighlights {
    pub fn tone(&self) -> SoftHighlightsTone {
        SoftHighlightsTone { value: self.value }
    }
    pub fn chroma(&self) -> SoftHighlightsChroma {
        SoftHighlightsChroma { value: self.value }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_zero_is_noop() {
        let mut buf: Buffer<LinearRec2020> =
            Buffer::from_planar((0..24).map(|i| (i as f32) / 24.0).collect(), 4, 2);
        let original = buf.data().to_vec();
        Contrast { value: 0.0 }.apply(&mut buf);
        for (a, b) in original.iter().zip(buf.data().iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn contrast_positive_steepens_around_mid_gray() {
        // Pixel above mid-gray should brighten with positive contrast; below should darken.
        let above = MIDDLE_GRAY * 2.0;
        let below = MIDDLE_GRAY * 0.5;
        let data = vec![above, below, above, below, above, below];
        let mut buf: Buffer<LinearRec2020> = Buffer::from_planar(data.clone(), 2, 1);
        Contrast { value: 0.5 }.apply(&mut buf);
        // Above-mid pixel got brighter.
        assert!(buf.r()[0] > above);
        // Below-mid pixel got darker.
        assert!(buf.r()[1] < below);
    }

    #[test]
    fn shadows_lift_brightens_low_luma() {
        let dark = MIDDLE_GRAY * 0.1;
        let mut buf: Buffer<LinearRec2020> = Buffer::from_planar(vec![dark; 24], 4, 2);
        Shadows {
            value: 1.0,
            iso: 100.0,
        }
        .apply(&mut buf);
        for &v in buf.data() {
            assert!(v > dark, "expected lift, got {v}");
        }
    }

    #[test]
    fn shadows_high_iso_caps_lift() {
        let dark = MIDDLE_GRAY * 0.1;
        let mut buf_low: Buffer<LinearRec2020> = Buffer::from_planar(vec![dark; 24], 4, 2);
        let mut buf_high: Buffer<LinearRec2020> = Buffer::from_planar(vec![dark; 24], 4, 2);
        Shadows {
            value: 1.0,
            iso: 100.0,
        }
        .apply(&mut buf_low);
        Shadows {
            value: 1.0,
            iso: 12800.0,
        }
        .apply(&mut buf_high);
        let low_avg: f32 = buf_low.data().iter().sum::<f32>() / buf_low.data().len() as f32;
        let high_avg: f32 = buf_high.data().iter().sum::<f32>() / buf_high.data().len() as f32;
        assert!(
            low_avg > high_avg,
            "ISO should cap lift: low {low_avg} > high {high_avg}"
        );
    }

    #[test]
    fn blacks_lift_brightens_near_black() {
        let near_black = MIDDLE_GRAY * 0.01;
        let mut buf: Buffer<LinearRec2020> = Buffer::from_planar(vec![near_black; 24], 4, 2);
        Blacks { value: 1.0 }.apply(&mut buf);
        for &v in buf.data() {
            assert!(v > near_black);
        }
    }

    #[test]
    fn soft_highlights_tone_compresses_highlights() {
        let bright = MIDDLE_GRAY * 8.0; // ~+3 EV
        let mut buf: Buffer<LinearRec2020> = Buffer::from_planar(vec![bright; 24], 4, 2);
        SoftHighlightsTone { value: 1.0 }.apply(&mut buf);
        for &v in buf.data() {
            assert!(v < bright, "expected compression, got {v}");
        }
    }

    #[test]
    fn soft_highlights_chroma_desaturates_high_l() {
        // L=0.95 (highlight), C=0.2.
        let data = vec![0.95, 0.95, 0.2, 0.2, 0.0, 0.0]; // 2x1 planar L,C,h
        let mut buf: Buffer<Oklch> = Buffer::from_planar(data, 2, 1);
        SoftHighlightsChroma { value: 1.0 }.apply(&mut buf);
        for &c in buf.g() {
            assert!(c < 0.2, "chroma should desaturate, got {c}");
        }
    }
}
