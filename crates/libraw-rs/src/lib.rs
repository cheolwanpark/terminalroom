use std::ffi::{CStr, CString, c_char, c_double, c_int, c_uint, c_ushort};
use std::fmt;
use std::mem::size_of;
use std::path::Path;
use std::ptr::NonNull;
use std::slice;
use std::sync::atomic::{AtomicBool, Ordering};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawMetadata {
    pub width: u32,
    pub height: u32,
    pub make: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearImage {
    pub width: u32,
    pub height: u32,
    /// 3-channel RGB, sRGB primaries, gamma 1.0 (linear), 16 bits per channel,
    /// row-major, host byte order. `data.len() == width * height * 3`.
    pub data: Vec<u16>,
}

/// JPEG bytes extracted from a RAW container's embedded thumbnail. Camera-encoded,
/// already display-referred (sRGB / camera profile). Decoder-agnostic: the buffer
/// is exactly what the camera wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedJpeg {
    /// Width reported by libraw's thumbnail header. May be 0 if libraw did not
    /// populate it; in that case the decoder reads the SOI marker.
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinearOptions {
    /// When true, libraw subsamples to half each dimension (1/4 pixel count).
    pub half_size: bool,
    /// When true, apply the camera-stored white balance.
    pub use_camera_wb: bool,
    /// Demosaic interpolation quality: 0 = bilinear (fast), 1 = VNG, 2 = PPG, 3 = AHD.
    pub user_qual: u8,
}

impl Default for LinearOptions {
    fn default() -> Self {
        Self {
            half_size: false,
            use_camera_wb: true,
            user_qual: 3,
        }
    }
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
                write!(f, "LibRaw failed to materialize processed image ({code}): {message}")
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

pub fn read_metadata(path: &Path) -> Result<RawMetadata> {
    let handle = RawHandle::open(path)?;

    let width = unsafe { ffi::libraw_get_iwidth(handle.as_ptr()) } as u32;
    let height = unsafe { ffi::libraw_get_iheight(handle.as_ptr()) } as u32;
    let (make, model) = unsafe { read_make_model(&handle) };

    Ok(RawMetadata {
        width,
        height,
        make,
        model,
    })
}

/// Read the camera's embedded thumbnail. Returns `Ok(None)` when no JPEG thumb
/// is available (the file may carry a bitmap thumb, or none at all).
///
/// This is a *decoding capability*, not a preview policy: the caller decides
/// whether the thumbnail is useful for the resolution it needs.
pub fn read_embedded_jpeg(path: &Path) -> Result<Option<EmbeddedJpeg>> {
    let handle = RawHandle::open(path)?;

    let code = unsafe { ffi::libraw_unpack_thumb(handle.as_ptr()) };
    if code != 0 {
        // No accessible thumb (typical: file has none, or unsupported variant).
        return Ok(None);
    }

    let mut errc: c_int = 0;
    let raw = unsafe { ffi::libraw_dcraw_make_mem_thumb(handle.as_ptr(), &mut errc) };
    let img = match NonNull::new(raw) {
        Some(p) => OwnedProcessedImage { ptr: p },
        None => return Ok(None),
    };

    let header = unsafe { img.header() };
    if header.type_ != ffi::LIBRAW_IMAGE_JPEG {
        // Bitmap thumbnails aren't useful here; the linear path is faster than
        // marshalling raw RGB bytes only to gamma-encode them again.
        return Ok(None);
    }

    let bytes = unsafe { img.bytes() }.to_vec();
    Ok(Some(EmbeddedJpeg {
        width: header.width as u32,
        height: header.height as u32,
        bytes,
    }))
}

pub fn read_linear(
    path: &Path,
    opts: &LinearOptions,
    cancel: Option<&AtomicBool>,
) -> Result<LinearImage> {
    check_cancel(cancel)?;
    let handle = RawHandle::open(path)?;

    unsafe {
        ffi::libraw_set_output_bps(handle.as_ptr(), 16);
        ffi::libraw_set_gamma(handle.as_ptr(), 0, 1.0);
        ffi::libraw_set_gamma(handle.as_ptr(), 1, 1.0);
        ffi::libraw_set_no_auto_bright(handle.as_ptr(), 1);
        ffi::libraw_set_output_color(handle.as_ptr(), 1);
        ffi::libraw_set_demosaic(handle.as_ptr(), opts.user_qual as c_int);
        ffi::tr_set_half_size(handle.as_ptr(), opts.half_size as c_int);
        ffi::tr_set_use_camera_wb(handle.as_ptr(), opts.use_camera_wb as c_int);
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

    let mut data = Vec::with_capacity(expected_pixels);
    let src = &bytes[..expected_bytes];
    // libraw documents host byte order; reinterpret pairs as u16 native-endian.
    for chunk in src.chunks_exact(2) {
        data.push(u16::from_ne_bytes([chunk[0], chunk[1]]));
    }

    Ok(LinearImage {
        width,
        height,
        data,
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

unsafe fn read_make_model(handle: &RawHandle) -> (Option<String>, Option<String>) {
    let iparams = unsafe { ffi::libraw_get_iparams(handle.as_ptr()) };
    if iparams.is_null() {
        return (None, None);
    }
    let make = c_array_to_string(unsafe { &(*iparams).make });
    let model = c_array_to_string(unsafe { &(*iparams).model });
    (make, model)
}

fn c_array_to_string(arr: &[c_char]) -> Option<String> {
    let len = arr.iter().position(|&c| c == 0).unwrap_or(arr.len());
    if len == 0 {
        return None;
    }
    let bytes: &[u8] = unsafe { slice::from_raw_parts(arr.as_ptr() as *const u8, len) };
    Some(String::from_utf8_lossy(bytes).into_owned())
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
    use super::{c_char, c_double, c_int, c_uint, c_ushort};

    #[allow(non_camel_case_types)]
    pub enum libraw_data_t {}

    pub const LIBRAW_IMAGE_JPEG: c_int = 1;

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

    #[repr(C)]
    #[allow(non_camel_case_types)]
    pub struct libraw_iparams_t {
        pub guard: [c_char; 4],
        pub make: [c_char; 64],
        pub model: [c_char; 64],
    }

    unsafe extern "C" {
        pub fn libraw_init(flags: c_uint) -> *mut libraw_data_t;
        pub fn libraw_open_file(data: *mut libraw_data_t, file: *const c_char) -> c_int;
        pub fn libraw_close(data: *mut libraw_data_t);
        pub fn libraw_strerror(error_code: c_int) -> *const c_char;
        #[allow(dead_code)] // exercised by FFI smoke tests only
        pub fn libraw_version() -> *const c_char;

        pub fn libraw_get_iwidth(data: *mut libraw_data_t) -> c_int;
        pub fn libraw_get_iheight(data: *mut libraw_data_t) -> c_int;
        pub fn libraw_get_iparams(data: *mut libraw_data_t) -> *const libraw_iparams_t;

        pub fn libraw_unpack(data: *mut libraw_data_t) -> c_int;
        pub fn libraw_unpack_thumb(data: *mut libraw_data_t) -> c_int;
        pub fn libraw_dcraw_process(data: *mut libraw_data_t) -> c_int;
        pub fn libraw_dcraw_make_mem_image(
            data: *mut libraw_data_t,
            errc: *mut c_int,
        ) -> *mut libraw_processed_image_t;
        pub fn libraw_dcraw_make_mem_thumb(
            data: *mut libraw_data_t,
            errc: *mut c_int,
        ) -> *mut libraw_processed_image_t;
        pub fn libraw_dcraw_clear_mem(img: *mut libraw_processed_image_t);

        pub fn libraw_set_demosaic(data: *mut libraw_data_t, value: c_int);
        pub fn libraw_set_output_color(data: *mut libraw_data_t, value: c_int);
        pub fn libraw_set_output_bps(data: *mut libraw_data_t, value: c_int);
        pub fn libraw_set_gamma(data: *mut libraw_data_t, index: c_int, value: c_double);
        pub fn libraw_set_no_auto_bright(data: *mut libraw_data_t, value: c_int);

        // Custom C wrappers (wrapper.c): expose params fields lacking public setters.
        pub fn tr_set_half_size(data: *mut libraw_data_t, value: c_int);
        pub fn tr_set_use_camera_wb(data: *mut libraw_data_t, value: c_int);
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
    fn libraw_get_iwidth_on_unopened_handle_returns_zero() {
        let ptr = unsafe { ffi::libraw_init(0) };
        assert!(!ptr.is_null());
        let width = unsafe { ffi::libraw_get_iwidth(ptr) };
        let height = unsafe { ffi::libraw_get_iheight(ptr) };
        unsafe { ffi::libraw_close(ptr) };
        assert_eq!(width, 0);
        assert_eq!(height, 0);
    }

    #[test]
    fn raw_handle_rejects_path_with_nul() {
        let path = Path::new("foo\0bar.raw");
        let err = RawHandle::open(path).unwrap_err();
        assert!(matches!(err, Error::PathContainsNul), "got {err:?}");
    }

    #[test]
    fn read_metadata_on_missing_file_returns_open_error() {
        let path = missing_path();
        let err = read_metadata(&path).unwrap_err();
        assert!(matches!(err, Error::OpenFailed { .. }), "got {err:?}");
    }

    #[test]
    fn read_linear_on_missing_file_returns_open_error() {
        let path = missing_path();
        let err = read_linear(&path, &LinearOptions::default(), None).unwrap_err();
        assert!(matches!(err, Error::OpenFailed { .. }), "got {err:?}");
    }

    #[test]
    fn read_embedded_jpeg_on_missing_file_returns_open_error() {
        let path = missing_path();
        let err = read_embedded_jpeg(&path).unwrap_err();
        assert!(matches!(err, Error::OpenFailed { .. }), "got {err:?}");
    }

    #[test]
    fn read_linear_pre_open_cancel_returns_cancelled() {
        let path = missing_path();
        let flag = AtomicBool::new(true);
        let err = read_linear(&path, &LinearOptions::default(), Some(&flag)).unwrap_err();
        assert!(matches!(err, Error::Cancelled), "got {err:?}");
    }

    #[test]
    fn libraw_setters_smoke_test() {
        // Confirm all setters are linkable and do not crash on a fresh handle.
        let ptr = unsafe { ffi::libraw_init(0) };
        assert!(!ptr.is_null());
        unsafe {
            ffi::libraw_set_output_bps(ptr, 16);
            ffi::libraw_set_gamma(ptr, 0, 1.0);
            ffi::libraw_set_gamma(ptr, 1, 1.0);
            ffi::libraw_set_no_auto_bright(ptr, 1);
            ffi::libraw_set_output_color(ptr, 1);
            ffi::libraw_set_demosaic(ptr, 0);
            ffi::tr_set_half_size(ptr, 1);
            ffi::tr_set_use_camera_wb(ptr, 1);
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
