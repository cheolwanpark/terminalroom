use std::ffi::{CStr, CString, c_char, c_double, c_float, c_int, c_uint, c_ushort};
use std::fmt;
use std::mem::size_of;
use std::path::Path;
use std::ptr::NonNull;
use std::slice;
use std::sync::atomic::{AtomicBool, Ordering};

pub type Result<T> = std::result::Result<T, Error>;

/// Output color space for `read_demosaiced`. Matches LibRaw's `output_color` codes.
///
/// `Raw` (code 0) skips the camera → output matrix entirely. In Terminalroom's
/// RAW-develop path we also disable LibRaw's `scale_colors()` step, so the
/// caller receives a black-subtracted, unbalanced demosaiced buffer and owns
/// white balance plus camera→working conversion end-to-end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputColorSpace {
    Raw,
    Srgb,
    AdobeRgb,
    WideGamut,
    ProPhoto,
    Xyz,
    Aces,
    DciP3,
    Rec2020,
}

impl OutputColorSpace {
    fn as_libraw_code(self) -> c_int {
        match self {
            OutputColorSpace::Raw => 0,
            OutputColorSpace::Srgb => 1,
            OutputColorSpace::AdobeRgb => 2,
            OutputColorSpace::WideGamut => 3,
            OutputColorSpace::ProPhoto => 4,
            OutputColorSpace::Xyz => 5,
            OutputColorSpace::Aces => 6,
            OutputColorSpace::DciP3 => 7,
            OutputColorSpace::Rec2020 => 8,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShotInfo {
    pub make: Option<String>,
    pub model: Option<String>,
    pub iso: Option<f32>,
    /// Exposure time in seconds.
    pub shutter: Option<f32>,
    /// f-number.
    pub aperture: Option<f32>,
    /// Focal length in millimetres.
    pub focal_length: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// CFA (color filter array) layout. Bayer is the common case; X-Trans is Fuji.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CfaPattern {
    /// Bayer-style mosaic. `filters` is libraw's packed 32-bit pattern; `cdesc`
    /// is the per-channel color label ordering (e.g. "RGBG").
    Bayer { filters: u32, cdesc: String },
    /// Fuji X-Trans. `abs` is the 6x6 absolute pattern; values are color indices
    /// into `cdesc`.
    XTrans { abs: [[u8; 6]; 6], cdesc: String },
    /// Monochrome (no CFA).
    Mono,
    /// Anything else libraw exposes (foveon-style, four-color CFAs, etc.).
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlackLevel {
    /// Global black level (libraw's `color.black`).
    pub global: u32,
    /// Per-channel offsets (libraw's `color.cblack[0..4]`), in CFA order.
    pub per_channel: [u32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WhiteLevel {
    /// Saturation level after black-level subtraction (libraw's `color.maximum`).
    pub saturation: u32,
    /// Per-channel saturation (libraw's `color.linear_max[0..4]`); zero if unset.
    pub per_channel: [u32; 4],
}

/// Sensor metadata exposed for develop pipelines. All values are read from
/// libraw immediately after `libraw_open_file`; no `unpack` is required.
#[derive(Debug, Clone, PartialEq)]
pub struct SensorInfo {
    pub raw_width: u32,
    pub raw_height: u32,
    /// Active image area inside the raw frame.
    pub active_area: Rect,
    /// Camera-suggested crop (libraw `raw_inset_crops[0]`); `None` on libraw < 0.21
    /// or when the camera did not write one.
    pub crop_area: Option<Rect>,
    /// LibRaw flip code: 0 = no rotation, 3 = 180°, 5 = 90° CCW, 6 = 90° CW.
    pub orientation: i32,
    pub cfa: CfaPattern,
    pub black_level: BlackLevel,
    pub white_level: WhiteLevel,
    /// As-shot white-balance multipliers (libraw `cam_mul[0..4]`), in CFA order.
    pub camera_wb: [f32; 4],
    /// Daylight white-balance multipliers (libraw `pre_mul[0..4]`).
    pub daylight_wb: [f32; 4],
    /// LibRaw `cam_xyz[0..4][0..3]`, which maps XYZ → camera space.
    /// Row-major; rows correspond to camera channels (typically 4 for CMYG
    /// sensors, 3 used otherwise). Bayer cameras usually expose a 3×3 RGB
    /// subset with the fourth row unused or zeroed.
    pub cam_to_xyz: [[f32; 3]; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemosaicOptions {
    pub output_color: OutputColorSpace,
    /// When true, libraw subsamples each dimension by 2 (1/4 pixel count).
    pub half_size: bool,
    /// When true, apply the camera-stored white balance.
    pub use_camera_wb: bool,
    /// Demosaic interpolation: 0 = bilinear (fast), 1 = VNG, 2 = PPG, 3 = AHD.
    pub user_qual: u8,
}

impl Default for DemosaicOptions {
    fn default() -> Self {
        Self {
            output_color: OutputColorSpace::Rec2020,
            half_size: false,
            use_camera_wb: true,
            user_qual: 3,
        }
    }
}

/// Demosaiced linear image: 3-channel RGB, gamma 1.0, 16 bits per channel,
/// row-major, host byte order. Color space follows `color_space`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemosaicedRaw {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u16>,
    pub color_space: OutputColorSpace,
}

#[derive(Debug)]
pub enum Error {
    PathNotUtf8,
    PathContainsNul,
    InitFailed,
    OpenFailed { code: i32, message: String },
    UnpackFailed { code: i32, message: String },
    ProcessFailed { code: i32, message: String },
    MakeMemImageFailed { code: i32, message: String },
    UnsupportedOutput { colors: u8, bits_per_channel: u8 },
    BufferTooSmall { expected: usize, got: usize },
    Cancelled,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::PathNotUtf8 => write!(f, "RAW path is not valid UTF-8"),
            Error::PathContainsNul => write!(f, "RAW path contains a NUL byte"),
            Error::InitFailed => write!(f, "failed to initialize LibRaw"),
            Error::OpenFailed { code, message } => {
                write!(f, "LibRaw failed to open file ({code}): {message}")
            }
            Error::UnpackFailed { code, message } => {
                write!(f, "LibRaw failed to unpack RAW ({code}): {message}")
            }
            Error::ProcessFailed { code, message } => {
                write!(f, "LibRaw failed to process RAW ({code}): {message}")
            }
            Error::MakeMemImageFailed { code, message } => {
                write!(
                    f,
                    "LibRaw failed to materialize processed image ({code}): {message}"
                )
            }
            Error::UnsupportedOutput {
                colors,
                bits_per_channel,
            } => write!(
                f,
                "LibRaw produced an unsupported layout: {colors} ch, {bits_per_channel} bpc"
            ),
            Error::BufferTooSmall { expected, got } => write!(
                f,
                "LibRaw output buffer too small: expected {expected} bytes, got {got}"
            ),
            Error::Cancelled => write!(f, "RAW decode cancelled"),
        }
    }
}

impl std::error::Error for Error {}

/// Read shot-info and sensor metadata without unpacking the raw image. This is
/// the cheap header read used for culling and any UI that lists files.
pub fn read_header(path: &Path) -> Result<(ShotInfo, SensorInfo)> {
    let handle = RawHandle::open(path)?;
    Ok(unsafe { read_header_from_handle(&handle) })
}

/// Demosaic the raw image to a linear RGB buffer in the chosen output color space.
/// `output_bps=16`, `gamma=1.0`, `no_auto_bright=1` are always set.
pub fn read_demosaiced(
    path: &Path,
    opts: &DemosaicOptions,
    cancel: Option<&AtomicBool>,
) -> Result<DemosaicedRaw> {
    check_cancel(cancel)?;
    let handle = RawHandle::open(path)?;

    unsafe {
        ffi::libraw_set_output_bps(handle.as_ptr(), 16);
        ffi::libraw_set_gamma(handle.as_ptr(), 0, 1.0);
        ffi::libraw_set_gamma(handle.as_ptr(), 1, 1.0);
        ffi::libraw_set_no_auto_bright(handle.as_ptr(), 1);
        ffi::libraw_set_output_color(handle.as_ptr(), opts.output_color.as_libraw_code());
        ffi::libraw_set_demosaic(handle.as_ptr(), opts.user_qual as c_int);
        ffi::tr_set_half_size(handle.as_ptr(), opts.half_size as c_int);
        ffi::tr_set_use_camera_wb(handle.as_ptr(), opts.use_camera_wb as c_int);
        ffi::tr_set_no_auto_scale(
            handle.as_ptr(),
            matches!(opts.output_color, OutputColorSpace::Raw) as c_int,
        );
    }

    check_cancel(cancel)?;
    let code = unsafe { ffi::libraw_unpack(handle.as_ptr()) };
    if code != 0 {
        return Err(Error::UnpackFailed {
            code,
            message: libraw_error_message(code),
        });
    }

    check_cancel(cancel)?;
    let code = unsafe { ffi::libraw_dcraw_process(handle.as_ptr()) };
    if code != 0 {
        return Err(Error::ProcessFailed {
            code,
            message: libraw_error_message(code),
        });
    }

    check_cancel(cancel)?;
    let mut errc: c_int = 0;
    let raw = unsafe { ffi::libraw_dcraw_make_mem_image(handle.as_ptr(), &mut errc) };
    let img = NonNull::new(raw)
        .map(|ptr| OwnedProcessedImage { ptr })
        .ok_or_else(|| Error::MakeMemImageFailed {
            code: errc,
            message: libraw_error_message(errc),
        })?;

    let header = unsafe { img.header() };
    if header.colors != 3 || header.bits != 16 {
        return Err(Error::UnsupportedOutput {
            colors: header.colors as u8,
            bits_per_channel: header.bits as u8,
        });
    }

    let width = header.width as u32;
    let height = header.height as u32;
    let expected_pixels = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(3);
    let expected_bytes = expected_pixels.saturating_mul(2);
    let bytes = unsafe { img.bytes() };
    if bytes.len() < expected_bytes {
        return Err(Error::BufferTooSmall {
            expected: expected_bytes,
            got: bytes.len(),
        });
    }

    let mut pixels = Vec::with_capacity(expected_pixels);
    let src = &bytes[..expected_bytes];
    for chunk in src.chunks_exact(2) {
        pixels.push(u16::from_ne_bytes([chunk[0], chunk[1]]));
    }

    Ok(DemosaicedRaw {
        width,
        height,
        pixels,
        color_space: opts.output_color,
    })
}

fn check_cancel(cancel: Option<&AtomicBool>) -> Result<()> {
    if let Some(flag) = cancel
        && flag.load(Ordering::Relaxed)
    {
        return Err(Error::Cancelled);
    }
    Ok(())
}

unsafe fn read_header_from_handle(handle: &RawHandle) -> (ShotInfo, SensorInfo) {
    let p = handle.as_ptr();

    let make = c_str_ptr_to_string(unsafe { ffi::tr_get_make(p) });
    let model = c_str_ptr_to_string(unsafe { ffi::tr_get_model(p) });
    let iso = some_if_positive(unsafe { ffi::tr_get_iso(p) });
    let shutter = some_if_positive(unsafe { ffi::tr_get_shutter(p) });
    let aperture = some_if_positive(unsafe { ffi::tr_get_aperture(p) });
    let focal_length = some_if_positive(unsafe { ffi::tr_get_focal_len(p) });

    let shot = ShotInfo {
        make,
        model,
        iso,
        shutter,
        aperture,
        focal_length,
    };

    let raw_width = unsafe { ffi::tr_get_raw_width(p) };
    let raw_height = unsafe { ffi::tr_get_raw_height(p) };
    let active_area = Rect {
        x: unsafe { ffi::tr_get_left_margin(p) },
        y: unsafe { ffi::tr_get_top_margin(p) },
        width: unsafe { ffi::tr_get_active_width(p) },
        height: unsafe { ffi::tr_get_active_height(p) },
    };
    let crop_area = unsafe { read_inset_crop(p, 0) };
    let orientation = unsafe { ffi::tr_get_flip(p) };

    let cfa = unsafe { read_cfa_pattern(p) };

    let black_level = BlackLevel {
        global: unsafe { ffi::tr_get_black(p) },
        per_channel: [
            unsafe { ffi::tr_get_cblack(p, 0) },
            unsafe { ffi::tr_get_cblack(p, 1) },
            unsafe { ffi::tr_get_cblack(p, 2) },
            unsafe { ffi::tr_get_cblack(p, 3) },
        ],
    };

    let white_level = WhiteLevel {
        saturation: unsafe { ffi::tr_get_maximum(p) },
        per_channel: [
            unsafe { ffi::tr_get_linear_max(p, 0) }.max(0) as u32,
            unsafe { ffi::tr_get_linear_max(p, 1) }.max(0) as u32,
            unsafe { ffi::tr_get_linear_max(p, 2) }.max(0) as u32,
            unsafe { ffi::tr_get_linear_max(p, 3) }.max(0) as u32,
        ],
    };

    let camera_wb = [
        unsafe { ffi::tr_get_cam_mul(p, 0) },
        unsafe { ffi::tr_get_cam_mul(p, 1) },
        unsafe { ffi::tr_get_cam_mul(p, 2) },
        unsafe { ffi::tr_get_cam_mul(p, 3) },
    ];
    let daylight_wb = [
        unsafe { ffi::tr_get_pre_mul(p, 0) },
        unsafe { ffi::tr_get_pre_mul(p, 1) },
        unsafe { ffi::tr_get_pre_mul(p, 2) },
        unsafe { ffi::tr_get_pre_mul(p, 3) },
    ];

    let mut cam_to_xyz = [[0.0_f32; 3]; 4];
    for (row, dst) in cam_to_xyz.iter_mut().enumerate() {
        for (col, slot) in dst.iter_mut().enumerate() {
            *slot = unsafe { ffi::tr_get_cam_xyz(p, row as c_int, col as c_int) } as f32;
        }
    }

    let sensor = SensorInfo {
        raw_width,
        raw_height,
        active_area,
        crop_area,
        orientation,
        cfa,
        black_level,
        white_level,
        camera_wb,
        daylight_wb,
        cam_to_xyz,
    };

    (shot, sensor)
}

unsafe fn read_inset_crop(p: *mut ffi::libraw_data_t, idx: c_int) -> Option<Rect> {
    let mut cleft: c_uint = 0;
    let mut ctop: c_uint = 0;
    let mut cwidth: c_uint = 0;
    let mut cheight: c_uint = 0;
    let ok = unsafe {
        ffi::tr_get_raw_inset_crop(p, idx, &mut cleft, &mut ctop, &mut cwidth, &mut cheight)
    };
    if ok == 0 || cwidth == 0 || cheight == 0 {
        None
    } else {
        Some(Rect {
            x: cleft,
            y: ctop,
            width: cwidth,
            height: cheight,
        })
    }
}

unsafe fn read_cfa_pattern(p: *mut ffi::libraw_data_t) -> CfaPattern {
    let filters = unsafe { ffi::tr_get_filters(p) };
    let colors = unsafe { ffi::tr_get_colors(p) };
    let cdesc = c_str_ptr_to_string(unsafe { ffi::tr_get_cdesc(p) }).unwrap_or_default();

    if colors == 1 {
        return CfaPattern::Mono;
    }

    if filters == 9 {
        let mut abs = [[0u8; 6]; 6];
        for (row, dst_row) in abs.iter_mut().enumerate() {
            for (col, slot) in dst_row.iter_mut().enumerate() {
                *slot = unsafe { ffi::tr_get_xtrans_abs(p, row as c_int, col as c_int) } as u8;
            }
        }
        return CfaPattern::XTrans { abs, cdesc };
    }

    if filters == 0 {
        return CfaPattern::Other;
    }

    CfaPattern::Bayer { filters, cdesc }
}

fn c_str_ptr_to_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let cstr = unsafe { CStr::from_ptr(ptr) };
    let bytes = cstr.to_bytes();
    if bytes.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(bytes).into_owned())
}

fn some_if_positive(value: c_float) -> Option<f32> {
    if value.is_finite() && value > 0.0 {
        Some(value)
    } else {
        None
    }
}

#[derive(Debug)]
struct RawHandle {
    ptr: NonNull<ffi::libraw_data_t>,
}

impl RawHandle {
    fn open(path: &Path) -> Result<Self> {
        let path_str = path.to_str().ok_or(Error::PathNotUtf8)?;
        let path_c = CString::new(path_str).map_err(|_| Error::PathContainsNul)?;

        let ptr = unsafe { ffi::libraw_init(0) };
        let ptr = NonNull::new(ptr).ok_or(Error::InitFailed)?;
        let handle = Self { ptr };

        let code = unsafe { ffi::libraw_open_file(handle.as_ptr(), path_c.as_ptr()) };
        if code != 0 {
            return Err(Error::OpenFailed {
                code,
                message: libraw_error_message(code),
            });
        }

        Ok(handle)
    }

    fn as_ptr(&self) -> *mut ffi::libraw_data_t {
        self.ptr.as_ptr()
    }
}

impl Drop for RawHandle {
    fn drop(&mut self) {
        unsafe {
            ffi::libraw_close(self.ptr.as_ptr());
        }
    }
}

struct OwnedProcessedImage {
    ptr: NonNull<ffi::libraw_processed_image_t>,
}

impl OwnedProcessedImage {
    unsafe fn header(&self) -> &ffi::libraw_processed_image_t {
        unsafe { self.ptr.as_ref() }
    }

    unsafe fn bytes(&self) -> &[u8] {
        let header_ptr = self.ptr.as_ptr();
        let data_ptr =
            unsafe { (header_ptr as *const u8).add(size_of::<ffi::libraw_processed_image_t>()) };
        let len = unsafe { (*header_ptr).data_size } as usize;
        unsafe { slice::from_raw_parts(data_ptr, len) }
    }
}

impl Drop for OwnedProcessedImage {
    fn drop(&mut self) {
        unsafe {
            ffi::libraw_dcraw_clear_mem(self.ptr.as_ptr());
        }
    }
}

fn libraw_error_message(code: i32) -> String {
    let message = unsafe { ffi::libraw_strerror(code) };
    if message.is_null() {
        return "unknown LibRaw error".to_string();
    }

    unsafe { CStr::from_ptr(message) }
        .to_string_lossy()
        .into_owned()
}

mod ffi {
    use super::{c_char, c_double, c_float, c_int, c_uint, c_ushort};

    #[allow(non_camel_case_types)]
    pub enum libraw_data_t {}

    #[repr(C)]
    #[allow(non_camel_case_types)]
    pub struct libraw_processed_image_t {
        pub type_: c_int,
        pub height: c_ushort,
        pub width: c_ushort,
        pub colors: c_ushort,
        pub bits: c_ushort,
        pub data_size: c_uint,
    }

    unsafe extern "C" {
        pub fn libraw_init(flags: c_uint) -> *mut libraw_data_t;
        pub fn libraw_open_file(data: *mut libraw_data_t, file: *const c_char) -> c_int;
        pub fn libraw_close(data: *mut libraw_data_t);
        pub fn libraw_strerror(error_code: c_int) -> *const c_char;
        #[allow(dead_code)] // exercised by FFI smoke tests only
        pub fn libraw_version() -> *const c_char;

        pub fn libraw_unpack(data: *mut libraw_data_t) -> c_int;
        pub fn libraw_dcraw_process(data: *mut libraw_data_t) -> c_int;
        pub fn libraw_dcraw_make_mem_image(
            data: *mut libraw_data_t,
            errc: *mut c_int,
        ) -> *mut libraw_processed_image_t;
        pub fn libraw_dcraw_clear_mem(img: *mut libraw_processed_image_t);

        pub fn libraw_set_demosaic(data: *mut libraw_data_t, value: c_int);
        pub fn libraw_set_output_color(data: *mut libraw_data_t, value: c_int);
        pub fn libraw_set_output_bps(data: *mut libraw_data_t, value: c_int);
        pub fn libraw_set_gamma(data: *mut libraw_data_t, index: c_int, value: c_float);
        pub fn libraw_set_no_auto_bright(data: *mut libraw_data_t, value: c_int);

        // Custom C wrappers (wrapper.c).
        pub fn tr_set_half_size(data: *mut libraw_data_t, value: c_int);
        pub fn tr_set_use_camera_wb(data: *mut libraw_data_t, value: c_int);
        pub fn tr_set_no_auto_scale(data: *mut libraw_data_t, value: c_int);

        pub fn tr_get_make(data: *mut libraw_data_t) -> *const c_char;
        pub fn tr_get_model(data: *mut libraw_data_t) -> *const c_char;
        pub fn tr_get_iso(data: *mut libraw_data_t) -> c_float;
        pub fn tr_get_shutter(data: *mut libraw_data_t) -> c_float;
        pub fn tr_get_aperture(data: *mut libraw_data_t) -> c_float;
        pub fn tr_get_focal_len(data: *mut libraw_data_t) -> c_float;

        pub fn tr_get_raw_width(data: *mut libraw_data_t) -> c_uint;
        pub fn tr_get_raw_height(data: *mut libraw_data_t) -> c_uint;
        pub fn tr_get_active_width(data: *mut libraw_data_t) -> c_uint;
        pub fn tr_get_active_height(data: *mut libraw_data_t) -> c_uint;
        pub fn tr_get_top_margin(data: *mut libraw_data_t) -> c_uint;
        pub fn tr_get_left_margin(data: *mut libraw_data_t) -> c_uint;
        pub fn tr_get_flip(data: *mut libraw_data_t) -> c_int;
        pub fn tr_get_raw_inset_crop(
            data: *mut libraw_data_t,
            idx: c_int,
            out_cleft: *mut c_uint,
            out_ctop: *mut c_uint,
            out_cwidth: *mut c_uint,
            out_cheight: *mut c_uint,
        ) -> c_int;

        pub fn tr_get_filters(data: *mut libraw_data_t) -> c_uint;
        pub fn tr_get_cdesc(data: *mut libraw_data_t) -> *const c_char;
        pub fn tr_get_colors(data: *mut libraw_data_t) -> c_uint;
        pub fn tr_get_xtrans_abs(data: *mut libraw_data_t, row: c_int, col: c_int) -> c_int;

        pub fn tr_get_black(data: *mut libraw_data_t) -> c_uint;
        pub fn tr_get_cblack(data: *mut libraw_data_t, idx: c_int) -> c_uint;
        pub fn tr_get_maximum(data: *mut libraw_data_t) -> c_uint;
        #[allow(dead_code)] // available for advanced develop; not used yet
        pub fn tr_get_data_maximum(data: *mut libraw_data_t) -> c_uint;
        pub fn tr_get_linear_max(data: *mut libraw_data_t, idx: c_int) -> c_int;

        pub fn tr_get_cam_mul(data: *mut libraw_data_t, idx: c_int) -> c_float;
        pub fn tr_get_pre_mul(data: *mut libraw_data_t, idx: c_int) -> c_float;
        pub fn tr_get_cam_xyz(data: *mut libraw_data_t, row: c_int, col: c_int) -> c_double;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libraw_version_returns_nonempty_string() {
        let ptr = unsafe { ffi::libraw_version() };
        assert!(!ptr.is_null(), "libraw_version returned null");
        let version = unsafe { CStr::from_ptr(ptr) }
            .to_str()
            .expect("libraw version is valid UTF-8");
        assert!(!version.is_empty(), "libraw version string is empty");
    }

    #[test]
    fn libraw_init_and_close_roundtrip() {
        let ptr = unsafe { ffi::libraw_init(0) };
        assert!(!ptr.is_null(), "libraw_init(0) returned null");
        unsafe { ffi::libraw_close(ptr) };
    }

    #[test]
    fn libraw_strerror_returns_message_for_known_codes() {
        for code in [0, -1, -2, -100] {
            let msg = unsafe { ffi::libraw_strerror(code) };
            assert!(!msg.is_null(), "libraw_strerror({code}) returned null");
            let s = unsafe { CStr::from_ptr(msg) }.to_string_lossy();
            assert!(!s.is_empty(), "libraw_strerror({code}) returned empty");
        }
    }

    #[test]
    fn raw_handle_rejects_path_with_nul() {
        let path = Path::new("foo\0bar.raw");
        let err = RawHandle::open(path).unwrap_err();
        assert!(matches!(err, Error::PathContainsNul), "got {err:?}");
    }

    #[test]
    fn read_header_on_missing_file_returns_open_error() {
        let path = missing_path();
        let err = read_header(&path).unwrap_err();
        assert!(matches!(err, Error::OpenFailed { .. }), "got {err:?}");
    }

    #[test]
    fn read_demosaiced_on_missing_file_returns_open_error() {
        let path = missing_path();
        let err = read_demosaiced(&path, &DemosaicOptions::default(), None).unwrap_err();
        assert!(matches!(err, Error::OpenFailed { .. }), "got {err:?}");
    }

    #[test]
    fn read_demosaiced_pre_open_cancel_returns_cancelled() {
        let path = missing_path();
        let flag = AtomicBool::new(true);
        let err = read_demosaiced(&path, &DemosaicOptions::default(), Some(&flag)).unwrap_err();
        assert!(matches!(err, Error::Cancelled), "got {err:?}");
    }

    #[test]
    fn output_color_codes_match_libraw() {
        assert_eq!(OutputColorSpace::Raw.as_libraw_code(), 0);
        assert_eq!(OutputColorSpace::Srgb.as_libraw_code(), 1);
        assert_eq!(OutputColorSpace::AdobeRgb.as_libraw_code(), 2);
        assert_eq!(OutputColorSpace::Rec2020.as_libraw_code(), 8);
    }

    #[test]
    fn libraw_setters_smoke_test() {
        let ptr = unsafe { ffi::libraw_init(0) };
        assert!(!ptr.is_null());
        unsafe {
            ffi::libraw_set_output_bps(ptr, 16);
            ffi::libraw_set_gamma(ptr, 0, 1.0);
            ffi::libraw_set_gamma(ptr, 1, 1.0);
            ffi::libraw_set_no_auto_bright(ptr, 1);
            ffi::libraw_set_output_color(ptr, OutputColorSpace::Rec2020.as_libraw_code());
            ffi::libraw_set_demosaic(ptr, 0);
            ffi::tr_set_half_size(ptr, 1);
            ffi::tr_set_use_camera_wb(ptr, 1);
            ffi::tr_set_no_auto_scale(ptr, 1);
            ffi::libraw_close(ptr);
        }
    }

    #[test]
    fn libraw_getter_wrappers_link_on_fresh_handle() {
        // All accessors should be linkable and safe to call on a fresh, unopened
        // handle. Values are zero / empty but no crash.
        let ptr = unsafe { ffi::libraw_init(0) };
        assert!(!ptr.is_null());
        unsafe {
            let _ = ffi::tr_get_make(ptr);
            let _ = ffi::tr_get_model(ptr);
            let _ = ffi::tr_get_iso(ptr);
            let _ = ffi::tr_get_shutter(ptr);
            let _ = ffi::tr_get_aperture(ptr);
            let _ = ffi::tr_get_focal_len(ptr);
            let _ = ffi::tr_get_raw_width(ptr);
            let _ = ffi::tr_get_raw_height(ptr);
            let _ = ffi::tr_get_active_width(ptr);
            let _ = ffi::tr_get_active_height(ptr);
            let _ = ffi::tr_get_top_margin(ptr);
            let _ = ffi::tr_get_left_margin(ptr);
            let _ = ffi::tr_get_flip(ptr);
            let _ = ffi::tr_get_filters(ptr);
            let _ = ffi::tr_get_cdesc(ptr);
            let _ = ffi::tr_get_colors(ptr);
            let _ = ffi::tr_get_xtrans_abs(ptr, 0, 0);
            let _ = ffi::tr_get_black(ptr);
            let _ = ffi::tr_get_cblack(ptr, 0);
            let _ = ffi::tr_get_maximum(ptr);
            let _ = ffi::tr_get_data_maximum(ptr);
            let _ = ffi::tr_get_linear_max(ptr, 0);
            let _ = ffi::tr_get_cam_mul(ptr, 0);
            let _ = ffi::tr_get_pre_mul(ptr, 0);
            let _ = ffi::tr_get_cam_xyz(ptr, 0, 0);

            let mut a: c_uint = 0;
            let mut b: c_uint = 0;
            let mut c: c_uint = 0;
            let mut d: c_uint = 0;
            let _ = ffi::tr_get_raw_inset_crop(ptr, 0, &mut a, &mut b, &mut c, &mut d);

            ffi::libraw_close(ptr);
        }
    }

    #[test]
    fn processed_image_header_size_matches_c_offsetof_data() {
        assert_eq!(size_of::<ffi::libraw_processed_image_t>(), 16);
    }

    fn missing_path() -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push("__terminalroom_definitely_not_a_real_raw_file__.raw");
        path
    }
}
