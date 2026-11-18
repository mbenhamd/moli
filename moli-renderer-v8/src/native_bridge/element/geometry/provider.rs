use std::collections::HashSet;

use moli_layout::{
    LayoutAnswers, LayoutBoxModel, LayoutCaretPosition, LayoutDocumentMetrics,
    LayoutElementMetrics, LayoutError, LayoutFlushReason, LayoutHit, LayoutPoint, LayoutQuery,
    LayoutQueryAnswer, LayoutQueryBatch, LayoutResolvedGridTracks, LayoutScrollIntoViewGeometry,
};

use moli_svg::SvgMatrixComponents;

use super::client_rect::{ClientRect, union_client_rect, zero_client_rect};
use super::mock::{answer_queries as answer_mock_queries, answer_query as answer_mock_query};
use crate::{document_runtime::DomHandle, native_bridge::PageScriptHost};

fn query_source(
    runtime: &PageScriptHost,
    source: DomHandle,
    query: LayoutQuery<DomHandle>,
) -> Result<Option<LayoutQueryAnswer<DomHandle>>, LayoutError> {
    if !runtime.dom_host().is_connected(source) {
        return Ok(None);
    }
    let Some(document) = runtime.layout_document_for_source(source) else {
        return Ok(None);
    };
    match observable_geometry_query(runtime, document, &query) {
        Ok(answer) => Ok(Some(answer)),
        Err(LayoutError::NoLayoutSnapshot) => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) fn observable_geometry_query(
    runtime: &PageScriptHost,
    document: DomHandle,
    query: &LayoutQuery<DomHandle>,
) -> Result<LayoutQueryAnswer<DomHandle>, LayoutError> {
    runtime.ensure_current_layout()?;
    published_geometry_query(runtime, document, query)
}

/// Style sampling never initializes layout as a side effect.
fn published_geometry_query(
    runtime: &PageScriptHost,
    document: DomHandle,
    query: &LayoutQuery<DomHandle>,
) -> Result<LayoutQueryAnswer<DomHandle>, LayoutError> {
    if runtime.layout_policy().uses_real_layout() {
        runtime.answer_layout_query_for_document(document, query)
    } else {
        Ok(answer_mock_query(runtime, document, query))
    }
}

pub(crate) fn observable_geometry_batch(
    runtime: &PageScriptHost,
    document: DomHandle,
    batch: &LayoutQueryBatch<DomHandle>,
) -> Result<LayoutAnswers<DomHandle>, LayoutError> {
    runtime.ensure_current_layout()?;
    published_geometry_batch(runtime, document, batch)
}

/// Style and observer sampling never initializes layout as a side effect.
pub(crate) fn published_geometry_batch(
    runtime: &PageScriptHost,
    document: DomHandle,
    batch: &LayoutQueryBatch<DomHandle>,
) -> Result<LayoutAnswers<DomHandle>, LayoutError> {
    if runtime.layout_policy().uses_real_layout() {
        runtime.answer_layout_for_document(document, batch)
    } else {
        Ok(answer_mock_queries(
            runtime,
            document,
            LayoutFlushReason::SynchronousGeometry,
            batch,
        ))
    }
}

pub(crate) fn observable_document_metrics(
    runtime: &PageScriptHost,
    document: DomHandle,
) -> Result<LayoutDocumentMetrics, LayoutError> {
    match observable_geometry_query(runtime, document, &LayoutQuery::DocumentMetrics)? {
        LayoutQueryAnswer::DocumentMetrics(metrics) => Ok(metrics),
        _ => Err(provider_contract_error("document metrics")),
    }
}

/// SVG content below an outermost `<svg>` has no CSS box. It reports its
/// SVG bounding box mapped through the screen CTM onto the outer box.
/// `None` means `source` is not such content and layout answers directly.
fn read_svg_content_client_rects(
    runtime: &PageScriptHost,
    source: DomHandle,
) -> Result<Option<Vec<ClientRect>>, LayoutError> {
    use crate::context_bootstrap::svg_runtime::screen;
    let Some(root) = screen::svg_layout_viewport_root(runtime, source) else {
        return Ok(None);
    };
    let Some(root_box) = read_css_box_client_rects(runtime, root)?
        .into_iter()
        .reduce(union_client_rect)
    else {
        return Ok(Some(Vec::new()));
    };
    let bounds = screen::svg_client_bounds(
        runtime,
        source,
        root,
        [root_box.left, root_box.top, root_box.width, root_box.height],
    );
    Ok(Some(
        bounds
            .into_iter()
            .map(|[left, top, width, height]| ClientRect {
                left,
                top,
                right: left + width,
                bottom: top + height,
                width,
                height,
            })
            .collect(),
    ))
}

/// The client stroke bounds of SVG content below an outermost `<svg>`, which
/// has no CSS box model of its own. `None` when `source` is not such content
/// or is not rendered.
pub(crate) fn read_svg_content_stroke_bounds(
    runtime: &PageScriptHost,
    source: DomHandle,
) -> Result<Option<[f64; 4]>, LayoutError> {
    use crate::context_bootstrap::svg_runtime::screen;
    let Some(root) = screen::svg_layout_viewport_root(runtime, source) else {
        return Ok(None);
    };
    runtime.ensure_current_layout()?;
    let Some(root_box) = read_css_box_client_rects(runtime, root)?
        .into_iter()
        .reduce(union_client_rect)
    else {
        return Ok(None);
    };
    Ok(screen::svg_client_stroke_bounds(
        runtime,
        source,
        root,
        [root_box.left, root_box.top, root_box.width, root_box.height],
    ))
}

/// The outermost `<svg>` hosting `source` (`source` itself when it is one),
/// with that root's rendered viewport when it has a CSS box: its
/// untransformed content size and the affine map from its content box to
/// client coordinates (which carries CSS transforms on the root and its
/// ancestors). `None` when no outermost `<svg>` hosts `source`.
pub(crate) fn svg_viewport_root_geometry(
    runtime: &PageScriptHost,
    source: DomHandle,
) -> Option<(
    DomHandle,
    Option<crate::context_bootstrap::svg_runtime::screen::SvgRootViewport>,
)> {
    use crate::context_bootstrap::svg_runtime::screen;
    let root = if screen::is_outermost_svg(runtime, source) {
        source
    } else {
        screen::svg_layout_viewport_root(runtime, source)?
    };
    if !runtime.dom_host().is_connected(root) {
        return Some((root, None));
    }
    Some((root, svg_root_viewport(runtime, root)))
}

fn svg_root_viewport(
    runtime: &PageScriptHost,
    root: DomHandle,
) -> Option<crate::context_bootstrap::svg_runtime::screen::SvgRootViewport> {
    use crate::context_bootstrap::svg_runtime::screen::{SvgRootViewport, svg_root_content_box};
    let (Some(metrics), Some(model)) = (
        read_element_metrics(runtime, root).ok().flatten(),
        read_box_model(runtime, root).ok().flatten(),
    ) else {
        // Without box-model geometry (an `<svg>` laid out in a
        // `<foreignObject>` content pass), the client border box is the
        // untransformed viewport.
        let border_box = read_css_box_client_rects(runtime, root)
            .ok()?
            .into_iter()
            .reduce(union_client_rect)?;
        let [x, y, width, height] = svg_root_content_box(
            runtime,
            root,
            [
                border_box.left,
                border_box.top,
                border_box.width,
                border_box.height,
            ],
        );
        return Some(SvgRootViewport {
            width,
            height,
            content_to_client: SvgMatrixComponents::translate(x, y),
        });
    };
    let width = f64::from(metrics.content_size.width);
    let height = f64::from(metrics.content_size.height);
    let [origin, top_right, _, bottom_left] = model
        .content
        .points
        .map(|point| (f64::from(point.x), f64::from(point.y)));
    // The content quad is the content box mapped through every transform up
    // to the viewport; its edges recover that affine map. An empty edge
    // carries no direction, so it keeps the unit axis.
    let axis = |to: (f64, f64), length: f64, unit: (f64, f64)| {
        if length > 0.0 {
            ((to.0 - origin.0) / length, (to.1 - origin.1) / length)
        } else {
            unit
        }
    };
    let (a, b) = axis(top_right, width, (1.0, 0.0));
    let (c, d) = axis(bottom_left, height, (0.0, 1.0));
    Some(SvgRootViewport {
        width,
        height,
        content_to_client: SvgMatrixComponents {
            a,
            b,
            c,
            d,
            e: origin.0,
            f: origin.1,
        },
    })
}

pub(crate) fn read_client_rects(
    runtime: &PageScriptHost,
    source: DomHandle,
) -> Result<Vec<ClientRect>, LayoutError> {
    // Real layout publishes SVG object geometry with the scene, so its answer
    // stays frozen with every other read of that publication. The screen-CTM
    // computation from live attributes only covers SVG content the
    // publication has no geometry for.
    if runtime.layout_policy().uses_real_layout() && screen_svg_content(runtime, source) {
        let published = read_layout_client_rects(runtime, source)?;
        if !published.is_empty() {
            return Ok(published);
        }
    }
    if let Some(rects) = read_svg_content_client_rects(runtime, source)? {
        return Ok(rects);
    }
    read_css_box_client_rects(runtime, source)
}

fn screen_svg_content(runtime: &PageScriptHost, source: DomHandle) -> bool {
    crate::context_bootstrap::svg_runtime::screen::svg_layout_viewport_root(runtime, source)
        .is_some()
}

/// Client rects of a source that has CSS boxes: in the Document's pass, or,
/// below a `<foreignObject>`, in that element's own content pass.
fn read_css_box_client_rects(
    runtime: &PageScriptHost,
    source: DomHandle,
) -> Result<Vec<ClientRect>, LayoutError> {
    if let Some(rects) = read_foreign_object_content_client_rects(runtime, source)? {
        return Ok(rects);
    }
    read_layout_client_rects(runtime, source)
}

/// The `<foreignObject>` hosting `source`'s CSS boxes, with the transform
/// from its content space to client coordinates. `Ok(None)` when `source`
/// is not such content or layout is mocked. The transform is `None` when
/// the outermost `<svg>` has no box, so the content is not rendered.
fn foreign_object_layout_host(
    runtime: &PageScriptHost,
    source: DomHandle,
) -> Result<Option<(DomHandle, Option<SvgMatrixComponents>)>, LayoutError> {
    use crate::context_bootstrap::svg_runtime::screen;
    if !runtime.layout_policy().uses_real_layout() {
        return Ok(None);
    }
    let Some((foreign_object, root)) = screen::svg_foreign_object_host(runtime, source) else {
        return Ok(None);
    };
    let transform = read_css_box_client_rects(runtime, root)?
        .into_iter()
        .reduce(union_client_rect)
        .map(|root_box| {
            screen::svg_foreign_object_content_transform(
                runtime,
                foreign_object,
                root,
                [root_box.left, root_box.top, root_box.width, root_box.height],
            )
        });
    Ok(Some((foreign_object, transform)))
}

/// Client rects of CSS content below a `<foreignObject>`: its rects in the
/// content pass, each mapped through the SVG screen CTM to the bounding box
/// of the transformed quad, as Blink reports them.
fn read_foreign_object_content_client_rects(
    runtime: &PageScriptHost,
    source: DomHandle,
) -> Result<Option<Vec<ClientRect>>, LayoutError> {
    let Some((foreign_object, transform)) = foreign_object_layout_host(runtime, source)? else {
        return Ok(None);
    };
    let Some(transform) = transform else {
        return Ok(Some(Vec::new()));
    };
    let rects = match runtime
        .answer_foreign_object_layout_query(foreign_object, &LayoutQuery::ClientRects { source })?
    {
        Some(LayoutQueryAnswer::ClientRects(rects)) => rects,
        None => return Ok(Some(Vec::new())),
        _ => return Err(provider_contract_error("foreignObject client rects")),
    };
    Ok(Some(
        rects
            .into_iter()
            .map(|rect| transformed_client_rect(ClientRect::from(rect), transform))
            .collect(),
    ))
}

fn transformed_client_rect(rect: ClientRect, transform: SvgMatrixComponents) -> ClientRect {
    let corners = [
        (rect.left, rect.top),
        (rect.right, rect.top),
        (rect.left, rect.bottom),
        (rect.right, rect.bottom),
    ]
    .map(|(x, y)| {
        (
            transform.a * x + transform.c * y + transform.e,
            transform.b * x + transform.d * y + transform.f,
        )
    });
    let left = corners
        .iter()
        .map(|(x, _)| *x)
        .fold(f64::INFINITY, f64::min);
    let right = corners
        .iter()
        .map(|(x, _)| *x)
        .fold(f64::NEG_INFINITY, f64::max);
    let top = corners
        .iter()
        .map(|(_, y)| *y)
        .fold(f64::INFINITY, f64::min);
    let bottom = corners
        .iter()
        .map(|(_, y)| *y)
        .fold(f64::NEG_INFINITY, f64::max);
    ClientRect {
        left,
        top,
        right,
        bottom,
        width: right - left,
        height: bottom - top,
    }
}

fn read_layout_client_rects(
    runtime: &PageScriptHost,
    source: DomHandle,
) -> Result<Vec<ClientRect>, LayoutError> {
    match query_source(runtime, source, LayoutQuery::ClientRects { source })? {
        Some(LayoutQueryAnswer::ClientRects(rects)) => {
            Ok(rects.into_iter().map(ClientRect::from).collect())
        }
        None => Ok(Vec::new()),
        _ => Err(provider_contract_error("client rects")),
    }
}

pub(crate) fn observable_sources_with_fragments(
    runtime: &PageScriptHost,
    document: DomHandle,
    sources: &[DomHandle],
) -> Result<HashSet<DomHandle>, LayoutError> {
    if sources.is_empty() {
        return Ok(HashSet::new());
    }
    let queries = sources
        .iter()
        .copied()
        .map(|source| LayoutQuery::ContentQuads { source })
        .collect();
    let answers = observable_geometry_batch(runtime, document, &LayoutQueryBatch::new(queries))?;
    if answers.answers.len() != sources.len() {
        return Err(provider_contract_error("rendered source fragment"));
    }
    let mut rendered = HashSet::new();
    for (source, answer) in sources.iter().copied().zip(answers.answers) {
        match answer {
            LayoutQueryAnswer::ContentQuads(quads) => {
                if !quads.is_empty() {
                    rendered.insert(source);
                }
            }
            _ => return Err(provider_contract_error("rendered source fragment")),
        }
    }
    Ok(rendered)
}

pub(crate) fn read_bounding_client_rect(
    runtime: &PageScriptHost,
    source: DomHandle,
) -> Result<ClientRect, LayoutError> {
    let mut rects = read_client_rects(runtime, source)?.into_iter();
    let Some(mut bounds) = rects.next() else {
        return Ok(zero_client_rect());
    };
    for rect in rects {
        bounds = union_client_rect(bounds, rect);
    }
    Ok(bounds)
}

pub(crate) fn observable_bounding_client_rects(
    runtime: &PageScriptHost,
    sources: &[DomHandle],
) -> Result<Vec<ClientRect>, LayoutError> {
    let Some((&first, rest)) = sources.split_first() else {
        return Ok(Vec::new());
    };
    if !runtime.dom_host().is_connected(first) {
        return Ok(vec![zero_client_rect(); sources.len()]);
    }
    let Some(document) = runtime.layout_document_for_source(first) else {
        return Ok(vec![zero_client_rect(); sources.len()]);
    };
    if rest.iter().any(|source| {
        runtime.layout_document_for_source(*source) != Some(document)
            || !runtime.dom_host().is_connected(*source)
    }) {
        return Err(LayoutError::source_contract(
            "geometry batch",
            "bounding-client-rect sources do not share one connected document",
        ));
    }
    let queries = sources
        .iter()
        .copied()
        .map(|source| LayoutQuery::ClientRects { source })
        .collect();
    if sources.iter().any(|source| {
        crate::context_bootstrap::svg_runtime::screen::svg_layout_viewport_root(runtime, *source)
            .is_some()
            || crate::context_bootstrap::svg_runtime::screen::svg_foreign_object_host(
                runtime, *source,
            )
            .is_some()
    }) {
        return sources
            .iter()
            .map(|source| read_bounding_client_rect(runtime, *source))
            .collect();
    }
    let answers =
        match observable_geometry_batch(runtime, document, &LayoutQueryBatch::new(queries)) {
            Ok(answers) => answers,
            Err(LayoutError::NoLayoutSnapshot) => {
                return Ok(vec![zero_client_rect(); sources.len()]);
            }
            Err(error) => return Err(error),
        };
    if answers.answers.len() != sources.len() {
        return Err(provider_contract_error("bounding client rects"));
    }
    answers
        .answers
        .into_iter()
        .map(|answer| match answer {
            LayoutQueryAnswer::ClientRects(rects) => Ok(rects
                .into_iter()
                .map(ClientRect::from)
                .reduce(union_client_rect)
                .unwrap_or_else(zero_client_rect)),
            _ => Err(provider_contract_error("bounding client rects")),
        })
        .collect()
}

pub(crate) fn read_box_model(
    runtime: &PageScriptHost,
    source: DomHandle,
) -> Result<Option<LayoutBoxModel>, LayoutError> {
    match query_source(runtime, source, LayoutQuery::BoxModel { source })? {
        Some(LayoutQueryAnswer::BoxModel(model)) => Ok(model),
        None => Ok(None),
        _ => Err(provider_contract_error("box model")),
    }
}

pub(crate) fn observable_event_offset(
    runtime: &PageScriptHost,
    source: DomHandle,
    point: LayoutPoint,
) -> Result<LayoutPoint, LayoutError> {
    match query_source(runtime, source, LayoutQuery::EventOffset { source, point })? {
        Some(LayoutQueryAnswer::EventOffset(offset)) => Ok(offset.unwrap_or(point)),
        None => Ok(point),
        _ => Err(provider_contract_error("event offset")),
    }
}

pub(crate) fn read_element_metrics(
    runtime: &PageScriptHost,
    source: DomHandle,
) -> Result<Option<LayoutElementMetrics<DomHandle>>, LayoutError> {
    if let Some((foreign_object, transform)) = foreign_object_layout_host(runtime, source)? {
        // Box metrics are untransformed CSS sizes, so they come straight from
        // the content pass; unrendered content has none.
        if transform.is_none() {
            return Ok(None);
        }
        return match runtime.answer_foreign_object_layout_query(
            foreign_object,
            &LayoutQuery::ElementMetrics { source },
        )? {
            Some(LayoutQueryAnswer::ElementMetrics(metrics)) => Ok(metrics),
            None => Ok(None),
            _ => Err(provider_contract_error("foreignObject element metrics")),
        };
    }
    match query_source(runtime, source, LayoutQuery::ElementMetrics { source })? {
        Some(LayoutQueryAnswer::ElementMetrics(metrics)) => Ok(metrics),
        None => Ok(None),
        _ => Err(provider_contract_error("element metrics")),
    }
}

pub(crate) fn observable_used_grid_tracks(
    runtime: &PageScriptHost,
    source: DomHandle,
) -> Result<Option<LayoutResolvedGridTracks>, LayoutError> {
    if !runtime.dom_host().is_connected(source) {
        return Ok(None);
    }
    let Some(document) = runtime.layout_document_for_source(source) else {
        return Ok(None);
    };
    let answer = match published_geometry_query(
        runtime,
        document,
        &LayoutQuery::UsedGridTracks { source },
    ) {
        Ok(answer) => answer,
        Err(LayoutError::NoLayoutSnapshot) => return Ok(None),
        Err(error) => return Err(error),
    };
    match answer {
        LayoutQueryAnswer::UsedGridTracks(tracks) => Ok(tracks),
        _ => Err(provider_contract_error("used Grid tracks")),
    }
}

pub(crate) fn read_scroll_into_view_geometry(
    runtime: &PageScriptHost,
    source: DomHandle,
) -> Result<Option<LayoutScrollIntoViewGeometry<DomHandle>>, LayoutError> {
    match query_source(
        runtime,
        source,
        LayoutQuery::ScrollIntoViewGeometry { source },
    )? {
        Some(LayoutQueryAnswer::ScrollIntoViewGeometry(geometry)) => Ok(geometry),
        None => Ok(None),
        _ => Err(provider_contract_error("scroll-into-view geometry")),
    }
}

pub(crate) fn observable_caret_position(
    runtime: &PageScriptHost,
    document: DomHandle,
    point: LayoutPoint,
) -> Result<Option<LayoutCaretPosition<DomHandle>>, LayoutError> {
    let answers = observable_geometry_batch(
        runtime,
        document,
        &LayoutQueryBatch::new(vec![
            LayoutQuery::DocumentMetrics,
            LayoutQuery::CaretPosition { point },
        ]),
    )?;
    let mut answers = answers.answers.into_iter();
    match (answers.next(), answers.next()) {
        (
            Some(LayoutQueryAnswer::DocumentMetrics(metrics)),
            Some(LayoutQueryAnswer::CaretPosition(position)),
        ) => {
            let inside_viewport = point.x >= 0.0
                && point.y >= 0.0
                && point.x < metrics.viewport.css_width as f32
                && point.y < metrics.viewport.css_height as f32;
            Ok(inside_viewport.then_some(position).flatten())
        }
        _ => Err(provider_contract_error("caret position")),
    }
}

pub(crate) fn observable_hit_test_all(
    runtime: &PageScriptHost,
    document: DomHandle,
    point: LayoutPoint,
    ignore_pointer_events_none: bool,
) -> Result<(LayoutDocumentMetrics, Vec<LayoutHit<DomHandle>>), LayoutError> {
    let answers = observable_geometry_batch(
        runtime,
        document,
        &LayoutQueryBatch::new(vec![
            LayoutQuery::DocumentMetrics,
            LayoutQuery::HitTestAll {
                point,
                ignore_pointer_events_none,
            },
        ]),
    )?;
    let mut answers = answers.answers.into_iter();
    match (answers.next(), answers.next()) {
        (
            Some(LayoutQueryAnswer::DocumentMetrics(metrics)),
            Some(LayoutQueryAnswer::HitTestAll(hits)),
        ) => Ok((metrics, hits)),
        _ => Err(provider_contract_error("complete hit test")),
    }
}

pub(super) fn provider_contract_error(answer: &str) -> LayoutError {
    LayoutError::source_contract(
        "renderer geometry provider",
        format!("returned a mismatched {answer} answer"),
    )
}
