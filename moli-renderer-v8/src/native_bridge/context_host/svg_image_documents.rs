//! `<foreignObject>` content of SVG images.
//!
//! `usvg` renders an SVG image's vector content and omits `<foreignObject>`.
//! That HTML belongs to CSS, so it runs through the same style, layout and
//! paint as a page: the image's XML becomes an inert Document in this page's
//! DOM arena, like one from `DOMParser`, with its own style sheets. Scripts in
//! it never run and it fetches no subresources, as in Blink's isolated SVG
//! image pages. The one exception is a `data:` raster image, which needs no
//! fetch: an `<img>` or CSS image inside a `<foreignObject>` decodes it from
//! the URL, as Blink does.

use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use moli_layout::{LayoutViewport, PaintRect, PaintSnapshot, PaintTransform2D};

use super::{
    PageScriptHost,
    image_resources::{InlineImagePermit, ReadyImageForLayout},
};
use crate::{context_bootstrap::svg_runtime::screen, document_runtime::DomHandle};

/// Parsed documents kept for repeated draws of the same image. An evicted
/// Document releases its style world and layout services. Its nodes stay in
/// the page's DOM arena, which never frees a node, like a `DOMParser`
/// Document that script dropped.
const MAX_CACHED_SVG_IMAGE_DOCUMENTS: usize = 16;

/// Documents in insertion order, keyed by the image's source bytes. A draw
/// of an image whose decoded resource is shared finds its entry by pointer.
/// A draw that decoded the source again finds it by length and then bytes.
/// The entry holds its buffer, so that address cannot be reused while cached.
#[derive(Default)]
pub(super) struct SvgImageDocuments {
    entries: VecDeque<SvgImageDocument>,
}

struct SvgImageDocument {
    source: Arc<[u8]>,
    document: DomHandle,
    root: DomHandle,
    /// `data:` raster images by URL, decoded on first use. A URL that does
    /// not decode, or does not fit the image budget, is kept as `None`.
    /// Evicting the document drops the pixels and returns their budget.
    inline_images: RefCell<HashMap<String, Option<(ReadyImageForLayout, InlineImagePermit)>>>,
}

impl SvgImageDocuments {
    fn get(&self, source: &Arc<[u8]>) -> Option<DomHandle> {
        self.entries
            .iter()
            .find(|entry| {
                Arc::ptr_eq(&entry.source, source)
                    || (entry.source.len() == source.len() && entry.source[..] == source[..])
            })
            .map(|entry| entry.root)
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
        // Without real layout nothing can be painted, so parse nothing.
        if !self.layout_policy.uses_real_layout() {
            return Vec::new();
        }
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
        if let Some(root) = self.svg_image_documents.get(source) {
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
            && let Some(evicted) = self.svg_image_documents.entries.pop_front()
        {
            self.retire_svg_image_document(evicted.document);
        }
        self.svg_image_documents
            .entries
            .push_back(SvgImageDocument {
                source: source.clone(),
                document,
                root,
                inline_images: RefCell::default(),
            });
        Some(root)
    }

    /// The `data:` raster image `url` names inside the SVG image `document`,
    /// or `None` when `document` is not a cached SVG image document or the
    /// URL yields no raster image.
    pub(crate) fn svg_image_inline_image(
        &self,
        document: DomHandle,
        url: &str,
    ) -> Option<ReadyImageForLayout> {
        let entry = self
            .svg_image_documents
            .entries
            .iter()
            .find(|entry| entry.document == document)?;
        if let Some(cached) = entry.inline_images.borrow().get(url) {
            return cached.as_ref().map(|(ready, _)| ready.clone());
        }
        let decoded = self.decode_inline_data_raster_image(url);
        let ready = decoded.as_ref().map(|(ready, _)| ready.clone());
        entry
            .inline_images
            .borrow_mut()
            .insert(url.to_owned(), decoded);
        ready
    }

    fn retire_svg_image_document(&mut self, document: DomHandle) {
        self.document_layout_state
            .borrow_mut()
            .retire_embedded_document(document);
        self.style_engine.retire_document_style_world(document);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::native::NativeNodeId;

    #[test]
    fn cached_documents_are_found_by_equal_bytes_and_missed_by_length() {
        let mut documents = SvgImageDocuments::default();
        documents.entries.push_back(SvgImageDocument {
            source: Arc::from(&b"<svg/>"[..]),
            document: NativeNodeId::new(1),
            root: NativeNodeId::new(2),
            inline_images: RefCell::default(),
        });

        let equal_bytes: Arc<[u8]> = Arc::from(&b"<svg/>"[..]);
        let longer: Arc<[u8]> = Arc::from(&b"<svg/> "[..]);
        assert_eq!(documents.get(&equal_bytes), Some(NativeNodeId::new(2)));
        assert_eq!(documents.get(&longer), None);
    }
}
