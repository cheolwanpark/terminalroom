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
j / Down   focus next knob
k / Up     focus previous knob
h / Left   decrement focused knob by one step
l / Right  increment focused knob by one step
r          reset focused knob to default
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

## Develop View

The develop view shows the current image with the develop pipeline applied at preview resolution, alongside a list of 12 user-facing knobs grouped by function (Input → Look → Tone → Detail per the design doc). The cursor highlights one knob at a time; `h/l` adjusts that knob by its step, `r` resets it to default, `j/k` move between knobs. The preview re-renders whenever a knob changes (debounced through the same worker thread that handles culling).

Layout:

```text
+----------------------------------+----------------+
|                                  | Exposure       |
|                                  |▶ Temperature   |
|        develop preview           |  Tint          |
|        (with knobs applied)      |  Look Strength |
|                                  |  Warmth        |
|                                  |  …             |
+----------------------------------+----------------+
| j/k navigate · h/l adjust · r reset · c back · q  |
+---------------------------------------------------+
```

Knob set (display order; per-knob ranges live in `app::DevelopKnob`):

```text
Input          Exposure       (-3..+3 EV, step 0.05)
               Temperature    (2000..12000 K, step 100)
               Tint           (-1..+1, step 0.05)
Look           Look Strength  (0..1, step 0.05)
Color          Warmth         (-1..+1)
               Color          (-1..+1)
Tone           Contrast       (-1..+1)
               Soft Highlights (0..1)
               Shadows        (-1..+1)
               Blacks         (-1..+1)
Detail         Clarity        (-1..+1)
               Grain          (0..1)
```

Defaults are identity (every knob at zero / 5500 K / Look Strength 1.0 with `Identity` look) — pressing `d` from culling shows the same preview, just with the knob list overlaid.

Knob values are session-only in the MVP. Sidecar persistence is post-MVP.

## Preview Behavior

When the selected file changes, the terminal resize moves the preview target ≥ 25% in either dim, or a develop knob is adjusted (the `DevelopParams` fingerprint changes):

1. Show whatever the cache already holds for the new file *if* its `params_fingerprint` matches the current one.
2. Bump a per-selection generation and flip the prior selection's cancel flag, then enqueue one job for the new selection at the new target size and current params. Each job carries an `Arc<AtomicBool>` cancel flag and a clone of the `DevelopParams`.
3. The worker calls `darkroom::decode` (cheap header read) followed by `pipeline::develop_preview`:
   - `Raw` → `codec::read_camera_linear` (libraw `output_color=Raw`, `use_camera_wb=false`, `half_size=true` for preview) → planar f32 buffer → resize to target → `CameraToWorking` (WB + cam→Rec.2020) → 12-knob chain → `Rec2020ToSrgb` → `SrgbEncode`.
   - `Jpeg` → `read_image_pixels` via `jpeg-decoder` with IDCT `.scale()` to the target size.
   - `Png` / `Tiff` → `read_image_pixels` via `image::ImageReader::open` at native resolution, then resize.
   EXIF orientation is read at decode time and applied so portraits render upright. For RAW the libraw `flip` code drives auto-rotate inside `read_demosaiced` (`params.user_flip = -1`).
4. Results land in the LRU preview cache (capacity 9, keyed by canonical path) as `PreviewEntry { proto, src_w, src_h, rendered_target, params_fingerprint }`. Stale results from prior generations are dropped on receive.
5. The image area renders the cached preview at a centered aspect-fit sub-rect — landscape uses the full preview width centered vertically, portrait uses the full preview height centered horizontally.
6. If decoding fails, the cache stays empty for that file and a text placeholder is shown with the error surfaced in the status line.

## Acceptance Criteria

- `terminalroom <directory>` opens the culling view for supported image files (RAW + JPEG/PNG/TIFF) in that directory.
- `.terminalroom.db` is created in the session root.
- `p`, `x`, and `u` persist state changes immediately.
- Restarting the app restores prior culling decisions.
- `f` opens the format filter popup; toggling a format hides matching files and updates the visible count in the status line.
- `d` opens the develop view (12 knobs over the live preview) and `c` returns to culling. `j/k` navigate knobs, `h/l` adjust, `r` resets the focused knob.
- `q` exits and restores the terminal.

