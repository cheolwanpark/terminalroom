use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use darkroom::DevelopParams;
use rusqlite::{Connection, OptionalExtension, params};

use crate::paths;
use crate::session::DiscoveredFile;

const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS files (
    id                            INTEGER PRIMARY KEY AUTOINCREMENT,
    canonical_path                TEXT    NOT NULL UNIQUE,
    display_name                  TEXT    NOT NULL,
    size_bytes                    INTEGER NOT NULL,
    modified_unix_seconds         INTEGER NOT NULL,
    source_fingerprint            INTEGER NOT NULL,
    removed                       INTEGER NOT NULL DEFAULT 0,
    develop_params_json           TEXT    NOT NULL,
    develop_params_fingerprint    INTEGER NOT NULL,
    cache_key                     TEXT,
    last_access_unix_seconds      INTEGER NOT NULL,
    first_seen_unix_seconds       INTEGER NOT NULL,
    last_seen_unix_seconds        INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_files_last_access ON files(last_access_unix_seconds);
CREATE INDEX IF NOT EXISTS idx_files_cache_key   ON files(cache_key) WHERE cache_key IS NOT NULL;
"#;

const SUPPORTED_VERSION: i32 = 1;

/// Per-file row materialized into Rust types.
#[derive(Clone, Debug)]
pub struct FileRow {
    pub id: i64,
    pub canonical_path: PathBuf,
    pub size_bytes: u64,
    pub modified_unix_seconds: i64,
    pub source_fp: u64,
    pub removed: bool,
    pub develop_params: DevelopParams,
    pub develop_params_fp: u64,
    pub cache_key: Option<String>,
}

#[derive(Debug)]
pub struct Db {
    conn: Connection,
}

impl Db {
    /// Open the global DB at `~/.terminalroom/db.sqlite`, creating it (and the
    /// parent directory) on first use.
    pub fn open_global() -> Result<Self> {
        let path = paths::db_path()?;
        let conn = Connection::open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        Self::with_connection(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("failed to open in-memory SQLite")?;
        Self::with_connection(conn)
    }

    fn with_connection(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;\n\
             PRAGMA journal_mode = WAL;",
        )?;
        let mut db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i32 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;

        match version {
            0 => {
                let tx = self.conn.transaction()?;
                tx.execute_batch(SCHEMA_V1)?;
                tx.execute_batch(&format!("PRAGMA user_version = {SUPPORTED_VERSION};"))?;
                tx.commit()?;
                Ok(())
            }
            v if v == SUPPORTED_VERSION => Ok(()),
            v => bail!("unsupported database version {v}; max supported is {SUPPORTED_VERSION}"),
        }
    }

    /// Upsert a discovered file. New rows get default `DevelopParams` and
    /// `removed = false`. Existing rows have their size/mtime/source_fp +
    /// `last_seen` refreshed; if the source fingerprint changed, the cache
    /// key is cleared (caller is responsible for unlinking the cache file).
    pub fn upsert_file(&mut self, file: &DiscoveredFile, now_unix: i64) -> Result<FileRow> {
        let canonical = file
            .canonical_path
            .to_str()
            .ok_or_else(|| anyhow!("path is not valid UTF-8: {}", file.canonical_path.display()))?;
        let new_source_fp = compute_source_fp(file.size_bytes as i64, file.modified_unix_seconds);

        let tx = self.conn.transaction()?;

        let existing: Option<(i64, u64)> = tx
            .query_row(
                "SELECT id, source_fingerprint FROM files WHERE canonical_path = ?1",
                params![canonical],
                |row| {
                    let id: i64 = row.get(0)?;
                    let fp: i64 = row.get(1)?;
                    Ok((id, fp as u64))
                },
            )
            .optional()?;

        let _id = if let Some((id, prev_fp)) = existing {
            tx.execute(
                "UPDATE files
                   SET display_name = ?2,
                       size_bytes = ?3,
                       modified_unix_seconds = ?4,
                       source_fingerprint = ?5,
                       last_seen_unix_seconds = ?6
                 WHERE id = ?1",
                params![
                    id,
                    file.display_name,
                    file.size_bytes as i64,
                    file.modified_unix_seconds,
                    new_source_fp as i64,
                    now_unix,
                ],
            )?;
            if prev_fp != new_source_fp {
                tx.execute(
                    "UPDATE files SET cache_key = NULL WHERE id = ?1",
                    params![id],
                )?;
            }
            id
        } else {
            let default_params = DevelopParams::default();
            let params_json = serde_json::to_string(&default_params)
                .context("failed to serialize default DevelopParams")?;
            let params_fp = default_params.fingerprint();
            tx.execute(
                "INSERT INTO files
                   (canonical_path, display_name, size_bytes, modified_unix_seconds,
                    source_fingerprint, removed, develop_params_json,
                    develop_params_fingerprint, cache_key,
                    last_access_unix_seconds, first_seen_unix_seconds, last_seen_unix_seconds)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, NULL, ?8, ?8, ?8)",
                params![
                    canonical,
                    file.display_name,
                    file.size_bytes as i64,
                    file.modified_unix_seconds,
                    new_source_fp as i64,
                    params_json,
                    params_fp as i64,
                    now_unix,
                ],
            )?;
            tx.last_insert_rowid()
        };

        tx.commit()?;

        // Re-read what we just wrote so the caller sees the canonical row.
        self.load_for_path(&file.canonical_path)?
            .ok_or_else(|| anyhow!("upsert_file: row vanished after upsert"))
    }

    pub fn load_for_path(&self, canonical: &Path) -> Result<Option<FileRow>> {
        let canonical_str = canonical
            .to_str()
            .ok_or_else(|| anyhow!("path is not valid UTF-8: {}", canonical.display()))?;
        let row = self
            .conn
            .query_row(
                "SELECT id, canonical_path, size_bytes, modified_unix_seconds,
                        source_fingerprint, removed, develop_params_json,
                        develop_params_fingerprint, cache_key
                   FROM files WHERE canonical_path = ?1",
                params![canonical_str],
                row_to_filerow,
            )
            .optional()
            .context("failed to load file row")?;
        Ok(row)
    }

    pub fn set_removed(&mut self, file_id: i64, removed: bool, now_unix: i64) -> Result<()> {
        let updated = self.conn.execute(
            "UPDATE files SET removed = ?1, last_access_unix_seconds = ?2 WHERE id = ?3",
            params![removed as i64, now_unix, file_id],
        )?;
        if updated == 0 {
            bail!("no files row for file_id {file_id}");
        }
        Ok(())
    }

    pub fn update_params(
        &mut self,
        file_id: i64,
        params: &DevelopParams,
        params_fp: u64,
        now_unix: i64,
    ) -> Result<()> {
        let json = serde_json::to_string(params).context("failed to serialize DevelopParams")?;
        let updated = self.conn.execute(
            "UPDATE files
                SET develop_params_json = ?1,
                    develop_params_fingerprint = ?2,
                    last_access_unix_seconds = ?3
              WHERE id = ?4",
            params![json, params_fp as i64, now_unix, file_id],
        )?;
        if updated == 0 {
            bail!("no files row for file_id {file_id}");
        }
        Ok(())
    }

    pub fn set_cache_key(&mut self, file_id: i64, key: Option<&str>) -> Result<()> {
        let updated = self.conn.execute(
            "UPDATE files SET cache_key = ?1 WHERE id = ?2",
            params![key, file_id],
        )?;
        if updated == 0 {
            bail!("no files row for file_id {file_id}");
        }
        Ok(())
    }

    pub fn touch_access(&mut self, file_id: i64, now_unix: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET last_access_unix_seconds = ?1 WHERE id = ?2",
            params![now_unix, file_id],
        )?;
        Ok(())
    }

    pub fn count_cached(&self) -> Result<i64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM files WHERE cache_key IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// Oldest `n` rows by `last_access_unix_seconds`, ascending. Each result is
    /// `(file_id, cache_key)`. Used by the cache LRU evictor.
    pub fn oldest_cached(&self, n: usize) -> Result<Vec<(i64, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, cache_key FROM files
              WHERE cache_key IS NOT NULL
              ORDER BY last_access_unix_seconds ASC
              LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![n as i64], |row| {
                let id: i64 = row.get(0)?;
                let key: String = row.get(1)?;
                Ok((id, key))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// All non-NULL cache keys currently referenced by `files`.
    pub fn all_cache_keys(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT cache_key FROM files WHERE cache_key IS NOT NULL")?;
        let rows = stmt
            .query_map([], |row| {
                let key: String = row.get(0)?;
                Ok(key)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

fn row_to_filerow(row: &rusqlite::Row) -> rusqlite::Result<FileRow> {
    let id: i64 = row.get(0)?;
    let canonical: String = row.get(1)?;
    let size_bytes: i64 = row.get(2)?;
    let mtime: i64 = row.get(3)?;
    let source_fp: i64 = row.get(4)?;
    let removed: i64 = row.get(5)?;
    let params_json: String = row.get(6)?;
    let params_fp: i64 = row.get(7)?;
    let cache_key: Option<String> = row.get(8)?;
    let develop_params: DevelopParams = serde_json::from_str(&params_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(FileRow {
        id,
        canonical_path: PathBuf::from(canonical),
        size_bytes: size_bytes as u64,
        modified_unix_seconds: mtime,
        source_fp: source_fp as u64,
        removed: removed != 0,
        develop_params,
        develop_params_fp: params_fp as u64,
        cache_key,
    })
}

/// Stable u64 hash of `(size, mtime)`. Used as the source fingerprint for
/// cache invalidation.
pub fn compute_source_fp(size_bytes: i64, modified_unix_seconds: i64) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    size_bytes.hash(&mut h);
    modified_unix_seconds.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use darkroom::ImageKind;

    fn make_file(path: &str, size: u64, mtime: i64) -> DiscoveredFile {
        let pb = PathBuf::from(path);
        let display = pb.file_name().unwrap().to_string_lossy().into_owned();
        DiscoveredFile {
            canonical_path: pb,
            display_name: display,
            size_bytes: size,
            modified_unix_seconds: mtime,
            kind: ImageKind::Raw,
        }
    }

    #[test]
    fn open_in_memory_sets_user_version_1() {
        let db = Db::open_in_memory().unwrap();
        let v: i32 = db
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 1);
    }

    #[test]
    fn upsert_file_inserts_default_row() {
        let mut db = Db::open_in_memory().unwrap();
        let row = db.upsert_file(&make_file("/tmp/a.cr3", 100, 1000), 2000).unwrap();
        assert_eq!(row.canonical_path, PathBuf::from("/tmp/a.cr3"));
        assert!(!row.removed);
        assert_eq!(row.cache_key, None);
        assert_eq!(row.size_bytes, 100);
        assert_eq!(row.develop_params.exposure_ev, 0.0);
        assert_eq!(row.develop_params.temperature_kelvin, 5500.0);
        assert_eq!(row.develop_params_fp, DevelopParams::default().fingerprint());
    }

    #[test]
    fn upsert_file_idempotent_preserves_state() {
        let mut db = Db::open_in_memory().unwrap();
        let row1 = db.upsert_file(&make_file("/tmp/a.cr3", 100, 1000), 2000).unwrap();
        db.set_removed(row1.id, true, 3000).unwrap();
        let row2 = db.upsert_file(&make_file("/tmp/a.cr3", 100, 1000), 4000).unwrap();
        assert_eq!(row1.id, row2.id);
        assert!(row2.removed, "removed flag must survive re-upsert");
        assert_eq!(row2.source_fp, row1.source_fp);
    }

    #[test]
    fn upsert_file_clears_cache_key_on_source_change() {
        let mut db = Db::open_in_memory().unwrap();
        let row = db.upsert_file(&make_file("/tmp/a.cr3", 100, 1000), 2000).unwrap();
        db.set_cache_key(row.id, Some("abc")).unwrap();
        let mtime_changed = make_file("/tmp/a.cr3", 100, 9999);
        let row2 = db.upsert_file(&mtime_changed, 3000).unwrap();
        assert_eq!(row2.cache_key, None);
        assert_ne!(row2.source_fp, row.source_fp);
    }

    #[test]
    fn update_params_round_trip() {
        let mut db = Db::open_in_memory().unwrap();
        let row = db.upsert_file(&make_file("/tmp/a.cr3", 100, 1000), 2000).unwrap();
        let mut p = DevelopParams::default();
        p.exposure_ev = 0.75;
        p.warmth = -0.3;
        p.look = "warm-muted-soft".to_string();
        let fp = p.fingerprint();
        db.update_params(row.id, &p, fp, 3000).unwrap();
        let loaded = db
            .load_for_path(&PathBuf::from("/tmp/a.cr3"))
            .unwrap()
            .unwrap();
        assert_eq!(loaded.develop_params.exposure_ev, 0.75);
        assert_eq!(loaded.develop_params.warmth, -0.3);
        assert_eq!(loaded.develop_params.look, "warm-muted-soft");
        assert_eq!(loaded.develop_params_fp, fp);
    }

    #[test]
    fn set_removed_round_trip() {
        let mut db = Db::open_in_memory().unwrap();
        let row = db.upsert_file(&make_file("/tmp/a.cr3", 100, 1000), 2000).unwrap();
        db.set_removed(row.id, true, 3000).unwrap();
        let loaded = db
            .load_for_path(&PathBuf::from("/tmp/a.cr3"))
            .unwrap()
            .unwrap();
        assert!(loaded.removed);
        db.set_removed(row.id, false, 4000).unwrap();
        let loaded = db
            .load_for_path(&PathBuf::from("/tmp/a.cr3"))
            .unwrap()
            .unwrap();
        assert!(!loaded.removed);
    }

    #[test]
    fn cache_key_round_trip() {
        let mut db = Db::open_in_memory().unwrap();
        let row = db.upsert_file(&make_file("/tmp/a.cr3", 100, 1000), 2000).unwrap();
        db.set_cache_key(row.id, Some("deadbeef")).unwrap();
        assert_eq!(db.count_cached().unwrap(), 1);
        let loaded = db
            .load_for_path(&PathBuf::from("/tmp/a.cr3"))
            .unwrap()
            .unwrap();
        assert_eq!(loaded.cache_key.as_deref(), Some("deadbeef"));
        db.set_cache_key(row.id, None).unwrap();
        assert_eq!(db.count_cached().unwrap(), 0);
    }

    #[test]
    fn oldest_cached_orders_by_last_access() {
        let mut db = Db::open_in_memory().unwrap();
        let r1 = db.upsert_file(&make_file("/tmp/a.cr3", 1, 1), 100).unwrap();
        let r2 = db.upsert_file(&make_file("/tmp/b.cr3", 1, 1), 200).unwrap();
        let r3 = db.upsert_file(&make_file("/tmp/c.cr3", 1, 1), 300).unwrap();
        db.set_cache_key(r1.id, Some("k1")).unwrap();
        db.set_cache_key(r2.id, Some("k2")).unwrap();
        db.set_cache_key(r3.id, Some("k3")).unwrap();
        db.touch_access(r1.id, 50).unwrap();
        db.touch_access(r2.id, 10).unwrap();
        db.touch_access(r3.id, 30).unwrap();
        let oldest = db.oldest_cached(2).unwrap();
        assert_eq!(oldest, vec![(r2.id, "k2".to_string()), (r3.id, "k3".to_string())]);
    }

    #[test]
    fn open_with_unsupported_user_version_errors() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA user_version = 99;").unwrap();
        let err = Db::with_connection(conn).unwrap_err();
        assert!(err.to_string().contains("99"), "unexpected error: {err}");
    }
}
