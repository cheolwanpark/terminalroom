# Terminalroom Docs

Terminalroom is planned as a terminal UI for culling and developing RAW images.
The first milestone is intentionally narrow:

- Run `terminalroom <path>`.
- Load RAW files from the target file or directory.
- Show a culling view with a main image and nearby thumbnails.
- Persist culling decisions in `<path>/.terminalroom.db`.
- Provide a placeholder develop view.

## Documents

- [Architecture](architecture.md): crate split, runtime flow, and ownership boundaries.
- [Dependencies](dependencies.md): practical notes for LibRaw, ratatui, ratatui-image, and SQLite.
- [Storage](storage.md): SQLite database location and first schema.
- [MVP UX](mvp-ux.md): command flow, views, keybindings, and acceptance criteria.

## Current Decisions

- Use a Cargo workspace with a `libraw-rs` library crate and a `terminalroom` library + binary crate. The library half exists so headless modules (`session`, `db`, `preview`) can be unit-tested without a TTY.
- Keep all LibRaw FFI and unsafe code inside `libraw-rs`.
- Use SQLite through `rusqlite` (bundled feature) for `.terminalroom.db`.
- Use the `image` crate (jpeg-only feature) in the application crate to decode embedded JPEG previews into `DynamicImage`. ratatui-image will consume the same type later.
- Prefer LibRaw embedded thumbnails for culling previews, with processed RGB as fallback.
- Scan only the direct children of a directory for the first MVP. Recursive scanning is deferred.

## Implementation Status

- [x] `libraw-rs` FFI surface and safe wrappers (`read_metadata`, `read_preview`).
- [x] `session` — file scanning, sort, fingerprint.
- [x] `db` — SQLite v1 schema, migrations, `sync_files`, `set_state`.
- [x] `preview` — `PreviewImage` → `DynamicImage` adapter.
- [ ] Culling TUI (ratatui + ratatui-image, event loop, app state, preview cache).
- [ ] Develop view placeholder.
- [ ] End-to-end smoke test against a real RAW fixture.

