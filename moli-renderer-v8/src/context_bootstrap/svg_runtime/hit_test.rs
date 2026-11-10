//! Hit testing of SVG content below an outermost `<svg>` box.
//!
//! CSS layout produces a box only for the outermost `<svg>`, so point queries
//! stop there. This module resolves the content under the point from SVG
//! geometry. Where the model cannot be exact (clipping, masks, `<switch>`,
//! context paint, `<use>`, unresolvable lengths), a hit becomes
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
/// Candidates honour `pointer-events` (`auto` acts as `visiblePainted`),
/// `visibility`, `display` and nested `<svg>` viewport clipping. Shapes test
/// their exact fill and their stroke (width, caps, dashing, `pathLength`).
/// Painted text and images test their SVG bounding box.
pub(crate) fn svg_content_hits_at_point(
    runtime: &JsContextHost,
    root: DomHandle,
    content_box: [f64; 4],
    point: [f64; 2],
    first_only: bool,
) -> Result<Vec<DomHandle>, Coarse> {
    let [x, y, width, height] = content_box;
    let Some(root_style) = computed_values(runtime, root) else {
        return Ok(Vec::new());
    };
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
        uncertain: clips_or_masks(runtime, root, &root_style),
    };
    let mut walk = Walk {
        runtime,
        point,
        first_only,
        hits: Vec::new(),
    };
    walk.children(root, scope)?;
    Ok(walk.hits)
}

#[derive(Clone, Copy)]
struct Scope {
    ctm: SvgMatrixComponents,
    /// The nearest viewport's size in user units, when it is known.
    viewport: Option<(f64, f64)>,
    /// Inside clipping, masking or `<switch>`: a hit here is not exact.
    uncertain: bool,
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
            let mut child_scope = Scope {
                uncertain: scope.uncertain
                    || local_name == "switch"
                    || clips_or_masks(runtime, child, &computed)
                    || has_css_only_transform(runtime, child, &computed),
                ..scope
            };
            if local_name == "svg" {
                match nested_viewport(runtime, child, &computed, scope, self.point) {
                    NestedViewport::ClipsPoint => continue,
                    NestedViewport::Known(viewport) => child_scope.viewport = viewport,
                    NestedViewport::Uncertain => {
                        child_scope.uncertain = true;
                        child_scope.viewport = None;
                    }
                }
                child_scope.ctm = scope
                    .ctm
                    .multiply(svg_nested_viewport_transform(runtime, child));
            } else {
                child_scope.ctm = scope.ctm.multiply(svg_transform_for_handle(runtime, child));
            }
            if !matches!(local_name, "use" | "image" | "foreignObject")
                && self.children(child, child_scope)?
            {
                return Ok(true);
            }
            if let ShapeHit::Hit = self.element(child, local_name, &computed, child_scope)? {
                if child_scope.uncertain {
                    return Err(Coarse);
                }
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
                Ok(hit(moli_svg::is_point_in_shape_hit_area(
                    &geometry,
                    local,
                    SvgShapeHitArea {
                        fill: fill.then(|| svg_fill_rule_from_computed(computed)),
                        stroke,
                    },
                )))
            }
            // A `<textPath>` hits along its glyphs, which no bounding box
            // here models; inside its text's box the answer is not exact.
            "textPath" => match super::text_layout::svg_text_root(runtime, handle) {
                Some(text) if bbox_contains(runtime, text, local) => Err(Coarse),
                _ => Ok(ShapeHit::Miss),
            },
            "text" | "tspan" | "a" => {
                if super::text_layout::svg_text_root(runtime, handle).is_none() {
                    return Ok(ShapeHit::Miss);
                }
                let painted = paint_is_painted(runtime, handle, &computed.clone_fill())?
                    || paint_is_painted(runtime, handle, &computed.clone_stroke())?;
                Ok(hit(painted && bbox_contains(runtime, handle, local)))
            }
            "image" | "foreignObject" => Ok(hit(bbox_contains(runtime, handle, local))),
            // A `<use>` hits only where its shadow content is painted, which
            // its bounding box over-approximates.
            "use" if bbox_contains(runtime, handle, local) => Err(Coarse),
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

fn clips_or_masks(runtime: &JsContextHost, handle: DomHandle, computed: &ComputedValues) -> bool {
    !matches!(
        computed.clone_clip_path(),
        style::values::generics::basic_shape::ClipPath::None
    ) || runtime
        .dom_host()
        .get_attribute(handle, "clip-path")
        .is_some()
        || runtime.dom_host().get_attribute(handle, "mask").is_some()
}

/// Element transforms are read from the `transform` attribute. A CSS
/// transform without one places the element elsewhere.
fn has_css_only_transform(
    runtime: &JsContextHost,
    handle: DomHandle,
    computed: &ComputedValues,
) -> bool {
    use style::values::generics::transform::{Rotate, Scale, Translate};
    let css_transform = !computed.clone_transform().0.is_empty()
        && runtime
            .dom_host()
            .get_attribute(handle, "transform")
            .is_none();
    css_transform
        || !matches!(computed.clone_rotate(), Rotate::None)
        || !matches!(computed.clone_scale(), Scale::None)
        || !matches!(computed.clone_translate(), Translate::None)
}

enum NestedViewport {
    /// The nested viewport clips its content away from the point.
    ClipsPoint,
    Known(Option<(f64, f64)>),
    Uncertain,
}

/// The viewport a nested `<svg>` establishes, from its `x`, `y`, `width`,
/// `height` and `viewBox` in the parent's user space.
fn nested_viewport(
    runtime: &JsContextHost,
    svg: DomHandle,
    computed: &ComputedValues,
    parent: Scope,
    point: [f64; 2],
) -> NestedViewport {
    // `svg_nested_viewport_transform` reads plain numbers; other units would
    // place the viewport elsewhere.
    let mut unit_error = false;
    let mut length = |attribute: &str| {
        let parsed = runtime
            .dom_host()
            .get_attribute(svg, attribute)
            .as_deref()
            .and_then(moli_svg::parse_length)?;
        // The transform also drops negative values, which then disagree
        // with the viewport clip.
        if matches!(parsed.unit, SvgLengthUnit::Number | SvgLengthUnit::Px)
            && parsed.value.is_finite()
            && parsed.value >= 0.0
        {
            Some(parsed.value)
        } else {
            unit_error = true;
            None
        }
    };
    let [x, y, width, height] = [length("x"), length("y"), length("width"), length("height")];
    // A nested `<svg>`'s own `transform` is not applied below.
    if unit_error || runtime.dom_host().get_attribute(svg, "transform").is_some() {
        return NestedViewport::Uncertain;
    }
    let view_box = runtime
        .dom_host()
        .get_attribute(svg, "viewBox")
        .as_deref()
        .and_then(parse_svg_view_box_value)
        .filter(|[_, _, width, height]| *width > 0.0 && *height > 0.0);
    let size = width.zip(height);
    let viewport = view_box.map(|[_, _, w, h]| (w, h)).or(size);
    if computed.clone_overflow_x() == style::computed_values::overflow_x::T::Visible
        && computed.clone_overflow_y() == style::computed_values::overflow_y::T::Visible
    {
        return NestedViewport::Known(viewport);
    }
    let Some((width, height)) = size else {
        return NestedViewport::Uncertain;
    };
    if !parent.ctm.is_invertible() {
        return NestedViewport::ClipsPoint;
    }
    let inverse = parent.ctm.inverse();
    let [px, py] = point;
    let local_x = inverse.a * px + inverse.c * py + inverse.e;
    let local_y = inverse.b * px + inverse.d * py + inverse.f;
    let (x, y) = (x.unwrap_or(0.0), y.unwrap_or(0.0));
    if local_x < x || local_y < y || local_x > x + width || local_y > y + height {
        NestedViewport::ClipsPoint
    } else {
        NestedViewport::Known(viewport)
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
    let same_document = |url: &str| url.split('#').next().unwrap_or(url);
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

    /// An SVG geometry attribute in user units: absent or invalid is 0,
    /// numbers, `px` and percentages resolve, other units are not modelled.
    fn attribute(
        self,
        runtime: &JsContextHost,
        handle: DomHandle,
        attribute: &str,
        axis: Axis,
    ) -> Result<f64, Coarse> {
        let Some(parsed) = runtime
            .dom_host()
            .get_attribute(handle, attribute)
            .as_deref()
            .and_then(moli_svg::parse_length)
            .filter(|parsed| parsed.value.is_finite())
        else {
            return Ok(0.0);
        };
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
        "path" => SvgGeometryElement::Path {
            d: match computed.clone_d() {
                style::values::computed::DProperty::Path(path) => {
                    super::path_data::geometry_path_data(&path)
                }
                style::values::computed::DProperty::None => runtime
                    .dom_host()
                    .get_attribute(handle, "d")
                    .unwrap_or_default(),
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
