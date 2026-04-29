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

Use ratatui as the immediate-mode terminal renderer. Every draw pass should render the full UI from current app state.

Recommended MVP structure:

- Initialize terminal with ratatui's crossterm-backed helpers or explicit crossterm setup.
- Keep a single main event loop that alternates input handling and drawing.
- Split rendering by view: culling view and develop placeholder view.
- Keep side effects out of rendering functions. DB writes and decode requests should happen in event handling or app update code.

Ratatui does not own input handling. Use crossterm keyboard events for shortcuts.

## image

Use the `image` crate as the application crate's pixel-data layer. The MVP only needs JPEG decoding (for LibRaw embedded thumbnails) and the `DynamicImage` type that ratatui-image consumes, so build with default features off:

- `image = { version = "0.25", default-features = false, features = ["jpeg"] }`

`preview.rs` performs the conversion from `libraw_rs::PreviewImage`:

- `PreviewFormat::Jpeg` → `image::load_from_memory_with_format(bytes, ImageFormat::Jpeg)`.
- `PreviewFormat::Rgb8 { colors: 3, bits_per_channel: 8 }` → `ImageBuffer::<Rgb<u8>, _>::from_raw(...)` wrapped in `DynamicImage::ImageRgb8`.
- Other RGB layouts (4 channels, 16 bits per channel) currently return an `UnsupportedRgb` error; revisit when a real RAW exposes one.

Keeping JPEG decoding in the application crate preserves the `libraw-rs` boundary: the FFI crate stays free of image-processing dependencies.

## ratatui-image

Use ratatui-image to render preview images in the terminal. It supports multiple terminal graphics protocols and falls back to text-based rendering.

MVP guidance:

- Use `Picker::from_query_stdio()` when possible to detect terminal protocol and font size.
- Keep `StatefulProtocol` or equivalent ratatui-image render state in app state.
- Use `StatefulImage` for the main image area because it can adapt to the current render area.
- Avoid resizing and encoding large images inside every frame. Decode or resize only when the selected image changes or the terminal size changes.
- Handle encoding errors and show a text placeholder instead of crashing the TUI.

The bottom strip can start as text labels or simple thumbnails. If thumbnails are rendered there, each visible item needs its own image state or a deliberate simplified renderer.

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

