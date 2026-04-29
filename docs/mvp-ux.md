# MVP UX

## Launch

```sh
terminalroom <path>
```

`<path>` can be a single image file or a directory.

For a directory, scan only immediate children. Sort matches by filename using a stable, case-insensitive comparison. Recursive scanning is deferred.

Supported extensions (case-insensitive):

```text
RAW:  arw, cr2, cr3, dng, nef, nrw, raf, raw, rw2, orf, pef, srw
JPEG: jpg, jpeg
PNG:  png
TIFF: tif, tiff
```

A single-file input that does not match one of the above is rejected with a clear error.

## Empty and Error States

- If `<path>` does not exist, exit with a clear error.
- If `<path>` is not a file or directory, exit with a clear error.
- If no supported image files are found in a directory, exit with a clear message.
- If a single-file input has an unsupported extension, exit with a clear error.
- If a preview fails to load, keep the file selected and show the error in the status line; the image area shows a text placeholder.

## Culling View

The app opens in culling view.

Layout (Option B — vertical filmstrip on the right):

```text
+----------------------------------+----------------+
|                                  |  IMG_0421 ✓    |
|                                  |  IMG_0422 ✗    |
|         current preview          |▶ IMG_0423 ·    |
|                                  |  IMG_0424      |
|                                  |  IMG_0425      |
+----------------------------------+----------------+
| filename  index/total  STATE  shortcuts/status    |
+---------------------------------------------------+
```

The preview area uses the full remaining width and height to the left of the filmstrip and above the status line. The filmstrip is text-only: each row shows the file name plus a state badge (`✓` pick, `✗` reject, `·` unset). The selected row is prefixed `▶ ` and reverse-highlighted; long names are truncated with an ellipsis to fit the strip width. The filmstrip auto-scrolls to keep the selection visible.

The status line shows the current filename, `i/N` (where `N` is the visible count after the active filter), the current state label, an optional `filter: enabled/total` indicator when a filter is active, and a compact shortcut hint.

## Keybindings

Culling view:

```text
Right / j  next image
Left / k   previous image
p          mark pick
x          mark reject
u          unset culling decision
f          open format filter popup
d          open develop view placeholder
q          quit
```

Filter popup:

```text
Down / j   move cursor down
Up / k     move cursor up
Space      toggle the highlighted format
Enter      close popup (also: Esc, f)
q          quit
```

Develop view:

```text
c          return to culling view
q          quit
```

Navigation does not wrap around at the ends. Pressing next on the last image or previous on the first image keeps the current selection.

## Filter Popup

Press `f` from the culling view to open a centered modal popup with one row per format present in the current scan:

```text
+-- Filter formats ---------+
| [x] JPEG  (12)            |
| [ ] PNG   (3)             |
| [x] RAW   (8)             |
| [x] TIFF  (1)             |
|                           |
| space toggle · enter close|
+---------------------------+
```

- Counts are computed once at scan time and do not change while the popup is open.
- Toggling a format immediately rebuilds the visible file list. The current selection is preserved by canonical path when it is still visible; otherwise the cursor clamps to the last visible row.
- The filter is session-only — it is not persisted to `.terminalroom.db`. Restarting the app resets every format to enabled.
- All formats are enabled when the app starts.

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
2. Otherwise dispatch by `ImageKind`:
   - `Raw` → load an embedded thumbnail through `libraw-rs`; if extraction fails, fall back to the processed preview path.
   - `Jpeg`/`Png`/`Tiff` → load directly via `image::ImageReader::open` with format-guessing.
3. Insert the decoded image into the LRU preview cache (capacity 9, keyed by canonical path).
4. If loading fails, show a text placeholder in the image area and surface the error in the status line.

## Acceptance Criteria

- `terminalroom <directory>` opens the culling view for supported image files (RAW + JPEG/PNG/TIFF) in that directory.
- `.terminalroom.db` is created in the session root.
- `p`, `x`, and `u` persist state changes immediately.
- Restarting the app restores prior culling decisions.
- `f` opens the format filter popup; toggling a format hides matching files and updates the visible count in the status line.
- `d` opens the develop placeholder and `c` returns to culling.
- `q` exits and restores the terminal.

