//! Shared building blocks reused across controls.
//!
//! - [`luminance`] — Y from RGB, plus the `Y'/Y` rescale used by tone controls
//!   to apply a curve in luminance domain while preserving hue.
//! - [`curve`] — `ToneCurve` parametric S-curve used by Contrast, Shadows,
//!   Blacks, Soft-Highlights tone.
//! - [`mask`] — smoothstep masks over log-luminance regions (highlight,
//!   shadow, midtone, near-black).
//! - [`protect`] — skin and specular guards used by Warmth, Color,
//!   Soft-Highlights chroma. Stub impls in MVP; refined later.
//! - [`blur`] — separable Gaussian on a 1-channel f32 plane. Used by Clarity.
//! - [`noise`] — deterministic per-pixel value noise. Used by Grain.

pub mod blur;
pub mod curve;
pub mod luminance;
pub mod mask;
pub mod noise;
pub mod protect;
