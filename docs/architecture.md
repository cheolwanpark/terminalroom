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
        decode_image.rs
        decode_raw.rs
        jpeg.rs
        metadata.rs
    darkroom/
      Cargo.toml
      src/
        lib.rs
        common.rs
        image_develop.rs
        raw_develop.rs
        thumbnail.rs
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

`libraw-rs` is a library crate. It owns LibRaw linking, bindings, unsafe calls, pointer lifetimes, and conversion into safe owned Rust buffers. `wrapper.c` (compiled via the `cc` build-dep) exposes per-field accessors over `libraw_data_t.{idata, sizes, color, other}` that the C API doesn't surface as getters, plus `tr_set_half_size` / `tr_set_use_camera_wb` for the two `imgdata.params` fields lacking public C setters.

`codec` is a library crate. It owns the file ↔ memory-struct conversion and exposes two structs — `Image` (sRGB image-format files: JPEG/PNG/TIFF) and `Raw` (RAW files, linear Rec.2020). Both carry shot-info; `Raw` additionally carries sensor-info and a camera-embedded JPEG thumbnail eagerly decoded to sRGB. Pixel buffers are loaded lazily via `read_image_pixels` and `read_raw_pixels`. EXIF parsing for image-format files goes through `kamadak-exif`; for RAW it goes through libraw's `imgother`.

`darkroom` is a library crate. It owns the develop pipeline: two independent developers (`image_develop`, `raw_develop`) over shared helpers (`common`). `image_develop::develop` is identity color-wise (sRGB → sRGB); `raw_develop::develop` resizes in linear Rec.2020 16-bit, applies the BT.2020 → BT.709 primaries matrix in linear light, then encodes to 8-bit sRGB via the precomputed transfer LUT. `develop_thumbnail` and `develop_culling` cover the culling-view fast paths.

`terminalroom` is a library + binary crate. The library half holds the headless modules (`session`, `db`, `app`) so they can be unit-tested without a TTY; `tui/` is the only module that depends on ratatui/crossterm. The binary half (`main.rs`) is a thin entry point: CLI parsing, then `App::init` and `tui::run`.

## Crate Boundary

Neither `codec`, `darkroom`, nor `terminalroom` may touch LibRaw C symbols directly. The `libraw-rs` surface is shaped around what the develop pipeline actually needs:

```rust
pub enum OutputColorSpace { Srgb, AdobeRgb, WideGamut, ProPhoto, Xyz, Aces, DciP3, Rec2020 }

pub struct ShotInfo {
    pub make: Option<String>,
    pub model: Option<String>,
    pub iso: Option<f32>,
    pub shutter: Option<f32>,        // seconds
    pub aperture: Option<f32>,       // f-number
    pub focal_length: Option<f32>,   // mm
}

pub struct SensorInfo {
    pub raw_width: u32, pub raw_height: u32,
    pub active_area: Rect,           // x, y, width, height
    pub crop_area: Option<Rect>,     // libraw raw_inset_crops[0]
    pub orientation: i32,            // libraw flip code (0/3/5/6)
    pub cfa: CfaPattern,             // Bayer | XTrans | Mono | Other
    pub black_level: BlackLevel,     // global + per-channel
    pub white_level: WhiteLevel,     // saturation + per-channel
    pub camera_wb: [f32; 4],         // cam_mul (as-shot)
    pub daylight_wb: [f32; 4],       // pre_mul
    pub cam_to_xyz: [[f32; 3]; 4],   // camera RGB → XYZ (D50 by libraw convention)
}

pub struct EmbeddedThumb { pub width: u32, pub height: u32, pub jpeg_bytes: Vec<u8> }

pub struct DemosaicOptions {
    pub output_color: OutputColorSpace,
    pub half_size: bool,
    pub use_camera_wb: bool,
    pub user_qual: u8,               // 0=bilinear, 3=AHD
}

pub struct DemosaicedRaw {
    pub width: u32, pub height: u32,
    pub pixels: Vec<u16>,            // 3-ch RGB, gamma 1.0, host endian
    pub color_space: OutputColorSpace,
}

pub fn read_header(path: &Path) -> Result<(ShotInfo, SensorInfo)>;
pub fn read_thumbnail(path: &Path) -> Result<Option<EmbeddedThumb>>;
pub fn read_demosaiced(path: &Path, opts: &DemosaicOptions, cancel: Option<&AtomicBool>) -> Result<DemosaicedRaw>;
```

`libraw-rs` exposes *capabilities*, not policy: callers pick which to use. `read_demosaiced` configures the output for true linear (`output_bps=16`, `gamm[0]=gamm[1]=1.0`, `no_auto_bright=1`) and applies camera WB when requested; the output color space is whichever `OutputColorSpace` was passed (we use `Rec2020`). Cancel is checked between FFI stages.

## Application Modules

`codec` exposes five modules under `crates/codec/src/`:

- `format` — the `ImageKind` enum (`Raw`, `Jpeg`, `Png`, `Tiff`), per-format extension lists, and `classify(extension) -> Option<ImageKind>`. Owned here because dispatch routes by `ImageKind`.
- `decode_image` — `Image { source, kind, width, height, orientation, shot_info }`, `decode_image(path)`, `read_image_pixels(img, target, cancel) -> Srgb8Pixels`. Image-format files have no eager preview — they decode fast enough that holding a thumbnail isn't worth the memory.
- `decode_raw` — `Raw { source, width, height, shot_info, sensor_info, preview }`, `decode_raw(path)`, `read_raw_pixels(raw, target, cancel) -> LinearRec2020Pixels`. The `preview: Option<Thumbnail>` field is decoded eagerly from the camera-embedded JPEG via libraw's thumb path; orientation comes from the embedded JPEG's own EXIF tag, with the libraw `flip` code as the fallback.
- `jpeg` — shared JPEG decoders (`decode_jpeg_to_srgb8`, `decode_jpeg_bytes_to_thumbnail`) used for both standalone JPEGs and RAW embedded thumbnails.
- `metadata` — kamadak-exif wrapper that turns a TIFF/JPEG/PNG/HEIF container's EXIF segment into `ShotInfo` + orientation.

`Loaded` is the dispatched union (`Image | Raw`); `decode(path)` returns it. `Srgb8Pixels` and `LinearRec2020Pixels` carry the lazily-decoded pixel buffers.

`darkroom` exposes four modules under `crates/darkroom/src/`:

- `common` — pure compute helpers: `fit_within`, `resize_u8x3`, `resize_u16x3` (both via `fast_image_resize`), `srgb_lut` (precomputed `[u8; 65536]` for the sRGB transfer), `linear_to_srgb8`, `rec2020_to_srgb_matrix` (BT.2020 → BT.709 primaries, D65), `apply_3x3_u16` (in-place per-pixel matrix with clamping), and the `RgbImage`/`DevelopError` types.
- `image_develop` — `develop(image, target, cancel) -> RgbImage`. Reads sRGB pixels lazily, resizes to fit target. Identity color-wise.
- `raw_develop` — `develop(raw, target, cancel) -> RgbImage`. Reads linear Rec.2020 pixels lazily, resizes in 16-bit linear, applies the Rec.2020 → sRGB primaries matrix in linear light, encodes via the sRGB transfer LUT to 8-bit. The Rec.2020 → sRGB step is the only color-space conversion, and it happens once.
- `thumbnail` — `develop_thumbnail(loaded, target) -> Option<RgbImage>`. Fast path: resizes the eagerly-loaded thumbnail when present (RAW files). Returns `None` for image-format files (no eager preview).

`darkroom::develop_culling(loaded, target, cancel)` is the dispatcher used by the TUI: thumbnail when available, full developer otherwise.

The headless half of `terminalroom` is split into three modules under `crates/terminalroom/src/`:

- `session` — resolves an input path into a `Session { root, files }`. Uses `darkroom::classify` (re-exported from `codec`) to identify supported extensions. Single file inputs use the parent directory as session root; directory inputs scan immediate children, filter by image extension, and sort by case-insensitive filename. Each `DiscoveredFile` carries an `ImageKind` tag so the preview pipeline and filter UI don't have to re-parse extensions. Single-file input rejects unsupported extensions. Provides `fingerprint()` (`<size>:<mtime>`) for change detection.
- `db` — owns the SQLite connection at `<root>/.terminalroom.db`. Handles `PRAGMA user_version` migrations, `sync_files` upsert (creates default `unset` culling rows; never deletes missing files), and `set_state` for culling state changes. `now_unix` is injected so callers control the clock.
- `app` — framework-agnostic state and update logic. Owns the `Db`, the `FileEntry` list, the `visible` index list (after filter), the `enabled_formats` set, and the modal `View` (`Culling`/`Develop`/`Filter`). Methods (`next`, `prev`, `set_state`, `toggle_format`, `open_filter`/`close_filter`, `filter_next`/`filter_prev`, `toggle_current_filter`) are pure state mutations — no I/O beyond DB writes. This makes navigation, filter, and persistence logic unit-testable without touching ratatui.

The TUI half lives under `crates/terminalroom/src/tui/`:

- `tui::run` — terminal setup with a Drop guard, `Picker::from_query_stdio()` (with halfblocks fallback). Spawns one worker thread that calls `darkroom::decode` + `darkroom::develop_culling`, plus one event-reader thread that pumps crossterm events through a channel. Main loop runs `crossbeam_channel::select!` over events, completed jobs, and a 100 ms tick. Cache is a single tier `LruCache<PathBuf, PreviewEntry>` where `PreviewEntry` holds `proto: StatefulProtocol`, the rendered source dimensions, and the `TargetSize` it was rendered for.
- `tui::culling` — Option B layout: outer vertical split into main area + 1-row status line; inner horizontal split into preview area (left) and filmstrip (right, fixed width). Preview rendering picks the cached `PreviewEntry` (or text placeholder) and computes a centered aspect-fit sub-rect (`aspect_fit_rect`) using `picker.font_size()` so landscape uses full preview width and portrait uses full preview height. Filmstrip is a `List` of text rows with state badges (`✓`/`✗`/`·`).
- `tui::develop` — placeholder paragraph.
- `tui::filter` — modal overlay rendered on top of the culling view: centered `Rect`, `Clear` widget, then a bordered `Block` containing the format list (`[x] JPEG  (12)` rows) and a footer hint.

Each headless module has unit tests that run without RAW fixtures or a TTY: `session` uses `tempfile` directories, `db` uses in-memory and temp-file SQLite, `decode_image` synthesizes JPEG/PNG/TIFF bytes at test time via the `image` crate's encoder, `darkroom::common`/`image_develop`/`thumbnail` exercise the linear → sRGB midtone, the BT.2020 → BT.709 clamping, and the no-upscale rule against synthesized buffers, and `app` exercises navigation/filter/state logic against in-memory DBs. RAW tests are deferred until a fixture is checked in.

## Runtime Flow

1. Parse `terminalroom <path>`.
2. Resolve the input path.
3. If the input is a single image file, use that file as the session (rejected with an error if the extension is not supported).
4. If the input is a directory, scan immediate children, filter supported image extensions (RAW + JPEG/PNG/TIFF), sort by filename, and use the directory as the session root.
5. Open or create `<session-root>/.terminalroom.db`.
6. Upsert discovered file records.
7. Build the `App` (zips session files with DB-backed states, computes `available_formats` with per-format counts, seeds `enabled_formats` with all kinds present).
8. Start the culling view; spawn the worker thread and the event-reader thread.
9. On each loop iteration, recompute the target size from the current preview rect (cells × `picker.font_size()`). On selection change — or on a target change ≥ 25% in either dim — bump a `current_generation`, flip the prior selection's `Arc<AtomicBool>` cancel flag, and enqueue a job for the new selection at the new target.
10. The worker calls `darkroom::decode(path)` to get a `Loaded`, then `darkroom::develop_culling(&loaded, target, cancel)` to produce an `RgbImage`. For RAW this resizes the eager embedded-JPEG thumbnail; for image files this lazy-decodes via `read_image_pixels` (JPEG IDCT scale; PNG/TIFF full decode) and resizes. Results come back through a channel; stale generations are dropped on receive. On the UI thread the `RgbImage` is wrapped in `DynamicImage::ImageRgb8`, handed to `picker.new_resize_protocol`, and stored in the cache alongside the source dims and the target it was rendered for.
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

The TUI layer (`tui::run`) owns the concurrency-and-rendering state outside `App`: the `Picker`, the `LruCache<PathBuf, PreviewEntry>`, the `crossbeam_channel` senders/receivers, the `current_cancel: Option<Arc<AtomicBool>>`, the per-path `latest_generation: HashMap<PathBuf, u64>`, and the worker/event threads. These are intentionally outside `App` so the headless logic stays free of ratatui and threading types.

Avoid introducing plugin systems, async runtimes, or generalized editing pipelines before the develop view becomes real.

## Preview Loading Strategy

For RAW, prefer the camera-embedded JPEG when present (`libraw_rs::read_thumbnail`). It skips `libraw_unpack` and `libraw_dcraw_process` — the long pole on large RAW files — by reading only the small thumbnail stream most cameras embed. The thumbnail is decoded once, eagerly, and stored on `Raw.preview`; the culling view re-resizes it for whatever target size the terminal currently has.

For image-format files (JPEG/PNG/TIFF), there is no eager preview. The culling worker calls `read_image_pixels` on every (re-)render, scaled to the preview target. This is fast enough: JPEG goes through `jpeg-decoder`'s `.scale()` IDCT factor (1/2/4/8); PNG/TIFF go through `image::ImageReader` at native resolution. Skipping the eager-preview indirection keeps the data model simple — image files have only metadata, no thumbnail.

The full RAW develop path (`libraw_rs::read_demosaiced` with `output_color=Rec2020` → 16-bit linear → SIMD resize → BT.2020→BT.709 matrix → sRGB transfer LUT → 8-bit) is wired into `darkroom::raw_develop` for use by the develop view (post-MVP). It is also the fallback inside `develop_culling` when a RAW file has no embedded JPEG thumb (rare on modern bodies).

For both decoded thumbnails and standalone JPEGs, EXIF orientation is applied at decode time. Standalone JPEGs go through `kamadak-exif` (in `codec::metadata`) for shot-info and orientation; embedded RAW thumbnails read orientation from the JPEG's own EXIF chunk via `jpeg_decoder::Decoder::exif_data()` + `parse_orientation_from_tiff_chunk`, with the libraw `flip` code as the fallback when the embedded JPEG carries no EXIF. The `libraw_rs::read_demosaiced` path is auto-rotated by libraw itself (default `params.user_flip = -1`).

Each LibRaw handler processes one file at a time. The current MVP runs one worker thread; multiple workers would need separate `libraw_data_t` handles per thread (libraw is not reentrant on a single handle).

## Color Pipeline

The two developers handle color very differently:

- **Image** (`image_develop`): the source is already in display-referred sRGB. Decoding hands back gamma-encoded sRGB 8-bit. The developer just resizes — no color-space conversion, no gamma round-trip.
- **Raw** (`raw_develop`): the source is linear Rec.2020 16-bit (libraw `output_color=8`). The developer:
  1. Resizes in 16-bit linear (Rec.2020).
  2. Applies the BT.2020 → BT.709 primaries matrix in linear light (per-pixel 3×3 matrix multiply, clamped to [0, 65535]).
  3. Encodes via the sRGB transfer LUT to 8-bit.

  The matrix step is the only color-space conversion in the pipeline. We do not go `sensor → sRGB-linear → Rec.2020 → sRGB-linear → sRGB-8bit`, which would double-clip wide-gamut content through sRGB primaries. We go directly `sensor → Rec.2020-linear` (libraw, using the camera→XYZ matrix and the Rec.2020 primaries), then `Rec.2020-linear → sRGB-linear → sRGB-8bit` (darkroom).

## Error Handling

MVP errors should be visible and recoverable:

- If a file fails to decode, keep it in the list and show an error placeholder in the image area.
- If DB open or migration fails, exit with a clear error.
- If no supported files are found, exit with a clear message.
- If the terminal does not support rich image protocols, fall back to ratatui-image halfblocks.
