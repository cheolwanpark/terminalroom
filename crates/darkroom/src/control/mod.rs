//! Knob-driven ops in a fixed color space.
//!
//! Each `Control` impl is tied to one space via its associated type, so
//! applying a `Control<Space=Oklab>` to a `Buffer<LinearRec2020>` is a compile
//! error. `Blend` is the two-buffer variant used for `LookStrength` (mix
//! neutral + looked).
//!
//! The pipeline does not store `Vec<Box<dyn Control>>`. Runtime ordering is
//! data: a `Vec<Op>` enumerates the ops in order, and the executor dispatches
//! each variant to its correct space. The trait impls land in Phase E.

use crate::space::{Buffer, ColorSpace};

pub mod color;
pub mod detail;
pub mod input;
pub mod look;
pub mod tone;

pub trait Control {
    type Space: ColorSpace;
    fn apply(&self, image: &mut Buffer<Self::Space>);
}

pub trait Blend {
    type Space: ColorSpace;
    fn apply(&self, base: &Buffer<Self::Space>, target: &mut Buffer<Self::Space>);
}

/// Closed enum of all knob ops. `pipeline::develop_inner` (Phase F) walks an
/// ordered `Vec<Op>` and dispatches each variant. Each variant's value is the
/// internal-normalized form (-1..=1 or 0..=1) except where physical units make
/// sense (EV, Kelvin).
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Exposure(f32),
    Temperature { kelvin: f32 },
    Tint(f32),
    LookStrength(f32),
    Warmth(f32),
    Color { value: f32, iso: f32 },
    Contrast(f32),
    SoftHighlightsTone(f32),
    SoftHighlightsChroma(f32),
    Shadows { value: f32, iso: f32 },
    Blacks(f32),
    Clarity { value: f32, iso: f32 },
    Grain { amount: f32, iso: f32, seed: u64 },
}
