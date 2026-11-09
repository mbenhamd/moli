//! The Canvas 2D `font` attribute and `measureText`.
//!
//! Assignments parse with the CSS `font` shorthand grammar shared with style
//! declarations. Relative sizes resolve against the canvas element's computed
//! font size (10px without one), and the attribute stores the Chromium
//! serialization of the resolved font. Measurement shapes the text through the
//! document's layout font services, so web fonts, `unicode-range` segments and
//! the platform's generic-family mapping match CSS layout.

use moli_css_parse::{
    CssFontFamily, CssNumericContext, CssNumericKind, FontSelectionCapabilities, UnitlessLength,
    parse_font_shorthand, resolve_css_numeric,
};
use moli_layout::{TextMeasureFont, TextMeasureFontStyle, TextMeasurement};
use style_traits::ToCss;

/// Font size used when the canvas has no computed style (spec: "10px
/// sans-serif" is the default font).
pub(super) const DEFAULT_CANVAS_FONT_SIZE_PX: f64 = 10.0;
const DEFAULT_ROOT_FONT_SIZE_PX: f64 = 16.0;

/// Font sizes relative units resolve against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CanvasFontSizeBasis {
    pub font_size: f64,
    pub root_font_size: f64,
}

impl Default for CanvasFontSizeBasis {
    fn default() -> Self {
        Self {
            font_size: DEFAULT_CANVAS_FONT_SIZE_PX,
            root_font_size: DEFAULT_ROOT_FONT_SIZE_PX,
        }
    }
}

/// A parsed, resolved canvas font.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CanvasFont {
    family: String,
    size: f32,
    weight: f32,
    style: TextMeasureFontStyle,
    stretch: f32,
    small_caps: bool,
}

impl CanvasFont {
    /// Parses a `font` attribute value. `None` means the assignment is
    /// ignored: invalid syntax, CSS-wide keywords, `var()` and system fonts.
    pub(super) fn parse(value: &str, basis: CanvasFontSizeBasis) -> Option<Self> {
        let shorthand = parse_font_shorthand(value)?;
        let size = resolve_font_size(shorthand.size.trim(), basis)?;
        let capabilities =
            FontSelectionCapabilities::new(&shorthand.weight, &shorthand.stretch, &shorthand.style);
        let style = match shorthand.style.split_whitespace().collect::<Vec<_>>()[..] {
            ["italic"] => TextMeasureFontStyle::Italic,
            ["oblique"] => TextMeasureFontStyle::Oblique(None),
            ["oblique", ..] => TextMeasureFontStyle::Oblique(Some(capabilities.slope.min)),
            _ => TextMeasureFontStyle::Normal,
        };
        let family = shorthand
            .families
            .iter()
            .map(serialize_family)
            .collect::<Vec<_>>()
            .join(", ");
        if family.is_empty() {
            return None;
        }
        Some(Self {
            family,
            size: size as f32,
            weight: capabilities.weight.min,
            style,
            stretch: capabilities.width.min,
            small_caps: shorthand.variant_caps.trim() == "small-caps",
        })
    }

    /// Chromium's serialization: `[italic] [weight] [small-caps] <px> <families>`.
    /// Oblique angles, `font-stretch` and `line-height` are not serialized.
    pub(super) fn serialize(&self) -> String {
        let mut parts = Vec::new();
        if matches!(
            self.style,
            TextMeasureFontStyle::Italic | TextMeasureFontStyle::Oblique(None)
        ) {
            parts.push("italic".to_owned());
        }
        if self.weight == 700.0 {
            parts.push("bold".to_owned());
        } else if self.weight != 400.0 {
            parts.push(format!("{}", self.weight));
        }
        if self.small_caps {
            parts.push("small-caps".to_owned());
        }
        parts.push(format!("{}px", self.size));
        parts.push(self.family.clone());
        parts.join(" ")
    }

    pub(super) fn measure_font(&self) -> TextMeasureFont<'_> {
        TextMeasureFont {
            family: &self.family,
            size: self.size,
            weight: self.weight,
            style: self.style,
            stretch: self.stretch,
            letter_spacing: 0.0,
            word_spacing: 0.0,
        }
    }
}

/// `TextMetrics` values for one `measureText` call, relative to the
/// alphabetic baseline and the start of the text (`textAlign`/`textBaseline`
/// are not modeled by the context and use their initial values).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct CanvasTextMetrics {
    pub width: f64,
    pub actual_bounding_box_left: f64,
    pub actual_bounding_box_right: f64,
    pub font_bounding_box_ascent: f64,
    pub font_bounding_box_descent: f64,
    pub actual_bounding_box_ascent: f64,
    pub actual_bounding_box_descent: f64,
    pub hanging_baseline: f64,
    pub alphabetic_baseline: f64,
    pub ideographic_baseline: f64,
}

impl From<TextMeasurement> for CanvasTextMetrics {
    fn from(measured: TextMeasurement) -> Self {
        // Blink reports the primary font's integer ascent/descent here.
        let ascent = f64::from(measured.ascent).round();
        let descent = f64::from(measured.descent).round();
        let ink = measured.ink.unwrap_or_default();
        Self {
            width: f64::from(measured.advance),
            actual_bounding_box_left: f64::from(ink.left),
            actual_bounding_box_right: f64::from(ink.right),
            font_bounding_box_ascent: ascent,
            font_bounding_box_descent: descent,
            actual_bounding_box_ascent: f64::from(ink.ascent),
            actual_bounding_box_descent: f64::from(ink.descent),
            hanging_baseline: ascent * 0.8,
            alphabetic_baseline: 0.0,
            ideographic_baseline: -descent,
        }
    }
}

fn resolve_font_size(size: &str, basis: CanvasFontSizeBasis) -> Option<f64> {
    // Blink's keyword table for a 16px medium font.
    let keyword = match size {
        "xx-small" => Some(9.0),
        "x-small" => Some(10.0),
        "small" => Some(13.0),
        "medium" => Some(16.0),
        "large" => Some(18.0),
        "x-large" => Some(24.0),
        "xx-large" => Some(32.0),
        "xxx-large" => Some(48.0),
        "larger" => Some(basis.font_size * 1.2),
        "smaller" => Some(basis.font_size / 1.2),
        _ => None,
    };
    let px = keyword.or_else(|| {
        resolve_css_numeric(
            size,
            CssNumericKind::LengthPercentage {
                basis: basis.font_size,
                unitless: UnitlessLength::ZeroOnly,
            },
            CssNumericContext {
                font_size_px: Some(basis.font_size),
                root_font_size_px: Some(basis.root_font_size),
                ..CssNumericContext::default()
            },
        )
        .and_then(|value| value.px_length())
    })?;
    // Blink stores the used size as a float.
    let px = f64::from(px as f32);
    (px.is_finite() && px >= 0.0).then_some(px)
}

fn serialize_family(family: &CssFontFamily) -> String {
    match family {
        CssFontFamily::Generic(generic) => generic.to_css_string(),
        CssFontFamily::Named(name) if is_bare_family_name(name) => name.clone(),
        CssFontFamily::Named(name) => {
            let mut quoted = String::with_capacity(name.len() + 2);
            quoted.push('"');
            for character in name.chars() {
                if matches!(character, '"' | '\\') {
                    quoted.push('\\');
                }
                quoted.push(character);
            }
            quoted.push('"');
            quoted
        }
    }
}

/// Blink serializes a family name without quotes only when it is a single
/// identifier that cannot be read back as a keyword.
fn is_bare_family_name(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    let ident_start = |character: char| {
        character.is_ascii_alphabetic() || character == '_' || !character.is_ascii()
    };
    let starts =
        ident_start(first) || (first == '-' && name[1..].chars().next().is_some_and(ident_start));
    let reserved = [
        "serif",
        "sans-serif",
        "monospace",
        "cursive",
        "fantasy",
        "system-ui",
        "math",
        "emoji",
        "fangsong",
        "ui-serif",
        "ui-sans-serif",
        "ui-monospace",
        "ui-rounded",
        "inherit",
        "initial",
        "unset",
        "revert",
        "revert-layer",
        "default",
    ];
    starts
        && name.chars().all(|character| {
            ident_start(character) || character.is_ascii_digit() || character == '-'
        })
        && !reserved
            .iter()
            .any(|keyword| keyword.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serialized(value: &str) -> Option<String> {
        CanvasFont::parse(
            value,
            CanvasFontSizeBasis {
                font_size: 16.0,
                root_font_size: 16.0,
            },
        )
        .map(|font| font.serialize())
    }

    #[test]
    fn serializes_like_chromium() {
        for (input, expected) in [
            (
                "italic 500 22px/1.3 \"Source Serif 4\", Georgia, serif",
                "italic 500 22px \"Source Serif 4\", Georgia, serif",
            ),
            (
                "bold small-caps italic 16px cursive",
                "italic bold small-caps 16px cursive",
            ),
            ("oblique 12px serif", "italic 12px serif"),
            ("oblique 20deg 12px serif", "12px serif"),
            ("lighter 12px serif", "100 12px serif"),
            ("normal 400 condensed 12px serif", "12px serif"),
            ("1em serif", "16px serif"),
            ("50% serif", "8px serif"),
            ("larger serif", "19.2px serif"),
            ("2rem serif", "32px serif"),
            ("12pt serif", "16px serif"),
            ("12px Times New Roman", "12px \"Times New Roman\""),
            ("10px  Georgia ,serif", "10px Georgia, serif"),
        ] {
            assert_eq!(serialized(input).as_deref(), Some(expected), "{input}");
        }
    }

    #[test]
    fn rejects_values_chromium_ignores() {
        for input in [
            "bogus",
            "12px",
            "inherit",
            "initial",
            "var(--font)",
            "10px sans-serif bold",
            "",
        ] {
            assert_eq!(serialized(input), None, "{input}");
        }
    }
}
