//! Stateless A → B color/representation conversions.
//!
//! `Transform` is the general form: consume `Buffer<Input>`, produce
//! `Buffer<Output>`. Allocation may happen.
//!
//! `InPlaceTransform` is the same-layout reinterpretation case used when the
//! pixel math can be done in place (matrix multiplies, OKLab ↔ OKLCh): the
//! `Vec<f32>` is reused; only the phantom type changes.
//!
//! Concrete impls land in later phases (`matrix`, `encode`, `oklab`, `camera`).

use crate::space::{Buffer, ColorSpace};

/// General A → B conversion. Inputs and outputs are not constrained to
/// `Buffer<S>` because some transforms cross between planar f32 buffers and
/// non-buffer representations (`Srgb8`, file decoders, etc).
pub trait Transform {
    type Input;
    type Output;
    fn apply(&self, src: Self::Input) -> Self::Output;
}

/// Same-layout reinterpretation between two `Buffer<S>` types. Reuses the
/// underlying `Vec<f32>` after performing per-pixel math in place. Used by
/// matrix multiplies (Rec.2020 ↔ sRGB primaries) and OKLab ↔ OKLCh.
pub trait InPlaceTransform {
    type In: ColorSpace;
    type Out: ColorSpace;
    fn apply(&self, buf: Buffer<Self::In>) -> Buffer<Self::Out>;
}

pub mod camera;
pub mod encode;
pub mod matrix;
pub mod oklab;
