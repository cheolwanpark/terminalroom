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

- Use a Cargo workspace with a `libraw-rs` library crate and a `terminalroom` binary crate.
- Keep all LibRaw FFI and unsafe code inside `libraw-rs`.
- Use SQLite through `rusqlite` for `.terminalroom.db`.
- Prefer LibRaw embedded thumbnails for culling previews, with processed RGB as fallback.
- Scan only the direct children of a directory for the first MVP. Recursive scanning is deferred.

