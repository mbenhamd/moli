//! Retained layout state for the live-geometry policy.
//!
//! A full pass builds a new box tree from borrowed DOM/style sources on every
//! demand. That construction stays authoritative: nothing here decides that a
//! DOM node is clean. Instead, the fresh world is compared box by box with the
//! world retained from the previous pass, and only then are the expensive
//! derived products (Parley shaping, numeric caches and subtree geometry)
//! moved across. Every input layout consumes is therefore read again from the
//! source on every pass, so a mutation path can't be missed by forgetting to
//! mark something dirty.
//!
//! Boxes correspond by index, so slice one requires an identical box-tree
//! shape. Any structural difference, or any other doubt, falls back to the
//! ordinary full pass.

use std::{
    fmt::Debug,
    hash::Hash,
    sync::atomic::{AtomicU64, Ordering},
};

use taffy::Layout;

use crate::{LayoutBoxId, LayoutEnvironment, LayoutViewport, LayoutWorld};

/// Numeric layout results and shaped text from one earlier world.
pub(crate) struct RetainedLayout<N>
where
    N: Copy + Debug + Eq + Hash,
{
    pub(crate) world: LayoutWorld<N>,
    /// Geometry captured right after numeric layout, before post-layout
    /// adapters rewrite `unrounded_layout`.
    pub(crate) numeric_unrounded: Vec<Layout>,
    /// Box count when the world was compared, before numeric preparation
    /// appended static-position placeholders.
    pub(crate) structural_len: usize,
    pub(crate) viewport: LayoutViewport,
    pub(crate) environment: LayoutEnvironment,
}

/// Which boxes in a fresh world may take products from a retained world.
pub(crate) struct RetainedReuse<N>
where
    N: Copy + Debug + Eq + Hash,
{
    pub(crate) prior: RetainedLayout<N>,
    /// `true` when this box and everything beneath it in the box tree
    /// matches the retained world.
    pub(crate) clean: Vec<bool>,
    pub(crate) reused_box_count: usize,
    pub(crate) reused_inline_count: usize,
    pub(crate) reused_numeric_count: usize,
}

/// Compares `new` against `prior`. Returns `None` when the box-tree shape
/// differs, which requires a full pass.
pub(crate) fn diff_worlds<N>(
    new: &LayoutWorld<N>,
    prior: RetainedLayout<N>,
) -> Option<RetainedReuse<N>>
where
    N: Copy + Debug + Eq + Hash,
{
    let old = &prior.world;
    if new.boxes.len() != prior.structural_len
        || old.boxes.len() < prior.structural_len
        || new.root != old.root
        || new.root_is_document_element != old.root_is_document_element
        || new.quirks_mode != old.quirks_mode
    {
        return None;
    }
    let count = new.boxes.len();
    for index in 0..count {
        let (a, b) = (&new.boxes[index], &old.boxes[index]);
        // A replaced text node keeps its position: `textContent` swaps the
        // node without changing the box tree. Its box is merely not clean.
        let same_source = a.source == b.source || a.kind.is_text();
        if !same_source
            || a.owner != b.owner
            || a.pseudo != b.pseudo
            || a.kind != b.kind
            || a.anonymous_reason != b.anonymous_reason
            || a.parent != b.parent
            || a.structural_parent != b.structural_parent
            || a.children != b.children
            || a.element_semantics != b.element_semantics
            || a.inline_formatting_context != b.inline_formatting_context
            || a.outside_list_marker != b.outside_list_marker
        {
            return None;
        }
    }
    let same_computed = |a: &crate::ResolvedLayoutStyle, b: &crate::ResolvedLayoutStyle| match (
        a.stylo_computed_values(),
        b.stylo_computed_values(),
    ) {
        (Some(a), Some(b)) => style::servo_arc::Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    };
    let mut style_same = vec![false; count];
    for (index, slot) in style_same.iter_mut().enumerate() {
        *slot = same_computed(&new.boxes[index].style, &old.boxes[index].style);
    }
    let mut local_same = vec![false; count];
    for index in 0..count {
        let (a, b) = (&new.boxes[index], &old.boxes[index]);
        // An anonymous box resolves a fresh ComputedValues on every pass, but
        // it is a pure function of its parent's values and display role.
        let style_matches = style_same[index]
            || (a.anonymous_reason.is_some()
                && a.parent.is_some_and(|parent| style_same[parent.index()]));
        local_same[index] = style_matches
            && a.source == b.source
            && a.text == b.text
            && a.text_selection == b.text_selection
            && a.scroll_offset == b.scroll_offset
            && replaced_inputs_equal(a.replaced_context, b.replaced_context)
            && !old_box_has_revealed_scrollbars(b);
    }
    // Post-order over the box tree: a box is clean only when all children are.
    let mut clean = vec![false; count];
    let mut stack = vec![(new.root, false)];
    while let Some((id, visited)) = stack.pop() {
        if visited {
            let layout_box = &new.boxes[id.index()];
            clean[id.index()] = local_same[id.index()]
                && layout_box.children.iter().all(|child| clean[child.index()]);
        } else {
            stack.push((id, true));
            stack.extend(new.boxes[id.index()].children.iter().map(|c| (*c, false)));
        }
    }
    let reused_box_count = clean.iter().filter(|c| **c).count();
    Some(RetainedReuse {
        prior,
        clean,
        reused_box_count,
        reused_inline_count: 0,
        reused_numeric_count: 0,
    })
}

fn replaced_inputs_equal(
    a: Option<crate::replaced::ReplacedContext>,
    b: Option<crate::replaced::ReplacedContext>,
) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.retained_layout_inputs_equal(&b),
        _ => false,
    }
}

/// A revealed automatic scrollbar participated in the old box's numeric
/// result, but a fresh pass starts with none, so its caches are unusable.
fn old_box_has_revealed_scrollbars<N>(layout_box: &crate::LayoutBox<N>) -> bool {
    layout_box.style.has_revealed_auto_scrollbars()
}

impl<N> RetainedReuse<N>
where
    N: Copy + Debug + Eq + Hash,
{
    /// Moves shaped inline contexts into `new` for clean formatting-context
    /// owners. Returns one flag per box: `true` means the box still needs its
    /// inline preparation.
    pub(crate) fn transfer_inline_flags_and_owners(
        &mut self,
        new: &mut LayoutWorld<N>,
    ) -> Vec<bool> {
        let mut needs_prepare = vec![true; new.boxes.len()];
        for (index, needs_prepare) in needs_prepare.iter_mut().enumerate() {
            if !self.clean[index] {
                continue;
            }
            let old = &mut self.prior.world.boxes[index];
            let layout_box = &mut new.boxes[index];
            layout_box.inline_context_owner = old.inline_context_owner;
            layout_box.inline_flattened = old.inline_flattened;
            *needs_prepare = false;
            if layout_box.inline_formatting_context
                && let Some(context) = old.inline_layout.take()
            {
                layout_box.inline_layout = Some(context);
                self.reused_inline_count += 1;
            }
        }
        needs_prepare
    }

    /// Moves numeric results for clean boxes. The caller has already built
    /// the new numeric tree; `allowed` is false when the whole world must
    /// recompute numerically.
    pub(crate) fn transfer_numeric(&mut self, new: &mut LayoutWorld<N>, allowed: bool) {
        if !allowed
            || new.boxes.len() != self.prior.world.boxes.len()
            || self.prior.numeric_unrounded.len() != new.boxes.len()
        {
            return;
        }
        // A cell's cached result depends on table-wide state; drop every box
        // inside a table whose own subtree changed.
        let mut numeric_clean = self.clean.clone();
        // Static-position placeholders are appended after the builder boxes and
        // carry no diff input. One is only as clean as the parent that lays it
        // out, and it must correspond to the same retained placeholder.
        let builder_len = self.clean.len();
        numeric_clean.resize(new.boxes.len(), false);
        for (index, slot) in numeric_clean.iter_mut().enumerate().skip(builder_len) {
            let (a, b) = (&new.boxes[index], &self.prior.world.boxes[index]);
            *slot = a.owner == b.owner
                && a.source == b.source
                && a.kind == b.kind
                && a.pseudo == b.pseudo
                && a.layout_parent.is_some_and(|parent| {
                    parent.index() < builder_len && self.clean[parent.index()]
                });
        }
        for (index, layout_box) in new.boxes.iter().enumerate().take(builder_len) {
            if crate::table::is_table_root_kind(layout_box.kind) && !numeric_clean[index] {
                mark_subtree_unclean(new, LayoutBoxId::from_index(index), &mut numeric_clean);
            }
        }
        for (index, is_clean) in numeric_clean.iter().enumerate() {
            if !*is_clean {
                continue;
            }
            let id = LayoutBoxId::from_index(index);
            let old_cache = self.prior.world.numeric_cache(id).map(std::mem::take);
            if let Some(cache) = old_cache {
                *new.numeric_cache_mut(id) = cache;
            }
            let old = &mut self.prior.world.boxes[index];
            let table_cache = old.table_measure_cache.take();
            let complete_cache = old.complete_measure_cache.take();
            let inline_static_position = old.inline_static_position;
            let layout_box = &mut new.boxes[index];
            layout_box.table_measure_cache = table_cache;
            layout_box.complete_measure_cache = complete_cache;
            layout_box.inline_static_position = inline_static_position;
            layout_box.unrounded_layout = self.prior.numeric_unrounded[index];
            if let Some(tracks) = self.prior.world.resolved_grid_tracks.remove(&id) {
                new.resolved_grid_tracks.insert(id, tracks);
            }
            if let Some(details) = self.prior.world.native_grid_details.remove(&id) {
                new.native_grid_details.insert(id, details);
            }
            self.reused_numeric_count += 1;
        }
    }
}

fn mark_subtree_unclean<N>(world: &LayoutWorld<N>, root: LayoutBoxId, clean: &mut [bool])
where
    N: Copy + Debug + Eq + Hash,
{
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        clean[id.index()] = false;
        stack.extend(world.boxes[id.index()].children.iter().copied());
    }
}

/// Whether any box in `world` is floated. A float's position depends on
/// earlier siblings through the block formatting context, which the numeric
/// cache key does not capture, so such a world never reuses numeric results.
pub(crate) fn world_has_floats<N>(world: &LayoutWorld<N>) -> bool
where
    N: Copy + Debug + Eq + Hash,
{
    world
        .boxes
        .iter()
        .any(|layout_box| layout_box.style.is_floated())
}

static NUMERIC_REUSED_TOTAL: AtomicU64 = AtomicU64::new(0);
static VERIFY_MISMATCHES: AtomicU64 = AtomicU64::new(0);
static VERIFY_COMPARISONS: AtomicU64 = AtomicU64::new(0);

/// Number of incremental passes whose geometry differed from a full rebuild.
pub fn incremental_verify_mismatch_count() -> u64 {
    VERIFY_MISMATCHES.load(Ordering::Relaxed)
}

/// Number of incremental passes the differential check has compared.
pub fn incremental_verify_comparison_count() -> u64 {
    VERIFY_COMPARISONS.load(Ordering::Relaxed)
}

/// Total boxes whose numeric results a live pass moved from a retained world.
pub fn incremental_numeric_reused_total() -> u64 {
    NUMERIC_REUSED_TOTAL.load(Ordering::Relaxed)
}

pub(crate) fn record_numeric_reused(count: usize) {
    NUMERIC_REUSED_TOTAL.fetch_add(count as u64, Ordering::Relaxed);
}

pub(crate) fn record_verify_comparison(matched: bool) {
    VERIFY_COMPARISONS.fetch_add(1, Ordering::Relaxed);
    if !matched {
        VERIFY_MISMATCHES.fetch_add(1, Ordering::Relaxed);
    }
}
