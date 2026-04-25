use std::ffi::{CStr, CString, c_char, c_int, c_uint};
use std::fmt;
use std::path::Path;
use std::ptr::NonNull;

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
    pub rgba8: Vec<u8>,
    pub source: PreviewSource,
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
    NotImplemented(&'static str),
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
            Error::NotImplemented(feature) => write!(f, "{feature} is not implemented yet"),
        }
    }
}

impl std::error::Error for Error {}

pub fn read_metadata(path: &Path) -> Result<RawMetadata> {
    let _handle = RawHandle::open(path)?;
    Err(Error::NotImplemented("metadata extraction"))
}

pub fn read_preview(path: &Path) -> Result<PreviewImage> {
    let _handle = RawHandle::open(path)?;
    Err(Error::NotImplemented("preview extraction"))
}

struct RawHandle {
    ptr: NonNull<ffi::libraw_data_t>,
}

impl RawHandle {
    fn open(path: &Path) -> Result<Self> {
        let path = path.to_str().ok_or(Error::PathNotUtf8)?;
        let path = CString::new(path).map_err(|_| Error::PathContainsNul)?;

        let ptr = unsafe { ffi::libraw_init(0) };
        let ptr = NonNull::new(ptr).ok_or(Error::InitFailed)?;
        let handle = Self { ptr };

        let code = unsafe { ffi::libraw_open_file(handle.ptr.as_ptr(), path.as_ptr()) };
        if code != 0 {
            return Err(Error::OpenFailed {
                code,
                message: libraw_error_message(code),
            });
        }

        Ok(handle)
    }
}

impl Drop for RawHandle {
    fn drop(&mut self) {
        unsafe {
            ffi::libraw_close(self.ptr.as_ptr());
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
    use super::{c_char, c_int, c_uint};

    #[allow(non_camel_case_types)]
    pub enum libraw_data_t {}

    unsafe extern "C" {
        pub fn libraw_init(flags: c_uint) -> *mut libraw_data_t;
        pub fn libraw_open_file(data: *mut libraw_data_t, file: *const c_char) -> c_int;
        pub fn libraw_close(data: *mut libraw_data_t);
        pub fn libraw_strerror(error_code: c_int) -> *const c_char;
    }
}
