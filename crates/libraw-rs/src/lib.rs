use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_ushort};
use std::fmt;
use std::mem::size_of;
use std::path::Path;
use std::ptr::NonNull;
use std::slice;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawMetadata {
    pub width: u32,
    pub height: u32,
    pub make: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewImage {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
    pub format: PreviewFormat,
    pub source: PreviewSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewFormat {
    Jpeg,
    Rgb8 { colors: u8, bits_per_channel: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewSource {
    EmbeddedThumbnail,
    ProcessedRaw,
}

#[derive(Debug)]
pub enum Error {
    PathNotUtf8,
    PathContainsNul,
    InitFailed,
    OpenFailed { code: i32, message: String },
    UnpackThumbFailed { code: i32, message: String },
    MakeMemThumbFailed { code: i32, message: String },
    UnpackFailed { code: i32, message: String },
    ProcessFailed { code: i32, message: String },
    MakeMemImageFailed { code: i32, message: String },
    UnsupportedPreviewFormat(i32),
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
            Error::UnpackThumbFailed { code, message } => {
                write!(f, "LibRaw failed to unpack thumbnail ({code}): {message}")
            }
            Error::MakeMemThumbFailed { code, message } => {
                write!(f, "LibRaw failed to materialize thumbnail ({code}): {message}")
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
            Error::UnsupportedPreviewFormat(kind) => {
                write!(f, "LibRaw returned an unsupported preview format ({kind})")
            }
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

pub fn read_preview(path: &Path) -> Result<PreviewImage> {
    let handle = RawHandle::open(path)?;

    if let Some(preview) = try_thumbnail(&handle)? {
        return Ok(preview);
    }

    read_processed_raw(&handle)
}

fn try_thumbnail(handle: &RawHandle) -> Result<Option<PreviewImage>> {
    let code = unsafe { ffi::libraw_unpack_thumb(handle.as_ptr()) };
    if code != 0 {
        return Ok(None);
    }

    let mut errc: c_int = 0;
    let raw = unsafe { ffi::libraw_dcraw_make_mem_thumb(handle.as_ptr(), &mut errc) };
    let img = match NonNull::new(raw) {
        Some(ptr) => OwnedProcessedImage { ptr },
        None => {
            if errc != 0 {
                return Err(Error::MakeMemThumbFailed {
                    code: errc,
                    message: libraw_error_message(errc),
                });
            }
            return Ok(None);
        }
    };

    let preview = build_preview(&img, PreviewSource::EmbeddedThumbnail)?;
    Ok(Some(preview))
}

fn read_processed_raw(handle: &RawHandle) -> Result<PreviewImage> {
    let code = unsafe { ffi::libraw_unpack(handle.as_ptr()) };
    if code != 0 {
        return Err(Error::UnpackFailed {
            code,
            message: libraw_error_message(code),
        });
    }

    let code = unsafe { ffi::libraw_dcraw_process(handle.as_ptr()) };
    if code != 0 {
        return Err(Error::ProcessFailed {
            code,
            message: libraw_error_message(code),
        });
    }

    let mut errc: c_int = 0;
    let raw = unsafe { ffi::libraw_dcraw_make_mem_image(handle.as_ptr(), &mut errc) };
    let img = NonNull::new(raw)
        .map(|ptr| OwnedProcessedImage { ptr })
        .ok_or_else(|| Error::MakeMemImageFailed {
            code: errc,
            message: libraw_error_message(errc),
        })?;

    build_preview(&img, PreviewSource::ProcessedRaw)
}

fn build_preview(img: &OwnedProcessedImage, source: PreviewSource) -> Result<PreviewImage> {
    let header = unsafe { img.header() };
    let format = match header.type_ {
        ffi::LIBRAW_IMAGE_JPEG => PreviewFormat::Jpeg,
        ffi::LIBRAW_IMAGE_BITMAP => PreviewFormat::Rgb8 {
            colors: header.colors as u8,
            bits_per_channel: header.bits as u8,
        },
        other => return Err(Error::UnsupportedPreviewFormat(other)),
    };

    let bytes = unsafe { img.bytes() }.to_vec();

    Ok(PreviewImage {
        width: header.width as u32,
        height: header.height as u32,
        bytes,
        format,
        source,
    })
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
    use super::{c_char, c_int, c_uint, c_ushort};

    #[allow(non_camel_case_types)]
    pub enum libraw_data_t {}

    pub const LIBRAW_IMAGE_JPEG: c_int = 1;
    pub const LIBRAW_IMAGE_BITMAP: c_int = 2;

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

        pub fn libraw_unpack_thumb(data: *mut libraw_data_t) -> c_int;
        pub fn libraw_dcraw_make_mem_thumb(
            data: *mut libraw_data_t,
            errc: *mut c_int,
        ) -> *mut libraw_processed_image_t;

        pub fn libraw_unpack(data: *mut libraw_data_t) -> c_int;
        pub fn libraw_dcraw_process(data: *mut libraw_data_t) -> c_int;
        pub fn libraw_dcraw_make_mem_image(
            data: *mut libraw_data_t,
            errc: *mut c_int,
        ) -> *mut libraw_processed_image_t;

        pub fn libraw_dcraw_clear_mem(img: *mut libraw_processed_image_t);
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
    fn read_preview_on_missing_file_returns_open_error() {
        let path = missing_path();
        let err = read_preview(&path).unwrap_err();
        assert!(matches!(err, Error::OpenFailed { .. }), "got {err:?}");
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
