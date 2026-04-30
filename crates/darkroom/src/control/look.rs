//! Looks: deterministic transform chains over the working buffer.
//!
//! A `Look` operates on `Buffer<LinearRec2020>` (post-Exposure, pre-tone). The
//! pipeline runs the look on the working buffer, branches to keep the neutral
//! version, converts both to OKLab, and blends via `LookStrength`.

use wide::f32x8;

use crate::control::Blend;
use crate::simd::map_f32x8;
use crate::space::{Buffer, LinearRec2020, Oklab};

/// A look transform. Implementations apply a deterministic operation to the
/// working buffer; the pipeline blends the looked output back toward a neutral
/// reference via `LookStrength` for partial application.
pub trait Look: Send + Sync {
    fn id(&self) -> &'static str;
    fn apply(&self, image: &mut Buffer<LinearRec2020>);
}

/// No-op look. Default; makes `LookStrength` a true no-op when no curated
/// look is selected.
#[derive(Debug, Clone, Copy)]
pub struct Identity;
impl Look for Identity {
    fn id(&self) -> &'static str {
        "identity"
    }
    fn apply(&self, _image: &mut Buffer<LinearRec2020>) {}
}

/// MVP curated look: warm + muted + soft. Small per-channel gain bias.
/// Future versions add OKLab chroma compression and shoulder shaping.
#[derive(Debug, Clone, Copy)]
pub struct WarmMutedSoft;
impl Look for WarmMutedSoft {
    fn id(&self) -> &'static str {
        "warm-muted-soft"
    }
    fn apply(&self, image: &mut Buffer<LinearRec2020>) {
        let warm_r = f32x8::splat(1.05);
        let warm_b = f32x8::splat(0.95);
        let (r, _g, b) = image.rgb_planes_mut();
        map_f32x8(r, |v| v * warm_r);
        map_f32x8(b, |v| v * warm_b);
    }
}

/// Resolve a look id to a concrete `&'static dyn Look`. Returns `Identity` for
/// any unrecognized id (so a sidecar referencing a removed look degrades to
/// the neutral develop).
pub fn lookup(id: &str) -> &'static dyn Look {
    match id {
        "warm-muted-soft" => &WarmMutedSoft,
        _ => &Identity,
    }
}

/// Linear interpolate the looked OKLab buffer back toward the neutral one by
/// `1 - strength`. `strength = 1.0` keeps the look fully; `0.0` is a complete
/// fallback to neutral.
#[derive(Debug, Clone, Copy)]
pub struct LookStrength {
    pub strength: f32,
}

impl Default for LookStrength {
    fn default() -> Self {
        Self { strength: 1.0 }
    }
}

impl Blend for LookStrength {
    type Space = Oklab;
    fn apply(&self, base: &Buffer<Oklab>, target: &mut Buffer<Oklab>) {
        let t = self.strength.clamp(0.0, 1.0);
        if t >= 1.0 {
            return;
        }
        let inv_t = 1.0 - t;
        debug_assert_eq!(base.data().len(), target.data().len());
        let target_data = target.data_mut();
        let base_data = base.data();
        for i in 0..target_data.len() {
            target_data[i] = inv_t * base_data[i] + t * target_data[i];
        }
    }
}

/// `Control` impl that wraps a look, so it can run in the `Op::ApplyLook`
/// dispatch. Not user-facing; the pipeline holds the actual `Look` and
/// invokes its `apply` directly.
pub fn apply_look(id: &str, image: &mut Buffer<LinearRec2020>) {
    lookup(id).apply(image);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_noop() {
        let data: Vec<f32> = (0..24).map(|i| i as f32 * 0.05).collect();
        let mut buf: Buffer<LinearRec2020> = Buffer::from_planar(data.clone(), 4, 2);
        Identity.apply(&mut buf);
        assert_eq!(buf.data(), data.as_slice());
    }

    #[test]
    fn warm_muted_soft_warms_and_cools_channels() {
        let mut buf: Buffer<LinearRec2020> = Buffer::from_planar(vec![0.5_f32; 24], 4, 2);
        WarmMutedSoft.apply(&mut buf);
        for &v in buf.r() {
            assert!(v > 0.5, "R should warm, got {v}");
        }
        for &v in buf.b() {
            assert!(v < 0.5, "B should cool, got {v}");
        }
    }

    #[test]
    fn lookup_falls_back_to_identity_on_unknown() {
        assert_eq!(lookup("nonexistent").id(), "identity");
        assert_eq!(lookup("warm-muted-soft").id(), "warm-muted-soft");
    }

    #[test]
    fn look_strength_one_keeps_target() {
        let base: Buffer<Oklab> = Buffer::from_planar(vec![0.0_f32; 12], 4, 1);
        let mut target: Buffer<Oklab> = Buffer::from_planar(vec![0.5_f32; 12], 4, 1);
        let original = target.data().to_vec();
        LookStrength { strength: 1.0 }.apply(&base, &mut target);
        assert_eq!(target.data(), original.as_slice());
    }

    #[test]
    fn look_strength_zero_falls_back_to_base() {
        let base: Buffer<Oklab> = Buffer::from_planar(vec![0.0_f32; 12], 4, 1);
        let mut target: Buffer<Oklab> = Buffer::from_planar(vec![0.5_f32; 12], 4, 1);
        LookStrength { strength: 0.0 }.apply(&base, &mut target);
        for v in target.data() {
            assert!(v.abs() < 1e-6);
        }
    }

    #[test]
    fn look_strength_half_lerps() {
        let base: Buffer<Oklab> = Buffer::from_planar(vec![0.0_f32; 12], 4, 1);
        let mut target: Buffer<Oklab> = Buffer::from_planar(vec![1.0_f32; 12], 4, 1);
        LookStrength { strength: 0.5 }.apply(&base, &mut target);
        for &v in target.data() {
            assert!((v - 0.5).abs() < 1e-6);
        }
    }
}
