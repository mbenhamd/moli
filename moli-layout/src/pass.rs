use crate::{
    DocumentLayoutServices, FrozenLayoutTree, LayoutError, LayoutFlushReason, LayoutPassResult,
    LayoutScrollbarAxis, LayoutSource, LayoutStyleResolver, LayoutViewport, PaintCaptureRequest,
    PaintSnapshot, PaintViewport, build_layout_world,
    form::prepare_form_controls,
    inline::prepare_inline_contexts,
    list::prepare_list_markers,
    overflow::OverflowProjection,
    projection::finish_layout_pass,
    taffy_tree::{compute_prepared_world_layout, prepare_world_layout},
};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::retention::{RetainedLayout, diff_worlds, record_verify_comparison};

/// Owned products for one embedded browsing context in the same synchronous
/// layout demand as its parent.
pub struct EmbeddedFrameSnapshot<N>
where
    N: Copy + std::fmt::Debug + Eq + std::hash::Hash,
{
    pub tree: FrozenLayoutTree<N>,
    pub paint: Option<PaintSnapshot>,
    pub css_image_references: Vec<crate::LayoutCssImageReference<N>>,
}

impl<N> EmbeddedFrameSnapshot<N>
where
    N: Copy + std::fmt::Debug + Eq + std::hash::Hash,
{
    #[must_use]
    pub const fn new(
        tree: FrozenLayoutTree<N>,
        paint: Option<PaintSnapshot>,
        css_image_references: Vec<crate::LayoutCssImageReference<N>>,
    ) -> Self {
        Self {
            tree,
            paint,
            css_image_references,
        }
    }
}

/// Renderer-owned bridge for one live embedded browsing context.
///
/// The parent numeric layout has already completed when this callback runs, so
/// `viewport` is the iframe's used content-box size rather than an attribute or
/// computed-style estimate. Implementations must return an owned, source-free
/// snapshot and must not run JavaScript, lifecycle work, or an event-loop turn.
/// Recursive child layout is allowed because every nested world remains local
/// to the same synchronous demand. The child tree is consumed into the single
/// parent-owned frozen snapshot; it is not a separately retained cache entry.
pub trait EmbeddedFrameRenderer<N>
where
    N: Copy + std::fmt::Debug + Eq + std::hash::Hash,
{
    fn render_embedded_frame(
        &mut self,
        frame: N,
        viewport: LayoutViewport,
    ) -> Result<Option<EmbeddedFrameSnapshot<N>>, LayoutError>;
}

struct NoEmbeddedFrames;

impl<N> EmbeddedFrameRenderer<N> for NoEmbeddedFrames
where
    N: Copy + std::fmt::Debug + Eq + std::hash::Hash,
{
    fn render_embedded_frame(
        &mut self,
        _frame: N,
        _viewport: LayoutViewport,
    ) -> Result<Option<EmbeddedFrameSnapshot<N>>, LayoutError> {
        Ok(None)
    }
}

/// Inputs for one complete, synchronous layout demand.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutPassRequest {
    pub viewport: LayoutViewport,
    pub reason: LayoutFlushReason,
    environment: crate::LayoutEnvironment,
    paint_capture: Option<PaintCaptureRequest>,
    retain_world: bool,
}

impl LayoutPassRequest {
    pub const fn new(viewport: LayoutViewport, reason: LayoutFlushReason) -> Self {
        Self {
            viewport,
            reason,
            environment: crate::LayoutEnvironment {
                scrollbars_hidden: false,
            },
            paint_capture: None,
            retain_world: false,
        }
    }

    /// Creates a demand that also projects the same world into owned paint input.
    pub const fn with_paint(viewport: LayoutViewport, reason: LayoutFlushReason) -> Self {
        Self::with_capture(viewport, reason, PaintCaptureRequest::viewport())
    }

    /// Creates a demand that projects a separate one-shot capture surface.
    pub const fn with_capture(
        viewport: LayoutViewport,
        reason: LayoutFlushReason,
        paint_capture: PaintCaptureRequest,
    ) -> Self {
        let mut request = Self::new(viewport, reason);
        request.paint_capture = Some(paint_capture);
        request
    }

    /// Sets document-wide runtime policy without changing authored CSS styles.
    pub const fn with_environment(mut self, environment: crate::LayoutEnvironment) -> Self {
        self.environment = environment;
        self
    }

    /// Keeps this pass's world for the next one so unchanged boxes reuse their
    /// shaped text and numeric results. Only the live-geometry policy asks
    /// for this; every other pass stays one-shot.
    pub const fn with_retained_world(mut self) -> Self {
        self.retain_world = true;
        self
    }

    /// Whether this demand also needs immutable software-paint input.
    pub const fn requests_paint(self) -> bool {
        self.paint_capture.is_some()
    }

    /// Whether paint snapshots for this demand should include CSS backgrounds.
    /// Layout-only demands return `true` so recursive renderers retain their
    /// normal paint defaults when no capture policy exists.
    pub const fn includes_backgrounds(self) -> bool {
        match self.paint_capture {
            Some(capture) => capture.include_backgrounds,
            None => true,
        }
    }
}

/// Inputs for one on-demand screenshot layout pass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenshotLayoutRequest {
    /// Viewport and device scale to lay out and paint.
    pub viewport: PaintViewport,
}

impl ScreenshotLayoutRequest {
    /// Creates a screenshot layout request.
    pub const fn new(viewport: PaintViewport) -> Self {
        Self { viewport }
    }
}

/// Builds, lays out, and paints one synchronous source view into an owned snapshot.
///
/// The working box tree and Taffy caches are dropped before this function
/// returns. This convenience path consumes the frozen tree as well and returns
/// only DOM-neutral paint input.
pub fn build_screenshot_snapshot<S, R>(
    source: &S,
    styles: &mut R,
    services: &mut DocumentLayoutServices,
    request: ScreenshotLayoutRequest,
) -> Result<PaintSnapshot, LayoutError>
where
    S: LayoutSource,
    S::NodeId: 'static,
    R: LayoutStyleResolver<S::NodeId>,
{
    build_layout_pass(
        source,
        styles,
        services,
        LayoutPassRequest::with_paint(request.viewport, LayoutFlushReason::Screenshot),
    )
    .and_then(LayoutPassResult::into_paint_snapshot)
}

/// Builds one complete layout result from a borrowed source view.
///
/// Box construction, inline shaping, Taffy caches, and all style borrows remain
/// local to this call. The returned pass value owns one [`crate::FrozenLayoutTree`]
/// plus pass-only metrics, diagnostics, and an optional DOM-neutral paint
/// snapshot. Consumers may retain the tree, but not the surrounding pass
/// value. Several geometry answers should be batched against one tree instead
/// of triggering a layout per query.
pub fn build_layout_pass<S, R>(
    source: &S,
    styles: &mut R,
    services: &mut DocumentLayoutServices,
    request: LayoutPassRequest,
) -> Result<LayoutPassResult<S::NodeId>, LayoutError>
where
    S: LayoutSource,
    S::NodeId: 'static,
    R: LayoutStyleResolver<S::NodeId>,
{
    build_layout_pass_with_embedded_frames(source, styles, services, request, &mut NoEmbeddedFrames)
}

/// Builds one complete layout result and resolves embedded frame geometry after
/// the parent numeric layout has established their exact content viewports.
///
/// This is a one-shot composition seam, not a per-Document cache. Child trees
/// are recursively owned by the one parent snapshot, child paint is consumed
/// into parent paint, and every working layout world is then dropped.
pub fn build_layout_pass_with_embedded_frames<S, R, F>(
    source: &S,
    styles: &mut R,
    services: &mut DocumentLayoutServices,
    request: LayoutPassRequest,
    frames: &mut F,
) -> Result<LayoutPassResult<S::NodeId>, LayoutError>
where
    S: LayoutSource,
    S::NodeId: 'static,
    R: LayoutStyleResolver<S::NodeId>,
    F: EmbeddedFrameRenderer<S::NodeId>,
{
    let mut preparation = LayoutPassPreparation::new(services, request);
    let prepared = loop {
        match preparation.prepare(source, styles, services)? {
            LayoutPassStep::Ready(prepared) => break *prepared,
            LayoutPassStep::Restyle(next) => preparation = next,
        }
    };
    let used_retained_world = prepared.used_retained_world;
    let applied_sizes = prepared.applied_sizes.clone();
    let result = finish_prepared_layout_pass(prepared, source, services, frames)?;
    verify_incremental_layout_pass(&result, services, used_retained_world, |services| {
        // Embedded frames verify in their own recursive passes, so the check
        // passes no frame renderer and compares this document's geometry.
        let full = build_layout_pass_with_embedded_frames(
            source,
            styles,
            services,
            LayoutPassRequest {
                retain_world: false,
                ..request
            },
            &mut NoEmbeddedFrames,
        );
        // The full pass ran its own container-query loop on the shared
        // resolver. Put back the sizes the incremental pass ended with so
        // later passes start from the same state as without the check.
        if let Some(sizes) = &applied_sizes
            && let Err(error) = styles.update_query_container_sizes(sizes)
        {
            record_verify_comparison(false);
            tracing::error!(
                target: "moli_layout_incremental",
                %error,
                "restoring container sizes after the incremental layout check failed"
            );
        }
        full
    });
    Ok(result)
}

/// Container-query convergence state for one layout demand.
///
/// Container query sizes feed back into style, so a changed size rebuilds the
/// world from restyled elements. Each [`Self::prepare`] call builds and lays
/// out one world with the given style resolver. A [`LayoutPassStep::Restyle`]
/// result means the resolver accepted new container sizes and the caller must
/// call `prepare` again, with a fresh resolver when its styles are pinned to
/// one resolution pass. The style resolver can be released once the step is
/// [`LayoutPassStep::Ready`], before SVG content, child documents, and the
/// frozen tree consume the prepared world.
pub struct LayoutPassPreparation<N>
where
    N: Copy + std::fmt::Debug + Eq + std::hash::Hash,
{
    request: LayoutPassRequest,
    started: Instant,
    box_tree_elapsed: Duration,
    list_marker_elapsed: Duration,
    form_control_elapsed: Duration,
    inline_preparation_elapsed: Duration,
    numeric_layout_elapsed: Duration,
    container_query_iteration_count: usize,
    // `seen` holds every vector applied so far in this pass: reaching one
    // again without converging means auto scrollbars (or equivalent)
    // oscillate, and the pass keeps the current world rather than alternate
    // forever. Only the last world is published.
    seen_sizes: Vec<Vec<(N, crate::LayoutQueryContainerSize)>>,
    // Container sizes last handed to the style resolver, so the differential
    // check can put the resolver back after its own full pass.
    applied_sizes: Option<Vec<(N, crate::LayoutQueryContainerSize)>>,
    // Web fonts change shaping through registration and per-pass face order,
    // neither of which a box comparison can see, so a world shaped under
    // other web-font inputs is dropped.
    can_retain: bool,
    web_fonts: crate::text::WebFontRetentionKey,
    retained: Option<RetainedLayout<N>>,
    reused_box_count: usize,
    reused_inline_count: usize,
    reused_numeric_count: usize,
    used_retained_world: bool,
}

/// One [`LayoutPassPreparation::prepare`] outcome.
pub enum LayoutPassStep<N>
where
    N: Copy + std::fmt::Debug + Eq + std::hash::Hash,
{
    /// Container-query sizes converged; the style resolver can be released.
    Ready(Box<PreparedLayoutPass<N>>),
    /// The resolver accepted changed container sizes; prepare again.
    Restyle(LayoutPassPreparation<N>),
}

/// A converged, numerically laid-out world for one layout demand.
pub struct PreparedLayoutPass<N>
where
    N: Copy + std::fmt::Debug + Eq + std::hash::Hash,
{
    state: LayoutPassPreparation<N>,
    world: crate::LayoutWorld<N>,
    numeric_metrics: NumericLayoutMetrics,
    numeric_unrounded: Vec<taffy::Layout>,
    structural_len: usize,
    built_policy: crate::world::ViewportScrollPolicy,
    used_retained_world: bool,
    applied_sizes: Option<Vec<(N, crate::LayoutQueryContainerSize)>>,
}

impl<N> PreparedLayoutPass<N>
where
    N: Copy + std::fmt::Debug + Eq + std::hash::Hash,
{
    /// Whether this pass reused a retained world, so an incremental check can
    /// compare it against a full pass.
    pub fn used_retained_world(&self) -> bool {
        self.used_retained_world
    }

    /// The container sizes last handed to the style resolver.
    pub fn applied_query_container_sizes(&self) -> Option<&[(N, crate::LayoutQueryContainerSize)]> {
        self.applied_sizes.as_deref()
    }
}

impl<N> LayoutPassPreparation<N>
where
    N: Copy + std::fmt::Debug + Eq + std::hash::Hash + 'static,
{
    pub fn new(services: &mut DocumentLayoutServices, request: LayoutPassRequest) -> Self {
        let can_retain = request.retain_world;
        let web_fonts = services.web_font_retention_key();
        let retained: Option<RetainedLayout<N>> = if can_retain {
            services
                .take_retained_world::<RetainedLayout<N>>()
                .filter(|prior| prior.web_fonts == web_fonts)
        } else {
            services.clear_retained_world();
            None
        };
        Self {
            request,
            started: Instant::now(),
            box_tree_elapsed: Duration::ZERO,
            list_marker_elapsed: Duration::ZERO,
            form_control_elapsed: Duration::ZERO,
            inline_preparation_elapsed: Duration::ZERO,
            numeric_layout_elapsed: Duration::ZERO,
            container_query_iteration_count: 0,
            seen_sizes: Vec::new(),
            applied_sizes: None,
            can_retain,
            web_fonts,
            retained,
            reused_box_count: 0,
            reused_inline_count: 0,
            reused_numeric_count: 0,
            used_retained_world: false,
        }
    }

    pub fn prepare<S, R>(
        mut self,
        source: &S,
        styles: &mut R,
        services: &mut DocumentLayoutServices,
    ) -> Result<LayoutPassStep<N>, LayoutError>
    where
        S: LayoutSource<NodeId = N>,
        R: LayoutStyleResolver<N>,
    {
        let request = self.request;
        self.container_query_iteration_count += 1;
        let phase_started = Instant::now();
        let mut world = build_layout_world(source, styles)?;
        world.environment = request.environment;
        self.box_tree_elapsed += phase_started.elapsed();
        debug_assert!(
            world
                .viewport_scroll_policy
                .defining_body()
                .is_none_or(|body| world.box_by_id(body).is_some()),
            "viewport policy must retain only a live pass-local body box identity"
        );
        let phase_started = Instant::now();
        prepare_list_markers(&mut world)?;
        self.list_marker_elapsed += phase_started.elapsed();
        let phase_started = Instant::now();
        prepare_form_controls(&mut world, styles)?;
        self.form_control_elapsed += phase_started.elapsed();
        let phase_started = Instant::now();
        let structural_len = world.boxes.len();
        let built_policy = world.viewport_scroll_policy;
        let mut reuse = self
            .retained
            .take()
            .and_then(|prior| diff_worlds(&world, prior));
        self.used_retained_world |= reuse.is_some();
        prepare_inline_contexts(&mut world, services, reuse.as_mut());
        self.inline_preparation_elapsed += phase_started.elapsed();
        world.validate_invariants()?;
        let phase_started = Instant::now();
        let (numeric_metrics, numeric_unrounded) = compute_world_layout_with_scrollbars(
            &mut world,
            request.viewport,
            request.environment,
            reuse.as_mut(),
        );
        if let Some(reuse) = reuse {
            self.reused_box_count += reuse.reused_box_count;
            self.reused_inline_count += reuse.reused_inline_count;
            self.reused_numeric_count += reuse.reused_numeric_count;
            crate::retention::record_numeric_reused(reuse.reused_numeric_count);
        }
        self.numeric_layout_elapsed += phase_started.elapsed();

        let (sizes, depth) = world.query_container_sizes();
        let hard_cap = (depth + 3).max(MIN_CONTAINER_QUERY_ITERATION_CAP);
        let converged = self.seen_sizes.contains(&sizes)
            || self.container_query_iteration_count >= hard_cap
            || {
                let changed = styles.update_query_container_sizes(&sizes)?;
                self.applied_sizes = Some(sizes.clone());
                !changed
            };
        if converged {
            let used_retained_world = self.used_retained_world;
            let applied_sizes = self.applied_sizes.clone();
            return Ok(LayoutPassStep::Ready(Box::new(PreparedLayoutPass {
                state: self,
                world,
                numeric_metrics,
                numeric_unrounded,
                structural_len,
                built_policy,
                used_retained_world,
                applied_sizes,
            })));
        }
        self.seen_sizes.push(sizes);
        if self.can_retain {
            // The next iteration diffs against this world, so a restyle that
            // touched few containers pays only for what it changed.
            self.retained = Some(RetainedLayout {
                world,
                numeric_unrounded,
                structural_len,
                viewport: request.viewport,
                environment: request.environment,
                viewport_policy: built_policy,
                web_fonts: self.web_fonts.clone(),
            });
        }
        Ok(LayoutPassStep::Restyle(self))
    }
}

/// Resolves SVG content and embedded frames against a converged world and
/// freezes the pass result. No style resolver is borrowed here.
pub fn finish_prepared_layout_pass<S, F>(
    prepared: PreparedLayoutPass<S::NodeId>,
    source: &S,
    services: &mut DocumentLayoutServices,
    frames: &mut F,
) -> Result<LayoutPassResult<S::NodeId>, LayoutError>
where
    S: LayoutSource,
    S::NodeId: 'static,
    F: EmbeddedFrameRenderer<S::NodeId>,
{
    let PreparedLayoutPass {
        state,
        mut world,
        numeric_metrics,
        numeric_unrounded,
        structural_len,
        built_policy,
        ..
    } = prepared;
    let LayoutPassPreparation {
        request,
        started,
        box_tree_elapsed,
        list_marker_elapsed,
        form_control_elapsed,
        inline_preparation_elapsed,
        mut numeric_layout_elapsed,
        container_query_iteration_count,
        can_retain,
        web_fonts,
        reused_box_count,
        reused_inline_count,
        reused_numeric_count,
        ..
    } = state;
    // SVG content depends only on the final used size, so it resolves once
    // against the converged world rather than on every container-query
    // iteration.
    let phase_started = Instant::now();
    let object_geometry = prepare_svg_content(&mut world, source, services);
    numeric_layout_elapsed += phase_started.elapsed();
    let phase_started = Instant::now();
    let mut embedded_frames = HashMap::new();
    for (index, layout_box) in world.boxes.iter().enumerate() {
        if !layout_box
            .element_semantics()
            .is_some_and(|semantics| semantics.replaced == Some(crate::LayoutReplacedKind::Frame))
        {
            continue;
        }
        let Some(source) = layout_box.source() else {
            continue;
        };
        let layout = layout_box.final_layout();
        let width = (layout.size.width
            - layout.border.left
            - layout.border.right
            - layout.padding.left
            - layout.padding.right)
            .max(0.0);
        let height = (layout.size.height
            - layout.border.top
            - layout.border.bottom
            - layout.padding.top
            - layout.padding.bottom)
            .max(0.0);
        if width <= 0.0 || height <= 0.0 {
            continue;
        }
        let viewport = LayoutViewport::new(
            css_viewport_dimension(width),
            css_viewport_dimension(height),
            request.viewport.device_pixel_ratio,
        );
        if let Some(snapshot) = frames.render_embedded_frame(source, viewport)? {
            embedded_frames.insert(crate::LayoutBoxId::from_index(index), (source, snapshot));
        }
    }
    let embedded_frame_elapsed = phase_started.elapsed();
    let result = finish_layout_pass(
        &world,
        source,
        request.viewport,
        request.reason,
        started,
        request.paint_capture,
        object_geometry,
        embedded_frames,
        crate::projection::LayoutPassPhaseMetrics {
            box_tree_elapsed,
            list_marker_elapsed,
            form_control_elapsed,
            inline_preparation_elapsed,
            numeric_layout_elapsed,
            numeric_first_pass_elapsed: numeric_metrics.first_pass_elapsed,
            numeric_followup_passes_elapsed: numeric_metrics.followup_passes_elapsed,
            overflow_detection_elapsed: numeric_metrics.overflow_detection_elapsed,
            scrollbar_feedback_elapsed: numeric_metrics.scrollbar_feedback_elapsed,
            embedded_frame_elapsed,
            numeric_layout_pass_count: numeric_metrics.pass_count,
            numeric_feedback_invalidated_node_count: numeric_metrics
                .feedback_invalidated_node_count,
            numeric_feedback_overflow_recomputed_node_count: numeric_metrics
                .feedback_overflow_recomputed_node_count,
            container_query_iteration_count,
            retained_reused_box_count: reused_box_count,
            retained_reused_inline_count: reused_inline_count,
            retained_reused_numeric_count: reused_numeric_count,
        },
    )?;
    // A pass over the retention budget fails its read; it must not leave its
    // whole world retained behind that failure.
    if can_retain && result.validate_retention_budget().is_ok() {
        services.store_retained_world(RetainedLayout {
            world,
            numeric_unrounded,
            structural_len,
            viewport: request.viewport,
            environment: request.environment,
            viewport_policy: built_policy,
            web_fonts,
        });
    }
    Ok(result)
}

/// With incremental verification on, runs `full_pass` for a demand that
/// reused a retained world and compares its geometry with `result`. The
/// retained world is set aside for the full pass and kept afterwards. The
/// incremental result stands on its own; a failed check is reported, never
/// turned into a failed read.
pub fn verify_incremental_layout_pass<N>(
    result: &LayoutPassResult<N>,
    services: &mut DocumentLayoutServices,
    used_retained_world: bool,
    full_pass: impl FnOnce(&mut DocumentLayoutServices) -> Result<LayoutPassResult<N>, LayoutError>,
) where
    N: Copy + std::fmt::Debug + Eq + std::hash::Hash + 'static,
{
    if !used_retained_world || !verify_incremental_layout() {
        return;
    }
    let Some(retained) = services.take_retained_world::<RetainedLayout<N>>() else {
        return;
    };
    let full = full_pass(services);
    services.store_retained_world(retained);
    match full {
        Ok(full) => record_verify_result(result, &full),
        Err(error) => {
            record_verify_comparison(false);
            tracing::error!(
                target: "moli_layout_incremental",
                %error,
                "full rebuild for the incremental layout check failed"
            );
        }
    }
}

/// Whether incremental passes must also run and compare against a full pass.
pub(crate) fn verify_incremental_layout() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    crate::retention::VERIFY_FOR_TEST.with(std::cell::Cell::get)
        || *FLAG.get_or_init(|| std::env::var_os("MOLI_LAYOUT_INCREMENTAL_VERIFY").is_some())
}

fn record_verify_result<N>(incremental: &LayoutPassResult<N>, full: &LayoutPassResult<N>)
where
    N: Copy + std::fmt::Debug + Eq + std::hash::Hash,
{
    let difference = incremental.tree.geometry_difference(&full.tree);
    record_verify_comparison(difference.is_none());
    if let Some(difference) = difference {
        tracing::error!(
            target: "moli_layout_incremental",
            difference,
            incremental_boxes = incremental.tree.boxes().len(),
            full_boxes = full.tree.boxes().len(),
            "incremental layout geometry differs from a full rebuild"
        );
    }
}

fn prepare_svg_content<S: LayoutSource>(
    world: &mut crate::LayoutWorld<S::NodeId>,
    source: &S,
    services: &mut DocumentLayoutServices,
) -> Vec<crate::LayoutObjectGeometry<S::NodeId>> {
    let mut objects = Vec::new();
    let mut fonts = None;
    for (index, layout_box) in world.boxes.iter_mut().enumerate() {
        if !layout_box
            .element_semantics()
            .is_some_and(|semantics| semantics.replaced == Some(crate::LayoutReplacedKind::Svg))
        {
            continue;
        }
        let Some(node) = layout_box.source() else {
            continue;
        };
        let layout = layout_box.final_layout;
        let zoom = layout_box.style.effective_zoom();
        let viewport = crate::LayoutSize::new(
            ((layout.size.width
                - layout.border.left
                - layout.border.right
                - layout.padding.left
                - layout.padding.right
                - layout.scrollbar_size.width)
                / zoom)
                .max(0.0),
            ((layout.size.height
                - layout.border.top
                - layout.border.bottom
                - layout.padding.top
                - layout.padding.bottom
                - layout.scrollbar_size.height)
                / zoom)
                .max(0.0),
        );
        let fonts = fonts.get_or_insert_with(|| services.svg_fonts());
        if let Some(content) = source.svg_content(node, &layout_box.style, viewport, fonts) {
            layout_box.replaced_image = content.image;
            objects.extend(content.objects.into_iter().map(|(source, quad)| {
                crate::LayoutObjectGeometry {
                    source,
                    owner: crate::LayoutOutputBoxId::from_index(index),
                    quad,
                }
            }));
        }
    }
    objects
}

/// Resolves `overflow:auto` with a monotonic browser-style feedback loop.
///
/// The first iteration lays out without an automatic gutter. If overflow
/// reveals a scrollbar, its gutter changes the available space and the
/// affected box must be laid out again; that gutter can in turn reveal the
/// perpendicular scrollbar. An axis is only ever revealed, never hidden, so
/// each box/axis changes at most once and the loop converges.
///
/// Every iteration enters Taffy through the synthetic viewport root, but that
/// is a scheduling entry point rather than an unconditional full-tree layout.
/// `invalidate_scrollbar_feedback` clears only changed boxes and their numeric
/// ancestors. All other subtrees keep valid Taffy cache entries and return
/// immediately. This is the same important boundary as Blink's corrective
/// scrollbar relayout: pay a follow-up pass only after state changes, and only
/// recompute the paths whose available space can have changed.
#[derive(Default)]
struct NumericLayoutMetrics {
    pass_count: usize,
    first_pass_elapsed: Duration,
    followup_passes_elapsed: Duration,
    overflow_detection_elapsed: Duration,
    scrollbar_feedback_elapsed: Duration,
    feedback_invalidated_node_count: usize,
    feedback_overflow_recomputed_node_count: usize,
}

fn compute_world_layout_with_scrollbars<N>(
    world: &mut crate::LayoutWorld<N>,
    viewport: LayoutViewport,
    environment: crate::LayoutEnvironment,
    retained: Option<&mut crate::retention::RetainedReuse<N>>,
) -> (NumericLayoutMetrics, Vec<taffy::Layout>)
where
    N: Copy + std::fmt::Debug + Eq + std::hash::Hash,
{
    let root_id = world.root;
    let defining_body = world.viewport_scroll_policy.defining_body();
    let scrollbars_hidden = world.environment.scrollbars_hidden;
    world
        .viewport_scroll_policy
        .prepare_scrollbar_layout(scrollbars_hidden);
    for (index, layout_box) in world.boxes.iter_mut().enumerate() {
        let id = crate::LayoutBoxId::from_index(index);
        if id == root_id {
            layout_box.style.prepare_viewport_root_layout();
        } else if defining_body == Some(id) {
            layout_box.style.prepare_viewport_defining_body_layout();
        } else {
            layout_box
                .style
                .prepare_scrollbar_layout(false, scrollbars_hidden);
        }
    }

    let mut metrics = NumericLayoutMetrics::default();
    let phase_started = Instant::now();
    let mut prepared = prepare_world_layout(world, viewport, environment, retained);
    let preparation_elapsed = phase_started.elapsed();
    let mut overflow_projection: Option<OverflowProjection> = None;
    let mut pending_feedback_seeds = Vec::new();
    loop {
        metrics.pass_count = metrics.pass_count.saturating_add(1);
        let phase_started = Instant::now();
        let mut touched = compute_prepared_world_layout(world, viewport, &mut prepared);
        let elapsed = phase_started.elapsed();
        if metrics.pass_count == 1 {
            metrics.first_pass_elapsed = preparation_elapsed + elapsed;
        } else {
            metrics.followup_passes_elapsed += elapsed;
        }
        // Hidden controls cannot introduce an automatic gutter. Final output
        // projection still computes scroll extents, so this demand needs no
        // scrollbar-feedback overflow projection or corrective numeric pass.
        if scrollbars_hidden {
            break;
        }
        let phase_started = Instant::now();
        touched.append(&mut pending_feedback_seeds);
        let candidates = if let Some(projection) = overflow_projection.as_mut() {
            let affected = projection.refresh(world, viewport, &touched);
            metrics.feedback_overflow_recomputed_node_count = metrics
                .feedback_overflow_recomputed_node_count
                .saturating_add(affected.len());
            affected
        } else {
            let projection = OverflowProjection::new(world, viewport);
            let candidates = (0..projection.len())
                .map(crate::LayoutBoxId::from_index)
                .collect();
            overflow_projection = Some(projection);
            candidates
        };
        let projection = overflow_projection
            .as_ref()
            .expect("overflow projection is initialized above");
        let candidates = candidates
            .into_iter()
            .map(|id| (id, projection.overflowing_axes(world, id)))
            .collect::<Vec<_>>();
        metrics.overflow_detection_elapsed += phase_started.elapsed();
        let phase_started = Instant::now();
        let mut viewport_changed = false;
        let mut changed_boxes = Vec::new();
        for (id, (overflow_x, overflow_y)) in candidates {
            if id == root_id {
                viewport_changed |= world
                    .viewport_scroll_policy
                    .reveal_auto_scrollbar(LayoutScrollbarAxis::Horizontal, overflow_x);
                viewport_changed |= world
                    .viewport_scroll_policy
                    .reveal_auto_scrollbar(LayoutScrollbarAxis::Vertical, overflow_y);
                continue;
            }
            if defining_body == Some(id) {
                continue;
            }
            let layout_box = &mut world.boxes[id.index()];
            let mut box_changed = layout_box.style.reveal_auto_scrollbar(
                LayoutScrollbarAxis::Horizontal,
                false,
                overflow_x,
            );
            box_changed |= layout_box.style.reveal_auto_scrollbar(
                LayoutScrollbarAxis::Vertical,
                false,
                overflow_y,
            );
            if box_changed {
                changed_boxes.push(id);
            }
        }
        let changed = viewport_changed || !changed_boxes.is_empty();
        if changed {
            // Do not clear the whole Taffy tree here. Although the next call to
            // `compute_prepared_world_layout` starts at the root, untouched
            // branches remain cache hits; only these boxes and their ancestor
            // paths participate in the corrective numeric layout.
            metrics.feedback_invalidated_node_count =
                metrics.feedback_invalidated_node_count.saturating_add(
                    prepared.invalidate_scrollbar_feedback(world, &changed_boxes, viewport_changed),
                );
            pending_feedback_seeds.extend(changed_boxes.iter().copied());
            if viewport_changed {
                pending_feedback_seeds.push(root_id);
            }
        }
        metrics.scrollbar_feedback_elapsed += phase_started.elapsed();
        if !changed {
            break;
        }
    }
    let numeric_unrounded = prepared.take_numeric_unrounded();
    (metrics, numeric_unrounded)
}

/// Backstop for container-query feedback that neither converges nor repeats.
const MIN_CONTAINER_QUERY_ITERATION_CAP: usize = 8;

fn css_viewport_dimension(value: f32) -> u32 {
    if !value.is_finite() {
        return 0;
    }
    value.round().clamp(0.0, u32::MAX as f32) as u32
}
