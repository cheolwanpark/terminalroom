use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use image::{DynamicImage, ImageDecoder, ImageReader};
use libraw_rs::ShotInfo;

use crate::format::ImageKind;
use crate::jpeg::{apply_orientation_to_rgb8, decode_jpeg_to_srgb8, dims_after_orientation};
use crate::metadata::read_image_exif;
use crate::{DecodeError, Srgb8Pixels, TargetSize, check_cancel, classify};

/// Header-level info about an image-format file (JPEG/PNG/TIFF). Pixel data is
/// loaded lazily via [`read_image_pixels`] — image-format files decode fast
/// enough that an eager preview is not worth the extra memory.
#[derive(Debug, Clone)]
pub struct Image {
    pub source: PathBuf,
    pub kind: ImageKind,
    /// Width after EXIF orientation has been applied.
    pub width: u32,
    /// Height after EXIF orientation has been applied.
    pub height: u32,
    /// EXIF Orientation tag value (1..=8). 1 = no transform.
    pub orientation: u16,
    pub shot_info: ShotInfo,
}

/// Open an image file and return its header (dimensions, shot-info, orientation).
/// Does not decode pixel data.
pub fn decode_image(path: &Path) -> Result<Image, DecodeError> {
    let kind = match path
        .extension()
        .and_then(|s| s.to_str())
        .and_then(classify)
    {
        Some(k @ (ImageKind::Jpeg | ImageKind::Png | ImageKind::Tiff)) => k,
        Some(ImageKind::Raw) => return Err(DecodeError::WrongKind { expected: "Image" }),
        None => return Err(DecodeError::UnsupportedExtension),
    };

    let exif = read_image_exif(path)?;
    let (file_w, file_h) = read_file_dimensions(path, kind)?;
    let (width, height) = dims_after_orientation(file_w, file_h, exif.orientation);

    Ok(Image {
        source: path.to_path_buf(),
        kind,
        width,
        height,
        orientation: exif.orientation,
        shot_info: exif.shot,
    })
}

/// Decode the full image pixels in sRGB 8-bit at (or below) the given target
/// size. JPEGs use IDCT-scale for speed; PNG/TIFF decode at native resolution.
/// EXIF orientation is applied so the output is upright.
pub fn read_image_pixels(
    img: &Image,
    target: TargetSize,
    cancel: Option<&AtomicBool>,
) -> Result<Srgb8Pixels, DecodeError> {
    check_cancel(cancel)?;
    match img.kind {
        ImageKind::Jpeg => {
            let file = File::open(&img.source).map_err(DecodeError::Io)?;
            decode_jpeg_to_srgb8(BufReader::new(file), target, img.orientation, cancel)
        }
        ImageKind::Png | ImageKind::Tiff => {
            decode_via_image_crate(&img.source, img.orientation, cancel)
        }
        ImageKind::Raw => unreachable!("Image::kind is never Raw"),
    }
}

fn read_file_dimensions(path: &Path, kind: ImageKind) -> Result<(u32, u32), DecodeError> {
    match kind {
        ImageKind::Jpeg => {
            let file = File::open(path).map_err(DecodeError::Io)?;
            let mut decoder = jpeg_decoder::Decoder::new(BufReader::new(file));
            decoder.read_info().map_err(DecodeError::Jpeg)?;
            let info = decoder.info().expect("info available after read_info");
            Ok((info.width as u32, info.height as u32))
        }
        ImageKind::Png | ImageKind::Tiff => {
            let reader = ImageReader::open(path)
                .map_err(DecodeError::Io)?
                .with_guessed_format()
                .map_err(DecodeError::Io)?;
            let decoder = reader.into_decoder().map_err(DecodeError::Image)?;
            Ok(decoder.dimensions())
        }
        ImageKind::Raw => unreachable!(),
    }
}

fn decode_via_image_crate(
    path: &Path,
    orientation_code: u16,
    cancel: Option<&AtomicBool>,
) -> Result<Srgb8Pixels, DecodeError> {
    let reader = ImageReader::open(path)
        .map_err(DecodeError::Io)?
        .with_guessed_format()
        .map_err(DecodeError::Io)?;
    let decoder = reader.into_decoder().map_err(DecodeError::Image)?;
    check_cancel(cancel)?;

    let img = DynamicImage::from_decoder(decoder).map_err(DecodeError::Image)?;
    let rgb = match img {
        DynamicImage::ImageRgb8(buf) => buf,
        other => other.to_rgb8(),
    };
    let (width, height) = rgb.dimensions();
    let (w, h, data) = apply_orientation_to_rgb8(width, height, rgb.into_raw(), orientation_code);
    Ok(Srgb8Pixels {
        width: w,
        height: h,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, ImageFormat, Rgb};
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;

    fn write_jpeg(path: &Path, w: u32, h: u32) {
        let buf = ImageBuffer::<Rgb<u8>, _>::from_pixel(w, h, Rgb([180, 90, 60]));
        let mut bytes = Vec::new();
        DynamicImage::ImageRgb8(buf)
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Jpeg)
            .unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn write_png(path: &Path, w: u32, h: u32) {
        let buf = ImageBuffer::<Rgb<u8>, _>::from_pixel(w, h, Rgb([10, 220, 30]));
        let mut bytes = Vec::new();
        DynamicImage::ImageRgb8(buf)
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn write_tiff(path: &Path, w: u32, h: u32) {
        let buf = ImageBuffer::<Rgb<u8>, _>::from_pixel(w, h, Rgb([5, 5, 240]));
        let mut bytes = Vec::new();
        DynamicImage::ImageRgb8(buf)
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Tiff)
            .unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn decode_image_returns_jpeg_header() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("a.jpg");
        write_jpeg(&path, 800, 600);

        let img = decode_image(&path).unwrap();
        assert_eq!(img.kind, ImageKind::Jpeg);
        assert_eq!(img.width, 800);
        assert_eq!(img.height, 600);
        assert_eq!(img.orientation, 1);
    }

    #[test]
    fn read_image_pixels_jpeg_scales_down_via_idct() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("a.jpg");
        write_jpeg(&path, 800, 600);

        let img = decode_image(&path).unwrap();
        let target = TargetSize::new(100, 100);
        let pixels = read_image_pixels(&img, target, None).unwrap();
        assert!(
            pixels.width < 800 && pixels.height < 600,
            "expected scaled-down dims, got {}x{}",
            pixels.width,
            pixels.height
        );
        assert_eq!(pixels.data.len() as u32, pixels.width * pixels.height * 3);
    }

    #[test]
    fn read_image_pixels_png_returns_full_size() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("a.png");
        write_png(&path, 16, 12);

        let img = decode_image(&path).unwrap();
        assert_eq!((img.width, img.height), (16, 12));
        let pixels = read_image_pixels(&img, TargetSize::new(8, 8), None).unwrap();
        assert_eq!((pixels.width, pixels.height), (16, 12));
        assert_eq!(&pixels.data[..3], &[10, 220, 30]);
    }

    #[test]
    fn read_image_pixels_tiff_returns_full_size() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("a.tiff");
        write_tiff(&path, 8, 8);

        let img = decode_image(&path).unwrap();
        assert_eq!((img.width, img.height), (8, 8));
        let pixels = read_image_pixels(&img, TargetSize::new(4, 4), None).unwrap();
        assert_eq!((pixels.width, pixels.height), (8, 8));
    }

    #[test]
    fn read_image_pixels_pre_cancelled_returns_cancelled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("a.jpg");
        write_jpeg(&path, 32, 32);

        let img = decode_image(&path).unwrap();
        let flag = AtomicBool::new(true);
        let err = read_image_pixels(&img, TargetSize::new(8, 8), Some(&flag)).unwrap_err();
        assert!(matches!(err, DecodeError::Cancelled));
    }

    #[test]
    fn decode_image_missing_file_returns_io_error() {
        let path = PathBuf::from("/nonexistent/__not_a_real_file__.jpg");
        let err = decode_image(&path).unwrap_err();
        assert!(matches!(err, DecodeError::Io(_)), "got {err:?}");
    }

    #[test]
    fn decode_image_rejects_raw_extension() {
        let path = PathBuf::from("/nonexistent/file.cr3");
        let err = decode_image(&path).unwrap_err();
        assert!(matches!(err, DecodeError::WrongKind { expected: "Image" }));
    }

    #[test]
    fn decode_image_rejects_unknown_extension() {
        let path = PathBuf::from("/nonexistent/file.txt");
        let err = decode_image(&path).unwrap_err();
        assert!(matches!(err, DecodeError::UnsupportedExtension));
    }

    #[test]
    fn apply_orientation_rotate90_swaps_dims_and_repositions_pixels() {
        // 2x1 image: red on left (0,0), blue on right (1,0).
        let pixels = vec![255, 0, 0, 0, 0, 255];
        // EXIF code 6 = Rotate90 CW.
        let (w, h, out) = apply_orientation_to_rgb8(2, 1, pixels, 6);
        assert_eq!((w, h), (1, 2));
        assert_eq!(&out[0..3], &[255, 0, 0]);
        assert_eq!(&out[3..6], &[0, 0, 255]);
    }

    #[test]
    fn apply_orientation_default_code_is_identity() {
        let pixels = vec![1, 2, 3, 4, 5, 6];
        let (w, h, out) = apply_orientation_to_rgb8(2, 1, pixels.clone(), 1);
        assert_eq!((w, h), (2, 1));
        assert_eq!(out, pixels);
    }
}
