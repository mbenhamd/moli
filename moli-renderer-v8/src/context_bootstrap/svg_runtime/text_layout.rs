//! Single-line SVG `<text>` layout for geometry queries.
//!
//! Glyph advances and font metrics come from the document's layout font
//! services, so `getBBox`, `getComputedTextLength` and client rects agree with
//! the fonts CSS layout selects. Supported: CSS whitespace collapsing across
//! the text subtree, the first value of `x`/`y`/`dx`/`dy` on `<text>` and
//! `<tspan>`/`<a>` descendants, `text-anchor` per text chunk, and the common
//! `dominant-baseline` keywords. Per-glyph position lists, `rotate`,
//! `textLength`, `<textPath>` and vertical writing modes are not modeled.

use moli_svg::{self as svg_geometry, SvgGeometryBox, SvgGeometryElement, SvgMatrixComponents};

use crate::document_runtime::DomHandle;
use crate::native_bridge::JsContextHost;
use crate::native_bridge::element::computed_style_property_for_handle;

struct SvgTextFragment {
    /// Elements from the text node's parent up to and including the root.
    owners: Vec<DomHandle>,
    text: String,
    x: f64,
    baseline: f64,
    width: f64,
    ascent: f64,
    descent: f64,
}

struct SvgTextChunk {
    start: usize,
    anchor: String,
}

struct SvgTextLayoutBuilder<'a> {
    runtime: &'a JsContextHost,
    pen_x: f64,
    pen_y: f64,
    last_was_space: bool,
    fragments: Vec<SvgTextFragment>,
    chunks: Vec<SvgTextChunk>,
    pending_chunk: Option<String>,
}

pub(super) fn svg_text_root(runtime: &JsContextHost, handle: DomHandle) -> Option<DomHandle> {
    let mut current = Some(handle);
    while let Some(candidate) = current {
        let node = runtime.dom_host().node(candidate)?;
        if node.local_name() == Some("text") {
            return Some(candidate);
        }
        current = node.parent_node_id();
    }
    None
}

/// Bounding box of the glyph cells that `handle` (a `<text>` or one of its
/// content elements) renders, in the coordinate system of `transform`.
pub(super) fn svg_text_bounding_box(
    runtime: &JsContextHost,
    handle: DomHandle,
    transform: SvgMatrixComponents,
) -> Option<SvgGeometryBox> {
    let root = svg_text_root(runtime, handle)?;
    layout_svg_text(runtime, root)
        .into_iter()
        .filter(|fragment| fragment.owners.contains(&handle))
        .filter_map(|fragment| {
            let geometry = SvgGeometryElement::Rect {
                x: fragment.x,
                y: fragment.baseline - fragment.ascent,
                width: fragment.width,
                height: fragment.ascent + fragment.descent,
                rx: 0.0,
                ry: 0.0,
            };
            svg_geometry::bounding_box_for_transformed_element(&geometry, transform)
        })
        .reduce(SvgGeometryBox::union)
}

/// Sum of the advances of the characters `handle` renders.
pub(super) fn svg_computed_text_length(runtime: &JsContextHost, handle: DomHandle) -> f64 {
    let Some(root) = svg_text_root(runtime, handle) else {
        return 0.0;
    };
    layout_svg_text(runtime, root)
        .iter()
        .filter(|fragment| fragment.owners.contains(&handle))
        .map(|fragment| fragment.width)
        .sum()
}

fn layout_svg_text(runtime: &JsContextHost, root: DomHandle) -> Vec<SvgTextFragment> {
    if !runtime
        .dom_host()
        .node(root)
        .is_some_and(|node| node.is_connected())
    {
        return Vec::new();
    }
    let mut builder = SvgTextLayoutBuilder {
        runtime,
        pen_x: 0.0,
        pen_y: 0.0,
        last_was_space: true,
        fragments: Vec::new(),
        chunks: Vec::new(),
        pending_chunk: Some(svg_text_property(runtime, root, "text-anchor")),
    };
    let mut owners = Vec::new();
    builder.visit(root, &mut owners);
    builder.finish()
}

impl SvgTextLayoutBuilder<'_> {
    fn visit(&mut self, element: DomHandle, owners: &mut Vec<DomHandle>) {
        if computed_style_property_for_handle(self.runtime, element, "display")
            .trim()
            .eq_ignore_ascii_case("none")
        {
            return;
        }
        let x = first_length(self.runtime, element, "x");
        let y = first_length(self.runtime, element, "y");
        if let Some(x) = x {
            self.pen_x = x;
        }
        if let Some(y) = y {
            self.pen_y = y;
        }
        if x.is_some() || y.is_some() {
            self.pending_chunk = Some(svg_text_property(self.runtime, element, "text-anchor"));
        }
        self.pen_x += first_length(self.runtime, element, "dx").unwrap_or(0.0);
        self.pen_y += first_length(self.runtime, element, "dy").unwrap_or(0.0);

        owners.insert(0, element);
        for child in self.runtime.dom_host().child_handles(element) {
            let Some(node) = self.runtime.dom_host().node(child) else {
                continue;
            };
            if node.is_text() {
                if let Some(data) = node.data_value() {
                    let data = data.to_owned();
                    self.append_text(&data, owners);
                }
                continue;
            }
            if node.namespace() == Some(crate::native_bridge::document::SVG_NS)
                && matches!(node.local_name(), Some("tspan" | "a"))
            {
                self.visit(child, owners);
            }
        }
        owners.remove(0);
    }

    fn append_text(&mut self, data: &str, owners: &[DomHandle]) {
        let mut text = String::with_capacity(data.len());
        for character in data.chars() {
            if matches!(character, ' ' | '\t' | '\n' | '\r') {
                if !self.last_was_space {
                    text.push(' ');
                    self.last_was_space = true;
                }
            } else {
                text.push(character);
                self.last_was_space = false;
            }
        }
        if text.is_empty() {
            return;
        }
        if let Some(anchor) = self.pending_chunk.take() {
            self.chunks.push(SvgTextChunk {
                start: self.fragments.len(),
                anchor,
            });
        }
        let owner = owners[0];
        let metrics = measure(self.runtime, owner, &text);
        let shift = baseline_shift(self.runtime, owner, &metrics);
        let fragment = SvgTextFragment {
            owners: owners.to_vec(),
            text,
            x: self.pen_x,
            baseline: self.pen_y + shift,
            width: f64::from(metrics.advance),
            ascent: f64::from(metrics.ascent).round(),
            descent: chromium_descent(f64::from(metrics.descent)),
        };
        self.pen_x += fragment.width;
        self.fragments.push(fragment);
    }

    fn finish(mut self) -> Vec<SvgTextFragment> {
        // The collapsed line drops its trailing space.
        if let Some(last) = self.fragments.last_mut()
            && last.text.ends_with(' ')
        {
            last.text.pop();
            let owner = last.owners[0];
            last.width = f64::from(measure(self.runtime, owner, &last.text).advance);
        }
        for (index, chunk) in self.chunks.iter().enumerate() {
            let end = self
                .chunks
                .get(index + 1)
                .map_or(self.fragments.len(), |next| next.start);
            let fragments = &mut self.fragments[chunk.start..end];
            let advance: f64 = fragments.iter().map(|fragment| fragment.width).sum();
            let offset = match chunk.anchor.as_str() {
                "middle" => -advance / 2.0,
                "end" => -advance,
                _ => 0.0,
            };
            for fragment in fragments {
                fragment.x += offset;
            }
        }
        self.fragments
    }
}

/// Blink rounds the ascent and rounds the descent up on Linux so glyph
/// descenders are never clipped; SVG text cells use these integer metrics.
fn chromium_descent(descent: f64) -> f64 {
    let rounded = descent.round();
    if rounded < descent {
        rounded + 1.0
    } else {
        rounded
    }
}

fn measure(runtime: &JsContextHost, owner: DomHandle, text: &str) -> moli_layout::TextMeasurement {
    let computed =
        crate::native_bridge::element::ComputedStyleRead::new(runtime, owner).computed_values();
    let Some(computed) = computed else {
        let size = 16.0_f32;
        return moli_layout::TextMeasurement {
            advance: text.chars().count() as f32 * size * 0.6,
            ascent: size * 0.8,
            descent: size * 0.2,
            x_height: size * 0.5,
        };
    };
    let document = runtime
        .layout_document_for_source(owner)
        .unwrap_or_else(|| runtime.document_handle());
    runtime.with_font_services(document, |services| {
        services.measure_text_with_computed_style(text, &computed)
    })
}

fn baseline_shift(
    runtime: &JsContextHost,
    owner: DomHandle,
    metrics: &moli_layout::TextMeasurement,
) -> f64 {
    let ascent = f64::from(metrics.ascent).round();
    let descent = chromium_descent(f64::from(metrics.descent));
    match svg_text_property(runtime, owner, "dominant-baseline").as_str() {
        "central" => (ascent - descent) / 2.0,
        "middle" => f64::from(metrics.x_height) / 2.0,
        "hanging" => ascent * 0.8,
        "mathematical" => ascent / 2.0,
        "text-before-edge" | "text-top" => ascent,
        "text-after-edge" | "text-bottom" | "ideographic" => -descent,
        _ => 0.0,
    }
}

/// Computed value of an inherited SVG text property, falling back to the
/// nearest presentation attribute when the style engine has no value.
fn svg_text_property(runtime: &JsContextHost, element: DomHandle, property: &str) -> String {
    let computed = computed_style_property_for_handle(runtime, element, property);
    let computed = computed.trim();
    if !computed.is_empty() {
        return computed.to_ascii_lowercase();
    }
    let mut current = Some(element);
    while let Some(candidate) = current {
        if let Some(value) = runtime.dom_host().get_attribute(candidate, property) {
            return value.trim().to_ascii_lowercase();
        }
        let Some(node) = runtime.dom_host().node(candidate) else {
            break;
        };
        if node.local_name() == Some("text") {
            break;
        }
        current = node.parent_node_id();
    }
    String::new()
}

fn first_length(runtime: &JsContextHost, element: DomHandle, attribute: &str) -> Option<f64> {
    let raw = runtime.dom_host().get_attribute(element, attribute)?;
    let first = raw
        .split(|character: char| character == ',' || character.is_ascii_whitespace())
        .find(|token| !token.is_empty())?;
    super::builders::svg_attribute_length_to_user_units(runtime, element, attribute, first)
}
