# Architecture

## Workspace Shape

The project is a Cargo workspace with four crates:

```text
terminalroom/
  Cargo.toml
  crates/
    libraw-rs/
      Cargo.toml
      build.rs
      wrapper.c
      src/
        lib.rs
    codec/
      Cargo.toml
      src/
        lib.rs
        format.rs
        decode.rs
    darkroom/
      Cargo.toml
      src/
        lib.rs
        develop.rs
    terminalroom/
      Cargo.toml
      src/
        main.rs
        lib.rs
        session.rs
        db.rs
        app.rs
        tui/
          mod.rs
          culling.rs
          develop.rs
          filter.rs
```

Dependency direction is strictly linear: `libraw-rs` → `codec` → `darkroom` → `terminalroom`. Each crate has a single responsibility; nothing reaches across stages.

`libraw-rs` is a library crate. It owns LibRaw linking, bindings, unsafe calls, pointer lifetimes, and conversion into safe owned Rust buffers. The new `wrapper.c` (compiled via the `cc` build-dep) provides `tr_set_half_size` / `tr_set_use_camera_wb` — the two `imgdata.params` fields libraw lacks public C setters for.

`codec` is a library crate. It owns the `ImageKind` taxonomy and the file-to-pixels dispatch. RAW goes through libraw-rs (preferring the camera-embedded JPEG when present, falling back to linear demosaic). JPEG goes through `jpeg-decoder` with IDCT `.scale()` for sub-resolution decode. PNG/TIFF go through `image::ImageReader`. EXIF orientation is applied here so downstream layers always see a correctly-rotated buffer. Output is `DecodedImage::{Linear { u16 } | Srgb8 { u8 }}`.

`darkroom` is a library crate. It owns the develop pipeline: `develop(decoded, target, cancel) -> RgbImage` and the convenience `develop_to_rgb(path, kind, target, cancel)`. The Linear branch runs SIMD resize on 16-bit linear data and applies the sRGB transfer via a precomputed LUT; the Srgb8 branch is a SIMD downscale only. Re-exports `codec::{ImageKind, classify, TargetSize, DecodedImage}` so `terminalroom` only imports darkroom.

`terminalroom` is a library + binary crate. The library half holds the headless modules (`session`, `db`, `app`) so they can be unit-tested without a TTY; `tui/` is the only module that depends on ratatui/crossterm. The binary half (`main.rs`) is a thin entry point: CLI parsing, then `App::init` and `tui::run`.

## Crate Boundary

Neither `codec`, `darkroom`, nor `terminalroom` may touch LibRaw C symbols directly. The `libraw-rs` surface is shaped around what the develop pipeline actually needs:

```rust
pub struct RawMetadata {
    pub width: u32,
    pub height: u32,
    pub make: Option<String>,
    pub model: Option<String>,
}

pub struct LinearImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u16>,        // 3-ch RGB, sRGB primaries, gamma 1.0, host endian
}

pub struct LinearOptions {
    pub half_size: bool,        // 1/4 pixel count
    pub use_camera_wb: bool,
    pub user_qual: u8,          // 0=bilinear, 3=AHD
}

pub struct EmbeddedJpeg {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

pub fn read_metadata(path: &Path) -> Result<RawMetadata>;
pub fn read_linear(path: &Path, opts: &LinearOptions, cancel: Option<&AtomicBool>) -> Result<LinearImage>;
pub fn read_embedded_jpeg(path: &Path) -> Result<Option<EmbeddedJpeg>>;
```

`libraw-rs` exposes *capabilities*, not policy: callers pick which to use. `read_linear` configures libraw for true linear output (`output_bps=16`, `gamm[0]=gamm[1]=1.0`, `no_auto_bright=1`, `output_color=1` for sRGB primaries) and applies camera WB when requested. Cancel is checked between FFI calls (granularity: per-stage, not preemptive). `read_embedded_jpeg` returns `Ok(None)` for files that carry a bitmap-only thumb or no thumb at all — the linear path is the fallback.

## Application Modules

`codec` exposes two modules under `crates/codec/src/`:

- `format` — the `ImageKind` enum (`Raw`, `Jpeg`, `Png`, `Tiff`), per-format extension lists (RAW: arw/cr2/cr3/dng/nef/nrw/raf/raw/rw2/orf/pef/srw — plus jpg/jpeg/png/tif/tiff), and `classify(extension) -> Option<ImageKind>`. Owned here because dispatch routes by `ImageKind`.
- `decode` — `decode(path, kind, target, cancel) -> DecodedImage`. RAW first tries `libraw_rs::read_embedded_jpeg`; on `Some(jpeg)`, decodes the bytes via the same JPEG path standalone files use. On `None`, falls through to `read_metadata` (for the `half_size` decision based on target vs sensor) + `read_linear` with `user_qual=0`. JPEG uses `jpeg_decoder::Decoder` + `.scale(target_w, target_h)` (IDCT factor 1/2/4/8). PNG/TIFF use `image::ImageReader::open(...).into_decoder()` so EXIF orientation is read from the decoder. Orientation is applied via `image::DynamicImage::apply_orientation`; width/height come back swapped for 90°/270° rotations.

`darkroom` exposes one module under `crates/darkroom/src/`:

- `develop` — pure compute. Consumes `DecodedImage`, computes a final size via `fit_within(src, target)` (never upscales), runs `fast_image_resize::Resizer` over `PixelType::U16x3` (linear) or `PixelType::U8x3` (sRGB), then for the linear branch applies the sRGB transfer via a precomputed `[u8; 65536]` LUT. Returns `RgbImage { width, height, pixels: Vec<u8> }`. `develop_to_rgb(path, kind, target, cancel)` chains `codec::decode` + `develop`.

The headless half of `terminalroom` is split into three modules under `crates/terminalroom/src/`:

- `session` — resolves an input path into a `Session { root, files }`. Uses `darkroom::classify` (re-exported from `codec`) to identify supported extensions. Single file inputs use the parent directory as session root; directory inputs scan immediate children, filter by image extension, and sort by case-insensitive filename. Each `DiscoveredFile` carries an `ImageKind` tag so the preview pipeline and filter UI don't have to re-parse extensions. Single-file input rejects unsupported extensions. Provides `fingerprint()` (`<size>:<mtime>`) for change detection.
- `db` — owns the SQLite connection at `<root>/.terminalroom.db`. Handles `PRAGMA user_version` migrations, `sync_files` upsert (creates default `unset` culling rows; never deletes missing files), and `set_state` for culling state changes. `now_unix` is injected so callers control the clock.
- `app` — framework-agnostic state and update logic. Owns the `Db`, the `FileEntry` list, the `visible` index list (after filter), the `enabled_formats` set, and the modal `View` (`Culling`/`Develop`/`Filter`). Methods (`next`, `prev`, `set_state`, `toggle_format`, `open_filter`/`close_filter`, `filter_next`/`filter_prev`, `toggle_current_filter`) are pure state mutations — no I/O beyond DB writes. This makes navigation, filter, and persistence logic unit-testable without touching ratatui.

The TUI half lives under `crates/terminalroom/src/tui/`:

- `tui::run` — terminal setup with a Drop guard, `Picker::from_query_stdio()` (with halfblocks fallback). Spawns one worker thread that calls `darkroom::develop_to_rgb` and one event-reader thread that pumps crossterm events through a channel. Main loop runs `crossbeam_channel::select!` over events, completed jobs, and a 100ms tick. Cache is `LruCache<PathBuf, PreviewSlot>`; `PreviewSlot` holds an optional `PreviewEntry { proto, src_w, src_h }` for each tier (fast/full).
- `tui::culling` — Option B layout: outer vertical split into main area + 1-row status line; inner horizontal split into preview area (left) and filmstrip (right, fixed width). Preview rendering picks `slot.full → slot.fast → text placeholder` and computes a centered aspect-fit sub-rect (`aspect_fit_rect`) using `picker.font_size()` so landscape uses full width and portrait uses full height. Filmstrip is a `List` of text rows with state badges (`✓`/`✗`/`·`).
- `tui::develop` — placeholder paragraph.
- `tui::filter` — modal overlay rendered on top of the culling view: centered `Rect`, `Clear` widget, then a bordered `Block` containing the format list (`[x] JPEG  (12)` rows) and a footer hint.

Each headless module has unit tests that run without RAW fixtures or a TTY: `session` uses `tempfile` directories, `db` uses in-memory and temp-file SQLite, `codec::decode` synthesizes JPEG/PNG/TIFF bytes at test time via the `image` crate's encoder, `darkroom::develop` exercises the linear → sRGB midtone and the no-upscale rule against synthesized buffers, and `app` exercises navigation/filter/state logic against in-memory DBs.

## Runtime Flow

1. Parse `terminalroom <path>`.
2. Resolve the input path.
3. If the input is a single image file, use that file as the session (rejected with an error if the extension is not supported).
4. If the input is a directory, scan immediate children, filter supported image extensions (RAW + JPEG/PNG/TIFF), sort by filename, and use the directory as the session root.
5. Open or create `<session-root>/.terminalroom.db`.
6. Upsert discovered file records.
7. Build the `App` (zips session files with DB-backed states, computes `available_formats` with per-format counts, seeds `enabled_formats` with all kinds present).
8. Start the culling view; spawn the worker thread and the event-reader thread.
9. On each loop iteration, recompute the target size from the current preview rect (cells × `picker.font_size()`). On selection change — or on a target change ≥ 25% in either dim — bump a `current_generation`, flip the prior selection's `Arc<AtomicBool>` cancel flag, and enqueue two jobs for the new selection: a fast tier at `target/4` (1/16 px) and a full tier at `target`. Both jobs share the new generation's cancel flag.
10. The worker calls `darkroom::develop_to_rgb` for each job. Results come back through a channel; stale generations (the user has moved on) are dropped on receive. On the UI thread the `RgbImage` is wrapped in `DynamicImage::ImageRgb8`, handed to `picker.new_resize_protocol`, and stored alongside `(src_w, src_h)` in the `PreviewSlot`'s `fast` or `full` slot.
11. Persist every culling state change immediately.
12. Open the filter popup when `f` is pressed; toggling formats rebuilds the visible list, preserving the current selection by canonical path when possible.
13. Switch to the develop placeholder view when `d` is requested; `c` returns to culling.

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

The TUI layer (`tui::run`) owns the concurrency-and-rendering state outside `App`: the `Picker`, the `LruCache<PathBuf, PreviewSlot>`, the `crossbeam_channel` senders/receivers, the `current_cancel: Option<Arc<AtomicBool>>`, the per-path `latest_generation: HashMap<PathBuf, u64>`, and the worker/event threads. These are intentionally outside `App` so the headless logic stays free of ratatui and threading types.

Avoid introducing plugin systems, async runtimes, or generalized editing pipelines before the develop view becomes real.

## Preview Loading Strategy

For RAW, prefer the camera-embedded JPEG when present (`libraw_rs::read_embedded_jpeg`). It skips `libraw_unpack` and `libraw_dcraw_process` — the long pole on large RAW files — by reading only the small thumbnail stream most cameras embed. Fall back to the linear demosaic path (`read_linear`) when no JPEG thumb is available:

1. Open the RAW file with a fresh LibRaw handler.
2. Set linear-output params (`output_bps=16`, `gamm[0]=gamm[1]=1.0`, `no_auto_bright=1`, `output_color=1`) and the speed knobs (`half_size` based on target vs sensor, `use_camera_wb=true`, `user_qual=0` bilinear).
3. `libraw_unpack` → `libraw_dcraw_process` → `libraw_dcraw_make_mem_image`.
4. Reinterpret the returned packed RGB16 buffer as `Vec<u16>` in host byte order.
5. Always close the LibRaw handler on drop (RAII `RawHandle`).

For both paths, EXIF orientation is applied in `codec::decode` (the embedded-JPEG path reads the EXIF chunk from `jpeg-decoder::exif_data`; the standalone-image path uses `ImageDecoder::orientation`). The libraw linear path is auto-rotated by libraw itself (default `params.user_flip = -1`) so the buffer arrives correctly oriented.

Each LibRaw handler processes one file at a time. The current MVP runs one worker thread; multiple workers would need separate `libraw_data_t` handles per thread (libraw is not reentrant on a single handle).

## Error Handling

MVP errors should be visible and recoverable:

- If a file fails to decode, keep it in the list and show an error placeholder in the image area.
- If DB open or migration fails, exit with a clear error.
- If no RAW files are found, exit with a clear message.
- If the terminal does not support rich image protocols, fall back to ratatui-image halfblocks.

