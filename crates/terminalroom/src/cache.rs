//! Persistent on-disk cache of developed-image previews.
//!
//! Each entry is a single `<key>.cache` file under `~/.terminalroom/cache/`.
//! The key is the BLAKE3 of the canonical path bytes (lowercase hex, first 32
//! chars). Files are written atomically (`<key>.cache.tmp` + rename) so a
//! crash mid-write never leaves a corrupt entry.
//!
//! The header records `source_fp` and `params_fp` so a stale entry (file
//! re-edited externally, or develop knobs changed) is rejected on read. CRC32
//! catches partial reads. Any rejection unlinks the file and clears the DB
//! pointer.
//!
//! LRU eviction is driven by `files.last_access_unix_seconds`. After every
//! successful insert, if the count of rows with non-null `cache_key` exceeds
//! `max_entries`, the excess oldest are unlinked and their pointers nulled.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use darkroom::Srgb8;

use crate::db::Db;
use crate::paths;

const MAGIC: &[u8; 4] = b"TRC1";
const VERSION: u16 = 1;
const HEADER_LEN: usize = 40;
const DEFAULT_MAX_ENTRIES: usize = 500;

pub struct Cache {
    dir: PathBuf,
    max_entries: usize,
}

impl Cache {
    pub fn new() -> Result<Self> {
        Ok(Self {
            dir: paths::cache_dir()?,
            max_entries: DEFAULT_MAX_ENTRIES,
        })
    }

    #[cfg(test)]
    pub fn with_dir(dir: PathBuf, max_entries: usize) -> Self {
        Self { dir, max_entries }
    }

    /// Compute the cache filename for a canonical path. The same path always
    /// resolves to the same key.
    pub fn key_for(canonical: &Path) -> String {
        let h = blake3::hash(canonical.as_os_str().to_string_lossy().as_bytes());
        let hex = h.to_hex();
        hex[..32].to_string()
    }

    fn path_for_key(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.cache"))
    }

    /// Try to load a developed image for the given file. Returns `Some(Srgb8)`
    /// on a hit (and bumps `last_access`); `None` on miss / fingerprint
    /// mismatch / corruption (corrupt files are unlinked and the DB pointer
    /// cleared as a side effect).
    pub fn get(
        &self,
        canonical: &Path,
        source_fp: u64,
        params_fp: u64,
        db: &mut Db,
        now_unix: i64,
    ) -> Option<Srgb8> {
        let row = db.load_for_path(canonical).ok().flatten()?;
        let key = row.cache_key.as_deref()?;
        let path = self.path_for_key(key);
        match read_cache_file(&path) {
            Ok(entry) if entry.source_fp == source_fp && entry.params_fp == params_fp => {
                let _ = db.touch_access(row.id, now_unix);
                Some(entry.srgb8)
            }
            _ => {
                // Stale or corrupt — drop it. Best-effort; ignore IO errors.
                let _ = fs::remove_file(&path);
                let _ = db.set_cache_key(row.id, None);
                None
            }
        }
    }

    /// Persist a developed image. Updates `cache_key` + `last_access`, runs
    /// LRU eviction.
    pub fn insert(
        &self,
        canonical: &Path,
        source_fp: u64,
        params_fp: u64,
        srgb8: &Srgb8,
        db: &mut Db,
        now_unix: i64,
    ) -> Result<()> {
        let row = db
            .load_for_path(canonical)?
            .ok_or_else(|| anyhow!("cache::insert: no files row for {}", canonical.display()))?;
        let key = Self::key_for(canonical);
        let path = self.path_for_key(&key);
        write_cache_file(&path, source_fp, params_fp, srgb8)?;
        db.set_cache_key(row.id, Some(&key))?;
        db.touch_access(row.id, now_unix)?;
        self.evict_if_needed(db)?;
        Ok(())
    }

    fn evict_if_needed(&self, db: &mut Db) -> Result<()> {
        let count = db.count_cached()? as usize;
        if count <= self.max_entries {
            return Ok(());
        }
        let excess = count - self.max_entries;
        let victims = db.oldest_cached(excess)?;
        for (id, key) in victims {
            let path = self.path_for_key(&key);
            let _ = fs::remove_file(&path);
            db.set_cache_key(id, None)?;
        }
        Ok(())
    }

    /// Walk the cache directory and unlink any file whose stem isn't
    /// referenced by `files.cache_key`. Run at startup.
    pub fn prune_orphans(&self, db: &Db) -> Result<()> {
        use std::collections::HashSet;
        let known: HashSet<String> = db.all_cache_keys()?.into_iter().collect();
        let entries = match fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if path.extension().and_then(|s| s.to_str()) != Some("cache") {
                // Leave alone (.tmp from in-progress writes; user files; etc.)
                continue;
            }
            if !known.contains(stem) {
                let _ = fs::remove_file(&path);
            }
        }
        Ok(())
    }
}

struct CachedEntry {
    source_fp: u64,
    params_fp: u64,
    srgb8: Srgb8,
}

fn write_cache_file(path: &Path, source_fp: u64, params_fp: u64, srgb8: &Srgb8) -> Result<()> {
    let pixel_len = (srgb8.width as u64) * (srgb8.height as u64) * 3;
    if srgb8.pixels.len() as u64 != pixel_len {
        return Err(anyhow!(
            "Srgb8 pixel buffer size {} != width*height*3 = {}",
            srgb8.pixels.len(),
            pixel_len
        ));
    }
    let crc = crc32fast::hash(&srgb8.pixels);

    let tmp = path.with_extension("cache.tmp");
    {
        let mut f = fs::File::create(&tmp)
            .with_context(|| format!("failed to create cache temp file {}", tmp.display()))?;
        let mut header = [0u8; HEADER_LEN];
        header[0..4].copy_from_slice(MAGIC);
        header[4..6].copy_from_slice(&VERSION.to_le_bytes());
        // 6..8 reserved (already zero)
        header[8..16].copy_from_slice(&source_fp.to_le_bytes());
        header[16..24].copy_from_slice(&params_fp.to_le_bytes());
        header[24..28].copy_from_slice(&srgb8.width.to_le_bytes());
        header[28..32].copy_from_slice(&srgb8.height.to_le_bytes());
        header[32..36].copy_from_slice(&(pixel_len as u32).to_le_bytes());
        header[36..40].copy_from_slice(&crc.to_le_bytes());
        f.write_all(&header)
            .with_context(|| format!("failed to write cache header to {}", tmp.display()))?;
        f.write_all(&srgb8.pixels)
            .with_context(|| format!("failed to write cache pixels to {}", tmp.display()))?;
        f.sync_all().ok();
    }
    fs::rename(&tmp, path)
        .with_context(|| format!("failed to rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

fn read_cache_file(path: &Path) -> Result<CachedEntry> {
    let mut f =
        fs::File::open(path).with_context(|| format!("failed to open cache {}", path.display()))?;
    let mut header = [0u8; HEADER_LEN];
    f.read_exact(&mut header)
        .with_context(|| format!("failed to read cache header {}", path.display()))?;
    if &header[0..4] != MAGIC {
        return Err(anyhow!("bad magic"));
    }
    let version = u16::from_le_bytes([header[4], header[5]]);
    if version != VERSION {
        return Err(anyhow!("unsupported cache version {version}"));
    }
    let source_fp = u64::from_le_bytes(header[8..16].try_into().unwrap());
    let params_fp = u64::from_le_bytes(header[16..24].try_into().unwrap());
    let width = u32::from_le_bytes(header[24..28].try_into().unwrap());
    let height = u32::from_le_bytes(header[28..32].try_into().unwrap());
    let pixel_len = u32::from_le_bytes(header[32..36].try_into().unwrap());
    let crc = u32::from_le_bytes(header[36..40].try_into().unwrap());

    let expected = (width as u64) * (height as u64) * 3;
    if pixel_len as u64 != expected {
        return Err(anyhow!(
            "pixel_len {} != width*height*3 {}",
            pixel_len,
            expected
        ));
    }
    let mut pixels = vec![0u8; pixel_len as usize];
    f.read_exact(&mut pixels)
        .with_context(|| format!("failed to read cache pixels {}", path.display()))?;
    if crc32fast::hash(&pixels) != crc {
        return Err(anyhow!("crc mismatch"));
    }
    Ok(CachedEntry {
        source_fp,
        params_fp,
        srgb8: Srgb8 {
            width,
            height,
            pixels,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::DiscoveredFile;
    use darkroom::ImageKind;
    use tempfile::TempDir;

    fn make_srgb(w: u32, h: u32) -> Srgb8 {
        let pixels: Vec<u8> = (0..(w * h * 3)).map(|i| (i % 251) as u8).collect();
        Srgb8 {
            width: w,
            height: h,
            pixels,
        }
    }

    fn upsert(db: &mut Db, path: &str) -> i64 {
        let f = DiscoveredFile {
            canonical_path: PathBuf::from(path),
            display_name: PathBuf::from(path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            size_bytes: 100,
            modified_unix_seconds: 1000,
            kind: ImageKind::Raw,
        };
        db.upsert_file(&f, 1000).unwrap().id
    }

    #[test]
    fn round_trip_insert_get() {
        let tmp = TempDir::new().unwrap();
        let cache = Cache::with_dir(tmp.path().to_path_buf(), 500);
        let mut db = Db::open_in_memory().unwrap();
        let path = PathBuf::from("/tmp/a.cr3");
        upsert(&mut db, "/tmp/a.cr3");
        let original = make_srgb(8, 6);
        cache
            .insert(&path, 0xAA, 0xBB, &original, &mut db, 100)
            .unwrap();
        let got = cache.get(&path, 0xAA, 0xBB, &mut db, 200).unwrap();
        assert_eq!(got.width, 8);
        assert_eq!(got.height, 6);
        assert_eq!(got.pixels, original.pixels);
    }

    #[test]
    fn miss_on_source_fp_mismatch() {
        let tmp = TempDir::new().unwrap();
        let cache = Cache::with_dir(tmp.path().to_path_buf(), 500);
        let mut db = Db::open_in_memory().unwrap();
        let path = PathBuf::from("/tmp/a.cr3");
        upsert(&mut db, "/tmp/a.cr3");
        cache
            .insert(&path, 0xAA, 0xBB, &make_srgb(4, 4), &mut db, 100)
            .unwrap();
        // Different source_fp (file edited externally).
        assert!(cache.get(&path, 0xCC, 0xBB, &mut db, 200).is_none());
        // Cache file should be unlinked + cache_key cleared.
        let row = db.load_for_path(&path).unwrap().unwrap();
        assert_eq!(row.cache_key, None);
    }

    #[test]
    fn miss_on_params_fp_mismatch() {
        let tmp = TempDir::new().unwrap();
        let cache = Cache::with_dir(tmp.path().to_path_buf(), 500);
        let mut db = Db::open_in_memory().unwrap();
        let path = PathBuf::from("/tmp/a.cr3");
        upsert(&mut db, "/tmp/a.cr3");
        cache
            .insert(&path, 0xAA, 0xBB, &make_srgb(4, 4), &mut db, 100)
            .unwrap();
        assert!(cache.get(&path, 0xAA, 0xCC, &mut db, 200).is_none());
    }

    #[test]
    fn corruption_is_rejected_and_pointer_cleared() {
        let tmp = TempDir::new().unwrap();
        let cache = Cache::with_dir(tmp.path().to_path_buf(), 500);
        let mut db = Db::open_in_memory().unwrap();
        let path = PathBuf::from("/tmp/a.cr3");
        upsert(&mut db, "/tmp/a.cr3");
        cache
            .insert(&path, 0xAA, 0xBB, &make_srgb(4, 4), &mut db, 100)
            .unwrap();
        // Flip a magic byte.
        let row = db.load_for_path(&path).unwrap().unwrap();
        let key = row.cache_key.unwrap();
        let cache_path = tmp.path().join(format!("{key}.cache"));
        let mut bytes = fs::read(&cache_path).unwrap();
        bytes[0] = 0;
        fs::write(&cache_path, &bytes).unwrap();
        assert!(cache.get(&path, 0xAA, 0xBB, &mut db, 200).is_none());
        let row = db.load_for_path(&path).unwrap().unwrap();
        assert_eq!(row.cache_key, None);
        assert!(!cache_path.exists());
    }

    #[test]
    fn eviction_drops_oldest() {
        let tmp = TempDir::new().unwrap();
        let cache = Cache::with_dir(tmp.path().to_path_buf(), 3);
        let mut db = Db::open_in_memory().unwrap();
        for i in 0..5 {
            let path_str = format!("/tmp/a{i}.cr3");
            upsert(&mut db, &path_str);
            cache
                .insert(
                    &PathBuf::from(&path_str),
                    0xAA,
                    0xBB,
                    &make_srgb(2, 2),
                    &mut db,
                    100 + i as i64,
                )
                .unwrap();
        }
        // After 5 inserts with cap=3, the two oldest (a0, a1) must be evicted.
        assert_eq!(db.count_cached().unwrap(), 3);
        let r0 = db.load_for_path(&PathBuf::from("/tmp/a0.cr3")).unwrap().unwrap();
        let r1 = db.load_for_path(&PathBuf::from("/tmp/a1.cr3")).unwrap().unwrap();
        let r4 = db.load_for_path(&PathBuf::from("/tmp/a4.cr3")).unwrap().unwrap();
        assert_eq!(r0.cache_key, None);
        assert_eq!(r1.cache_key, None);
        assert!(r4.cache_key.is_some());
        // Files for evicted entries are gone.
        let entries: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("cache"))
            .collect();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn prune_orphans_removes_unreferenced_files() {
        let tmp = TempDir::new().unwrap();
        let cache = Cache::with_dir(tmp.path().to_path_buf(), 500);
        let mut db = Db::open_in_memory().unwrap();
        let path = PathBuf::from("/tmp/a.cr3");
        upsert(&mut db, "/tmp/a.cr3");
        cache
            .insert(&path, 0xAA, 0xBB, &make_srgb(2, 2), &mut db, 100)
            .unwrap();
        // Drop a stray file that has no DB pointer.
        fs::write(tmp.path().join("ghost.cache"), b"x").unwrap();
        cache.prune_orphans(&db).unwrap();
        assert!(!tmp.path().join("ghost.cache").exists());
        // The legitimate file is preserved.
        let row = db.load_for_path(&path).unwrap().unwrap();
        let key = row.cache_key.unwrap();
        assert!(tmp.path().join(format!("{key}.cache")).exists());
    }
}
