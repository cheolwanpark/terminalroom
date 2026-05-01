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

## Layout

The app opens in a single screen. The layout is fixed:

```text
+--------------------------------------------------------------------+
|     TERMINALROOM   (ASCII banner — 6 rows, ANSI Shadow)            |
+--------------------------------------------------------------------+
|                  | Develop      | Image Info     | Navigation       |
|                  | Exposure  +0 | Make:  SONY    |  IMG_0421        |
|     preview      |▶Temp   5500K | Model: A7iii   |▶ IMG_0422        |
|                  | Tint    0.0  | ISO    400     |  IMG_0423 R      |
|                  | Look Str 1.0 | 1/250 f/2.8    |  IMG_0424        |
|                  | …            | 35mm           |                  |
|                  |              | File: IMG_…ARW |                  |
|                  |              | RAW · 24.3 MB  |                  |
|                  |              | 6000 × 4000    |                  |
+--------------------------------------------------------------------+
| <filename>  i/N  [REMOVED]  [filter:e/t]  <focus-aware shortcuts>  |
+--------------------------------------------------------------------+
```

Top-level vertical split: banner / main / 1-row status. Inner main horizontal split: preview (`Min(20)`) / Develop tab (28 cells) / Image Info tab (28 cells) / Navigation tab / filmstrip (28 cells).

The banner falls back to a single-row stylized title when the terminal is narrower than the full ASCII art (~100 cells).

The currently focused side tab is drawn with a thick yellow border; the other tabs use the default border. Image Info is read-only and never gets focus.

The filmstrip is text-only: each row shows the file name. Removed entries are dimmed and carry a red `R` badge on the right; non-removed entries render plain. The selected row is reverse-highlighted; long names are truncated with an ellipsis. The filmstrip auto-scrolls to keep the selection visible.

The status line shows the current filename, `i/N` (where `N` is the visible count after the active filter and the show-removed toggle), a bold red `REMOVED` label when the current entry is removed and visible (only possible when `show-removed` is on), an optional `filter: enabled/total` indicator when a filter is active, and a focus-aware shortcut hint.

## Focus Model

Two focus modes inside the main view:

- **Navigation focus** (default): keys move between images, mark them removed/restored, and toggle show-removed view.
- **Develop focus**: keys navigate and adjust the 5 develop entries (Exposure, Temperature, Tint, Look, Look Strength).

`Enter` in Navigation focus shifts focus to the Develop tab. `Esc` in Develop focus returns to Navigation. The active focus is indicated by the tab border (thick yellow on the focused tab; default border elsewhere) and by the shortcut hint in the status line.

Focus is fully modal: image-navigation keys are inert in Develop focus, knob keys are inert in Navigation focus. The filter popup is reachable from Navigation focus only (`f`); the Looks modal is reachable from Navigation focus (`L`) or by pressing `Enter` on the Look entry in Develop focus.

## Keybindings

Navigation focus:

```text
j / Down   next image
k / Up     previous image
x          mark current image as removed
r          restore current image (no-op if not removed)
R          toggle "show removed" (Shift+R)
f          open format filter popup
L          open looks modal (Shift+L)
Enter      focus the Develop tab
q          quit
```

Develop focus:

```text
j / Down   focus next knob
k / Up     focus previous knob
h / Left   decrement focused knob by one step (Look: cycle to previous look)
l / Right  increment focused knob by one step (Look: cycle to next look)
r          reset focused knob to default (Look: reset to "(none)")
Enter      open Looks modal (only when on the Look entry; inert otherwise)
Esc        return to Navigation focus
q          quit
```

Filter popup:

```text
j / Down           move cursor down
k / Up             move cursor up
Space              toggle the highlighted format
Enter / Esc / f    close popup
q                  quit
```

Looks modal:

```text
j / Down       move cursor down
k / Up         move cursor up
Enter          apply the selected look to the current file + close
Esc / L        close
q              quit
```

Navigation does not wrap around at the ends. Pressing next on the last image or previous on the first image keeps the current selection.

## Filter Popup

Press `f` from Navigation focus to open a centered modal popup with one row per format present in the current scan:

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

## Develop Tab

The Develop tab is a side column in the main layout — always visible. It shows the 5 user-facing entries (Exposure / Temperature / Tint / Look / Look Strength). The cursor highlights one entry at a time; in Develop focus, `h/l` adjusts the current entry by its step (or cycles the look for the discrete `Look` entry), `r` resets it, `j/k` move between entries, `Enter` on the `Look` entry opens the Looks modal. The preview re-renders whenever a knob changes (debounced through the same worker thread that handles culling — 50 ms once the worker has the source cached, 250 ms on the cold first preview).

Entry set:

```text
Exposure        (-3..+3 EV, step 0.05)
Temperature     (2000..12000 K, step 100)
Tint            (-1..+1, step 0.05)
Look            cycles through registered XMP looks; defaults to "(none)"
Look Strength   (0..1, step 0.05; blends look ↔ neutral)
```

Defaults are identity (Exposure 0, 5500 K WB, Tint 0, Look "(none)", Strength 1.0) — the default preview is the neutral develop. The eight pre-redesign knobs (Warmth, Color, Contrast, Soft Highlights, Shadows, Blacks, Clarity, Grain) are now carried inside curated XMP-driven Looks rather than surfaced as per-image sliders; see `looks.md`.

When the Develop tab is **not** focused, the entry list is dimmed (no cursor symbol, dim labels and values) so the focus state is visible at a glance. When focused, labels are normal weight, values are bold, and a `▶ ` cursor marks the focused entry.

Entry values are persisted per file in the global SQLite (`~/.terminalroom/db.sqlite`). Edits commit in-memory as soon as the tiered debounce expires (50 ms hot, 250 ms cold), and the matching SQLite write is queued for a save worker thread — the UI never blocks on disk during a knob tick. The same debounce gates re-rendering, so quickly held arrow keys settle into one re-render once you stop pressing. Force-flush points (file change, focus change, quit) route through the same async path; on quit the save worker is joined before exit so no edits are lost.

## Looks Modal

Press `L` from Navigation focus (or `Enter` on the `Look` entry from Develop focus) to open a centered modal listing the available looks:

```text
+-- Looks --------------------------------------------------+
| (none)                                                    |
| Sample Positive Look                                      |
| ...                                                       |
|                                                           |
| j/k navigate · enter apply · esc close ·                  |
|     drop XMPs in ~/.terminalroom/looks/                   |
+-----------------------------------------------------------+
```

The list is filesystem-driven: drop `.xmp` sidecars into `~/.terminalroom/looks/` and they appear here on the next modal open. There is no `add` or `delete` UI inside the modal — to remove a look, delete its file from the watch directory and reopen the modal. Parse failures are skipped silently with a status-bar notice; a stray malformed sidecar never crashes the TUI.

Selecting a look applies it to the current file (the slug is written to the file's `develop_params.look` and persisted via the standard debounce path). The applied look name shows up in the Develop tab's `Look` entry next time the tab is rendered.

The actual XMP→pixels math is currently a stub — selecting a look persists the choice and resolves it through the pipeline, but the rendered preview is identical to identity. Roadmap and rationale: see `looks.md`.

## Image Info Tab

The Image Info tab is a read-only side column showing two sections for the current selection:

- **Shoot**: Make, Model, ISO, Shutter (formatted `1/250` or `2.0s`), Aperture (`f/2.8`), Focal length (`35 mm`). Rows whose source `Option` is `None` are omitted.
- **File**: Name (truncated to fit), Format (`RAW` / `JPEG` / `PNG` / `TIFF`), Size (`24.3 MB`), Dimensions (`6000 × 4000`), EXIF orientation (only when ≠ 1).

Metadata is populated lazily by the preview worker — the first time a file is selected, the worker decodes the header and the result is cached in `App.file_meta`. Until the first preview job for that file completes, the tab shows a single dim "loading…" line.

## Preview Behavior

When the selected file changes, the terminal resize moves the preview target ≥ 25% in either dim, or a develop knob is adjusted (the `DevelopParams` fingerprint changes):

1. The preview pane reads from `displayed_path`, not the live cursor. While a held-key nav burst is in flight, `displayed_path` stays pinned to the last settled file so the pane shows that image stably (and after release, stays on it until the new file's preview lands in the cache rather than dropping to "loading…" text).
2. Both disk-cache installs and worker dispatch are gated on `NAV_SETTLE = 150 ms` of nav-input silence (anchored on the most recent nav event). Once settled: bump a per-selection generation and flip the prior selection's cancel flag, then enqueue one job for the new selection at the new target size and current params. Each job carries an `Arc<AtomicBool>` cancel flag, a clone of the `DevelopParams`, and the file's `size_bytes`.
3. The worker calls `darkroom::decode` (cheap header read), builds a `FileMeta` from the result, and runs `pipeline::develop_preview`:
   - `Raw` → `codec::read_camera_linear` (libraw `output_color=Raw`, `use_camera_wb=false`, Raw-mode `no_auto_scale=true`, `half_size=true` for preview) → planar f32 buffer normalized against sensor white after black subtraction → resize to target → `CameraToWorking` (WB + LibRaw-aligned cam→Rec.2020) → 12-knob chain → `Rec2020ToSrgb` → `SrgbEncode`.
   - `Jpeg` → `read_image_pixels` via `jpeg-decoder` with IDCT `.scale()` to the target size.
   - `Png` / `Tiff` → `read_image_pixels` via `image::ImageReader::open` at native resolution, then resize.
   EXIF orientation is read at decode time and applied so portraits render upright. For RAW the libraw `flip` code drives auto-rotate inside `read_demosaiced` (`params.user_flip = -1`).
4. Results land in the LRU preview cache (capacity 15, keyed by canonical path) as `PreviewEntry { proto, src_w, src_h, rendered_target, params_fingerprint }`. The `FileMeta` is written into `App.file_meta`. Stale results from prior generations are dropped on receive. When an entry is replaced or LRU-evicted the kitty `_Ga=d,d=I,i={id};` delete escape is emitted so the terminal frees the image's quota.
5. The image area renders the cached preview at a centered aspect-fit sub-rect — landscape uses the full preview width centered vertically, portrait uses the full preview height centered horizontally.
6. If decoding fails, the cache stays empty for that file and a text placeholder is shown with the error surfaced in the status line.

### Held-key navigation

Holding `j` / `k` (or `↓` / `↑`) in Navigation focus goes through a rate-limiter:

- The initial press advances the cursor immediately.
- For the first second of holding, the cursor advances at 5 Hz (one step per 200 ms) regardless of the OS auto-repeat rate — so the user can read what's flying past in the navigation list.
- After one second, the rate limit lifts and the cursor advances at the full incoming event rate.
- The preview pane stays frozen on whichever file was loaded before the burst started; image dispatch only fires after release, when nav input has been silent for 150 ms.

Single taps and slow tapping (>350 ms apart) bypass the rate limit — each press advances the cursor on its own.

## Acceptance Criteria

- `terminalroom <directory>` opens the single-screen layout for supported image files (RAW + JPEG/PNG/TIFF) in that directory.
- `.terminalroom.db` is created in the session root.
- The ASCII banner renders on top; falls back to a single-row title on terminals narrower than the full art.
- `p`, `x`, and `u` (Navigation focus only) persist state changes immediately.
- Restarting the app restores prior culling decisions.
- `f` opens the format filter popup; toggling a format hides matching files and updates the visible count in the status line.
- `Enter` from Navigation focus shifts focus to the Develop tab; the tab border becomes thick yellow. `j/k` navigates knobs, `h/l` adjusts, `r` resets the focused knob. `Esc` returns to Navigation focus.
- The Image Info tab shows shoot info + file info for the current selection; switching selections updates it (after the first preview job for that file completes).
- The status-line shortcut hint changes between the two focus modes.
- `q` exits and restores the terminal.
