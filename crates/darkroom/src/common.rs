use std::fmt;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use codec::{DecodeError, TargetSize};
use fast_image_resize::images::{Image as FirImage, ImageRef};
use fast_image_resize::{PixelType, ResizeError, Resizer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbImage {
    pub width: u32,
    pub height: u32,
    /// Row-major sRGB 8-bit RGB. `pixels.len() == width * height * 3`.
    pub pixels: Vec<u8>,
}

#[derive(Debug)]
pub enum DevelopError {
    Decode(DecodeError),
    BufferShape(fast_image_resize::ImageBufferError),
    Resize(ResizeError),
    Cancelled,
}

impl fmt::Display for DevelopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(e) => write!(f, "decode error: {e}"),
            Self::BufferShape(e) => write!(f, "fir buffer error: {e:?}"),
            Self::Resize(e) => write!(f, "fir resize error: {e:?}"),
            Self::Cancelled => write!(f, "develop cancelled"),
        }
    }
}

impl std::error::Error for DevelopError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Decode(e) => Some(e),
            _ => None,
        }
    }
}

/// Aspect-preserving fit. Never upscales: a tiny source stays at native size.
pub fn fit_within(src_w: u32, src_h: u32, target: TargetSize) -> (u32, u32) {
    if src_w == 0 || src_h == 0 {
        return (target.max_w.max(1), target.max_h.max(1));
    }
    let scale_w = target.max_w as f64 / src_w as f64;
    let scale_h = target.max_h as f64 / src_h as f64;
    let scale = scale_w.min(scale_h).min(1.0);
    let w = ((src_w as f64 * scale).round() as u32).max(1);
    let h = ((src_h as f64 * scale).round() as u32).max(1);
    (w, h)
}

pub fn resize_u8x3(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Result<Vec<u8>, DevelopError> {
    let src = ImageRef::new(src_w, src_h, src, PixelType::U8x3).map_err(DevelopError::BufferShape)?;
    let mut dst = FirImage::new(dst_w, dst_h, PixelType::U8x3);
    Resizer::new()
        .resize(&src, &mut dst, None)
        .map_err(DevelopError::Resize)?;
    Ok(dst.into_vec())
}

pub fn resize_u16x3(
    src: &[u16],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Result<Vec<u16>, DevelopError> {
    // Reinterpret as bytes (host endian) for fast_image_resize.
    let src_bytes: &[u8] =
        unsafe { std::slice::from_raw_parts(src.as_ptr() as *const u8, src.len() * 2) };
    let src = ImageRef::new(src_w, src_h, src_bytes, PixelType::U16x3)
        .map_err(DevelopError::BufferShape)?;
    let mut dst = FirImage::new(dst_w, dst_h, PixelType::U16x3);
    Resizer::new()
        .resize(&src, &mut dst, None)
        .map_err(DevelopError::Resize)?;
    let bytes = dst.into_vec();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        out.push(u16::from_ne_bytes([chunk[0], chunk[1]]));
    }
    Ok(out)
}

/// Maps each 16-bit linear sample to an 8-bit sRGB sample via a precomputed LUT.
pub fn linear_to_srgb8(linear: &[u16]) -> Vec<u8> {
    let lut = srgb_lut();
    let mut out = Vec::with_capacity(linear.len());
    for &v in linear {
        out.push(lut[v as usize]);
    }
    out
}

pub fn srgb_lut() -> &'static [u8; 65536] {
    static LUT: OnceLock<Box<[u8; 65536]>> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = Box::new([0u8; 65536]);
        for v in 0..=65535u32 {
            let linear = v as f64 / 65535.0;
            let srgb = if linear <= 0.0031308 {
                linear * 12.92
            } else {
                1.055 * linear.powf(1.0 / 2.4) - 0.055
            };
            lut[v as usize] = (srgb.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
        lut
    })
}

/// BT.2020 → BT.709 (sRGB) linear-light primaries matrix, D65 to D65.
/// Operates on linear-light values; the gamma encode is a separate step.
/// Each row sums to 1 (white preserves to white).
pub fn rec2020_to_srgb_matrix() -> [[f32; 3]; 3] {
    [
        [1.66049, -0.58764, -0.07285],
        [-0.12455, 1.13290, -0.00835],
        [-0.01815, -0.10058, 1.11873],
    ]
}

/// In-place 3×3 matrix multiply across an interleaved u16 RGB buffer with
/// clamping. `pixels.len() % 3 == 0`.
pub fn apply_3x3_u16(pixels: &mut [u16], m: &[[f32; 3]; 3]) {
    for chunk in pixels.chunks_exact_mut(3) {
        let r = chunk[0] as f32;
        let g = chunk[1] as f32;
        let b = chunk[2] as f32;
        let nr = m[0][0] * r + m[0][1] * g + m[0][2] * b;
        let ng = m[1][0] * r + m[1][1] * g + m[1][2] * b;
        let nb = m[2][0] * r + m[2][1] * g + m[2][2] * b;
        chunk[0] = nr.clamp(0.0, 65535.0) as u16;
        chunk[1] = ng.clamp(0.0, 65535.0) as u16;
        chunk[2] = nb.clamp(0.0, 65535.0) as u16;
    }
}

pub fn check_cancel(cancel: Option<&AtomicBool>) -> Result<(), DevelopError> {
    if let Some(flag) = cancel
        && flag.load(Ordering::Relaxed)
    {
        return Err(DevelopError::Cancelled);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_within_does_not_upscale() {
        let (w, h) = fit_within(4, 4, TargetSize::new(64, 64));
        assert_eq!((w, h), (4, 4));
    }

    #[test]
    fn fit_within_scales_proportionally() {
        // 16x12 fit into 8x8 → scale=0.5 → 8x6.
        let (w, h) = fit_within(16, 12, TargetSize::new(8, 8));
        assert_eq!((w, h), (8, 6));
    }

    #[test]
    fn srgb_lut_endpoints() {
        let lut = srgb_lut();
        assert_eq!(lut[0], 0);
        assert_eq!(lut[65535], 255);
    }

    #[test]
    fn linear_to_srgb8_mid_gray_is_srgb_midtone() {
        // 16-bit linear mid-gray (0x8000) should encode to ~188 in sRGB.
        let linear = vec![0x8000_u16; 12];
        let srgb = linear_to_srgb8(&linear);
        for v in srgb {
            assert!((185..=190).contains(&v), "unexpected sample {v}");
        }
    }

    #[test]
    fn apply_identity_3x3_is_noop() {
        let identity = [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ];
        let mut pixels = vec![1000u16, 20000u16, 50000u16];
        apply_3x3_u16(&mut pixels, &identity);
        assert_eq!(pixels, vec![1000u16, 20000u16, 50000u16]);
    }

    #[test]
    fn apply_rec2020_to_srgb_clamps_negatives() {
        // Rec.2020 pure red → sRGB. With BT.2020 → BT.709, the green/blue
        // coefficients are negative, so a saturated R produces small negatives
        // for G and B that should clamp to 0.
        let m = rec2020_to_srgb_matrix();
        let mut pixels = vec![65535u16, 0u16, 0u16];
        apply_3x3_u16(&mut pixels, &m);
        // R blows past saturation → clamped to 65535. G and B get small negs → 0.
        assert_eq!(pixels[0], 65535);
        assert_eq!(pixels[1], 0);
        assert_eq!(pixels[2], 0);
    }

    #[test]
    fn resize_u8x3_downscales() {
        let pixels = vec![100u8; 16 * 12 * 3];
        let out = resize_u8x3(&pixels, 16, 12, 8, 6).unwrap();
        assert_eq!(out.len(), 8 * 6 * 3);
        // Constant input → constant output.
        assert!(out.iter().all(|&v| (95..=105).contains(&v)));
    }

    #[test]
    fn resize_u16x3_downscales() {
        let pixels = vec![0x8000_u16; 8 * 8 * 3];
        let out = resize_u16x3(&pixels, 8, 8, 4, 4).unwrap();
        assert_eq!(out.len(), 4 * 4 * 3);
        // Constant input → constant output.
        for v in out {
            assert!((0x7F00..=0x80FF).contains(&v));
        }
    }
}
