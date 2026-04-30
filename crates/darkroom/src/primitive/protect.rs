//! Skin and specular protection guards.
//!
//! These return an attenuation in [0, 1] applied to a control's strength when
//! the pixel falls into a "protected" region — preventing e.g. Color from
//! over-saturating skin tones, or Soft-Highlights from killing specular
//! highlights.
//!
//! MVP versions are heuristic and conservative; refined later with real
//! perceptual data.

/// Skin protection in OKLCh.
///
/// Returns 1.0 (full effect) outside the skin hue band, fading toward a
/// minimum (~0.4 — keep some effect, just less) inside it. Skin hue in OKLab
/// sits roughly around `h ≈ 0.6 rad` (≈ 35°) for warm tones; the band widens
/// with chroma.
pub fn skin_protection(_l: f32, c: f32, h: f32) -> f32 {
    // Hue distance from the skin reference, accounting for circular hue.
    let skin_h = 0.6_f32;
    let mut dh = (h - skin_h).abs();
    if dh > std::f32::consts::PI {
        dh = 2.0 * std::f32::consts::PI - dh;
    }
    // Wider band when chroma is moderate (skin is rarely highly saturated).
    let band = 0.5 + (0.3 - c).max(0.0) * 0.5;
    let in_band = (1.0 - (dh / band).min(1.0)).max(0.0);
    let min_effect = 0.4;
    1.0 - in_band * (1.0 - min_effect)
}

/// Specular protection in OKLab.
///
/// Specular highlights are very bright + low chroma. Protect them from
/// aggressive highlight desaturation so they don't go gray.
pub fn specular_protection(l: f32, c: f32) -> f32 {
    // Bright + low-chroma = specular signature.
    let bright = ((l - 0.85) / 0.15).clamp(0.0, 1.0);
    let neutral = ((0.05 - c) / 0.05).clamp(0.0, 1.0);
    let specular = bright * neutral;
    let min_effect = 0.3;
    1.0 - specular * (1.0 - min_effect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skin_protection_attenuates_in_skin_hue_band() {
        let outside = skin_protection(0.6, 0.1, 3.0); // far hue
        let inside = skin_protection(0.6, 0.1, 0.6); // skin hue
        assert!(outside > inside);
        assert!(outside <= 1.0);
        assert!(inside >= 0.0);
    }

    #[test]
    fn specular_protection_attenuates_at_bright_low_chroma() {
        let normal = specular_protection(0.5, 0.2);
        let specular = specular_protection(0.95, 0.01);
        assert!(normal > specular);
        assert!((normal - 1.0).abs() < 1e-4); // no protection in normal range
    }
}
