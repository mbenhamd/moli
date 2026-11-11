//! Hit testing of SVG content below an outermost `<svg>` box.
//!
//! CSS layout produces a box only for the outermost `<svg>`, so point queries
//! stop there. This module resolves the content under the point from SVG
//! geometry, but only for an `<svg>` whose whole subtree the geometry model
//! covers exactly: an allowlist of elements and computed properties checked
//! once per query before any shape is tested. Any other subtree is
//! [`Coarse`]: the caller then keeps the `<svg>` box answer, as before SVG
//! content was resolved at all.

use moli_svg::{
    SvgGeometryElement, SvgGeometryPoint, SvgLengthUnit, SvgMatrixComponents, SvgShapeHitArea,
    SvgStrokeHitArea, SvgStrokeLinecap,
};
use style::properties::ComputedValues;
use style::values::computed::{Length, LengthPercentage};

use super::builders::{
    parse_svg_view_box_value, svg_bbox_for_handle, svg_fill_rule_from_computed,
    svg_local_name_is_non_rendered_container, svg_transform_for_handle,
};
use super::screen::{svg_nested_viewport_transform, svg_view_box_transform};
use crate::document_runtime::DomHandle;
use crate::native_bridge::JsContextHost;
use crate::native_bridge::document::SVG_NS;

/// The geometry model cannot answer exactly; keep the outer `<svg>` result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Coarse;

/// SVG content of the outermost `<svg>` `root` under `point`, foremost
/// (reverse paint order) first. With `first_only` the walk stops at the
/// foremost hit.
///
/// `content_box` is `root`'s CSS content box `[x, y, width, height]` and
/// `point` the query point, both in the coordinate space of `root`'s hit
/// fragment, so CSS transforms on the outer box are already undone.
///
/// [`Coarse`] unless every element below `root` is on the allowlist (see
/// [`eligible_children`]). Candidates honour `pointer-events` (`auto` acts
/// as `visiblePainted`), `visibility`, `display` and nested `<svg>`
/// viewport clipping. Shapes test their exact fill and their stroke (width,
/// caps, dashing, `pathLength`). Painted text tests its SVG bounding box.
pub(crate) fn svg_content_hits_at_point(
    runtime: &JsContextHost,
    root: DomHandle,
    content_box: [f64; 4],
    point: [f64; 2],
    first_only: bool,
) -> Result<Vec<DomHandle>, Coarse> {
    let [x, y, width, height] = content_box;
    // The outer `<svg>` clips its content to the content box by default, and
    // `overflow: visible` content past it is not modelled either.
    if !(point[0] >= x && point[0] <= x + width && point[1] >= y && point[1] <= y + height) {
        return Err(Coarse);
    }
    let Some(root_style) = computed_values(runtime, root) else {
        return Ok(Vec::new());
    };
    if has_unmodelled_effects(runtime, root, &root_style)
        || document_declares_svg_pointer_events(runtime, root)
    {
        return Err(Coarse);
    }
    let view_box = runtime
        .dom_host()
        .get_attribute(root, "viewBox")
        .as_deref()
        .and_then(parse_svg_view_box_value)
        .filter(|[_, _, width, height]| *width > 0.0 && *height > 0.0);
    let scope = Scope {
        ctm: SvgMatrixComponents::translate(x, y)
            .multiply(svg_view_box_transform(runtime, root, width, height)),
        viewport: Some(view_box.map_or((width, height), |[_, _, w, h]| (w, h))),
    };
    eligible_children(runtime, root, scope.viewport, Content::Graphics)?;
    let mut walk = Walk {
        runtime,
        point,
        first_only,
        hits: Vec::new(),
    };
    walk.children(root, scope)?;
    Ok(walk.hits)
}

/// Which children an element may have.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Content {
    Graphics,
    /// Inside `<text>`: only `<tspan>` and descriptive elements.
    Text,
}

/// The allowlist pre-pass: `Ok` when the geometry model covers every child
/// of `parent` and their subtrees exactly, given the nearest `viewport`.
///
/// Rendered content may use `g`, nested `svg` (see [`nested_svg`]), the
/// basic shapes, `path`, `text` and `tspan`; `defs` may hold only paint
/// servers. Paint servers, `title` and `desc` are never hit and their
/// content is not inspected. Every other element (`use`, `image`,
/// `foreignObject`, `switch`, `symbol`, `a`, `textPath`, animation
/// elements, ...) is [`Coarse`], as is any rendered element with an effect,
/// a transform or a length the model does not reproduce. Stops at the first
/// such node.
fn eligible_children(
    runtime: &JsContextHost,
    parent: DomHandle,
    viewport: Option<(f64, f64)>,
    content: Content,
) -> Result<(), Coarse> {
    for child in runtime.dom_host().child_handles(parent).collect::<Vec<_>>() {
        let Some(node) = runtime.dom_host().node(child) else {
            continue;
        };
        // Text, comments and elements outside the SVG namespace are not
        // rendered here.
        if node.namespace() != Some(SVG_NS) {
            continue;
        }
        let Some(local_name) = node.local_name() else {
            continue;
        };
        match (content, local_name) {
            (_, "title" | "desc") => {}
            (Content::Graphics, "linearGradient" | "radialGradient" | "pattern") => {}
            (Content::Graphics, "defs") => {
                eligible_definitions(runtime, child)?;
            }
            (
                Content::Graphics,
                "g" | "svg" | "circle" | "ellipse" | "line" | "path" | "polygon" | "polyline"
                | "rect" | "text",
            )
            | (Content::Text, "tspan") => {
                eligible_element(runtime, child, local_name, viewport)?;
            }
            _ => return Err(Coarse),
        }
    }
    Ok(())
}

/// `<defs>` content is never rendered in place; only paint servers, which
/// fills and strokes reference, are on the allowlist.
fn eligible_definitions(runtime: &JsContextHost, defs: DomHandle) -> Result<(), Coarse> {
    for child in runtime.dom_host().child_handles(defs).collect::<Vec<_>>() {
        let Some(node) = runtime.dom_host().node(child) else {
            continue;
        };
        if node.namespace() != Some(SVG_NS) {
            continue;
        }
        if !matches!(
            node.local_name(),
            Some("linearGradient" | "radialGradient" | "pattern" | "title" | "desc")
        ) {
            return Err(Coarse);
        }
    }
    Ok(())
}

fn eligible_element(
    runtime: &JsContextHost,
    handle: DomHandle,
    local_name: &str,
    viewport: Option<(f64, f64)>,
) -> Result<(), Coarse> {
    let Some(computed) = computed_values(runtime, handle) else {
        return Ok(());
    };
    // Not rendered, so neither it nor its subtree is hit.
    if computed.clone_display().is_none() {
        return Ok(());
    }
    if has_unmodelled_effects(runtime, handle, &computed)
        || !transform_is_the_attribute(runtime, handle, &computed)
    {
        return Err(Coarse);
    }
    let mut viewport = viewport;
    let lengths = Lengths(viewport);
    match local_name {
        "svg" => viewport = nested_svg(runtime, handle, &computed)?.viewport,
        "circle" | "ellipse" | "line" | "path" | "polygon" | "polyline" | "rect" => {
            paint_is_painted(runtime, handle, &computed.clone_fill())?;
            paint_is_painted(runtime, handle, &computed.clone_stroke())?;
            if local_name == "path" && !path_data_is_the_attribute(runtime, handle, &computed) {
                return Err(Coarse);
            }
            geometry_element(runtime, handle, local_name, &computed, lengths)?;
            stroke_width(&computed, lengths)?;
            stroke_dash_offset(&computed, lengths)?;
            if !stroke_dashes(&computed, lengths)?.is_empty() {
                path_length(runtime, handle)?;
            }
        }
        "text" | "tspan" => {
            paint_is_painted(runtime, handle, &computed.clone_fill())?;
            paint_is_painted(runtime, handle, &computed.clone_stroke())?;
        }
        _ => {}
    }
    let content = if matches!(local_name, "text" | "tspan") {
        Content::Text
    } else {
        Content::Graphics
    };
    eligible_children(runtime, handle, viewport, content)
}

#[derive(Clone, Copy)]
struct Scope {
    ctm: SvgMatrixComponents,
    /// The nearest viewport's size in user units, when it is known.
    viewport: Option<(f64, f64)>,
}

struct Walk<'a> {
    runtime: &'a JsContextHost,
    point: [f64; 2],
    first_only: bool,
    hits: Vec<DomHandle>,
}

enum ShapeHit {
    Hit,
    Miss,
}

impl Walk<'_> {
    /// Walks `parent`'s children in reverse paint order: later siblings, and
    /// a subtree's descendants above the subtree root, come first. Returns
    /// whether the walk is done.
    fn children(&mut self, parent: DomHandle, scope: Scope) -> Result<bool, Coarse> {
        let runtime = self.runtime;
        let children = runtime.dom_host().child_handles(parent).collect::<Vec<_>>();
        for child in children.into_iter().rev() {
            let Some(node) = runtime.dom_host().node(child) else {
                continue;
            };
            if node.namespace() != Some(SVG_NS) {
                continue;
            }
            let Some(local_name) = node.local_name() else {
                continue;
            };
            if svg_local_name_is_non_rendered_container(local_name) {
                continue;
            }
            let Some(computed) = computed_values(runtime, child) else {
                continue;
            };
            if computed.clone_display().is_none() {
                continue;
            }
            let mut child_scope = scope;
            if local_name == "svg" {
                let nested = nested_svg(runtime, child, &computed)?;
                if nested.clips_point(scope.ctm, self.point) {
                    continue;
                }
                child_scope.viewport = nested.viewport;
                child_scope.ctm = scope
                    .ctm
                    .multiply(svg_nested_viewport_transform(runtime, child));
            } else {
                child_scope.ctm = scope.ctm.multiply(svg_transform_for_handle(runtime, child));
            }
            if self.children(child, child_scope)? {
                return Ok(true);
            }
            if let ShapeHit::Hit = self.element(child, local_name, &computed, child_scope)? {
                self.hits.push(child);
                if self.first_only {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn element(
        &self,
        handle: DomHandle,
        local_name: &str,
        computed: &ComputedValues,
        scope: Scope,
    ) -> Result<ShapeHit, Coarse> {
        let runtime = self.runtime;
        // Stylo's Servo build parses only `auto | none`; SVG treats `auto` as
        // `visiblePainted`.
        if computed.clone_pointer_events() == style::computed_values::pointer_events::T::None
            || computed.clone_visibility() != style::computed_values::visibility::T::Visible
            || !scope.ctm.is_invertible()
        {
            return Ok(ShapeHit::Miss);
        }
        let inverse = scope.ctm.inverse();
        let [x, y] = self.point;
        let local = SvgGeometryPoint::new(
            inverse.a * x + inverse.c * y + inverse.e,
            inverse.b * x + inverse.d * y + inverse.f,
        );
        let hit = |inside: bool| {
            if inside {
                ShapeHit::Hit
            } else {
                ShapeHit::Miss
            }
        };
        match local_name {
            "circle" | "ellipse" | "line" | "path" | "polygon" | "polyline" | "rect" => {
                let fill = paint_is_painted(runtime, handle, &computed.clone_fill())?;
                let stroke = paint_is_painted(runtime, handle, &computed.clone_stroke())?;
                if !fill && !stroke {
                    return Ok(ShapeHit::Miss);
                }
                let lengths = Lengths(scope.viewport);
                let Some(geometry) =
                    geometry_element(runtime, handle, local_name, computed, lengths)?
                else {
                    return Ok(ShapeHit::Miss);
                };
                let dashes;
                let stroke = if stroke {
                    dashes = stroke_dashes(computed, lengths)?;
                    Some(SvgStrokeHitArea {
                        width: stroke_width(computed, lengths)?,
                        linecap: match computed.clone_stroke_linecap() {
                            style::computed_values::stroke_linecap::T::Butt => {
                                SvgStrokeLinecap::Butt
                            }
                            style::computed_values::stroke_linecap::T::Round => {
                                SvgStrokeLinecap::Round
                            }
                            style::computed_values::stroke_linecap::T::Square => {
                                SvgStrokeLinecap::Square
                            }
                        },
                        dashes: &dashes,
                        dash_offset: stroke_dash_offset(computed, lengths)?,
                        path_length: if dashes.is_empty() {
                            None
                        } else {
                            path_length(runtime, handle)?
                        },
                    })
                } else {
                    None
                };
                moli_svg::is_point_in_shape_hit_area(
                    &geometry,
                    local,
                    SvgShapeHitArea {
                        fill: fill.then(|| svg_fill_rule_from_computed(computed)),
                        stroke,
                    },
                )
                .map(hit)
                .map_err(|moli_svg::SvgHitTestUnbounded| Coarse)
            }
            "text" | "tspan" => {
                if super::text_layout::svg_text_root(runtime, handle).is_none() {
                    return Ok(ShapeHit::Miss);
                }
                let painted = paint_is_painted(runtime, handle, &computed.clone_fill())?
                    || paint_is_painted(runtime, handle, &computed.clone_stroke())?;
                Ok(hit(painted && bbox_contains(runtime, handle, local)))
            }
            _ => Ok(ShapeHit::Miss),
        }
    }
}

fn computed_values(
    runtime: &JsContextHost,
    handle: DomHandle,
) -> Option<style::servo_arc::Arc<ComputedValues>> {
    crate::native_bridge::element::ComputedStyleRead::new(runtime, handle).computed_values()
}

fn bbox_contains(runtime: &JsContextHost, handle: DomHandle, local: SvgGeometryPoint) -> bool {
    svg_bbox_for_handle(
        runtime,
        handle,
        SvgMatrixComponents::identity(),
        false,
        &mut Vec::new(),
    )
    .is_some_and(|bounds| {
        local.x >= bounds.x
            && local.y >= bounds.y
            && local.x <= bounds.x + bounds.width
            && local.y <= bounds.y + bounds.height
    })
}

/// Clipping, masking, filters, markers and `vector-effect` change what is
/// painted or hit in ways the model does not reproduce.
fn has_unmodelled_effects(
    runtime: &JsContextHost,
    handle: DomHandle,
    computed: &ComputedValues,
) -> bool {
    use style::values::generics::url::GenericUrlOrNone;
    let marker =
        |value: style::values::computed::url::UrlOrNone| !matches!(value, GenericUrlOrNone::None);
    !matches!(
        computed.clone_clip_path(),
        style::values::generics::basic_shape::ClipPath::None
    ) || !computed.clone_filter().0.is_empty()
        || marker(computed.clone_marker_start())
        || marker(computed.clone_marker_mid())
        || marker(computed.clone_marker_end())
        || computed.clone_vector_effect() != style::values::computed::VectorEffect::none()
        || [
            "clip-path",
            "mask",
            "filter",
            "marker",
            "marker-start",
            "marker-mid",
            "marker-end",
        ]
        .into_iter()
        .any(|attribute| {
            runtime
                .dom_host()
                .get_attribute(handle, attribute)
                .is_some()
        })
        || element_declares_svg_pointer_events(runtime, handle)
}

/// `pointer-events` keywords other than `auto` and `none` are SVG-only, and
/// the style engine drops them, so the element computes `auto` (treated as
/// `visiblePainted`). `visiblePainted` itself behaves exactly like that.
/// Any other one (`all`, `fill`, `stroke`, ...) changes what is hit and is
/// not modelled.
fn is_unmodelled_pointer_events_keyword(keyword: &str) -> bool {
    [
        "visiblefill",
        "visiblestroke",
        "visible",
        "painted",
        "fill",
        "stroke",
        "all",
        "bounding-box",
    ]
    .iter()
    .any(|candidate| keyword.eq_ignore_ascii_case(candidate))
}

/// Whether CSS `text` declares `pointer-events` with an unmodelled keyword.
/// A textual scan: it may also match inside a string, which only makes a
/// query coarse.
pub(crate) fn css_declares_svg_pointer_events(text: &str) -> bool {
    const NAME: &str = "pointer-events";
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(offset) = bytes[from..]
        .windows(NAME.len())
        .position(|window| window.eq_ignore_ascii_case(NAME.as_bytes()))
    {
        from += offset + NAME.len();
        let index = skip_whitespace_and_comments(bytes, from);
        if bytes.get(index) == Some(&b':')
            && value_starts_with_unmodelled_keyword(text, index + 1)
        {
            return true;
        }
    }
    false
}

/// Whether a CSSOM write of `value` to the property `name` declares an
/// unmodelled `pointer-events` keyword.
pub(crate) fn property_declares_svg_pointer_events(name: &str, value: &str) -> bool {
    name.eq_ignore_ascii_case("pointer-events") && value_starts_with_unmodelled_keyword(value, 0)
}

/// Whether the CSS value at byte `index` of `text` starts with an
/// unmodelled `pointer-events` keyword.
fn value_starts_with_unmodelled_keyword(text: &str, index: usize) -> bool {
    let bytes = text.as_bytes();
    let start = skip_whitespace_and_comments(bytes, index);
    let end = bytes[start..]
        .iter()
        .position(|byte| !(byte.is_ascii_alphanumeric() || *byte == b'-'))
        .map_or(bytes.len(), |length| start + length);
    is_unmodelled_pointer_events_keyword(&text[start..end])
}

/// The first byte at or after `index` that is neither whitespace nor part
/// of a `/* ... */` comment.
fn skip_whitespace_and_comments(bytes: &[u8], mut index: usize) -> usize {
    loop {
        match bytes.get(index) {
            Some(byte) if byte.is_ascii_whitespace() => index += 1,
            Some(b'/') if bytes.get(index + 1) == Some(&b'*') => {
                index = bytes[index + 2..]
                    .windows(2)
                    .position(|window| window == b"*/")
                    .map_or(bytes.len(), |offset| index + 2 + offset + 2);
            }
            _ => return index,
        }
    }
}

fn element_declares_svg_pointer_events(runtime: &JsContextHost, handle: DomHandle) -> bool {
    let host = runtime.dom_host();
    host.get_attribute(handle, "pointer-events")
        .is_some_and(|value| is_unmodelled_pointer_events_keyword(value.trim()))
        || host
            .get_attribute(handle, "style")
            .is_some_and(|style| css_declares_svg_pointer_events(&style))
}

/// Notes a CSSOM write to the inline style of element `handle`; `declares` is
/// whether the written text declares an unmodelled `pointer-events`
/// keyword. The write reserializes the `style` attribute from what the
/// style engine kept, dropping such a keyword from the written text and
/// from the current attribute alike, so the runtime remembers it instead.
pub(crate) fn note_inline_style_write(runtime: &JsContextHost, handle: DomHandle, declares: bool) {
    if declares
        || runtime
            .dom_host()
            .get_attribute(handle, "style")
            .is_some_and(|style| css_declares_svg_pointer_events(&style))
    {
        runtime.note_cssom_svg_pointer_events();
    }
}

/// Notes a CSSOM write in the page of `scope` to a declaration block or
/// stylesheet that keeps only what the style engine parsed; `declares` is
/// whether the written text declares an unmodelled `pointer-events`
/// keyword.
pub(crate) fn note_cssom_write(scope: &mut v8::PinScope<'_, '_>, declares: bool) {
    if !declares {
        return;
    }
    if let Some(host_ptr) = crate::util::context_host_ptr_from_global_bridge(scope) {
        unsafe { &*host_ptr }.note_cssom_svg_pointer_events();
    }
}

/// An author stylesheet rule with an unmodelled `pointer-events` keyword is
/// dropped before any computed value can show it, so its presence in the
/// document, or in any CSSOM write the text of which is gone, makes every
/// query coarse.
fn document_declares_svg_pointer_events(runtime: &JsContextHost, root: DomHandle) -> bool {
    if runtime.cssom_declared_svg_pointer_events() {
        return true;
    }
    let document = runtime
        .dom_host()
        .owner_document_handle(root)
        .unwrap_or_else(|| runtime.document_handle());
    runtime.any_author_stylesheet_text(document, css_declares_svg_pointer_events)
}

/// Whether the element is placed by its `transform` attribute alone, which
/// the walk applies.
///
/// The attribute is a presentation hint, so the computed `transform`
/// carries it unless author CSS overrides it (`transform: none` included).
/// The two must therefore agree, as matrices. `translate`, `rotate` and
/// `scale` have no attribute and must be `none`. A non-identity transform
/// also needs an unauthored or zero `transform-origin`: SVG content
/// transforms about `0 0`, which this stylesheet set leaves at the CSS
/// initial value. An author `transform-origin: 50% 50%` is
/// indistinguishable from that and is read as `0 0`.
fn transform_is_the_attribute(
    runtime: &JsContextHost,
    handle: DomHandle,
    computed: &ComputedValues,
) -> bool {
    use style::values::generics::transform::{Rotate, Scale, Translate};
    if !matches!(computed.clone_rotate(), Rotate::None)
        || !matches!(computed.clone_scale(), Scale::None)
        || !matches!(computed.clone_translate(), Translate::None)
    {
        return false;
    }
    let attribute = svg_transform_for_handle(runtime, handle);
    let transform = computed.clone_transform();
    let css = if transform.0.is_empty() {
        SvgMatrixComponents::identity()
    } else {
        match transform.to_transform_3d_matrix_f64(None) {
            Ok((matrix, false)) => SvgMatrixComponents {
                a: matrix.m11,
                b: matrix.m12,
                c: matrix.m21,
                d: matrix.m22,
                e: matrix.m41,
                f: matrix.m42,
            },
            _ => return false,
        }
    };
    if !matrices_agree(css, attribute) {
        return false;
    }
    if matrices_agree(attribute, SvgMatrixComponents::identity()) {
        return true;
    }
    let origin = computed.clone_transform_origin();
    origin == style::values::computed::TransformOrigin::initial_value()
        || (is_zero_length(&origin.horizontal)
            && is_zero_length(&origin.vertical)
            && origin.depth.px() == 0.0)
}

fn is_zero_length(value: &LengthPercentage) -> bool {
    value.to_length().is_some_and(|length| length.px() == 0.0)
}

/// Equal up to the single precision a computed transform stores.
fn matrices_agree(left: SvgMatrixComponents, right: SvgMatrixComponents) -> bool {
    [
        (left.a, right.a),
        (left.b, right.b),
        (left.c, right.c),
        (left.d, right.d),
        (left.e, right.e),
        (left.f, right.f),
    ]
    .into_iter()
    .all(|(left, right)| (left - right).abs() <= 1e-5 * (1.0 + left.abs().max(right.abs())))
}

/// Whether the computed `d` is the `d` attribute's path. The walk reads the
/// computed path, so CSS `d` that differs from the attribute, or `d: none`
/// over a non-empty attribute, is not modelled.
fn path_data_is_the_attribute(
    runtime: &JsContextHost,
    handle: DomHandle,
    computed: &ComputedValues,
) -> bool {
    use style::values::specified::svg_path::SVGPathData;
    let attribute = runtime
        .dom_host()
        .get_attribute(handle, "d")
        .map(|raw| SVGPathData::parse_bytes(raw.as_bytes()));
    match computed.clone_d() {
        style::values::computed::DProperty::Path(path) => {
            attribute.is_some_and(|(parsed, valid)| {
                valid && parsed.normalize(false) == path.normalize(false)
            })
        }
        style::values::computed::DProperty::None => {
            attribute.is_none_or(|(parsed, _)| parsed.commands().is_empty())
        }
    }
}

/// A nested `<svg>` the model places and clips exactly.
struct NestedSvg {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    /// The viewport size its content resolves percentages against.
    viewport: Option<(f64, f64)>,
}

/// The viewport a nested `<svg>` establishes, from its `x`, `y`, `width`,
/// `height` and `viewBox` in the parent's user space.
///
/// Exact only with `width` and `height` given, every position in plain
/// numbers or `px` (what `svg_nested_viewport_transform` reads), no
/// `transform` and no CSS geometry, and when it clips: SVG clips a nested
/// viewport by default, but this stylesheet set computes `overflow:
/// visible` for it, which is then indistinguishable from an author's.
fn nested_svg(
    runtime: &JsContextHost,
    svg: DomHandle,
    computed: &ComputedValues,
) -> Result<NestedSvg, Coarse> {
    use style::values::generics::length::GenericSize;
    let length = |attribute: &str| -> Result<Option<f64>, Coarse> {
        let Some(raw) = runtime.dom_host().get_attribute(svg, attribute) else {
            return Ok(None);
        };
        match moli_svg::parse_length(&raw) {
            Some(parsed)
                if matches!(parsed.unit, SvgLengthUnit::Number | SvgLengthUnit::Px)
                    && parsed.value.is_finite()
                    && parsed.value >= 0.0 =>
            {
                Ok(Some(parsed.value))
            }
            _ => Err(Coarse),
        }
    };
    let (x, y) = (length("x")?, length("y")?);
    let (Some(width), Some(height)) = (length("width")?, length("height")?) else {
        return Err(Coarse);
    };
    if runtime.dom_host().get_attribute(svg, "transform").is_some()
        || !matches!(computed.clone_width(), GenericSize::Auto)
        || !matches!(computed.clone_height(), GenericSize::Auto)
        || !is_zero_length(&computed.clone_x())
        || !is_zero_length(&computed.clone_y())
        || computed.clone_overflow_x() == style::computed_values::overflow_x::T::Visible
        || computed.clone_overflow_y() == style::computed_values::overflow_y::T::Visible
    {
        return Err(Coarse);
    }
    let view_box = runtime
        .dom_host()
        .get_attribute(svg, "viewBox")
        .as_deref()
        .and_then(parse_svg_view_box_value)
        .filter(|[_, _, width, height]| *width > 0.0 && *height > 0.0);
    Ok(NestedSvg {
        x: x.unwrap_or(0.0),
        y: y.unwrap_or(0.0),
        width,
        height,
        viewport: Some(view_box.map_or((width, height), |[_, _, w, h]| (w, h))),
    })
}

impl NestedSvg {
    /// Whether the viewport clips its content away from `point`, given the
    /// parent's CTM.
    fn clips_point(&self, parent_ctm: SvgMatrixComponents, point: [f64; 2]) -> bool {
        if !parent_ctm.is_invertible() {
            return true;
        }
        let inverse = parent_ctm.inverse();
        let [px, py] = point;
        let local_x = inverse.a * px + inverse.c * py + inverse.e;
        let local_y = inverse.b * px + inverse.d * py + inverse.f;
        local_x < self.x
            || local_y < self.y
            || local_x > self.x + self.width
            || local_y > self.y + self.height
    }
}

/// Whether a `fill` or `stroke` paints. An unresolved same-document paint
/// server uses its fallback, and `none` without one.
fn paint_is_painted(
    runtime: &JsContextHost,
    handle: DomHandle,
    paint: &style::values::computed::SVGPaint,
) -> Result<bool, Coarse> {
    use style::values::generics::svg::{SVGPaintFallback, SVGPaintKind};
    match &paint.kind {
        SVGPaintKind::None => Ok(false),
        SVGPaintKind::Color(_) => Ok(true),
        SVGPaintKind::PaintServer(url) => {
            if paint_server_resolves(runtime, handle, url)? {
                Ok(true)
            } else {
                Ok(matches!(paint.fallback, SVGPaintFallback::Color(_)))
            }
        }
        SVGPaintKind::ContextFill | SVGPaintKind::ContextStroke => Err(Coarse),
    }
}

fn paint_server_resolves(
    runtime: &JsContextHost,
    handle: DomHandle,
    url: &style::values::computed::url::ComputedUrl,
) -> Result<bool, Coarse> {
    let Some(url) = url.url() else {
        return Ok(false);
    };
    let dom = runtime.dom_host();
    let document = dom.owner_document_handle(handle).ok_or(Coarse)?;
    let document_url = runtime.document_url_for_handle(document);
    fn same_document(url: &str) -> &str {
        url.split('#').next().unwrap_or(url)
    }
    let Some((_, fragment)) = url.as_str().split_once('#') else {
        return Err(Coarse);
    };
    if same_document(url.as_str()) != same_document(document_url.as_str()) {
        return Err(Coarse);
    }
    let id = percent_encoding::percent_decode_str(fragment).decode_utf8_lossy();
    let target = dom
        .root_node_handle(handle)
        .and_then(|root| dom.element_handle_by_id_in_subtree(root, &id))
        .and_then(|target| dom.node(target));
    Ok(target.is_some_and(|node| {
        node.namespace() == Some(SVG_NS)
            && matches!(
                node.local_name(),
                Some("linearGradient" | "radialGradient" | "pattern")
            )
    }))
}

/// Resolves SVG lengths and percentages against the nearest viewport.
#[derive(Clone, Copy)]
struct Lengths(Option<(f64, f64)>);

#[derive(Clone, Copy)]
enum Axis {
    X,
    Y,
    Diagonal,
}

impl Lengths {
    fn basis(self, axis: Axis) -> Result<f64, Coarse> {
        let (width, height) = self.0.ok_or(Coarse)?;
        Ok(match axis {
            Axis::X => width,
            Axis::Y => height,
            Axis::Diagonal => ((width * width + height * height) / 2.0).sqrt(),
        })
    }

    fn css(self, value: &LengthPercentage, axis: Axis) -> Result<f64, Coarse> {
        let resolved = match value.to_length() {
            Some(length) => length,
            None => value.resolve(Length::new(self.basis(axis)? as f32)),
        };
        let resolved = f64::from(resolved.px());
        resolved.is_finite().then_some(resolved).ok_or(Coarse)
    }

    /// An SVG geometry attribute in user units: absent is 0, numbers, `px`
    /// and percentages resolve. Other units, and values `moli_svg` does not
    /// parse (which may be valid CSS lengths), are not modelled.
    fn attribute(
        self,
        runtime: &JsContextHost,
        handle: DomHandle,
        attribute: &str,
        axis: Axis,
    ) -> Result<f64, Coarse> {
        let Some(raw) = runtime.dom_host().get_attribute(handle, attribute) else {
            return Ok(0.0);
        };
        let parsed = moli_svg::parse_length(&raw)
            .filter(|parsed| parsed.value.is_finite())
            .ok_or(Coarse)?;
        match parsed.unit {
            SvgLengthUnit::Number | SvgLengthUnit::Px => Ok(parsed.value),
            SvgLengthUnit::Percentage => Ok(parsed.value / 100.0 * self.basis(axis)?),
            _ => Err(Coarse),
        }
    }
}

/// Shape geometry from the computed geometry properties, which carry the
/// presentation attributes and any CSS that overrides them.
fn geometry_element(
    runtime: &JsContextHost,
    handle: DomHandle,
    local_name: &str,
    computed: &ComputedValues,
    lengths: Lengths,
) -> Result<Option<SvgGeometryElement>, Coarse> {
    use style::values::computed::{NonNegativeLengthPercentageOrAuto, Size};
    use style::values::generics::length::{GenericLengthPercentageOrAuto, GenericSize};
    let radius = |value: NonNegativeLengthPercentageOrAuto, axis| match value {
        GenericLengthPercentageOrAuto::LengthPercentage(value) => {
            lengths.css(&value.0, axis).map(Some)
        }
        GenericLengthPercentageOrAuto::Auto => Ok(None),
    };
    let size = |value: Size, axis| match value {
        GenericSize::LengthPercentage(value) => lengths.css(&value.0, axis),
        // `auto` is 0 for a rect; intrinsic keywords are not modelled.
        GenericSize::Auto => Ok(0.0),
        _ => Err(Coarse),
    };
    Ok(Some(match local_name {
        "circle" => SvgGeometryElement::Circle {
            cx: lengths.css(&computed.clone_cx(), Axis::X)?,
            cy: lengths.css(&computed.clone_cy(), Axis::Y)?,
            r: lengths.css(&computed.clone_r().0, Axis::Diagonal)?,
        },
        "ellipse" => {
            let rx = radius(computed.clone_rx(), Axis::X)?;
            let ry = radius(computed.clone_ry(), Axis::Y)?;
            SvgGeometryElement::Ellipse {
                cx: lengths.css(&computed.clone_cx(), Axis::X)?,
                cy: lengths.css(&computed.clone_cy(), Axis::Y)?,
                rx: rx.or(ry).unwrap_or(0.0),
                ry: ry.or(rx).unwrap_or(0.0),
            }
        }
        "rect" => {
            let rx = radius(computed.clone_rx(), Axis::X)?;
            let ry = radius(computed.clone_ry(), Axis::Y)?;
            SvgGeometryElement::Rect {
                x: lengths.css(&computed.clone_x(), Axis::X)?,
                y: lengths.css(&computed.clone_y(), Axis::Y)?,
                width: size(computed.clone_width(), Axis::X)?,
                height: size(computed.clone_height(), Axis::Y)?,
                rx: rx.or(ry).unwrap_or(0.0),
                ry: ry.or(rx).unwrap_or(0.0),
            }
        }
        "line" => SvgGeometryElement::Line {
            x1: lengths.attribute(runtime, handle, "x1", Axis::X)?,
            y1: lengths.attribute(runtime, handle, "y1", Axis::Y)?,
            x2: lengths.attribute(runtime, handle, "x2", Axis::X)?,
            y2: lengths.attribute(runtime, handle, "y2", Axis::Y)?,
        },
        // The allowlist ensures the computed path is the attribute's.
        "path" => SvgGeometryElement::Path {
            d: match computed.clone_d() {
                style::values::computed::DProperty::Path(path) => {
                    super::path_data::geometry_path_data(&path)
                }
                style::values::computed::DProperty::None => String::new(),
            },
        },
        "polygon" => SvgGeometryElement::Polygon {
            points: runtime
                .dom_host()
                .get_attribute(handle, "points")
                .unwrap_or_default(),
        },
        "polyline" => SvgGeometryElement::Polyline {
            points: runtime
                .dom_host()
                .get_attribute(handle, "points")
                .unwrap_or_default(),
        },
        _ => return Ok(None),
    }))
}

fn stroke_width(computed: &ComputedValues, lengths: Lengths) -> Result<f64, Coarse> {
    match computed.clone_stroke_width() {
        style::values::generics::svg::SVGLength::LengthPercentage(width) => {
            lengths.css(&width.0, Axis::Diagonal)
        }
        style::values::generics::svg::SVGLength::ContextValue => Err(Coarse),
    }
}

fn stroke_dash_offset(computed: &ComputedValues, lengths: Lengths) -> Result<f64, Coarse> {
    match computed.clone_stroke_dashoffset() {
        style::values::generics::svg::SVGLength::LengthPercentage(offset) => {
            lengths.css(&offset, Axis::Diagonal)
        }
        style::values::generics::svg::SVGLength::ContextValue => Err(Coarse),
    }
}

/// Resolved `stroke-dasharray`; empty for `none`.
fn stroke_dashes(computed: &ComputedValues, lengths: Lengths) -> Result<Vec<f64>, Coarse> {
    match computed.clone_stroke_dasharray() {
        style::values::generics::svg::SVGStrokeDashArray::Values(values) => values
            .iter()
            .map(|value| lengths.css(&value.0, Axis::Diagonal))
            .collect(),
        style::values::generics::svg::SVGStrokeDashArray::ContextValue => Err(Coarse),
    }
}

/// A positive `pathLength`, which recalibrates dashing. Zero or an invalid
/// value is not modelled.
fn path_length(runtime: &JsContextHost, handle: DomHandle) -> Result<Option<f64>, Coarse> {
    let Some(raw) = runtime.dom_host().get_attribute(handle, "pathLength") else {
        return Ok(None);
    };
    match moli_svg::parse_number(raw.trim()) {
        Some(value) if value.is_finite() && value > 0.0 => Ok(Some(value)),
        _ => Err(Coarse),
    }
}
