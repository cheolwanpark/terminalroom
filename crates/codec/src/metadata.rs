use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use exif::{Exif, In, Reader, Tag, Value};
use libraw_rs::ShotInfo;

use crate::DecodeError;

/// Parsed EXIF data from an image-format file (JPEG/PNG/TIFF).
#[derive(Debug)]
pub(crate) struct ImageExif {
    pub shot: ShotInfo,
    /// EXIF Orientation tag value (1..=8). Defaults to 1 (no transform) when absent.
    pub orientation: u16,
}

impl Default for ImageExif {
    fn default() -> Self {
        Self {
            shot: ShotInfo::default(),
            orientation: 1,
        }
    }
}

/// Read EXIF metadata from an image file. Missing or unparseable EXIF returns
/// the default (empty shot-info, orientation=1) — not an error.
pub(crate) fn read_image_exif(path: &Path) -> Result<ImageExif, DecodeError> {
    let file = File::open(path).map_err(DecodeError::Io)?;
    let mut reader = BufReader::new(file);
    let exif = match Reader::new().read_from_container(&mut reader) {
        Ok(e) => e,
        Err(_) => return Ok(ImageExif::default()),
    };

    Ok(ImageExif {
        shot: read_shot_info(&exif),
        orientation: read_orientation(&exif).unwrap_or(1),
    })
}

fn read_shot_info(exif: &Exif) -> ShotInfo {
    let make = string_field(exif, Tag::Make);
    let model = string_field(exif, Tag::Model);
    let iso = exif
        .get_field(Tag::PhotographicSensitivity, In::PRIMARY)
        .or_else(|| exif.get_field(Tag::ISOSpeed, In::PRIMARY))
        .and_then(|f| f.value.get_uint(0))
        .map(|v| v as f32);
    let shutter = rational_field(exif, Tag::ExposureTime);
    let aperture = rational_field(exif, Tag::FNumber);
    let focal_length = rational_field(exif, Tag::FocalLength);

    ShotInfo {
        make,
        model,
        iso,
        shutter,
        aperture,
        focal_length,
    }
}

fn read_orientation(exif: &Exif) -> Option<u16> {
    exif.get_field(Tag::Orientation, In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .map(|v| v as u16)
}

fn string_field(exif: &Exif, tag: Tag) -> Option<String> {
    let field = exif.get_field(tag, In::PRIMARY)?;
    if let Value::Ascii(ref entries) = field.value {
        let bytes = entries.first()?;
        let s = String::from_utf8_lossy(bytes)
            .trim_end_matches('\0')
            .trim()
            .to_string();
        if s.is_empty() { None } else { Some(s) }
    } else {
        None
    }
}

fn rational_field(exif: &Exif, tag: Tag) -> Option<f32> {
    let field = exif.get_field(tag, In::PRIMARY)?;
    match field.value {
        Value::Rational(ref v) => v.first().map(|r| r.to_f64() as f32),
        Value::SRational(ref v) => v.first().map(|r| r.to_f64() as f32),
        _ => None,
    }
}
