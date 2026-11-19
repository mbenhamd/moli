//! `<foreignObject>` content of SVG images.
//!
//! `usvg` renders an SVG image's vector content and omits `<foreignObject>`.
//! That HTML belongs to CSS, so it runs through the same style, layout and
//! paint as a page: the image's XML becomes an inert Document in this page's
//! DOM arena, like one from `DOMParser`, with its own style sheets. Scripts in
//! it never run and it fetches no subresources, as in Blink's isolated SVG
//! image pages.

use std::{collections::VecDeque, sync::Arc};

use moli_layout::{LayoutViewport, PaintRect, PaintSnapshot, PaintTransform2D};

use super::PageScriptHost;
use crate::{context_bootstrap::svg_runtime::screen, document_runtime::DomHandle};

/// Parsed documents kept for repeated draws of the same image. An evicted
/// Document releases its style world and layout services. Its nodes stay in
/// the page's DOM arena, which never frees a node, like a `DOMParser`
/// Document that script dropped.
const MAX_CACHED_SVG_IMAGE_DOCUMENTS: usize = 16;

/// Documents in insertion order, keyed by the decoded image's source buffer.
/// The entry holds that buffer, so its address cannot be reused while cached,
/// and a lookup never hashes the source bytes.
#[derive(Default)]
pub(super) struct SvgImageDocuments {
    entries: VecDeque<(Arc<[u8]>, (DomHandle, DomHandle))>,
}

impl SvgImageDocuments {
    fn get(&self, source: &Arc<[u8]>) -> Option<(DomHandle, DomHandle)> {
        self.entries
            .iter()
            .find(|(cached, _)| Arc::ptr_eq(cached, source))
            .map(|(_, documents)| *documents)
    }
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
        let foreign_objects = {
            let Some(document) = self.dom_host().owner_document_handle(root) else {
                return Vec::new();
            };
            // Every read of the image's styles uses its Document's own
            // context. A context-free read would build that Document's style
            // world without the sheets of its unconnected `<style>` elements,
            // and layout would then reuse that world.
            let mut styles =
                crate::layout_renderer::foreign_object_style_scope(self, document, viewport);
            screen::svg_image_foreign_objects(self, &mut styles, root, image_size)
        };
        foreign_objects
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
        if let Some((_, root)) = self.svg_image_documents.get(source) {
            return Some(root);
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
        let root = self
            .dom_host()
            .child_handles(document)
            .find(|&node| self.dom_host().node(node).is_some_and(|n| n.is_element()))?;
        if self.svg_image_documents.entries.len() >= MAX_CACHED_SVG_IMAGE_DOCUMENTS
            && let Some((_, (evicted, _))) = self.svg_image_documents.entries.pop_front()
        {
            self.retire_svg_image_document(evicted);
        }
        self.svg_image_documents
            .entries
            .push_back((source.clone(), (document, root)));
        Some(root)
    }

    fn retire_svg_image_document(&mut self, document: DomHandle) {
        self.document_layout_state
            .borrow_mut()
            .retire_embedded_document(document);
        self.style_engine.retire_document_style_world(document);
    }
}
