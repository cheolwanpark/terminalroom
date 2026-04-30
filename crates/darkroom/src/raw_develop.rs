use std::sync::atomic::AtomicBool;

use codec::{Raw, TargetSize, read_raw_pixels};

use crate::common::{
    DevelopError, RgbImage, apply_3x3_u16, check_cancel, fit_within, linear_to_srgb8,
    rec2020_to_srgb_matrix, resize_u16x3,
};

/// Develop a `Raw` into a display-ready sRGB 8-bit `RgbImage` at or below
/// `target`. Pipeline: read linear Rec.2020 pixels → resize in 16-bit linear →
/// apply Rec.2020 → sRGB primaries matrix (still linear) → encode sRGB transfer
/// curve via LUT to 8-bit.
pub fn develop(
    raw: &Raw,
    target: TargetSize,
    cancel: Option<&AtomicBool>,
) -> Result<RgbImage, DevelopError> {
    let pixels = read_raw_pixels(raw, target, cancel).map_err(DevelopError::Decode)?;
    check_cancel(cancel)?;

    let (dst_w, dst_h) = fit_within(pixels.width, pixels.height, target);
    let mut resized = if (dst_w, dst_h) == (pixels.width, pixels.height) {
        pixels.data
    } else {
        resize_u16x3(&pixels.data, pixels.width, pixels.height, dst_w, dst_h)?
    };
    check_cancel(cancel)?;

    apply_3x3_u16(&mut resized, &rec2020_to_srgb_matrix());
    check_cancel(cancel)?;

    let srgb = linear_to_srgb8(&resized);
    Ok(RgbImage {
        width: dst_w,
        height: dst_h,
        pixels: srgb,
    })
}
