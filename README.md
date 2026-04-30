# Terminalroom

Terminalroom is a terminal UI for culling and developing photographs. The MVP supports culling: navigating a directory of images, marking pick/reject/unset, and persisting decisions in a per-directory SQLite database. RAW (12 formats) plus JPEG, PNG, and TIFF are scanned; a modal `f`-keyed popup filters the visible list by format.

The Rust workspace (strict linear pipeline `libraw-rs → codec → darkroom → terminalroom`):

- `crates/libraw-rs`: safe Rust boundary for LibRaw. RAW only. Surface: `read_metadata`, `read_linear` (16-bit linear RGB at full or half_size, with cancellation), `read_embedded_jpeg` (camera-stored thumb when present). Custom `wrapper.c` (compiled via `cc`) exposes the `half_size` / `use_camera_wb` params libraw lacks public setters for.
- `crates/codec`: file → `DecodedImage::{Linear | Srgb8}` at the coarsest size that meets the requested target. Hosts the `ImageKind` taxonomy. RAW dispatches to libraw-rs (embedded JPEG first, demosaic fallback). JPEG goes through `jpeg-decoder` with IDCT `.scale()` for fast sub-resolution decode. PNG/TIFF go through `image::ImageReader`. Applies EXIF orientation for both standalone JPEGs and embedded RAW thumbs.
- `crates/darkroom`: develop pipeline. Takes a `DecodedImage` and a target size, runs SIMD resize (`fast_image_resize`, U16x3 / U8x3) to the exact target, applies the sRGB transfer via a precomputed LUT for the linear branch, returns 8-bit sRGB `RgbImage`. Re-exports `codec::{ImageKind, classify, TargetSize, DecodedImage}` so the TUI only depends on darkroom.
- `crates/terminalroom`: library + binary. Headless modules (`session`, `db`, `app`) are unit-tested without a TTY. `tui/` contains the ratatui rendering plus a worker thread that runs `darkroom::develop_to_rgb`, fed two-tier jobs (fast at 1/16 px, full at preview-area px) per selection with `Arc<AtomicBool>` cancellation. Cache is `LruCache<PathBuf, PreviewSlot { fast, full }>`.

See [`docs/`](docs/README.md) for architecture, dependencies, storage, and UX details.

## Prerequisites

LibRaw must be installed and linkable. The build script first tries `pkg-config`
for `libraw_r` or `libraw`, then checks explicit environment overrides.

```sh
LIBRAW_LIB_DIR=/path/to/lib LIBRAW_LIB_NAME=raw_r cargo check --workspace
```

On macOS, Homebrew's `libraw` package should work once installed.

## Run

```sh
cargo run -- <path>
```

`<path>` is either a single image file or a directory. Best results come from terminals with a graphics protocol (Kitty, iTerm2, WezTerm); other terminals fall back to halfblock rendering.
