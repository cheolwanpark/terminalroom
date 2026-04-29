use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, anyhow, bail};

const RAW_EXTENSIONS: &[&str] = &[
    "arw", "cr2", "cr3", "dng", "nef", "nrw", "raf", "raw", "rw2", "orf", "pef", "srw",
];

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
}

pub fn discover(input: &Path) -> Result<Session> {
    let canonical = input
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", input.display()))?;
    let metadata = fs::metadata(&canonical)
        .with_context(|| format!("failed to stat {}", canonical.display()))?;

    if metadata.is_file() {
        let file = describe(&canonical, &metadata)?;
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
            if !has_raw_extension(&path) {
                continue;
            }
            let meta = fs::metadata(&path)
                .with_context(|| format!("failed to stat {}", path.display()))?;
            files.push(describe(&path, &meta)?);
        }
        if files.is_empty() {
            bail!("no RAW files found in {}", canonical.display());
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

fn describe(canonical: &Path, metadata: &fs::Metadata) -> Result<DiscoveredFile> {
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
    })
}

fn has_raw_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let lower = e.to_ascii_lowercase();
            RAW_EXTENSIONS.iter().any(|ext| *ext == lower)
        })
        .unwrap_or(false)
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
            err.to_string().contains("no RAW files"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn discover_dir_filters_and_sorts_case_insensitively() {
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "b.NEF");
        touch(tmp.path(), "a.txt");
        touch(tmp.path(), "C.cr3");
        touch(tmp.path(), "d.JPG");

        let session = discover(tmp.path()).unwrap();
        let names: Vec<_> = session
            .files
            .iter()
            .map(|f| f.display_name.as_str())
            .collect();
        assert_eq!(names, vec!["b.NEF", "C.cr3"]);
    }

    #[test]
    fn discover_single_file_uses_parent_as_root() {
        let tmp = TempDir::new().unwrap();
        let file = touch(tmp.path(), "x.cr3");
        let session = discover(&file).unwrap();
        assert_eq!(session.files.len(), 1);
        assert_eq!(session.root, tmp.path().canonicalize().unwrap());
    }

    #[test]
    fn discover_single_non_raw_file_is_accepted() {
        let tmp = TempDir::new().unwrap();
        let file = touch(tmp.path(), "notes.txt");
        let session = discover(&file).unwrap();
        assert_eq!(session.files.len(), 1);
        assert_eq!(session.files[0].display_name, "notes.txt");
    }

    #[test]
    fn fingerprint_format() {
        let f = DiscoveredFile {
            canonical_path: PathBuf::from("/x"),
            display_name: "x".into(),
            size_bytes: 42,
            modified_unix_seconds: 1234,
        };
        assert_eq!(fingerprint(&f), "42:1234");
    }
}
