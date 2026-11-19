//! `<foreignObject>` content of SVG images.
//!
//! `usvg` renders an SVG image's vector content and omits `<foreignObject>`.
//! That HTML belongs to CSS, so it runs through the same style, layout and
//! paint as a page: the image's XML becomes an inert Document in this page's
//! DOM arena, like one from `DOMParser`, with its own style sheets. Scripts in
//! it never run and it fetches no subresources, as in Blink's isolated SVG
//! image pages.

use std::{collections::HashMap, sync::Arc};

use moli_layout::{LayoutViewport, PaintRect, PaintSnapshot, PaintTransform2D};

use super::PageScriptHost;
use crate::{context_bootstrap::svg_runtime::screen, document_runtime::DomHandle};

/// Parsed documents kept for repeated draws. The DOM arena never frees nodes,
/// so sharing one Document per distinct source is what bounds their cost.
const MAX_CACHED_SVG_IMAGE_DOCUMENTS: usize = 16;

#[derive(Default)]
pub(super) struct SvgImageDocuments {
    roots: HashMap<Arc<[u8]>, DomHandle>,
}

/// Painted content of one `<foreignObject>`.
pub(crate) struct SvgImageForeignContent {
    pub(crate) snapshot: PaintSnapshot,
    /// The `<foreignObject>` box in its content space.
    pub(crate) clip: PaintRect,
    /// From that content space to image pixels.
    pub(crate) content_to_image: PaintTransform2D,
}

impl PageScriptHost {
    /// Paints every rendered `<foreignObject>` of an SVG image of `image_size`.
    /// A document that cannot be parsed or laid out contributes nothing.
    pub(crate) fn svg_image_foreign_content(
        &mut self,
        source: &Arc<[u8]>,
        image_size: (f64, f64),
    ) -> Vec<SvgImageForeignContent> {
        let Some(root) = self.svg_image_document_root(source) else {
            return Vec::new();
        };
        // The `<foreignObject>` box resolves its percentage size, a CSS
        // presentation attribute, against the image's user viewport.
        let (viewport_width, viewport_height) =
            screen::svg_image_user_viewport(self, root, image_size);
        let viewport = LayoutViewport::new(
            viewport_width.ceil() as u32,
            viewport_height.ceil() as u32,
            1.0,
        );
        screen::svg_image_foreign_objects(self, root, image_size)
            .into_iter()
            .filter_map(|foreign_object| {
                let snapshot = self
                    .paint_foreign_object_content(foreign_object.handle, viewport)
                    .inspect_err(|error| {
                        tracing::debug!(%error, "SVG image foreignObject layout failed");
                    })
                    .ok()
                    .flatten()?;
                let matrix = foreign_object.content_to_image;
                Some(SvgImageForeignContent {
                    snapshot,
                    clip: PaintRect::new(
                        0.0,
                        0.0,
                        foreign_object.width as f32,
                        foreign_object.height as f32,
                    ),
                    content_to_image: PaintTransform2D::new([
                        matrix.a, matrix.b, matrix.c, matrix.d, matrix.e, matrix.f,
                    ]),
                })
            })
            .collect()
    }

    fn svg_image_document_root(&mut self, source: &Arc<[u8]>) -> Option<DomHandle> {
        if let Some(root) = self.svg_image_documents.roots.get(source) {
            return Some(*root);
        }
        let xml = std::str::from_utf8(source).ok()?;
        let url = url::Url::parse("about:blank").ok()?;
        let document = self.create_detached_xml_document_with_url(url);
        moli_parser::XmlParser.parse_inert_tree_into_document(
            self.dom_host_mut(),
            document,
            xml,
        )?;
        // The image's own `<style>` sheets apply inside it, as in any Document.
        self.initialize_style_element_sources_for_document(document);
        {
            let c = self.dom_host().stylesheet_candidate_handles_for_tree_scope(document);
            eprintln!("DBG-SVGIMG candidates={}", c.len());
            for &o in c.iter() {
                eprintln!("DBG-SVGIMG owner text={:?}", self.style_engine.owner_style_sheet_text_with_host(self.dom_host(), o).map(|t| t.len()));
            }
        }
        let root = self
            .dom_host()
            .child_handles(document)
            .find(|&node| self.dom_host().node(node).is_some_and(|n| n.is_element()))?;
        if self.svg_image_documents.roots.len() >= MAX_CACHED_SVG_IMAGE_DOCUMENTS {
            self.svg_image_documents.roots.clear();
        }
        self.svg_image_documents.roots.insert(source.clone(), root);
        Some(root)
    }
}
