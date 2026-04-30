//! Shared JPEG decoder helpers used by both `decode_image` (standalone JPEG +
//! IFD1 thumbnails) and `decode_raw` (camera-embedded thumbnails).

use std::io::{Cursor, Read};
use std::sync::atomic::AtomicBool;

use image::metadata::Orientation;
use image::{DynamicImage, ImageBuffer, Rgb};

use crate::metadata::parse_orientation_from_tiff_chunk;
use crate::{DecodeError, Srgb8Pixels, TargetSize, Thumbnail, check_cancel};

/// Decode JPEG bytes at native size and apply orientation. The orientation is
/// read from the JPEG's own EXIF tag if present; otherwise `fallback_orientation`
/// is used (callers pass the parent file's orientation when the embedded JPEG
/// carries no EXIF — common for camera RAW thumbnails of older bodies).
pub(crate) fn decode_jpeg_bytes_to_thumbnail(
    bytes: &[u8],
    fallback_orientation: u16,
) -> Result<Thumbnail, DecodeError> {
    let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(bytes));
    decoder.read_info().map_err(DecodeError::Jpeg)?;
    let pixels = decoder.decode().map_err(DecodeError::Jpeg)?;
    let info = decoder.info().expect("info available after decode");
    let (width, height) = (info.width as u32, info.height as u32);
    let orientation = decoder
        .exif_data()
        .and_then(parse_orientation_from_tiff_chunk)
        .unwrap_or(fallback_orientation);
    let rgb = pixel_format_to_rgb(info.pixel_format, &pixels)?;
    let (w, h, data) = apply_orientation_to_rgb8(width, height, rgb, orientation);
    Ok(Thumbnail {
        width: w,
        height: h,
        pixels: data,
    })
}

/// Decode a JPEG stream at a target size using IDCT scale factors (1/2/4/8) for
/// speed, then apply orientation.
pub(crate) fn decode_jpeg_to_srgb8<R: Read>(
    reader: R,
    target: TargetSize,
    orientation: u16,
    cancel: Option<&AtomicBool>,
) -> Result<Srgb8Pixels, DecodeError> {
    let mut decoder = jpeg_decoder::Decoder::new(reader);
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
    let rgb = pixel_format_to_rgb(info.pixel_format, &pixels)?;
    let (w, h, data) = apply_orientation_to_rgb8(width, height, rgb, orientation);
    Ok(Srgb8Pixels {
        width: w,
        height: h,
        data,
    })
}

fn pixel_format_to_rgb(
    format: jpeg_decoder::PixelFormat,
    pixels: &[u8],
) -> Result<Vec<u8>, DecodeError> {
    Ok(match format {
        jpeg_decoder::PixelFormat::RGB24 => pixels.to_vec(),
        jpeg_decoder::PixelFormat::L8 => expand_l8_to_rgb(pixels),
        jpeg_decoder::PixelFormat::L16 => expand_l16_to_rgb(pixels),
        jpeg_decoder::PixelFormat::CMYK32 => cmyk32_to_rgb(pixels),
    })
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
        let y = chunk[1];
        out.extend_from_slice(&[y, y, y]);
    }
    out
}

fn cmyk32_to_rgb(cmyk: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity((cmyk.len() / 4) * 3);
    for chunk in cmyk.chunks_exact(4) {
        let (c, m, y, k) = (
            chunk[0] as u16,
            chunk[1] as u16,
            chunk[2] as u16,
            chunk[3] as u16,
        );
        let r = ((c * k) / 255) as u8;
        let g = ((m * k) / 255) as u8;
        let b = ((y * k) / 255) as u8;
        out.extend_from_slice(&[r, g, b]);
    }
    out
}

pub(crate) fn apply_orientation_to_rgb8(
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    orientation_code: u16,
) -> (u32, u32, Vec<u8>) {
    let orientation = exif_to_orientation(orientation_code);
    if orientation == Orientation::NoTransforms {
        return (width, height, pixels);
    }
    let buf = ImageBuffer::<Rgb<u8>, _>::from_raw(width, height, pixels)
        .expect("rgb buffer length must match width*height*3");
    let mut img = DynamicImage::ImageRgb8(buf);
    img.apply_orientation(orientation);
    (img.width(), img.height(), img.into_rgb8().into_raw())
}

fn exif_to_orientation(code: u16) -> Orientation {
    match code {
        2 => Orientation::FlipHorizontal,
        3 => Orientation::Rotate180,
        4 => Orientation::FlipVertical,
        5 => Orientation::Rotate90FlipH,
        6 => Orientation::Rotate90,
        7 => Orientation::Rotate270FlipH,
        8 => Orientation::Rotate270,
        _ => Orientation::NoTransforms,
    }
}

fn clamp_u16(v: u32) -> u16 {
    v.min(u16::MAX as u32) as u16
}

pub(crate) fn dims_after_orientation(width: u32, height: u32, orientation_code: u16) -> (u32, u32) {
    if matches!(orientation_code, 5 | 6 | 7 | 8) {
        (height, width)
    } else {
        (width, height)
    }
}
