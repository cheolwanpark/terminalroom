# Terminalroom

Terminalroom is a terminal UI for culling and developing RAW images.

The Rust workspace currently contains the initial scaffold described in `docs/`:

- `crates/libraw-rs`: safe Rust boundary for LibRaw.
- `crates/terminalroom`: application binary.

## Prerequisites

LibRaw must be installed and linkable. The build script first tries `pkg-config`
for `libraw_r` or `libraw`, then checks explicit environment overrides.

```sh
LIBRAW_LIB_DIR=/path/to/lib LIBRAW_LIB_NAME=raw_r cargo check --workspace
```

On macOS, Homebrew's `libraw` package should work once installed.
