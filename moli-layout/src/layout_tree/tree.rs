//! The immutable retained tree and its memory-retention boundary.

use std::{fmt::Debug, hash::Hash, ops::Deref};

use crate::LayoutError;

use super::model::{
    FrozenCoordinateSpace, LayoutBoxGeometry, LayoutClipNode, LayoutCoordinateSpaceId,
    LayoutFragment, LayoutFragmentId, LayoutOutputBoxId, LayoutPoint, LayoutScrollExtent,
    LayoutSize, LayoutTransform2D, LayoutViewport,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CssSizingBox {
    Content,
    Border,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CssSizing {
    pub box_kind: CssSizingBox,
    pub horizontal: bool,
}

/// One node in the immutable layout tree retained after a full pass.
///
/// `geometry_source` associates ordinary CSSOM geometry with its source. A
/// split inline continuation can therefore share its originating source while
/// remaining a distinct box-tree node. `hit_source` is separate because a
/// generated pseudo box participates in hit testing as its originating DOM
/// element without manufacturing CSSOM rects for that element.
#[derive(Clone, Debug, PartialEq)]
pub struct FrozenLayoutBox<N> {
    pub geometry: LayoutBoxGeometry,
    pub scroll_extent: LayoutScrollExtent,
    pub coordinate_space: FrozenCoordinateSpace,
    pub geometry_source: Option<N>,
    pub principal_source: Option<N>,
    pub hit_source: Option<N>,
    /// Box selection and logical-axis orientation for CSSOM sizes, sampled
    /// with the geometry. `None` means no used size (for example, a non-replaced
    /// inline). Retaining this discriminator avoids consulting live display
    /// or box-sizing/writing-mode when an ordinary read reuses an older layout
    /// snapshot. This is interpretation metadata, not another copy of sizes.
    pub(crate) css_sizing: Option<CssSizing>,
    pub resolved_grid_tracks: Option<super::model::LayoutResolvedGridTracks>,
    /// Paint ordinal of this box's scrollbar/corner surface, if any.
    pub(crate) control_paint_order: Option<u32>,
}

/// One child browsing context owned by the same latest frozen snapshot as its
/// parent. It is deliberately recursive rather than an independently keyed
/// cache entry, so capturing input geometry does not publish a child over the
/// top-level tree.
pub struct FrozenEmbeddedFrame<N>
where
    N: Copy + Debug + Eq + Hash,
{
    pub source: N,
    pub tree: Box<FrozenLayoutTree<N>>,
}

impl<N> Deref for FrozenLayoutBox<N> {
    type Target = LayoutBoxGeometry;

    fn deref(&self) -> &Self::Target {
        &self.geometry
    }
}

/// Retained footprint for one frozen layout tree.
///
/// The byte count is an allocation-capacity estimate for the tree's box,
/// fragment, source-provenance, scroll, transform, and clip storage. It
/// excludes allocator metadata and every pass-only diagnostic or paint value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayoutTreeRetentionMetrics {
    pub box_count: usize,
    pub fragment_count: usize,
    pub estimated_geometry_bytes: usize,
}

/// Maximum geometry boxes retained in the single latest-layout snapshot.
pub const MAX_RETAINED_LAYOUT_BOXES: usize = 1_000_000;
/// Maximum fragments retained in the single latest-layout snapshot.
pub const MAX_RETAINED_LAYOUT_FRAGMENTS: usize = 4_000_000;
/// Maximum estimated allocation capacity retained by one frozen layout tree.
pub const MAX_RETAINED_LAYOUT_TREE_BYTES: usize = 256 * 1024 * 1024;

/// Immutable, DOM-independent layout tree produced by one complete pass.
///
/// The box tree is stored densely through parent IDs. Text/inline fragments,
/// coordinate spaces, clips, and scroll extents are canonical layout data:
/// they preserve results that cannot be reconstructed after the working tree,
/// Taffy caches, Parley state, and computed styles are dropped. Source and
/// hit-test indexes are derived from the box provenance and fragments.
pub struct FrozenLayoutTree<N>
where
    N: Copy + Debug + Eq + Hash,
{
    source_root: N,
    pub viewport: LayoutViewport,
    pub viewport_scroll: LayoutPoint,
    pub content_size: LayoutSize,
    pub root_box: LayoutOutputBoxId,
    pub boxes: Vec<FrozenLayoutBox<N>>,
    pub fragments: Vec<LayoutFragment>,
    /// Source/box relationships for `display: contents` nodes, which own no
    /// principal box but can still nominate rendered descendants for scroll.
    pub scroll_proxy_links: Vec<(N, LayoutOutputBoxId)>,
    viewport_coordinate_space: FrozenCoordinateSpace,
    pub clip_chain: Vec<LayoutClipNode>,
    embedded_frames: Vec<FrozenEmbeddedFrame<N>>,
    /// Per-box projection inputs that a scroll offset change re-projects
    /// with; empty when the tree was not produced by a layout pass.
    scroll_projection: Vec<ScrollProjectionInput>,
}

/// What [`FrozenLayoutTree::sync_scroll_offsets`] needs to rebuild one box's
/// coordinate space exactly as the projection built it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ScrollProjectionInput {
    /// The box's layout location in its layout parent's space.
    pub(crate) location: LayoutPoint,
    /// The box's resolved CSS transform.
    pub(crate) css_transform: LayoutTransform2D,
    /// A parentless box anchored to the viewport (fixed positioned).
    pub(crate) viewport_anchored_root: bool,
    /// The box is `position: sticky`.
    pub(crate) sticky: bool,
    /// A sticky box sits somewhere below this box in its layout parent
    /// chain.
    pub(crate) sticky_descendant: bool,
}

impl<N> FrozenLayoutTree<N>
where
    N: Copy + Debug + Eq + Hash,
{
    /// Source root whose complete layout projection this member tree owns.
    pub const fn source_root(&self) -> N {
        self.source_root
    }

    /// Finds an exact Document/source-root member without a per-Document
    /// index. Browsing-context depth is bounded, so this walks only the small
    /// recursive frame projection rather than the member trees' boxes.
    pub fn tree_for_root(&self, source_root: N) -> Option<&FrozenLayoutTree<N>> {
        if self.source_root == source_root {
            return Some(self);
        }
        self.embedded_frames
            .iter()
            .find_map(|frame| frame.tree.tree_for_root(source_root))
    }

    pub fn retention_metrics(&self) -> LayoutTreeRetentionMetrics {
        fn allocation<T>(capacity: usize) -> usize {
            capacity.saturating_mul(std::mem::size_of::<T>())
        }

        let box_allocations = self.boxes.iter().fold(0usize, |bytes, layout_box| {
            let fragment_bytes = allocation::<LayoutFragmentId>(layout_box.fragments.capacity());
            let grid_bytes = layout_box.resolved_grid_tracks.as_ref().map_or(0, |grid| {
                let numeric_bytes = [
                    grid.rows.used_track_sizes.capacity(),
                    grid.columns.used_track_sizes.capacity(),
                ]
                .into_iter()
                .fold(0usize, |bytes, capacity| {
                    bytes.saturating_add(allocation::<f32>(capacity))
                });
                let line_name_bytes =
                    [&grid.rows, &grid.columns]
                        .into_iter()
                        .fold(0usize, |bytes, tracks| {
                            let outer =
                                allocation::<Vec<String>>(tracks.explicit_line_names.capacity());
                            tracks.explicit_line_names.iter().fold(
                                bytes.saturating_add(outer),
                                |bytes, names| {
                                    let atoms = allocation::<style::Atom>(names.capacity());
                                    let retained_text = names.iter().fold(0usize, |bytes, name| {
                                        bytes.saturating_add(name.len())
                                    });
                                    bytes.saturating_add(atoms).saturating_add(retained_text)
                                },
                            )
                        });
                numeric_bytes.saturating_add(line_name_bytes)
            });
            bytes
                .saturating_add(fragment_bytes)
                .saturating_add(grid_bytes)
        });
        let own_estimated_geometry_bytes = std::mem::size_of::<Self>()
            .saturating_add(allocation::<FrozenLayoutBox<N>>(self.boxes.capacity()))
            .saturating_add(allocation::<LayoutFragment>(self.fragments.capacity()))
            .saturating_add(allocation::<(N, LayoutOutputBoxId)>(
                self.scroll_proxy_links.capacity(),
            ))
            .saturating_add(allocation::<LayoutClipNode>(self.clip_chain.capacity()))
            .saturating_add(allocation::<FrozenEmbeddedFrame<N>>(
                self.embedded_frames.capacity(),
            ))
            .saturating_add(allocation::<ScrollProjectionInput>(
                self.scroll_projection.capacity(),
            ))
            .saturating_add(box_allocations);
        let embedded = self.embedded_frames.iter().fold(
            LayoutTreeRetentionMetrics::default(),
            |total, frame| {
                let child = frame.tree.retention_metrics();
                LayoutTreeRetentionMetrics {
                    box_count: total.box_count.saturating_add(child.box_count),
                    fragment_count: total.fragment_count.saturating_add(child.fragment_count),
                    estimated_geometry_bytes: total
                        .estimated_geometry_bytes
                        .saturating_add(child.estimated_geometry_bytes),
                }
            },
        );
        LayoutTreeRetentionMetrics {
            box_count: self.boxes.len().saturating_add(embedded.box_count),
            fragment_count: self.fragments.len().saturating_add(embedded.fragment_count),
            estimated_geometry_bytes: own_estimated_geometry_bytes
                .saturating_add(embedded.estimated_geometry_bytes),
        }
    }

    /// Rejects a tree that would make the single latest-layout slot an
    /// unbounded retained allocation.
    pub fn validate_retention_budget(&self) -> Result<(), LayoutError> {
        validate_retention_metrics(self.retention_metrics())
    }

    pub fn box_geometry(&self, id: LayoutOutputBoxId) -> Option<&LayoutBoxGeometry> {
        self.boxes
            .get(id.index())
            .map(|layout_box| &layout_box.geometry)
    }

    pub fn fragment(&self, id: LayoutFragmentId) -> Option<&LayoutFragment> {
        self.fragments.get(id.index())
    }

    pub fn coordinate_space(&self, id: LayoutCoordinateSpaceId) -> Option<&FrozenCoordinateSpace> {
        match id.index() {
            0 => Some(&self.viewport_coordinate_space),
            index => self
                .boxes
                .get(index - 1)
                .map(|layout_box| &layout_box.coordinate_space),
        }
    }

    pub fn scroll_extent(&self, id: LayoutOutputBoxId) -> Option<&LayoutScrollExtent> {
        self.boxes
            .get(id.index())
            .map(|layout_box| &layout_box.scroll_extent)
    }

    /// Returns the embedded child tree frozen into this same retained snapshot.
    pub fn embedded_frame_tree(&self, source: N) -> Option<&FrozenLayoutTree<N>> {
        self.embedded_frames
            .iter()
            .find(|frame| frame.source == source)
            .map(|frame| frame.tree.as_ref())
    }

    pub fn embedded_frames(&self) -> impl ExactSizeIterator<Item = &FrozenEmbeddedFrame<N>> {
        self.embedded_frames.iter()
    }
}

impl<N> FrozenLayoutTree<N>
where
    N: Copy + Debug + Eq + Hash,
{
    pub(crate) fn new(
        source_root: N,
        viewport: LayoutViewport,
        viewport_scroll: LayoutPoint,
        content_size: LayoutSize,
        root_box: LayoutOutputBoxId,
        boxes: Vec<FrozenLayoutBox<N>>,
        fragments: Vec<LayoutFragment>,
        scroll_proxy_links: Vec<(N, LayoutOutputBoxId)>,
        viewport_coordinate_space: FrozenCoordinateSpace,
        clip_chain: Vec<LayoutClipNode>,
        embedded_frames: Vec<FrozenEmbeddedFrame<N>>,
    ) -> Self {
        Self {
            source_root,
            viewport,
            viewport_scroll,
            content_size,
            root_box,
            boxes,
            fragments,
            scroll_proxy_links,
            viewport_coordinate_space,
            clip_chain,
            embedded_frames,
            scroll_projection: Vec::new(),
        }
    }

    pub(crate) fn with_scroll_projection(mut self, inputs: Vec<ScrollProjectionInput>) -> Self {
        self.scroll_projection = inputs;
        self
    }
}

/// Result of [`FrozenLayoutTree::sync_scroll_offsets`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScrollOffsetSync {
    /// Every scroll container already applies its source's offset.
    Unchanged,
    /// Applied offsets, scrollbars and coordinate spaces now match the
    /// sampled offsets.
    Updated,
    /// The tree cannot follow the new offsets without a layout pass.
    NeedsLayout,
}

impl<N> FrozenLayoutTree<N>
where
    N: Copy + Debug + Eq + Hash,
{
    /// Re-samples every scroll container's offset and moves the published
    /// projection to it without a layout pass.
    ///
    /// A layout pass reads scroll offsets only when it projects boxes: the
    /// applied offset is the requested offset clamped to the box's scroll
    /// range, each child of a non-root scroll container is translated by
    /// minus that offset in the container's space, and every box that is not
    /// anchored to the viewport is translated by minus the viewport offset.
    /// Box sizes, positions, scroll ranges, clips and fragments do not depend
    /// on them, so moving an offset is exactly a translation of the affected
    /// coordinate spaces, which this applies.
    ///
    /// Sticky positioning is the exception: a sticky box's position follows
    /// the offset of the scrollport it sticks to. A change that moves a
    /// scroll container with a sticky descendant, or the viewport while any
    /// sticky box exists, needs a layout pass instead. That is decided
    /// before any applied offset changes, so the tree stays as it was.
    ///
    /// `sample` returns a source's scroll offset in CSSOM (unzoomed) pixels,
    /// as [`crate::LayoutSource::scroll_offset`] does during a pass.
    pub fn sync_scroll_offsets(
        &mut self,
        sample: &mut dyn FnMut(N) -> LayoutPoint,
    ) -> ScrollOffsetSync {
        let mut outcome = ScrollOffsetSync::Unchanged;
        for frame in &mut self.embedded_frames {
            match frame.tree.sync_scroll_offsets(sample) {
                ScrollOffsetSync::NeedsLayout => return ScrollOffsetSync::NeedsLayout,
                ScrollOffsetSync::Updated => outcome = ScrollOffsetSync::Updated,
                ScrollOffsetSync::Unchanged => {}
            }
        }

        // Sample each scroll container's offset, in the box's own zoomed
        // coordinate space, as the projection does. Nothing changes yet.
        let mut moved = Vec::new();
        for (index, layout_box) in self.boxes.iter().enumerate() {
            let extent = &layout_box.scroll_extent;
            if !extent.is_scroll_container {
                continue;
            }
            let Some(source) = layout_box.principal_source else {
                // Only a principal box imports a source offset; any other
                // box was projected at the origin.
                continue;
            };
            let zoom = layout_box.geometry.effective_zoom;
            let offset = sample(source);
            let requested = LayoutPoint::new(
                finite_or_zero(offset.x * zoom),
                finite_or_zero(offset.y * zoom),
            );
            let applied = LayoutPoint::new(
                requested
                    .x
                    .clamp(extent.minimum_offset.x, extent.maximum_offset.x),
                requested
                    .y
                    .clamp(extent.minimum_offset.y, extent.maximum_offset.y),
            );
            if applied != extent.applied_offset {
                moved.push((index, applied));
            }
        }
        if moved.is_empty() {
            return outcome;
        }

        let root = self.root_box.index();
        let count = self.boxes.len();
        if root >= count || self.scroll_projection.len() != count {
            return ScrollOffsetSync::NeedsLayout;
        }
        let any_sticky = self.scroll_projection.iter().any(|input| input.sticky);
        if moved.iter().any(|&(index, _)| {
            self.scroll_projection[index].sticky_descendant || (index == root && any_sticky)
        }) {
            return ScrollOffsetSync::NeedsLayout;
        }

        for (index, applied) in moved {
            let extent = &mut self.boxes[index].scroll_extent;
            extent.applied_offset = applied;
            extent.horizontal_scrollbar = extent.horizontal_scrollbar.map(|bar| {
                crate::LayoutScrollbarGeometry::new(
                    bar.axis,
                    bar.frame,
                    bar.minimum_offset,
                    bar.maximum_offset,
                    applied.x,
                    extent.scrollport.width,
                    extent.scroll_size.width,
                )
            });
            extent.vertical_scrollbar = extent.vertical_scrollbar.map(|bar| {
                crate::LayoutScrollbarGeometry::new(
                    bar.axis,
                    bar.frame,
                    bar.minimum_offset,
                    bar.maximum_offset,
                    applied.y,
                    extent.scrollport.height,
                    extent.scroll_size.height,
                )
            });
        }

        let viewport_scroll = self.boxes[root].scroll_extent.applied_offset;
        self.viewport_scroll = viewport_scroll;

        // Rebuild every coordinate space parent first with the projection's
        // own arithmetic (`resolve_coordinate_space`): the f32 location minus
        // the parent's applied offset, then the f64 concatenations. Adding an
        // exact f64 delta instead would round differently than a fresh pass.
        let mut document: Vec<Option<(LayoutTransform2D, LayoutTransform2D)>> = vec![None; count];
        let mut chain = Vec::new();
        for start in 0..count {
            let mut current = start;
            while document[current].is_none() {
                chain.push(current);
                match self.boxes[current].geometry.layout_parent {
                    Some(parent) if parent.index() < count => current = parent.index(),
                    Some(_) => return ScrollOffsetSync::NeedsLayout,
                    None => break,
                }
                if chain.len() > count {
                    return ScrollOffsetSync::NeedsLayout;
                }
            }
            while let Some(index) = chain.pop() {
                let input = self.scroll_projection[index];
                let parent = self.boxes[index]
                    .geometry
                    .layout_parent
                    .map(|parent| parent.index());
                let parent_scroll = parent
                    .filter(|parent| {
                        *parent != root && self.boxes[*parent].scroll_extent.is_scroll_container
                    })
                    .map_or(LayoutPoint::ZERO, |parent| {
                        self.boxes[parent].scroll_extent.applied_offset
                    });
                let local_to_parent_ignoring_css_transforms = LayoutTransform2D::translation(
                    input.location.x - parent_scroll.x,
                    input.location.y - parent_scroll.y,
                );
                let local_to_parent =
                    local_to_parent_ignoring_css_transforms.concatenate(input.css_transform);
                let resolved = match parent {
                    None if input.viewport_anchored_root => {
                        let anchor =
                            LayoutTransform2D::translation(viewport_scroll.x, viewport_scroll.y);
                        (
                            anchor.concatenate(local_to_parent),
                            anchor.concatenate(local_to_parent_ignoring_css_transforms),
                        )
                    }
                    None => (
                        LayoutTransform2D::IDENTITY.concatenate(local_to_parent),
                        LayoutTransform2D::IDENTITY
                            .concatenate(local_to_parent_ignoring_css_transforms),
                    ),
                    Some(parent) => {
                        let (exact, plain) =
                            document[parent].expect("a parent resolves before its child");
                        (
                            exact.concatenate(local_to_parent),
                            plain.concatenate(local_to_parent_ignoring_css_transforms),
                        )
                    }
                };
                document[index] = Some(resolved);
            }
        }
        let to_viewport = LayoutTransform2D::translation(-viewport_scroll.x, -viewport_scroll.y);
        for (layout_box, resolved) in self.boxes.iter_mut().zip(document) {
            let (exact, plain) = resolved.expect("every box resolves its space");
            let space = &mut layout_box.coordinate_space;
            space.local_to_viewport = to_viewport.concatenate(exact);
            space.local_to_viewport_ignoring_css_transforms = to_viewport.concatenate(plain);
        }
        ScrollOffsetSync::Updated
    }
}

fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

fn validate_retention_metrics(metrics: LayoutTreeRetentionMetrics) -> Result<(), LayoutError> {
    if metrics.box_count > MAX_RETAINED_LAYOUT_BOXES
        || metrics.fragment_count > MAX_RETAINED_LAYOUT_FRAGMENTS
        || metrics.estimated_geometry_bytes > MAX_RETAINED_LAYOUT_TREE_BYTES
    {
        return Err(LayoutError::TreeRetentionBudgetExceeded {
            boxes: metrics.box_count,
            fragments: metrics.fragment_count,
            estimated_bytes: metrics.estimated_geometry_bytes,
            max_boxes: MAX_RETAINED_LAYOUT_BOXES,
            max_fragments: MAX_RETAINED_LAYOUT_FRAGMENTS,
            max_bytes: MAX_RETAINED_LAYOUT_TREE_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_tree_budget_reports_each_bounded_dimension() {
        for metrics in [
            LayoutTreeRetentionMetrics {
                box_count: MAX_RETAINED_LAYOUT_BOXES + 1,
                ..Default::default()
            },
            LayoutTreeRetentionMetrics {
                fragment_count: MAX_RETAINED_LAYOUT_FRAGMENTS + 1,
                ..Default::default()
            },
            LayoutTreeRetentionMetrics {
                estimated_geometry_bytes: MAX_RETAINED_LAYOUT_TREE_BYTES + 1,
                ..Default::default()
            },
        ] {
            assert!(matches!(
                validate_retention_metrics(metrics),
                Err(LayoutError::TreeRetentionBudgetExceeded { .. })
            ));
        }
        validate_retention_metrics(LayoutTreeRetentionMetrics {
            box_count: MAX_RETAINED_LAYOUT_BOXES,
            fragment_count: MAX_RETAINED_LAYOUT_FRAGMENTS,
            estimated_geometry_bytes: MAX_RETAINED_LAYOUT_TREE_BYTES,
        })
        .expect("each exact retention limit should be accepted");
    }
}
