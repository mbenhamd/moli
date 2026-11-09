//! Client geometry of SVG content below an outermost `<svg>` box.
//!
//! CSS layout produces a box only for the outermost `<svg>`. Its descendants
//! report the bounding box of their SVG geometry mapped through the screen
//! CTM: element transforms, nested viewports and the outer viewport's viewBox
//! mapping into the outer box's content area.

use moli_svg::{SvgGeometryElement, SvgMatrixComponents};

use super::builders::{
    parse_svg_preserve_aspect_ratio_value, parse_svg_view_box_value, svg_bbox_for_handle,
    svg_bbox_has_non_rendered_ancestor, svg_computed_pixel_value,
    svg_geometry_optional_length_attribute_for_handle, svg_transform_for_handle,
};
use super::{
    SVG_MEET_OR_SLICE_SLICE, SVG_PRESERVE_ASPECT_RATIO_NONE, SVG_PRESERVE_ASPECT_RATIO_X_MAX_Y_MAX,
    SVG_PRESERVE_ASPECT_RATIO_X_MAX_Y_MID, SVG_PRESERVE_ASPECT_RATIO_X_MAX_Y_MIN,
    SVG_PRESERVE_ASPECT_RATIO_X_MID_Y_MAX, SVG_PRESERVE_ASPECT_RATIO_X_MID_Y_MID,
    SVG_PRESERVE_ASPECT_RATIO_X_MID_Y_MIN, SVG_PRESERVE_ASPECT_RATIO_X_MIN_Y_MAX,
    SVG_PRESERVE_ASPECT_RATIO_X_MIN_Y_MID,
};
use crate::document_runtime::DomHandle;
use crate::native_bridge::JsContextHost;

fn is_svg_element(runtime: &JsContextHost, handle: DomHandle) -> bool {
    runtime
        .dom_host()
        .node(handle)
        .is_some_and(|node| node.namespace() == Some(crate::native_bridge::document::SVG_NS))
}

fn is_outermost_svg(runtime: &JsContextHost, handle: DomHandle) -> bool {
    let Some(node) = runtime.dom_host().node(handle) else {
        return false;
    };
    node.local_name() == Some("svg")
        && node.namespace() == Some(crate::native_bridge::document::SVG_NS)
        && node
            .parent_node_id()
            .and_then(|parent| runtime.dom_host().node(parent))
            .is_none_or(|parent| {
                parent.namespace() != Some(crate::native_bridge::document::SVG_NS)
                    || parent.local_name() == Some("foreignObject")
            })
}

/// The outermost `<svg>` whose CSS box hosts `handle`, when `handle` is SVG
/// content without a CSS box of its own.
pub(crate) fn svg_layout_viewport_root(
    runtime: &JsContextHost,
    handle: DomHandle,
) -> Option<DomHandle> {
    if !is_svg_element(runtime, handle) || is_outermost_svg(runtime, handle) {
        return None;
    }
    let mut current = runtime.dom_host().node(handle)?.parent_node_id();
    while let Some(candidate) = current {
        let node = runtime.dom_host().node(candidate)?;
        if node.namespace() != Some(crate::native_bridge::document::SVG_NS)
            || node.local_name() == Some("foreignObject")
        {
            return None;
        }
        if is_outermost_svg(runtime, candidate) {
            return Some(candidate);
        }
        current = node.parent_node_id();
    }
    None
}

/// Client bounds `[x, y, width, height]` of `handle`, given the client
/// border box of its outermost `<svg>` (`root`). `None` when `handle` is not
/// rendered (inside `<defs>`-like containers or `display: none`).
pub(crate) fn svg_client_bounds(
    runtime: &JsContextHost,
    handle: DomHandle,
    root: DomHandle,
    root_border_box: [f64; 4],
) -> Option<[f64; 4]> {
    if svg_bbox_has_non_rendered_ancestor(runtime, handle) {
        return None;
    }
    let screen_ctm = svg_root_content_transform(runtime, root, root_border_box)
        .multiply(svg_ctm_below_root(runtime, handle, root));
    if let Some(bounds) = svg_bbox_for_handle(runtime, handle, screen_ctm, false, &mut Vec::new()) {
        return Some([bounds.x, bounds.y, bounds.width, bounds.height]);
    }
    // An element without geometry still has a position: its user-space origin.
    let origin = SvgGeometryElement::Rect {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
        rx: 0.0,
        ry: 0.0,
    };
    moli_svg::bounding_box_for_transformed_element(&origin, screen_ctm)
        .map(|origin| [origin.x, origin.y, 0.0, 0.0])
}

/// Transform from `handle`'s local user space (including its own transform)
/// to the user space established by `root`.
fn svg_ctm_below_root(
    runtime: &JsContextHost,
    handle: DomHandle,
    root: DomHandle,
) -> SvgMatrixComponents {
    let mut matrix = SvgMatrixComponents::identity();
    let mut current = Some(handle);
    while let Some(candidate) = current
        && candidate != root
    {
        let local = if runtime
            .dom_host()
            .node(candidate)
            .and_then(|node| node.local_name())
            == Some("svg")
        {
            svg_nested_viewport_transform(runtime, candidate)
        } else {
            svg_transform_for_handle(runtime, candidate)
        };
        matrix = local.multiply(matrix);
        current = runtime
            .dom_host()
            .node(candidate)
            .and_then(|node| node.parent_node_id());
    }
    matrix
}

fn svg_root_content_transform(
    runtime: &JsContextHost,
    root: DomHandle,
    [x, y, width, height]: [f64; 4],
) -> SvgMatrixComponents {
    let edge = |property: &str| svg_computed_pixel_value(runtime, root, property).unwrap_or(0.0);
    let left = edge("border-left-width") + edge("padding-left");
    let top = edge("border-top-width") + edge("padding-top");
    let right = edge("border-right-width") + edge("padding-right");
    let bottom = edge("border-bottom-width") + edge("padding-bottom");
    let content_width = (width - left - right).max(0.0);
    let content_height = (height - top - bottom).max(0.0);
    SvgMatrixComponents::translate(x + left, y + top).multiply(svg_view_box_transform(
        runtime,
        root,
        content_width,
        content_height,
    ))
}

/// The `x`/`y` offset and viewBox mapping a nested `<svg>` establishes.
pub(super) fn svg_nested_viewport_transform(
    runtime: &JsContextHost,
    svg: DomHandle,
) -> SvgMatrixComponents {
    let length = |attribute: &str| {
        svg_geometry_optional_length_attribute_for_handle(runtime, svg, attribute)
            .filter(|value| *value >= 0.0)
    };
    let view_box = runtime
        .dom_host()
        .get_attribute(svg, "viewBox")
        .as_deref()
        .and_then(parse_svg_view_box_value);
    let width = length("width").or(view_box.map(|[_, _, width, _]| width));
    let height = length("height").or(view_box.map(|[_, _, _, height]| height));
    let translate =
        SvgMatrixComponents::translate(length("x").unwrap_or(0.0), length("y").unwrap_or(0.0));
    match (width, height) {
        (Some(width), Some(height)) => {
            translate.multiply(svg_view_box_transform(runtime, svg, width, height))
        }
        _ => translate,
    }
}

/// The viewBox-to-viewport transform of `svg` for a viewport of the given
/// size (SVG 2 "equivalent transform of an SVG viewport").
fn svg_view_box_transform(
    runtime: &JsContextHost,
    svg: DomHandle,
    width: f64,
    height: f64,
) -> SvgMatrixComponents {
    let Some([view_x, view_y, view_width, view_height]) = runtime
        .dom_host()
        .get_attribute(svg, "viewBox")
        .as_deref()
        .and_then(parse_svg_view_box_value)
        .filter(|[_, _, width, height]| *width > 0.0 && *height > 0.0)
    else {
        return SvgMatrixComponents::identity();
    };
    let [align, meet_or_slice] = runtime
        .dom_host()
        .get_attribute(svg, "preserveAspectRatio")
        .as_deref()
        .and_then(parse_svg_preserve_aspect_ratio_value)
        .unwrap_or([SVG_PRESERVE_ASPECT_RATIO_X_MID_Y_MID, 1]);
    let mut scale_x = width / view_width;
    let mut scale_y = height / view_height;
    if align != SVG_PRESERVE_ASPECT_RATIO_NONE {
        let scale = if meet_or_slice == SVG_MEET_OR_SLICE_SLICE {
            scale_x.max(scale_y)
        } else {
            scale_x.min(scale_y)
        };
        scale_x = scale;
        scale_y = scale;
    }
    let mut translate_x = -view_x * scale_x;
    let mut translate_y = -view_y * scale_y;
    let free_x = width - view_width * scale_x;
    let free_y = height - view_height * scale_y;
    match align {
        SVG_PRESERVE_ASPECT_RATIO_X_MID_Y_MIN
        | SVG_PRESERVE_ASPECT_RATIO_X_MID_Y_MID
        | SVG_PRESERVE_ASPECT_RATIO_X_MID_Y_MAX => translate_x += free_x / 2.0,
        SVG_PRESERVE_ASPECT_RATIO_X_MAX_Y_MIN
        | SVG_PRESERVE_ASPECT_RATIO_X_MAX_Y_MID
        | SVG_PRESERVE_ASPECT_RATIO_X_MAX_Y_MAX => translate_x += free_x,
        _ => {}
    }
    match align {
        SVG_PRESERVE_ASPECT_RATIO_X_MIN_Y_MID
        | SVG_PRESERVE_ASPECT_RATIO_X_MID_Y_MID
        | SVG_PRESERVE_ASPECT_RATIO_X_MAX_Y_MID => translate_y += free_y / 2.0,
        SVG_PRESERVE_ASPECT_RATIO_X_MIN_Y_MAX
        | SVG_PRESERVE_ASPECT_RATIO_X_MID_Y_MAX
        | SVG_PRESERVE_ASPECT_RATIO_X_MAX_Y_MAX => translate_y += free_y,
        _ => {}
    }
    SvgMatrixComponents {
        a: scale_x,
        b: 0.0,
        c: 0.0,
        d: scale_y,
        e: translate_x,
        f: translate_y,
    }
}
