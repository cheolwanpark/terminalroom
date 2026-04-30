# Terminalroom

Terminalroom is a terminal UI for culling and developing photographs. The MVP supports culling: navigating a directory of images, marking pick/reject/unset, and persisting decisions in a per-directory SQLite database. RAW (12 formats) plus JPEG, PNG, and TIFF are scanned; a modal `f`-keyed popup filters the visible list by format.

The Rust workspace:

- `crates/libraw-rs`: safe Rust boundary for LibRaw (RAW metadata + preview extraction).
- `crates/darkroom`: photo-processing layer (format taxonomy + preview decoding pipeline). Depends on `libraw-rs` and the `image` crate.
- `crates/terminalroom`: library + binary. Headless modules (`session`, `db`, `app`) are unit-tested without a TTY; `tui/` contains the ratatui rendering and event loop. Depends on `darkroom`.

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
