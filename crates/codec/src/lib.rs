pub mod decode;
pub mod format;

pub use decode::{DecodeError, DecodedImage, TargetSize, decode};
pub use format::{ImageKind, classify};
