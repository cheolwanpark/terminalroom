pub mod common;
pub mod image_develop;
pub mod raw_develop;
pub mod thumbnail;

use std::sync::atomic::AtomicBool;

pub use common::{DevelopError, RgbImage};
pub use thumbnail::develop_thumbnail;

pub use codec::{
    BlackLevel, CfaPattern, DecodeError, Image, ImageKind, LinearRec2020Pixels, Loaded,
    OutputColorSpace, Raw, Rect, SensorInfo, ShotInfo, Srgb8Pixels, TargetSize, Thumbnail,
    WhiteLevel, classify, decode, decode_image, decode_raw, read_image_pixels, read_raw_pixels,
};

/// Develop a `Loaded` to a culling-view `RgbImage`. Uses the eagerly-cached RAW
/// thumbnail as the fast path; falls back to the full image/raw develop pipeline
/// when no thumbnail is available (always for image-format files; rare for RAW).
pub fn develop_culling(
    loaded: &Loaded,
    target: TargetSize,
    cancel: Option<&AtomicBool>,
) -> Result<RgbImage, DevelopError> {
    if let Some(rgb) = develop_thumbnail(loaded, target) {
        return Ok(rgb);
    }
    match loaded {
        Loaded::Image(img) => image_develop::develop(img, target, cancel),
        Loaded::Raw(raw) => raw_develop::develop(raw, target, cancel),
    }
}
