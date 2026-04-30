use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

mod decode_image;
mod decode_raw;
mod format;
mod jpeg;
mod metadata;

pub use decode_image::{Image, decode_image, read_image_pixels};
pub use decode_raw::{Raw, decode_raw, read_camera_linear};
pub use format::{ImageKind, classify};
pub use libraw_rs::{
    BlackLevel, CfaPattern, OutputColorSpace, Rect, SensorInfo, ShotInfo, WhiteLevel,
};

/// Lazily-decoded sRGB 8-bit pixel buffer for an image-format file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Srgb8Pixels {
    pub width: u32,
    pub height: u32,
    /// Row-major sRGB 8-bit RGB. `data.len() == width * height * 3`.
    pub data: Vec<u8>,
}

/// Lazily-decoded camera-linear pixel buffer. Output of libraw with
/// `output_color = Raw` and `use_camera_wb = false`: no color matrix applied,
/// no white balance. Layout is **planar** f32 (`R..R G..G B..B`) so it drops
/// directly into the develop pipeline's planar SIMD kernels. Values are in
/// [0, ~1] (libraw's u16 output divided by 65535; per-channel can exceed 1.0
/// after subsequent gain).
#[derive(Debug, Clone, PartialEq)]
pub struct CameraLinearPixels {
    pub width: u32,
    pub height: u32,
    /// Planar f32 RGB. `data.len() == 3 * width * height`.
    pub data: Vec<f32>,
}

/// Target preview size; pixel buffers are decoded at or below this size.
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

/// Header-level result of [`decode`]: either an image-format or a RAW file.
#[derive(Debug, Clone)]
pub enum Loaded {
    Image(Image),
    Raw(Raw),
}

impl Loaded {
    pub fn source(&self) -> &Path {
        match self {
            Self::Image(i) => &i.source,
            Self::Raw(r) => &r.source,
        }
    }

    pub fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Image(i) => (i.width, i.height),
            Self::Raw(r) => (r.width, r.height),
        }
    }
}

/// Open a file and read its header. Dispatches to [`decode_image`] or
/// [`decode_raw`] based on the extension.
pub fn decode(path: &Path) -> Result<Loaded, DecodeError> {
    match path.extension().and_then(|s| s.to_str()).and_then(classify) {
        Some(ImageKind::Raw) => decode_raw(path).map(Loaded::Raw),
        Some(ImageKind::Jpeg | ImageKind::Png | ImageKind::Tiff) => {
            decode_image(path).map(Loaded::Image)
        }
        None => Err(DecodeError::UnsupportedExtension),
    }
}

#[derive(Debug)]
pub enum DecodeError {
    Io(std::io::Error),
    Image(image::ImageError),
    Jpeg(jpeg_decoder::Error),
    Raw(libraw_rs::Error),
    UnsupportedJpegPixelFormat(jpeg_decoder::PixelFormat),
    UnsupportedExtension,
    WrongKind { expected: &'static str },
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
            Self::UnsupportedExtension => write!(f, "unsupported file extension"),
            Self::WrongKind { expected } => {
                write!(f, "decoder mismatch: expected {expected} file")
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

pub(crate) fn check_cancel(cancel: Option<&AtomicBool>) -> Result<(), DecodeError> {
    if let Some(flag) = cancel
        && flag.load(Ordering::Relaxed)
    {
        return Err(DecodeError::Cancelled);
    }
    Ok(())
}
