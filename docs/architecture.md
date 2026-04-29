# Architecture

## Workspace Shape

The project should start as a Cargo workspace with two crates:

```text
terminalroom/
  Cargo.toml
  crates/
    libraw-rs/
      Cargo.toml
      src/
        lib.rs
    terminalroom/
      Cargo.toml
      src/
        main.rs
        lib.rs
        session.rs
        db.rs
        preview.rs
```

`libraw-rs` is a library crate. It owns LibRaw linking, bindings, unsafe calls, pointer lifetimes, and conversion into safe Rust image data.

`terminalroom` is a library + binary crate. The library half holds the headless modules (`session`, `db`, `preview`) so they can be unit-tested without a TTY. The binary half is the entry point: CLI parsing, terminal rendering, keyboard input, and view routing.

## Crate Boundary

The app crate must not depend on LibRaw C symbols directly. Its interaction with RAW files should go through safe `libraw-rs` APIs shaped around the MVP:

```rust
pub struct RawMetadata {
    pub width: u32,
    pub height: u32,
    pub make: Option<String>,
    pub model: Option<String>,
}

pub struct PreviewImage {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
    pub format: PreviewFormat,
    pub source: PreviewSource,
}

pub enum PreviewFormat {
    Jpeg,
    Rgb8 { colors: u8, bits_per_channel: u8 },
}

pub enum PreviewSource {
    EmbeddedThumbnail,
    ProcessedRaw,
}

pub fn read_metadata(path: &Path) -> Result<RawMetadata>;
pub fn read_preview(path: &Path) -> Result<PreviewImage>;
```

The exact Rust names can change during implementation, but the boundary should stay this simple: the TUI asks for metadata or a preview and receives owned Rust values. `libraw-rs` returns the preview bytes as LibRaw produced them (JPEG for most embedded thumbnails, packed RGB for processed RAW) plus a `PreviewFormat` tag; JPEG decoding and pixel conversion live in the application crate so the FFI crate stays free of image-processing dependencies.

## Application Modules

The headless half of `terminalroom` is split into three modules under `crates/terminalroom/src/`. Each module owns one responsibility and exposes a small surface that the future TUI consumes.

- `session` — resolves an input path into a `Session { root, files }`. Single file inputs use the parent directory as session root; directory inputs scan immediate children, filter by RAW extension (case-insensitive), and sort by case-insensitive filename. Provides `fingerprint()` (`<size>:<mtime>`) for change detection.
- `db` — owns the SQLite connection at `<root>/.terminalroom.db`. Handles `PRAGMA user_version` migrations, `sync_files` upsert (creates default `unset` culling rows; never deletes missing files), and `set_state` for culling state changes. `now_unix` is injected so callers control the clock.
- `preview` — converts `libraw_rs::PreviewImage` into `image::DynamicImage` for the TUI. JPEG bytes go through `image::load_from_memory_with_format`; packed `Rgb8` 3-channel/8-bit becomes `DynamicImage::ImageRgb8`. Other RGB layouts return `PreviewError::UnsupportedRgb` until a real RAW forces support.

Each module has unit tests that run without RAW fixtures: `session` uses `tempfile` directories, `db` uses in-memory and temp-file SQLite, `preview` synthesizes JPEG bytes at test time via the `image` crate's encoder.

## Runtime Flow

1. Parse `terminalroom <path>`.
2. Resolve the input path.
3. If the input is a RAW file, use that single file as the session.
4. If the input is a directory, scan immediate children, filter supported RAW extensions, sort by filename, and use the directory as the session root.
5. Open or create `<session-root>/.terminalroom.db`.
6. Upsert discovered file records.
7. Start the culling view.
8. Decode previews on demand and cache them in memory for nearby files.
9. Persist every culling state change immediately.
10. Switch to the develop placeholder view when requested.

For a single-file session, use the file's parent directory as the session root for `.terminalroom.db`.

## App State

The app state should stay explicit and small for the first MVP:

- `session_root`: directory containing `.terminalroom.db`.
- `files`: sorted list of discovered RAW files and their DB-backed culling states.
- `selected_index`: current file index.
- `view`: `Culling` or `Develop`.
- `preview_cache`: bounded in-memory cache keyed by canonical path.
- `image_protocol_state`: ratatui-image state for the currently rendered image.

Avoid introducing plugin systems, async runtimes, or generalized editing pipelines before the first culling loop works.

## Preview Loading Strategy

Use embedded thumbnails first because culling needs fast visual feedback more than final color accuracy. The `libraw-rs` flow should be:

1. Open the RAW file with a fresh LibRaw handler.
2. Try `libraw_unpack_thumb`.
3. Wrap the `libraw_processed_image_t` returned by `libraw_dcraw_make_mem_thumb` in an owned RAII buffer, copy the bytes out, and tag the format (`Jpeg` for `LIBRAW_IMAGE_JPEG`, `Rgb8` for `LIBRAW_IMAGE_BITMAP`).
4. If thumbnail extraction fails or returns an unsupported image type, run a basic processed preview path (`libraw_unpack` → `libraw_dcraw_process` → `libraw_dcraw_make_mem_image`).
5. Always close or recycle the LibRaw handler before returning.

Each LibRaw handler processes one file at a time. Multiple handlers may exist in different threads later, but the first MVP can decode synchronously or through one worker thread if UI blocking becomes visible.

## Error Handling

MVP errors should be visible and recoverable:

- If a file fails to decode, keep it in the list and show an error placeholder in the image area.
- If DB open or migration fails, exit with a clear error.
- If no RAW files are found, exit with a clear message.
- If the terminal does not support rich image protocols, fall back to ratatui-image halfblocks.

