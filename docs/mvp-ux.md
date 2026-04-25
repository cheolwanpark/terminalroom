# MVP UX

## Launch

```sh
terminalroom <path>
```

`<path>` can be a single RAW file or a directory.

For a directory, scan only immediate children. Sort matches by filename using a stable, case-insensitive comparison. Recursive scanning is deferred.

Initial RAW extension filter:

```text
arw, cr2, cr3, dng, nef, nrw, raf, raw, rw2, orf, pef, srw
```

Extension matching should be case-insensitive.

## Empty and Error States

- If `<path>` does not exist, exit with a clear error.
- If `<path>` is not a file or directory, exit with a clear error.
- If no RAW files are found, exit with a clear message.
- If a preview fails to load, keep the file selected and show an error placeholder in the image area.

## Culling View

The app opens in culling view.

Layout:

```text
+--------------------------------------------------+
| current RAW preview                              |
|                                                  |
|                                                  |
+--------------------------------------------------+
| prev thumbnails | current | next thumbnails      |
+--------------------------------------------------+
| filename | index | state | shortcuts/status      |
+--------------------------------------------------+
```

The main image area should use the full available width and remaining height above the bottom strip. The bottom strip shows nearby items centered around the selected file. It can start with filenames or simple thumbnail placeholders if rendering multiple thumbnails is not ready.

## Keybindings

```text
Right / j  next image
Left / k   previous image
p          mark pick
x          mark reject
u          unset culling decision
d          open develop view placeholder
q          quit
?          toggle shortcut help, optional for MVP
```

Navigation should not wrap around at the ends for the first MVP. Pressing next on the last image or previous on the first image should keep the current selection.

## Culling State Display

Show the current culling state in the status line:

- `UNSET`
- `PICK`
- `REJECT`

When the user changes state, persist it immediately to SQLite and update the status line.

## Develop View Placeholder

The develop view has no editing functionality in the first MVP. It should render:

```text
Develop view

Editing controls are not implemented yet.

Press c to return to culling.
Press q to quit.
```

Keybindings:

```text
c          return to culling view
q          quit
```

## Preview Behavior

When the selected file changes:

1. Show cached preview immediately if available.
2. Otherwise load an embedded thumbnail through `libraw-rs`.
3. If no embedded thumbnail is available, load a processed preview.
4. If loading fails, show an error placeholder.

Keep a small in-memory cache for the selected file and nearby files. A fixed capacity of 9 previews is enough for the first implementation.

## Acceptance Criteria

- `terminalroom <directory>` opens the culling view for supported RAW files in that directory.
- `.terminalroom.db` is created in the session root.
- `p`, `x`, and `u` persist state changes.
- Restarting the app restores prior culling decisions.
- `d` opens the develop placeholder and `c` returns to culling.
- `q` exits and restores the terminal.

