//! Client geometry of SVG content below an outermost `<svg>` box.
//!
//! CSS layout produces a box only for the outermost `<svg>`. Its descendants
//! report the bounding box of their SVG geometry mapped through the screen
//! CTM: element transforms, nested viewports and the outer viewport's viewBox
//! mapping into the outer box's content area.

use moli_svg::SvgMatrixComponents;

use super::builders::{
    parse_svg_preserve_aspect_ratio_value, parse_svg_view_box_value, svg_bbox_for_handle,
    svg_bbox_has_non_rendered_ancestor, svg_computed_pixel_value,
    svg_geometry_optional_length_attribute_for_handle, svg_has_non_rendered_ancestor_with,
    svg_transform_for_handle,
};
use super::hit_test::computed_transform_matrix;
use super::{
    SVG_MEET_OR_SLICE_SLICE, SVG_PRESERVE_ASPECT_RATIO_NONE, SVG_PRESERVE_ASPECT_RATIO_X_MAX_Y_MAX,
    SVG_PRESERVE_ASPECT_RATIO_X_MAX_Y_MID, SVG_PRESERVE_ASPECT_RATIO_X_MAX_Y_MIN,
    SVG_PRESERVE_ASPECT_RATIO_X_MID_Y_MAX, SVG_PRESERVE_ASPECT_RATIO_X_MID_Y_MID,
    SVG_PRESERVE_ASPECT_RATIO_X_MID_Y_MIN, SVG_PRESERVE_ASPECT_RATIO_X_MIN_Y_MAX,
    SVG_PRESERVE_ASPECT_RATIO_X_MIN_Y_MID,
};
use crate::document_runtime::DomHandle;
use crate::native_bridge::PageScriptHost;
use crate::native_bridge::element::StyleObservation;

fn is_svg_element(runtime: &PageScriptHost, handle: DomHandle) -> bool {
    runtime
        .dom_host()
        .node(handle)
        .is_some_and(|node| node.namespace() == Some(crate::native_bridge::document::SVG_NS))
}

pub(crate) fn is_outermost_svg(runtime: &PageScriptHost, handle: DomHandle) -> bool {
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
    runtime: &PageScriptHost,
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

/// The `<foreignObject>` whose CSS content contains `handle`, and the
/// outermost `<svg>` whose CSS box hosts that `<foreignObject>`.
///
/// Only non-SVG ancestors may separate `handle` from the `<foreignObject>`:
/// HTML placed directly in other SVG content is not rendered. `handle` may
/// itself be an outermost `<svg>` embedded in the `<foreignObject>`.
pub(crate) fn svg_foreign_object_host(
    runtime: &PageScriptHost,
    handle: DomHandle,
) -> Option<(DomHandle, DomHandle)> {
    let mut current = runtime.dom_host().node(handle)?.parent_node_id();
    while let Some(candidate) = current {
        let node = runtime.dom_host().node(candidate)?;
        if node.namespace() == Some(crate::native_bridge::document::SVG_NS) {
            if node.local_name() != Some("foreignObject")
                || svg_bbox_has_non_rendered_ancestor(runtime, candidate)
            {
                return None;
            }
            return svg_layout_viewport_root(runtime, candidate).map(|root| (candidate, root));
        }
        current = node.parent_node_id();
    }
    None
}

/// Transform from the CSS content space of `foreign_object` (CSS pixels
/// with the origin at its `x`/`y`) to client coordinates, given the client
/// border box of its outermost `<svg>` (`root`).
pub(crate) fn svg_foreign_object_content_transform(
    runtime: &PageScriptHost,
    foreign_object: DomHandle,
    root: DomHandle,
    root_border_box: [f64; 4],
) -> SvgMatrixComponents {
    let offset = |attribute: &str| {
        svg_geometry_optional_length_attribute_for_handle(runtime, foreign_object, attribute)
            .unwrap_or(0.0)
    };
    svg_root_content_transform(runtime, root, root_border_box)
        .multiply(svg_ctm_below_root(runtime, foreign_object, Some(root)))
        .multiply(SvgMatrixComponents::translate(offset("x"), offset("y")))
}

/// One rendered `<foreignObject>` of an SVG image document.
pub(crate) struct SvgImageForeignObject {
    pub(crate) handle: DomHandle,
    pub(crate) width: f64,
    pub(crate) height: f64,
    /// From the CSS content space (origin at `x`/`y`) to image pixels.
    pub(crate) content_to_image: SvgMatrixComponents,
}

/// The size percentages resolve against in an SVG image document: the root
/// viewBox when it has one, otherwise the image's own size.
pub(crate) fn svg_image_user_viewport(
    runtime: &PageScriptHost,
    root: DomHandle,
    image_size: (f64, f64),
) -> (f64, f64) {
    runtime
        .dom_host()
        .get_attribute(root, "viewBox")
        .as_deref()
        .and_then(parse_svg_view_box_value)
        .filter(|[_, _, width, height]| *width > 0.0 && *height > 0.0)
        .map_or(image_size, |[_, _, width, height]| (width, height))
}

/// Every rendered `<foreignObject>` below the root `<svg>` of an image
/// document, in document order. Percentages use the root's user viewport, also
/// below a nested `<svg>`. `styles` reads the image document's own styles.
pub(crate) fn svg_image_foreign_objects(
    runtime: &PageScriptHost,
    styles: &mut StyleObservation<'_>,
    root: DomHandle,
    image_size: (f64, f64),
) -> Vec<SvgImageForeignObject> {
    let (viewport_width, viewport_height) = svg_image_user_viewport(runtime, root, image_size);
    let root_transform = svg_view_box_transform(runtime, root, image_size.0, image_size.1);
    let length = |handle: DomHandle, attribute: &str, reference: f64| {
        let parsed = runtime
            .dom_host()
            .get_attribute(handle, attribute)
            .as_deref()
            .and_then(moli_svg::parse_length)
            .filter(|length| length.value.is_finite())?;
        Some(if parsed.unit == moli_svg::SvgLengthUnit::Percentage {
            parsed.value / 100.0 * reference
        } else {
            parsed.value
        })
    };
    let mut found = Vec::new();
    let mut pending = runtime.dom_host().child_handles(root).collect::<Vec<_>>();
    pending.reverse();
    while let Some(handle) = pending.pop() {
        if !is_svg_element(runtime, handle) {
            continue;
        }
        let is_foreign_object = runtime
            .dom_host()
            .node(handle)
            .and_then(|node| node.local_name())
            == Some("foreignObject");
        if !is_foreign_object {
            let start = pending.len();
            pending.extend(runtime.dom_host().child_handles(handle));
            pending[start..].reverse();
            continue;
        }
        let width = length(handle, "width", viewport_width).unwrap_or(0.0);
        let height = length(handle, "height", viewport_height).unwrap_or(0.0);
        if width <= 0.0
            || height <= 0.0
            || svg_has_non_rendered_ancestor_with(runtime, handle, |candidate| {
                styles
                    .read(candidate)
                    .computed_values()
                    .is_some_and(|computed| computed.get_box().display.is_none())
            })
        {
            continue;
        }
        let x = length(handle, "x", viewport_width).unwrap_or(0.0);
        let y = length(handle, "y", viewport_height).unwrap_or(0.0);
        // The element's own transform is its computed `transform`, which
        // carries the attribute as a presentation hint unless author CSS
        // replaces it. The hint is dropped when the attribute is not also
        // valid CSS (`rotate(30)`, `rotate(90 20 10)`), and a computed value
        // that needs a reference box has no matrix here; both use the
        // attribute. Known gaps: an identity CSS transform (`none`,
        // `translate(0px, 0px)`) over a non-identity attribute keeps the
        // attribute, because computed values do not say whether the hint or
        // author CSS supplied them; and the transform applies about
        // `0 0`. An author `transform-origin` is ignored, because this
        // stylesheet set leaves SVG elements at the CSS initial `50% 50%`, so
        // an authored `center` cannot be told from the SVG default.
        let attribute_transform = svg_transform_for_handle(runtime, handle);
        let own_transform = styles
            .read(handle)
            .computed_values()
            .and_then(|computed| computed_transform_matrix(&computed))
            .filter(|computed| *computed != SvgMatrixComponents::identity())
            .unwrap_or(attribute_transform);
        let parent = runtime
            .dom_host()
            .node(handle)
            .and_then(|node| node.parent_node_id());
        let parent_ctm = parent.map_or_else(SvgMatrixComponents::identity, |parent| {
            svg_ctm_below_root(runtime, parent, Some(root))
        });
        found.push(SvgImageForeignObject {
            handle,
            width,
            height,
            content_to_image: root_transform
                .multiply(parent_ctm)
                .multiply(own_transform)
                .multiply(SvgMatrixComponents::translate(x, y)),
        });
    }
    found
}

/// Client bounds `[x, y, width, height]` of `handle`, given the client
/// border box of its outermost `<svg>` (`root`). `None` when `handle` is not
/// rendered (inside `<defs>`-like containers or `display: none`).
pub(crate) fn svg_client_bounds(
    runtime: &PageScriptHost,
    handle: DomHandle,
    root: DomHandle,
    root_border_box: [f64; 4],
) -> Option<[f64; 4]> {
    if svg_bbox_has_non_rendered_ancestor(runtime, handle) {
        return None;
    }
    let screen_ctm = svg_root_content_transform(runtime, root, root_border_box)
        .multiply(svg_ctm_below_root(runtime, handle, Some(root)));
    if let Some(bounds) = svg_bbox_for_handle(runtime, handle, screen_ctm, false, &mut Vec::new()) {
        return Some([bounds.x, bounds.y, bounds.width, bounds.height]);
    }
    // An element without geometry still has a position: its user-space
    // origin mapped through the screen CTM.
    Some([screen_ctm.e, screen_ctm.f, 0.0, 0.0])
}

/// Client stroke bounds `[x, y, width, height]` of `handle`: its client
/// bounds grown by half its painted stroke width, scaled by the screen CTM.
/// DevTools reports these as an SVG element's box model and content quad.
pub(crate) fn svg_client_stroke_bounds(
    runtime: &PageScriptHost,
    handle: DomHandle,
    root: DomHandle,
    root_border_box: [f64; 4],
) -> Option<[f64; 4]> {
    let [x, y, width, height] = svg_client_bounds(runtime, handle, root, root_border_box)?;
    let screen_ctm = svg_root_content_transform(runtime, root, root_border_box)
        .multiply(svg_ctm_below_root(runtime, handle, Some(root)));
    let scale = (screen_ctm.a * screen_ctm.d - screen_ctm.b * screen_ctm.c)
        .abs()
        .sqrt();
    let outset = super::hit_test::svg_shape_stroke_outset(runtime, handle) * scale;
    Some([
        x - outset,
        y - outset,
        width + 2.0 * outset,
        height + 2.0 * outset,
    ])
}

/// Transform from `handle`'s local user space (including its own transform)
/// to the user space established by `root`. Without a `root` the walk stops
/// at the first non-SVG ancestor (or the top of a detached tree).
fn svg_ctm_below_root(
    runtime: &PageScriptHost,
    handle: DomHandle,
    root: Option<DomHandle>,
) -> SvgMatrixComponents {
    let mut matrix = SvgMatrixComponents::identity();
    let mut current = Some(handle);
    while let Some(candidate) = current
        && Some(candidate) != root
        && is_svg_element(runtime, candidate)
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

/// The rendered viewport of an outermost `<svg>`: its untransformed content
/// size and the map from its content box to client coordinates.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SvgRootViewport {
    pub(crate) width: f64,
    pub(crate) height: f64,
    pub(crate) content_to_client: SvgMatrixComponents,
}

/// The viewBox mapping an outermost `<svg>` establishes; an unrendered root
/// has no viewport, so it contributes the identity.
fn svg_root_view_box_transform(
    runtime: &PageScriptHost,
    root: DomHandle,
    viewport: Option<SvgRootViewport>,
) -> SvgMatrixComponents {
    viewport.map_or_else(SvgMatrixComponents::identity, |viewport| {
        svg_view_box_transform(runtime, root, viewport.width, viewport.height)
    })
}

/// `getScreenCTM()`: from `handle`'s user space to client coordinates. `root`
/// is the outermost `<svg>` hosting `handle` (possibly `handle` itself);
/// without a rendered viewport only the user-space walk below it remains.
pub(crate) fn svg_screen_ctm(
    runtime: &PageScriptHost,
    handle: DomHandle,
    root: Option<DomHandle>,
    viewport: Option<SvgRootViewport>,
) -> SvgMatrixComponents {
    let below = svg_ctm_below_root(runtime, handle, root);
    match (root, viewport) {
        (Some(root), Some(viewport)) => viewport
            .content_to_client
            .multiply(svg_root_view_box_transform(runtime, root, Some(viewport)))
            .multiply(below),
        _ => below,
    }
}

/// `getCTM()`: from `handle`'s user space to the user space of its nearest
/// viewport element, including that viewport's own `x`/`y` and viewBox
/// mapping (SVG 2 §8.4, the "nearest viewport" CTM scope). `root` is the
/// outermost `<svg>`, whose viewBox maps into its untransformed content box
/// when rendered.
pub(crate) fn svg_nearest_viewport_ctm(
    runtime: &PageScriptHost,
    handle: DomHandle,
    root: Option<DomHandle>,
    viewport: Option<SvgRootViewport>,
) -> SvgMatrixComponents {
    let mut matrix = SvgMatrixComponents::identity();
    let mut current = Some(handle);
    while let Some(candidate) = current
        && is_svg_element(runtime, candidate)
    {
        let is_svg = runtime
            .dom_host()
            .node(candidate)
            .and_then(|node| node.local_name())
            == Some("svg");
        let is_root = Some(candidate) == root;
        let local = if is_root {
            svg_root_view_box_transform(runtime, candidate, viewport)
        } else if is_svg {
            svg_nested_viewport_transform(runtime, candidate)
        } else {
            svg_transform_for_handle(runtime, candidate)
        };
        matrix = local.multiply(matrix);
        if is_root || (candidate != handle && is_svg) {
            break;
        }
        current = runtime
            .dom_host()
            .node(candidate)
            .and_then(|node| node.parent_node_id());
    }
    matrix
}

/// Client content box `[x, y, width, height]` of the outermost `<svg>`.
pub(crate) fn svg_root_content_box(
    runtime: &PageScriptHost,
    root: DomHandle,
    [x, y, width, height]: [f64; 4],
) -> [f64; 4] {
    let edge = |property: &str| svg_computed_pixel_value(runtime, root, property).unwrap_or(0.0);
    let left = edge("border-left-width") + edge("padding-left");
    let top = edge("border-top-width") + edge("padding-top");
    let right = edge("border-right-width") + edge("padding-right");
    let bottom = edge("border-bottom-width") + edge("padding-bottom");
    [
        x + left,
        y + top,
        (width - left - right).max(0.0),
        (height - top - bottom).max(0.0),
    ]
}

fn svg_root_content_transform(
    runtime: &PageScriptHost,
    root: DomHandle,
    root_border_box: [f64; 4],
) -> SvgMatrixComponents {
    let [x, y, width, height] = svg_root_content_box(runtime, root, root_border_box);
    SvgMatrixComponents::translate(x, y)
        .multiply(svg_view_box_transform(runtime, root, width, height))
}

/// The `x`/`y` offset and viewBox mapping a nested `<svg>` establishes.
pub(super) fn svg_nested_viewport_transform(
    runtime: &PageScriptHost,
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
pub(super) fn svg_view_box_transform(
    runtime: &PageScriptHost,
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
