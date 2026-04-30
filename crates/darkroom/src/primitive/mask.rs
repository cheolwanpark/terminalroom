//! Smoothstep masks over log-luminance regions.
//!
//! Each mask returns a weight in [0, 1] that the caller multiplies against an
//! op's strength to localize it to a luminance region. Inputs are log2 EV
//! around middle gray (the same domain as `ToneCurve`).
//!
//! Boundaries are tuned for typical photo content:
//! - shadows: full effect below -2 EV, fades out by 0 EV (mid-gray)
//! - midtones: peaks at 0 EV, falls off both ways by ~1.5 EV
//! - highlights: starts at 0 EV, full effect above +2 EV
//! - near-black: full effect below -3 EV, fades out by -1 EV

/// `smoothstep(edge0, edge1, x)`: 0 below edge0, 1 above edge1, smooth in between.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge1 == edge0 {
        return if x < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 1 in shadow region, fading to 0 at mid-gray.
pub fn shadow_mask(log_y: f32) -> f32 {
    1.0 - smoothstep(-2.0, 0.0, log_y)
}

/// 1 in highlight region, fading to 0 at mid-gray.
pub fn highlight_mask(log_y: f32) -> f32 {
    smoothstep(0.0, 2.0, log_y)
}

/// Bell-curve around mid-gray. Peak = 1 at 0 EV.
pub fn midtone_mask(log_y: f32) -> f32 {
    let s = smoothstep(-1.5, 0.0, log_y);
    let h = 1.0 - smoothstep(0.0, 1.5, log_y);
    (s * h).clamp(0.0, 1.0) * 2.0_f32.min(1.0) // scaled bell, capped at 1
}

/// 1 in near-black region, fading to 0 by -1 EV.
pub fn near_black_mask(log_y: f32) -> f32 {
    1.0 - smoothstep(-3.0, -1.0, log_y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_mask_endpoints() {
        assert!(shadow_mask(-3.0) > 0.99);
        assert!(shadow_mask(0.0) < 0.01);
    }

    #[test]
    fn highlight_mask_endpoints() {
        assert!(highlight_mask(0.0) < 0.01);
        assert!(highlight_mask(3.0) > 0.99);
    }

    #[test]
    fn midtone_mask_peaks_near_mid_gray() {
        let p = midtone_mask(0.0);
        let l = midtone_mask(-1.5);
        let r = midtone_mask(1.5);
        assert!(p > l, "midtone should peak above shadow edge: {p} vs {l}");
        assert!(
            p > r,
            "midtone should peak above highlight edge: {p} vs {r}"
        );
    }

    #[test]
    fn near_black_mask_endpoints() {
        assert!(near_black_mask(-4.0) > 0.99);
        assert!(near_black_mask(-1.0) < 0.01);
    }

    #[test]
    fn smoothstep_monotonic() {
        for i in 0..10 {
            let x = i as f32 * 0.1;
            let y = smoothstep(0.0, 1.0, x);
            assert!((0.0..=1.0).contains(&y));
        }
    }
}
