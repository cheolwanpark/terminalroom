# Terminalroom Docs

Terminalroom is a terminal UI for culling and developing photographs. The first milestone:

- Run `terminalroom <path>`.
- Load image files (RAW + JPEG/PNG/TIFF) from the target file or directory.
- Show a culling view with a main preview and a vertical filmstrip on the right.
- Filter the visible files by format through a modal popup (key `f`).
- Persist culling decisions in `<path>/.terminalroom.db`.
- Provide a placeholder develop view.

## Documents

- [Architecture](architecture.md): crate split, runtime flow, and ownership boundaries.
- [Dependencies](dependencies.md): practical notes for LibRaw, ratatui, ratatui-image, and SQLite.
- [Storage](storage.md): SQLite database location and first schema.
- [MVP UX](mvp-ux.md): command flow, views, keybindings, and acceptance criteria.

## Current Decisions

- Use a Cargo workspace with a `libraw-rs` library crate and a `terminalroom` library + binary crate. The library half holds headless modules (`session`, `db`, `preview`, `app`) so they can be unit-tested without a TTY; `tui/` contains the ratatui rendering and event loop.
- Keep all LibRaw FFI and unsafe code inside `libraw-rs`. Non-RAW images are loaded directly via the `image` crate, so the FFI boundary stays clean.
- Use SQLite through `rusqlite` (bundled feature) for `.terminalroom.db`.
- Use the `image` crate with `jpeg`, `png`, and `tiff` features (no defaults). RAW previews come from LibRaw and are converted into `DynamicImage` via `preview::decode_preview`; non-RAW files go through `image::ImageReader::open`.
- Prefer LibRaw embedded thumbnails for RAW culling previews, with processed RGB as fallback.
- Scan only the direct children of a directory for the first MVP. Recursive scanning is deferred.
- Culling layout: Option B — vertical filmstrip on the right of the main preview (text labels with state badges, no per-row image rendering).
- Filter is a session-only modal popup. Toggling rebuilds the visible list without rescanning; selection survives toggles when the current file is still visible.

## Implementation Status

- [x] `libraw-rs` FFI surface and safe wrappers (`read_metadata`, `read_preview`).
- [x] `session` — file scanning (RAW + JPEG/PNG/TIFF), sort, fingerprint, `ImageKind`.
- [x] `db` — SQLite v1 schema, migrations, `sync_files`, `set_state`.
- [x] `preview` — `PreviewImage` → `DynamicImage` adapter; on-disk loader for non-RAW formats.
- [x] Culling TUI (ratatui + ratatui-image, event loop, app state, preview cache, format filter popup).
- [x] Develop view placeholder.
- [ ] End-to-end smoke test against a real RAW fixture.

