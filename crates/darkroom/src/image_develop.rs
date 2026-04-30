use std::sync::atomic::AtomicBool;

use codec::{Image, TargetSize, read_image_pixels};

use crate::common::{DevelopError, RgbImage, check_cancel, fit_within, resize_u8x3};

/// Develop an `Image` into a display-ready sRGB 8-bit `RgbImage` at or below
/// `target`. Identity color-wise (sRGB → sRGB); only resize is applied.
pub fn develop(
    img: &Image,
    target: TargetSize,
    cancel: Option<&AtomicBool>,
) -> Result<RgbImage, DevelopError> {
    let pixels = read_image_pixels(img, target, cancel).map_err(DevelopError::Decode)?;
    check_cancel(cancel)?;

    let (dst_w, dst_h) = fit_within(pixels.width, pixels.height, target);
    if (dst_w, dst_h) == (pixels.width, pixels.height) {
        return Ok(RgbImage {
            width: pixels.width,
            height: pixels.height,
            pixels: pixels.data,
        });
    }
    let resized = resize_u8x3(&pixels.data, pixels.width, pixels.height, dst_w, dst_h)?;
    Ok(RgbImage {
        width: dst_w,
        height: dst_h,
        pixels: resized,
    })
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
    fn develop_jpeg_fits_within_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("a.jpg");
        write_jpeg(&path, 200, 150, [120, 80, 200]);

        let img = decode_image(&path).unwrap();
        assert_eq!(img.kind, ImageKind::Jpeg);
        let out = develop(&img, TargetSize::new(50, 50), None).unwrap();
        assert!(out.width <= 50 && out.height <= 50);
        assert_eq!(out.pixels.len() as u32, out.width * out.height * 3);
    }

    #[test]
    fn develop_does_not_upscale_when_target_is_larger() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("tiny.png");
        let buf = ImageBuffer::<Rgb<u8>, _>::from_pixel(4, 4, Rgb([200, 200, 200]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(buf)
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        std::fs::write(&path, bytes).unwrap();

        let img = decode_image(&path).unwrap();
        let out = develop(&img, TargetSize::new(64, 64), None).unwrap();
        assert_eq!((out.width, out.height), (4, 4));
    }
}
