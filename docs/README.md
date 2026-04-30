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
- [Dependencies](dependencies.md): practical notes for LibRaw, kamadak-exif, ratatui, ratatui-image, and SQLite.
- [Storage](storage.md): SQLite database location and first schema.
- [MVP UX](mvp-ux.md): command flow, views, keybindings, and acceptance criteria.

## Current Decisions

- Use a Cargo workspace with four crates: `libraw-rs` (FFI), `codec` (file → memory struct with shot/sensor metadata), `darkroom` (develop pipeline), and `terminalroom` (library + binary). The library half of `terminalroom` holds headless modules (`session`, `db`, `app`) so they can be unit-tested without a TTY; `tui/` contains the ratatui rendering, the worker thread, and the event loop.
- Dependency chain: `libraw-rs` → `codec` → `darkroom` → `terminalroom`. Strict linear pipeline; each crate has a single responsibility.
- Keep all LibRaw FFI and unsafe code inside `libraw-rs`. The crate exposes capabilities (`read_header`, `read_thumbnail`, `read_demosaiced`) plus the metadata types (`ShotInfo`, `SensorInfo`, `OutputColorSpace`, `CfaPattern`, etc.) — orchestration policy lives in `codec`.
- Two memory structs in `codec`, one per source kind:
  - `Image` for JPEG/PNG/TIFF — sRGB color space, header-only (no eager preview); pixels lazy via `read_image_pixels` → `Srgb8Pixels`.
  - `Raw` for RAW files — header + camera-embedded JPEG thumbnail (eager); pixels lazy via `read_raw_pixels` → `LinearRec2020Pixels`.
  - `Loaded` is the dispatched union; `decode(path)` picks one based on extension.
- **RAW pixel data is loaded directly to linear Rec.2020** via libraw `output_color=8`. We do **not** go through linear sRGB and then convert — that would clip wide-gamut content through sRGB primaries. The Rec.2020 → BT.709/sRGB primaries matrix is applied once in `darkroom::raw_develop`, in linear light, before the sRGB transfer.
- Shot info (make/model/ISO/shutter/aperture/focal length) is parsed for both kinds. RAW reads it from libraw's `imgother`; image-format files read it from EXIF via `kamadak-exif`. Sensor info (black/white levels, CFA pattern, camera/daylight WB, color matrix, active area, crop area, orientation) is RAW-only.
- `darkroom` has two independent developers (`image_develop`, `raw_develop`) sharing helpers (`common`: SIMD resize, sRGB transfer LUT, Rec.2020 → sRGB matrix). `develop_thumbnail` is the fast culling path for RAW (resize the eager thumbnail). `develop_culling` is the dispatcher used by the TUI: thumbnail when present, otherwise the full pipeline.
- TUI preview loading runs on a dedicated worker thread fed by a `crossbeam-channel`. Each job decodes the header and runs `develop_culling` for the requested target size; the cache is `LruCache<PathBuf, PreviewEntry>` keyed by canonical path. Selection change or significant target change re-enqueues the job; stale generations are dropped on receive.
- TUI preview rendering uses ratatui-image's `Resize::Scale` and a centered, aspect-fit sub-rect (computed against `picker.font_size()`) so landscape full-fills the width and portrait full-fills the height.
- Use SQLite through `rusqlite` (bundled feature) for `.terminalroom.db`.
- Scan only the direct children of a directory for the first MVP. Recursive scanning is deferred.
- Culling layout: Option B — vertical filmstrip on the right of the main preview (text labels with state badges, no per-row image rendering).
- Filter is a session-only modal popup. Toggling rebuilds the visible list without rescanning; selection survives toggles when the current file is still visible.

## Implementation Status

- [x] `libraw-rs` FFI surface: `read_header`, `read_thumbnail`, `read_demosaiced` plus `ShotInfo`/`SensorInfo`/`CfaPattern`/`BlackLevel`/`WhiteLevel`/`OutputColorSpace`. C accessor wrappers in `wrapper.c` cover `imgdata.idata` / `sizes` / `color` / `other`.
- [x] `codec::format` — `ImageKind` taxonomy and extension-based `classify`.
- [x] `codec::decode_image` — `Image` header (shot-info from EXIF via `kamadak-exif`); `read_image_pixels` for lazy decode (JPEG via `jpeg-decoder` IDCT scale; PNG/TIFF via `image::ImageReader`).
- [x] `codec::decode_raw` — `Raw` header (shot-info + sensor-info from libraw, embedded JPEG thumbnail eagerly decoded with EXIF/libraw-flip orientation); `read_raw_pixels` returns linear Rec.2020 16 bpc.
- [x] `darkroom::common` — `fit_within`, `resize_u8x3`, `resize_u16x3`, `srgb_lut`, `linear_to_srgb8`, `rec2020_to_srgb_matrix`, `apply_3x3_u16`.
- [x] `darkroom::image_develop` — sRGB → resize → `RgbImage`.
- [x] `darkroom::raw_develop` — linear Rec.2020 → resize → BT.2020 → BT.709 matrix → sRGB transfer LUT → `RgbImage`.
- [x] `darkroom::develop_thumbnail` and `darkroom::develop_culling` (dispatcher).
- [x] `session` — file scanning (RAW + JPEG/PNG/TIFF), sort, fingerprint.
- [x] `db` — SQLite v1 schema, migrations, `sync_files`, `set_state`.
- [x] Culling TUI (ratatui + ratatui-image, single worker + RgbImage cache, format filter popup, aspect-fit centered preview).
- [x] Develop view placeholder.
- [ ] End-to-end smoke test against a real RAW fixture.

