# Looks (XMP-driven recipes)

## Why 4 develop knobs

Earlier versions exposed 12 develop knobs (Exposure, Temperature, Tint, Look
Strength, Warmth, Color, Contrast, Soft Highlights, Shadows, Blacks, Clarity,
Grain). The user-facing surface is now four logical inputs:

1. **Exposure** — physical correction every shot occasionally needs.
2. **WB** (Temperature + Tint) — illuminant correction; only the camera knows
   what the scene looked like.
3. **Look** — a curated preset chosen from a library of XMP-driven recipes.
4. **Look Strength** — the blend factor between the look and the neutral
   render.

Everything else that used to be a knob is information that *belongs in a Look*
— a photographer makes those decisions once for an aesthetic and then reuses
the recipe across shoots. Surfacing them as per-image sliders pretends they're
per-image decisions, which they almost never are. The redesign makes the
common path (apply a known look) one keystroke and demotes the long tail
(per-image fine-tuning) to a follow-up advanced mode.

## Why darkroom keeps the Control impls

The `Control` impls in `crates/darkroom/src/control/{tone,color,detail,look}.rs`
(Warmth, Color, Contrast, SoftHighlights, Shadows, Blacks, Clarity, Grain) are
**unwired from the live pipeline** but stay in the crate and stay tested.
They're the primitives the upcoming XMP applier composes; deleting them
would make the next phase strictly harder. The cut happens at the call site
in `apply_from_camera_linear` and at the field set of `DevelopParams` —
nowhere else.

## Watch directory

The Looks modal is filesystem-driven. Drop XMP sidecars into
`~/.terminalroom/looks/`; the modal scans the directory on every open and
reconciles it with the DB:

- new `*.xmp` files are parsed and registered (one DB row, one in-memory
  registry entry);
- rows whose source file is gone are deleted;
- parse failures are skipped silently with a status-bar notice — a stray
  malformed sidecar should never crash the TUI.

There's no in-modal "add" or "delete" UI. To remove a look from the library,
delete the `.xmp` file from `~/.terminalroom/looks/` and reopen the modal.

## Look identifier slug

Each registered look has a stable slug `"xmp:<source_fp_hex>"`. The fingerprint
reuses `db::compute_source_fp(size, mtime)` — the same family as files. Two
properties fall out:

- The slug is **deterministic from file content**: re-importing the same
  XMP after a delete produces the same slug, so any file referencing the
  deleted look self-heals on re-add.
- The slug is **stable across renames**: renaming an XMP in the watch
  directory doesn't change its identity (size+mtime preserved).

The slug is what's stored in `DevelopParams.look`. Built-in `"identity"` is
reserved as the only static look (the no-op default).

## Stub status: `ApplyXmp` is currently identity

`crates/darkroom/src/transform/xmp.rs` parses an XMP into a strongly-typed
`XmpRecipe`, but `ApplyXmp::apply` is a **no-op**. Selecting a look in the
modal correctly persists the slug, the pipeline correctly resolves it to the
recipe, but the actual XMP→pixels math is deferred. The data flow is wired
end-to-end (UI → DB → registry → pipeline) so the math drops in cleanly.

The roadmap from the file-level doc in `transform/xmp.rs`:

| XMP field | Maps to |
|---|---|
| `Exposure2012` | `crate::control::input::Exposure` |
| `Contrast2012`, `Parametric*` | `primitive::curve::ToneCurve` |
| Tone curves (master + R/G/B) | new `primitive::point_curve` + ProPhoto-tone-curve gamma |
| `Highlights2012`, `Shadows2012`, `Whites2012`, `Blacks2012` | PV2012 variants of `control::tone` ops |
| `Texture`, `Clarity2012` | multi-scale `control::detail::Clarity` |
| `Dehaze` | new (dark-channel-prior op) |
| `Vibrance`, `Saturation` | `control::color::Color` retuned + new global saturation |
| HSL bands × 8 | new OKLCh hue-domain primitive |
| Split toning, color grade | new shadow/highlight tinting |
| Sharpening, NR, lens corrections | out of scope for v1 |
| `CameraProfile` (DCP) | requires DCP parser; deferred |

The canonical reference XMP is `example.xmp` at the project root (a
third-party Camera Raw preset; the `<crs:Name>` is whatever the original
author wrote). The parser is exercised against it in
`darkroom::transform::xmp::tests`.

## When you'd choose Look vs. Exposure/WB

Exposure and WB are scene-and-frame inputs the photographer adjusts per shot
based on what the camera captured. Looks are aesthetic decisions — what the
photographer wants their work to *feel* like. The redesigned UI keeps these
two responsibilities cleanly separated; the Look picker doesn't fight with
fine-tuning the WB on a tricky-light shot.
