// Tests grouped by behavior. Shared fixtures live in the parent module.
use super::*;

/// SVG content has no CSS box: client rects map its SVG bounding box through
/// the screen CTM, and text geometry uses the fonts CSS layout selects.
#[tokio::test(flavor = "current_thread")]
async fn svg_content_client_rects_and_text_boxes_follow_screen_ctm_and_fonts() {
    run_page_vm_async_test(async move {
        let loader =
            crate::network::ResourceRequestClient::new(&FetchConfig::default()).expect("loader");
        loader.set_optional_resource_fetch_mask(
            crate::protocol_types::OptionalResourceFetchMask::FONT,
        );
        let mut page_vm = test_page_vm_with_loader_and_document_url(
            &loader,
            Vec::new(),
            Url::parse("https://example.com/svg-client-geometry.html")?,
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
html,body {{ margin:0 }}
svg {{ font-family:MoliAhem }}
</style>`;
document.body.innerHTML = `<svg id=s width=300 height=150 viewBox="0 0 600 300"
    style="display:block;margin:5px;padding:4px;border:2px solid">
  <g id=g transform="translate(50,40)">
    <rect id=r width=120 height=40 stroke=black stroke-width=10 />
    <rect id=rot transform="rotate(90)" width=20 height=10 />
    <g id=empty></g>
    <text id=t x=100 y=100 font-size=20 text-anchor=middle>  XX <tspan id=ts>XXX</tspan>  </text>
    <text id=c x=0 y=200 font-size=20 dominant-baseline=central>X</text>
    <svg id=inner x=10 y=10 width=100 height=50 viewBox="0 0 10 5"><rect id=ir x=1 y=1 width=2 height=2 /></svg>
  </g>
  <defs><rect id=hidden width=5 height=5 /></defs>
</svg>`;
'installed'
"#
        ))?;
        page_vm
            .vm_mut()
            .prime_document_lifecycle_processing_and_record_stylesheet_network_results();
        page_vm.vm_mut().publish_layout_for_test()?;

        let geometry = page_vm.vm_mut().eval(
            r#"(() => {
  const out = {};
  const box = r => [r.x, r.y, r.width, r.height].map(v => Math.round(v * 100) / 100);
  for (const id of ['s', 'r', 'rot', 'g', 'empty', 't', 'ts', 'c', 'inner', 'ir', 'hidden']) {
    const element = document.getElementById(id);
    out[id] = { rect: box(element.getBoundingClientRect()), count: element.getClientRects().length };
  }
  for (const id of ['t', 'ts', 'c']) {
    const element = document.getElementById(id);
    out[id].bbox = box(element.getBBox());
    out[id].length = element.getComputedTextLength();
  }
  return JSON.stringify(out);
})()"#,
        )?;
        let geometry: serde_json::Value = serde_json::from_str(&geometry)?;
        // The outer box's content area starts at 5 + 2 + 4 = 11 and the
        // viewBox halves user units. The Ahem fixture advances 0.6em with a
        // 0.8em ascent and 0.2em descent.
        let expected = serde_json::json!({
            "s": { "rect": [5, 5, 312, 162], "count": 1 },
            "r": { "rect": [36, 31, 60, 20], "count": 1 },
            "rot": { "rect": [31, 31, 5, 10], "count": 1 },
            "g": { "rect": [31, 31, 73, 105], "count": 1 },
            "empty": { "rect": [36, 31, 0, 0], "count": 1 },
            "t": { "rect": [68, 73, 36, 10], "count": 1,
                   "bbox": [64, 84, 72, 20], "length": 72 },
            "ts": { "rect": [86, 73, 18, 10], "count": 1,
                    "bbox": [100, 84, 36, 20], "length": 36 },
            "c": { "rect": [36, 126, 6, 10], "count": 1,
                   "bbox": [0, 190, 12, 20], "length": 12 },
            "inner": { "rect": [46, 41, 10, 10], "count": 1 },
            "ir": { "rect": [46, 41, 10, 10], "count": 1 },
            "hidden": { "rect": [0, 0, 0, 0], "count": 0 },
        });
        assert_eq!(geometry, expected, "geometry={geometry}");
        Ok::<_, anyhow::Error>(())
    })
    .await
    .expect("SVG client geometry fixture should run");
}

/// HTML in a `<foreignObject>` gets CSS boxes of its own: box metrics are
/// untransformed CSS sizes, and client rects map them through the SVG screen
/// CTM, so a scaled diagram reports scaled label rects (Blink behaviour).
#[tokio::test(flavor = "current_thread")]
async fn foreign_object_content_has_css_boxes_mapped_through_screen_ctm() {
    run_page_vm_async_test(async move {
        let loader =
            crate::network::ResourceRequestClient::new(&FetchConfig::default()).expect("loader");
        loader.set_optional_resource_fetch_mask(
            crate::protocol_types::OptionalResourceFetchMask::FONT,
        );
        let mut page_vm = test_page_vm_with_loader_and_document_url(
            &loader,
            Vec::new(),
            Url::parse("https://example.com/svg-foreign-object-geometry.html")?,
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
html,body {{ margin:0 }}
.label {{ display:table-cell; white-space:nowrap; line-height:1.5; font:20px MoliAhem }}
</style>`;
document.body.innerHTML = `<svg id=s width=300 height=150 viewBox="0 0 600 300" style="display:block">
  <g transform="translate(50,40)">
    <foreignObject id=fo x=10 y=20 width=200 height=60>
      <div id=d class=label><span id=l>XXXX</span></div>
    </foreignObject>
  </g>
</svg>
<svg style="display:none"><foreignObject width=10 height=10><div id=hidden class=label>X</div></foreignObject></svg>`;
'installed'
"#
        ))?;
        page_vm
            .vm_mut()
            .prime_document_lifecycle_processing_and_record_stylesheet_network_results();
        page_vm.vm_mut().publish_layout_for_test()?;

        let geometry = page_vm.vm_mut().eval(
            r#"(() => {
  const out = {};
  const box = r => [r.x, r.y, r.width, r.height].map(v => Math.round(v * 100) / 100);
  for (const id of ['fo', 'd', 'l', 'hidden']) {
    const element = document.getElementById(id);
    out[id] = {
      rect: box(element.getBoundingClientRect()),
      count: element.getClientRects().length,
    };
    if (id !== 'fo') out[id].offset = [element.offsetWidth, element.offsetHeight];
  }
  return JSON.stringify(out);
})()"#,
        )?;
        let geometry: serde_json::Value = serde_json::from_str(&geometry)?;
        // viewBox halves user units; the content origin is the
        // <foreignObject>'s (10,20) after translate(50,40): client (30,30).
        // "XXXX" is 4 x 0.6em = 48px wide; the `font` shorthand resets
        // line-height to normal, a 20px line. Values match Chromium.
        let expected = serde_json::json!({
            "fo": { "rect": [30, 30, 100, 30], "count": 1 },
            "d": { "rect": [30, 30, 24, 10], "count": 1, "offset": [48, 20] },
            "l": { "rect": [30, 30, 24, 10], "count": 1, "offset": [48, 20] },
            "hidden": { "rect": [0, 0, 0, 0], "count": 0, "offset": [0, 0] },
        });
        assert_eq!(geometry, expected, "geometry={geometry}");
        Ok::<_, anyhow::Error>(())
    })
    .await
    .expect("foreignObject geometry fixture should run");
}

fn svg_hit_test_page_vm(name: &str, body: &str) -> anyhow::Result<PageVm> {
    let loader =
        crate::network::ResourceRequestClient::new(&FetchConfig::default()).expect("loader");
    let mut page_vm = test_page_vm_with_loader_and_document_url(
        &loader,
        Vec::new(),
        Url::parse(&format!("https://example.com/{name}.html"))?,
    );
    page_vm.vm_mut().eval(&format!(
        r#"
document.head.innerHTML = '<style>html,body{{margin:0}}</style>';
document.body.innerHTML = `{body}`;
globalThis.__hits = points => points.map(([x, y]) => {{
  const name = element => element ? (element.id || element.localName) : 'null';
  return name(document.elementFromPoint(x, y)) + '|' +
    document.elementsFromPoint(x, y).map(name).join(',');
}}).join(' ');
'installed'
"#
    ))?;
    page_vm.vm_mut().sync_live_document_style_sources();
    page_vm.vm_mut().publish_layout_for_test()?;
    Ok(page_vm)
}

/// Layout stops at the outer `<svg>` box; the point then resolves against
/// the SVG geometry below it, foremost shape first.
#[tokio::test(flavor = "current_thread")]
async fn elements_from_point_resolve_svg_shapes_below_the_outer_svg_box() {
    run_page_vm_async_test(async move {
        let mut page_vm = svg_hit_test_page_vm(
            "svg-shape-hit-test",
            r#"<svg id=s width=200 height=100 viewBox="0 0 100 50" style="display:block">
  <rect id=r x=5 y=5 width=40 height=40 fill=blue />
  <circle id=c cx=25 cy=25 r=10 fill=red />
  <g transform="translate(50,0)"><line id=l x1=0 y1=25 x2=40 y2=25 stroke=black stroke-width=4 /></g>
  <circle id=ring cx=80 cy=40 r=5 fill=none stroke=green stroke-width=2 />
</svg>"#,
        )?;
        // The viewBox halves user units: client (x, y) is user (x/2, y/2).
        assert_eq!(
            page_vm.vm_mut().eval(
                "__hits([[50, 50], [14, 14], [140, 53], [140, 57], [170, 80], [160, 80], [190, 10]])"
            )?,
            [
                "c|c,r,s,body,html",
                "r|r,s,body,html",
                "l|l,s,body,html",
                "s|s,body,html",
                "ring|ring,s,body,html",
                "s|s,body,html",
                "s|s,body,html",
            ]
            .join(" "),
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .expect("SVG shape hit-test fixture should run");
}

/// CSSOM View: a point inside the viewport always yields the document
/// element, even over no box; outside the viewport there is no hit.
#[tokio::test(flavor = "current_thread")]
async fn element_from_point_falls_back_to_the_root_inside_the_viewport() {
    run_page_vm_async_test(async move {
        let mut page_vm = svg_hit_test_page_vm(
            "root-fallback-hit-test",
            r#"<div id=box style="position:absolute;left:0;top:0;width:40px;height:40px"></div>"#,
        )?;
        assert_eq!(
            page_vm
                .vm_mut()
                .eval("__hits([[10, 10], [150, 150], [-1, 10]])")?,
            "box|box,html html|html null|",
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .expect("root fallback hit-test fixture should run");
}

/// SVG content can re-enable pointer events below an outer `<svg>` that is
/// transparent to them; content that inherits `none` lets the point through.
#[tokio::test(flavor = "current_thread")]
async fn svg_content_pointer_events_override_a_transparent_outer_svg() {
    run_page_vm_async_test(async move {
        let mut page_vm = svg_hit_test_page_vm(
            "svg-pointer-events-hit-test",
            r#"<div id=under style="position:absolute;left:0;top:0;width:200px;height:100px"></div>
<svg id=s width=200 height=100 style="position:absolute;left:0;top:0;pointer-events:none">
  <circle id=on cx=50 cy=50 r=20 style="pointer-events:auto" />
  <circle id=off cx=150 cy=50 r=20 />
</svg>"#,
        )?;
        let hits = page_vm.vm_mut().eval(
            r#"[[50, 50], [150, 50], [5, 5]].map(([x, y]) => {
  const list = document.elementsFromPoint(x, y);
  return [document.elementFromPoint(x, y)?.id, list[0]?.id, list[1]?.id, list.includes(document.getElementById('s'))].join(',');
}).join(' ')"#,
        )?;
        assert_eq!(hits, "on,on,under,false under,under,,false under,under,,false");
        Ok::<_, anyhow::Error>(())
    })
    .await
    .expect("SVG pointer-events hit-test fixture should run");
}
