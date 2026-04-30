use std::fmt;
use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use image::metadata::Orientation;
use image::{DynamicImage, ImageBuffer, ImageDecoder, ImageReader, Rgb};
use libraw_rs::{LinearOptions, read_embedded_jpeg, read_linear, read_metadata};

use crate::format::ImageKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetSize {
    pub max_w: u32,
    pub max_h: u32,
}

impl TargetSize {
    pub fn new(max_w: u32, max_h: u32) -> Self {
        Self {
            max_w: max_w.max(1),
            max_h: max_h.max(1),
        }
    }
}

#[derive(Debug)]
pub enum DecodedImage {
    /// Linear scene-referred RGB, 16 bpc, 3 channels, sRGB primaries, gamma 1.0.
    Linear {
        width: u32,
        height: u32,
        data: Vec<u16>,
    },
    /// Display-referred sRGB, 8 bpc, 3 channels.
    Srgb8 {
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    },
}

impl DecodedImage {
    pub fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Linear { width, height, .. } | Self::Srgb8 { width, height, .. } => {
                (*width, *height)
            }
        }
    }
}

#[derive(Debug)]
pub enum DecodeError {
    Io(std::io::Error),
    Image(image::ImageError),
    Jpeg(jpeg_decoder::Error),
    Raw(libraw_rs::Error),
    UnsupportedJpegPixelFormat(jpeg_decoder::PixelFormat),
    Cancelled,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "i/o error: {e}"),
            Self::Image(e) => write!(f, "image decode error: {e}"),
            Self::Jpeg(e) => write!(f, "jpeg decode error: {e}"),
            Self::Raw(e) => write!(f, "raw decode error: {e}"),
            Self::UnsupportedJpegPixelFormat(p) => {
                write!(f, "unsupported JPEG pixel format: {p:?}")
            }
            Self::Cancelled => write!(f, "decode cancelled"),
        }
    }
}

impl std::error::Error for DecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Image(e) => Some(e),
            Self::Jpeg(e) => Some(e),
            Self::Raw(e) => Some(e),
            _ => None,
        }
    }
}

pub fn decode(
    path: &Path,
    kind: ImageKind,
    target: TargetSize,
    cancel: Option<&AtomicBool>,
) -> Result<DecodedImage, DecodeError> {
    check_cancel(cancel)?;
    match kind {
        ImageKind::Raw => decode_raw(path, target, cancel),
        ImageKind::Jpeg => decode_jpeg(path, target, cancel),
        ImageKind::Png | ImageKind::Tiff => decode_via_image(path, cancel),
    }
}

fn decode_raw(
    path: &Path,
    target: TargetSize,
    cancel: Option<&AtomicBool>,
) -> Result<DecodedImage, DecodeError> {
    // Fast path: most RAW files carry a camera-encoded JPEG thumbnail. For terminal-
    // sized previews this skips libraw_unpack/dcraw_process entirely (the long pole).
    // Only fall through to demosaic when the camera didn't embed a JPEG thumb.
    if let Some(thumb) = read_embedded_jpeg(path).map_err(DecodeError::Raw)? {
        check_cancel(cancel)?;
        return decode_jpeg_bytes(&thumb.bytes, target, cancel);
    }

    let meta = read_metadata(path).map_err(DecodeError::Raw)?;
    let half_size = should_halve(target, meta.width, meta.height);
    let opts = LinearOptions {
        half_size,
        use_camera_wb: true,
        // Bilinear interpolation for previews — sufficient for terminal-sized targets,
        // ~2-4x faster than AHD on most sensors.
        user_qual: 0,
    };
    let linear = read_linear(path, &opts, cancel).map_err(DecodeError::Raw)?;
    Ok(DecodedImage::Linear {
        width: linear.width,
        height: linear.height,
        data: linear.data,
    })
}

fn decode_jpeg(
    path: &Path,
    target: TargetSize,
    cancel: Option<&AtomicBool>,
) -> Result<DecodedImage, DecodeError> {
    let file = File::open(path).map_err(DecodeError::Io)?;
    let mut decoder = jpeg_decoder::Decoder::new(BufReader::new(file));
    decode_jpeg_with(&mut decoder, target, cancel)
}

fn decode_jpeg_bytes(
    bytes: &[u8],
    target: TargetSize,
    cancel: Option<&AtomicBool>,
) -> Result<DecodedImage, DecodeError> {
    let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(bytes));
    decode_jpeg_with(&mut decoder, target, cancel)
}

fn decode_jpeg_with<R: std::io::Read>(
    decoder: &mut jpeg_decoder::Decoder<R>,
    target: TargetSize,
    cancel: Option<&AtomicBool>,
) -> Result<DecodedImage, DecodeError> {
    decoder.read_info().map_err(DecodeError::Jpeg)?;

    let target_w = clamp_u16(target.max_w);
    let target_h = clamp_u16(target.max_h);
    let _ = decoder
        .scale(target_w, target_h)
        .map_err(DecodeError::Jpeg)?;

    check_cancel(cancel)?;
    let pixels = decoder.decode().map_err(DecodeError::Jpeg)?;
    let info = decoder.info().expect("info available after decode");

    let (width, height) = (info.width as u32, info.height as u32);
    let rgb = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => pixels,
        jpeg_decoder::PixelFormat::L8 => expand_l8_to_rgb(&pixels),
        jpeg_decoder::PixelFormat::L16 => expand_l16_to_rgb(&pixels),
        jpeg_decoder::PixelFormat::CMYK32 => cmyk32_to_rgb(&pixels),
    };

    // EXIF orientation tag — present on both standalone JPEGs and the camera-embedded
    // thumbs we extract from RAW containers. Without applying it, portrait shots taken
    // on a landscape sensor render sideways.
    let orientation = decoder
        .exif_data()
        .and_then(Orientation::from_exif_chunk)
        .unwrap_or(Orientation::NoTransforms);
    let (width, height, pixels) = apply_orientation_to_rgb8(width, height, rgb, orientation);

    Ok(DecodedImage::Srgb8 {
        width,
        height,
        pixels,
    })
}

fn apply_orientation_to_rgb8(
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    orientation: Orientation,
) -> (u32, u32, Vec<u8>) {
    if orientation == Orientation::NoTransforms {
        return (width, height, pixels);
    }
    // from_raw consumes the Vec; the buffer length is `width*height*3` by construction
    // so this never returns None.
    let buf = ImageBuffer::<Rgb<u8>, _>::from_raw(width, height, pixels)
        .expect("rgb buffer length must match width*height*3");
    let mut img = DynamicImage::ImageRgb8(buf);
    img.apply_orientation(orientation);
    let (new_w, new_h) = (img.width(), img.height());
    (new_w, new_h, img.into_rgb8().into_raw())
}

fn decode_via_image(
    path: &Path,
    cancel: Option<&AtomicBool>,
) -> Result<DecodedImage, DecodeError> {
    let reader = ImageReader::open(path)
        .map_err(DecodeError::Io)?
        .with_guessed_format()
        .map_err(DecodeError::Io)?;
    let mut decoder = reader.into_decoder().map_err(DecodeError::Image)?;
    // Pull orientation before consuming the decoder. Default impl returns NoTransforms;
    // TIFF/JPEG/WebP override it to read EXIF.
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    check_cancel(cancel)?;

    let mut img = DynamicImage::from_decoder(decoder).map_err(DecodeError::Image)?;
    if orientation != Orientation::NoTransforms {
        img.apply_orientation(orientation);
    }

    let rgb = match img {
        DynamicImage::ImageRgb8(buf) => buf,
        other => other.to_rgb8(),
    };
    let (width, height) = rgb.dimensions();
    Ok(DecodedImage::Srgb8 {
        width,
        height,
        pixels: rgb.into_raw(),
    })
}

fn should_halve(target: TargetSize, sensor_w: u32, sensor_h: u32) -> bool {
    if sensor_w == 0 || sensor_h == 0 {
        return false;
    }
    target.max_w.saturating_mul(2) <= sensor_w && target.max_h.saturating_mul(2) <= sensor_h
}

fn check_cancel(cancel: Option<&AtomicBool>) -> Result<(), DecodeError> {
    if let Some(flag) = cancel
        && flag.load(Ordering::Relaxed)
    {
        return Err(DecodeError::Cancelled);
    }
    Ok(())
}

fn clamp_u16(v: u32) -> u16 {
    v.min(u16::MAX as u32) as u16
}

fn expand_l8_to_rgb(luma: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(luma.len() * 3);
    for &y in luma {
        out.extend_from_slice(&[y, y, y]);
    }
    out
}

fn expand_l16_to_rgb(luma: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity((luma.len() / 2) * 3);
    for chunk in luma.chunks_exact(2) {
        // Take the high byte; we're collapsing 16-bit luma into 8-bit RGB.
        let y = chunk[1];
        out.extend_from_slice(&[y, y, y]);
    }
    out
}

fn cmyk32_to_rgb(cmyk: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity((cmyk.len() / 4) * 3);
    for chunk in cmyk.chunks_exact(4) {
        let (c, m, y, k) = (chunk[0] as u16, chunk[1] as u16, chunk[2] as u16, chunk[3] as u16);
        // Standard JFIF inverted-CMYK conversion (as Adobe writes it).
        let r = ((c * k) / 255) as u8;
        let g = ((m * k) / 255) as u8;
        let b = ((y * k) / 255) as u8;
        out.extend_from_slice(&[r, g, b]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, ImageFormat, Rgb};
    use std::io::Cursor;
    use std::path::PathBuf;

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
    fn decode_jpeg_scales_down_via_idct() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("a.jpg");
        write_jpeg(&path, 800, 600);

        let target = TargetSize::new(100, 100);
        let img = decode(&path, ImageKind::Jpeg, target, None).unwrap();
        let (w, h) = img.dimensions();
        // jpeg-decoder picks an IDCT factor (1/2/4/8); the result is roughly target-sized,
        // not the original 800x600.
        assert!(
            w < 800 && h < 600,
            "expected scaled-down dimensions, got {w}x{h}"
        );
        match img {
            DecodedImage::Srgb8 { pixels, .. } => assert_eq!(pixels.len() as u32, w * h * 3),
            other => panic!("expected Srgb8, got {other:?}"),
        }
    }

    #[test]
    fn decode_png_returns_full_size_srgb() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("a.png");
        write_png(&path, 16, 12);

        let img = decode(&path, ImageKind::Png, TargetSize::new(8, 8), None).unwrap();
        assert_eq!(img.dimensions(), (16, 12));
        match img {
            DecodedImage::Srgb8 { pixels, .. } => {
                assert_eq!(pixels[..3], [10, 220, 30]);
            }
            _ => panic!("expected Srgb8"),
        }
    }

    #[test]
    fn decode_tiff_returns_full_size_srgb() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("a.tiff");
        write_tiff(&path, 8, 8);

        let img = decode(&path, ImageKind::Tiff, TargetSize::new(4, 4), None).unwrap();
        assert_eq!(img.dimensions(), (8, 8));
    }

    #[test]
    fn decode_jpeg_pre_cancelled_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("a.jpg");
        write_jpeg(&path, 32, 32);

        let flag = AtomicBool::new(true);
        let err = decode(&path, ImageKind::Jpeg, TargetSize::new(8, 8), Some(&flag)).unwrap_err();
        assert!(matches!(err, DecodeError::Cancelled));
    }

    #[test]
    fn decode_missing_jpeg_returns_io_error() {
        let path = PathBuf::from("/nonexistent/__not_a_real_file__.jpg");
        let err = decode(&path, ImageKind::Jpeg, TargetSize::new(8, 8), None).unwrap_err();
        assert!(matches!(err, DecodeError::Io(_)), "got {err:?}");
    }

    #[test]
    fn apply_orientation_rotate90_swaps_dims_and_repositions_pixels() {
        // 2x1 image: red on left (0,0), blue on right (1,0).
        let pixels = vec![255, 0, 0, 0, 0, 255];
        let (w, h, out) = apply_orientation_to_rgb8(2, 1, pixels, Orientation::Rotate90);
        assert_eq!((w, h), (1, 2), "Rotate90 swaps dims");
        // After 90° CW: red moves from top-left to top-right of a 1-wide column,
        // i.e. red is at (0, 0) and blue is at (0, 1).
        assert_eq!(&out[0..3], &[255, 0, 0], "top pixel should be red");
        assert_eq!(&out[3..6], &[0, 0, 255], "bottom pixel should be blue");
    }

    #[test]
    fn apply_orientation_no_transforms_is_identity() {
        let pixels = vec![1, 2, 3, 4, 5, 6];
        let (w, h, out) = apply_orientation_to_rgb8(2, 1, pixels.clone(), Orientation::NoTransforms);
        assert_eq!((w, h), (2, 1));
        assert_eq!(out, pixels);
    }

    #[test]
    fn should_halve_uses_target_vs_sensor() {
        // Half-size when target is at least 2x smaller than sensor in both dims.
        assert!(should_halve(TargetSize::new(2000, 1500), 6000, 4000));
        // Don't halve when target is too close to the sensor.
        assert!(!should_halve(TargetSize::new(3500, 1500), 6000, 4000));
        // Don't halve for unknown sensor dims.
        assert!(!should_halve(TargetSize::new(100, 100), 0, 0));
    }
}
