use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use codec::{DecodeError, TargetSize};
use fast_image_resize::images::{Image as FirImage, ImageRef};
use fast_image_resize::{PixelType, ResizeError, Resizer};

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

/// Resize an interleaved sRGB 8-bit RGB buffer. Used at the image-format
/// input boundary (JPEG/PNG/TIFF after decode).
pub fn resize_u8x3(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Result<Vec<u8>, DevelopError> {
    let src =
        ImageRef::new(src_w, src_h, src, PixelType::U8x3).map_err(DevelopError::BufferShape)?;
    let mut dst = FirImage::new(dst_w, dst_h, PixelType::U8x3);
    Resizer::new()
        .resize(&src, &mut dst, None)
        .map_err(DevelopError::Resize)?;
    Ok(dst.into_vec())
}

/// Resize a planar f32 `Buffer<S>` to the given dimensions. Each channel
/// plane is resized independently via fast_image_resize's `PixelType::F32`
/// (single-channel) — `fir` doesn't have a planar 3-channel f32 type.
///
/// Returns the input unchanged if `(dst_w, dst_h) == src.dimensions()`.
pub fn resize_f32_planar<S: crate::space::ColorSpace>(
    src: crate::space::Buffer<S>,
    dst_w: u32,
    dst_h: u32,
) -> Result<crate::space::Buffer<S>, DevelopError> {
    let (src_w, src_h) = src.dimensions();
    if (src_w, src_h) == (dst_w, dst_h) {
        return Ok(src);
    }
    let plane_dst = (dst_w as usize) * (dst_h as usize);
    let mut out_data = vec![0.0f32; plane_dst * 3];
    let (out_r, rest) = out_data.split_at_mut(plane_dst);
    let (out_g, out_b) = rest.split_at_mut(plane_dst);
    resize_plane_f32(src.r(), src_w, src_h, out_r, dst_w, dst_h)?;
    resize_plane_f32(src.g(), src_w, src_h, out_g, dst_w, dst_h)?;
    resize_plane_f32(src.b(), src_w, src_h, out_b, dst_w, dst_h)?;
    Ok(crate::space::Buffer::<S>::from_planar(
        out_data, dst_w, dst_h,
    ))
}

fn resize_plane_f32(
    src: &[f32],
    src_w: u32,
    src_h: u32,
    dst: &mut [f32],
    dst_w: u32,
    dst_h: u32,
) -> Result<(), DevelopError> {
    let src_bytes: &[u8] =
        unsafe { std::slice::from_raw_parts(src.as_ptr() as *const u8, src.len() * 4) };
    let src_img = ImageRef::new(src_w, src_h, src_bytes, PixelType::F32)
        .map_err(DevelopError::BufferShape)?;
    let mut dst_img = FirImage::new(dst_w, dst_h, PixelType::F32);
    Resizer::new()
        .resize(&src_img, &mut dst_img, None)
        .map_err(DevelopError::Resize)?;
    let bytes = dst_img.into_vec();
    debug_assert_eq!(bytes.len(), dst.len() * 4);
    for (i, chunk) in bytes.chunks_exact(4).enumerate() {
        dst[i] = f32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    Ok(())
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
    fn resize_u8x3_downscales() {
        let pixels = vec![100u8; 16 * 12 * 3];
        let out = resize_u8x3(&pixels, 16, 12, 8, 6).unwrap();
        assert_eq!(out.len(), 8 * 6 * 3);
        assert!(out.iter().all(|&v| (95..=105).contains(&v)));
    }

    #[test]
    fn resize_f32_planar_constant_input_constant_output() {
        use crate::space::{Buffer, LinearRec2020};
        let buf: Buffer<LinearRec2020> = Buffer::from_planar(vec![0.5_f32; 8 * 8 * 3], 8, 8);
        let out = resize_f32_planar(buf, 4, 4).unwrap();
        for &v in out.data() {
            assert!((v - 0.5).abs() < 1e-3);
        }
    }

    #[test]
    fn resize_f32_planar_identity_dims_returns_input() {
        use crate::space::{Buffer, LinearRec2020};
        let data: Vec<f32> = (0..48).map(|i| i as f32).collect();
        let buf: Buffer<LinearRec2020> = Buffer::from_planar(data.clone(), 4, 4);
        let out = resize_f32_planar(buf, 4, 4).unwrap();
        assert_eq!(out.into_data(), data);
    }
}
