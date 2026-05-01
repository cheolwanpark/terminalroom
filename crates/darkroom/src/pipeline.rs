//! Develop pipeline orchestrator.
//!
//! `DevelopParams` carries the user-facing knob values (5 fields after the
//! 4-knob UI redesign): exposure, WB temperature/tint, look id, look
//! strength. Curated looks (parsed from XMP sidecars) carry everything else
//! that used to be a knob — see `transform/xmp.rs` and `docs/looks.md`.
//!
//! Reduced pipeline order:
//!
//! ```text
//! camera-linear (libraw)
//!   Temperature, Tint                      [CameraLinear]
//! T1 → linear Rec.2020 (WB + cam_to_rec2020)  [Raw path]
//!     or linear sRGB → Rec.2020 retag         [Image path]
//!   Exposure                                [LinearRec2020]
//!   apply_xmp_with_strength(registry, look, strength, ...)   [LinearRec2020]
//! T2 → linear sRGB primaries
//! T10 → sRGB 8-bit
//! ```
//!
//! Image-format input (JPEG/PNG/TIFF): decode → resize → return. No
//! demosaic step; the same control chain applies via the same code path.
//!
//! The OKLab/OKLch round-trip and the eight removed knobs (Warmth, Color,
//! Contrast, SoftHighlights, Shadows, Blacks, Clarity, Grain) are no longer
//! invoked here. Their `Control` impls live on in `crate::control::*` for
//! the upcoming XMP applier to compose; see `transform::xmp::ApplyXmp`.

use std::sync::atomic::AtomicBool;

use codec::{
    Image, Loaded, Raw, SensorInfo, ShotInfo, TargetSize, read_camera_linear, read_image_pixels,
};

use crate::common::{DevelopError, check_cancel, fit_within, resize_f32_planar, resize_u8x3};
use crate::control::Control;
use crate::control::input::{Exposure, Temperature, Tint};
use crate::control::look::{IDENTITY_ID, LookRegistry, ResolvedLook};
use crate::space::{Buffer, CameraLinear, LinearRec2020, LinearSrgb, Srgb8};
use crate::transform::camera::CameraToWorking;
use crate::transform::encode::{SrgbDecode, SrgbEncode};
use crate::transform::matrix::{Rec2020ToSrgb, SrgbToRec2020};
use crate::transform::xmp::ApplyXmp;
use crate::transform::{InPlaceTransform, Transform};

/// User-facing develop parameters. Five fields total: physical inputs (EV,
/// Kelvin, tint) plus look id + strength. Curated XMP looks carry the rest.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DevelopParams {
    pub exposure_ev: f32,
    pub temperature_kelvin: f32,
    pub tint: f32,
    /// Look id resolved at apply time via [`LookRegistry::resolve`]. Either
    /// `"identity"` or `"xmp:<source_fp_hex>"`. Unknown ids fall back to
    /// Identity.
    pub look: String,
    pub look_strength: f32,
}

impl Default for DevelopParams {
    fn default() -> Self {
        Self {
            exposure_ev: 0.0,
            temperature_kelvin: 5500.0,
            tint: 0.0,
            look: IDENTITY_ID.to_string(),
            look_strength: 1.0,
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
        ] {
            v.to_bits().hash(&mut h);
        }
        self.look.hash(&mut h);
        h.finish()
    }
}

/// Param-independent prepared source: decoded + resized to a CameraLinear
/// buffer ready for [`apply_pipeline`]. Cache this in the worker keyed by
/// `(path, source_fp, target_bucket)` so knob ticks skip decode + resize.
#[derive(Debug, Clone)]
pub enum PreparedSource {
    /// RAW path: post-`read_camera_linear`, post-resize. `sensor_info` is
    /// needed by `CameraToWorking`; `shot_info` for `effective_iso`.
    Raw {
        buf: Buffer<CameraLinear>,
        sensor_info: SensorInfo,
        shot_info: ShotInfo,
    },
    /// Image path: post-decode, post-resize, post-`SrgbDecode`, retagged as
    /// `CameraLinear` (zero-cost) so Temperature/Tint can run uniformly.
    Image {
        buf: Buffer<CameraLinear>,
        shot_info: ShotInfo,
    },
}

impl PreparedSource {
    /// Dimensions of the prepared (target-sized) buffer. Useful for logging.
    pub fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Raw { buf, .. } => buf.dimensions(),
            Self::Image { buf, .. } => buf.dimensions(),
        }
    }
}

/// Decode + resize a `Loaded` into a [`PreparedSource`] at preview quality
/// (libraw `half_size = true` for RAW). All work here is param-independent —
/// safe to cache and reuse across knob changes.
pub fn prepare_source(
    loaded: &Loaded,
    target: TargetSize,
    cancel: Option<&AtomicBool>,
) -> Result<PreparedSource, DevelopError> {
    prepare_source_inner(loaded, target, /* half_size */ true, cancel)
}

fn prepare_source_inner(
    loaded: &Loaded,
    target: TargetSize,
    half_size: bool,
    cancel: Option<&AtomicBool>,
) -> Result<PreparedSource, DevelopError> {
    match loaded {
        Loaded::Raw(raw) => prepare_raw(raw, target, half_size, cancel),
        Loaded::Image(img) => prepare_image(img, target, cancel),
    }
}

fn prepare_raw(
    raw: &Raw,
    target: TargetSize,
    half_size: bool,
    cancel: Option<&AtomicBool>,
) -> Result<PreparedSource, DevelopError> {
    let pixels = read_camera_linear(raw, half_size, cancel).map_err(DevelopError::Decode)?;
    check_cancel(cancel)?;
    let buf = Buffer::<CameraLinear>::from_planar(pixels.data, pixels.width, pixels.height);
    let (dst_w, dst_h) = fit_within(buf.width(), buf.height(), target);
    let buf = resize_f32_planar(buf, dst_w, dst_h)?;
    check_cancel(cancel)?;
    Ok(PreparedSource::Raw {
        buf,
        sensor_info: raw.sensor_info.clone(),
        shot_info: raw.shot_info.clone(),
    })
}

fn prepare_image(
    img: &Image,
    target: TargetSize,
    cancel: Option<&AtomicBool>,
) -> Result<PreparedSource, DevelopError> {
    let pixels = read_image_pixels(img, target, cancel).map_err(DevelopError::Decode)?;
    check_cancel(cancel)?;

    let (dst_w, dst_h) = fit_within(pixels.width, pixels.height, target);
    let (srgb_w, srgb_h, srgb_pixels) = if (dst_w, dst_h) == (pixels.width, pixels.height) {
        (pixels.width, pixels.height, pixels.data)
    } else {
        let resized = resize_u8x3(&pixels.data, pixels.width, pixels.height, dst_w, dst_h)?;
        (dst_w, dst_h, resized)
    };
    let srgb8 = Srgb8 {
        width: srgb_w,
        height: srgb_h,
        pixels: srgb_pixels,
    };
    let lin_srgb = SrgbDecode.apply(srgb8);
    check_cancel(cancel)?;
    // Image-format files don't have sensor/WB metadata, so Temperature and
    // Tint are reinterpreted as plain per-channel gain in linear-light sRGB.
    // Mathematically the same operation as the CameraLinear path; we just
    // borrow the type tag (zero-cost retag).
    let buf: Buffer<CameraLinear> = lin_srgb.into_space();
    Ok(PreparedSource::Image {
        buf,
        shot_info: img.shot_info.clone(),
    })
}

/// Apply the param-dependent develop pipeline to a [`PreparedSource`].
/// Clones the cached buffer once at entry; the rest of the pipeline owns its
/// data.
pub fn apply_pipeline(
    src: &PreparedSource,
    params: &DevelopParams,
    registry: &LookRegistry,
    cancel: Option<&AtomicBool>,
) -> Result<Srgb8, DevelopError> {
    match src {
        PreparedSource::Raw {
            buf,
            sensor_info,
            shot_info,
        } => apply_from_camera_linear(
            buf.clone(),
            TailMeta::Raw { sensor_info },
            shot_info,
            params,
            registry,
            cancel,
        ),
        PreparedSource::Image { buf, shot_info } => apply_from_camera_linear(
            buf.clone(),
            TailMeta::Image,
            shot_info,
            params,
            registry,
            cancel,
        ),
    }
}

enum TailMeta<'a> {
    Raw { sensor_info: &'a SensorInfo },
    Image,
}

fn apply_from_camera_linear(
    mut buf: Buffer<CameraLinear>,
    meta: TailMeta<'_>,
    shot_info: &ShotInfo,
    params: &DevelopParams,
    registry: &LookRegistry,
    cancel: Option<&AtomicBool>,
) -> Result<Srgb8, DevelopError> {
    Temperature {
        kelvin: params.temperature_kelvin,
    }
    .apply(&mut buf);
    Tint { value: params.tint }.apply(&mut buf);

    let mut buf: Buffer<LinearRec2020> = match meta {
        TailMeta::Raw { sensor_info } => CameraToWorking::from_sensor(sensor_info).apply(buf),
        TailMeta::Image => {
            let lin_srgb: Buffer<LinearSrgb> = buf.into_space();
            SrgbToRec2020.apply(lin_srgb)
        }
    };
    check_cancel(cancel)?;

    Exposure {
        ev: params.exposure_ev,
    }
    .apply(&mut buf);

    apply_xmp_with_strength(
        registry,
        &params.look,
        params.look_strength,
        &mut buf,
        shot_info,
    );
    check_cancel(cancel)?;

    let lin_srgb = Rec2020ToSrgb.apply(buf);
    Ok(SrgbEncode.apply(lin_srgb))
}

/// Develop a `Loaded` to an 8-bit sRGB preview at or below `target` with
/// preview-quality settings (libraw `half_size = true` for RAW).
pub fn develop_preview(
    loaded: &Loaded,
    params: &DevelopParams,
    registry: &LookRegistry,
    target: TargetSize,
    cancel: Option<&AtomicBool>,
) -> Result<Srgb8, DevelopError> {
    let prepared = prepare_source(loaded, target, cancel)?;
    apply_pipeline(&prepared, params, registry, cancel)
}

/// Develop a `Loaded` to an 8-bit sRGB output at full quality (libraw
/// `half_size = false`). `target = None` develops at source resolution; some
/// `Some(t)` develops at-or-below `t`.
pub fn develop_full(
    loaded: &Loaded,
    params: &DevelopParams,
    registry: &LookRegistry,
    target: Option<TargetSize>,
    cancel: Option<&AtomicBool>,
) -> Result<Srgb8, DevelopError> {
    let effective_target = target.unwrap_or_else(|| {
        let (w, h) = loaded.dimensions();
        TargetSize::new(w.max(1), h.max(1))
    });
    let prepared = prepare_source_inner(loaded, effective_target, /* half_size */ false, cancel)?;
    apply_pipeline(&prepared, params, registry, cancel)
}

/// Apply the resolved look at the given strength via in-linear lerp. Identity
/// is a no-op; XMP recipes go through the (currently stub) `ApplyXmp`. For
/// strength in (0, 1) we clone-and-lerp; at 1 we apply in place.
fn apply_xmp_with_strength(
    reg: &LookRegistry,
    id: &str,
    strength: f32,
    image: &mut Buffer<LinearRec2020>,
    shot: &ShotInfo,
) {
    let strength = strength.clamp(0.0, 1.0);
    if strength <= 0.0 {
        return;
    }
    match reg.resolve(id) {
        ResolvedLook::Identity => {
            // identity is identity at every strength
        }
        ResolvedLook::Xmp(recipe) => {
            let iso = effective_iso(shot);
            let applier = ApplyXmp { recipe, iso };
            if strength >= 1.0 {
                applier.apply(image);
                return;
            }
            let neutral = image.clone();
            applier.apply(image);
            let inv = 1.0 - strength;
            let dst = image.data_mut();
            let neu = neutral.data();
            for i in 0..dst.len() {
                dst[i] = inv * neu[i] + strength * dst[i];
            }
        }
    }
}

fn effective_iso(shot: &ShotInfo) -> f32 {
    shot.iso.unwrap_or(100.0).max(50.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transform::xmp::XmpRecipe;
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
        let registry = LookRegistry::new();
        let out = develop_preview(
            &loaded,
            &DevelopParams::default(),
            &registry,
            TargetSize::new(50, 50),
            None,
        )
        .unwrap();
        assert!(out.width <= 50 && out.height <= 50);
        assert_eq!(out.pixels.len() as u32, out.width * out.height * 3);
    }

    #[test]
    fn develop_preview_jpeg_temperature_changes_output() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("a.jpg");
        write_jpeg(&path, 64, 64, [128, 128, 128]);
        let img = decode_image(&path).unwrap();
        let loaded = Loaded::Image(img);
        let registry = LookRegistry::new();
        let neutral = develop_preview(
            &loaded,
            &DevelopParams::default(),
            &registry,
            TargetSize::new(64, 64),
            None,
        )
        .unwrap();
        let mut warm_params = DevelopParams::default();
        warm_params.temperature_kelvin = 4000.0;
        let warm = develop_preview(
            &loaded,
            &warm_params,
            &registry,
            TargetSize::new(64, 64),
            None,
        )
        .unwrap();
        // Warmer (lower K) should boost R and drop B in the rendered output.
        let r_neutral = neutral.pixels.iter().step_by(3).map(|&x| x as u32).sum::<u32>();
        let r_warm = warm.pixels.iter().step_by(3).map(|&x| x as u32).sum::<u32>();
        assert!(r_warm > r_neutral, "warm R {r_warm} <= neutral R {r_neutral}");
    }

    #[test]
    fn develop_preview_jpeg_tint_changes_output() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("a.jpg");
        write_jpeg(&path, 64, 64, [128, 128, 128]);
        let img = decode_image(&path).unwrap();
        let loaded = Loaded::Image(img);
        let registry = LookRegistry::new();
        let neutral = develop_preview(
            &loaded,
            &DevelopParams::default(),
            &registry,
            TargetSize::new(64, 64),
            None,
        )
        .unwrap();
        let mut tinted = DevelopParams::default();
        tinted.tint = 0.8;
        let out = develop_preview(&loaded, &tinted, &registry, TargetSize::new(64, 64), None).unwrap();
        // Positive tint reduces G.
        let g_neutral: u32 = neutral.pixels[1..].iter().step_by(3).map(|&x| x as u32).sum();
        let g_out: u32 = out.pixels[1..].iter().step_by(3).map(|&x| x as u32).sum();
        assert!(g_out < g_neutral, "tinted G {g_out} >= neutral G {g_neutral}");
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
        let registry = LookRegistry::new();
        let out = develop_preview(
            &loaded,
            &DevelopParams::default(),
            &registry,
            TargetSize::new(64, 64),
            None,
        )
        .unwrap();
        assert_eq!((out.width, out.height), (4, 4));
    }

    #[test]
    fn develop_params_default_is_identity_safe() {
        let p = DevelopParams::default();
        assert_eq!(p.exposure_ev, 0.0);
        assert_eq!(p.temperature_kelvin, 5500.0);
        assert_eq!(p.tint, 0.0);
        assert_eq!(p.look, "identity");
        assert_eq!(p.look_strength, 1.0);
    }

    #[test]
    fn develop_params_serde_round_trip_only_5_fields() {
        let p = DevelopParams::default();
        let json = serde_json::to_string(&p).unwrap();
        // The JSON must contain exactly these 5 keys; no warmth/color/etc.
        for key in [
            "exposure_ev",
            "temperature_kelvin",
            "tint",
            "look",
            "look_strength",
        ] {
            assert!(json.contains(key), "missing {key} in {json}");
        }
        for forbidden in [
            "warmth",
            "color",
            "contrast",
            "soft_highlights",
            "shadows",
            "blacks",
            "clarity",
            "grain",
            "grain_seed",
        ] {
            assert!(!json.contains(forbidden), "stray {forbidden} in {json}");
        }
        let back: DevelopParams = serde_json::from_str(&json).unwrap();
        assert_eq!(back.look, p.look);
        assert_eq!(back.look_strength, p.look_strength);
    }

    #[test]
    fn develop_params_v1_json_loads_with_unknowns_dropped() {
        // A v1 JSON row carrying the old 12-field shape. After the shrink,
        // serde silently drops the unknown fields and the surviving 5 load.
        let v1 = r#"{
            "exposure_ev": 0.5,
            "temperature_kelvin": 5500.0,
            "tint": 0.0,
            "look": "identity",
            "look_strength": 1.0,
            "warmth": 0.7,
            "color": -0.3,
            "contrast": 0.2,
            "soft_highlights": 0.1,
            "shadows": -0.4,
            "blacks": 0.0,
            "clarity": 0.5,
            "grain": 0.0,
            "grain_seed": 42
        }"#;
        let p: DevelopParams = serde_json::from_str(v1).unwrap();
        assert_eq!(p.exposure_ev, 0.5);
        assert_eq!(p.look, "identity");
        assert_eq!(p.look_strength, 1.0);
    }

    #[test]
    fn develop_params_fingerprint_changes_on_each_field() {
        let base = DevelopParams::default();
        let base_fp = base.fingerprint();

        let mut p = base.clone();
        p.exposure_ev = 0.5;
        assert_ne!(p.fingerprint(), base_fp);

        let mut p = base.clone();
        p.temperature_kelvin = 6500.0;
        assert_ne!(p.fingerprint(), base_fp);

        let mut p = base.clone();
        p.tint = 0.2;
        assert_ne!(p.fingerprint(), base_fp);

        let mut p = base.clone();
        p.look = "xmp:deadbeef".to_string();
        assert_ne!(p.fingerprint(), base_fp);

        let mut p = base.clone();
        p.look_strength = 0.5;
        assert_ne!(p.fingerprint(), base_fp);
    }

    #[test]
    fn apply_xmp_with_strength_zero_is_noop() {
        let mut reg = LookRegistry::new();
        reg.register_xmp("xmp:1".into(), XmpRecipe::default());
        let mut buf: Buffer<LinearRec2020> = Buffer::from_planar(vec![0.5_f32; 24], 4, 2);
        let original = buf.data().to_vec();
        let shot = ShotInfo::default();
        apply_xmp_with_strength(&reg, "xmp:1", 0.0, &mut buf, &shot);
        assert_eq!(buf.data(), original.as_slice());
    }

    #[test]
    fn apply_xmp_with_strength_identity_is_noop_at_full_strength() {
        let reg = LookRegistry::new();
        let mut buf: Buffer<LinearRec2020> = Buffer::from_planar(vec![0.5_f32; 24], 4, 2);
        let original = buf.data().to_vec();
        let shot = ShotInfo::default();
        apply_xmp_with_strength(&reg, IDENTITY_ID, 1.0, &mut buf, &shot);
        assert_eq!(buf.data(), original.as_slice());
    }
}
