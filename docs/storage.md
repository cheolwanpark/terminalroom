# Storage

Terminalroom stores culling data in a SQLite database named `.terminalroom.db`.

## Location

- Directory input: `<path>/.terminalroom.db`.
- File input: `<parent-of-file>/.terminalroom.db`.

The database belongs to the session root, not to the current working directory.

## Schema Versioning

Use `PRAGMA user_version` for migrations. Version `1` is the MVP schema.

On startup:

1. Open or create the DB.
2. Read `PRAGMA user_version`.
3. If it is `0`, create the v1 schema in one transaction and set `user_version = 1`.
4. If it is `1`, continue.
5. If it is greater than the app supports, exit with a clear error.

## Culling States

Use string states in the DB for readability:

- `unset`: no decision yet.
- `pick`: selected as a keeper.
- `reject`: rejected during culling.

The Rust app can represent this as an enum and convert at the DB boundary.

## MVP Schema

```sql
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
```

`fingerprint` should be deterministic for the MVP. Use:

```text
<size_bytes>:<modified_unix_seconds>
```

This is not a cryptographic identity. It is enough to detect common file changes without adding expensive hashing to the first pass.

## Startup Sync

For every discovered RAW file:

1. Canonicalize the path.
2. Read file size and modification time.
3. Compute the MVP fingerprint.
4. Upsert into `files`.
5. Insert `unset` into `culling` if the file has no culling row.

Do not delete DB rows for files that are missing from the current scan. They may reappear if a drive or directory is restored. Filtering the visible session should be based on the current scan result.

## Write Policy

Persist culling changes immediately when the user presses a culling shortcut.

For each state change:

```sql
UPDATE culling
SET state = ?1,
    updated_unix_seconds = ?2
WHERE file_id = ?3;
```

The UI should update in memory only after the DB write succeeds. If the write fails, keep the old state and show an error in the status line.

## Future Extensions

Likely future tables:

- `develop_settings`: per-file non-destructive edit parameters.
- `exports`: output history and export presets.
- `tags`: user labels or workflow metadata.

Do not add these tables in v1.

