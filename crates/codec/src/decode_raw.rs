use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use libraw_rs::{
    DemosaicOptions, OutputColorSpace, SensorInfo, ShotInfo, read_demosaiced, read_header,
    read_thumbnail,
};

use crate::format::ImageKind;
use crate::jpeg::decode_jpeg_bytes_to_thumbnail;
use crate::{DecodeError, LinearRec2020Pixels, TargetSize, Thumbnail, check_cancel, classify};

/// Header-level info about a RAW file. Holds an eagerly-decoded sRGB thumbnail
/// (from the camera-embedded JPEG, when present) plus full sensor metadata. The
/// heavy linear-Rec.2020 buffer is loaded lazily via [`read_raw_pixels`].
#[derive(Debug, Clone)]
pub struct Raw {
    pub source: PathBuf,
    /// Active-area width as reported by libraw, after orientation.
    pub width: u32,
    pub height: u32,
    pub shot_info: ShotInfo,
    pub sensor_info: SensorInfo,
    /// Decoded sRGB thumbnail from the camera-embedded JPEG, if any.
    pub preview: Option<Thumbnail>,
}

/// Open a RAW file, read its header and embedded thumbnail. Does not unpack
/// the main image data.
pub fn decode_raw(path: &Path) -> Result<Raw, DecodeError> {
    let kind = path
        .extension()
        .and_then(|s| s.to_str())
        .and_then(classify);
    if kind != Some(ImageKind::Raw) {
        return Err(if kind.is_none() {
            DecodeError::UnsupportedExtension
        } else {
            DecodeError::WrongKind { expected: "RAW" }
        });
    }

    let (shot_info, sensor_info) = read_header(path).map_err(DecodeError::Raw)?;
    let fallback_orientation = libraw_flip_to_exif_orientation(sensor_info.orientation);
    let preview = match read_thumbnail(path).map_err(DecodeError::Raw)? {
        Some(thumb) => {
            decode_jpeg_bytes_to_thumbnail(&thumb.jpeg_bytes, fallback_orientation).ok()
        }
        None => None,
    };

    let (width, height) = active_dims_after_orientation(&sensor_info);

    Ok(Raw {
        source: path.to_path_buf(),
        width,
        height,
        shot_info,
        sensor_info,
        preview,
    })
}

/// Demosaic the raw image and return linear Rec.2020 pixels (16 bpc, gamma 1.0)
/// at or below the target size.
pub fn read_raw_pixels(
    raw: &Raw,
    target: TargetSize,
    cancel: Option<&AtomicBool>,
) -> Result<LinearRec2020Pixels, DecodeError> {
    check_cancel(cancel)?;
    let half_size = should_halve(target, raw.sensor_info.raw_width, raw.sensor_info.raw_height);
    let opts = DemosaicOptions {
        output_color: OutputColorSpace::Rec2020,
        half_size,
        use_camera_wb: true,
        // Bilinear: ~2-4x faster than AHD on most sensors and good enough for
        // terminal-sized previews. Develop quality lives in the develop layer.
        user_qual: 0,
    };
    let demos = read_demosaiced(&raw.source, &opts, cancel).map_err(DecodeError::Raw)?;
    Ok(LinearRec2020Pixels {
        width: demos.width,
        height: demos.height,
        data: demos.pixels,
    })
}

fn active_dims_after_orientation(sensor: &SensorInfo) -> (u32, u32) {
    let (w, h) = (sensor.active_area.width, sensor.active_area.height);
    // libraw flip codes: 5 = 90° CCW, 6 = 90° CW. These swap dims.
    if matches!(sensor.orientation, 5 | 6) {
        (h, w)
    } else {
        (w, h)
    }
}

/// Map a libraw `flip` code to an EXIF Orientation tag value.
///
/// libraw flip semantics (matches dcraw): 0=none, 3=180°, 5=90° CCW, 6=90° CW.
/// EXIF: 1=none, 3=180°, 6=90° CW, 8=90° CCW.
fn libraw_flip_to_exif_orientation(flip: i32) -> u16 {
    match flip {
        3 => 3,
        5 => 8,
        6 => 6,
        _ => 1,
    }
}

fn should_halve(target: TargetSize, sensor_w: u32, sensor_h: u32) -> bool {
    if sensor_w == 0 || sensor_h == 0 {
        return false;
    }
    target.max_w.saturating_mul(2) <= sensor_w && target.max_h.saturating_mul(2) <= sensor_h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_halve_uses_target_vs_sensor() {
        assert!(should_halve(TargetSize::new(2000, 1500), 6000, 4000));
        assert!(!should_halve(TargetSize::new(3500, 1500), 6000, 4000));
        assert!(!should_halve(TargetSize::new(100, 100), 0, 0));
    }
}
