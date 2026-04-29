# Architecture

## Workspace Shape

The project is a Cargo workspace with two crates:

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
        app.rs
        tui/
          mod.rs
          culling.rs
          develop.rs
          filter.rs
```

`libraw-rs` is a library crate. It owns LibRaw linking, bindings, unsafe calls, pointer lifetimes, and conversion into safe Rust image data.

`terminalroom` is a library + binary crate. The library half holds the headless modules (`session`, `db`, `preview`, `app`) so they can be unit-tested without a TTY; `tui/` is the only module that depends on ratatui/crossterm. The binary half (`main.rs`) is a thin entry point: CLI parsing, then `App::init` and `tui::run`.

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

The headless half of `terminalroom` is split into four modules under `crates/terminalroom/src/`. Each module owns one responsibility and exposes a small surface that the TUI consumes.

- `session` — resolves an input path into a `Session { root, files }`. Single file inputs use the parent directory as session root; directory inputs scan immediate children, filter by image extension (RAW: arw/cr2/cr3/dng/nef/nrw/raf/raw/rw2/orf/pef/srw — plus jpg/jpeg/png/tif/tiff), and sort by case-insensitive filename. Each `DiscoveredFile` carries an `ImageKind` tag (`Raw`, `Jpeg`, `Png`, `Tiff`) so the preview pipeline and filter UI don't have to re-parse extensions. Single-file input rejects unsupported extensions. Provides `fingerprint()` (`<size>:<mtime>`) for change detection.
- `db` — owns the SQLite connection at `<root>/.terminalroom.db`. Handles `PRAGMA user_version` migrations, `sync_files` upsert (creates default `unset` culling rows; never deletes missing files), and `set_state` for culling state changes. `now_unix` is injected so callers control the clock.
- `preview` — converts RAW data and on-disk images into `image::DynamicImage` for the TUI. `decode_preview` consumes `libraw_rs::PreviewImage` (JPEG bytes through `image::load_from_memory_with_format`; packed `Rgb8` 3-channel/8-bit through `ImageBuffer`). `load_preview(path, kind)` is the high-level entry point: it routes RAW through `libraw_rs::read_preview` + `decode_preview`, and JPEG/PNG/TIFF through `image::ImageReader::open`.
- `app` — framework-agnostic state and update logic. Owns the `Db`, the `FileEntry` list, the `visible` index list (after filter), the `enabled_formats` set, and the modal `View` (`Culling`/`Develop`/`Filter`). Methods (`next`, `prev`, `set_state`, `toggle_format`, `open_filter`/`close_filter`, `filter_next`/`filter_prev`, `toggle_current_filter`) are pure state mutations — no I/O beyond DB writes. This makes navigation, filter, and persistence logic unit-testable without touching ratatui.

The TUI half lives under `crates/terminalroom/src/tui/`:

- `tui::run` — terminal setup with a Drop guard, `Picker::from_query_stdio()` (with halfblocks fallback), an LRU preview cache (capacity 9 keyed by canonical path), and the main poll/draw loop.
- `tui::culling` — Option B layout: outer vertical split into main area + 1-row status line; inner horizontal split into preview area (left) and filmstrip (right, fixed width). Filmstrip is a `List` of text rows with state badges (`✓`/`✗`/`·`).
- `tui::develop` — placeholder paragraph.
- `tui::filter` — modal overlay rendered on top of the culling view: centered `Rect`, `Clear` widget, then a bordered `Block` containing the format list (`[x] JPEG  (12)` rows) and a footer hint.

Each headless module has unit tests that run without RAW fixtures or a TTY: `session` uses `tempfile` directories, `db` uses in-memory and temp-file SQLite, `preview` synthesizes JPEG bytes at test time via the `image` crate's encoder, and `app` exercises navigation/filter/state logic against in-memory DBs.

## Runtime Flow

1. Parse `terminalroom <path>`.
2. Resolve the input path.
3. If the input is a single image file, use that file as the session (rejected with an error if the extension is not supported).
4. If the input is a directory, scan immediate children, filter supported image extensions (RAW + JPEG/PNG/TIFF), sort by filename, and use the directory as the session root.
5. Open or create `<session-root>/.terminalroom.db`.
6. Upsert discovered file records.
7. Build the `App` (zips session files with DB-backed states, computes `available_formats` with per-format counts, seeds `enabled_formats` with all kinds present).
8. Start the culling view.
9. Decode previews on demand and cache them in memory (LRU, capacity 9) keyed by canonical path.
10. Persist every culling state change immediately.
11. Open the filter popup when `f` is pressed; toggling formats rebuilds the visible list, preserving the current selection by canonical path when possible.
12. Switch to the develop placeholder view when `d` is requested; `c` returns to culling.

For a single-file session, use the file's parent directory as the session root for `.terminalroom.db`.

## App State

The actual `App` struct (in `crates/terminalroom/src/app.rs`):

- `session_root: PathBuf` — directory containing `.terminalroom.db`.
- `files: Vec<FileEntry>` — every discovered file (1:1 with the scan), each with id, `DiscoveredFile`, and `CullingState`.
- `visible: Vec<usize>` — indices into `files`, after applying the current format filter.
- `cursor: usize` — index into `visible`.
- `view: View` — `Culling`, `Develop`, or `Filter`.
- `enabled_formats: BTreeSet<ImageKind>` — the live filter; mutated by `toggle_format`.
- `available_formats: Vec<(ImageKind, usize)>` — sorted format list with counts; computed once at init.
- `filter_cursor: usize` — popup-local row cursor.
- `status: Option<String>` — transient error/info line for the status bar.
- `db: Db` — owned SQLite handle for state writes.

The TUI layer (`tui::run`) owns ratatui-specific state alongside the `App`: the `Picker` and the `LruCache<PathBuf, StatefulProtocol>`. These are intentionally outside `App` so the headless logic stays free of ratatui types.

Avoid introducing plugin systems, async runtimes, or generalized editing pipelines before the develop view becomes real.

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

