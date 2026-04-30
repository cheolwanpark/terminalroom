# Terminalroom

Terminalroom is a terminal UI for culling and developing photographs. The MVP shows a single-screen layout — ASCII banner on top, a live preview on the left, three side tabs on the right (Develop / Image Info / Navigation), and a focus-aware status line at the bottom. Mark pick/reject/unset, browse the filmstrip, adjust 12 develop knobs (`Enter` to focus the Develop tab, `Esc` to return), and filter by format from a modal popup (`f`). Decisions persist in a per-directory SQLite database. RAW (12 formats) plus JPEG, PNG, and TIFF are scanned.

The Rust workspace (strict linear pipeline `libraw-rs → codec → darkroom → terminalroom`):

- `crates/libraw-rs`: safe Rust boundary for LibRaw. RAW only. Surface: `read_header` (shot-info + sensor metadata), `read_demosaiced` (16-bit linear RGB in any `OutputColorSpace` including the camera-native `Raw`, with `half_size` and `use_camera_wb` flags + cancellation). Custom `wrapper.c` (compiled via `cc`) exposes per-field accessors over `imgdata.{idata, sizes, color, other}` plus the two `imgdata.params` setters libraw lacks.
- `crates/codec`: file → `Image` (sRGB header) or `Raw` (camera-linear header). `Loaded` is the dispatched union; `decode(path)` picks one based on extension. Pixels load lazily: `read_image_pixels` (sRGB 8-bit interleaved) for JPEG/PNG/TIFF, `read_camera_linear` (planar f32, libraw `output_color=Raw` + `use_camera_wb=false`) for RAW. JPEG goes through `jpeg-decoder` with IDCT `.scale()` for fast sub-resolution decode. PNG/TIFF go through `image::ImageReader`. EXIF orientation applies at decode time for image-format files; libraw's own auto-rotate handles RAW.
- `crates/darkroom`: develop pipeline. Three traits — `Transform`/`InPlaceTransform` (stateless A→B), `Control` (knob in a fixed `ColorSpace`), `Blend` (two-buffer) — over a planar f32 `Buffer<S>` phantom-typed by its color space. 12 user-facing knobs (Exposure, Temperature, Tint, Look Strength, Warmth, Color, Contrast, Soft Highlights, Shadows, Blacks, Clarity, Grain) + a `Look` system (`Identity`, `WarmMutedSoft`). SIMD via `wide::f32x8` over the planar layout for gain, matrix multiplies, OKLab math, and sRGB quantization; `pulp` reserved for runtime AVX2 dispatch on heavier kernels. Resize stays on `fast_image_resize` (`PixelType::F32` per plane). Public API: `pipeline::develop_preview` (libraw `half_size=true` for fast culling) and `develop_full` (full resolution).
- `crates/terminalroom`: library + binary. Headless modules (`session`, `db`, `app`) are unit-tested without a TTY. `app` holds `view: View` (`Main`/`Filter`), `focus: Focus` (`Navigation`/`Develop`), `DevelopParams`, `develop_cursor`, and a `file_meta: HashMap<PathBuf, FileMeta>` cache for the Image Info tab. `tui/` is split per panel (`banner`, `preview`, `develop`, `info`, `filmstrip`, `status`, `filter`) plus the worker/event-loop in `mod.rs`. The worker runs `pipeline::develop_preview` on each `Job` and returns the rendered `Srgb8` together with a `FileMeta` parsed from the same `Loaded` — so opening a file populates both the preview cache and the info tab in one round-trip. Cache is `LruCache<PathBuf, PreviewEntry>` keyed by path with `params_fingerprint` per entry — adjusting a knob invalidates the current entry without disturbing the rest.

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
