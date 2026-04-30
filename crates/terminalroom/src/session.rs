use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, anyhow, bail};
use darkroom::{ImageKind, classify};

#[derive(Debug)]
pub struct Session {
    pub root: PathBuf,
    pub files: Vec<DiscoveredFile>,
}

#[derive(Clone, Debug)]
pub struct DiscoveredFile {
    pub canonical_path: PathBuf,
    pub display_name: String,
    pub size_bytes: u64,
    pub modified_unix_seconds: i64,
    pub kind: ImageKind,
}

pub fn discover(input: &Path) -> Result<Session> {
    let canonical = input
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", input.display()))?;
    let metadata = fs::metadata(&canonical)
        .with_context(|| format!("failed to stat {}", canonical.display()))?;

    if metadata.is_file() {
        let kind = path_kind(&canonical).ok_or_else(|| {
            anyhow!("{} is not a supported image format", canonical.display())
        })?;
        let file = describe(&canonical, &metadata, kind)?;
        let root = canonical
            .parent()
            .ok_or_else(|| anyhow!("{} has no parent directory", canonical.display()))?
            .to_path_buf();
        return Ok(Session {
            root,
            files: vec![file],
        });
    }

    if metadata.is_dir() {
        let mut files = Vec::new();
        for entry in fs::read_dir(&canonical)
            .with_context(|| format!("failed to read directory {}", canonical.display()))?
        {
            let entry = entry?;
            let entry_type = entry.file_type()?;
            if !entry_type.is_file() {
                continue;
            }
            let path = entry.path();
            let Some(kind) = path_kind(&path) else {
                continue;
            };
            let meta = fs::metadata(&path)
                .with_context(|| format!("failed to stat {}", path.display()))?;
            files.push(describe(&path, &meta, kind)?);
        }
        if files.is_empty() {
            bail!("no image files found in {}", canonical.display());
        }
        files.sort_by(|a, b| {
            a.display_name
                .to_ascii_lowercase()
                .cmp(&b.display_name.to_ascii_lowercase())
        });
        return Ok(Session {
            root: canonical,
            files,
        });
    }

    bail!("{} is not a file or directory", canonical.display())
}

pub fn fingerprint(file: &DiscoveredFile) -> String {
    format!("{}:{}", file.size_bytes, file.modified_unix_seconds)
}

fn describe(
    canonical: &Path,
    metadata: &fs::Metadata,
    kind: ImageKind,
) -> Result<DiscoveredFile> {
    let display_name = canonical
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("{} has no file name", canonical.display()))?;
    let size_bytes = metadata.len();
    let modified_unix_seconds = metadata
        .modified()
        .with_context(|| format!("{} has no mtime", canonical.display()))?
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    Ok(DiscoveredFile {
        canonical_path: canonical.to_path_buf(),
        display_name,
        size_bytes,
        modified_unix_seconds,
        kind,
    })
}

fn path_kind(path: &Path) -> Option<ImageKind> {
    path.extension().and_then(|e| e.to_str()).and_then(classify)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::TempDir;

    fn touch(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        File::create(&path).unwrap();
        path
    }

    #[test]
    fn discover_empty_dir_errors() {
        let tmp = TempDir::new().unwrap();
        let err = discover(tmp.path()).unwrap_err();
        assert!(
            err.to_string().contains("no image files"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn discover_dir_includes_raw_and_non_raw_filters_unknown() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "b.NEF");
        touch(tmp.path(), "a.txt");
        touch(tmp.path(), "C.cr3");
        touch(tmp.path(), "d.JPG");
        touch(tmp.path(), "E.png");
        touch(tmp.path(), "f.TIFF");

        let session = discover(tmp.path()).unwrap();
        let names: Vec<_> = session
            .files
            .iter()
            .map(|f| f.display_name.as_str())
            .collect();
        // sorted case-insensitively
        assert_eq!(names, vec!["b.NEF", "C.cr3", "d.JPG", "E.png", "f.TIFF"]);

        let kinds: Vec<_> = session.files.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ImageKind::Raw,
                ImageKind::Raw,
                ImageKind::Jpeg,
                ImageKind::Png,
                ImageKind::Tiff,
            ]
        );
    }

    #[test]
    fn discover_single_raw_file_uses_parent_as_root() {
        let tmp = TempDir::new().unwrap();
        let file = touch(tmp.path(), "x.cr3");
        let session = discover(&file).unwrap();
        assert_eq!(session.files.len(), 1);
        assert_eq!(session.files[0].kind, ImageKind::Raw);
        assert_eq!(session.root, tmp.path().canonicalize().unwrap());
    }

    #[test]
    fn discover_single_jpeg_file_is_accepted() {
        let tmp = TempDir::new().unwrap();
        let file = touch(tmp.path(), "x.jpg");
        let session = discover(&file).unwrap();
        assert_eq!(session.files.len(), 1);
        assert_eq!(session.files[0].kind, ImageKind::Jpeg);
    }

    #[test]
    fn discover_single_unsupported_file_errors() {
        let tmp = TempDir::new().unwrap();
        let file = touch(tmp.path(), "notes.txt");
        let err = discover(&file).unwrap_err();
        assert!(
            err.to_string().contains("not a supported image format"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn fingerprint_format() {
        let f = DiscoveredFile {
            canonical_path: PathBuf::from("/x"),
            display_name: "x".into(),
            size_bytes: 42,
            modified_unix_seconds: 1234,
            kind: ImageKind::Raw,
        };
        assert_eq!(fingerprint(&f), "42:1234");
    }
}
