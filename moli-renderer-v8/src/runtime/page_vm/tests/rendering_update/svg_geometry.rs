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

/// Even-odd leaves the inner square of two same-direction subpaths unfilled;
/// non-zero fills it (winding 2).
#[tokio::test(flavor = "current_thread")]
async fn svg_fill_rule_decides_whether_an_inner_subpath_is_a_hole() {
    run_page_vm_async_test(async move {
        let mut page_vm = svg_hit_test_page_vm(
            "svg-fill-rule-hit-test",
            r#"<svg id=s width=200 height=100 style="display:block">
  <path id=eo fill-rule=evenodd d="M0 0H80V80H0Z M20 20H60V60H20Z" />
  <path id=nz transform="translate(100,0)" d="M0 0H80V80H0Z M20 20H60V60H20Z" />
</svg>"#,
        )?;
        assert_eq!(
            page_vm
                .vm_mut()
                .eval("__hits([[40, 40], [10, 40], [140, 40]])")?,
            ["s|s,body,html", "eo|eo,s,body,html", "nz|nz,s,body,html"].join(" "),
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .expect("SVG fill-rule hit-test fixture should run");
}

/// A `visibility: hidden` shape is not painted and so not hit, even below a
/// visible group; the point falls through to the `<svg>`.
#[tokio::test(flavor = "current_thread")]
async fn svg_hidden_shape_under_a_visible_group_falls_through_to_the_svg() {
    run_page_vm_async_test(async move {
        let mut page_vm = svg_hit_test_page_vm(
            "svg-visibility-hit-test",
            r#"<svg id=s width=200 height=100 style="display:block">
  <g id=g><rect id=hidden width=80 height=80 style="visibility:hidden" /></g>
  <rect id=shown x=100 width=80 height=80 />
</svg>"#,
        )?;
        assert_eq!(
            page_vm.vm_mut().eval("__hits([[40, 40], [140, 40]])")?,
            ["s|s,body,html", "shown|shown,s,body,html"].join(" "),
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .expect("SVG visibility hit-test fixture should run");
}

/// Two dashed strokes of one ring, as in a donut chart. The circumference is
/// 2π·50 ≈ 314.16, so each dash covers a quarter (78.54). `a` starts at 3
/// o'clock and runs clockwise to 6; `b` is shifted one quarter on, 6 to 9.
/// The top of the ring is a gap in both.
#[tokio::test(flavor = "current_thread")]
async fn svg_dashed_ring_segments_are_hit_only_on_their_own_arc() {
    run_page_vm_async_test(async move {
        let mut page_vm = svg_hit_test_page_vm(
            "svg-dashed-stroke-hit-test",
            r#"<svg id=s width=200 height=200 style="display:block">
  <circle id=a cx=100 cy=100 r=50 fill=none stroke=red stroke-width=20 stroke-dasharray="78.54 235.62" />
  <circle id=b cx=100 cy=100 r=50 fill=none stroke=blue stroke-width=20 stroke-dasharray="78.54 235.62" stroke-dashoffset="-78.54" />
</svg>"#,
        )?;
        // Points on the ring at 45°, 135° and 270° (y grows downward).
        assert_eq!(
            page_vm
                .vm_mut()
                .eval("__hits([[135, 135], [65, 135], [100, 50]])")?,
            ["a|a,s,body,html", "b|b,s,body,html", "s|s,body,html"].join(" "),
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .expect("SVG dashed stroke hit-test fixture should run");
}

/// A pseudo-element with `pointer-events: none` over its owner does not
/// hide the owner's own box.
#[tokio::test(flavor = "current_thread")]
async fn pointer_events_none_pseudo_element_does_not_hide_its_owner() {
    run_page_vm_async_test(async move {
        let mut page_vm = svg_hit_test_page_vm(
            "pseudo-pointer-events-hit-test",
            r#"<style>#o::after{content:"";position:absolute;left:0;top:0;width:100px;height:100px;pointer-events:none}</style>
<div id=o style="position:relative;width:50px;height:50px"></div>"#,
        )?;
        assert_eq!(
            page_vm.vm_mut().eval("__hits([[25, 25], [75, 25]])")?,
            ["o|o,body,html", "body|body,html"].join(" "),
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .expect("pseudo-element pointer-events hit-test fixture should run");
}

/// Dashes end in the stroke's line caps: none past a butt end, a disk for
/// round, a half-width square for square.
#[tokio::test(flavor = "current_thread")]
async fn svg_dash_ends_follow_the_stroke_linecap() {
    run_page_vm_async_test(async move {
        let mut page_vm = svg_hit_test_page_vm(
            "svg-dash-linecap-hit-test",
            r#"<svg id=s width=200 height=120 style="display:block">
  <line id=b x1=10 y1=20 x2=190 y2=20 stroke=red stroke-width=20 stroke-dasharray="10 20" />
  <line id=r x1=10 y1=60 x2=190 y2=60 stroke=red stroke-width=20 stroke-dasharray="10 20" stroke-linecap=round />
  <line id=q x1=10 y1=100 x2=190 y2=100 stroke=red stroke-width=20 stroke-dasharray="10 20" stroke-linecap=square />
</svg>"#,
        )?;
        // The first dash runs from x=10 to x=20.
        assert_eq!(
            page_vm
                .vm_mut()
                .eval("__hits([[15, 20], [25, 20], [25, 60], [29, 68], [29, 108]])")?,
            [
                "b|b,s,body,html",
                "s|s,body,html",
                "r|r,s,body,html",
                "s|s,body,html",
                "q|q,s,body,html",
            ]
            .join(" "),
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .expect("SVG dash linecap hit-test fixture should run");
}

/// Percentage stroke lengths resolve against the viewport diagonal
/// (200 here), and `pathLength` rescales the dash pattern.
#[tokio::test(flavor = "current_thread")]
async fn svg_stroke_percentages_and_path_length_scale_the_stroke() {
    run_page_vm_async_test(async move {
        let mut page_vm = svg_hit_test_page_vm(
            "svg-stroke-percent-hit-test",
            r#"<svg id=s width=200 height=200 style="display:block">
  <line id=w x1=0 y1=20 x2=200 y2=20 stroke=red stroke-width="10%" />
  <line id=d x1=0 y1=60 x2=200 y2=60 stroke=red stroke-width=10 stroke-dasharray="5% 10%" />
  <line id=p x1=0 y1=100 x2=200 y2=100 stroke=red stroke-width=10 stroke-dasharray="10 10" pathLength=100 />
</svg>"#,
        )?;
        assert_eq!(
            page_vm.vm_mut().eval(
                "__hits([[100, 28], [100, 32], [5, 60], [20, 60], [15, 100], [25, 100]])"
            )?,
            [
                "w|w,s,body,html",
                "s|s,body,html",
                "d|d,s,body,html",
                "s|s,body,html",
                "p|p,s,body,html",
                "s|s,body,html",
            ]
            .join(" "),
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .expect("SVG stroke percentage hit-test fixture should run");
}

/// Percentage geometry resolves against the viewport. An `<svg>` with
/// clipped or `<switch>` content, which the geometry model does not decide,
/// keeps the outer `<svg>` answer everywhere, even over a plain shape.
#[tokio::test(flavor = "current_thread")]
async fn svg_percent_geometry_resolves_and_clipped_content_stays_coarse() {
    run_page_vm_async_test(async move {
        let mut page_vm = svg_hit_test_page_vm(
            "svg-coarse-fallback-hit-test",
            r#"<svg id=s width=200 height=200 style="display:block">
  <rect id=r width="50%" height="50%" fill=red />
</svg>
<svg id=k width=200 height=200 style="display:block">
  <defs><clipPath id=c><rect x=0 y=100 width=50 height=50 /></clipPath></defs>
  <rect id=r2 width=100 height=100 fill=red />
  <g clip-path="url(#c)"><rect id=c1 x=0 y=100 width=100 height=100 fill=blue /></g>
  <switch>
    <rect id=w1 x=100 y=100 width=100 height=100 fill=green systemLanguage="zz" />
    <rect id=w2 x=100 y=100 width=100 height=100 fill=green />
  </switch>
</svg>"#,
        )?;
        assert_eq!(
            page_vm
                .vm_mut()
                .eval("__hits([[75, 75], [150, 150], [75, 275], [75, 375], [150, 350]])")?,
            [
                "r|r,s,body,html",
                "s|s,body,html",
                "k|k,body,html",
                "k|k,body,html",
                "k|k,body,html",
            ]
            .join(" "),
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .expect("SVG coarse fallback hit-test fixture should run");
}

/// A paint server reference that does not resolve paints its fallback, and
/// nothing without one; unpainted shapes and text are not hit.
#[tokio::test(flavor = "current_thread")]
async fn svg_unresolved_paint_and_unpainted_text_are_not_hit() {
    run_page_vm_async_test(async move {
        let mut page_vm = svg_hit_test_page_vm(
            "svg-unpainted-hit-test",
            r#"<svg id=s width=300 height=100 style="display:block">
  <defs><linearGradient id=lg><stop offset=0 stop-color=red /></linearGradient></defs>
  <rect id=m width=100 height=100 fill="url(#missing) none" />
  <rect id=g x=100 width=100 height=100 fill="url(#missing)" />
  <rect id=gr x=200 width=100 height=100 fill="url(#lg)" />
  <text id=t x=10 y=50 font-size=40 fill=none stroke=none>Hi</text>
</svg>"#,
        )?;
        assert_eq!(
            page_vm
                .vm_mut()
                .eval("__hits([[30, 40], [150, 50], [250, 50]])")?,
            ["s|s,body,html", "s|s,body,html", "gr|gr,s,body,html"].join(" "),
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .expect("SVG unpainted hit-test fixture should run");
}

/// A point on the viewport's scrollbar hits no element, not even the root.
#[tokio::test(flavor = "current_thread")]
async fn element_from_point_on_the_viewport_scrollbar_is_null() {
    run_page_vm_async_test(async move {
        let mut page_vm =
            svg_hit_test_page_vm("scrollbar-hit-test", r#"<div style="height:3000px"></div>"#)?;
        assert_eq!(
            page_vm
                .vm_mut()
                .eval("__hits([[innerWidth - 5, 10], [10, 10]])")?,
            ["null|", "div|div,body,html"].join(" "),
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .expect("viewport scrollbar hit-test fixture should run");
}

/// Content outside the geometry model's allowlist keeps the outer `<svg>`
/// answer for the whole `<svg>`, so a lower shape never wins where the
/// model would misplace the front one: a CSS transform, `transform: none`
/// over a `transform` attribute, a `<use>` whose stroke reaches past its
/// bounding box, a non-scaling stroke, and a length unit the model does not
/// parse. Each `<svg>` is 100px tall, a back rect filling it.
#[tokio::test(flavor = "current_thread")]
async fn svg_content_outside_the_allowlist_keeps_the_outer_svg() {
    run_page_vm_async_test(async move {
        let mut page_vm = svg_hit_test_page_vm(
            "svg-allowlist-hit-test",
            r##"<svg id=s1 width=100 height=100 style="display:block">
  <rect id=b1 width=100 height=100 fill=blue />
  <rect id=f1 width=50 height=50 fill=red style="transform:translate(50px,50px)" />
</svg>
<svg id=s2 width=100 height=100 style="display:block">
  <rect id=b2 width=100 height=100 fill=blue />
  <rect id=f2 width=50 height=50 fill=red transform="translate(50,50)" style="transform:none" />
</svg>
<svg id=s3 width=100 height=100 style="display:block">
  <defs><line id=src x1=10 y1=50 x2=90 y2=50 /></defs>
  <rect id=b3 width=100 height=100 fill=blue />
  <use id=f3 href="#src" stroke=red stroke-width=30 />
</svg>
<svg id=s4 width=100 height=100 style="display:block">
  <rect id=b4 width=100 height=100 fill=blue />
  <g transform="scale(0.1)"><line id=f4 x1=0 y1=500 x2=1000 y2=500 stroke=red stroke-width=20 vector-effect=non-scaling-stroke /></g>
</svg>
<svg id=s5 width=100 height=100 style="display:block">
  <rect id=b5 width=100 height=100 fill=blue />
  <line id=f5 x1=0 y1="10vb" x2=100 y2="10vb" stroke=red stroke-width=10 />
</svg>"##,
        )?;
        // Each point is on the front shape where the browser paints it and
        // off it where the attribute-only geometry would put it.
        assert_eq!(
            page_vm.vm_mut().eval(
                "__hits([[75, 75], [25, 125], [50, 260], [50, 358], [50, 450]])"
            )?,
            [
                "s1|s1,body,html",
                "s2|s2,body,html",
                "s3|s3,body,html",
                "s4|s4,body,html",
                "s5|s5,body,html",
            ]
            .join(" "),
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .expect("SVG allowlist fallback hit-test fixture should run");
}

/// A dash pattern that `pathLength` scales to nothing would expand into
/// an unbounded number of dashes; the hit test stops at its bound and
/// keeps the outer `<svg>` answer.
#[tokio::test(flavor = "current_thread")]
async fn svg_dash_expansion_is_bounded_under_a_huge_path_length() {
    run_page_vm_async_test(async move {
        let mut page_vm = svg_hit_test_page_vm(
            "svg-dash-bound-hit-test",
            r#"<svg id=s width=200 height=100 style="display:block">
  <line id=l x1=0 y1=50 x2=200 y2=50 stroke=red stroke-width=20 stroke-dasharray="5 5" pathLength="1e30" />
</svg>"#,
        )?;
        let started = std::time::Instant::now();
        assert_eq!(
            page_vm.vm_mut().eval("__hits([[100, 50]])")?,
            "s|s,body,html"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "the bounded hit test took {:?}",
            started.elapsed()
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .expect("SVG dash bound hit-test fixture should run");
}

/// `pointer-events` keywords other than `auto` and `none` are SVG-only and
/// the style engine drops them, so the element would read as `auto`. An
/// element naming one, and a query outside the outer `<svg>`'s content box,
/// keep the outer `<svg>` answer instead of a guessed one. Each `<svg>` is
/// a block; the third has 20px of padding.
#[tokio::test(flavor = "current_thread")]
async fn svg_unmodelled_pointer_events_and_points_outside_the_content_box_stay_coarse() {
    run_page_vm_async_test(async move {
        let mut page_vm = svg_hit_test_page_vm(
            "svg-pointer-events-keyword-hit-test",
            r#"<svg id=s1 width=100 height=100 style="display:block">
  <rect id=b1 width=100 height=100 fill=red />
  <circle id=c1 cx=50 cy=50 r=30 fill=none pointer-events="all" />
</svg>
<svg id=s2 width=100 height=100 style="display:block">
  <rect id=r2 width=100 height=100 fill=blue style="pointer-events: stroke" />
</svg>
<svg id=s3 width=100 height=100 viewBox="0 0 100 100" style="display:block;padding:20px">
  <rect id=r3 x=-10 y=-10 width=60 height=60 fill=green />
</svg>"#,
        )?;
        assert_eq!(
            page_vm
                .vm_mut()
                .eval("__hits([[50, 50], [50, 150], [15, 215], [40, 240]])")?,
            [
                "s1|s1,body,html",
                "s2|s2,body,html",
                "s3|s3,body,html",
                "r3|r3,s3,body,html",
            ]
            .join(" "),
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .expect("SVG pointer-events keyword hit-test fixture should run");
}

/// A stylesheet rule with an SVG-only `pointer-events` keyword is dropped
/// before any computed value shows it, so its presence keeps every outer
/// `<svg>` answer in that document.
#[tokio::test(flavor = "current_thread")]
async fn svg_stylesheet_pointer_events_keyword_keeps_the_outer_svg() {
    run_page_vm_async_test(async move {
        let mut page_vm = svg_hit_test_page_vm(
            "svg-stylesheet-pointer-events-hit-test",
            r#"<style>.edge { pointer-events: stroke }</style>
<svg id=s width=100 height=100 style="display:block">
  <rect id=r width=100 height=100 fill=blue class=edge />
</svg>"#,
        )?;
        assert_eq!(
            page_vm.vm_mut().eval("__hits([[50, 50]])")?,
            "s|s,body,html"
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .expect("SVG stylesheet pointer-events hit-test fixture should run");
}
