use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, params};

use crate::session::{self, DiscoveredFile};

const SCHEMA_V1: &str = r#"
CREATE TABLE files (
    id INTEGER PRIMARY KEY,
    canonical_path TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    modified_unix_seconds INTEGER NOT NULL,
    fingerprint TEXT NOT NULL,
    first_seen_unix_seconds INTEGER NOT NULL,
    last_seen_unix_seconds INTEGER NOT NULL
);

CREATE TABLE culling (
    file_id INTEGER PRIMARY KEY,
    state TEXT NOT NULL CHECK (state IN ('unset', 'pick', 'reject')),
    updated_unix_seconds INTEGER NOT NULL,
    FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE
);

CREATE INDEX idx_culling_state ON culling(state);
"#;

const SUPPORTED_VERSION: i32 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CullingState {
    Unset,
    Pick,
    Reject,
}

impl CullingState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unset => "unset",
            Self::Pick => "pick",
            Self::Reject => "reject",
        }
    }

    pub fn from_db_str(s: &str) -> Result<Self> {
        match s {
            "unset" => Ok(Self::Unset),
            "pick" => Ok(Self::Pick),
            "reject" => Ok(Self::Reject),
            other => bail!("unknown culling state in db: {other}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct FileRecord {
    pub id: i64,
    pub canonical_path: PathBuf,
    pub state: CullingState,
}

#[derive(Debug)]
pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(root: &Path) -> Result<Self> {
        let path = root.join(".terminalroom.db");
        let conn = Connection::open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        Self::with_connection(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("failed to open in-memory SQLite")?;
        Self::with_connection(conn)
    }

    fn with_connection(conn: Connection) -> Result<Self> {
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
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

    pub fn sync_files(
        &mut self,
        files: &[DiscoveredFile],
        now_unix: i64,
    ) -> Result<Vec<FileRecord>> {
        let tx = self.conn.transaction()?;
        let mut records = Vec::with_capacity(files.len());

        for file in files {
            let canonical = file.canonical_path.to_str().ok_or_else(|| {
                anyhow!("path is not valid UTF-8: {}", file.canonical_path.display())
            })?;
            let fingerprint = session::fingerprint(file);

            let existing: Option<i64> = tx
                .query_row(
                    "SELECT id FROM files WHERE canonical_path = ?1",
                    params![canonical],
                    |row| row.get(0),
                )
                .optional()?;

            let id = if let Some(id) = existing {
                tx.execute(
                    "UPDATE files
                     SET display_name = ?2,
                         size_bytes = ?3,
                         modified_unix_seconds = ?4,
                         fingerprint = ?5,
                         last_seen_unix_seconds = ?6
                     WHERE id = ?1",
                    params![
                        id,
                        file.display_name,
                        file.size_bytes as i64,
                        file.modified_unix_seconds,
                        fingerprint,
                        now_unix,
                    ],
                )?;
                id
            } else {
                tx.execute(
                    "INSERT INTO files
                     (canonical_path, display_name, size_bytes, modified_unix_seconds,
                      fingerprint, first_seen_unix_seconds, last_seen_unix_seconds)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                    params![
                        canonical,
                        file.display_name,
                        file.size_bytes as i64,
                        file.modified_unix_seconds,
                        fingerprint,
                        now_unix,
                    ],
                )?;
                tx.last_insert_rowid()
            };

            tx.execute(
                "INSERT OR IGNORE INTO culling (file_id, state, updated_unix_seconds)
                 VALUES (?1, 'unset', ?2)",
                params![id, now_unix],
            )?;

            let state: String = tx.query_row(
                "SELECT state FROM culling WHERE file_id = ?1",
                params![id],
                |row| row.get(0),
            )?;

            records.push(FileRecord {
                id,
                canonical_path: file.canonical_path.clone(),
                state: CullingState::from_db_str(&state)?,
            });
        }

        tx.commit()?;
        Ok(records)
    }

    pub fn set_state(&mut self, file_id: i64, state: CullingState, now_unix: i64) -> Result<()> {
        let updated = self.conn.execute(
            "UPDATE culling SET state = ?1, updated_unix_seconds = ?2 WHERE file_id = ?3",
            params![state.as_str(), now_unix, file_id],
        )?;
        if updated == 0 {
            bail!("no culling row for file_id {file_id}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use darkroom::ImageKind;
    use tempfile::TempDir;

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
    fn sync_files_inserts_new_files_as_unset() {
        let mut db = Db::open_in_memory().unwrap();
        let files = vec![make_file("/tmp/a.cr3", 100, 1000)];
        let recs = db.sync_files(&files, 2000).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].state, CullingState::Unset);
    }

    #[test]
    fn sync_files_second_call_updates_last_seen_only() {
        let mut db = Db::open_in_memory().unwrap();
        let files = vec![make_file("/tmp/a.cr3", 100, 1000)];
        db.sync_files(&files, 2000).unwrap();
        db.sync_files(&files, 5000).unwrap();
        let (first_seen, last_seen): (i64, i64) = db
            .conn
            .query_row(
                "SELECT first_seen_unix_seconds, last_seen_unix_seconds
                 FROM files WHERE canonical_path = '/tmp/a.cr3'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(first_seen, 2000);
        assert_eq!(last_seen, 5000);
    }

    #[test]
    fn sync_files_does_not_delete_missing() {
        let mut db = Db::open_in_memory().unwrap();
        let a = make_file("/tmp/a.cr3", 100, 1000);
        let b = make_file("/tmp/b.cr3", 200, 2000);
        db.sync_files(&[a.clone(), b], 3000).unwrap();
        db.sync_files(&[a], 4000).unwrap();
        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn set_state_persists_across_reopen() {
        let tmp = TempDir::new().unwrap();
        let file_id;
        {
            let mut db = Db::open(tmp.path()).unwrap();
            let recs = db
                .sync_files(&[make_file("/tmp/a.cr3", 100, 1000)], 2000)
                .unwrap();
            file_id = recs[0].id;
            db.set_state(file_id, CullingState::Pick, 3000).unwrap();
        }
        let db = Db::open(tmp.path()).unwrap();
        let state: String = db
            .conn
            .query_row(
                "SELECT state FROM culling WHERE file_id = ?1",
                params![file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "pick");
    }

    #[test]
    fn culling_check_constraint_rejects_invalid_state() {
        let db = Db::open_in_memory().unwrap();
        db.conn
            .execute(
                "INSERT INTO files
                 (canonical_path, display_name, size_bytes, modified_unix_seconds,
                  fingerprint, first_seen_unix_seconds, last_seen_unix_seconds)
                 VALUES ('/tmp/x', 'x', 0, 0, '0:0', 0, 0)",
                [],
            )
            .unwrap();
        let id = db.conn.last_insert_rowid();
        let res = db.conn.execute(
            "INSERT INTO culling (file_id, state, updated_unix_seconds) VALUES (?1, 'bogus', 0)",
            params![id],
        );
        assert!(res.is_err(), "CHECK constraint should reject 'bogus'");
    }

    #[test]
    fn open_with_unsupported_user_version_errors() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".terminalroom.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("PRAGMA user_version = 99;").unwrap();
        }
        let err = Db::open(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("99"), "unexpected error: {err}");
    }

    #[test]
    fn culling_state_string_roundtrip() {
        for s in [
            CullingState::Unset,
            CullingState::Pick,
            CullingState::Reject,
        ] {
            assert_eq!(CullingState::from_db_str(s.as_str()).unwrap(), s);
        }
    }
}
