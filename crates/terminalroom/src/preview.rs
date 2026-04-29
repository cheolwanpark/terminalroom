use std::fmt;

use image::{DynamicImage, ImageBuffer, ImageFormat, Rgb};
use libraw_rs::{PreviewFormat, PreviewImage};

#[derive(Debug)]
pub enum PreviewError {
    JpegDecode(image::ImageError),
    UnsupportedRgb { colors: u8, bits_per_channel: u8 },
    BufferTooSmall { expected: usize, got: usize },
}

impl fmt::Display for PreviewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::JpegDecode(e) => write!(f, "failed to decode JPEG preview: {e}"),
            Self::UnsupportedRgb {
                colors,
                bits_per_channel,
            } => write!(
                f,
                "unsupported RGB layout: {colors} channels, {bits_per_channel} bits per channel"
            ),
            Self::BufferTooSmall { expected, got } => write!(
                f,
                "preview buffer too small: expected {expected} bytes, got {got}"
            ),
        }
    }
}

impl std::error::Error for PreviewError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::JpegDecode(e) => Some(e),
            _ => None,
        }
    }
}

pub fn decode_preview(preview: PreviewImage) -> Result<DynamicImage, PreviewError> {
    match preview.format {
        PreviewFormat::Jpeg => {
            image::load_from_memory_with_format(&preview.bytes, ImageFormat::Jpeg)
                .map_err(PreviewError::JpegDecode)
        }
        PreviewFormat::Rgb8 {
            colors: 3,
            bits_per_channel: 8,
        } => {
            let expected = (preview.width as usize)
                .saturating_mul(preview.height as usize)
                .saturating_mul(3);
            if preview.bytes.len() < expected {
                return Err(PreviewError::BufferTooSmall {
                    expected,
                    got: preview.bytes.len(),
                });
            }
            let mut bytes = preview.bytes;
            bytes.truncate(expected);
            // ImageBuffer::from_raw returns None only if bytes.len() < w*h*channels;
            // we just truncated to exactly that length, so this is unreachable.
            let buf = ImageBuffer::<Rgb<u8>, _>::from_raw(preview.width, preview.height, bytes)
                .ok_or(PreviewError::BufferTooSmall {
                    expected,
                    got: expected,
                })?;
            Ok(DynamicImage::ImageRgb8(buf))
        }
        PreviewFormat::Rgb8 {
            colors,
            bits_per_channel,
        } => Err(PreviewError::UnsupportedRgb {
            colors,
            bits_per_channel,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::GenericImageView;
    use libraw_rs::{PreviewFormat, PreviewImage, PreviewSource};
    use std::io::Cursor;

    fn make_preview(
        format: PreviewFormat,
        width: u32,
        height: u32,
        bytes: Vec<u8>,
    ) -> PreviewImage {
        PreviewImage {
            width,
            height,
            bytes,
            format,
            source: PreviewSource::EmbeddedThumbnail,
        }
    }

    fn encode_jpeg_rgb(width: u32, height: u32, pixels: Vec<u8>) -> Vec<u8> {
        let buf = ImageBuffer::<Rgb<u8>, _>::from_raw(width, height, pixels).unwrap();
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(buf)
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Jpeg)
            .unwrap();
        out
    }

    #[test]
    fn decode_preview_jpeg_roundtrips_dimensions() {
        let pixels = vec![255u8; 4 * 4 * 3];
        let jpeg = encode_jpeg_rgb(4, 4, pixels);

        let p = make_preview(PreviewFormat::Jpeg, 4, 4, jpeg);
        let decoded = decode_preview(p).unwrap();
        assert_eq!(decoded.dimensions(), (4, 4));
    }

    #[test]
    fn decode_preview_jpeg_bad_bytes_errors() {
        let p = make_preview(PreviewFormat::Jpeg, 4, 4, vec![0x00, 0x01, 0x02, 0x03]);
        let err = decode_preview(p).unwrap_err();
        assert!(matches!(err, PreviewError::JpegDecode(_)));
    }

    #[test]
    fn decode_preview_rgb8_3ch_8bit_passes_through() {
        let mut bytes = Vec::with_capacity(2 * 2 * 3);
        for _ in 0..4 {
            bytes.extend_from_slice(&[255, 0, 0]);
        }
        let p = make_preview(
            PreviewFormat::Rgb8 {
                colors: 3,
                bits_per_channel: 8,
            },
            2,
            2,
            bytes,
        );
        let decoded = decode_preview(p).unwrap();
        assert_eq!(decoded.dimensions(), (2, 2));
        let pixel = decoded.get_pixel(0, 0);
        assert_eq!(pixel.0, [255, 0, 0, 255]);
    }

    #[test]
    fn decode_preview_rgb8_4ch_errors() {
        let p = make_preview(
            PreviewFormat::Rgb8 {
                colors: 4,
                bits_per_channel: 8,
            },
            1,
            1,
            vec![0; 4],
        );
        let err = decode_preview(p).unwrap_err();
        assert!(matches!(
            err,
            PreviewError::UnsupportedRgb { colors: 4, .. }
        ));
    }

    #[test]
    fn decode_preview_rgb8_16bit_errors() {
        let p = make_preview(
            PreviewFormat::Rgb8 {
                colors: 3,
                bits_per_channel: 16,
            },
            1,
            1,
            vec![0; 6],
        );
        let err = decode_preview(p).unwrap_err();
        assert!(matches!(
            err,
            PreviewError::UnsupportedRgb {
                bits_per_channel: 16,
                ..
            }
        ));
    }

    #[test]
    fn decode_preview_rgb8_truncated_buffer_errors() {
        let p = make_preview(
            PreviewFormat::Rgb8 {
                colors: 3,
                bits_per_channel: 8,
            },
            4,
            4,
            vec![0; 5],
        );
        let err = decode_preview(p).unwrap_err();
        assert!(matches!(err, PreviewError::BufferTooSmall { .. }));
    }
}
