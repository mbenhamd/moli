// SPDX-License-Identifier: MIT OR Apache-2.0

//! Single-line text measurement outside a box tree.
//!
//! SVG text geometry and Canvas `measureText` must agree with the glyphs that
//! CSS layout shapes. Both therefore resolve fonts through the document's
//! Parley services (web fonts, `unicode-range` segments and platform family
//! substitution) instead of carrying their own font tables.

use std::borrow::Cow;

use parley::{
    FontFamily, FontFamilyName, FontStyle, FontWeight, FontWidth, TextStyle, TextWrapMode,
};
use style::properties::ComputedValues;

use super::{DocumentLayoutServices, ParleyDocumentServices};
use crate::stylo_to_parley::{TextBrush, text_style};

/// CSS `font-style` of a [`TextMeasureFont`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum TextMeasureFontStyle {
    #[default]
    Normal,
    Italic,
    /// Oblique angle in degrees; `None` uses the font's default angle.
    Oblique(Option<f32>),
}

/// A resolved CSS font for measuring text without a styled element.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextMeasureFont<'a> {
    /// A CSS `<family-name>#` list, including generic families.
    pub family: &'a str,
    /// Used font size in CSS pixels.
    pub size: f32,
    /// Numeric CSS font weight.
    pub weight: f32,
    pub style: TextMeasureFontStyle,
    /// CSS `font-stretch` as a percentage.
    pub stretch: f32,
    /// Extra advance after every character, in CSS pixels.
    pub letter_spacing: f32,
    /// Extra advance after every word separator, in CSS pixels.
    pub word_spacing: f32,
}

/// Shaped advance and primary-font metrics for one line of text.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextMeasurement {
    /// Total advance of the shaped line, including trailing whitespace.
    pub advance: f32,
    /// Ascent of the first available font, in CSS pixels (positive up).
    pub ascent: f32,
    /// Descent of the first available font, in CSS pixels (positive down).
    pub descent: f32,
    /// x-height of the first available font, in CSS pixels.
    pub x_height: f32,
    /// Union of the shaped glyphs' outline boxes, when every glyph's font
    /// exposes TrueType outline bounds and at least one glyph has ink.
    pub ink: Option<TextInkBounds>,
}

/// Glyph outline extent relative to the start of the line (x) and the
/// alphabetic baseline (y), in CSS pixels. All values grow outward.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextInkBounds {
    /// Distance from the line start to the leftmost ink (positive = left).
    pub left: f32,
    /// Distance from the line start to the rightmost ink.
    pub right: f32,
    /// Distance from the baseline to the topmost ink (positive = up).
    pub ascent: f32,
    /// Distance from the baseline to the lowest ink (positive = down).
    pub descent: f32,
}

impl DocumentLayoutServices {
    /// Shapes `text` on one unwrapped line with the given font.
    ///
    /// Whitespace is preserved; callers apply their own collapsing rules.
    pub fn measure_text(&mut self, text: &str, font: &TextMeasureFont<'_>) -> TextMeasurement {
        let style = TextStyle {
            // Platform substitution only rewrites parsed family lists.
            font_family: FontFamily::List(Cow::Owned(
                FontFamilyName::parse_css_list(font.family)
                    .filter_map(Result::ok)
                    .map(FontFamilyName::into_owned)
                    .collect(),
            )),
            font_size: font.size,
            font_width: FontWidth::from_percentage(font.stretch),
            font_style: match font.style {
                TextMeasureFontStyle::Normal => FontStyle::Normal,
                TextMeasureFontStyle::Italic => FontStyle::Italic,
                TextMeasureFontStyle::Oblique(angle) => FontStyle::Oblique(angle),
            },
            font_weight: FontWeight::new(font.weight),
            letter_spacing: font.letter_spacing,
            word_spacing: font.word_spacing,
            ..TextStyle::default()
        };
        self.measure_parley_text(text, style)
    }

    /// Shapes `text` on one unwrapped line with an element's computed font.
    ///
    /// Whitespace is preserved; callers apply their own collapsing rules.
    pub fn measure_text_with_computed_style(
        &mut self,
        text: &str,
        computed: &ComputedValues,
    ) -> TextMeasurement {
        self.measure_parley_text(text, text_style(computed))
    }

    fn measure_parley_text(
        &mut self,
        text: &str,
        mut style: TextStyle<'static, 'static, TextBrush>,
    ) -> TextMeasurement {
        style.white_space_collapse = parley::WhiteSpaceCollapse::Preserve;
        style.text_wrap_mode = TextWrapMode::NoWrap;
        let parley = self.parley_mut();
        let (ascent, descent, x_height) = primary_metrics(parley, &style, text);
        if text.is_empty() {
            return TextMeasurement {
                advance: 0.0,
                ascent,
                descent,
                x_height,
                ink: None,
            };
        }
        let mut styles: Vec<TextStyle<'static, 'static, TextBrush>> = Vec::new();
        let mut runs: Vec<(std::ops::Range<usize>, usize)> = Vec::new();
        let mut push_run = |styles: &mut Vec<_>, range: std::ops::Range<usize>, resolved| {
            let slot = styles
                .iter()
                .position(|existing| existing == &resolved)
                .unwrap_or_else(|| {
                    styles.push(resolved);
                    styles.len() - 1
                });
            match runs.last_mut() {
                Some((last, last_slot)) if *last_slot == slot && last.end == range.start => {
                    last.end = range.end;
                }
                _ => runs.push((range, slot)),
            }
        };
        if parley.requires_character_font_resolution(&style) {
            for (start, character) in text.char_indices() {
                let mut resolved = style.clone();
                parley.resolve_font_families(&mut resolved, Some(character));
                push_run(&mut styles, start..start + character.len_utf8(), resolved);
            }
        } else {
            let mut resolved = style;
            parley.resolve_font_families(&mut resolved, None);
            push_run(&mut styles, 0..text.len(), resolved);
        }
        let mut builder =
            parley
                .layout_context
                .style_run_builder(&mut parley.font_context, text, 1.0, false);
        let indices = styles
            .into_iter()
            .map(|style| builder.push_style(style))
            .collect::<Vec<_>>();
        for (range, slot) in runs {
            builder.push_style_run(indices[slot], range);
        }
        let mut layout = builder.build(text);
        layout.break_all_lines(None);
        TextMeasurement {
            advance: layout.full_width(),
            ascent,
            descent,
            x_height,
            ink: ink_bounds(&layout),
        }
    }
}

fn ink_bounds(layout: &parley::Layout<TextBrush>) -> Option<TextInkBounds> {
    use read_fonts::{TableProvider, types::GlyphId};

    let mut bounds: Option<TextInkBounds> = None;
    for line in layout.lines() {
        let baseline = line.metrics().baseline;
        for item in line.items() {
            let parley::PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                continue;
            };
            let run = glyph_run.run();
            let font_data = &run.font().font;
            let font =
                read_fonts::FontRef::from_index(font_data.data.as_ref(), font_data.index).ok()?;
            let units_per_em = f32::from(font.head().ok()?.units_per_em());
            if units_per_em <= 0.0 {
                return None;
            }
            let scale = run.font_size() / units_per_em;
            let glyf = font.glyf().ok()?;
            let loca = font.loca(None).ok()?;
            for glyph in glyph_run.positioned_glyphs() {
                // An empty outline (a space) has no ink.
                let Some(outline) = loca.get_glyf(GlyphId::new(glyph.id), &glyf).ok()? else {
                    continue;
                };
                let rise = baseline - glyph.y;
                let glyph_bounds = TextInkBounds {
                    left: -(glyph.x + f32::from(outline.x_min()) * scale),
                    right: glyph.x + f32::from(outline.x_max()) * scale,
                    ascent: rise + f32::from(outline.y_max()) * scale,
                    descent: -rise - f32::from(outline.y_min()) * scale,
                };
                bounds = Some(bounds.map_or(glyph_bounds, |current| TextInkBounds {
                    left: current.left.max(glyph_bounds.left),
                    right: current.right.max(glyph_bounds.right),
                    ascent: current.ascent.max(glyph_bounds.ascent),
                    descent: current.descent.max(glyph_bounds.descent),
                }));
            }
        }
    }
    bounds
}

fn primary_metrics(
    parley: &mut ParleyDocumentServices,
    style: &TextStyle<'static, 'static, TextBrush>,
    text: &str,
) -> (f32, f32, f32) {
    let mut resolved = style.clone();
    parley.resolve_font_families(&mut resolved, None);
    parley
        .inline_font_metrics(&resolved, text.chars().find(|c| !c.is_whitespace()))
        .map_or(
            (
                style.font_size * 0.8,
                style.font_size * 0.2,
                style.font_size * 0.5,
            ),
            |metrics| (metrics.ascent, metrics.descent, metrics.x_height),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SystemFontPolicy, WebFontFace, WebFontRegistration};

    const AHEM: &[u8] = include_bytes!("../../tests/fixtures/moli-ahem.ttf");

    fn ahem_services() -> DocumentLayoutServices {
        let mut services =
            DocumentLayoutServices::with_system_font_policy(SystemFontPolicy::Disabled);
        services
            .register_web_font(WebFontRegistration::new(
                "ahem",
                WebFontFace::new("Ahem"),
                AHEM.to_vec(),
            ))
            .unwrap();
        services
    }

    fn font(family: &str, size: f32) -> TextMeasureFont<'_> {
        TextMeasureFont {
            family,
            size,
            weight: 400.0,
            style: TextMeasureFontStyle::Normal,
            stretch: 100.0,
            letter_spacing: 0.0,
            word_spacing: 0.0,
        }
    }

    // The fixture's glyphs advance 0.6em; its ascent is 0.8em, descent 0.2em.
    #[test]
    fn measures_the_shaped_advance_and_primary_font_metrics() {
        let mut services = ahem_services();
        let measured = services.measure_text("XX X ", &font("Ahem, serif", 10.0));
        assert_eq!(measured.advance, 30.0);
        assert_eq!((measured.ascent, measured.descent), (8.0, 2.0));
        let empty = services.measure_text("", &font("Ahem", 20.0));
        assert_eq!(
            (empty.advance, empty.ascent, empty.descent),
            (0.0, 16.0, 4.0)
        );
    }

    #[test]
    fn ink_bounds_cover_the_shaped_outlines() {
        let mut services = ahem_services();
        // The fixture's X outline spans its 0.6em advance from the baseline
        // up to 0.8em. A space has no outline.
        let ink = services
            .measure_text(" XX", &font("Ahem", 10.0))
            .ink
            .expect("Ahem has TrueType outlines");
        assert_eq!(
            ink,
            TextInkBounds {
                left: -6.0,
                right: 18.0,
                ascent: 8.0,
                descent: 0.0
            }
        );
        assert_eq!(services.measure_text("  ", &font("Ahem", 10.0)).ink, None);
    }

    #[test]
    fn letter_spacing_adds_to_every_character() {
        let mut services = ahem_services();
        let spaced = TextMeasureFont {
            letter_spacing: 2.0,
            ..font("Ahem", 10.0)
        };
        assert_eq!(services.measure_text("XXX", &spaced).advance, 24.0);
    }
}
