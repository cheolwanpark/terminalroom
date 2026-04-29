# Dependency Notes

These notes document the intended first-use pattern for the dependencies chosen for the MVP.

## LibRaw

Use LibRaw through its C API from `libraw-rs`. The C API wraps the C++ API and keeps the Rust FFI boundary simpler.

Important calls for the first milestone:

- `libraw_init`: create a handler.
- `libraw_open_file`: open the RAW file and read metadata.
- `libraw_unpack_thumb`: unpack an embedded thumbnail.
- `libraw_dcraw_make_mem_thumb`: get thumbnail bytes.
- `libraw_dcraw_clear_mem`: free processed image buffers returned by LibRaw.
- `libraw_unpack`, `libraw_dcraw_process`, and `libraw_dcraw_make_mem_image`: fallback processed preview path.
- `libraw_close`: free the handler.
- `libraw_strerror`: convert LibRaw error codes into messages.

Linking should prefer the thread-safe LibRaw variant where the platform exposes it. On Unix-like systems this is commonly `-lraw_r`; `-lraw` is the ordinary variant. `build.rs` should use `pkg-config` or documented environment overrides rather than hard-coded paths.

`libraw-rs` should expose owned Rust buffers only. It must not expose LibRaw pointers, LibRaw-owned memory, or raw C structs to `terminalroom`.

## ratatui

Use ratatui as the immediate-mode terminal renderer. Every draw pass renders the full UI from current app state.

Realized MVP structure (in `tui/`):

- Terminal init/teardown via explicit crossterm calls (`enable_raw_mode`, `EnterAlternateScreen`) wrapped in a Drop guard so the terminal is restored even on panic.
- Single main loop that alternates `event::poll(100ms)` → `app.on_key(...)` and `terminal.draw(...)`.
- Three view renderers: `culling`, `develop`, `filter`. The filter view is rendered as a `Clear`-backed modal overlay on top of the culling view.
- Rendering functions are pure: they read from `App` and write to the `Frame`. DB writes and preview decoding happen in the event/loop step (`app.set_state`, `ensure_preview_loaded`), never inside `draw`.

Ratatui does not own input handling. Crossterm keyboard events are dispatched per-view (`KeyCode::Char('p')`, etc.).

## image

Use the `image` crate as the application crate's pixel-data layer. The MVP needs JPEG decoding for LibRaw embedded thumbnails and on-disk decoding for non-RAW image files, so build with defaults off and only the formats we ship support for:

- `image = { version = "0.25", default-features = false, features = ["jpeg", "png", "tiff"] }`

`preview.rs` exposes two pieces:

- `decode_preview(libraw_rs::PreviewImage)` — pure conversion, no I/O. JPEG bytes go through `image::load_from_memory_with_format`; packed `Rgb8` 3-channel/8-bit becomes `DynamicImage::ImageRgb8`. Other RGB layouts (4 channels, 16 bits per channel) currently return `UnsupportedRgb`.
- `load_preview(path, ImageKind)` — the high-level loader the TUI calls. Routes `ImageKind::Raw` through `libraw_rs::read_preview` + `decode_preview`; routes `Jpeg`/`Png`/`Tiff` through `image::ImageReader::open(path).with_guessed_format().decode()`.

Keeping JPEG decoding in the application crate preserves the `libraw-rs` boundary: the FFI crate stays free of image-processing dependencies, and the `image` crate is the only place that opens non-RAW files.

## ratatui-image

Use ratatui-image to render preview images in the terminal. It supports multiple terminal graphics protocols and falls back to text-based rendering.

Realized MVP behavior:

- `Picker::from_query_stdio()` is called once at TUI startup; if it fails (terminal does not respond to the query), the TUI falls back to `Picker::from_fontsize((1, 2))` and surfaces the warning in the status line. ratatui-image then renders with halfblocks.
- `StatefulProtocol` instances are stored in an `LruCache<PathBuf, StatefulProtocol>` (capacity 9), owned by `tui::run` and not by `App`. Cache entries are produced by `picker.new_resize_protocol(image)` after `preview::load_preview` succeeds.
- The main image area uses `StatefulImage::default().resize(Resize::Fit(None))`. The widget handles area-aware resize/encode internally; we don't touch the cache on terminal-resize events.
- `ensure_preview_loaded` runs once per loop iteration before `draw`. On cache miss it loads synchronously; if loading fails, a text placeholder is shown and the error is captured in `App::status`.

The right-side filmstrip is text-only with state badges (`✓`/`✗`/`·`) — no per-row image rendering. This keeps the cache small and avoids per-frame encoding for thumbnails. Real mini thumbnails are deferred.

## SQLite and rusqlite

Use SQLite through `rusqlite` for `.terminalroom.db`.

SQLite is the default choice because:

- The DB file is easy to inspect with external tools.
- Schema migrations are straightforward through `PRAGMA user_version`.
- Future develop settings will likely need structured queries.
- The culling state is small, relational, and path-oriented.

Use one connection on the UI thread for the first MVP. If preview decoding moves to worker threads, keep DB access on the app thread unless there is a concrete performance issue.

## sled Comparison

sled is a viable embedded key-value store, but it is not the MVP default. It is less convenient for ad hoc inspection, migrations, and future queries like "show all picked files from this session".

Revisit sled only if SQLite becomes a proven bottleneck or if the app later needs a key-value event log.

