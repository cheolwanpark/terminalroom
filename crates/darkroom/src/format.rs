const RAW_EXTENSIONS: &[&str] = &[
    "arw", "cr2", "cr3", "dng", "nef", "nrw", "raf", "raw", "rw2", "orf", "pef", "srw",
];

const JPEG_EXTENSIONS: &[&str] = &["jpg", "jpeg"];
const PNG_EXTENSIONS: &[&str] = &["png"];
const TIFF_EXTENSIONS: &[&str] = &["tif", "tiff"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ImageKind {
    Raw,
    Jpeg,
    Png,
    Tiff,
}

impl ImageKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Raw => "RAW",
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::Tiff => "TIFF",
        }
    }
}

pub fn classify(extension: &str) -> Option<ImageKind> {
    let lower = extension.to_ascii_lowercase();
    if RAW_EXTENSIONS.iter().any(|e| *e == lower) {
        Some(ImageKind::Raw)
    } else if JPEG_EXTENSIONS.iter().any(|e| *e == lower) {
        Some(ImageKind::Jpeg)
    } else if PNG_EXTENSIONS.iter().any(|e| *e == lower) {
        Some(ImageKind::Png)
    } else if TIFF_EXTENSIONS.iter().any(|e| *e == lower) {
        Some(ImageKind::Tiff)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_handles_case_and_unknown() {
        assert_eq!(classify("CR3"), Some(ImageKind::Raw));
        assert_eq!(classify("cr3"), Some(ImageKind::Raw));
        assert_eq!(classify("JPG"), Some(ImageKind::Jpeg));
        assert_eq!(classify("jpeg"), Some(ImageKind::Jpeg));
        assert_eq!(classify("png"), Some(ImageKind::Png));
        assert_eq!(classify("TIFF"), Some(ImageKind::Tiff));
        assert_eq!(classify("tif"), Some(ImageKind::Tiff));
        assert_eq!(classify("txt"), None);
        assert_eq!(classify(""), None);
    }
}
