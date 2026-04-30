//! Develop pipeline orchestrator.
//!
//! `DevelopParams` carries the user-facing knob values. The pipeline
//! materializes per-image controls (filling in ISO, black/white level, etc.
//! from `Raw.shot_info` / `Raw.sensor_info`) and walks them in the order:
//!
//! ```text
//! camera-linear (libraw)
//!   Temperature, Tint                          [CameraLinear]
//! T1 → linear Rec.2020 (WB + cam_to_rec2020)
//!   Exposure                                   [LinearRec2020]
//!   Look apply (with strength via in-linear blend)
//!   Contrast, SoftHighlightsTone, Shadows, Blacks  [LinearRec2020]
//! T2 → linear sRGB primaries
//! T4 → Oklab
//!   Warmth                                     [Oklab]
//! T6 → Oklch
//!   SoftHighlightsChroma, Color, Clarity        [Oklch]
//! T7 → Oklab → T5 → linear sRGB
//!   Grain                                       [LinearSrgb]
//! T10 → sRGB 8-bit
//! ```
//!
//! Image-format input (JPEG/PNG/TIFF): decode → resize → return. No knobs in
//! MVP — the develop pipeline is RAW-only (per the design doc, Temperature/
//! Tint require a sensor-native opinion that display-referred sRGB doesn't
//! have).

use std::sync::atomic::AtomicBool;

use codec::{Image, Loaded, Raw, ShotInfo, TargetSize, read_camera_linear, read_image_pixels};

use crate::common::{DevelopError, check_cancel, fit_within, resize_f32_planar, resize_u8x3};
use crate::control::Control;
use crate::control::color::{Color, Warmth};
use crate::control::detail::{Clarity, Grain};
use crate::control::input::{Exposure, Temperature, Tint};
use crate::control::look::lookup;
use crate::control::tone::{Blacks, Contrast, Shadows, SoftHighlights};
use crate::space::{Buffer, CameraLinear, LinearRec2020, Srgb8};
use crate::transform::camera::CameraToWorking;
use crate::transform::encode::SrgbEncode;
use crate::transform::matrix::Rec2020ToSrgb;
use crate::transform::oklab::{LinearToOklab, OklabToLinear, OklabToOklch, OklchToOklab};
use crate::transform::{InPlaceTransform, Transform};

/// User-facing develop parameters. All values are in the internal-normalized
/// form (-1..=1 or 0..=1) except where physical units make sense (EV, Kelvin).
/// Default is identity / no-op.
#[derive(Debug, Clone)]
pub struct DevelopParams {
    pub exposure_ev: f32,
    pub temperature_kelvin: f32,
    pub tint: f32,
    /// Look id resolved at apply time via [`crate::control::look::lookup`];
    /// unknown ids fall back to identity.
    pub look: String,
    pub look_strength: f32,
    pub warmth: f32,
    pub color: f32,
    pub contrast: f32,
    pub soft_highlights: f32,
    pub shadows: f32,
    pub blacks: f32,
    pub clarity: f32,
    pub grain: f32,
    /// Seed for deterministic grain noise — same params + same seed = same output.
    pub grain_seed: u64,
}

impl Default for DevelopParams {
    fn default() -> Self {
        Self {
            exposure_ev: 0.0,
            temperature_kelvin: 5500.0,
            tint: 0.0,
            look: "identity".to_string(),
            look_strength: 1.0,
            warmth: 0.0,
            color: 0.0,
            contrast: 0.0,
            soft_highlights: 0.0,
            shadows: 0.0,
            blacks: 0.0,
            clarity: 0.0,
            grain: 0.0,
            grain_seed: 0,
        }
    }
}

impl DevelopParams {
    /// Stable 64-bit fingerprint of all knob values. Used by the TUI cache to
    /// invalidate a preview entry when the user adjusts a knob.
    pub fn fingerprint(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        for v in [
            self.exposure_ev,
            self.temperature_kelvin,
            self.tint,
            self.look_strength,
            self.warmth,
            self.color,
            self.contrast,
            self.soft_highlights,
            self.shadows,
            self.blacks,
            self.clarity,
            self.grain,
        ] {
            v.to_bits().hash(&mut h);
        }
        self.grain_seed.hash(&mut h);
        self.look.hash(&mut h);
        h.finish()
    }
}

/// Develop a `Loaded` to an 8-bit sRGB preview at or below `target` with
/// preview-quality settings (libraw `half_size = true` for RAW).
pub fn develop_preview(
    loaded: &Loaded,
    params: &DevelopParams,
    target: TargetSize,
    cancel: Option<&AtomicBool>,
) -> Result<Srgb8, DevelopError> {
    match loaded {
        Loaded::Raw(raw) => {
            develop_raw_full(raw, params, target, /* half_size */ true, cancel)
        }
        Loaded::Image(img) => develop_image_passthrough(img, target, cancel),
    }
}

/// Develop a `Loaded` to an 8-bit sRGB output at full quality (libraw
/// `half_size = false`). `target = None` develops at source resolution; some
/// `Some(t)` develops at-or-below `t`.
pub fn develop_full(
    loaded: &Loaded,
    params: &DevelopParams,
    target: Option<TargetSize>,
    cancel: Option<&AtomicBool>,
) -> Result<Srgb8, DevelopError> {
    let effective_target = target.unwrap_or_else(|| {
        let (w, h) = loaded.dimensions();
        TargetSize::new(w.max(1), h.max(1))
    });
    match loaded {
        Loaded::Raw(raw) => {
            develop_raw_full(
                raw,
                params,
                effective_target,
                /* half_size */ false,
                cancel,
            )
        }
        Loaded::Image(img) => develop_image_passthrough(img, effective_target, cancel),
    }
}

fn develop_raw_full(
    raw: &Raw,
    params: &DevelopParams,
    target: TargetSize,
    half_size: bool,
    cancel: Option<&AtomicBool>,
) -> Result<Srgb8, DevelopError> {
    // 1. Read camera-linear (no WB, no matrix).
    let pixels = read_camera_linear(raw, half_size, cancel).map_err(DevelopError::Decode)?;
    check_cancel(cancel)?;
    let buf = Buffer::<CameraLinear>::from_planar(pixels.data, pixels.width, pixels.height);

    // 2. Resize to target — transforms run on the smaller buffer.
    let (dst_w, dst_h) = fit_within(buf.width(), buf.height(), target);
    let mut buf = resize_f32_planar(buf, dst_w, dst_h)?;
    check_cancel(cancel)?;

    // 3. Input controls in camera-linear.
    Temperature {
        kelvin: params.temperature_kelvin,
    }
    .apply(&mut buf);
    Tint { value: params.tint }.apply(&mut buf);

    // 4. Camera → working linear Rec.2020.
    let xform = CameraToWorking::from_sensor(&raw.sensor_info);
    let mut buf = xform.apply(buf);
    check_cancel(cancel)?;

    // 5. Exposure.
    Exposure {
        ev: params.exposure_ev,
    }
    .apply(&mut buf);

    // 6. Look + LookStrength via in-linear blend.
    apply_look_with_strength(&params.look, params.look_strength, &mut buf);

    // 7. Tone fine-tune (hue-preserving, in linear Rec.2020).
    let iso = effective_iso(&raw.shot_info);
    Contrast {
        value: params.contrast,
    }
    .apply(&mut buf);
    SoftHighlights {
        value: params.soft_highlights,
    }
    .tone()
    .apply(&mut buf);
    Shadows {
        value: params.shadows,
        iso,
    }
    .apply(&mut buf);
    Blacks {
        value: params.blacks,
    }
    .apply(&mut buf);
    check_cancel(cancel)?;

    // 8. Color stage in OKLab / OKLCh.
    let lin_srgb = Rec2020ToSrgb.apply(buf);
    let mut oklab = LinearToOklab.apply(lin_srgb);
    Warmth {
        value: params.warmth,
    }
    .apply(&mut oklab);
    let mut oklch = OklabToOklch.apply(oklab);
    SoftHighlights {
        value: params.soft_highlights,
    }
    .chroma()
    .apply(&mut oklch);
    Color {
        value: params.color,
        iso,
    }
    .apply(&mut oklch);
    Clarity {
        value: params.clarity,
        iso,
    }
    .apply(&mut oklch);
    check_cancel(cancel)?;

    // 9. Back to linear sRGB for grain + encode.
    let oklab = OklchToOklab.apply(oklch);
    let mut lin_srgb = OklabToLinear.apply(oklab);
    Grain {
        amount: params.grain,
        iso,
        seed: params.grain_seed,
    }
    .apply(&mut lin_srgb);

    // 10. Encode.
    Ok(SrgbEncode.apply(lin_srgb))
}

fn develop_image_passthrough(
    img: &Image,
    target: TargetSize,
    cancel: Option<&AtomicBool>,
) -> Result<Srgb8, DevelopError> {
    let pixels = read_image_pixels(img, target, cancel).map_err(DevelopError::Decode)?;
    check_cancel(cancel)?;

    let (dst_w, dst_h) = fit_within(pixels.width, pixels.height, target);
    if (dst_w, dst_h) == (pixels.width, pixels.height) {
        return Ok(Srgb8 {
            width: pixels.width,
            height: pixels.height,
            pixels: pixels.data,
        });
    }
    let resized = resize_u8x3(&pixels.data, pixels.width, pixels.height, dst_w, dst_h)?;
    Ok(Srgb8 {
        width: dst_w,
        height: dst_h,
        pixels: resized,
    })
}

/// Apply the named look at the given strength via in-linear lerp. For simple
/// gain-only looks this is mathematically equivalent to OKLab blending; for
/// future complex looks (hue warps, chroma compression) we'll switch to the
/// proper OKLab two-buffer blend via `LookStrength: Blend`.
fn apply_look_with_strength(id: &str, strength: f32, image: &mut Buffer<LinearRec2020>) {
    let strength = strength.clamp(0.0, 1.0);
    if strength <= 0.0 {
        return;
    }
    let look = lookup(id);
    if strength >= 1.0 {
        look.apply(image);
        return;
    }
    let neutral = image.clone();
    look.apply(image);
    let inv = 1.0 - strength;
    let dst = image.data_mut();
    let neu = neutral.data();
    for i in 0..dst.len() {
        dst[i] = inv * neu[i] + strength * dst[i];
    }
}

fn effective_iso(shot: &ShotInfo) -> f32 {
    shot.iso.unwrap_or(100.0).max(50.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codec::{ImageKind, decode_image};
    use image::{ImageBuffer, ImageFormat, Rgb};
    use std::io::Cursor;
    use std::path::Path;

    fn write_jpeg(path: &Path, w: u32, h: u32, color: [u8; 3]) {
        let buf = ImageBuffer::<Rgb<u8>, _>::from_pixel(w, h, Rgb(color));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(buf)
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Jpeg)
            .unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn develop_preview_jpeg_fits_within_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("a.jpg");
        write_jpeg(&path, 200, 150, [120, 80, 200]);

        let img = decode_image(&path).unwrap();
        assert_eq!(img.kind, ImageKind::Jpeg);
        let loaded = Loaded::Image(img);
        let out = develop_preview(
            &loaded,
            &DevelopParams::default(),
            TargetSize::new(50, 50),
            None,
        )
        .unwrap();
        assert!(out.width <= 50 && out.height <= 50);
        assert_eq!(out.pixels.len() as u32, out.width * out.height * 3);
    }

    #[test]
    fn develop_preview_does_not_upscale_image_when_target_is_larger() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("tiny.png");
        let buf = ImageBuffer::<Rgb<u8>, _>::from_pixel(4, 4, Rgb([200, 200, 200]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(buf)
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        std::fs::write(&path, bytes).unwrap();

        let img = decode_image(&path).unwrap();
        let loaded = Loaded::Image(img);
        let out = develop_preview(
            &loaded,
            &DevelopParams::default(),
            TargetSize::new(64, 64),
            None,
        )
        .unwrap();
        assert_eq!((out.width, out.height), (4, 4));
    }

    #[test]
    fn develop_params_default_is_identity_safe() {
        // The default params should be a no-op set of knobs (all zeros, sensible
        // baseline kelvin). Just check that `Default` returns reasonable values.
        let p = DevelopParams::default();
        assert_eq!(p.exposure_ev, 0.0);
        assert_eq!(p.temperature_kelvin, 5500.0);
        assert_eq!(p.look, "identity");
        assert_eq!(p.look_strength, 1.0);
        assert_eq!(p.color, 0.0);
        assert_eq!(p.grain, 0.0);
    }

    #[test]
    fn apply_look_with_strength_zero_is_noop() {
        let mut buf: Buffer<LinearRec2020> = Buffer::from_planar(vec![0.5_f32; 24], 4, 2);
        let original = buf.data().to_vec();
        apply_look_with_strength("warm-muted-soft", 0.0, &mut buf);
        assert_eq!(buf.data(), original.as_slice());
    }

    #[test]
    fn apply_look_with_strength_full_applies_look() {
        let mut buf: Buffer<LinearRec2020> = Buffer::from_planar(vec![0.5_f32; 24], 4, 2);
        apply_look_with_strength("warm-muted-soft", 1.0, &mut buf);
        for &v in buf.r() {
            assert!(v > 0.5, "R should warm at full strength");
        }
        for &v in buf.b() {
            assert!(v < 0.5, "B should cool at full strength");
        }
    }

    #[test]
    fn apply_look_with_strength_half_lerps() {
        let mut buf_full: Buffer<LinearRec2020> = Buffer::from_planar(vec![0.5_f32; 24], 4, 2);
        let mut buf_half: Buffer<LinearRec2020> = Buffer::from_planar(vec![0.5_f32; 24], 4, 2);
        apply_look_with_strength("warm-muted-soft", 1.0, &mut buf_full);
        apply_look_with_strength("warm-muted-soft", 0.5, &mut buf_half);
        // Half should be between neutral (0.5) and full.
        for (&full, &half) in buf_full.r().iter().zip(buf_half.r().iter()) {
            assert!(half < full, "half R {half} should be < full R {full}");
            assert!(half > 0.5, "half R {half} should be > neutral 0.5");
        }
    }
}
