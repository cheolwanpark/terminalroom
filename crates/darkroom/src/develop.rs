use std::fmt;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use codec::{DecodeError, DecodedImage, ImageKind, TargetSize};
use fast_image_resize::images::{Image, ImageRef};
use fast_image_resize::{PixelType, ResizeError, Resizer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbImage {
    pub width: u32,
    pub height: u32,
    /// Row-major 3-channel sRGB, 8 bpc. `pixels.len() == width * height * 3`.
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

pub fn develop_to_rgb(
    path: &Path,
    kind: ImageKind,
    target: TargetSize,
    cancel: Option<&AtomicBool>,
) -> Result<RgbImage, DevelopError> {
    let decoded = codec::decode(path, kind, target, cancel).map_err(DevelopError::Decode)?;
    develop(decoded, target, cancel)
}

pub fn develop(
    decoded: DecodedImage,
    target: TargetSize,
    cancel: Option<&AtomicBool>,
) -> Result<RgbImage, DevelopError> {
    check_cancel(cancel)?;
    let (src_w, src_h) = decoded.dimensions();
    let (dst_w, dst_h) = fit_within(src_w, src_h, target);

    match decoded {
        DecodedImage::Linear { width, height, data } => {
            // Reinterpret the u16 buffer as bytes (host endian) for fast_image_resize.
            let src_bytes: &[u8] = unsafe {
                std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2)
            };
            let src = ImageRef::new(width, height, src_bytes, PixelType::U16x3)
                .map_err(DevelopError::BufferShape)?;
            let mut dst = Image::new(dst_w, dst_h, PixelType::U16x3);
            Resizer::new()
                .resize(&src, &mut dst, None)
                .map_err(DevelopError::Resize)?;

            check_cancel(cancel)?;
            let pixels = encode_linear_to_srgb8(dst.buffer());
            Ok(RgbImage {
                width: dst_w,
                height: dst_h,
                pixels,
            })
        }
        DecodedImage::Srgb8 {
            width,
            height,
            pixels,
        } => {
            let src = ImageRef::new(width, height, &pixels, PixelType::U8x3)
                .map_err(DevelopError::BufferShape)?;
            let mut dst = Image::new(dst_w, dst_h, PixelType::U8x3);
            Resizer::new()
                .resize(&src, &mut dst, None)
                .map_err(DevelopError::Resize)?;

            check_cancel(cancel)?;
            Ok(RgbImage {
                width: dst_w,
                height: dst_h,
                pixels: dst.into_vec(),
            })
        }
    }
}

/// Maps each 16-bit linear sample to an 8-bit sRGB sample via a precomputed LUT.
fn encode_linear_to_srgb8(linear_bytes: &[u8]) -> Vec<u8> {
    let lut = srgb_lut();
    let mut out = Vec::with_capacity(linear_bytes.len() / 2);
    for chunk in linear_bytes.chunks_exact(2) {
        let v = u16::from_ne_bytes([chunk[0], chunk[1]]) as usize;
        out.push(lut[v]);
    }
    out
}

fn srgb_lut() -> &'static [u8; 65536] {
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

fn fit_within(src_w: u32, src_h: u32, target: TargetSize) -> (u32, u32) {
    if src_w == 0 || src_h == 0 {
        return (target.max_w.max(1), target.max_h.max(1));
    }
    let scale_w = target.max_w as f64 / src_w as f64;
    let scale_h = target.max_h as f64 / src_h as f64;
    // Never upscale: a tiny source stays at its native size.
    let scale = scale_w.min(scale_h).min(1.0);
    let w = ((src_w as f64 * scale).round() as u32).max(1);
    let h = ((src_h as f64 * scale).round() as u32).max(1);
    (w, h)
}

fn check_cancel(cancel: Option<&AtomicBool>) -> Result<(), DevelopError> {
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
    use image::{ImageBuffer, ImageFormat, Rgb};
    use std::io::Cursor;

    fn write_jpeg(path: &Path, w: u32, h: u32, color: [u8; 3]) {
        let buf = ImageBuffer::<Rgb<u8>, _>::from_pixel(w, h, Rgb(color));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(buf)
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Jpeg)
            .unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn develop_linear_mid_gray_produces_srgb_midtone() {
        // 16-bit linear mid-gray (0.5 in linear light) → ~188 in sRGB.
        let pixels: Vec<u16> = vec![0x8000; 8 * 8 * 3];
        let decoded = DecodedImage::Linear {
            width: 8,
            height: 8,
            data: pixels,
        };
        let out = develop(decoded, TargetSize::new(4, 4), None).unwrap();
        assert_eq!(out.width, 4);
        assert_eq!(out.height, 4);
        assert_eq!(out.pixels.len(), 4 * 4 * 3);
        for &v in &out.pixels {
            // sRGB encoding of 0.5 linear is ~0.7354, i.e. ~187.5. Allow a small window.
            assert!((185..=190).contains(&v), "unexpected sample value {v}");
        }
    }

    #[test]
    fn develop_srgb_resizes_without_upscaling() {
        let pixels = vec![100u8; 16 * 12 * 3];
        let decoded = DecodedImage::Srgb8 {
            width: 16,
            height: 12,
            pixels,
        };
        let out = develop(decoded, TargetSize::new(8, 8), None).unwrap();
        // src 16x12, target 8x8 → scale = min(0.5, 0.667) = 0.5 → 8x6.
        assert_eq!((out.width, out.height), (8, 6));
        assert_eq!(out.pixels.len(), (out.width * out.height * 3) as usize);
    }

    #[test]
    fn develop_does_not_upscale_when_target_larger_than_source() {
        let decoded = DecodedImage::Srgb8 {
            width: 4,
            height: 4,
            pixels: vec![200u8; 4 * 4 * 3],
        };
        let out = develop(decoded, TargetSize::new(64, 64), None).unwrap();
        assert_eq!((out.width, out.height), (4, 4));
    }

    #[test]
    fn develop_cancelled_pre_resize_errors() {
        let decoded = DecodedImage::Srgb8 {
            width: 4,
            height: 4,
            pixels: vec![0u8; 4 * 4 * 3],
        };
        let flag = AtomicBool::new(true);
        let err = develop(decoded, TargetSize::new(2, 2), Some(&flag)).unwrap_err();
        assert!(matches!(err, DevelopError::Cancelled));
    }

    #[test]
    fn develop_to_rgb_jpeg_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("a.jpg");
        write_jpeg(&path, 200, 150, [120, 80, 200]);

        let out = develop_to_rgb(&path, ImageKind::Jpeg, TargetSize::new(50, 50), None).unwrap();
        // Aspect-preserving fit: 200x150 → 50xN with scale=0.25 → 50x37 or 50x38 depending on jpeg-decoder's IDCT pick + fit rounding.
        assert!(out.width <= 50 && out.height <= 50);
        assert_eq!(out.pixels.len() as u32, out.width * out.height * 3);
    }

    #[test]
    fn srgb_lut_endpoints() {
        let lut = srgb_lut();
        assert_eq!(lut[0], 0);
        assert_eq!(lut[65535], 255);
    }
}
