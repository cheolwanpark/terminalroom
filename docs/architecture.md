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
        simd.rs
        space.rs
        pipeline.rs
        transform/
          mod.rs
          camera.rs
          encode.rs
          matrix.rs
          oklab.rs
        control/
          mod.rs
          input.rs
          tone.rs
          color.rs
          detail.rs
          look.rs
        primitive/
          mod.rs
          luminance.rs
          curve.rs
          mask.rs
          protect.rs
          blur.rs
          noise.rs
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

`codec` is a library crate. It owns the file ↔ memory-struct conversion and exposes two structs — `Image` (sRGB image-format files: JPEG/PNG/TIFF) and `Raw` (RAW files, header-only). Both carry shot-info; `Raw` additionally carries sensor-info. Pixel buffers are loaded lazily via `read_image_pixels` (sRGB) and `read_camera_linear` (planar f32 camera-linear). EXIF parsing for image-format files goes through `kamadak-exif`; for RAW it goes through libraw's `imgother`.

`darkroom` is a library crate. It owns the develop pipeline. Three traits structure it: `Transform` / `InPlaceTransform` (stateless A→B), `Control` (knob in a fixed `ColorSpace`), `Blend` (two-buffer, used by Look Strength). The buffer type is `Buffer<S>` — planar f32 in a single `Vec<f32>`, phantom-typed by `S: ColorSpace` so spaces don't mix at compile time. The pipeline orchestrator (`pipeline.rs`) chains transforms and controls in a fixed order driven by `DevelopParams`; the closed `Op` enum is reserved for future runtime-data-driven ordering.

`terminalroom` is a library + binary crate. The library half holds the headless modules (`session`, `db`, `app`) so they can be unit-tested without a TTY; `tui/` is the only module that depends on ratatui/crossterm. The binary half (`main.rs`) is a thin entry point: CLI parsing, then `App::init` and `tui::run`.

## Crate Boundary

Neither `codec`, `darkroom`, nor `terminalroom` may touch LibRaw C symbols directly. The `libraw-rs` surface is shaped around what the develop pipeline actually needs:

```rust
pub enum OutputColorSpace { Raw, Srgb, AdobeRgb, WideGamut, ProPhoto, Xyz, Aces, DciP3, Rec2020 }

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
pub fn read_demosaiced(path: &Path, opts: &DemosaicOptions, cancel: Option<&AtomicBool>) -> Result<DemosaicedRaw>;
```

`libraw-rs` exposes *capabilities*, not policy: callers pick which to use. `read_demosaiced` configures the output for true linear (`output_bps=16`, `gamm[0]=gamm[1]=1.0`, `no_auto_bright=1`) and applies camera WB only when requested. The develop pipeline drives `output_color = Raw` (libraw code 0) with `use_camera_wb = false` so darkroom owns the WB and camera→working transform end-to-end. Cancel is checked between FFI stages.

## Application Modules

`codec` exposes five modules under `crates/codec/src/`:

- `format` — the `ImageKind` enum (`Raw`, `Jpeg`, `Png`, `Tiff`), per-format extension lists, and `classify(extension) -> Option<ImageKind>`. Owned here because dispatch routes by `ImageKind`.
- `decode_image` — `Image { source, kind, width, height, orientation, shot_info }`, `decode_image(path)`, `read_image_pixels(img, target, cancel) -> Srgb8Pixels`.
- `decode_raw` — `Raw { source, width, height, shot_info, sensor_info }`, `decode_raw(path)`, `read_camera_linear(raw, half_size, cancel) -> CameraLinearPixels`. `read_camera_linear` drives libraw with `output_color=Raw` and `use_camera_wb=false`; the result is planar f32 (`R..R G..G B..B`) so it drops directly into the develop pipeline's planar SIMD kernels.
- `jpeg` — shared JPEG decoder helpers (`decode_jpeg_to_srgb8`, orientation utilities) for the image-format input pipeline.
- `metadata` — kamadak-exif wrapper that turns a TIFF/JPEG/PNG/HEIF container's EXIF segment into `ShotInfo` + orientation.

`Loaded` is the dispatched union (`Image | Raw`); `decode(path)` returns it. `Srgb8Pixels` (interleaved u8) and `CameraLinearPixels` (planar f32) carry the lazily-decoded pixel buffers.

`darkroom` exposes seven top-level modules under `crates/darkroom/src/`:

- `space` — `ColorSpace` marker trait + unit-struct tags (`CameraLinear`, `LinearRec2020`, `LinearSrgb`, `Oklab`, `Oklch`); the planar f32 `Buffer<S>` type with a single `Vec<f32>` laid out `R..R G..G B..B`; the `Srgb8` output struct.
- `transform` — `Transform` (general A→B) and `InPlaceTransform` (same-layout reinterpretation) traits, plus impls: `transform::matrix::{Rec2020ToSrgb, SrgbToRec2020}`, `transform::oklab::{LinearToOklab, OklabToLinear, OklabToOklch, OklchToOklab}`, `transform::camera::CameraToWorking` (per-channel WB followed by the offline-composed cam→Rec.2020 matrix), `transform::encode::{SrgbEncode, SrgbDecode}` (linear↔8-bit sRGB via a precomputed 16-bit→8-bit LUT private to the module).
- `control` — `Control` and `Blend` traits + closed `Op` enum + 12 controls grouped by stage: `input` (Exposure, Temperature, Tint), `tone` (Contrast, Shadows, Blacks, SoftHighlights{Tone,Chroma} — hue-preserving via Y-extract + tone curve + Y'/Y rescale), `color` (Warmth, Color), `detail` (Clarity, Grain), `look` (`Look` trait + `Identity`/`WarmMutedSoft` presets + `LookStrength` blend).
- `primitive` — shared building blocks: `luminance` (Rec.2020 / Rec.709 weights + the Y'/Y rescale), `curve::ToneCurve` (parametric S-curve over log-luminance), `mask` (smoothstep highlight/shadow/midtone/near-black masks), `protect` (skin/specular guards), `blur` (separable Gaussian for Clarity), `noise` (deterministic SplitMix-flavored hash for Grain).
- `simd` — `wide::f32x8` helpers tied to the planar layout: `map_f32x8` (one-channel in-place map), `map_pixel_f32x8` (three-channel per-pixel map), `apply_3x3_planar` (matrix multiply via splat + multiply). Tail handling pads to 8 lanes and writes back the prefix.
- `pipeline` — `DevelopParams` (the user-facing knob values + `fingerprint()` for cache invalidation), `develop_preview` (libraw `half_size=true` for fast culling), `develop_full` (full-resolution export). RAW path: `read_camera_linear` → resize → Temperature/Tint → CameraToWorking → Exposure → Look + LookStrength (in-linear blend) → tone batch → Rec2020ToSrgb → LinearToOklab → Warmth → OklabToOklch → SoftHighlightsChroma + Color + Clarity → OklchToOklab → OklabToLinear → Grain → SrgbEncode. Image-format path is decode + resize (no knobs in MVP).
- `common` — small leftover helpers used by the rest: `DevelopError`, `fit_within`, `resize_u8x3` (interleaved sRGB at the image-format input boundary), `resize_f32_planar` (per-plane via `fast_image_resize` `PixelType::F32`), `check_cancel`.

The headless half of `terminalroom` is split into three modules under `crates/terminalroom/src/`:

- `session` — resolves an input path into a `Session { root, files }`. Uses `darkroom::classify` (re-exported from `codec`) to identify supported extensions. Single file inputs use the parent directory as session root; directory inputs scan immediate children, filter by image extension, and sort by case-insensitive filename. Each `DiscoveredFile` carries an `ImageKind` tag so the preview pipeline and filter UI don't have to re-parse extensions. Single-file input rejects unsupported extensions. Provides `fingerprint()` (`<size>:<mtime>`) for change detection.
- `db` — owns the SQLite connection at `<root>/.terminalroom.db`. Handles `PRAGMA user_version` migrations, `sync_files` upsert (creates default `unset` culling rows; never deletes missing files), and `set_state` for culling state changes. `now_unix` is injected so callers control the clock.
- `app` — framework-agnostic state and update logic. Owns the `Db`, the `FileEntry` list, the `visible` index list (after filter), the `enabled_formats` set, the `develop_params: DevelopParams`, the `develop_cursor: usize` over the `DEVELOP_KNOBS` table, and the modal `View` (`Culling`/`Develop`/`Filter`). Methods (`next`, `prev`, `set_state`, `toggle_format`, `open_filter`/`close_filter`, `filter_next`/`filter_prev`, `toggle_current_filter`, `develop_next`/`develop_prev`, `develop_adjust`, `develop_reset`) are pure state mutations — no I/O beyond DB writes.

The TUI half lives under `crates/terminalroom/src/tui/`:

- `tui::run` — terminal setup with a Drop guard, `Picker::from_query_stdio()` (with halfblocks fallback). Spawns one worker thread that calls `codec::decode` + `pipeline::develop_preview`, plus one event-reader thread that pumps crossterm events through a channel. Main loop runs `crossbeam_channel::select!` over events, completed jobs, and a 100 ms tick. Cache is a single tier `LruCache<PathBuf, PreviewEntry>` where `PreviewEntry` holds `proto: StatefulProtocol`, the rendered source dimensions, the `TargetSize` it was rendered for, and the `params_fingerprint` of the `DevelopParams` that produced it. Selection change, ≥ 25% target change, or knob adjustment (params fingerprint mismatch) re-enqueues a job; stale generations and stale fingerprints are dropped on receive.
- `tui::culling` — Option B layout: outer vertical split into main area + 1-row status line; inner horizontal split into preview area (left) and filmstrip (right, fixed width). Preview rendering picks the cached `PreviewEntry` (or text placeholder) and computes a centered aspect-fit sub-rect (`aspect_fit_rect`) using `picker.font_size()` so landscape uses full preview width and portrait uses full preview height. Filmstrip is a `List` of text rows with state badges (`✓`/`✗`/`·`).
- `tui::develop` — knob list rendered as a `List` over `DEVELOP_KNOBS` with the focused row reverse-highlighted, plus a one-line keybind hint. The preview itself is the same cached `PreviewEntry` as the culling view (same worker, same cache; the params change is what triggers re-render).
- `tui::filter` — modal overlay rendered on top of the culling view: centered `Rect`, `Clear` widget, then a bordered `Block` containing the format list (`[x] JPEG  (12)` rows) and a footer hint.

Each headless module has unit tests that run without RAW fixtures or a TTY: `session` uses `tempfile` directories, `db` uses in-memory and temp-file SQLite, `decode_image` synthesizes JPEG/PNG/TIFF bytes at test time via the `image` crate's encoder, `darkroom::space`/`simd`/`transform`/`control`/`primitive`/`pipeline` exercise the planar SIMD kernels, OKLab round-trips, BT.2020 → BT.709 white preservation, hue-preserving rescale, knob no-op-at-default, ISO attenuation, deterministic noise, and the JPEG develop path against synthesized buffers; `app` exercises navigation/filter/state logic against in-memory DBs. RAW end-to-end tests are deferred until a fixture is checked in.

## Runtime Flow

1. Parse `terminalroom <path>`.
2. Resolve the input path.
3. If the input is a single image file, use that file as the session (rejected with an error if the extension is not supported).
4. If the input is a directory, scan immediate children, filter supported image extensions (RAW + JPEG/PNG/TIFF), sort by filename, and use the directory as the session root.
5. Open or create `<session-root>/.terminalroom.db`.
6. Upsert discovered file records.
7. Build the `App` (zips session files with DB-backed states, computes `available_formats` with per-format counts, seeds `enabled_formats` with all kinds present, initializes `develop_params` to `DevelopParams::default()`).
8. Start the culling view; spawn the worker thread and the event-reader thread.
9. On each loop iteration, recompute the target size from the current preview rect (cells × `picker.font_size()`) and the `DevelopParams::fingerprint()`. On selection change — on a target change ≥ 25% in either dim — or on params fingerprint change — bump a `current_generation`, flip the prior selection's `Arc<AtomicBool>` cancel flag, and enqueue a job for the new selection at the new target carrying a clone of `DevelopParams`.
10. The worker calls `codec::decode(path)` to get a `Loaded`, then `pipeline::develop_preview(&loaded, &params, target, cancel)` to produce an `Srgb8`. For RAW this calls `read_camera_linear` (libraw `half_size=true`, `output_color=Raw`, `use_camera_wb=false`), wraps in `Buffer<CameraLinear>`, resizes to target, applies `CameraToWorking` (WB + cam→Rec.2020 matrix from `SensorInfo`), runs the 12-knob chain, and encodes via `SrgbEncode`. For image files it lazy-decodes via `read_image_pixels` (JPEG IDCT scale; PNG/TIFF full decode) and resizes. Results come back through a channel; stale generations and stale `params_fingerprint` are dropped on receive. On the UI thread the `Srgb8` is wrapped in `DynamicImage::ImageRgb8`, handed to `picker.new_resize_protocol`, and stored in the cache alongside the source dims, the target, and the params fingerprint that produced it.
11. Persist every culling state change immediately.
12. Open the filter popup when `f` is pressed; toggling formats rebuilds the visible list, preserving the current selection by canonical path when possible.
13. Switch to the develop view when `d` is requested; `j/k` move between knobs, `h/l` adjust the focused knob by its step, `r` resets the focused knob to its default, `c` returns to culling. Each knob change updates the `DevelopParams` fingerprint and triggers a re-render of the current selection.

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
- `develop_params: DevelopParams` — the live knob values; cloned into each preview job. `DevelopParams::default()` is identity (no-op pipeline).
- `develop_cursor: usize` — index into `DEVELOP_KNOBS` (the static knob table) for the develop view's selection.

The TUI layer (`tui::run`) owns the concurrency-and-rendering state outside `App`: the `Picker`, the `LruCache<PathBuf, PreviewEntry>`, the `crossbeam_channel` senders/receivers, the `current_cancel: Option<Arc<AtomicBool>>`, the per-path `latest_generation: HashMap<PathBuf, u64>`, the `last_params_fp: Option<u64>` for change detection, and the worker/event threads. These are intentionally outside `App` so the headless logic stays free of ratatui and threading types.

Avoid introducing plugin systems, async runtimes, or generalized editing pipelines beyond the existing trait/Op surface.

## Preview Loading Strategy

The develop pipeline is the single preview path for both culling and develop view. The only difference is libraw's `half_size` flag and which `DevelopParams` is passed:

- **Culling**: `pipeline::develop_preview(loaded, &app.develop_params, target, cancel)` — libraw `half_size=true` (~4× faster demosaic); develop at TUI target resolution. The default `DevelopParams` is identity, so culling shows the neutral develop unless the user has been editing.
- **Develop view**: same call site, but the user has been adjusting `app.develop_params`; the cache invalidates on params fingerprint change so each knob press triggers a re-render.
- **Export** (post-MVP): `pipeline::develop_full(loaded, &params, target, cancel)` — libraw `half_size=false`, full or user-supplied target.

For RAW: `codec::read_camera_linear` calls libraw with `output_color=Raw`, `use_camera_wb=false`, `output_bps=16`, `gamma=1.0`, `no_auto_bright=1`. The result is interleaved u16 from libraw, deinterleaved into a planar f32 `Vec<f32>` (length `3 * w * h`) at the codec boundary. From there the develop pipeline owns the buffer.

For image-format files (JPEG/PNG/TIFF) the worker calls `read_image_pixels` on every (re-)render, scaled to the preview target: JPEG goes through `jpeg-decoder`'s `.scale()` IDCT factor (1/2/4/8); PNG/TIFF go through `image::ImageReader` at native resolution. EXIF orientation is applied at decode time via `kamadak-exif`. The MVP image-format path applies no knobs — it's identity color-wise.

For RAW orientation: `read_demosaiced` is auto-rotated by libraw itself (default `params.user_flip = -1`); we don't read EXIF separately.

Each LibRaw handler processes one file at a time. The current MVP runs one worker thread; multiple workers would need separate `libraw_data_t` handles per thread (libraw is not reentrant on a single handle).

## Color Pipeline

The two paths handle color very differently:

- **Image** (JPEG/PNG/TIFF input): the source is already in display-referred sRGB. Decoding hands back gamma-encoded sRGB 8-bit. The pipeline just resizes — no color-space conversion, no gamma round-trip. (Per-knob image work is post-MVP.)
- **RAW** (`pipeline::develop_raw_full`): the source is camera-native linear (libraw `output_color=Raw`, no WB, no matrix). The pipeline:
  1. Wraps the planar f32 in `Buffer<CameraLinear>` and resizes to target.
  2. Applies Temperature/Tint as per-channel gain on camera-linear (before WB, so the user is adjusting illuminant rather than fighting it).
  3. `CameraToWorking` applies the WB multipliers (G-normalized) and the camera→Rec.2020 matrix (composed offline as `xyz_to_rec2020(D65) · cam_to_xyz`), producing `Buffer<LinearRec2020>`.
  4. Exposure is a uniform gain in working linear.
  5. Look (currently `Identity` or `WarmMutedSoft`) applies in linear Rec.2020, with `LookStrength` blending in linear toward neutral.
  6. Tone fine-tune (Contrast, Soft-Highlights tone, Shadows, Blacks) operates in linear Rec.2020 by extracting Y (Rec.2020 weights), evaluating a curve in log2-EV space, and scaling RGB by Y'/Y. This preserves hue without a separate log-luma buffer state.
  7. `Rec2020ToSrgb` (BT.2020 → BT.709 primaries matrix in linear light) → `LinearToOklab`. Warmth applies as a luminance-weighted b-axis offset in OKLab.
  8. `OklabToOklch`. Soft-Highlights chroma desaturates highlights; Color does vibrance-aware chroma scaling with skin/specular guards and an ISO cap; Clarity is an unsharp mask on the L channel via a separable Gaussian, midtone-mask weighted.
  9. `OklchToOklab` → `OklabToLinear` (back to linear sRGB primaries). Grain adds deterministic luma noise here, in linear, before the sRGB transfer.
  10. `SrgbEncode` clamps to [0, 1], scales to 16-bit linear, looks up the precomputed sRGB transfer LUT, and interleaves R/G/B into the output bytes.

The chain has **two color-space round-trips total**: linear ↔ OKLab once for color/detail, OKLab → linear for the final encode. Tone work happens in linear Rec.2020 directly; Clarity reuses OKLCh's L without an extra conversion. We do not double-clip wide-gamut content through sRGB primaries — we go `sensor → Rec.2020-linear` via the camera→XYZ matrix and the Rec.2020 primaries, then `Rec.2020-linear → sRGB-linear → sRGB-8bit` only at the very end.

## Error Handling

MVP errors should be visible and recoverable:

- If a file fails to decode, keep it in the list and show an error placeholder in the image area.
- If DB open or migration fails, exit with a clear error.
- If no supported files are found, exit with a clear message.
- If the terminal does not support rich image protocols, fall back to ratatui-image halfblocks.
