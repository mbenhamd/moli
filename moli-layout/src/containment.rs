//! Box-owned CSS containment eligibility and derived layout/paint roles.
//!
//! Stylo owns computed containment bits, but whether those bits apply depends
//! on the exact principal box produced by construction. Keep that used-value
//! decision here so containing-block selection, stacking, geometry projection,
//! hit testing, and paint clipping all consume one Chromium-aligned answer.

use std::{fmt::Debug, hash::Hash};

use taffy::Size;

use crate::{
    LayoutQueryContainerSize,
    world::{LayoutBox, LayoutBoxId, LayoutBoxKind, LayoutWorld},
};

impl<N> LayoutBox<N> {
    /// Mirrors Blink's `LayoutObject::IsBox()` boundary.
    ///
    /// Non-atomic inline content is flattened into its owner IFC and has no
    /// independent CSS box on which transform containing-block semantics can
    /// apply. Table structural boxes are still boxes even though containment
    /// has a narrower eligibility rule for them.
    pub(crate) fn is_css_box(&self) -> bool {
        !self.inline_flattened
            && !matches!(
                self.kind,
                LayoutBoxKind::Text
                    | LayoutBoxKind::LineBreak
                    | LayoutBoxKind::PrincipalInline
                    | LayoutBoxKind::InlineContinuation
            )
    }

    /// Mirrors Blink's `IsEligibleForPaintOrLayoutContainment()` overrides.
    ///
    /// LayoutBox is eligible by default. Internal table sections, columns and
    /// rows explicitly reject paint/layout containment; table wrappers, cells,
    /// captions, replaced content and atomic inlines remain eligible.
    pub(crate) fn is_eligible_for_paint_or_layout_containment(&self) -> bool {
        self.is_css_box()
            && !matches!(
                self.kind,
                LayoutBoxKind::TableRowGroup
                    | LayoutBoxKind::TableHeaderGroup
                    | LayoutBoxKind::TableFooterGroup
                    | LayoutBoxKind::TableColumnGroup
                    | LayoutBoxKind::TableColumn
                    | LayoutBoxKind::TableRow
                    | LayoutBoxKind::AnonymousTableRowGroup
                    | LayoutBoxKind::AnonymousTableRow
            )
    }

    /// Mirrors Blink's `LayoutObject::IsEligibleForSizeContainment()`.
    ///
    /// Size containment skips every box whose size the table algorithm owns,
    /// which is narrower than paint/layout containment: wrappers and cells are
    /// ineligible too, while captions stay eligible.
    pub(crate) fn is_eligible_for_size_containment(&self) -> bool {
        self.is_css_box()
            && !matches!(
                self.kind,
                LayoutBoxKind::TableWrapper
                    | LayoutBoxKind::InlineTableWrapper
                    | LayoutBoxKind::AnonymousTableWrapper
                    | LayoutBoxKind::TableRowGroup
                    | LayoutBoxKind::TableHeaderGroup
                    | LayoutBoxKind::TableFooterGroup
                    | LayoutBoxKind::TableColumnGroup
                    | LayoutBoxKind::TableColumn
                    | LayoutBoxKind::TableRow
                    | LayoutBoxKind::TableCell
                    | LayoutBoxKind::AnonymousTableRowGroup
                    | LayoutBoxKind::AnonymousTableRow
                    | LayoutBoxKind::AnonymousTableCell
            )
    }

    /// Whether this box's contents are skipped, as Blink's display lock does
    /// for `content-visibility: hidden`. The contents are still laid out, so
    /// geometry queries answer from them, as Blink's forced layout for a
    /// locked subtree does, but they are not painted, hit-tested, or part
    /// of an ancestor's scrollable overflow.
    pub(crate) fn skips_contents(&self) -> bool {
        self.anonymous_reason == Some(crate::LayoutAnonymousReason::ClosedDetailsContent)
    }

    pub(crate) fn is_pseudo_box(&self) -> bool {
        matches!(
            self.kind,
            LayoutBoxKind::PseudoMarker | LayoutBoxKind::PseudoBefore | LayoutBoxKind::PseudoAfter
        )
    }

    /// Physical axes under size containment on this box.
    ///
    /// Native text controls and selects keep their control-sized box, as
    /// measured against Chromium for `input`, `textarea` and `select`
    /// (`select` matches main rather than Chromium's arrow-only width).
    /// Moli has no rendered-legend layout, so a contained fieldset would lose
    /// its legend too; it keeps main's size instead.
    pub(crate) fn size_contained_axes(&self) -> Size<bool> {
        let is_fieldset = self.element_semantics.as_ref().is_some_and(|semantics| {
            matches!(
                semantics.category,
                crate::LayoutElementCategory::FormControl(crate::LayoutFormControlKind::FieldSet)
            )
        });
        if !self.is_eligible_for_size_containment()
            || self.style.taffy.item_is_compressible_replaced
            || is_fieldset
        {
            return Size {
                width: false,
                height: false,
            };
        }
        self.style.size_containment_axes()
    }

    pub(crate) fn creates_stacking_context(
        &self,
        is_root: bool,
        is_flex_or_grid_item: bool,
    ) -> bool {
        self.style.creates_stacking_context(
            is_root,
            is_flex_or_grid_item,
            self.is_eligible_for_paint_or_layout_containment(),
        )
    }

    pub(crate) fn clips_descendant_paint(&self) -> bool {
        self.style.clips_overflow()
            || (self.is_eligible_for_paint_or_layout_containment()
                && self.style.applies_paint_containment())
    }
}

impl<N> LayoutWorld<N>
where
    N: Copy + Debug + Eq + Hash,
{
    /// Tree-ordered content-box sizes of this pass's query containers, plus
    /// their maximum nesting depth (0 with no containers, 1 for an unnested
    /// one).
    pub(crate) fn query_container_sizes(&self) -> (Vec<(N, LayoutQueryContainerSize)>, usize) {
        let mut sizes = Vec::new();
        let mut depth = 0;
        for layout_box in &self.boxes {
            let axes = layout_box.style.query_container_physical_axes();
            if (!axes.width && !axes.height)
                || !layout_box.is_eligible_for_size_containment()
                || layout_box.is_pseudo_box()
            {
                continue;
            }
            let Some(source) = layout_box.source() else {
                continue;
            };
            let layout = layout_box.final_layout();
            let insets = layout_box.style.scrollbar_layout_insets(false);
            let content_width = (layout.size.width
                - layout.border.left
                - layout.border.right
                - layout.padding.left
                - layout.padding.right
                - insets.left
                - insets.right)
                .max(0.0);
            let content_height = (layout.size.height
                - layout.border.top
                - layout.border.bottom
                - layout.padding.top
                - layout.padding.bottom
                - insets.top
                - insets.bottom)
                .max(0.0);
            // Blink answers from the size known before the box's contents
            // are laid out, so an auto block size with a block-flow parent
            // reads as zero even when intrinsic or minimum sizes grow it.
            let block_size_was_unknown = layout_box.style.taffy.size.height.is_auto()
                && layout_box.style.taffy.position != taffy::Position::Absolute
                && !layout_box.layout_parent.is_some_and(|parent| {
                    matches!(
                        self.boxes[parent.index()].style.taffy.display,
                        taffy::Display::Flex | taffy::Display::Grid
                    )
                });
            let content_height = if axes.height && block_size_was_unknown {
                0.0
            } else {
                content_height
            };
            sizes.push((
                source,
                LayoutQueryContainerSize {
                    width: axes.width.then_some(content_width),
                    height: axes.height.then_some(content_height),
                },
            ));
            let mut nesting = 1;
            let mut ancestor = layout_box.layout_parent;
            while let Some(id) = ancestor {
                let parent = &self.boxes[id.index()];
                let parent_axes = parent.style.query_container_physical_axes();
                if parent_axes.width || parent_axes.height {
                    nesting += 1;
                }
                ancestor = parent.layout_parent;
            }
            depth = depth.max(nesting);
        }
        (sizes, depth)
    }

    /// Resolves containing-block capability from both computed style and the
    /// box's identity in this layout tree.
    ///
    /// Filter effects establish containing blocks on inline LayoutObjects too,
    /// but the Filter Effects specifications explicitly exempt the document
    /// element. Keeping that identity-dependent rule on `LayoutWorld` avoids
    /// conflating the root of a subtree layout source with the document
    /// element.
    pub(crate) fn establishes_positioned_containing_block(&self, id: LayoutBoxId) -> bool {
        let layout_box = &self.boxes[id.index()];
        layout_box.style.establishes_positioned_containing_block(
            self.is_document_element(id),
            layout_box.is_css_box(),
            layout_box.is_eligible_for_paint_or_layout_containment(),
        )
    }

    pub(crate) fn establishes_fixed_containing_block(&self, id: LayoutBoxId) -> bool {
        let layout_box = &self.boxes[id.index()];
        layout_box.style.establishes_fixed_containing_block(
            self.is_document_element(id),
            layout_box.is_css_box(),
            layout_box.is_eligible_for_paint_or_layout_containment(),
        )
    }
}
