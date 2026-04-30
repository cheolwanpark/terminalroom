//! Parametric tone curve over log-luminance.
//!
//! Used by Contrast, Shadows, Blacks, and Soft-Highlights tone. The curve is
//! evaluated in log2 luminance space (natural for stops of light) and the
//! output is also log2 luminance — the caller exponentiates and applies the
//! `Y'/Y` rescale via [`primitive::luminance::rescale_*_to_luma`].

/// Middle gray reference: linear 0.18 ≈ -2.47 EV. Anchors log2 luma.
pub const MIDDLE_GRAY: f32 = 0.18;

/// Parametric S-curve over log2 luminance.
///
/// Definition:
///
/// ```text
/// x = log2(Y / MIDDLE_GRAY)         // input EV around mid-gray
/// y = pivot + slope * (x - pivot)   // base linear region
/// + toe contribution at x << pivot
/// + shoulder contribution at x >> pivot
/// ```
///
/// `slope` controls midtone contrast. `toe` shapes the dark roll-off
/// (negative values lift shadows, positive deepens). `shoulder` shapes the
/// highlight roll-off (higher values compress highlights).
#[derive(Debug, Clone, Copy)]
pub struct ToneCurve {
    /// Pivot point in log2 EV space. Usually 0.0 (anchored at middle gray).
    pub pivot: f32,
    /// Slope at the pivot. 1.0 = identity, > 1 = more contrast.
    pub slope: f32,
    /// Toe (shadow) shaping. Range typically [-1.0, 1.0].
    pub toe: f32,
    /// Shoulder (highlight) shaping. Range typically [0.0, 1.0]. Higher =
    /// more compression near top.
    pub shoulder: f32,
}

impl ToneCurve {
    pub fn identity() -> Self {
        Self {
            pivot: 0.0,
            slope: 1.0,
            toe: 0.0,
            shoulder: 0.0,
        }
    }

    /// Evaluate at an input log2-EV value `x`.
    pub fn eval(&self, x: f32) -> f32 {
        let dx = x - self.pivot;
        // Linear core.
        let mut y = self.pivot + self.slope * dx;

        // Toe: lifts/deepens shadows. Acts in the dx < 0 region.
        if dx < 0.0 {
            // tanh-like soft mix: blend linear region with a flatter line
            // controlled by `toe`. For toe > 0 we deepen (reduce slope); for
            // toe < 0 we lift (increase slope).
            let weight = (-dx).min(3.0) / 3.0; // 0..1 over 3 EV of shadow
            y -= self.toe * weight * dx;
        }

        // Shoulder: rolls off highlights. Acts in the dx > 0 region.
        if dx > 0.0 && self.shoulder > 0.0 {
            let weight = dx.min(3.0) / 3.0; // 0..1 over 3 EV of highlight
            // Compression: y -= shoulder * weight * dx (subtracts EV from result).
            y -= self.shoulder * weight * dx;
        }

        y
    }
}

/// Apply a tone curve to a luminance buffer. Both `y_in` and `y_out` are
/// linear luminance (not log); the curve operates on log2(Y / MIDDLE_GRAY).
/// `y_out.len() == y_in.len()` is required and overwritten.
pub fn apply_curve_to_luma(curve: &ToneCurve, y_in: &[f32], y_out: &mut [f32]) {
    debug_assert_eq!(y_in.len(), y_out.len());
    let inv_mid = 1.0 / MIDDLE_GRAY;
    for (i, &y) in y_in.iter().enumerate() {
        if y <= 0.0 {
            y_out[i] = 0.0;
            continue;
        }
        let x = (y * inv_mid).log2();
        let xp = curve.eval(x);
        y_out[i] = (xp.exp2()) * MIDDLE_GRAY;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_curve_preserves_values() {
        let c = ToneCurve::identity();
        for &x in &[-3.0, -1.0, 0.0, 1.0, 3.0] {
            assert!((c.eval(x) - x).abs() < 1e-6, "identity drift at {x}");
        }
    }

    #[test]
    fn slope_two_doubles_offset() {
        let c = ToneCurve {
            pivot: 0.0,
            slope: 2.0,
            toe: 0.0,
            shoulder: 0.0,
        };
        // No toe/shoulder triggers in the +1 EV region for shoulder=0.
        assert!((c.eval(1.0) - 2.0).abs() < 1e-6);
        // Toe=0, so dx=-1 is just linear-doubled = -2.
        assert!((c.eval(-1.0) - (-2.0)).abs() < 1e-6);
    }

    #[test]
    fn shoulder_compresses_highlights() {
        let plain = ToneCurve {
            pivot: 0.0,
            slope: 1.0,
            toe: 0.0,
            shoulder: 0.0,
        };
        let rolled = ToneCurve {
            pivot: 0.0,
            slope: 1.0,
            toe: 0.0,
            shoulder: 0.5,
        };
        // At +2 EV, shoulder should pull the result below the linear identity.
        assert!(rolled.eval(2.0) < plain.eval(2.0));
    }

    #[test]
    fn apply_curve_to_luma_zero_stays_zero() {
        let c = ToneCurve {
            pivot: 0.0,
            slope: 2.0,
            toe: 0.0,
            shoulder: 0.0,
        };
        let mut out = vec![0.0; 4];
        apply_curve_to_luma(&c, &[0.0, 0.0, 0.0, 0.0], &mut out);
        assert!(out.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn apply_curve_to_luma_middle_gray_is_anchor() {
        let c = ToneCurve {
            pivot: 0.0,
            slope: 2.0,
            toe: 0.0,
            shoulder: 0.0,
        };
        // log2(0.18 / 0.18) = 0; eval(0) = 0; back to 2^0 * 0.18 = 0.18.
        let mut out = vec![0.0; 1];
        apply_curve_to_luma(&c, &[MIDDLE_GRAY], &mut out);
        assert!((out[0] - MIDDLE_GRAY).abs() < 1e-6);
    }
}
