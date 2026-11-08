use moli_layout::FrozenLayoutTree;

use crate::document_runtime::DomHandle;

/// Inputs a published layout was built from.
///
/// These are the DOM, style-source, used-geometry and visual-resource
/// generations the screencast visual-state token compares, plus the layout
/// viewport and media environment of the pass. Under the live-geometry
/// policy a synchronous read lays out again only when this key differs, so
/// repeated reads of an unchanged Document reuse the same frozen tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LayoutInputKey {
    pub(super) dom: u64,
    pub(super) styles: Vec<(DomHandle, u64, u64)>,
    pub(super) used_geometry: u64,
    pub(super) resources: u64,
    /// CSS width, CSS height and device-pixel-ratio bits.
    pub(super) viewport: (u32, u32, u32),
    pub(super) environment: crate::style_engine::StyloStyleEnvironment,
}

/// What can move layout without changing [`LayoutInputKey`]'s other fields:
/// DOM changes other than style-only attributes, and style invalidations.
pub(super) type LayoutDirtyMark = (u64, u64);

struct LatestFrozenLayout {
    document: DomHandle,
    dirty_mark: LayoutDirtyMark,
    /// Recorded only under the live-geometry policy; frozen policies never
    /// compare inputs.
    inputs: Option<LayoutInputKey>,
    tree: Box<FrozenLayoutTree<DomHandle>>,
}

/// Single-slot storage for the latest successful frozen layout tree.
///
/// This owns exactly one recursively frozen snapshot. It has no separately
/// keyed child tree, working layout world, source index, hit-test index, Taffy
/// cache, style borrow, pass diagnostics, paint snapshot, timer, or invalidation
/// policy beyond the input key recorded at publication. The first geometry
/// demand initializes it; under the live-geometry policy a later demand also
/// replaces it once that key changes. Screenshots, screencast frames and
/// explicit layout refreshes replace this published layout; print
/// projections never enter this cache.
#[derive(Default)]
pub(super) struct LatestLayoutTreeCache {
    latest: Option<LatestFrozenLayout>,
}

impl LatestLayoutTreeCache {
    pub(super) fn get(&self, document: DomHandle) -> Option<&FrozenLayoutTree<DomHandle>> {
        self.latest
            .as_ref()
            .filter(|snapshot| snapshot.document == document)
            .map(|snapshot| snapshot.tree.as_ref())
    }

    pub(super) fn get_for_root(&self, root: DomHandle) -> Option<&FrozenLayoutTree<DomHandle>> {
        self.latest.as_ref()?.tree.tree_for_root(root)
    }

    /// Whether the latest tree for `document` was built from `inputs`.
    pub(super) fn is_current(&self, document: DomHandle, inputs: &LayoutInputKey) -> bool {
        self.latest.as_ref().is_some_and(|snapshot| {
            snapshot.document == document && snapshot.inputs.as_ref() == Some(inputs)
        })
    }

    /// Whether nothing that can affect layout changed since the latest tree:
    /// the inputs match apart from the DOM version, and `dirty_mark` matches.
    pub(super) fn is_layout_clean(
        &self,
        document: DomHandle,
        inputs: &LayoutInputKey,
        dirty_mark: LayoutDirtyMark,
    ) -> bool {
        self.latest.as_ref().is_some_and(|snapshot| {
            snapshot.document == document
                && snapshot.dirty_mark == dirty_mark
                && snapshot.inputs.as_ref().is_some_and(|published| {
                    published.styles == inputs.styles
                        && published.used_geometry == inputs.used_geometry
                        && published.resources == inputs.resources
                        && published.viewport == inputs.viewport
                        && published.environment == inputs.environment
                })
        })
    }

    pub(super) fn publish(
        &mut self,
        document: DomHandle,
        inputs: Option<LayoutInputKey>,
        dirty_mark: LayoutDirtyMark,
        tree: FrozenLayoutTree<DomHandle>,
    ) {
        self.latest = Some(LatestFrozenLayout {
            document,
            dirty_mark,
            inputs,
            tree: Box::new(tree),
        });
    }

    pub(super) fn clear(&mut self) {
        self.latest = None;
    }

    #[cfg(test)]
    pub(super) fn observability(
        &self,
    ) -> Option<(DomHandle, moli_layout::LayoutTreeRetentionMetrics)> {
        self.latest
            .as_ref()
            .map(|snapshot| (snapshot.document, snapshot.tree.retention_metrics()))
    }
}
