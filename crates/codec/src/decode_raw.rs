use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use libraw_rs::{
    DemosaicOptions, OutputColorSpace, SensorInfo, ShotInfo, read_demosaiced, read_header,
};

use crate::format::ImageKind;
use crate::{CameraLinearPixels, DecodeError, check_cancel, classify};

/// Header-level info about a RAW file. Cheap to construct (no demosaic). The
/// heavy planar-f32 camera-linear buffer is loaded lazily via
/// [`read_camera_linear`].
#[derive(Debug, Clone)]
pub struct Raw {
    pub source: PathBuf,
    /// Active-area width as reported by libraw, after orientation.
    pub width: u32,
    pub height: u32,
    pub shot_info: ShotInfo,
    pub sensor_info: SensorInfo,
}

/// Open a RAW file and read its header. Does not unpack the main image data.
pub fn decode_raw(path: &Path) -> Result<Raw, DecodeError> {
    let kind = path.extension().and_then(|s| s.to_str()).and_then(classify);
    if kind != Some(ImageKind::Raw) {
        return Err(if kind.is_none() {
            DecodeError::UnsupportedExtension
        } else {
            DecodeError::WrongKind { expected: "RAW" }
        });
    }

    let (shot_info, sensor_info) = read_header(path).map_err(DecodeError::Raw)?;
    let (width, height) = active_dims_after_orientation(&sensor_info);

    Ok(Raw {
        source: path.to_path_buf(),
        width,
        height,
        shot_info,
        sensor_info,
    })
}

/// Demosaic the raw image with `output_color=Raw`, `use_camera_wb=false`, and
/// return planar f32 camera-linear RGB. The develop pipeline applies WB and
/// the camera → Rec.2020 matrix downstream (so Temperature/Tint can intercept
/// before WB).
///
/// `half_size` is the caller's choice — preview wants `true` for ~4× faster
/// demosaic; export wants `false`.
pub fn read_camera_linear(
    raw: &Raw,
    half_size: bool,
    cancel: Option<&AtomicBool>,
) -> Result<CameraLinearPixels, DecodeError> {
    check_cancel(cancel)?;
    let opts = DemosaicOptions {
        output_color: OutputColorSpace::Raw,
        half_size,
        use_camera_wb: false,
        user_qual: 0,
    };
    let demos = read_demosaiced(&raw.source, &opts, cancel).map_err(DecodeError::Raw)?;
    let plane = (demos.width as usize) * (demos.height as usize);
    let mut data = vec![0.0f32; plane * 3];
    let (r, gb) = data.split_at_mut(plane);
    let (g, b) = gb.split_at_mut(plane);
    let inv = 1.0_f32 / 65535.0;
    for (i, px) in demos.pixels.chunks_exact(3).enumerate() {
        r[i] = px[0] as f32 * inv;
        g[i] = px[1] as f32 * inv;
        b[i] = px[2] as f32 * inv;
    }
    Ok(CameraLinearPixels {
        width: demos.width,
        height: demos.height,
        data,
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
