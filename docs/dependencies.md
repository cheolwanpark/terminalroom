# Dependency Notes

These notes document the intended first-use pattern for the dependencies chosen for the MVP.

## LibRaw

Use LibRaw through its C API from `libraw-rs`. The C API wraps the C++ API and keeps the Rust FFI boundary simpler. LibRaw 0.21+ is required (for `raw_inset_crops` and `output_color=8` Rec.2020); the project is developed against 0.22.

Important calls in the realized MVP:

- `libraw_init` / `libraw_close`: create / free a handler.
- `libraw_open_file`: open the RAW file and read metadata into `libraw_data_t`.
- `libraw_set_output_color` / `set_output_bps` / `set_gamma` / `set_no_auto_bright` / `set_demosaic`: configure the linear-output pipeline. `output_color` is now driven by `OutputColorSpace` (default `Rec2020` = libraw code 8).
- `tr_set_half_size`, `tr_set_use_camera_wb`: custom helpers from `wrapper.c` for the two `imgdata.params` fields libraw lacks public C setters for.
- `tr_get_*` accessors in `wrapper.c`: per-field readers over `imgdata.{idata, sizes, color, other}` covering the metadata `read_header` exposes (make/model, ISO/shutter/aperture/focal-length, raw and active dims, top/left margins, flip, `raw_inset_crops`, filters/cdesc/X-Trans pattern, black/white levels, `cam_mul`/`pre_mul`, `cam_xyz`).
- `libraw_unpack`, `libraw_dcraw_process`, `libraw_dcraw_make_mem_image`: linear demosaic path (`read_demosaiced`). `libraw_dcraw_clear_mem` frees the returned image buffer.
- `libraw_unpack_thumb`, `libraw_dcraw_make_mem_thumb`: embedded JPEG thumb extraction (`read_thumbnail`). Bitmap thumbs are filtered out — callers get back JPEG bytes only.
- `libraw_strerror`: convert LibRaw error codes into messages.

Linking should prefer the thread-safe LibRaw variant where the platform exposes it. On Unix-like systems this is commonly `-lraw_r`; `-lraw` is the ordinary variant. `build.rs` uses `pkg-config` or `LIBRAW_LIB_DIR` / `LIBRAW_LIB_NAME` / `LIBRAW_INCLUDE_DIR` environment overrides; on macOS it auto-detects the Homebrew prefix.

`libraw-rs` exposes owned Rust buffers and metadata structs only. It must not expose LibRaw pointers, LibRaw-owned memory, or raw C structs across the crate boundary.

## kamadak-exif

`kamadak-exif = "0.6"` parses EXIF from JPEG/PNG/TIFF containers in `codec::metadata`. Used to populate `ShotInfo` (make/model/ISO/shutter/aperture/focal-length) and the EXIF Orientation tag for image-format files. It is also used by `codec::jpeg::decode_jpeg_bytes_to_thumbnail` (via `parse_orientation_from_tiff_chunk`) to read the orientation tag from a camera-embedded JPEG when the parent RAW file's libraw `flip` code is not the right answer.

`Reader::read_from_container` handles all three formats (JPEG APP1, PNG `eXIf`, TIFF baseline IFD); `Reader::read_raw` parses a bare TIFF chunk (the bytes `jpeg_decoder::Decoder::exif_data()` returns).

## jpeg-decoder

`jpeg-decoder = "0.3"` decodes JPEG bytes for both standalone `.jpg` files and the camera-embedded thumbs returned by `libraw_rs::read_thumbnail`. It is preferred over the JPEG path inside the `image` crate (which uses `zune-jpeg`) because it exposes `Decoder::scale(target_w, target_h)` — IDCT-based sub-resolution decode at factor 1/2/4/8. For terminal-sized previews this is dramatically cheaper than decoding at native resolution and downscaling.

`Decoder::exif_data()` returns the TIFF chunk after the `Exif\0\0` prefix. We feed it into kamadak-exif's `read_raw` to pull orientation for embedded JPEG thumbnails.

## fast_image_resize

`fast_image_resize = "6"` is the SIMD resampler used by `darkroom::common`. It supports SSE4.1 / AVX2 / NEON for U8x3 (sRGB) and U16x3 (linear) pixel types. The default `Resizer::new()` picks the best available CPU extension and uses Lanczos3 by default. For RAW develop the resize runs on 16-bit linear data **before** the sRGB transfer; for image develop and thumbnail re-render it runs on 8-bit sRGB.

## image

`image = { version = "0.25", default-features = false, features = ["jpeg", "png", "tiff"] }` is used in two places:

- `codec::decode_image::decode_via_image_crate` uses `ImageReader::open(...).with_guessed_format()?.into_decoder()` for PNG and TIFF, then converts to `Rgb8`. JPEG goes through `jpeg-decoder` instead (for `.scale()`).
- `codec::jpeg::apply_orientation_to_rgb8` wraps the decoded RGB buffer in a `DynamicImage::ImageRgb8` and calls `apply_orientation` to apply EXIF transforms.

`darkroom` itself does not depend on `image`; it operates on `Vec<u8>` / `Vec<u16>` and `fast_image_resize`'s buffer types.

## ratatui

Use ratatui as the immediate-mode terminal renderer. Every draw pass renders the full UI from current app state.

Realized MVP structure (in `tui/`):

- Terminal init/teardown via explicit crossterm calls (`enable_raw_mode`, `EnterAlternateScreen`) wrapped in a Drop guard so the terminal is restored even on panic.
- One worker thread runs `darkroom::decode` + `darkroom::develop_culling` jobs from a `crossbeam_channel`. One event-reader thread feeds crossterm `Event`s into another channel.
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
- Cache is `LruCache<PathBuf, PreviewEntry>` (capacity 9). Each entry holds a `StatefulProtocol`, the source dims, and the `TargetSize` it was rendered for. On selection change or significant target change, the worker re-runs `decode` + `develop_culling` and the result replaces the prior entry.
- The main image area uses `StatefulImage::default().resize(Resize::Scale(None))` — `Scale` (not `Fit`) so the same display rect is filled regardless of source resolution; the terminal-cell size of the preview is computed by `aspect_fit_rect`.
- A centered, aspect-preserving sub-rect (`aspect_fit_rect` in `tui::culling`) is computed from `(src_w, src_h)` and `picker.font_size()`. Landscape images use full preview width and center vertically; portrait images use full preview height and center horizontally. The sub-rect — not the full preview area — is what gets passed to `StatefulImage`.

The right-side filmstrip is text-only with state badges (`✓`/`✗`/`·`) — no per-row image rendering. This keeps the cache small and avoids per-frame encoding for thumbnails.

## SQLite and rusqlite

Use SQLite through `rusqlite` for `.terminalroom.db`.

SQLite is the default choice because:

- The DB file is easy to inspect with external tools.
- Schema migrations are straightforward through `PRAGMA user_version`.
- Future develop settings will likely need structured queries.
- The culling state is small, relational, and path-oriented.

Use one connection on the UI thread for the first MVP. If preview decoding moves to multiple worker threads, keep DB access on the app thread unless there is a concrete performance issue.

## sled Comparison

sled is a viable embedded key-value store, but it is not the MVP default. It is less convenient for ad hoc inspection, migrations, and future queries like "show all picked files from this session".

Revisit sled only if SQLite becomes a proven bottleneck or if the app later needs a key-value event log.
