# Terminalroom Docs

Terminalroom is a terminal UI for culling and developing photographs. The first milestone:

- Run `terminalroom <path>`.
- Load image files (RAW + JPEG/PNG/TIFF) from the target file or directory.
- Show a single-screen layout: ASCII banner on top, preview on the left, three side tabs on the right (Develop / Image Info / Navigation), focus-aware status line at the bottom.
- Cull (`p`/`x`/`u`), navigate (`j`/`k`), and adjust 12 develop knobs from the same screen — `Enter` shifts focus to the Develop tab, `Esc` returns.
- Filter the visible files by format through a modal popup (key `f`).
- Persist culling decisions in `<path>/.terminalroom.db`.
- Develop knobs: Exposure, Temperature, Tint, Look Strength, Warmth, Color, Contrast, Soft Highlights, Shadows, Blacks, Clarity, Grain.

## Documents

- [Architecture](architecture.md): crate split, runtime flow, and ownership boundaries.
- [Dependencies](dependencies.md): practical notes for LibRaw, kamadak-exif, ratatui, ratatui-image, SQLite, and the SIMD stack (`wide`, `pulp`, `fast_image_resize`).
- [Storage](storage.md): SQLite database location and first schema.
- [MVP UX](mvp-ux.md): command flow, views, keybindings, and acceptance criteria.

## Current Decisions

- Use a Cargo workspace with four crates: `libraw-rs` (FFI), `codec` (file → memory struct with shot/sensor metadata), `darkroom` (develop pipeline), and `terminalroom` (library + binary). The library half of `terminalroom` holds headless modules (`session`, `db`, `app`) so they can be unit-tested without a TTY; `tui/` contains the ratatui rendering, split per panel (`banner`, `preview`, `develop`, `info`, `filmstrip`, `status`, `filter`), plus the worker thread and the event loop in `tui::mod`.
- Dependency chain: `libraw-rs` → `codec` → `darkroom` → `terminalroom`. Strict linear pipeline; each crate has a single responsibility.
- Keep all LibRaw FFI and unsafe code inside `libraw-rs`. The crate exposes capabilities (`read_header`, `read_demosaiced`) plus the metadata types (`ShotInfo`, `SensorInfo`, `OutputColorSpace`, `CfaPattern`, etc.) — orchestration policy lives in `codec`. `OutputColorSpace::Raw` (libraw code 0) is the develop-pipeline workhorse; `Rec2020` and others remain available for callers who want the matrix baked in.
- Two memory structs in `codec`, one per source kind:
  - `Image` for JPEG/PNG/TIFF — header-only; pixels lazy via `read_image_pixels` → `Srgb8Pixels`.
  - `Raw` for RAW files — header-only (shot-info + sensor-info); camera-linear pixels lazy via `read_camera_linear` → `CameraLinearPixels` (planar f32, no WB, no matrix).
  - `Loaded` is the dispatched union; `decode(path)` picks one based on extension.
- **The develop pipeline owns the camera→working transform end-to-end.** `read_camera_linear` calls libraw with `output_color=Raw`, `use_camera_wb=false`, and Raw-mode `no_auto_scale=true`, so the decoded buffer is black-subtracted, unbalanced camera RGB normalized against the sensor white level. Darkroom's `CameraToWorking` then applies WB multipliers and a LibRaw-aligned camera→Rec.2020 matrix derived from `cam_xyz_coeff()` semantics (`cam_xyz` row normalization + pseudoinverse + sRGB→Rec.2020). This lets Temperature/Tint operate on camera-native data before WB.
- Shot info (make/model/ISO/shutter/aperture/focal length) is parsed for both kinds. RAW reads it from libraw's `imgother`; image-format files read it from EXIF via `kamadak-exif`. Sensor info (black/white levels, CFA pattern, camera/daylight WB, color matrix, active area, crop area, orientation) is RAW-only.
- `darkroom` is structured around three traits — `Transform`/`InPlaceTransform` (stateless A→B), `Control` (knob in a fixed `ColorSpace`), `Blend` (two-buffer, used by Look Strength) — over a planar f32 `Buffer<S>` phantom-typed by its color space (`CameraLinear`, `LinearRec2020`, `LinearSrgb`, `Oklab`, `Oklch`). Output is `Srgb8`. The closed `Op` enum carries runtime ordering; the pipeline executor in `pipeline.rs` walks the chain and inserts transforms between knob spaces.
- Pipeline order (RAW): `read_camera_linear` → resize → Temperature/Tint (CameraLinear) → CameraToWorking → Exposure (LinearRec2020) → Look + LookStrength (in-linear blend) → tone batch in LinearRec2020 (extract Y, curve in log domain, scale RGB by Y'/Y to preserve hue) → Rec2020ToSrgb → LinearToOklab → Warmth (Oklab) → OklabToOklch → SoftHighlightsChroma + Color + Clarity (Oklch L) → OklchToOklab → OklabToLinear → Grain (LinearSrgb) → SrgbEncode. Two round-trips total: linear↔OKLab once for color, OKLab→linear for the final encode.
- TUI preview loading runs on a dedicated worker thread fed by a `crossbeam-channel`. Each job decodes the header and runs `pipeline::develop_preview` (libraw `half_size=true` for RAW) for the requested target size and current `DevelopParams`; the cache is `LruCache<PathBuf, PreviewEntry>` keyed by canonical path with a `params_fingerprint` per entry. Selection change, ≥ 25% target dimension change, or knob adjustment re-enqueues the job; stale generations are dropped on receive.
- SIMD strategy: `wide::f32x8` for the always-on stable baseline (NEON on aarch64, SSE2 baseline on x86_64) over the planar layout — gain ops, 3×3 matrix multiply, OKLab linear stages, luminance, sRGB quantize. `pulp` is in the dep graph for runtime AVX2 dispatch on heavier kernels (planned: cbrt, blur). Resize stays on `fast_image_resize` (`PixelType::F32` per plane).
- TUI preview rendering uses ratatui-image's `Resize::Scale` and a centered, aspect-fit sub-rect (computed against `picker.font_size()`) so landscape full-fills the width and portrait full-fills the height.
- Use SQLite through `rusqlite` (bundled feature) for `.terminalroom.db`.
- Scan only the direct children of a directory for the first MVP. Recursive scanning is deferred.
- Single-screen layout: top ASCII banner (`TERMINALROOM`, ANSI-Shadow), then a horizontal split with the preview on the left and three fixed-width (28-cell) side tabs on the right — `Develop` (12 knobs), `Image Info` (shoot info + file info), `Navigation` (filmstrip with state badges). A 1-row focus-aware status line is at the bottom.
- Two focus modes inside the main view: `Navigation` (default) handles image nav (`j`/`k`) and culling (`p`/`x`/`u`); `Develop` (entered via `Enter`, exited via `Esc`) handles knob nav (`j`/`k`) and adjustment (`h`/`l`, `r`). Image-nav keys are inert in Develop focus and vice versa — fully modal.
- The currently focused side tab is drawn with a thick yellow border; the other tabs use the default border. Image Info is read-only and never gets focus.
- Filter is a session-only modal popup. Toggling rebuilds the visible list without rescanning; selection survives toggles when the current file is still visible.
- Image Info metadata (shoot info + dimensions) is populated lazily by the preview worker — when a job decodes a file, it includes a `FileMeta` in the result that `tui::mod` writes into `App.file_meta`.

## Implementation Status

- [x] `libraw-rs` FFI surface: `read_header`, `read_demosaiced` plus `ShotInfo`/`SensorInfo`/`CfaPattern`/`BlackLevel`/`WhiteLevel`/`OutputColorSpace` (with `Raw` variant for code 0). C accessor wrappers in `wrapper.c` cover `imgdata.idata` / `sizes` / `color` / `other`.
- [x] `codec::format` — `ImageKind` taxonomy and extension-based `classify`.
- [x] `codec::decode_image` — `Image` header (shot-info from EXIF via `kamadak-exif`); `read_image_pixels` for lazy decode (JPEG via `jpeg-decoder` IDCT scale; PNG/TIFF via `image::ImageReader`).
- [x] `codec::decode_raw` — `Raw` header (shot-info + sensor-info from libraw); `read_camera_linear` returns planar f32 camera-linear (libraw `output_color=Raw`, `use_camera_wb=false`, Raw-mode `no_auto_scale=true`).
- [x] `darkroom::space` — `ColorSpace` marker, planar `Buffer<S>`, `Srgb8`.
- [x] `darkroom::transform` — `Transform`/`InPlaceTransform` traits + impls: `CameraToWorking`, `Rec2020ToSrgb`/`SrgbToRec2020`, `LinearToOklab`/`OklabToLinear`, `OklabToOklch`/`OklchToOklab`, `SrgbEncode`/`SrgbDecode`.
- [x] `darkroom::control` — `Control`/`Blend` traits + closed `Op` enum + 12 controls: Exposure, Temperature, Tint, LookStrength, Warmth, Color, Contrast, SoftHighlights{Tone,Chroma}, Shadows, Blacks, Clarity, Grain. `Look` trait with `Identity` and `WarmMutedSoft` presets.
- [x] `darkroom::primitive` — luminance + Y'/Y rescale, parametric `ToneCurve`, smoothstep masks, skin/specular guards, separable Gaussian blur, deterministic noise.
- [x] `darkroom::simd` — `wide::f32x8` helpers (`map_f32x8`, `map_pixel_f32x8`, `apply_3x3_planar`).
- [x] `darkroom::pipeline` — `DevelopParams`, `develop_preview` (half-size demosaic + full knob chain), `develop_full` (full resolution).
- [x] `darkroom::common` — `fit_within`, `resize_u8x3`, `resize_f32_planar` (per-plane via `fast_image_resize` `PixelType::F32`).
- [x] `session` — file scanning (RAW + JPEG/PNG/TIFF), sort, fingerprint.
- [x] `db` — SQLite v1 schema, migrations, `sync_files`, `set_state`.
- [x] Single-screen TUI (ratatui + ratatui-image, single worker + `Srgb8` cache with `params_fingerprint`, format filter popup, aspect-fit centered preview, ASCII banner with single-row fallback).
- [x] Develop tab + focus model (`Enter` enters, `Esc` exits; in-focus knob list driven by `j/k/h/l/r`; live re-render on knob change).
- [x] Image Info tab (shoot info from `ShotInfo` + file info; lazy populated by the worker via `FileMeta`).
- [ ] End-to-end smoke test against a real RAW fixture.
- [ ] Sidecar persistence for `DevelopParams` (post-MVP).
- [ ] Export action (post-MVP, uses `develop_full`).
