//! The immutable retained tree and its memory-retention boundary.

use std::{fmt::Debug, hash::Hash, ops::Deref};

use crate::LayoutError;

use super::model::{
    FrozenCoordinateSpace, LayoutBoxGeometry, LayoutClipNode, LayoutCoordinateSpaceId,
    LayoutFragment, LayoutFragmentId, LayoutOutputBoxId, LayoutPoint, LayoutScrollExtent,
    LayoutSize, LayoutViewport,
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
        }
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
    /// on them; sticky positioning resolves against the unscrolled scrollport
    /// (`finish_sticky_positioning`). Moving an offset is therefore exactly a
    /// translation of the affected coordinate spaces, which this applies.
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

        // Offset change (old minus new) per scroll container, in the box's
        // own zoomed coordinate space.
        let mut moved = vec![LayoutPoint::ZERO; self.boxes.len()];
        let mut any_moved = false;
        for (index, layout_box) in self.boxes.iter_mut().enumerate() {
            let extent = &mut layout_box.scroll_extent;
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
            if applied == extent.applied_offset {
                continue;
            }
            moved[index] = LayoutPoint::new(
                extent.applied_offset.x - applied.x,
                extent.applied_offset.y - applied.y,
            );
            any_moved = true;
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
        if !any_moved {
            return outcome;
        }

        let root = self.root_box.index();
        let viewport_moved = moved.get(root).copied().unwrap_or(LayoutPoint::ZERO);
        if let Some(root_box) = self.boxes.get(root) {
            self.viewport_scroll = root_box.scroll_extent.applied_offset;
        }

        // Viewport translation each box gains, exact and ignoring CSS
        // transforms, resolved parent first.
        let count = self.boxes.len();
        let mut shift: Vec<Option<((f64, f64), (f64, f64))>> = vec![None; count];
        let mut chain = Vec::new();
        for start in 0..count {
            let mut current = start;
            while shift[current].is_none() {
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
                let geometry = &self.boxes[index].geometry;
                let resolved = match geometry.layout_parent {
                    None if geometry.position.is_fixed() => ((0.0, 0.0), (0.0, 0.0)),
                    None => {
                        let viewport = (f64::from(viewport_moved.x), f64::from(viewport_moved.y));
                        (viewport, viewport)
                    }
                    Some(parent) => {
                        let parent = parent.index();
                        let (exact, plain) =
                            shift[parent].expect("a parent resolves before its child");
                        let scrolled = moved[parent];
                        if parent == root || scrolled == LayoutPoint::ZERO {
                            (exact, plain)
                        } else {
                            let [a, b, c, d, _, _] = self.boxes[parent]
                                .coordinate_space
                                .local_to_viewport
                                .coefficients;
                            let (dx, dy) = (f64::from(scrolled.x), f64::from(scrolled.y));
                            (
                                (exact.0 + a * dx + c * dy, exact.1 + b * dx + d * dy),
                                (plain.0 + dx, plain.1 + dy),
                            )
                        }
                    }
                };
                shift[index] = Some(resolved);
            }
        }
        for (layout_box, shift) in self.boxes.iter_mut().zip(shift) {
            let ((exact_x, exact_y), (plain_x, plain_y)) =
                shift.expect("every box resolves its shift");
            let space = &mut layout_box.coordinate_space;
            space.local_to_viewport.coefficients[4] += exact_x;
            space.local_to_viewport.coefficients[5] += exact_y;
            space.local_to_viewport_ignoring_css_transforms.coefficients[4] += plain_x;
            space.local_to_viewport_ignoring_css_transforms.coefficients[5] += plain_y;
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
