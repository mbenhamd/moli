use crate::web_api_interfaces;
use std::collections::HashSet;

use super::super::element::{ClientRect, observable_caret_position, observable_geometry_batch};
use super::super::node::{node_is_document, node_runtime_and_handle_from_args};
use crate::document_runtime::DomHandle;
use crate::native_bridge::{JsContextHost, set_wrapped_handle_or_null_for_receiver};
use crate::util::{
    context_host_ptr_from_global_bridge, get_private_value, node_wrapper_from_handle,
    serialize_v8_array, throw_type_error, v8_string, v8str,
};
use crate::webidl;
use moli_webapi_declare::{WebApiFunctionTemplate, WebApiObject};

const CARET_POSITION_OFFSET_NODE_SLOT: &str = "__moliCaretPositionOffsetNode";
const CARET_POSITION_OFFSET_SLOT: &str = "__moliCaretPositionOffset";
const CARET_POSITION_RECT_X_SLOT: &str = "__moliCaretPositionRectX";
const CARET_POSITION_RECT_Y_SLOT: &str = "__moliCaretPositionRectY";
const CARET_POSITION_RECT_WIDTH_SLOT: &str = "__moliCaretPositionRectWidth";
const CARET_POSITION_RECT_HEIGHT_SLOT: &str = "__moliCaretPositionRectHeight";

#[derive(WebApiObject)]
#[webapi(interface = web_api_interfaces::CaretPosition)]
struct CaretPositionDeclaration<'scope> {
    #[webapi(slot = CARET_POSITION_OFFSET_NODE_SLOT)]
    offset_node: v8::Local<'scope, v8::Object>,
    #[webapi(slot = CARET_POSITION_OFFSET_SLOT)]
    offset: f64,
    #[webapi(slot = CARET_POSITION_RECT_X_SLOT)]
    rect_x: f64,
    #[webapi(slot = CARET_POSITION_RECT_Y_SLOT)]
    rect_y: f64,
    #[webapi(slot = CARET_POSITION_RECT_WIDTH_SLOT)]
    rect_width: f64,
    #[webapi(slot = CARET_POSITION_RECT_HEIGHT_SLOT)]
    rect_height: f64,
}

#[derive(WebApiFunctionTemplate)]
#[webapi(interface = web_api_interfaces::CaretPosition, enumerable)]
struct CaretPositionPrototypeDeclaration {
    #[webapi(accessor_property, getter = caret_position_offset_node_getter)]
    offset_node: (),
    #[webapi(accessor_property, getter = caret_position_offset_getter)]
    offset: (),
    #[webapi(method, length = 0, callback = caret_position_get_client_rect_callback)]
    get_client_rect: (),
}

#[derive(webidl::WebIdlArgs)]
#[webidl(prefix = "Document.elementFromPoint")]
struct DocumentElementFromPointArgs {
    #[webidl(required, converter = "double")]
    x: f64,
    #[webidl(required, converter = "double")]
    y: f64,
}

#[derive(webidl::WebIdlArgs)]
#[webidl(prefix = "Document.elementsFromPoint")]
struct DocumentElementsFromPointArgs {
    #[webidl(required, converter = "double")]
    x: f64,
    #[webidl(required, converter = "double")]
    y: f64,
}

fn parse_caret_shadow_roots_argument(
    scope: &mut v8::PinScope<'_, '_>,
    runtime: &JsContextHost,
    args: &v8::FunctionCallbackArguments<'_>,
) -> Option<Vec<DomHandle>> {
    if args.length() < 3 || args.get(2).is_null_or_undefined() {
        return Some(Vec::new());
    }
    let options = args.get(2);
    if !options.is_object() {
        throw_type_error(
            scope,
            "Document.caretPositionFromPoint options must be an object.",
        );
        return None;
    }
    let options = v8::Local::<v8::Object>::try_from(options).ok()?;
    let Some(shadow_roots_value) = options.get(scope, v8str(scope, "shadowRoots").into()) else {
        return Some(Vec::new());
    };
    if shadow_roots_value.is_null_or_undefined() {
        return Some(Vec::new());
    }
    let Some(shadow_roots) = v8::Local::<v8::Array>::try_from(shadow_roots_value).ok() else {
        throw_type_error(
            scope,
            "Document.caretPositionFromPoint shadowRoots must be a sequence.",
        );
        return None;
    };

    let mut handles = Vec::new();
    for index in 0..shadow_roots.length() {
        let Some(value) = shadow_roots.get_index(scope, index) else {
            continue;
        };
        if let Some(handle) = crate::native_bridge::callback_value_dom_handle(scope, value)
            && runtime.dom_host().is_shadow_root(handle)
        {
            handles.push(handle);
        }
    }
    Some(handles)
}

fn visual_line_starts(text: &str, cols: usize) -> Vec<usize> {
    let cols = cols.max(1);
    let chars: Vec<char> = text.chars().collect();
    let mut starts = vec![0];
    let mut column = 0usize;
    for (index, ch) in chars.iter().enumerate() {
        if *ch == '\n' {
            column = 0;
            if index + 1 < chars.len() {
                starts.push(index + 1);
            }
            continue;
        }
        column += 1;
        if column == cols && index + 1 < chars.len() && chars[index + 1] != '\n' {
            starts.push(index + 1);
            column = 0;
        }
    }
    starts
}

fn textarea_caret_offset_and_rect(
    runtime: &JsContextHost,
    handle: DomHandle,
    rect: ClientRect,
    x: f64,
    y: f64,
) -> (u32, ClientRect) {
    let rows = runtime
        .dom_host()
        .get_attribute(handle, "rows")
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(1);
    let cols = runtime
        .dom_host()
        .get_attribute(handle, "cols")
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(20);
    let line_height = if rows > 0 && rect.height > 0.0 {
        rect.height / rows as f64
    } else {
        20.0
    };
    let char_width = if cols > 0 && rect.width > 0.0 {
        rect.width / cols as f64
    } else {
        line_height
    };
    let row = ((y - rect.top) / line_height)
        .floor()
        .clamp(0.0, (rows.saturating_sub(1)) as f64) as usize;
    let col = ((x - rect.left) / char_width)
        .floor()
        .clamp(0.0, cols as f64) as usize;
    let text = runtime.dom_host().text_content(handle).unwrap_or_default();
    let starts = visual_line_starts(&text, cols);
    let start = starts
        .get(row)
        .copied()
        .or_else(|| starts.last().copied())
        .unwrap_or(0);
    let end = starts
        .get(row + 1)
        .copied()
        .map(|next| {
            if text
                .chars()
                .nth(next.saturating_sub(1))
                .is_some_and(|ch| ch == '\n')
            {
                next.saturating_sub(1)
            } else {
                next
            }
        })
        .unwrap_or_else(|| text.chars().count());
    let char_offset = start.saturating_add(col).min(end);
    let offset = text
        .chars()
        .take(char_offset)
        .map(char::len_utf16)
        .sum::<usize>()
        .min(u32::MAX as usize) as u32;
    let caret_left = rect.left + char_width * col as f64;
    let caret_top = rect.top + line_height * row as f64;
    (
        offset,
        ClientRect {
            left: caret_left,
            top: caret_top,
            right: caret_left,
            bottom: caret_top + line_height,
            width: 0.0,
            height: line_height,
        },
    )
}

fn child_offset(runtime: &JsContextHost, parent: DomHandle, child: DomHandle) -> u32 {
    runtime.dom_host().child_index(parent, child).unwrap_or(0) as u32
}

fn box_rect_for_source(
    source: DomHandle,
    ancestor_boxes: &[(DomHandle, moli_layout::LayoutBoxModel)],
) -> Option<ClientRect> {
    ancestor_boxes.iter().find_map(|(candidate, model)| {
        (*candidate == source).then(|| ClientRect::from(model.border))
    })
}

fn caret_candidate_from_layout(
    runtime: &JsContextHost,
    position: &moli_layout::LayoutCaretPosition<DomHandle>,
    x: f64,
    y: f64,
) -> (DomHandle, u32, ClientRect) {
    let caret_rect = ClientRect::from(position.rect);
    if let Some(offset) = position.utf16_offset {
        return (
            position.source,
            offset.min(u32::MAX as usize) as u32,
            caret_rect,
        );
    }

    let Some(element) = element_for_hit_source(runtime, position.source) else {
        return (position.source, 0, caret_rect);
    };
    let element_rect = box_rect_for_source(element, &position.ancestor_boxes).unwrap_or(caret_rect);
    if (runtime.dom_host().is_html_element_named(element, "audio")
        || runtime.dom_host().is_html_element_named(element, "video"))
        && let Some(parent) = runtime.dom_host().parent_node(element)
    {
        return (
            parent,
            child_offset(runtime, parent, element),
            ClientRect {
                left: element_rect.left,
                right: element_rect.left,
                width: 0.0,
                ..element_rect
            },
        );
    }
    if runtime
        .dom_host()
        .is_html_element_named(element, "textarea")
    {
        let (offset, rect) = textarea_caret_offset_and_rect(runtime, element, element_rect, x, y);
        return (element, offset, rect);
    }
    (element, 0, caret_rect)
}

fn shadow_host_parent_caret(
    runtime: &JsContextHost,
    shadow_root: DomHandle,
    ancestor_boxes: &[(DomHandle, moli_layout::LayoutBoxModel)],
    fallback_rect: ClientRect,
) -> Option<(DomHandle, u32, ClientRect)> {
    let host = runtime.dom_host().shadow_root_host(shadow_root)?;
    let parent = runtime.dom_host().parent_node(host)?;
    let host_rect = box_rect_for_source(host, ancestor_boxes).unwrap_or(fallback_rect);
    Some((
        parent,
        child_offset(runtime, parent, host),
        ClientRect {
            left: host_rect.left,
            top: host_rect.top,
            right: host_rect.left,
            bottom: host_rect.bottom,
            width: 0.0,
            height: host_rect.height,
        },
    ))
}

fn retarget_caret_position(
    runtime: &JsContextHost,
    mut node: DomHandle,
    mut offset: u32,
    mut rect: ClientRect,
    shadow_roots: &[DomHandle],
    ancestor_boxes: &[(DomHandle, moli_layout::LayoutBoxModel)],
) -> (DomHandle, u32, ClientRect) {
    loop {
        if let Some(root) = runtime.dom_host().containing_shadow_root(node) {
            if shadow_roots.contains(&root) {
                return (node, offset, rect);
            }
            let Some((parent, parent_offset, parent_rect)) =
                shadow_host_parent_caret(runtime, root, ancestor_boxes, rect)
            else {
                return (node, offset, rect);
            };
            node = parent;
            offset = parent_offset;
            rect = parent_rect;
            continue;
        }
        if runtime.dom_host().is_shadow_root(node) && !shadow_roots.contains(&node) {
            let Some((parent, parent_offset, parent_rect)) =
                shadow_host_parent_caret(runtime, node, ancestor_boxes, rect)
            else {
                return (node, offset, rect);
            };
            node = parent;
            offset = parent_offset;
            rect = parent_rect;
            continue;
        }
        return (node, offset, rect);
    }
}

fn number_slot<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
    slot: &'static str,
) -> f64 {
    get_private_value(scope, object, slot)
        .and_then(|value| value.number_value(scope))
        .unwrap_or(f64::NAN)
}

fn caret_position_offset_node_getter<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let value = get_private_value(scope, args.this(), CARET_POSITION_OFFSET_NODE_SLOT)
        .unwrap_or_else(|| v8::undefined(scope).into());
    rv.set(value);
}

fn caret_position_offset_getter<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let value = get_private_value(scope, args.this(), CARET_POSITION_OFFSET_SLOT)
        .unwrap_or_else(|| v8::undefined(scope).into());
    rv.set(value);
}

fn caret_position_get_client_rect_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let this = args.this();
    let x = number_slot(scope, this, CARET_POSITION_RECT_X_SLOT);
    let y = number_slot(scope, this, CARET_POSITION_RECT_Y_SLOT);
    let width = number_slot(scope, this, CARET_POSITION_RECT_WIDTH_SLOT);
    let height = number_slot(scope, this, CARET_POSITION_RECT_HEIGHT_SLOT);
    let global = scope.get_current_context().global(scope);
    let Some(constructor) = global
        .get(scope, v8str(scope, "DOMRect").into())
        .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok())
    else {
        rv.set_null();
        return;
    };
    let x = v8::Number::new(scope, x);
    let y = v8::Number::new(scope, y);
    let width = v8::Number::new(scope, width);
    let height = v8::Number::new(scope, height);
    if let Some(rect) =
        constructor.new_instance(scope, &[x.into(), y.into(), width.into(), height.into()])
    {
        rv.set(rect.into());
    } else {
        rv.set_null();
    }
}

pub(crate) fn install_caret_position_template_bindings<'s>(
    scope: &mut v8::PinScope<'s, '_, ()>,
    template: v8::Local<'s, v8::FunctionTemplate>,
    interface_name: &str,
) {
    if interface_name == "CaretPosition" {
        CaretPositionPrototypeDeclaration::initialize_prototype_template(
            scope,
            template.prototype_template(scope),
        );
    }
}

fn build_caret_position_object<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    receiver: v8::Local<'s, v8::Object>,
    node: DomHandle,
    offset: u32,
    rect: ClientRect,
) -> Option<v8::Local<'s, v8::Object>> {
    let context = receiver.get_creation_context(scope)?;
    let scope = &mut v8::ContextScope::new(scope, context);
    let wrapper = node_wrapper_from_handle(scope, node)?;
    CaretPositionDeclaration::new(
        wrapper,
        offset as f64,
        rect.left,
        rect.top,
        rect.width,
        rect.height,
    )
    .bind(scope)
    .ok()
}

fn document_runtime_and_handle_from_args<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: &v8::FunctionCallbackArguments<'s>,
) -> anyhow::Result<(*mut JsContextHost, DomHandle)> {
    node_runtime_and_handle_from_args(scope, args).or_else(|error| {
        let Some(runtime_ptr) = context_host_ptr_from_global_bridge(scope) else {
            return Err(error);
        };
        super::detached_native_handle_for_runtime(scope, runtime_ptr, args.this())
            .map(|handle| (runtime_ptr, handle))
            .ok_or(error)
    })
}

fn element_for_hit_source(runtime: &JsContextHost, mut handle: DomHandle) -> Option<DomHandle> {
    loop {
        let node = runtime.dom_host().node(handle)?;
        if node.is_element() {
            return Some(handle);
        }
        let parent = node.parent_node()?;
        if runtime.dom_host().is_shadow_root(parent) {
            return runtime.dom_host().shadow_root_host(parent);
        }
        handle = parent;
    }
}

fn list_hit_accepts_non_element_source(runtime: &JsContextHost, source: DomHandle) -> bool {
    let Some(parent) = runtime
        .dom_host()
        .node(source)
        .and_then(|node| node.parent_node())
    else {
        return false;
    };
    let Some(parent) = runtime
        .dom_host()
        .node(parent)
        .and_then(|node| node.as_element())
    else {
        return false;
    };

    // Blink's TreeScope::ElementsFromHitTestResult drops ordinary text-node
    // hits from a penetrating list. Slot and SVG text-content children are
    // retained because those parent elements may not contribute their own
    // hit-test background entry.
    parent.is_html_element("slot") || parent.is_svg_text_content_element()
}

fn element_for_list_hit_source(runtime: &JsContextHost, source: DomHandle) -> Option<DomHandle> {
    let node = runtime.dom_host().node(source)?;
    if !node.is_element() && !list_hit_accepts_non_element_source(runtime, source) {
        return None;
    }
    element_for_hit_source(runtime, source)
}

fn retarget_element_to_tree_scope(
    runtime: &JsContextHost,
    mut element: DomHandle,
    tree_scope: DomHandle,
) -> Option<DomHandle> {
    loop {
        let Some(root) = runtime.dom_host().containing_shadow_root(element) else {
            return Some(element);
        };
        if root == tree_scope {
            return Some(element);
        }
        element = runtime.dom_host().shadow_root_host(root)?;
    }
}

/// Whether a point query at `(x, y)` can hit anything: inside the viewport
/// and off its scrollbars, whose gutters the root's client size excludes.
fn point_is_inside_viewport(
    metrics: moli_layout::LayoutDocumentMetrics,
    root_client_size: Option<moli_layout::LayoutSize>,
    x: f64,
    y: f64,
) -> bool {
    x >= 0.0
        && y >= 0.0
        && x < f64::from(metrics.viewport.css_width)
        && y < f64::from(metrics.viewport.css_height)
        && root_client_size
            .is_none_or(|size| x < f64::from(size.width) && y < f64::from(size.height))
}

/// Answers `hit_queries` from one frozen-layout pass, together with whether
/// the point is inside the viewport's client area. `None` without a layout.
fn point_answers(
    runtime: &JsContextHost,
    document: DomHandle,
    x: f64,
    y: f64,
    hit_queries: Vec<moli_layout::LayoutQuery<DomHandle>>,
) -> Result<Option<(bool, Vec<moli_layout::LayoutQueryAnswer<DomHandle>>)>, moli_layout::LayoutError>
{
    use moli_layout::{LayoutQuery, LayoutQueryAnswer, LayoutQueryBatch};
    let root = document_root_element(runtime, document);
    let mut queries = vec![LayoutQuery::DocumentMetrics];
    queries.extend(root.map(|source| LayoutQuery::ElementMetrics { source }));
    let hit_count = hit_queries.len();
    queries.extend(hit_queries);
    let answers =
        match observable_geometry_batch(runtime, document, &LayoutQueryBatch::new(queries)) {
            Ok(answers) => answers,
            Err(moli_layout::LayoutError::NoLayoutSnapshot) => return Ok(None),
            Err(error) => return Err(error),
        };
    let mut answers = answers.answers.into_iter();
    let metrics = match answers.next() {
        Some(LayoutQueryAnswer::DocumentMetrics(metrics)) => Some(metrics),
        _ => None,
    };
    let root_client_size = match root.map(|_| answers.next()) {
        None => Some(None),
        Some(Some(LayoutQueryAnswer::ElementMetrics(metrics))) => {
            Some(metrics.map(|metrics| metrics.client_size))
        }
        Some(_) => None,
    };
    let hits = answers.collect::<Vec<_>>();
    match (metrics, root_client_size) {
        (Some(metrics), Some(root_client_size)) if hits.len() == hit_count => Ok(Some((
            point_is_inside_viewport(metrics, root_client_size, x, y),
            hits,
        ))),
        _ => Err(mismatched_point_answer()),
    }
}

fn mismatched_point_answer() -> moli_layout::LayoutError {
    moli_layout::LayoutError::source_contract(
        "renderer geometry provider",
        "returned a mismatched point hit test answer".to_owned(),
    )
}

/// Every hit under the point, front to back, including boxes with
/// `pointer-events: none`. Each hit reports its own `pointer-events`, so the
/// respecting answer is a filter of the same pass, and SVG content can still
/// re-enable hit testing below an outer `<svg>` that is itself transparent
/// to pointer events.
fn every_hit(
    answers: Vec<moli_layout::LayoutQueryAnswer<DomHandle>>,
) -> Result<Vec<moli_layout::LayoutHit<DomHandle>>, moli_layout::LayoutError> {
    match <[_; 1]>::try_from(answers) {
        Ok([moli_layout::LayoutQueryAnswer::HitTestAll(every)]) => Ok(every),
        _ => Err(mismatched_point_answer()),
    }
}

fn hit_test_all_query(
    x: f64,
    y: f64,
    ignore_pointer_events_none: bool,
) -> moli_layout::LayoutQuery<DomHandle> {
    moli_layout::LayoutQuery::HitTestAll {
        point: moli_layout::LayoutPoint::new(x as f32, y as f32),
        ignore_pointer_events_none,
    }
}

/// The outermost `<svg>` a hit lands on, with its content box.
fn outer_svg_hit(
    runtime: &JsContextHost,
    hit: &moli_layout::LayoutHit<DomHandle>,
) -> Option<moli_layout::LayoutRect> {
    let content_box = hit.local_content_box?;
    crate::context_bootstrap::svg_runtime::screen::is_outermost_svg(runtime, hit.source)
        .then_some(content_box)
}

/// SVG content below an outermost `<svg>` has no CSS box, so layout stops at
/// the `<svg>`. Resolve the shapes under the point from the SVG geometry,
/// foremost first. Where the geometry model is not exact, no content is
/// reported and the `<svg>` box answers on its own.
fn svg_content_hits(
    runtime: &JsContextHost,
    hit: &moli_layout::LayoutHit<DomHandle>,
    first_only: bool,
) -> Vec<DomHandle> {
    let Some(content_box) = outer_svg_hit(runtime, hit) else {
        return Vec::new();
    };
    crate::context_bootstrap::svg_runtime::hit_test::svg_content_hits_at_point(
        runtime,
        hit.source,
        [
            f64::from(content_box.x),
            f64::from(content_box.y),
            f64::from(content_box.width),
            f64::from(content_box.height),
        ],
        [f64::from(hit.local_point.x), f64::from(hit.local_point.y)],
        first_only,
    )
    .unwrap_or_default()
}

/// CSSOM View appends the document element to a hit list inside the viewport,
/// so a point over no box still yields the root.
fn document_root_element(runtime: &JsContextHost, document: DomHandle) -> Option<DomHandle> {
    runtime
        .dom_host()
        .dom()
        .document_element_handle_for_document(document)
}

fn element_at_point(
    runtime: &JsContextHost,
    document: DomHandle,
    tree_scope: DomHandle,
    x: f64,
    y: f64,
) -> Result<Option<DomHandle>, moli_layout::LayoutError> {
    use moli_layout::{LayoutQuery, LayoutQueryAnswer};
    // A single-point query intentionally keeps a foremost text hit and maps
    // it to its web-exposed parent element. This is observably different from
    // the penetrating-list filtering performed by `elements_at_point`.
    let point = moli_layout::LayoutPoint::new(x as f32, y as f32);
    let Some((inside, answers)) = point_answers(
        runtime,
        document,
        x,
        y,
        vec![
            LayoutQuery::HitTest {
                point,
                ignore_pointer_events_none: true,
            },
            LayoutQuery::HitTest {
                point,
                ignore_pointer_events_none: false,
            },
        ],
    )?
    else {
        return Ok(None);
    };
    if !inside {
        return Ok(None);
    }
    let Ok(
        [
            LayoutQueryAnswer::HitTest(foremost),
            LayoutQueryAnswer::HitTest(respecting),
        ],
    ) = <[_; 2]>::try_from(answers)
    else {
        return Err(mismatched_point_answer());
    };
    let identity = |hit: &moli_layout::LayoutHit<DomHandle>| {
        (
            hit.source,
            hit.fragment,
            hit.paint_order,
            hit.pointer_events,
        )
    };
    let element = if foremost.as_ref().map(identity) == respecting.as_ref().map(identity) {
        // Nothing transparent to pointer events lies above the respecting
        // hit, so it answers alone: its SVG content first, then itself.
        respecting.and_then(|hit| {
            svg_content_hits(runtime, &hit, true)
                .into_iter()
                .next()
                .or_else(|| element_for_hit_source(runtime, hit.source))
        })
    } else {
        // A box with `pointer-events: none` lies in front. Front to back, the
        // first box that respects pointer events wins unless SVG content
        // painted at or above it is hit first; content can re-enable
        // pointer events below a transparent `<svg>`.
        let Some((_, answers)) = point_answers(
            runtime,
            document,
            x,
            y,
            vec![hit_test_all_query(x, y, true)],
        )?
        else {
            return Ok(None);
        };
        let mut element = None;
        for hit in every_hit(answers)? {
            if let Some(shape) = svg_content_hits(runtime, &hit, true).into_iter().next() {
                element = Some(shape);
                break;
            }
            if hit.pointer_events {
                element = element_for_hit_source(runtime, hit.source);
                break;
            }
        }
        element
    };
    Ok(element
        .and_then(|element| retarget_element_to_tree_scope(runtime, element, tree_scope))
        .or_else(|| document_root_element(runtime, document)))
}

fn elements_at_point(
    runtime: &JsContextHost,
    document: DomHandle,
    tree_scope: DomHandle,
    x: f64,
    y: f64,
) -> Result<Vec<DomHandle>, moli_layout::LayoutError> {
    let Some((inside, answers)) = point_answers(
        runtime,
        document,
        x,
        y,
        vec![hit_test_all_query(x, y, true)],
    )?
    else {
        return Ok(Vec::new());
    };
    if !inside {
        return Ok(Vec::new());
    }

    // Front to back; SVG content sits directly above the `<svg>` box that
    // hosts it.
    let mut seen = HashSet::new();
    let mut elements = Vec::new();
    let mut push = |element: DomHandle, elements: &mut Vec<DomHandle>| {
        if let Some(element) = retarget_element_to_tree_scope(runtime, element, tree_scope)
            && seen.insert(element)
        {
            elements.push(element);
        }
    };
    for hit in every_hit(answers)? {
        for shape in svg_content_hits(runtime, &hit, false) {
            push(shape, &mut elements);
        }
        if hit.pointer_events
            && let Some(element) = element_for_list_hit_source(runtime, hit.source)
        {
            push(element, &mut elements);
        }
    }
    if let Some(root) = document_root_element(runtime, document) {
        push(root, &mut elements);
    }
    Ok(elements)
}

fn throw_hit_test_layout_error(scope: &mut v8::PinScope<'_, '_>, error: moli_layout::LayoutError) {
    let Some(message) = v8_string(scope, &format!("Layout failed: {error}")) else {
        return;
    };
    let exception = v8::Exception::error(scope, message);
    scope.throw_exception(exception);
}

fn element_array<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    receiver: v8::Local<'s, v8::Object>,
    elements: impl IntoIterator<Item = DomHandle>,
) -> Option<v8::Local<'s, v8::Array>> {
    let context = receiver.get_creation_context(scope)?;
    let scope = &mut v8::ContextScope::new(scope, context);
    let wrappers = elements
        .into_iter()
        .filter_map(|handle| node_wrapper_from_handle(scope, handle))
        .collect::<Vec<_>>();
    serialize_v8_array(scope, wrappers)
}

pub(in crate::native_bridge) fn node_document_element_from_point_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Ok((runtime_ptr, handle)) = document_runtime_and_handle_from_args(scope, &args) else {
        rv.set_null();
        return;
    };
    if !node_is_document(unsafe { &*runtime_ptr }, handle) {
        rv.set_null();
        return;
    }
    let Some(parsed) = webidl::parse_args::<DocumentElementFromPointArgs>(scope, &args) else {
        return;
    };
    let runtime = unsafe { &*runtime_ptr };
    match element_at_point(runtime, handle, handle, parsed.x, parsed.y) {
        Ok(element) => {
            set_wrapped_handle_or_null_for_receiver(
                scope,
                &mut rv,
                runtime_ptr,
                args.this(),
                element,
            );
        }
        Err(error) => {
            throw_hit_test_layout_error(scope, error);
            rv.set_null();
        }
    }
}

pub(in crate::native_bridge) fn node_document_elements_from_point_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Ok((runtime_ptr, handle)) = document_runtime_and_handle_from_args(scope, &args) else {
        rv.set(v8::Array::new(scope, 0).into());
        return;
    };
    if !node_is_document(unsafe { &*runtime_ptr }, handle) {
        rv.set(v8::Array::new(scope, 0).into());
        return;
    }
    let Some(parsed) = webidl::parse_args::<DocumentElementsFromPointArgs>(scope, &args) else {
        return;
    };
    let runtime = unsafe { &*runtime_ptr };
    match elements_at_point(runtime, handle, handle, parsed.x, parsed.y) {
        Ok(elements) => {
            let array = element_array(scope, args.this(), elements)
                .unwrap_or_else(|| v8::Array::new(scope, 0));
            rv.set(array.into());
        }
        Err(error) => {
            throw_hit_test_layout_error(scope, error);
            rv.set(v8::Array::new(scope, 0).into());
        }
    }
}

pub(in crate::native_bridge) fn node_document_caret_position_from_point_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Ok((runtime_ptr, handle)) = document_runtime_and_handle_from_args(scope, &args) else {
        rv.set_null();
        return;
    };
    if !node_is_document(unsafe { &*runtime_ptr }, handle) {
        rv.set_null();
        return;
    }
    let Some(parsed) = webidl::parse_args::<DocumentElementFromPointArgs>(scope, &args) else {
        return;
    };

    let runtime = unsafe { &*runtime_ptr };
    let Some(shadow_roots) = parse_caret_shadow_roots_argument(scope, runtime, &args) else {
        return;
    };
    match observable_caret_position(
        runtime,
        handle,
        moli_layout::LayoutPoint::new(parsed.x as f32, parsed.y as f32),
    ) {
        Ok(Some(position)) => {
            let (node, offset, rect) =
                caret_candidate_from_layout(runtime, &position, parsed.x, parsed.y);
            let (node, offset, rect) = retarget_caret_position(
                runtime,
                node,
                offset,
                rect,
                &shadow_roots,
                &position.ancestor_boxes,
            );
            if let Some(caret) = build_caret_position_object(scope, args.this(), node, offset, rect)
            {
                rv.set(caret.into());
            } else {
                rv.set_null();
            }
        }
        Ok(None) | Err(moli_layout::LayoutError::NoLayoutSnapshot) => rv.set_null(),
        Err(error) => {
            throw_hit_test_layout_error(scope, error);
            rv.set_null();
        }
    }
}

pub(in crate::native_bridge) fn node_shadow_root_element_from_point_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Ok((runtime_ptr, handle)) = node_runtime_and_handle_from_args(scope, &args) else {
        rv.set_null();
        return;
    };
    if !unsafe { &*runtime_ptr }.dom_host().is_shadow_root(handle) {
        rv.set_null();
        return;
    }
    let Some(parsed) = webidl::parse_args::<DocumentElementFromPointArgs>(scope, &args) else {
        return;
    };
    let runtime = unsafe { &*runtime_ptr };
    let Some(document) = runtime.dom_host().owner_document_handle(handle) else {
        rv.set_null();
        return;
    };
    match element_at_point(runtime, document, handle, parsed.x, parsed.y) {
        Ok(element) => {
            set_wrapped_handle_or_null_for_receiver(
                scope,
                &mut rv,
                runtime_ptr,
                args.this(),
                element,
            );
        }
        Err(error) => {
            throw_hit_test_layout_error(scope, error);
            rv.set_null();
        }
    }
}

pub(in crate::native_bridge) fn node_shadow_root_elements_from_point_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Ok((runtime_ptr, handle)) = node_runtime_and_handle_from_args(scope, &args) else {
        rv.set(v8::Array::new(scope, 0).into());
        return;
    };
    if !unsafe { &*runtime_ptr }.dom_host().is_shadow_root(handle) {
        rv.set(v8::Array::new(scope, 0).into());
        return;
    }
    let Some(parsed) = webidl::parse_args::<DocumentElementsFromPointArgs>(scope, &args) else {
        return;
    };
    let runtime = unsafe { &*runtime_ptr };
    let Some(document) = runtime.dom_host().owner_document_handle(handle) else {
        rv.set(v8::Array::new(scope, 0).into());
        return;
    };
    match elements_at_point(runtime, document, handle, parsed.x, parsed.y) {
        Ok(elements) => {
            let array = element_array(scope, args.this(), elements)
                .unwrap_or_else(|| v8::Array::new(scope, 0));
            rv.set(array.into());
        }
        Err(error) => {
            throw_hit_test_layout_error(scope, error);
            rv.set(v8::Array::new(scope, 0).into());
        }
    }
}
