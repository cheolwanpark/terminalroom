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
        paths.rs
        session.rs
        db.rs
        cache.rs
        app.rs
        tui/
          mod.rs
          banner.rs
          preview.rs
          develop.rs
          info.rs
          filmstrip.rs
          status.rs
          filter.rs
```

Dependency direction is strictly linear: `libraw-rs` → `codec` → `darkroom` → `terminalroom`. Each crate has a single responsibility; nothing reaches across stages.

`libraw-rs` is a library crate. It owns LibRaw linking, bindings, unsafe calls, pointer lifetimes, and conversion into safe owned Rust buffers. `wrapper.c` (compiled via the `cc` build-dep) exposes per-field accessors over `libraw_data_t.{idata, sizes, color, other}` that the C API doesn't surface as getters, plus `tr_set_half_size` / `tr_set_use_camera_wb` / `tr_set_no_auto_scale` for the `imgdata.params` fields the develop path needs to override.

`codec` is a library crate. It owns the file ↔ memory-struct conversion and exposes two structs — `Image` (sRGB image-format files: JPEG/PNG/TIFF) and `Raw` (RAW files, header-only). Both carry shot-info; `Raw` additionally carries sensor-info. Pixel buffers are loaded lazily via `read_image_pixels` (sRGB) and `read_camera_linear` (planar f32 camera-linear). EXIF parsing for image-format files goes through `kamadak-exif`; for RAW it goes through libraw's `imgother`.

`darkroom` is a library crate. It owns the develop pipeline. Three traits structure it: `Transform` / `InPlaceTransform` (stateless A→B), `Control` (knob in a fixed `ColorSpace`), `Blend` (two-buffer, used by Look Strength). The buffer type is `Buffer<S>` — planar f32 in a single `Vec<f32>`, phantom-typed by `S: ColorSpace` so spaces don't mix at compile time. The pipeline orchestrator (`pipeline.rs`) chains transforms and controls in a fixed order driven by `DevelopParams`; the closed `Op` enum is reserved for future runtime-data-driven ordering.

`terminalroom` is a library + binary crate. The library half holds the headless modules (`paths`, `session`, `db`, `cache`, `app`) so they can be unit-tested without a TTY; `tui/` is the only module that depends on ratatui/crossterm. The binary half (`main.rs`) is a thin entry point: CLI parsing, open the global `~/.terminalroom/db.sqlite`, upsert each discovered file, prune cache orphans, then `App::init` and `tui::run`.

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
    pub cam_to_xyz: [[f32; 3]; 4],   // libraw cam_xyz: XYZ → camera space
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

`libraw-rs` exposes *capabilities*, not policy: callers pick which to use. `read_demosaiced` configures the output for true linear (`output_bps=16`, `gamm[0]=gamm[1]=1.0`, `no_auto_bright=1`) and applies camera WB only when requested. The develop pipeline drives `output_color = Raw` (libraw code 0) with `use_camera_wb = false` and Raw-mode `no_auto_scale = true` so darkroom owns the WB and camera→working transform end-to-end from a black-subtracted, unbalanced buffer. Cancel is checked between FFI stages.

## Application Modules

`codec` exposes five modules under `crates/codec/src/`:

- `format` — the `ImageKind` enum (`Raw`, `Jpeg`, `Png`, `Tiff`), per-format extension lists, and `classify(extension) -> Option<ImageKind>`. Owned here because dispatch routes by `ImageKind`.
- `decode_image` — `Image { source, kind, width, height, orientation, shot_info }`, `decode_image(path)`, `read_image_pixels(img, target, cancel) -> Srgb8Pixels`.
- `decode_raw` — `Raw { source, width, height, shot_info, sensor_info }`, `decode_raw(path)`, `read_camera_linear(raw, half_size, cancel) -> CameraLinearPixels`. `read_camera_linear` drives libraw with `output_color=Raw`, `use_camera_wb=false`, and Raw-mode `no_auto_scale=true`; the result is planar f32 (`R..R G..G B..B`) normalized against sensor white after black subtraction, so it drops directly into the develop pipeline's planar SIMD kernels.
- `jpeg` — shared JPEG decoder helpers (`decode_jpeg_to_srgb8`, orientation utilities) for the image-format input pipeline.
- `metadata` — kamadak-exif wrapper that turns a TIFF/JPEG/PNG/HEIF container's EXIF segment into `ShotInfo` + orientation.

`Loaded` is the dispatched union (`Image | Raw`); `decode(path)` returns it. `Srgb8Pixels` (interleaved u8) and `CameraLinearPixels` (planar f32) carry the lazily-decoded pixel buffers.

`darkroom` exposes seven top-level modules under `crates/darkroom/src/`:

- `space` — `ColorSpace` marker trait + unit-struct tags (`CameraLinear`, `LinearRec2020`, `LinearSrgb`, `Oklab`, `Oklch`); the planar f32 `Buffer<S>` type with a single `Vec<f32>` laid out `R..R G..G B..B`; the `Srgb8` output struct.
- `transform` — `Transform` (general A→B) and `InPlaceTransform` (same-layout reinterpretation) traits, plus impls: `transform::matrix::{Rec2020ToSrgb, SrgbToRec2020}`, `transform::oklab::{LinearToOklab, OklabToLinear, OklabToOklch, OklchToOklab}`, `transform::camera::CameraToWorking` (per-channel WB followed by a LibRaw-aligned camera→Rec.2020 matrix built from `cam_xyz` via row normalization, pseudoinverse, and sRGB→Rec.2020 composition), `transform::encode::{SrgbEncode, SrgbDecode}` (linear↔8-bit sRGB via a precomputed 16-bit→8-bit LUT private to the module).
- `control` — `Control` and `Blend` traits + closed `Op` enum + 12 controls grouped by stage: `input` (Exposure, Temperature, Tint), `tone` (Contrast, Shadows, Blacks, SoftHighlights{Tone,Chroma} — hue-preserving via Y-extract + tone curve + Y'/Y rescale), `color` (Warmth, Color), `detail` (Clarity, Grain), `look` (`Identity` static no-op + `LookRegistry` runtime registry of XMP-driven recipes resolved by slug). After the 4-knob redesign the live pipeline only invokes Temperature/Tint/Exposure + the configured Look. The other 8 controls stay compiled and tested as primitives the upcoming XMP applier will compose; see `docs/looks.md`. The previous `WarmMutedSoft` built-in look is gone — non-trivial looks come from XMP sidecars registered via the TUI.
- `primitive` — shared building blocks: `luminance` (Rec.2020 / Rec.709 weights + the Y'/Y rescale), `curve::ToneCurve` (parametric S-curve over log-luminance), `mask` (smoothstep highlight/shadow/midtone/near-black masks), `protect` (skin/specular guards), `blur` (separable Gaussian for Clarity), `noise` (deterministic SplitMix-flavored hash for Grain).
- `simd` — `wide::f32x8` helpers tied to the planar layout: `map_f32x8` (one-channel in-place map), `map_pixel_f32x8` (three-channel per-pixel map), `apply_3x3_planar` (matrix multiply via splat + multiply). Tail handling pads to 8 lanes and writes back the prefix.
- `pipeline` — `DevelopParams` (5 user-facing knob values: `exposure_ev`, `temperature_kelvin`, `tint`, `look`, `look_strength`; `fingerprint()` hashes those exactly), `develop_preview` (libraw `half_size=true` for fast culling), `develop_full` (full-resolution export). All three take `&LookRegistry` so the worker can resolve `params.look` to either `Identity` or a registered XMP recipe. RAW path: `read_camera_linear` → resize → Temperature/Tint → CameraToWorking → Exposure → `apply_xmp_with_strength` → Rec2020ToSrgb → SrgbEncode. Image-format path: `read_image_pixels` (decode + resize) → `SrgbDecode` → Temperature/Tint as per-channel gain (the buffer is re-tagged from `LinearSrgb` to `CameraLinear`) → `SrgbToRec2020` → identical chain from Exposure onward. The OKLab/OKLch round-trip and the eight removed knobs (Warmth/Color/Contrast/SoftHighlights/Shadows/Blacks/Clarity/Grain) are no longer invoked; they will resurface composed inside `transform::xmp::ApplyXmp` (currently a no-op stub). See `docs/looks.md`.
- `common` — small leftover helpers used by the rest: `DevelopError`, `fit_within`, `resize_u8x3` (interleaved sRGB at the image-format input boundary), `resize_f32_planar` (per-plane via `fast_image_resize` `PixelType::F32`), `check_cancel`.

The headless half of `terminalroom` is split into five modules under `crates/terminalroom/src/`:

- `paths` — resolves `~/.terminalroom/`, `~/.terminalroom/db.sqlite`, and `~/.terminalroom/cache/`. Creates the directories on first call.
- `session` — resolves an input path into a `Session { root, files }`. Uses `darkroom::classify` (re-exported from `codec`) to identify supported extensions. Single file inputs use the parent directory as session root; directory inputs scan immediate children, filter by image extension, and sort by case-insensitive filename. Each `DiscoveredFile` carries an `ImageKind` tag so the preview pipeline and filter UI don't have to re-parse extensions. Single-file input rejects unsupported extensions.
- `db` — owns the global SQLite connection at `~/.terminalroom/db.sqlite`. Handles `PRAGMA user_version` migrations (v0→v2 fresh, v1→v2 upgrade) and exposes per-file ops: `upsert_file` (idempotent insert, recomputes `source_fingerprint` and clears `cache_key` if it changed), `load_for_path`, `set_removed`, `update_params` (DevelopParams JSON + fingerprint), `set_cache_key`, `touch_access`, `count_cached`, `oldest_cached`, `all_cache_keys`, plus the v2 looks ops: `insert_look`, `list_looks`, `find_look_by_fp`, `delete_look`. `now_unix` is injected so callers control the clock.
- `cache` — owns `~/.terminalroom/cache/<key>.cache` files (BLAKE3-of-canonical-path keyed). Atomic write through `<key>.cache.tmp` + rename. TRC1 header carries source/params fingerprints, dimensions, and a CRC32 of the pixel bytes — any mismatch unlinks the file and clears the DB pointer. LRU eviction (default cap 500) runs after every successful insert, deleting the oldest entries by `last_access_unix_seconds`. Startup `prune_orphans()` walks the cache directory and unlinks any `*.cache` file whose stem isn't referenced by `files.cache_key`.
- `app` — framework-agnostic state and update logic. Owns the `Db`, the `FileEntry` list (each carries persisted `develop_params`, `develop_params_fp`, `source_fp`, and `removed`), the `visible` index list (after filter + show-removed), the `enabled_formats` set, the `develop_params: DevelopParams` live editing buffer, the `develop_cursor: usize` over the 5-entry `DEVELOP_KNOBS` table (Exposure, Temperature, Tint, Look, Look Strength), the `view: View` (`Main`/`Filter`/`Looks`), the `focus: Focus` (`Navigation`/`Develop`), the `show_removed: bool`, a `file_meta: HashMap<PathBuf, FileMeta>` cache, the `looks: Vec<LookRow>` library sorted by name, the `looks_cursor: usize`, and the `look_registry: Arc<LookRegistry>` shared with the worker. Methods (`next`, `prev`, `remove_current`, `restore_current`, `toggle_show_removed`, `commit_develop_params`, `sync_develop_params_from_current`, `toggle_format`, `open_filter`/`close_filter`, `filter_next`/`filter_prev`, `toggle_current_filter`, `enter_develop`/`exit_develop`, `develop_next`/`develop_prev`, `develop_adjust` (numeric clamp or — for the discrete `LookSelector` knob — slug cycle), `develop_reset`, `open_looks`/`close_looks`, `looks_next`/`looks_prev`, `looks_apply_to_current`, `reconcile_looks_with_dir`) are pure state mutations — DB writes through `App.db` are limited to `set_removed`, the disk-cache-hit `touch_access`, and the looks library CRUD inside `reconcile_looks_with_dir`. `update_params` and the on-disk-cache `insert` no longer run on the UI thread; the TUI ships them to a save worker that owns its own `Db` connection.

The TUI half lives under `crates/terminalroom/src/tui/`:

- `tui::run` — terminal setup with a Drop guard, `Picker::from_query_stdio()` (with halfblocks fallback). Spawns three threads beyond the main loop: a develop worker that calls `codec::decode` + `pipeline::prepare_source` + `pipeline::apply_pipeline`; an event-reader thread that pumps crossterm events through a channel; and a save worker that owns its own `Db` connection (opened from the same global path) plus a `Clone` of the on-disk cache. Channels: `bounded(1)` for foreground develop jobs (with `try_send` — the cancel-flag pattern supersedes stale work), `bounded(2)` for prefetch jobs (cursor±1 warm-up), unbounded for events / `JobDone` / source-cache events / save messages. The develop worker drains the foreground channel via `try_recv` first, then `select!`s over both channels — foreground starvation of prefetch is intentional. Three preview cache tiers: a worker-private `LruCache<SourceKey, Arc<PreparedSource>>` (cap 3, `SourceKey = (path, source_fp, target_bucket)`) holding the post-decode + post-resize `Buffer<CameraLinear>` so knob ticks reuse it; a hot `LruCache<PathBuf, PreviewEntry>` (cap 15) on the main thread where `PreviewEntry` holds `proto: StatefulProtocol`, source dims, the `TargetSize` rendered for, and the `params_fingerprint`; and the warm on-disk `Cache` from `crate::cache`. Main loop steps each iteration: (0) drain `event_rx.try_recv()` through `process_key`, which routes nav keys (j/k/↑/↓ in Navigation focus) to a `NavCoalesce` rate-limiter and everything else to `handle_key`; (1) detect pending develop edit and run a tiered debounce — `DEBOUNCE_HOT = 50 ms` when the active path is in the worker's source cache, `DEBOUNCE_COLD = 250 ms` otherwise; (2) on debounce expiry call `flush_pending_develop`, which commits in-memory immediately and queues a `SaveMsg::Params` for the save worker; (3) compute selection/target/render-fp deltas; both disk-cache installs and worker dispatch are gated on `NAV_SETTLE = 150 ms` of nav-input silence (anchored on `nav.last_input_at`, not on cursor moves, so a held burst stays unsettled even when rate-limiting drops events between advances); enqueue prefetch ±1 with each entry's persisted `develop_params` (also gated on settle); (4) update `displayed_path` (only when settled and the cursor's path is in `mem_cache`) and draw; (5) `select!` on `done_rx` / `cache_rx` / single-event `event_rx` / `default(timeout)` with the timeout bounded by the remaining debounce, the remaining nav-settle window, and the `TICK` (100 ms). On shutdown, drain the in-memory tier through `release_terminal_image` to free kitty graphics, drop the develop and prefetch senders, drop the save sender, and `join` the save worker before `run` returns so pending writes always land. `handle_job_done` first installs the rendered preview into the in-memory tier (popping any prior entry for that path and emitting kitty deletes for both the prior and any LRU-evicted entry), then ships a `SaveMsg::CacheBlob` to the save worker; foreground vs prefetch results are distinguished by a `JobKind` field so prefetch hits go straight into the hot cache (and the prefetch-cancel HashMap is cleaned). Each completed `JobDone` also carries an `Option<FileMeta>` (shot info + dims + size + kind) that gets written into `app.file_meta`.
- `tui::draw` (in `mod.rs`) — top-level vertical layout: banner / main / status. Inner main is a horizontal split: preview (`Min(20)`) | Develop tab (28 cells) | Image Info tab (28 cells) | Navigation tab / filmstrip (28 cells). Banner height adapts to terminal width via `banner::height_for`. The filter popup is a modal overlay drawn on top when `app.view == View::Filter`. Key dispatch keys on `(view, focus)`: filter view → `handle_filter_key`; main view + Navigation focus → `handle_navigation_key`; main view + Develop focus → `handle_develop_key`. A shared `tab_block(title, focused)` helper produces the focus-aware border (thick yellow when focused, default otherwise).
- `tui::banner` — multi-line `TERMINALROOM` ASCII figlet (ANSI-Shadow style) with a `height_for(width)` helper and a single-row stylized fallback when the terminal is narrower than the banner.
- `tui::preview` — preview-area rendering. Owns `aspect_fit_rect(area, src_w, src_h, font_size)` which computes a centered aspect-fit sub-rect using `picker.font_size()` so landscape uses full preview width and portrait uses full preview height. Looks up the cached `PreviewEntry` by `displayed_path` (passed in from `tui::run`), not by the live cursor's path, so the pane stays frozen on the previously-settled image while a held-key nav burst is in flight; renders via `StatefulImage` or a text placeholder.
- `tui::develop` — Develop tab column: knob list rendered as a `List` over `DEVELOP_KNOBS`. Border, label/value styles, and highlight symbol all switch on `app.focus == Focus::Develop` (focused: thick yellow border, bold values, `▶ ` cursor; unfocused: default border, dim values, no cursor symbol).
- `tui::info` — Image Info tab column. Reads `app.file_meta.get(&path)` for the current selection and renders two sections: `Shoot` (Make / Model / ISO / Shutter / Aperture / Focal — only fields that are `Some`) and `File` (Name / Format / Size / Dims / Orient.). Shows a single dim "loading…" line when meta is not yet cached. Read-only — never gets focus highlight.
- `tui::filmstrip` — Navigation tab column: filmstrip `List` of text rows. Removed entries render dim with a red `R` badge; non-removed entries render plain. Border switches on `app.focus == Focus::Navigation` like the Develop tab.
- `tui::status` — bottom 1-row status line. Composes `<filename>  i/N  [REMOVED]  [filter:e/t]` and a focus-aware shortcut hint ("j/k navigate · x remove · r restore · R show-removed:{on|off} · f filter · enter develop · q quit" in Navigation focus; "j/k knob · h/l adjust · r reset · esc back · q quit" in Develop focus). The `REMOVED` span is shown bold red only when the current entry is removed (which requires `show_removed = true` to be visible). Errors in `app.status` replace the shortcut hint until cleared.
- `tui::filter` — modal overlay rendered on top of the main layout: centered `Rect`, `Clear` widget, then a bordered `Block` containing the format list (`[x] JPEG  (12)` rows) and a footer hint.

Each headless module has unit tests that run without RAW fixtures or a TTY: `session` uses `tempfile` directories, `db` uses in-memory SQLite (`upsert_file` idempotency, `set_removed` round-trip, `update_params` JSON round-trip, `oldest_cached` ordering, source-fingerprint cache invalidation), `cache` uses temp-dir + in-memory DB (insert/get round-trip, source/params fingerprint mismatch, magic-byte corruption rejection, eviction at cap, orphan pruning), `decode_image` synthesizes JPEG/PNG/TIFF bytes at test time via the `image` crate's encoder, `darkroom::space`/`simd`/`transform`/`control`/`primitive`/`pipeline` exercise the planar SIMD kernels, OKLab round-trips, BT.2020 → BT.709 white preservation, hue-preserving rescale, knob no-op-at-default, ISO attenuation, deterministic noise, and the JPEG develop path against synthesized buffers; `app` exercises navigation/filter/remove/restore/show-removed/per-file params logic against in-memory DBs. The TUI submodules have small isolated tests too — `tui::preview::aspect_fit_rect` (landscape/portrait/non-square cells/zero-dim safety), `tui::banner` (banner row-width invariant + width-fallback), `tui::info` (shutter/byte/truncate formatters). RAW end-to-end tests are deferred until a fixture is checked in.

## Runtime Flow

1. Parse `terminalroom <path>`.
2. Resolve the input path.
3. If the input is a single image file, use that file as the session (rejected with an error if the extension is not supported).
4. If the input is a directory, scan immediate children, filter supported image extensions (RAW + JPEG/PNG/TIFF), sort by filename, and use the directory as the session root.
5. Open the global `~/.terminalroom/db.sqlite` (`Db::open_global`), creating `~/.terminalroom/` and the schema on first run.
6. `Db::upsert_file` for each discovered file (recomputes `source_fingerprint`; clears `cache_key` if it changed). Construct `Cache::new()` (creates `~/.terminalroom/cache/`) and run `prune_orphans` to delete cache files whose stem isn't referenced by `files.cache_key`.
7. Build the `App` (zips session files with their DB-backed `FileRow`s, computes `available_formats` with per-format counts, seeds `enabled_formats` with all kinds present, initializes `develop_params` to the first file's persisted params, sets `view = Main`, `focus = Navigation`, `show_removed = false`, and starts with an empty `file_meta` cache).
8. Start the TUI; spawn the develop worker, the event-reader thread, and a save worker. The save worker opens its own `Db` connection from the same `~/.terminalroom/db.sqlite` (WAL + `PRAGMA busy_timeout = 5000`) and receives a `Clone` of the on-disk `Cache`; the develop worker keeps its own private `LruCache<SourceKey, Arc<PreparedSource>>` (cap 3) for prepared-source reuse.
9. On each loop iteration:
   - Drain `event_rx.try_recv()` through `process_key`, which routes Navigation-focus j/k/↑/↓ to a `NavCoalesce` rate-limiter (first event of a fresh burst always advances; continuation events within `NAV_BURST_GAP = 350 ms` are throttled to one advance per `NAV_SLOW_INTERVAL = 200 ms` for the first `NAV_RAMP_DURATION = 1 s`, then advance per-event) and everything else to `handle_key`. The coalescer updates `nav.last_input_at` on every nav event (advance or skipped), which serves as the "user is still pressing keys" signal. Any `Quit` signal flushes pending edits through the save worker and breaks.
   - Detect any pending develop edit (live `app.develop_params.fingerprint()` differs from the current entry's `develop_params_fp`). The debounce is tiered: `DEBOUNCE_HOT = 50 ms` when the active path is in the worker's source cache, `DEBOUNCE_COLD = 250 ms` otherwise. On expiry, `flush_pending_develop` calls `app.commit_develop_params(fp)` immediately (so subsequent renders see the new fp) and queues a `SaveMsg::Params` for the save worker — no DB I/O on the UI thread.
   - Recompute the target size from the current preview rect (terminal width minus the three 28-cell side tabs minus 2 preview borders, terminal height minus the banner height minus the status row minus 2 preview borders, multiplied by `picker.font_size()`).
   - Compute `nav_settled = nav.last_input_at.elapsed() >= NAV_SETTLE`. On selection change — or a target change ≥ 25% in either dim — or a committed params fingerprint change — always cancel any in-flight foreground job; then try the in-memory `LruCache`. On hit, mark resolved. On miss *and only if `nav_settled`*, try the on-disk `Cache::get`; on hit install into the in-memory tier (popping any prior entry for the path and emitting kitty deletes for prior + LRU-evicted), on miss dispatch a `Job` to the `bounded(1)` foreground channel via `try_send` (a full channel just means the queued job is already stale and will be cancelled by the next iteration), tagged `JobKind::Foreground`. While unsettled, both disk-cache install and dispatch are deferred — `last_id`/`last_target` aren't bumped, so the next iteration retries once the user releases the key. After the foreground decision, `update_prefetch` cancels prefetches outside the cursor±1 window and queues new ones (each carrying that entry's persisted `develop_params`) on the `bounded(2)` prefetch channel — also gated on settle.
   - Update `displayed_path`: when `nav_settled` is true and the cursor's current path is present in the in-memory tier, set `displayed_path = current_path`. Otherwise leave it pinned to the last value. `displayed_path` is what `tui::preview::render` uses to look up the cache, so the preview pane stays frozen on the prior settled image during a burst — and stays on the prior image even after settle until the new path's preview lands in `mem_cache`. Then draw.
   - Bound the `crossbeam_channel::select!` timeout by the remaining debounce, the remaining nav-settle window (`NAV_SETTLE - nav.last_input_at.elapsed()`), and the `TICK` (100 ms) so the flush and the deferred dispatch both fire close to schedule. The `select!` arms cover `event_rx` (single event; siblings caught by the next iteration's drain — events are still routed through `process_key` for nav coalescing), `done_rx`, the `cache_rx` channel that updates `hot_paths` from the worker's source-cache puts/evictions, and `default(timeout)`.
10. The worker drains the foreground channel via `try_recv` first; only when empty does it `select!` on both channels (foreground starvation of prefetch is intentional). For each job it computes `SourceKey { path, source_fp, target_bucket }` (target bucket is a log-1.25 quantization of `target.max_w`/`max_h`) and looks the source cache up. On hit it skips decode + resize entirely; on miss it calls `codec::decode(path)`, builds a `FileMeta` from it, then `pipeline::prepare_source(&loaded, target, cancel)` to produce a `PreparedSource` (post-decode, post-resize, in `Buffer<CameraLinear>` for both RAW and image-format paths), wraps it in `Arc`, and inserts it into the source cache (sending `CacheEvent::Cached` and any LRU `CacheEvent::Evicted` over `cache_tx`). It then calls `pipeline::apply_pipeline(&prepared, &params, cancel)` — which clones the cached `Buffer` once and runs the param-dependent pipeline tail — and ships the `Srgb8` + `FileMeta` + `JobKind` through `done_tx`. Foreground stale generations are dropped on receive (`latest_generation` mismatch); prefetch results just install into the in-memory tier when `Ok` and remove themselves from `prefetch_cancels`. `handle_job_done` installs the rendered preview into the in-memory tier first (so the screen refreshes immediately), then ships a `SaveMsg::CacheBlob` to the save worker for the on-disk write — `fs::sync_all` and `Cache::insert`'s SQLite touches happen off the UI thread.
11. Persist removed-flag changes immediately (Navigation focus only — `x` removes, `r` restores; both call `Db::set_removed` synchronously inside the key handler).
12. Open the filter popup when `f` is pressed (Navigation focus only); toggling formats rebuilds the visible list, preserving the current selection by canonical path when possible.
13. Shift focus to the Develop tab when `Enter` is pressed in Navigation focus; `j/k` then move between knobs, `h/l` adjust the focused knob by its step, `r` resets the focused knob to its default, `Esc` returns focus to Navigation. Knob edits accumulate in `app.develop_params` and are flushed via the tiered debounce (50 ms hot / 250 ms cold) — committed in-memory immediately and persisted asynchronously by the save worker. Image-navigation keys (`x`/`r`/`R`/`f`) are inert in Develop focus.
14. On `Quit`: drain the in-memory preview tier through `release_terminal_image` so any kitty graphics still resident on the terminal are explicitly freed; drop the develop, prefetch, and save senders, then `join` the save worker `JoinHandle` before `run` returns so any pending DB writes and disk-cache blobs land before the alternate-screen guard tears down.

For a single-file session, use the file's parent directory as the session root for filename ordering only — the DB is global.

## App State

The actual `App` struct (in `crates/terminalroom/src/app.rs`):

- `session_root: PathBuf` — directory used for filename ordering; the DB is global, not per-directory.
- `files: Vec<FileEntry>` — every discovered file (1:1 with the scan). Each `FileEntry` carries `id`, `DiscoveredFile`, `removed: bool`, persisted `develop_params: DevelopParams`, `develop_params_fp: u64`, and `source_fp: u64`.
- `visible: Vec<usize>` — indices into `files` after applying the current format filter and (when `show_removed` is false) hiding removed entries.
- `cursor: usize` — index into `visible`.
- `view: View` — `Main` or `Filter` (the filter modal). The Develop column lives inside `Main`.
- `focus: Focus` — `Navigation` (default) or `Develop`. Selects which key handler runs and which side tab gets the focus border.
- `show_removed: bool` — session-only toggle for the Navigation view; mutated by `R`. Persists across selections within a session but resets on app restart.
- `enabled_formats: BTreeSet<ImageKind>` — the live filter; mutated by `toggle_format`.
- `available_formats: Vec<(ImageKind, usize)>` — sorted format list with counts; computed once at init.
- `filter_cursor: usize` — popup-local row cursor.
- `status: Option<String>` — transient error/info line for the status bar.
- `db: Db` — owned SQLite handle for the UI thread. Used synchronously for `set_removed` (on `x`/`r`) and the `touch_access` write inside `Cache::get` on disk-cache hits. Hot-path writes (`update_params`, `Cache::insert`) are routed to a save worker that owns its own `Db` connection — see the TUI-layer state below.
- `develop_params: DevelopParams` — live editing buffer for the currently-selected file's knobs. On selection change `sync_develop_params_from_current` copies the new entry's persisted params here. After the tiered debounce expires (50 ms hot / 250 ms cold), `commit_develop_params(fp)` writes the new value back onto the entry in-memory and the matching `SaveMsg::Params` is queued for the save worker.
- `develop_cursor: usize` — index into `DEVELOP_KNOBS` (the static knob table) for the Develop tab's selection.
- `file_meta: HashMap<PathBuf, FileMeta>` — lazy cache of header metadata (shot info + dims + orientation + size + kind) for the Image Info tab. Populated by the preview worker as files are decoded.

The TUI layer (`tui::run`) owns the concurrency-and-rendering state outside `App`: the `Picker` and the `is_tmux: bool` flag (used to wrap the kitty delete escape with tmux passthrough where applicable); the on-disk `Cache` (also `Clone`d into the save worker); the in-memory `LruCache<PathBuf, PreviewEntry>` (cap 15); the develop, prefetch, event, done, source-cache-event, and save channels (a mix of `bounded(1)` / `bounded(2)` / unbounded); the `current_cancel: Option<Arc<AtomicBool>>` for the active foreground job; the `prefetch_cancels: HashMap<PathBuf, Arc<AtomicBool>>` for in-flight prefetches; the per-path `latest_generation: HashMap<PathBuf, u64>`; the `last_id: Option<i64>` / `last_target` / `last_rendered_fp` change-detection state; the `params_dirty_at: Option<Instant>` debounce timer; the `nav: NavCoalesce` held-key state (`last_input_at` + `last_key` + `burst_started_at` + `last_advance_at` — also serves as the nav-settle anchor); the `displayed_path: Option<PathBuf>` that pins the preview pane to the last settled file during a burst; the `hot_paths: HashSet<PathBuf>` tracking which paths the develop worker has prepared; the save worker `JoinHandle` (joined on shutdown); and the develop / event / save threads themselves. These are intentionally outside `App` so the headless logic stays free of ratatui and threading types.

Avoid introducing plugin systems, async runtimes, or generalized editing pipelines beyond the existing trait/Op surface.

## Preview Loading Strategy

Three cache tiers back the same render path:

- **Source tier** (worker-private): `LruCache<SourceKey, Arc<PreparedSource>>` (cap 3) inside the develop worker, keyed by `(path, source_fp, target_bucket)`. Each entry holds the post-decode + post-resize `Buffer<CameraLinear>` plus the `SensorInfo`/`ShotInfo` that `apply_pipeline` needs, and the `FileMeta` derived at decode time. `target_bucket` is a coarse log-1.25 quantization of the requested target so terminal-resize jitter doesn't churn the cache. A hit here means a knob tick reuses the prepared buffer and only runs the param-dependent pipeline tail.
- **Hot tier** (UI thread): `LruCache<PathBuf, PreviewEntry>` (cap 15) keyed by canonical path, holding the `StatefulProtocol` ratatui-image needs to re-paint without re-decoding, plus the `params_fingerprint` and rendered `TargetSize` for invalidation.
- **Warm tier** (persistent): on-disk `Cache` at `~/.terminalroom/cache/` keyed by BLAKE3-of-canonical-path, holding the `Srgb8` pixel buffer (header + raw RGB bytes). The `cache_key` reference and `last_access_unix_seconds` LRU clock live in the global DB.

On selection change or target change, the loop tries the hot tier first. Both the warm-tier install and any foreground dispatch are gated on `nav_settled = nav.last_input_at.elapsed() >= NAV_SETTLE (150 ms)` — i.e. on the user releasing the key — so a held burst doesn't keep mutating `mem_cache` (which would re-transmit kitty images and flicker the preview pane through every previously-cached file the cursor scrolls over). On hot-tier hit, the entry's `params_fingerprint` and rendered `TargetSize` validate it against the live target. On miss after settle, try the warm tier; on miss again dispatch a foreground job to the develop worker. The worker checks its source-tier cache before redecoding. On knob change the flow runs through tiered debounce — `DEBOUNCE_HOT = 50 ms` once the source is in the worker's cache, `DEBOUNCE_COLD = 250 ms` until then — so quickly held arrow keys settle into one re-render after release. Selecting a file whose persisted develop has been seen before pulls the developed `Srgb8` straight from disk and skips the worker entirely.

The preview pane reads from a separate `displayed_path: Option<PathBuf>` rather than `app.current()`. `displayed_path` only advances when both (a) `nav_settled` is true and (b) the cursor's path is already in the hot tier — otherwise it stays pinned to the last settled file. The result: during a held-key burst the pane is frozen on the previously-loaded image, and after release it stays on the previous image until the new file's preview lands in the hot tier (rather than dropping to "loading…" text).

Held-key navigation goes through a `NavCoalesce` rate-limiter at the input layer: the first event of a fresh burst (or new direction) advances the cursor immediately; continuation events within `NAV_BURST_GAP = 350 ms` are throttled to one advance per `NAV_SLOW_INTERVAL = 200 ms` for the first `NAV_RAMP_DURATION = 1 s` of the burst, then advance per-event for the remainder. The OS auto-repeat delay (~250 ms on macOS) is intentionally inside `NAV_BURST_GAP`, so the slow phase anchors at the keypress rather than the first auto-repeat.

After every foreground decision, `update_prefetch` queues develop jobs for `cursor-1` and `cursor+1` on a `bounded(2)` prefetch channel — also gated on `nav_settled`. Each prefetch carries that entry's *persisted* `develop_params` (not the active editing buffer). The worker only services prefetch when the foreground channel is empty; results land in the hot tier ahead of the user reaching them. Prefetches that fall outside the window are cancelled and removed from `prefetch_cancels`.

When a `PreviewEntry` is replaced or evicted from the hot tier (or the run loop tears down), `release_terminal_image` emits a kitty `_Ga=d,d=I,i={id};` delete escape so the terminal frees the image's storage. ratatui-image's kitty backend transmits image bytes once per `StatefulProtocol` lifetime under a random 32-bit id and references them via unicode placeholders thereafter; without explicit deletes, the terminal's graphics quota (kitty default ≈ 320 MB) fills after a few hundred previews and the terminal evicts images whose placeholders we still emit, surfacing as a blank preview pane. The helper is no-op for sixel/iterm2/halfblocks (those backends carry pixels inline per render).

Modal close (Filter or Looks) calls `terminal.clear()` on the modal→Main transition. Without the explicit clear, modal cells stay visually stuck on kitty/Ghostty: ratatui's buffer diff treats those cells as already-correct (its previous-buffer reflects what we wrote, not what the terminal actually rendered), so it never re-emits the underlying preview placeholders. `terminal.clear()` resets ratatui's previous-buffer and sends `\x1b[2J`, after which the next draw repaints every cell from scratch. The kitty image stays in terminal storage across the clear (the escape clears cell content only, not graphics), so `StatefulProtocol::render` re-emits placeholders that re-bind to the same image — no re-decode, no "loading…" flash, no `mem_cache` churn.

The worker pipeline:

- **Default render** (Navigation focus): `pipeline::prepare_source(loaded, target, cancel)` followed by `pipeline::apply_pipeline(prepared, &app.develop_params, cancel)` — libraw `half_size=true` (~4× faster demosaic); develop at TUI target resolution. New files default to identity `DevelopParams`; existing files use their persisted knobs.
- **Develop focus**: same call sites, but the user has been adjusting `app.develop_params`; after the active debounce expires the new params are committed in-memory + queued for the save worker, and the next iteration re-runs `apply_pipeline` against the cached `PreparedSource`.
- **Export** (post-MVP): `pipeline::develop_full(loaded, &params, target, cancel)` — libraw `half_size=false`, full or user-supplied target. Internally it also goes through `prepare_source_inner` + `apply_pipeline`, just at full quality.

For RAW: `codec::read_camera_linear` calls libraw with `output_color=Raw`, `use_camera_wb=false`, Raw-mode `no_auto_scale=true`, `output_bps=16`, `gamma=1.0`, `no_auto_bright=1`. The result is interleaved u16 from libraw, deinterleaved into a planar f32 `Vec<f32>` (length `3 * w * h`) at the codec boundary and normalized against sensor white after black subtraction. From there the develop pipeline owns the buffer.

For image-format files (JPEG/PNG/TIFF) the worker calls `read_image_pixels` on every (re-)render, scaled to the preview target: JPEG goes through `jpeg-decoder`'s `.scale()` IDCT factor (1/2/4/8); PNG/TIFF go through `image::ImageReader` at native resolution. EXIF orientation is applied at decode time via `kamadak-exif`. The decoded sRGB 8-bit buffer is then run through the full develop chain (linear sRGB → Temperature/Tint as channel gain → Rec.2020 → Exposure → Look → tone → OKLab/OKLCh → Warmth/Color/Clarity → linear sRGB → Grain → encode), so all knobs apply to image-format input.

For RAW orientation: `read_demosaiced` is auto-rotated by libraw itself (default `params.user_flip = -1`); we don't read EXIF separately.

Each LibRaw handler processes one file at a time. The current MVP runs one worker thread; multiple workers would need separate `libraw_data_t` handles per thread (libraw is not reentrant on a single handle).

## Color Pipeline

The two paths handle color very differently:

- **Image** (JPEG/PNG/TIFF input): the source is display-referred sRGB 8-bit. The pipeline decodes + resizes, runs `SrgbDecode` to linear sRGB, applies Temperature/Tint as per-channel gain (the buffer is re-tagged `CameraLinear` so the existing `Temperature`/`Tint` controls work without additional code), goes through `SrgbToRec2020`, and from there shares the RAW chain (Exposure → Look → tone → OKLab/OKLCh stages → Grain → encode). `CameraToWorking` is skipped (no sensor metadata). Soft Highlights tone, Shadows, Blacks, Warmth, Color, Clarity, and Grain all apply.
- **RAW** (`pipeline::develop_raw_full`): the source is camera-native linear (libraw `output_color=Raw`, no WB, no matrix). The pipeline:
  1. Wraps the planar f32 in `Buffer<CameraLinear>` and resizes to target.
  2. Applies Temperature/Tint as per-channel gain on camera-linear (before WB, so the user is adjusting illuminant rather than fighting it).
  3. `CameraToWorking` applies the WB multipliers (G-normalized) and a LibRaw-aligned camera→Rec.2020 matrix derived from `cam_xyz_coeff()` semantics (`cam_xyz` row normalization + pseudoinverse + sRGB→Rec.2020), producing `Buffer<LinearRec2020>`.
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
- If a cache file is corrupt (bad magic, wrong CRC, fingerprint mismatch), unlink it and clear the DB pointer; the next render repopulates.
- The save worker silently retains pending state on a `Db::update_params` or `Cache::insert` failure (errors are not surfaced to `app.status` in v1). WAL + `PRAGMA busy_timeout = 5000` plus a single owning thread per connection makes contention failures rare; the next successful write recovers the row. A future iteration can add a status callback channel if real users hit persistent errors.
