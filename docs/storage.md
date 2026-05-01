# Storage

Terminalroom keeps a single global SQLite database under
`~/.terminalroom/db.sqlite` and a persistent on-disk cache of developed-image
previews under `~/.terminalroom/cache/`.

## Locations

```
~/.terminalroom/
  db.sqlite              # global file metadata + per-file develop knobs + removed flag
  cache/<key>.cache      # one developed-preview file per cached entry (binary, see below)
```

The DB is created on first run; `~/.terminalroom/` and `~/.terminalroom/cache/`
are auto-created. Old per-directory `.terminalroom.db` files from earlier
versions are not read and not migrated — they can be deleted manually if
desired.

## Schema Versioning

`PRAGMA user_version` drives migrations. Version `1` is the current schema.

On startup:

1. Open or create the DB.
2. Read `PRAGMA user_version`.
3. If `0`, create the v1 schema in one transaction and set `user_version = 1`.
4. If `1`, continue.
5. If greater than the app supports, exit with a clear error.

## v1 Schema

```sql
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
PRAGMA user_version = 1;

CREATE TABLE files (
    id                            INTEGER PRIMARY KEY AUTOINCREMENT,
    canonical_path                TEXT    NOT NULL UNIQUE,
    display_name                  TEXT    NOT NULL,
    size_bytes                    INTEGER NOT NULL,
    modified_unix_seconds         INTEGER NOT NULL,
    source_fingerprint            INTEGER NOT NULL,  -- u64 hash(size, mtime), bit-cast
    removed                       INTEGER NOT NULL DEFAULT 0,
    develop_params_json           TEXT    NOT NULL,  -- serde_json of DevelopParams
    develop_params_fingerprint    INTEGER NOT NULL,  -- u64 fingerprint, bit-cast
    cache_key                     TEXT,              -- NULL when no on-disk cache
    last_access_unix_seconds      INTEGER NOT NULL,
    first_seen_unix_seconds       INTEGER NOT NULL,
    last_seen_unix_seconds        INTEGER NOT NULL
);

CREATE INDEX idx_files_last_access ON files(last_access_unix_seconds);
CREATE INDEX idx_files_cache_key   ON files(cache_key) WHERE cache_key IS NOT NULL;
```

`develop_params_json` carries the full `DevelopParams` struct as JSON. The
fingerprint column is stored separately so cache validation never has to parse
JSON. The knob set evolves over time — using JSON keeps the schema stable as
new knobs are added.

The TUI runs two `Db` connections concurrently: one on the UI thread for
synchronous `set_removed` and disk-cache `touch_access`, and one inside a
save worker for the hot-path writes (`update_params` and `Cache::insert`).
WAL allows concurrent readers + one writer at a time; `busy_timeout = 5000`
serializes the rare write/write contention without surfacing `SQLITE_BUSY`
errors to the application.

## Selection Model

There is no pick/reject/unset state. Files have a single `removed` boolean
that defaults to `false`. The TUI has three keys (Navigation focus only):

- `x` — set `removed = true`. The file disappears from the visible list (when
  show-removed is off).
- `r` — set `removed = false` (no-op when not removed).
- `R` (Shift+R) — toggle the session-only "show removed" view. When on, removed
  files appear in the filmstrip with a dim style and a red `R` badge.

Both `x` and `r` write synchronously through `Db::set_removed`.

## Develop Persistence

`DevelopParams` is per-file. When a file is loaded for the first time, a row is
upserted with default knobs (identity / no-op pipeline). Knob edits in the
TUI mutate `App.develop_params` immediately; the changes are committed in-memory
as soon as a tiered debounce expires (50 ms once the worker has the source
cached, 250 ms on the cold first preview), and the matching `update_params`
write is queued for the save worker — the UI thread never blocks on SQLite
during a knob tick. Force-flush points (selection change, focus change, app
exit) call the same async path; on exit the save worker is joined before
`run` returns so pending writes always land.

## On-Disk Preview Cache

For each row that has a recently-rendered developed preview, `cache_key`
points at `~/.terminalroom/cache/<cache_key>.cache`. The key is BLAKE3 of the
canonical path bytes (lowercase hex, first 32 chars).

Each file has a 40-byte header followed by the raw RGB pixel buffer:

```
offset  size  field
0       4     magic                b"TRC1"
4       2     version              u16 LE = 1
6       2     reserved             u16 LE = 0
8       8     source_fingerprint   u64 LE
16      8     params_fingerprint   u64 LE
24      4     width                u32 LE
28      4     height               u32 LE
32      4     pixel_len            u32 LE  (= width*height*3, sanity check)
36      4     crc32                u32 LE  (CRC32 of pixel bytes via crc32fast)
40      ...   pixels               width*height*3 bytes, interleaved RGB
```

Reads reject the file on any of: open error, magic mismatch, version
mismatch, length mismatch, CRC mismatch, fingerprint mismatch (source or
params). On rejection, the file is unlinked and `cache_key` is cleared. Writes
go through a `<key>.cache.tmp` + rename so a crash mid-write never corrupts a
live entry.

The cache is **LRU-bounded by file count**. Default cap is 500 entries. After
each successful insert, if the count of non-NULL `cache_key` rows exceeds the
cap, the oldest excess (by `last_access_unix_seconds`) are unlinked and their
pointers cleared in the same transaction. A startup `prune_orphans()` walks
the cache directory and unlinks any `*.cache` file whose stem isn't referenced
by `files.cache_key`.

## Source-Change Invalidation

`source_fingerprint = hash(size_bytes, modified_unix_seconds)`. On every
session startup, `Db::upsert_file` recomputes it; if the new value differs
from the row's stored value, `cache_key` is set to NULL (the develop pipeline
will repopulate). Develop params are kept across source changes — the user's
edits don't depend on file contents.

## Future Extensions

Likely future columns / tables:

- A separate `looks` table for user-defined Look presets.
- Per-file tags or workflow metadata.

Don't add these in v1.
