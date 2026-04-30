# Terminalroom Docs

Terminalroom is a terminal UI for culling and developing photographs. The first milestone:

- Run `terminalroom <path>`.
- Load image files (RAW + JPEG/PNG/TIFF) from the target file or directory.
- Show a culling view with a main preview and a vertical filmstrip on the right.
- Filter the visible files by format through a modal popup (key `f`).
- Persist culling decisions in `<path>/.terminalroom.db`.
- Provide a placeholder develop view.

## Documents

- [Architecture](architecture.md): crate split, runtime flow, and ownership boundaries.
- [Dependencies](dependencies.md): practical notes for LibRaw, ratatui, ratatui-image, and SQLite.
- [Storage](storage.md): SQLite database location and first schema.
- [MVP UX](mvp-ux.md): command flow, views, keybindings, and acceptance criteria.

## Current Decisions

- Use a Cargo workspace with four crates: `libraw-rs` (FFI), `codec` (file → pixel buffer with format taxonomy), `darkroom` (develop pipeline), and `terminalroom` (library + binary). The library half of `terminalroom` holds headless modules (`session`, `db`, `app`) so they can be unit-tested without a TTY; `tui/` contains the ratatui rendering, the worker thread, and the event loop.
- Dependency chain: `libraw-rs` → `codec` → `darkroom` → `terminalroom`. Strict linear pipeline; each crate has a single responsibility.
- Keep all LibRaw FFI and unsafe code inside `libraw-rs`. The crate exposes only decoding capabilities (`read_linear`, `read_embedded_jpeg`) — orchestration policy lives in `codec`.
- `codec` owns the `ImageKind` taxonomy and dispatches by kind. JPEG decodes through `jpeg-decoder` with IDCT `.scale()` for fast sub-resolution decode; PNG/TIFF through `image::ImageReader`. EXIF orientation is applied here (via `image::metadata::Orientation` + `DynamicImage::apply_orientation`) so the develop layer always sees a correctly-rotated buffer.
- For RAW, prefer the camera-embedded JPEG when present (skips libraw's `unpack`/`dcraw_process`, the long pole) and fall back to `read_linear` + demosaic.
- `darkroom` runs SIMD resize via `fast_image_resize` (U16x3 for linear → resize → sRGB-gamma LUT → 8-bit; U8x3 for already-sRGB). The output is `RgbImage`; the TUI never sees `image` types directly.
- TUI preview loading runs on a dedicated worker thread fed by a `crossbeam-channel`; selection change enqueues a fast tier (1/16 px) and a full tier (preview-area px), bumps a generation, and flips an `Arc<AtomicBool>` cancel flag for any prior generation. Cache is `LruCache<PathBuf, PreviewSlot { fast, full }>`; render preference is full → fast → text placeholder.
- TUI preview rendering uses ratatui-image's `Resize::Scale` so the small fast-tier buffer is upscaled to match the full-tier display rect. A centered, aspect-fit sub-rect (computed against `picker.font_size()`) gives landscape full width / vertically centered and portrait full height / horizontally centered.
- Use SQLite through `rusqlite` (bundled feature) for `.terminalroom.db`.
- Scan only the direct children of a directory for the first MVP. Recursive scanning is deferred.
- Culling layout: Option B — vertical filmstrip on the right of the main preview (text labels with state badges, no per-row image rendering).
- Filter is a session-only modal popup. Toggling rebuilds the visible list without rescanning; selection survives toggles when the current file is still visible.

## Implementation Status

- [x] `libraw-rs` FFI surface and safe wrappers (`read_metadata`, `read_linear`, `read_embedded_jpeg`).
- [x] `codec::format` — `ImageKind` taxonomy and extension-based `classify`.
- [x] `codec::decode` — `DecodedImage::{Linear | Srgb8}`, RAW with embedded-JPEG fast path + demosaic fallback, JPEG with IDCT scale, PNG/TIFF via `image::ImageReader`, EXIF orientation applied.
- [x] `darkroom::develop` — SIMD resize, sRGB gamma LUT, `develop` and `develop_to_rgb` entry points.
- [x] `session` — file scanning (RAW + JPEG/PNG/TIFF), sort, fingerprint.
- [x] `db` — SQLite v1 schema, migrations, `sync_files`, `set_state`.
- [x] Culling TUI (ratatui + ratatui-image, worker thread + two-tier cache + cancellation, format filter popup, aspect-fit centered preview).
- [x] Develop view placeholder.
- [ ] End-to-end smoke test against a real RAW fixture.

