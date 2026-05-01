//! XMP (Adobe Camera Raw sidecar) — stub-but-typed.
//!
//! `parse_xmp(xml)` extracts the `crs:` settings of the `<rdf:Description>`
//! element into an [`XmpRecipe`]. [`ApplyXmp`] is a `Control` over
//! `Buffer<LinearRec2020>` whose `apply` is currently a **no-op**. The data
//! flow is wired end-to-end (UI → DB → pipeline), but the actual XMP→pixel
//! math is deferred. See `docs/looks.md` for the roadmap.
//!
//! Eventual mapping from XMP fields to existing primitives:
//!
//! - `exposure_2012` → [`crate::control::input::Exposure`]
//! - `contrast_2012`, `parametric_*` → [`crate::primitive::curve::ToneCurve`]
//! - `tone_curve_pv2012` (master + R/G/B) → new `primitive::point_curve`
//!   (cubic Hermite) evaluated in ProPhoto-tone-curve gamma
//!   (new `transform::encode::prophoto`)
//! - `highlights_2012`, `shadows_2012`, `whites_2012`, `blacks_2012` →
//!   variants of [`crate::control::tone::SoftHighlights`],
//!   [`crate::control::tone::Shadows`], [`crate::control::tone::Blacks`]
//!   with PV2012-specific log-luma masks
//! - `texture`, `clarity_2012` → multi-scale variant of
//!   [`crate::control::detail::Clarity`]
//! - `dehaze` → new (dark-channel-prior op)
//! - `vibrance`, `saturation` → [`crate::control::color::Color`] / new
//!   global saturation primitive
//! - HSL bands (8 × {Hue, Sat, Lum}) → new primitive in OKLCh hue domain
//! - Split toning, color grading → new shadow/highlight tinting primitive
//! - Sharpening, NR, lens corrections — out of scope for v1.

use std::error::Error;
use std::fmt;

use quick_xml::Reader;
use quick_xml::events::Event;
use quick_xml::events::attributes::AttrError;

use crate::control::Control;
use crate::space::{Buffer, LinearRec2020};

/// Single point on a PV2012 tone curve. Both axes are 8-bit (0..=255), as
/// stored by Adobe Camera Raw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CurvePoint {
    pub x: u8,
    pub y: u8,
}

/// One HSL color band (Red / Orange / Yellow / Green / Aqua / Blue / Purple /
/// Magenta). Values are in ACR's slider domain (typically -100..=100); the
/// applier rescales to internal units.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HslBand {
    pub hue: f32,
    pub saturation: f32,
    pub luminance: f32,
}

/// Parsed XMP develop settings. Only fields exercised by `example.xmp` today;
/// extend as new XMP-driven tags need first-class support.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct XmpRecipe {
    pub name: Option<String>,
    pub camera_profile: Option<String>,
    pub white_balance: Option<String>,

    pub exposure_2012: Option<f32>,
    pub contrast_2012: Option<f32>,
    pub highlights_2012: Option<f32>,
    pub shadows_2012: Option<f32>,
    pub whites_2012: Option<f32>,
    pub blacks_2012: Option<f32>,
    pub texture: Option<f32>,
    pub clarity_2012: Option<f32>,
    pub dehaze: Option<f32>,
    pub vibrance: Option<f32>,
    pub saturation: Option<f32>,

    pub parametric_shadows: Option<f32>,
    pub parametric_darks: Option<f32>,
    pub parametric_lights: Option<f32>,
    pub parametric_highlights: Option<f32>,
    pub parametric_shadow_split: Option<f32>,
    pub parametric_midtone_split: Option<f32>,
    pub parametric_highlight_split: Option<f32>,

    pub tone_curve_pv2012: Vec<CurvePoint>,
    pub tone_curve_pv2012_red: Vec<CurvePoint>,
    pub tone_curve_pv2012_green: Vec<CurvePoint>,
    pub tone_curve_pv2012_blue: Vec<CurvePoint>,

    pub hsl_red: HslBand,
    pub hsl_orange: HslBand,
    pub hsl_yellow: HslBand,
    pub hsl_green: HslBand,
    pub hsl_aqua: HslBand,
    pub hsl_blue: HslBand,
    pub hsl_purple: HslBand,
    pub hsl_magenta: HslBand,

    pub split_toning_shadow_hue: Option<f32>,
    pub split_toning_shadow_saturation: Option<f32>,
    pub split_toning_highlight_hue: Option<f32>,
    pub split_toning_highlight_saturation: Option<f32>,
    pub split_toning_balance: Option<f32>,

    pub color_grade_midtone_hue: Option<f32>,
    pub color_grade_midtone_sat: Option<f32>,
    pub color_grade_midtone_lum: Option<f32>,
    pub color_grade_shadow_lum: Option<f32>,
    pub color_grade_highlight_lum: Option<f32>,
    pub color_grade_global_hue: Option<f32>,
    pub color_grade_global_sat: Option<f32>,
    pub color_grade_global_lum: Option<f32>,
    pub color_grade_blending: Option<f32>,

    pub sharpness: Option<f32>,
    pub sharpen_radius: Option<f32>,
    pub sharpen_detail: Option<f32>,
    pub sharpen_edge_masking: Option<f32>,

    pub luminance_smoothing: Option<f32>,
    pub color_noise_reduction: Option<f32>,
    pub color_noise_reduction_detail: Option<f32>,
    pub color_noise_reduction_smoothness: Option<f32>,
}

#[derive(Debug)]
pub enum XmpParseError {
    Xml(quick_xml::Error),
    Attr(AttrError),
}

impl fmt::Display for XmpParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Xml(e) => write!(f, "XMP XML error: {e}"),
            Self::Attr(e) => write!(f, "XMP attribute error: {e}"),
        }
    }
}

impl Error for XmpParseError {}

impl From<quick_xml::Error> for XmpParseError {
    fn from(e: quick_xml::Error) -> Self {
        Self::Xml(e)
    }
}

impl From<AttrError> for XmpParseError {
    fn from(e: AttrError) -> Self {
        Self::Attr(e)
    }
}

/// Parse an XMP sidecar's XML into an [`XmpRecipe`]. Tolerant of unknown
/// `crs:` attributes (silently ignored).
pub fn parse_xmp(xml: &str) -> Result<XmpRecipe, XmpParseError> {
    let mut reader = Reader::from_str(xml);
    let mut recipe = XmpRecipe::default();

    enum CurveSlot {
        Master,
        Red,
        Green,
        Blue,
    }
    enum NameSlot {
        Name,
        Other,
    }

    let mut active_curve: Option<CurveSlot> = None;
    let mut active_name: Option<NameSlot> = None;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => {
                let local = e.name().as_ref().to_vec();
                if local == b"rdf:Description" {
                    for attr in e.attributes() {
                        let attr = attr?;
                        let key_full = attr.key.as_ref().to_vec();
                        let stripped = key_full
                            .strip_prefix(b"crs:" as &[u8])
                            .unwrap_or(&key_full[..]);
                        let value = attr.unescape_value()?;
                        apply_attribute(&mut recipe, stripped, value.as_ref());
                    }
                } else if local == b"crs:ToneCurvePV2012" {
                    active_curve = Some(CurveSlot::Master);
                } else if local == b"crs:ToneCurvePV2012Red" {
                    active_curve = Some(CurveSlot::Red);
                } else if local == b"crs:ToneCurvePV2012Green" {
                    active_curve = Some(CurveSlot::Green);
                } else if local == b"crs:ToneCurvePV2012Blue" {
                    active_curve = Some(CurveSlot::Blue);
                } else if local == b"crs:Name" {
                    active_name = Some(NameSlot::Name);
                } else if matches!(
                    local.as_slice(),
                    b"crs:ShortName" | b"crs:SortName" | b"crs:Group" | b"crs:Description"
                ) {
                    active_name = Some(NameSlot::Other);
                }
            }
            Event::End(e) => {
                let local = e.name().as_ref().to_vec();
                if local.starts_with(b"crs:ToneCurvePV2012") {
                    active_curve = None;
                } else if matches!(
                    local.as_slice(),
                    b"crs:Name"
                        | b"crs:ShortName"
                        | b"crs:SortName"
                        | b"crs:Group"
                        | b"crs:Description"
                ) {
                    active_name = None;
                }
            }
            Event::Text(t) => {
                let text = t.unescape()?;
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Some(slot) = &active_curve {
                    if let Some(point) = parse_curve_point(trimmed) {
                        let target = match slot {
                            CurveSlot::Master => &mut recipe.tone_curve_pv2012,
                            CurveSlot::Red => &mut recipe.tone_curve_pv2012_red,
                            CurveSlot::Green => &mut recipe.tone_curve_pv2012_green,
                            CurveSlot::Blue => &mut recipe.tone_curve_pv2012_blue,
                        };
                        target.push(point);
                    }
                }
                if let Some(NameSlot::Name) = &active_name {
                    if recipe.name.is_none() {
                        recipe.name = Some(trimmed.to_string());
                    }
                }
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(recipe)
}

fn parse_curve_point(s: &str) -> Option<CurvePoint> {
    let mut parts = s.split(',').map(str::trim);
    let x: u32 = parts.next()?.parse().ok()?;
    let y: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(CurvePoint {
        x: x.min(255) as u8,
        y: y.min(255) as u8,
    })
}

fn apply_attribute(r: &mut XmpRecipe, key: &[u8], value: &str) {
    let parsed_f = || -> Option<f32> { value.parse::<f32>().ok() };
    match key {
        b"CameraProfile" => r.camera_profile = Some(value.to_string()),
        b"WhiteBalance" => r.white_balance = Some(value.to_string()),
        b"Exposure2012" => r.exposure_2012 = parsed_f(),
        b"Contrast2012" => r.contrast_2012 = parsed_f(),
        b"Highlights2012" => r.highlights_2012 = parsed_f(),
        b"Shadows2012" => r.shadows_2012 = parsed_f(),
        b"Whites2012" => r.whites_2012 = parsed_f(),
        b"Blacks2012" => r.blacks_2012 = parsed_f(),
        b"Texture" => r.texture = parsed_f(),
        b"Clarity2012" => r.clarity_2012 = parsed_f(),
        b"Dehaze" => r.dehaze = parsed_f(),
        b"Vibrance" => r.vibrance = parsed_f(),
        b"Saturation" => r.saturation = parsed_f(),

        b"ParametricShadows" => r.parametric_shadows = parsed_f(),
        b"ParametricDarks" => r.parametric_darks = parsed_f(),
        b"ParametricLights" => r.parametric_lights = parsed_f(),
        b"ParametricHighlights" => r.parametric_highlights = parsed_f(),
        b"ParametricShadowSplit" => r.parametric_shadow_split = parsed_f(),
        b"ParametricMidtoneSplit" => r.parametric_midtone_split = parsed_f(),
        b"ParametricHighlightSplit" => r.parametric_highlight_split = parsed_f(),

        b"HueAdjustmentRed" => r.hsl_red.hue = parsed_f().unwrap_or(0.0),
        b"HueAdjustmentOrange" => r.hsl_orange.hue = parsed_f().unwrap_or(0.0),
        b"HueAdjustmentYellow" => r.hsl_yellow.hue = parsed_f().unwrap_or(0.0),
        b"HueAdjustmentGreen" => r.hsl_green.hue = parsed_f().unwrap_or(0.0),
        b"HueAdjustmentAqua" => r.hsl_aqua.hue = parsed_f().unwrap_or(0.0),
        b"HueAdjustmentBlue" => r.hsl_blue.hue = parsed_f().unwrap_or(0.0),
        b"HueAdjustmentPurple" => r.hsl_purple.hue = parsed_f().unwrap_or(0.0),
        b"HueAdjustmentMagenta" => r.hsl_magenta.hue = parsed_f().unwrap_or(0.0),

        b"SaturationAdjustmentRed" => r.hsl_red.saturation = parsed_f().unwrap_or(0.0),
        b"SaturationAdjustmentOrange" => r.hsl_orange.saturation = parsed_f().unwrap_or(0.0),
        b"SaturationAdjustmentYellow" => r.hsl_yellow.saturation = parsed_f().unwrap_or(0.0),
        b"SaturationAdjustmentGreen" => r.hsl_green.saturation = parsed_f().unwrap_or(0.0),
        b"SaturationAdjustmentAqua" => r.hsl_aqua.saturation = parsed_f().unwrap_or(0.0),
        b"SaturationAdjustmentBlue" => r.hsl_blue.saturation = parsed_f().unwrap_or(0.0),
        b"SaturationAdjustmentPurple" => r.hsl_purple.saturation = parsed_f().unwrap_or(0.0),
        b"SaturationAdjustmentMagenta" => r.hsl_magenta.saturation = parsed_f().unwrap_or(0.0),

        b"LuminanceAdjustmentRed" => r.hsl_red.luminance = parsed_f().unwrap_or(0.0),
        b"LuminanceAdjustmentOrange" => r.hsl_orange.luminance = parsed_f().unwrap_or(0.0),
        b"LuminanceAdjustmentYellow" => r.hsl_yellow.luminance = parsed_f().unwrap_or(0.0),
        b"LuminanceAdjustmentGreen" => r.hsl_green.luminance = parsed_f().unwrap_or(0.0),
        b"LuminanceAdjustmentAqua" => r.hsl_aqua.luminance = parsed_f().unwrap_or(0.0),
        b"LuminanceAdjustmentBlue" => r.hsl_blue.luminance = parsed_f().unwrap_or(0.0),
        b"LuminanceAdjustmentPurple" => r.hsl_purple.luminance = parsed_f().unwrap_or(0.0),
        b"LuminanceAdjustmentMagenta" => r.hsl_magenta.luminance = parsed_f().unwrap_or(0.0),

        b"SplitToningShadowHue" => r.split_toning_shadow_hue = parsed_f(),
        b"SplitToningShadowSaturation" => r.split_toning_shadow_saturation = parsed_f(),
        b"SplitToningHighlightHue" => r.split_toning_highlight_hue = parsed_f(),
        b"SplitToningHighlightSaturation" => r.split_toning_highlight_saturation = parsed_f(),
        b"SplitToningBalance" => r.split_toning_balance = parsed_f(),

        b"ColorGradeMidtoneHue" => r.color_grade_midtone_hue = parsed_f(),
        b"ColorGradeMidtoneSat" => r.color_grade_midtone_sat = parsed_f(),
        b"ColorGradeMidtoneLum" => r.color_grade_midtone_lum = parsed_f(),
        b"ColorGradeShadowLum" => r.color_grade_shadow_lum = parsed_f(),
        b"ColorGradeHighlightLum" => r.color_grade_highlight_lum = parsed_f(),
        b"ColorGradeGlobalHue" => r.color_grade_global_hue = parsed_f(),
        b"ColorGradeGlobalSat" => r.color_grade_global_sat = parsed_f(),
        b"ColorGradeGlobalLum" => r.color_grade_global_lum = parsed_f(),
        b"ColorGradeBlending" => r.color_grade_blending = parsed_f(),

        b"Sharpness" => r.sharpness = parsed_f(),
        b"SharpenRadius" => r.sharpen_radius = parsed_f(),
        b"SharpenDetail" => r.sharpen_detail = parsed_f(),
        b"SharpenEdgeMasking" => r.sharpen_edge_masking = parsed_f(),

        b"LuminanceSmoothing" => r.luminance_smoothing = parsed_f(),
        b"ColorNoiseReduction" => r.color_noise_reduction = parsed_f(),
        b"ColorNoiseReductionDetail" => r.color_noise_reduction_detail = parsed_f(),
        b"ColorNoiseReductionSmoothness" => r.color_noise_reduction_smoothness = parsed_f(),

        _ => {}
    }
}

/// `Control` form of an XMP recipe. Currently a no-op stub; the file-level
/// doc lists the eventual primitive composition.
pub struct ApplyXmp<'a> {
    pub recipe: &'a XmpRecipe,
    /// Effective ISO of the source frame, used by the (future) ISO-attenuated
    /// stages (Shadows, Color, Clarity, Grain).
    pub iso: f32,
}

impl<'a> Control for ApplyXmp<'a> {
    type Space = LinearRec2020;
    fn apply(&self, _image: &mut Buffer<LinearRec2020>) {
        // STUB: identity. Real implementation TBD; see file-level doc and
        // docs/looks.md.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE_XMP: &str = include_str!("../../../../example.xmp");

    #[test]
    fn parse_example_xmp_extracts_name_and_curve() {
        let recipe = parse_xmp(EXAMPLE_XMP).expect("parse");
        // Name is whatever the example.xmp's <crs:Name> contains. We assert
        // it parses to a non-empty string rather than echoing the literal
        // (the literal is the original author's preset name).
        let name = recipe.name.as_deref().expect("name parsed");
        assert!(!name.is_empty(), "expected a non-empty <crs:Name>");
        assert!((recipe.exposure_2012.unwrap() - 0.6).abs() < 1e-4);
        assert_eq!(recipe.tone_curve_pv2012.len(), 5);
        assert_eq!(recipe.tone_curve_pv2012[0], CurvePoint { x: 0, y: 0 });
        assert_eq!(recipe.tone_curve_pv2012[1], CurvePoint { x: 16, y: 21 });
        assert_eq!(recipe.tone_curve_pv2012[4], CurvePoint { x: 255, y: 255 });
        assert_eq!(recipe.tone_curve_pv2012_red.len(), 7);
        assert_eq!(recipe.tone_curve_pv2012_green.len(), 6);
        assert_eq!(recipe.tone_curve_pv2012_blue.len(), 6);
        assert_eq!(recipe.camera_profile.as_deref(), Some("Camera Positive Film"));
        assert_eq!(recipe.white_balance.as_deref(), Some("As Shot"));
    }

    #[test]
    fn parse_example_xmp_extracts_hsl_and_split_toning() {
        let recipe = parse_xmp(EXAMPLE_XMP).expect("parse");
        assert_eq!(recipe.hsl_red.hue, -5.0);
        assert_eq!(recipe.hsl_orange.hue, -3.0);
        assert_eq!(recipe.hsl_red.saturation, 7.0);
        assert_eq!(recipe.hsl_orange.saturation, -15.0);
        assert_eq!(recipe.hsl_magenta.saturation, -17.0);
        assert_eq!(recipe.hsl_red.luminance, -10.0);
        assert_eq!(recipe.hsl_magenta.luminance, -24.0);
        assert_eq!(recipe.split_toning_shadow_hue, Some(176.0));
        assert_eq!(recipe.split_toning_shadow_saturation, Some(9.0));
        assert_eq!(recipe.split_toning_highlight_hue, Some(57.0));
        assert_eq!(recipe.split_toning_highlight_saturation, Some(13.0));
        assert_eq!(recipe.vibrance, Some(41.0));
        assert_eq!(recipe.saturation, Some(-16.0));
    }

    #[test]
    fn parse_xmp_tolerates_unrelated_input() {
        // The parser is intentionally permissive: text without any
        // recognized `crs:` attributes parses to the default recipe rather
        // than panicking. Strict validation is intentionally out of scope —
        // a stray sidecar should never crash the develop pipeline.
        let recipe = parse_xmp("hello world").expect("parse");
        assert!(recipe.name.is_none());
        assert!(recipe.exposure_2012.is_none());
        assert!(recipe.tone_curve_pv2012.is_empty());
    }

    #[test]
    fn apply_xmp_is_identity_in_stub_mode() {
        let recipe = XmpRecipe::default();
        let applier = ApplyXmp {
            recipe: &recipe,
            iso: 100.0,
        };
        let mut buf: Buffer<LinearRec2020> = Buffer::from_planar(vec![0.5_f32; 24], 4, 2);
        let original = buf.data().to_vec();
        applier.apply(&mut buf);
        assert_eq!(buf.data(), original.as_slice());
    }

    #[test]
    fn parse_curve_point_handles_whitespace_and_bounds() {
        assert_eq!(parse_curve_point("0, 0"), Some(CurvePoint { x: 0, y: 0 }));
        assert_eq!(parse_curve_point("  16, 21 "), Some(CurvePoint { x: 16, y: 21 }));
        assert_eq!(parse_curve_point("999, 999"), Some(CurvePoint { x: 255, y: 255 }));
        assert_eq!(parse_curve_point("not, a, point"), None);
        assert_eq!(parse_curve_point("missing"), None);
    }
}
