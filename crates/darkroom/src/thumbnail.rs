use codec::{Loaded, TargetSize, Thumbnail};

use crate::common::{DevelopError, RgbImage, fit_within, resize_u8x3};

/// Render the eagerly-loaded thumbnail of a `Loaded` at the given target size.
/// Returns `None` when the file has no preview (image-format files without an
/// EXIF IFD1 thumbnail). The fast path used by the culling view — no pixel
/// decoding, just a SIMD downscale of the cached thumbnail.
pub fn develop_thumbnail(loaded: &Loaded, target: TargetSize) -> Option<RgbImage> {
    let thumb = loaded.preview()?;
    develop_from_thumbnail(thumb, target).ok()
}

fn develop_from_thumbnail(
    thumb: &Thumbnail,
    target: TargetSize,
) -> Result<RgbImage, DevelopError> {
    let (dst_w, dst_h) = fit_within(thumb.width, thumb.height, target);
    if (dst_w, dst_h) == (thumb.width, thumb.height) {
        return Ok(RgbImage {
            width: thumb.width,
            height: thumb.height,
            pixels: thumb.pixels.clone(),
        });
    }
    let resized = resize_u8x3(&thumb.pixels, thumb.width, thumb.height, dst_w, dst_h)?;
    Ok(RgbImage {
        width: dst_w,
        height: dst_h,
        pixels: resized,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn develop_from_thumbnail_downscales() {
        let thumb = Thumbnail {
            width: 16,
            height: 12,
            pixels: vec![100u8; 16 * 12 * 3],
        };
        let out = develop_from_thumbnail(&thumb, TargetSize::new(8, 8)).unwrap();
        assert_eq!((out.width, out.height), (8, 6));
    }

    #[test]
    fn develop_from_thumbnail_does_not_upscale() {
        let thumb = Thumbnail {
            width: 4,
            height: 4,
            pixels: vec![50u8; 4 * 4 * 3],
        };
        let out = develop_from_thumbnail(&thumb, TargetSize::new(64, 64)).unwrap();
        assert_eq!((out.width, out.height), (4, 4));
    }
}
