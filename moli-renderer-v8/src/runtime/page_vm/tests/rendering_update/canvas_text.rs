// Tests grouped by behavior. Shared fixtures live in the parent module.
use super::*;

/// The 2D context's `font` uses the CSS shorthand grammar and Chromium's
/// serialization, and `measureText` shapes with the fonts CSS layout loads.
#[tokio::test(flavor = "current_thread")]
async fn canvas_font_parses_like_css_and_measure_text_uses_layout_fonts() {
    run_page_vm_async_test(async move {
        let loader =
            crate::network::ResourceRequestClient::new(&FetchConfig::default()).expect("loader");
        loader.set_optional_resource_fetch_mask(
            crate::protocol_types::OptionalResourceFetchMask::FONT,
        );
        let mut page_vm = test_page_vm_with_loader_and_document_url(
            &loader,
            Vec::new(),
            Url::parse("https://example.com/canvas-text.html")?,
        );
        let encoded_font = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../moli-layout/tests/fixtures/moli-ahem.woff2"
        ));
        let encoded = base64::engine::general_purpose::STANDARD.encode(encoded_font);
        page_vm.vm_mut().eval(&format!(
            r#"
document.head.innerHTML = `<style>
@font-face {{ font-family:MoliAhem; src:url(data:font/woff2;base64,{encoded}) format('woff2') }}
body {{ font-family:MoliAhem }}
canvas {{ font-size:20px }}
</style>`;
document.body.innerHTML = `<p>XX</p><canvas id=c></canvas>`;
'installed'
"#
        ))?;
        page_vm
            .vm_mut()
            .prime_document_lifecycle_processing_and_record_stylesheet_network_results();
        page_vm.vm_mut().publish_layout_for_test()?;

        let result = page_vm.vm_mut().eval(
            r#"(() => {
  const ctx = document.getElementById('c').getContext('2d');
  const fonts = [];
  for (const value of ['italic 500 22px/1.3 "Source Serif 4", Georgia, serif',
                       'bogus', '1.5em  MoliAhem ,serif', 'inherit']) {
    ctx.font = value;
    fonts.push(ctx.font);
  }
  ctx.font = '10px MoliAhem';
  const m = ctx.measureText('XX X');
  const empty = ctx.measureText('');
  return JSON.stringify({
    fonts,
    metrics: [m.width, m.actualBoundingBoxLeft, m.actualBoundingBoxRight,
              m.actualBoundingBoxAscent, m.actualBoundingBoxDescent,
              m.fontBoundingBoxAscent, m.fontBoundingBoxDescent,
              m.hangingBaseline, m.alphabeticBaseline, m.ideographicBaseline],
    empty: [empty.width, empty.fontBoundingBoxAscent, empty.actualBoundingBoxRight],
  });
})()"#,
        )?;
        let result: serde_json::Value = serde_json::from_str(&result)?;
        // The fixture advances 0.6em; its X outline spans the advance from the
        // baseline to 0.8em, and its ascent/descent are 0.8em/0.2em. Relative
        // sizes resolve against the canvas element's 20px font size.
        let expected = serde_json::json!({
            "fonts": [
                "italic 500 22px \"Source Serif 4\", Georgia, serif",
                "italic 500 22px \"Source Serif 4\", Georgia, serif",
                "30px MoliAhem, serif",
                "30px MoliAhem, serif",
            ],
            "metrics": [24, 0, 24, 8, 0, 8, 2, 6.4, 0, -2],
            "empty": [0, 8, 0],
        });
        assert_eq!(result, expected, "result={result}");
        Ok::<_, anyhow::Error>(())
    })
    .await
    .expect("canvas text fixture should run");
}
