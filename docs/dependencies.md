# Dependency Notes

These notes document the intended first-use pattern for the dependencies chosen for the MVP.

## LibRaw

Use LibRaw through its C API from `libraw-rs`. The C API wraps the C++ API and keeps the Rust FFI boundary simpler.

Important calls in the realized MVP:

- `libraw_init` / `libraw_close`: create / free a handler.
- `libraw_open_file`: open the RAW file and read metadata.
- `libraw_set_demosaic`, `libraw_set_output_color`, `libraw_set_output_bps`, `libraw_set_gamma`, `libraw_set_no_auto_bright`: configure linear output (16 bpc, gamma 1.0, sRGB primaries, no auto-stretch).
- `tr_set_half_size`, `tr_set_use_camera_wb`: custom helpers from `wrapper.c` for the two `imgdata.params` fields libraw lacks public C setters for. Compiled into the crate via the `cc` build-dep.
- `libraw_unpack`, `libraw_dcraw_process`, `libraw_dcraw_make_mem_image`: linear demosaic path. `libraw_dcraw_clear_mem` frees the returned image buffer.
- `libraw_unpack_thumb`, `libraw_dcraw_make_mem_thumb`: embedded JPEG thumb extraction (the fast path).
- `libraw_strerror`: convert LibRaw error codes into messages.

Linking should prefer the thread-safe LibRaw variant where the platform exposes it. On Unix-like systems this is commonly `-lraw_r`; `-lraw` is the ordinary variant. `build.rs` uses `pkg-config` or `LIBRAW_LIB_DIR` / `LIBRAW_LIB_NAME` / `LIBRAW_INCLUDE_DIR` environment overrides; on macOS it auto-detects the Homebrew prefix.

`libraw-rs` exposes owned Rust buffers only. It must not expose LibRaw pointers, LibRaw-owned memory, or raw C structs across the crate boundary.

## jpeg-decoder

`jpeg-decoder = "0.3"` decodes JPEG bytes for both standalone `.jpg` files and the camera-embedded thumbs returned by `libraw_dcraw_make_mem_thumb`. It is preferred over the JPEG path inside the `image` crate (which uses `zune-jpeg`) because it exposes `Decoder::scale(target_w, target_h)` — IDCT-based sub-resolution decode at factor 1/2/4/8. For terminal-sized previews this is dramatically cheaper than decoding at native resolution and downscaling.

EXIF orientation is read from `Decoder::exif_data()` (returns the TIFF chunk after the `Exif\0\0` prefix) and parsed by `image::metadata::Orientation::from_exif_chunk`.

## fast_image_resize

`fast_image_resize = "6"` is the SIMD resampler used by `darkroom::develop`. It supports SSE4.1 / AVX2 / NEON for U8x3 (sRGB) and U16x3 (linear) pixel types. The default `Resizer::new()` picks the best available CPU extension and uses Lanczos3 by default. It is the only resize step that does meaningful per-pixel work in the pipeline; `codec` decodes at the coarsest size that meets the target so that step is small.

## image

`image = { version = "0.25", default-features = false, features = ["jpeg", "png", "tiff"] }` plays a smaller role than before:

- `codec::decode_via_image` uses `ImageReader::open(...).with_guessed_format()?.into_decoder()` for PNG and TIFF. JPEG goes through `jpeg-decoder` instead (for `.scale()`).
- `image::metadata::Orientation::from_exif_chunk` parses orientation from the JPEG path's EXIF chunk. `ImageDecoder::orientation()` handles it for the TIFF path. Both apply via `DynamicImage::apply_orientation`.
- `terminalroom`'s TUI uses `DynamicImage::ImageRgb8` to wrap a developed `RgbImage` for `picker.new_resize_protocol`.

`darkroom` itself does not depend on `image`; it operates on `Vec<u8>` / `Vec<u16>` and `fast_image_resize`'s buffer types.

## ratatui

Use ratatui as the immediate-mode terminal renderer. Every draw pass renders the full UI from current app state.

Realized MVP structure (in `tui/`):

- Terminal init/teardown via explicit crossterm calls (`enable_raw_mode`, `EnterAlternateScreen`) wrapped in a Drop guard so the terminal is restored even on panic.
- One worker thread runs `darkroom::develop_to_rgb` jobs from a `crossbeam_channel`. One event-reader thread feeds crossterm `Event`s into another channel.
- Main loop runs `crossbeam_channel::select!` over events, completed jobs, and a 100 ms tick (used for periodic redraw and to recompute the preview target on terminal resize).
- Three view renderers: `culling`, `develop`, `filter`. The filter view is rendered as a `Clear`-backed modal overlay on top of the culling view.
- Rendering functions are pure: they read from `App` and write to the `Frame`. DB writes happen in the event/loop step (`app.set_state`); preview decoding happens off-thread on the worker and lands in the cache via `JobDone` messages.

Ratatui does not own input handling. Crossterm keyboard events are dispatched per-view (`KeyCode::Char('p')`, etc.).

## crossbeam-channel

`crossbeam-channel = "0.5"` provides the unbounded `Sender`/`Receiver` pairs and the `select!` macro the TUI uses to multiplex over events, completed jobs, and timeouts. Picked over `std::sync::mpsc` for the macro and the better receive-with-timeout ergonomics.

## ratatui-image

Use ratatui-image to render preview images in the terminal. It supports multiple terminal graphics protocols and falls back to text-based rendering.

Realized MVP behavior:

- `Picker::from_query_stdio()` is called once at TUI startup; if it fails (terminal does not respond to the query), the TUI falls back to `Picker::from_fontsize((1, 2))` and surfaces the warning in the status line. ratatui-image then renders with halfblocks.
- Cache is `LruCache<PathBuf, PreviewSlot>` (capacity 9) where `PreviewSlot` holds an optional `PreviewEntry { proto: StatefulProtocol, src_w, src_h }` for each tier (fast / full). The cache, channels, and worker thread are owned by `tui::run`, never by `App`.
- The main image area uses `StatefulImage::default().resize(Resize::Scale(None))` — `Scale` (not `Fit`) because the fast tier's source is much smaller than the preview area; `Fit` would refuse to upscale, so the fast-tier preview would render at its native (small) size. With `Scale`, both tiers fill the same display rect; only sharpness differs.
- A centered, aspect-preserving sub-rect (`aspect_fit_rect` in `tui::culling`) is computed from `(src_w, src_h)` and `picker.font_size()`. Landscape images use full preview width and center vertically; portrait images use full preview height and center horizontally. The sub-rect — not the full preview area — is what gets passed to `StatefulImage`.
- On selection change the worker is fed two jobs sharing one `Arc<AtomicBool>`: a fast tier at `target/4` (1/16 px) and a full tier at `target`. Selection change bumps a `current_generation` and flips the prior cancel flag. Stale results are dropped on receive. Render preference is `slot.full → slot.fast → text placeholder`.

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

