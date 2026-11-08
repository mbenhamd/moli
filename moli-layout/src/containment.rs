//! Box-owned CSS containment eligibility and derived layout/paint roles.
//!
//! Stylo owns computed containment bits, but whether those bits apply depends
//! on the exact principal box produced by construction. Keep that used-value
//! decision here so containing-block selection, stacking, geometry projection,
//! hit testing, and paint clipping all consume one Chromium-aligned answer.

use std::{fmt::Debug, hash::Hash};

use taffy::Size;

use crate::world::{LayoutBox, LayoutBoxId, LayoutBoxKind, LayoutWorld};

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

    /// Physical axes under size containment on this box.
    ///
    /// Native text controls and selects keep their control-sized box, as
    /// measured against Chromium for `input`, `textarea` and `select`
    /// (`select` matches main rather than Chromium's arrow-only width).
    pub(crate) fn size_contained_axes(&self) -> Size<bool> {
        if !self.is_eligible_for_size_containment()
            || self.style.taffy.item_is_compressible_replaced
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
