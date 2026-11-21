mod inline_svg;
#[cfg(test)]
pub(crate) use inline_svg::parse_count as svg_parse_count;
mod source_view;
mod style_resolver;

use std::collections::HashMap;

use moli_layout::{
    DocumentLayoutServices, EmbeddedFrameRenderer, LayoutPassPreparation, LayoutPassRequest,
    LayoutPassResult, LayoutPassStep, LayoutViewport, PreparedLayoutPass,
    finish_prepared_layout_pass, verify_incremental_layout_pass,
};

use crate::{document_runtime::DomHandle, native_bridge::PageScriptHost};

/// Kill switch for retained-world relayout under the live-geometry policy.
fn incremental_layout_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("MOLI_LAYOUT_INCREMENTAL").is_none_or(|value| value != "0")
    })
}

pub(crate) fn build_native_layout_pass(
    runtime: &PageScriptHost,
    root: DomHandle,
    services: &mut DocumentLayoutServices,
    embedded_document_services: &mut HashMap<DomHandle, DocumentLayoutServices>,
    request: LayoutPassRequest,
    environment_override: Option<crate::style_engine::StyloStyleEnvironment>,
) -> Result<LayoutPassResult<DomHandle>, moli_layout::LayoutError> {
    let mut document_stack = Vec::new();
    build_native_layout_pass_recursive(
        runtime,
        root,
        services,
        embedded_document_services,
        request,
        environment_override,
        &mut document_stack,
        false,
    )
}

/// The style observation `build_foreign_object_layout_pass` reads `document`
/// in for `viewport`.
pub(crate) fn foreign_object_style_scope(
    runtime: &PageScriptHost,
    document: DomHandle,
    viewport: moli_layout::LayoutViewport,
) -> crate::native_bridge::element::StyleObservation<'_> {
    style_resolver::layout_style_read_scope(runtime, document, viewport, None)
}

/// Lays out the HTML content of one `<foreignObject>` as its own block root
/// in its Document's current style world.
///
/// The outer `<svg>` is a replaced box in the Document's pass, so content
/// below it has no CSS box there. This one-shot pass gives that content the
/// CSS geometry Blink computes for it, in the `<foreignObject>`'s local
/// coordinate space (CSS pixels before the SVG screen CTM). It never
/// publishes, keeps the Document's retained world, and does not update the
/// Document's container-query sizes.
pub(crate) fn build_foreign_object_layout_pass(
    runtime: &PageScriptHost,
    foreign_object: DomHandle,
    services: &mut DocumentLayoutServices,
    embedded_document_services: &mut HashMap<DomHandle, DocumentLayoutServices>,
    request: LayoutPassRequest,
) -> Result<LayoutPassResult<DomHandle>, moli_layout::LayoutError> {
    let mut document_stack = Vec::new();
    services.with_retained_world_set_aside(|services| {
        build_native_layout_pass_recursive(
            runtime,
            foreign_object,
            services,
            embedded_document_services,
            request,
            None,
            &mut document_stack,
            true,
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn build_native_layout_pass_recursive(
    runtime: &PageScriptHost,
    root: DomHandle,
    services: &mut DocumentLayoutServices,
    embedded_document_services: &mut HashMap<DomHandle, DocumentLayoutServices>,
    request: LayoutPassRequest,
    environment_override: Option<crate::style_engine::StyloStyleEnvironment>,
    document_stack: &mut Vec<DomHandle>,
    foreign_object_root: bool,
) -> Result<LayoutPassResult<DomHandle>, moli_layout::LayoutError> {
    let profile_started = moli_trace::cpu_profile_enabled().then(std::time::Instant::now);
    let document = runtime
        .dom_host()
        .owner_document_handle(root)
        .unwrap_or_else(|| runtime.document_handle());
    let mut request = request.with_environment(moli_layout::LayoutEnvironment {
        scrollbars_hidden: runtime.scrollbars_hidden(),
    });
    let one_shot_request = request;
    // Only live relayout retains a world; print and media overrides resolve a
    // different style world, and the frozen policies stay one-shot.
    if runtime.layout_policy().relayouts_stale_reads()
        && environment_override.is_none()
        && !foreign_object_root
        && incremental_layout_enabled()
    {
        request = request.with_retained_world();
    }
    document_stack.push(document);
    let source = source_view::NativeLayoutSourceView::with_paint_resources(
        runtime,
        root,
        style_resolver::layout_style_read_scope(
            runtime,
            document,
            request.viewport,
            environment_override,
        ),
        request.requests_paint(),
    );
    let source_ready = profile_started.map(|_| std::time::Instant::now());
    let mut styles_ready = None;
    let container_query_store = style_resolver::ContainerQueryStoreScope::begin(
        runtime,
        document,
        environment_override.is_some(),
        foreign_object_root,
    );
    let mut resources = None;
    let result = (|| {
        let styles = NativeLayoutStyleScope {
            runtime,
            root,
            document,
            environment_override,
            foreign_object_root,
        };
        let prepared = styles.prepare(
            &source,
            services,
            request,
            &mut resources,
            &mut styles_ready,
        )?;
        let used_retained_world = prepared.used_retained_world();
        let applied_sizes = prepared.applied_query_container_sizes().map(<[_]>::to_vec);
        let mut frames = NativeEmbeddedFrameRenderer {
            runtime,
            reason: request.reason,
            capture_paint: request.requests_paint(),
            include_backgrounds: request.includes_backgrounds(),
            environment_override,
            document_stack,
            embedded_document_services,
        };
        with_layout_fonts(services, resources.as_ref(), |services| {
            let result = finish_prepared_layout_pass(prepared, &source, services, &mut frames)?;
            verify_incremental_layout_pass(&result, services, used_retained_world, |services| {
                // Embedded frames verify in their own recursive passes, so the
                // check renders no frames and compares this document's geometry.
                let mut full_resources = None;
                let full = styles
                    .prepare(
                        &source,
                        services,
                        one_shot_request,
                        &mut full_resources,
                        &mut None,
                    )
                    .and_then(|prepared| {
                        with_layout_fonts(services, full_resources.as_ref(), |services| {
                            finish_prepared_layout_pass(
                                prepared,
                                &source,
                                services,
                                &mut NoEmbeddedFrames,
                            )
                        })
                    });
                // The full pass ran its own container-query loop. Put back the
                // sizes the incremental pass ended with so later passes start
                // from the same state as without the check.
                if let Some(sizes) = &applied_sizes
                    && !foreign_object_root
                {
                    style_resolver::store_query_container_sizes(runtime, document, sizes);
                }
                full
            });
            Ok(result)
        })
    })();
    drop(container_query_store);
    if let (Some(started), Some(source_ready), Some(styles_ready)) =
        (profile_started, source_ready, styles_ready)
    {
        let finished = std::time::Instant::now();
        tracing::info!(
            target: "moli_cpu_profile",
            stage = "layout_native_adapter",
            document = document.index_u32(),
            root = ?root,
            reason = ?request.reason,
            total_us = finished.duration_since(started).as_micros(),
            source_setup_us = source_ready.duration_since(started).as_micros(),
            style_setup_us = styles_ready.duration_since(source_ready).as_micros(),
            backend_us = finished.duration_since(styles_ready).as_micros(),
            backend_reported_us = result.as_ref().map(|pass| pass.metrics.elapsed.as_micros()).ok(),
            success = result.is_ok(),
        );
    }
    document_stack.pop();
    result
}

/// The style inputs of one native layout demand. Each container-query
/// iteration resolves its styles in a fresh style resolution pass.
#[derive(Clone, Copy)]
struct NativeLayoutStyleScope<'a> {
    runtime: &'a PageScriptHost,
    root: DomHandle,
    document: DomHandle,
    environment_override: Option<crate::style_engine::StyloStyleEnvironment>,
    foreign_object_root: bool,
}

impl NativeLayoutStyleScope<'_> {
    /// Builds and lays out worlds until container-query sizes converge. The
    /// stylesheet font manifest is read once, in the first resolution pass,
    /// and every iteration shapes text under its declarations.
    fn prepare(
        &self,
        source: &source_view::NativeLayoutSourceView<'_>,
        services: &mut DocumentLayoutServices,
        request: LayoutPassRequest,
        resources: &mut Option<crate::style_engine::StylesheetResourceSnapshot>,
        styles_ready: &mut Option<std::time::Instant>,
    ) -> Result<PreparedLayoutPass<DomHandle>, moli_layout::LayoutError> {
        let mut preparation: Option<LayoutPassPreparation<DomHandle>> = None;
        loop {
            let step = style_resolver::with_layout_root_styles(
                self.runtime,
                self.root,
                self.document,
                request.viewport,
                self.environment_override,
                self.foreign_object_root,
                |styles| -> Result<_, moli_layout::LayoutError> {
                    if styles_ready.is_none() && moli_trace::cpu_profile_enabled() {
                        *styles_ready = Some(std::time::Instant::now());
                    }
                    if resources.is_none() && services.has_stylesheet_web_fonts() {
                        *resources = Some(
                            self.runtime
                                .stylesheet_resource_snapshot_for_document(self.document)
                                .ok_or_else(|| {
                                    moli_layout::LayoutError::source_contract(
                                        "layout fonts",
                                        "missing current stylesheet resource manifest",
                                    )
                                })?,
                        );
                    }
                    with_layout_fonts(services, resources.as_ref(), |services| {
                        preparation
                            .take()
                            .unwrap_or_else(|| LayoutPassPreparation::new(services, request))
                            .prepare(source, styles, services)
                    })
                },
            )?;
            match step {
                LayoutPassStep::Ready(prepared) => return Ok(*prepared),
                LayoutPassStep::Restyle(next) => preparation = Some(next),
            }
        }
    }
}

/// Runs `consume` with this pass's stylesheet `@font-face` declarations
/// selected, when the Document has stylesheet web fonts.
fn with_layout_fonts<R>(
    services: &mut DocumentLayoutServices,
    resources: Option<&crate::style_engine::StylesheetResourceSnapshot>,
    consume: impl FnOnce(&mut DocumentLayoutServices) -> R,
) -> R {
    let Some(resources) = resources else {
        return consume(services);
    };
    services.with_stylesheet_font_declarations(
        resources.web_fonts().iter().filter_map(|resource| {
            let font = resource.web_font()?;
            Some(moli_layout::WebFontDeclaration {
                slot: font.registration_slot(),
                source_key: font.resource_key(),
                face: font.face(),
            })
        }),
        consume,
    )
}

/// The incremental-layout check renders no embedded frames.
struct NoEmbeddedFrames;

impl EmbeddedFrameRenderer<DomHandle> for NoEmbeddedFrames {
    fn render_embedded_frame(
        &mut self,
        _frame: DomHandle,
        _viewport: LayoutViewport,
    ) -> Result<Option<moli_layout::EmbeddedFrameSnapshot<DomHandle>>, moli_layout::LayoutError>
    {
        Ok(None)
    }
}

struct NativeEmbeddedFrameRenderer<'a> {
    runtime: &'a PageScriptHost,
    reason: moli_layout::LayoutFlushReason,
    capture_paint: bool,
    include_backgrounds: bool,
    environment_override: Option<crate::style_engine::StyloStyleEnvironment>,
    document_stack: &'a mut Vec<DomHandle>,
    embedded_document_services: &'a mut HashMap<DomHandle, DocumentLayoutServices>,
}

impl EmbeddedFrameRenderer<DomHandle> for NativeEmbeddedFrameRenderer<'_> {
    fn render_embedded_frame(
        &mut self,
        frame: DomHandle,
        viewport: LayoutViewport,
    ) -> Result<Option<moli_layout::EmbeddedFrameSnapshot<DomHandle>>, moli_layout::LayoutError>
    {
        const MAX_EMBEDDED_DOCUMENT_DEPTH: usize = 32;

        let Some(document) = self.runtime.child_browsing_context_document_handle(frame) else {
            return Ok(None);
        };
        if self.document_stack.len() >= MAX_EMBEDDED_DOCUMENT_DEPTH
            || self.document_stack.contains(&document)
        {
            return Ok(None);
        }
        let Some(root) = self
            .runtime
            .dom_host()
            .dom()
            .document_element_handle_for_document(document)
        else {
            return Ok(None);
        };
        let mut services = self
            .embedded_document_services
            .remove(&document)
            .unwrap_or_default();
        let request = if self.capture_paint {
            let mut capture = moli_layout::PaintCaptureRequest::viewport();
            capture.include_backgrounds = self.include_backgrounds;
            capture.base_background_color = moli_layout::PaintColor::TRANSPARENT;
            LayoutPassRequest::with_capture(viewport, self.reason, capture)
        } else {
            LayoutPassRequest::new(viewport, self.reason)
        };
        let result = build_native_layout_pass_recursive(
            self.runtime,
            root,
            &mut services,
            self.embedded_document_services,
            request,
            self.environment_override,
            self.document_stack,
            false,
        );
        self.embedded_document_services.insert(document, services);
        let output = result?;
        let (tree, paint, css_image_references) = output.into_embedded_parts();
        Ok(Some(moli_layout::EmbeddedFrameSnapshot::new(
            tree,
            paint,
            css_image_references,
        )))
    }
}

#[cfg(test)]
pub(crate) fn build_normalized_native_box_tree_for_test(
    runtime: &PageScriptHost,
    root: DomHandle,
) -> Result<moli_layout::NormalizedBoxTree, moli_layout::LayoutError> {
    let document = runtime
        .dom_host()
        .owner_document_handle(root)
        .unwrap_or_else(|| runtime.document_handle());
    let viewport = runtime.layout_viewport_for_document(document);
    let source = source_view::NativeLayoutSourceView::new(runtime, root);
    style_resolver::with_layout_styles(runtime, document, viewport, None, |styles| {
        moli_layout::build_layout_world(&source, styles).map(|world| world.normalized_tree())
    })
}
