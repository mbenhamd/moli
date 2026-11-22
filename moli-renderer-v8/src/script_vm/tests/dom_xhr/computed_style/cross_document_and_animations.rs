use super::*;

#[test]
fn popup_target_selector_invalidation_uses_popup_document_world() {
    let mut vm = new_storage_test_vm("https://popup-target-style-cache.test/");

    let setup = vm
        .eval(
            r#"
(() => {
  const root = document.documentElement || document.appendChild(document.createElement('html'));
  const head = document.head || root.appendChild(document.createElement('head'));
  const body = document.body || root.appendChild(document.createElement('body'));
  const activeStyle = document.createElement('style');
  activeStyle.textContent = '#active-popup-target-cache { color: rgb(1, 2, 3); }';
  head.appendChild(activeStyle);
  const active = document.createElement('div');
  active.id = 'active-popup-target-cache';
  body.appendChild(active);
  globalThis.__popupTargetActiveStyle = getComputedStyle(active);

  const popup = open('about:blank');
  globalThis.__popupTargetWindow = popup;
  const popupRoot = popup.document.documentElement ||
    popup.document.appendChild(popup.document.createElement('html'));
  const popupHead = popup.document.head ||
    popupRoot.appendChild(popup.document.createElement('head'));
  const popupBody = popup.document.body ||
    popupRoot.appendChild(popup.document.createElement('body'));
  const style = popup.document.createElement('style');
  style.textContent = [
    '.probe { color: rgb(4, 5, 6); }',
    '#popup-old-target:target { color: rgb(7, 8, 9); }',
    '#popup-new-target:target { color: rgb(10, 11, 12); }'
  ].join('\n');
  popupHead.appendChild(style);
  const oldTarget = popup.document.createElement('div');
  oldTarget.id = 'popup-old-target';
  oldTarget.className = 'probe';
  const newTarget = popup.document.createElement('div');
  newTarget.id = 'popup-new-target';
  newTarget.className = 'probe';
  popupBody.append(oldTarget, newTarget);
  popup.history.replaceState(null, '', '#popup-old-target');
  globalThis.__popupTargetOldStyle = popup.getComputedStyle(oldTarget);
  globalThis.__popupTargetNewStyle = popup.getComputedStyle(newTarget);

  return [
    globalThis.__popupTargetActiveStyle.color,
    globalThis.__popupTargetOldStyle.color,
    globalThis.__popupTargetNewStyle.color
  ].join('|');
})()
"#,
        )
        .expect("popup target style setup should evaluate");

    assert_eq!(setup, "rgb(1, 2, 3)|rgb(7, 8, 9)|rgb(4, 5, 6)");
    let active_document = vm.document_handle_for_test();
    let popup_document = owner_document_handle_for_element_id(&vm, "popup-old-target");
    assert_ne!(popup_document, active_document);
    let active_cache_before = computed_style_cache_entry_count_for_document(&vm, active_document);
    assert!(active_cache_before > 0);
    assert_eq!(
        computed_style_cache_entry_count_for_document(&vm, popup_document),
        2
    );

    let replaced = vm
        .eval(
            r#"
(() => {
  __popupTargetWindow.history.replaceState(null, '', '#popup-new-target');
  const result = [
    globalThis.__popupTargetOldStyle.color,
    globalThis.__popupTargetNewStyle.color
  ].join('|');
  delete globalThis.__popupTargetWindow;
  delete globalThis.__popupTargetOldStyle;
  delete globalThis.__popupTargetNewStyle;
  delete globalThis.__popupTargetActiveStyle;
  return result;
})()
"#,
        )
        .expect("popup target style mutation should evaluate");

    assert_eq!(replaced, "rgb(4, 5, 6)|rgb(10, 11, 12)");
    assert_eq!(
        computed_style_cache_entry_count_for_document(&vm, active_document),
        active_cache_before,
        "popup target invalidation should not clear active document cache"
    );
    assert!(
        computed_style_cache_entry_count_for_document(&vm, popup_document) > 0,
        "popup target invalidation should keep style work in the popup document world"
    );
}

#[test]
fn isolated_world_target_selector_invalidation_uses_root_document_world() {
    let mut vm = new_storage_test_vm("https://isolated-target-style-cache.test/");

    let setup = vm
        .eval(
            r#"
(() => {
  const root = document.documentElement || document.appendChild(document.createElement('html'));
  const head = document.head || root.appendChild(document.createElement('head'));
  const body = document.body || root.appendChild(document.createElement('body'));
  const style = document.createElement('style');
  style.textContent = [
    '.probe { color: rgb(4, 5, 6); }',
    '#isolated-old-target:target { color: rgb(7, 8, 9); }',
    '#isolated-new-target:target { color: rgb(10, 11, 12); }'
  ].join('\n');
  head.appendChild(style);
  const oldTarget = document.createElement('div');
  oldTarget.id = 'isolated-old-target';
  oldTarget.className = 'probe';
  const newTarget = document.createElement('div');
  newTarget.id = 'isolated-new-target';
  newTarget.className = 'probe';
  body.append(oldTarget, newTarget);
  history.replaceState(null, '', '#isolated-old-target');
  globalThis.__isolatedTargetOldStyle = getComputedStyle(oldTarget);
  globalThis.__isolatedTargetNewStyle = getComputedStyle(newTarget);
  return [
    globalThis.__isolatedTargetOldStyle.color,
    globalThis.__isolatedTargetNewStyle.color
  ].join('|');
})()
"#,
        )
        .expect("isolated target style setup should evaluate");

    assert_eq!(setup, "rgb(7, 8, 9)|rgb(4, 5, 6)");
    let document = vm.document_handle_for_test();
    let generation_before_target =
        vm.computed_style_cache_generation_for_document_for_test(document);
    let context_id = vm
        .create_isolated_world("style-target-test", false)
        .expect("isolated world should be created");
    let changed = vm
        .eval_in_isolated_context(
            context_id,
            r#"
(() => {
  history.replaceState(null, '', '#isolated-new-target');
  return [
    location.hash,
    document.getElementById('isolated-new-target').matches(':target')
  ].join('|');
})()
"#,
        )
        .expect("isolated target mutation should evaluate");

    assert_eq!(changed, "#isolated-new-target|true");
    let resolved = vm
        .eval(
            r#"
(() => {
  const result = [
    globalThis.__isolatedTargetOldStyle.color,
    globalThis.__isolatedTargetNewStyle.color
  ].join('|');
  delete globalThis.__isolatedTargetOldStyle;
  delete globalThis.__isolatedTargetNewStyle;
  return result;
})()
"#,
        )
        .expect("default world held style should see isolated target invalidation");

    assert_eq!(resolved, "rgb(4, 5, 6)|rgb(10, 11, 12)");
    assert_eq!(
        vm.computed_style_cache_generation_for_document_for_test(document),
        generation_before_target,
        "isolated target invalidation should not bump the retained style generation"
    );
    assert!(computed_style_cache_entry_count_for_document(&vm, document) > 0);
}

#[test]
fn moving_stylesheet_link_into_shadow_root_removes_document_stylesheet_entry() {
    let mut vm = new_storage_test_vm("https://shadow-stylesheets-boundary.test/");
    let request_url =
        url::Url::parse("https://shadow-stylesheets-boundary.test/sheet.css").unwrap();
    let initial = vm
        .eval(
            r#"
(() => {
  if (!document.documentElement) {
    document.appendChild(document.createElement('html'));
  }
  if (!document.body) {
    document.documentElement.appendChild(document.createElement('body'));
  }
  const initial = document.styleSheets.length;
  const link = document.createElement('link');
  link.id = 'shadow-boundary-link';
  link.rel = 'stylesheet';
  link.href = '/sheet.css';
  document.body.appendChild(link);
  const host = document.createElement('div');
  host.id = 'shadow-boundary-host';
  document.body.appendChild(host);
  host.attachShadow({ mode: 'open' });
  return initial;
})()
"#,
        )
        .expect("document stylesheet shadow-boundary setup should evaluate");
    let link = element_handle_by_id(&vm, "shadow-boundary-link");
    install_linked_stylesheet_for_test(
        &mut vm,
        link,
        request_url.clone(),
        crate::style_engine::StyloStylesheetSource::new(String::new(), request_url.clone())
            .with_sheet_url(request_url),
    );
    let result = vm
        .eval(
            r#"
(() => {
  const link = document.getElementById('shadow-boundary-link');
  const afterBody = document.styleSheets.length;
  document.getElementById('shadow-boundary-host').shadowRoot.appendChild(link);
  return [afterBody, document.styleSheets.length].join('|');
})()
"#,
        )
        .expect("document styleSheets should update when stylesheet moves into shadow root");

    assert_eq!(format!("{initial}|{result}"), "0|1|0");
}

#[test]
fn child_document_stylesheet_link_moved_into_shadow_root_is_hidden_from_document_stylesheets() {
    let mut vm = new_storage_test_vm("https://child-shadow-stylesheets-boundary.test/");
    let request_url =
        url::Url::parse("https://child-shadow-stylesheets-boundary.test/sheet.css").unwrap();
    let initial = vm
        .eval(
            r#"
(() => {
  if (!document.documentElement) {
    document.appendChild(document.createElement('html'));
  }
  if (!document.body) {
    document.documentElement.appendChild(document.createElement('body'));
  }
  const frame = document.createElement('iframe');
  document.body.appendChild(frame);
  const doc = frame.contentWindow.document;
  if (!doc.documentElement) {
    doc.appendChild(doc.createElement('html'));
  }
  if (!doc.body) {
    doc.documentElement.appendChild(doc.createElement('body'));
  }
  const initial = doc.styleSheets.length;
  const link = doc.createElement('link');
  link.id = 'child-shadow-boundary-link';
  link.setAttribute('rel', 'stylesheet');
  link.href = 'https://child-shadow-stylesheets-boundary.test/sheet.css';
  doc.body.appendChild(link);
  const host = doc.createElement('div');
  host.id = 'child-shadow-boundary-host';
  doc.body.appendChild(host);
  host.attachShadow({ mode: 'open' });
  return initial;
})()
"#,
        )
        .expect("child document stylesheet shadow-boundary setup should evaluate");
    let link = element_handle_by_id(&vm, "child-shadow-boundary-link");
    install_linked_stylesheet_for_test(
        &mut vm,
        link,
        request_url.clone(),
        crate::style_engine::StyloStylesheetSource::new(String::new(), request_url.clone())
            .with_sheet_url(request_url),
    );
    let result = vm
        .eval(
            r#"
(() => {
  const doc = document.querySelector('iframe').contentDocument;
  const link = doc.getElementById('child-shadow-boundary-link');
  const afterBody = doc.styleSheets.length;
  doc.getElementById('child-shadow-boundary-host').shadowRoot.appendChild(link);
  return [afterBody, doc.styleSheets.length].join('|');
})()
"#,
        )
        .expect("child document styleSheets should update across shadow root moves");

    assert_eq!(format!("{initial}|{result}"), "0|1|0");
}

#[test]
fn shadow_styles_scope_and_inherit_host_font_size() {
    let mut vm = new_storage_test_vm("https://shadow-style-scope-geometry.test/");

    let result = vm
        .eval(
            r#"
(() => {
  if (!document.documentElement) {
    document.appendChild(document.createElement('html'));
  }
  if (!document.body) {
    document.documentElement.appendChild(document.createElement('body'));
  }
  const frame = document.createElement('iframe');
  document.body.appendChild(frame);
  const d = frame.contentWindow.document;

  d.head.innerHTML = '<style>.document-only-hidden { display: none; }</style>';
  const documentStyleHost = d.createElement('div');
  d.body.appendChild(documentStyleHost);
  const documentStyleRoot = documentStyleHost.attachShadow({ mode: 'open' });
  const shadowVisibleWrapper = d.createElement('div');
  shadowVisibleWrapper.innerHTML = '<span id="shadowVisible" class="document-only-hidden">shadow</span>';
  documentStyleRoot.appendChild(shadowVisibleWrapper);
  const shadowVisible = documentStyleRoot.querySelector('#shadowVisible');

  const shadowStyleHost = d.createElement('div');
  d.body.appendChild(shadowStyleHost);
  const documentSpan = d.createElement('span');
  documentSpan.id = 'documentSpan';
  documentSpan.className = 'shadow-only-hidden';
  d.body.appendChild(documentSpan);
  const shadowStyleRoot = shadowStyleHost.attachShadow({ mode: 'open' });
  const style = d.createElement('style');
  style.textContent = '.shadow-only-hidden { display: none; }';
  shadowStyleRoot.appendChild(style);
  const shadowHidden = d.createElement('span');
  shadowHidden.id = 'shadowHidden';
  shadowHidden.className = 'shadow-only-hidden';
  shadowStyleRoot.appendChild(shadowHidden);
  const classPrefixVisible = d.createElement('span');
  classPrefixVisible.id = 'classPrefixVisible';
  classPrefixVisible.className = 'shadow-only';
  shadowStyleRoot.appendChild(classPrefixVisible);

  const inheritHost = d.createElement('div');
  inheritHost.setAttribute('style', 'font-size:10px');
  inheritHost.innerHTML = '<span id="lightChild">light</span>';
  d.body.appendChild(inheritHost);
  const inheritRoot = inheritHost.attachShadow({ mode: 'open' });
  const inherited = d.createElement('span');
  inherited.id = 'inherited';
  inherited.textContent = 'shadow';
  inheritRoot.appendChild(inherited);
  const lightChild = d.querySelector('#lightChild');
  const lightChildRect = lightChild.getBoundingClientRect();
  const initialFontSize = getComputedStyle(inherited).fontSize;
  inheritHost.setAttribute('style', 'font-size:20px');

	  return JSON.stringify({
	    documentStyleDoesNotEnterShadow: getComputedStyle(shadowVisible).display !== 'none',
	    shadowStyleDoesNotLeaveShadow: getComputedStyle(documentSpan).display !== 'none',
	    shadowStyleAppliesInsideShadow: getComputedStyle(shadowHidden).display === 'none',
	    classSelectorDoesNotMatchPrefix: getComputedStyle(classPrefixVisible).display !== 'none',
	    lightChildWithoutSlotHasNoBox:
        lightChild.offsetTop === 0 && lightChildRect.width === 0 && lightChildRect.height === 0,
	    inheritedFontSizeUpdates: initialFontSize === '10px' && getComputedStyle(inherited).fontSize === '20px'
	  });
})()
"#,
        )
        .expect("shadow scoped style should evaluate");

    assert_eq!(
        result,
        r#"{"documentStyleDoesNotEnterShadow":true,"shadowStyleDoesNotLeaveShadow":true,"shadowStyleAppliesInsideShadow":true,"classSelectorDoesNotMatchPrefix":true,"lightChildWithoutSlotHasNoBox":true,"inheritedFontSizeUpdates":true}"#
    );
}

#[test]
fn child_document_shadow_visibility_offsets_match_rendered_wpt_probe() {
    let mut vm = new_storage_test_vm("https://shadow-style-offset-wpt-probe.test/");

    let result = eval_with_layout_publications(
        &mut vm,
        r#"
(function* () {
  if (!document.documentElement) {
    document.appendChild(document.createElement('html'));
  }
  if (!document.body) {
    document.documentElement.appendChild(document.createElement('body'));
  }
  const frame = document.createElement('iframe');
  document.body.appendChild(frame);
  const d = frame.contentWindow.document;

  d.head.innerHTML = '<style>.document-only-hidden { display: none; }</style>';
  const documentStyleHost = d.createElement('div');
  d.body.appendChild(documentStyleHost);
  const documentStyleRoot = documentStyleHost.attachShadow({ mode: 'open' });
  const shadowVisibleWrapper = d.createElement('div');
  shadowVisibleWrapper.innerHTML =
    '<span id="shadowVisible" class="document-only-hidden">shadow</span>';
  documentStyleRoot.appendChild(shadowVisibleWrapper);
  const shadowVisible = documentStyleRoot.querySelector('#shadowVisible');

  const shadowStyleHost = d.createElement('div');
  d.body.appendChild(shadowStyleHost);
  const documentSpan = d.createElement('span');
  documentSpan.id = 'documentSpan';
  documentSpan.className = 'shadow-only-hidden';
  d.body.appendChild(documentSpan);
  const shadowStyleRoot = shadowStyleHost.attachShadow({ mode: 'open' });
  const style = d.createElement('style');
  style.textContent = '.shadow-only-hidden { display: none; }';
  shadowStyleRoot.appendChild(style);
  const shadowHidden = d.createElement('span');
  shadowHidden.id = 'shadowHidden';
  shadowHidden.className = 'shadow-only-hidden';
  shadowStyleRoot.appendChild(shadowHidden);

yield; // Publish this scene before reading its geometry.
  return JSON.stringify({
    documentStyleDoesNotEnterShadow:
      frame.contentWindow.getComputedStyle(shadowVisible).display !== 'none',
    documentStyleShadowOffsetIsPositive: shadowVisible.offsetTop > 0,
    shadowStyleDoesNotLeaveDocument:
      frame.contentWindow.getComputedStyle(documentSpan).display !== 'none',
    documentTreeOffsetIsPositive: documentSpan.offsetTop > 0,
    shadowStyleAppliesInsideShadow:
      frame.contentWindow.getComputedStyle(shadowHidden).display === 'none',
    shadowHiddenClientRectIsEmpty:
      shadowHidden.getBoundingClientRect().width === 0 &&
      shadowHidden.getBoundingClientRect().height === 0
  });
})()
"#,
    )
    .expect("child document shadow style visibility offsets should evaluate");

    assert_eq!(
        result,
        r#"{"documentStyleDoesNotEnterShadow":true,"documentStyleShadowOffsetIsPositive":true,"shadowStyleDoesNotLeaveDocument":true,"documentTreeOffsetIsPositive":true,"shadowStyleAppliesInsideShadow":true,"shadowHiddenClientRectIsEmpty":true}"#
    );
}

#[test]
fn shadow_dir_pseudo_styles_slotted_nodes_from_document_direction() {
    let mut vm = new_storage_test_vm("https://shadow-dir-pseudo-style.test/");

    let result = vm
        .eval(
            r#"
(() => {
  if (!document.documentElement) {
    document.appendChild(document.createElement('html'));
  }
  if (!document.head) {
    document.documentElement.appendChild(document.createElement('head'));
  }
  if (!document.body) {
    document.documentElement.appendChild(document.createElement('body'));
  }
  document.head.appendChild(document.createElement('style')).textContent =
    '.slotted { color: red; } .slotted:dir(rtl) { color: green; }';
  document.body.setAttribute('dir', 'rtl');

  const host = document.createElement('div');
  const slotted = document.createElement('div');
  slotted.className = 'slotted';
  host.appendChild(slotted);
  host.attachShadow({ mode: 'open' }).appendChild(document.createElement('slot'));
  document.body.appendChild(host);

  return getComputedStyle(slotted).color;
})()
"#,
        )
        .expect(":dir() pseudo-class should style slotted nodes");

    assert_eq!(result, "rgb(0, 128, 0)");
}

#[test]
fn computed_direction_tracks_input_html_directionality() {
    let mut vm = new_storage_test_vm("https://input-direction-computed-style.test/");

    let result = vm
        .eval(
            r#"
(() => {
  if (!document.documentElement) {
    document.appendChild(document.createElement('html'));
  }
  if (!document.body) {
    document.documentElement.appendChild(document.createElement('body'));
  }

  const container = document.createElement('div');
  container.dir = 'rtl';
  const input = document.createElement('input');
  input.type = 'tel';
  container.appendChild(input);
  document.body.appendChild(container);

  const tel = `${input.matches(':dir(ltr)')}:${getComputedStyle(input).direction}`;
  input.type = 'text';
  const text = `${input.matches(':dir(rtl)')}:${getComputedStyle(input).direction}`;
  input.type = 'tel';
  const restoredTel = `${input.matches(':dir(ltr)')}:${getComputedStyle(input).direction}`;
  input.dir = 'auto';
  input.value = '\u05ea';
  const auto = `${input.matches(':dir(rtl)')}:${getComputedStyle(input).direction}`;
  input.style.direction = 'ltr';
  const inlineOverride = `${input.matches(':dir(rtl)')}:${getComputedStyle(input).direction}`;

  return `${tel}|${text}|${restoredTel}|${auto}|${inlineOverride}`;
})()
"#,
        )
        .expect("input direction computed style should evaluate");

    assert_eq!(result, "true:ltr|true:rtl|true:ltr|true:rtl|true:ltr");
}

#[test]
fn author_direction_overrides_dir_attribute_hint() {
    let mut vm = new_storage_test_vm("https://direction-cascade-computed-style.test/");

    let result = vm
        .eval(
            r#"
(() => {
  if (!document.documentElement) {
    document.appendChild(document.createElement('html'));
  }
  if (!document.head) {
    document.documentElement.appendChild(document.createElement('head'));
  }
  if (!document.body) {
    document.documentElement.appendChild(document.createElement('body'));
  }
  document.head.appendChild(document.createElement('style')).textContent =
    '.ltr { direction: ltr; } .rtl { direction: rtl; }';

  const add = (parent, tag, className, dir, text) => {
    const element = document.createElement(tag);
    element.className = className;
    if (dir) element.dir = dir;
    element.textContent = text;
    parent.appendChild(element);
    return element;
  };
  const direction = (element) => getComputedStyle(element).direction;

  const authored = add(document.body, 'div', 'ltr', '', 'x');
  const authoredChild = add(authored, 'span', '', '', 'x');
  const inherited = add(document.body, 'div', '', '', 'x');
  const initial = direction(inherited);
  document.documentElement.dir = 'rtl';

  const ltrScope = add(document.body, 'div', '', 'ltr', '');
  const auto = add(ltrScope, 'div', '', 'auto', 'abc');
  const autoChild = add(auto, 'span', '', '', '');
  const autoBefore = direction(autoChild);
  auto.firstChild.data = 'ת';

  const mathNs = 'http://www.w3.org/1998/Math/MathML';
  const math = ltrScope.appendChild(document.createElementNS(mathNs, 'math'));
  math.setAttribute('dir', 'rtl');
  const mi = math.appendChild(document.createElementNS(mathNs, 'mi'));

  return [
    initial, direction(authored), direction(authoredChild), direction(inherited),
    autoBefore, direction(auto), direction(autoChild), direction(math), direction(mi),
  ].join('|');
})()
"#,
        )
        .expect("direction cascade computed style should evaluate");

    assert_eq!(result, "ltr|ltr|ltr|rtl|ltr|rtl|rtl|rtl|rtl");
}

#[test]
fn has_pseudo_class_computed_style_updates_after_dom_mutation() {
    let mut vm = new_storage_test_vm("https://has-pseudo-computed-style.test/");

    let result = vm
        .eval(
            r#"
(() => {
  if (!document.documentElement) {
    document.appendChild(document.createElement('html'));
  }
  if (!document.head) {
    document.documentElement.appendChild(document.createElement('head'));
  }
  if (!document.body) {
    document.documentElement.appendChild(document.createElement('body'));
  }
  document.head.appendChild(document.createElement('style')).textContent =
    '.target { color: red; } .parent:has(.marker) .target { color: green; }';

  const parent = document.createElement('div');
  parent.className = 'parent';
  const target = document.createElement('span');
  target.className = 'target';
  parent.appendChild(target);
  document.body.appendChild(parent);

  const before = getComputedStyle(target).color;
  const marker = document.createElement('span');
  marker.className = 'marker';
  parent.appendChild(marker);
  const after = getComputedStyle(target).color;
  return `${before}|${after}`;
})()
"#,
        )
        .expect(":has() computed style mutation test should evaluate");

    assert_eq!(result, "rgb(255, 0, 0)|rgb(0, 128, 0)");
}

#[test]
fn empty_pseudo_class_computed_style_updates_after_child_list_mutation() {
    let mut vm = new_storage_test_vm("https://empty-pseudo-computed-style.test/");

    let result = vm
        .eval(
            r#"
(() => {
  if (!document.documentElement) {
    document.appendChild(document.createElement('html'));
  }
  if (!document.head) {
    document.documentElement.appendChild(document.createElement('head'));
  }
  if (!document.body) {
    document.documentElement.appendChild(document.createElement('body'));
  }
  document.head.appendChild(document.createElement('style')).textContent =
    '.box { color: red; } .box:empty { color: green; }';

  const box = document.createElement('div');
  box.className = 'box';
  document.body.appendChild(box);
  const before = getComputedStyle(box).color;
  const child = document.createElement('span');
  box.appendChild(child);
  const afterInsert = getComputedStyle(box).color;
  child.remove();
  const afterRemove = getComputedStyle(box).color;
  return `${before}|${afterInsert}|${afterRemove}`;
})()
"#,
        )
        .expect(":empty computed style mutation test should evaluate");

    assert_eq!(result, "rgb(0, 128, 0)|rgb(255, 0, 0)|rgb(0, 128, 0)");
}

#[test]
fn computed_style_create_read_remove_loop_does_not_grow_caches() {
    let mut vm = new_storage_test_vm("https://computed-style-cache-loop.test/");
    let document = vm.document_handle_for_test();

    let result = vm
        .eval(
            r#"
(() => {
  const root = document.documentElement || document.appendChild(document.createElement('html'));
  const head = document.head || root.appendChild(document.createElement('head'));
  const body = document.body || root.appendChild(document.createElement('body'));
  const style = document.createElement('style');
  style.textContent = '.target { color: rgb(20, 21, 22); }';
  head.appendChild(style);

  let last = '';
  for (let i = 0; i < 40; i += 1) {
    const target = document.createElement('div');
    target.className = 'target';
    body.appendChild(target);
    last = getComputedStyle(target).color;
    target.remove();
  }
  return last;
})()
"#,
        )
        .expect("computed style create/read/remove loop should evaluate");

    assert_eq!(result, "rgb(20, 21, 22)");
    assert_eq!(
        vm.retained_style_system_rebuild_count_for_document_for_test(document),
        1
    );
    assert_eq!(
        vm.computed_style_cache_entry_count_for_document_for_test(document),
        0
    );
}

#[test]
fn tab_key_respects_reading_flow_item_order() {
    let mut vm = new_streamed_parser_test_vm(
        "https://shadow-reading-flow.test/",
        r#"<!doctype html>
<style>
.source {
  display: block;
  reading-flow: source-order;
}
.grid {
  display: grid;
  reading-flow: grid-order;
}
</style>
<div class="source">
  <button id="a" style="reading-order: 1">A</button>
  <button id="b" style="reading-order: -1">B</button>
  <button id="c">C</button>
</div>
<div class="grid">
  <button id="gA" style="order: -1">Grid A</button>
  <button id="gB">Grid B</button>
  <button id="gC" tabindex="1" style="order: -1">Grid C</button>
</div>
"#,
    );

    let result = vm
        .eval(
            r#"
(() => {
  function pressTab() {
    __moliDispatchTrustedKey('keydown', 'Tab', 'Tab', false, false, false, false);
  }
  const seen = [];
  for (let i = 0; i < 6; i++) {
    pressTab();
    seen.push(document.activeElement && document.activeElement.id);
  }
  return seen.join(',');
})()
"#,
        )
        .expect("Tab default action should respect reading-flow item order");

    assert_eq!(result, "b,c,a,gA,gC,gB");
}

#[test]
fn tab_key_respects_reading_flow_display_contents_items() {
    let mut vm = new_streamed_parser_test_vm(
        "https://shadow-reading-flow-display-contents.test/",
        r#"<!doctype html>
<style>
.wrapper {
  display: grid;
  reading-flow: grid-order;
}
</style>
<div class="wrapper">
  <div style="display: contents">
    <button id="order3" style="order: 3">Order 3</button>
    <button id="order1" style="order: 1">Order 1</button>
    <div style="display: contents">
      <button id="order4" style="order: 4">Order 4</button>
      <button id="order2" style="order: 2">Order 2</button>
    </div>
  </div>
</div>
<div class="wrapper">
  <div id="div1B" style="display: contents" tabindex="0">
    <button id="order3B" style="order: 3">Order 3</button>
    <button id="order1B" style="order: 1">Order 1</button>
    <div id="div2B" style="display: contents" tabindex="0">
      <button id="order4B" style="order: 4">Order 4</button>
      <button id="order2B" style="order: 2">Order 2</button>
    </div>
  </div>
</div>
"#,
    );

    let result = vm
        .eval(
            r#"
(() => {
  function pressTab() {
    __moliDispatchTrustedKey('keydown', 'Tab', 'Tab', false, false, false, false);
  }
  const seen = [];
  for (let i = 0; i < 10; i++) {
    pressTab();
    seen.push(document.activeElement && document.activeElement.id);
  }
  return seen.join(',');
})()
"#,
        )
        .expect(
            "Tab default action should respect independent display:contents reading-flow items",
        );

    assert_eq!(
        result,
        "order1,order2,order4,order3,div1B,order1B,div2B,order2B,order4B,order3B"
    );
}

#[tokio::test]
async fn css_transition_state_uses_final_values_without_runtime_events() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_rendered_test_vm(
        "https://move-before-transition-state.test/",
        r#"<html><head></head><body></body></html>"#,
    );

    vm.eval(
        r#"
(() => {
  document.head.appendChild(document.createElement('style')).textContent = `
    body { margin-left: 0; }
    section { position: relative; }
    #item, #trigger, #pseudo {
      width: 100px;
      height: 100px;
      position: absolute;
      left: 0;
      transition: left 10s steps(1, jump-both);
    }
    #new-parent #trigger { left: 400px; }
    #pseudo::before {
      content: "x";
      position: absolute;
      left: 0;
      transition: left 10s steps(1, jump-both);
    }
    #pseudo.big::before { left: 400px; }
  `;
  document.body.innerHTML = `
    <section id="old-parent">
      <div id="item"></div>
      <div id="trigger"></div>
      <div id="pseudo"></div>
    </section>
    <section id="new-parent"></section>`;
  globalThis.__lmTransitionEvents = 0;
  for (const node of document.querySelectorAll('div')) {
    node.addEventListener('transitionstart', () => { globalThis.__lmTransitionEvents += 1; });
  }
  document.getElementById('item').style.left = '400px';
  document.getElementById('pseudo').classList.add('big');
  document.getElementById('new-parent').moveBefore(document.getElementById('trigger'), null);
})()
"#,
    )
    .expect("transition setup should evaluate");

    vm.advance_timers_until_deadline_for_test(&loader)
        .await
        .expect("transition timers should drain");

    publish_layout_for_test(&mut vm);
    let result = vm
        .eval(
            r#"
(() => {
  const item = document.getElementById('item');
  const trigger = document.getElementById('trigger');
  const pseudo = document.getElementById('pseudo');
  const itemStyle = getComputedStyle(item);
  return [
    globalThis.__lmTransitionEvents,
    item.getBoundingClientRect().x,
    item.getAnimations().length,
    trigger.getBoundingClientRect().x,
    itemStyle.left,
    itemStyle.transitionProperty,
    itemStyle.transitionDuration,
    itemStyle.transitionDelay,
    itemStyle.transitionTimingFunction,
    itemStyle.transitionBehavior,
    getComputedStyle(pseudo, '::before').left
  ].join('|');
})()
"#,
        )
        .expect("transition final-state should be readable");

    assert_eq!(
        result,
        "0|400|0|400|400px|left|10s|0s|steps(1, jump-both)|normal|400px"
    );
}

/// Chromium 156: a style change after a style flush starts a `CSSTransition`
/// whose current value applies at once (left 200px at progress 0 for
/// `steps(1, jump-both)`); `cancel()` drops it and fires `transitioncancel`.
#[tokio::test]
async fn css_transition_starts_at_style_change_and_cancels_with_event() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_rendered_test_vm(
        "https://css-transition-start-cancel.test/",
        r#"<html><head><style>
body { margin: 0; }
#t { position: absolute; left: 0; width: 10px; height: 10px;
     transition: left 3600s steps(1, jump-both); }
#t.on { left: 400px; }
</style></head><body><div id="t"></div></body></html>"#,
    );

    let started = vm
        .eval(
            r#"
(() => {
  const t = document.getElementById('t');
  globalThis.__events = [];
  for (const type of ['transitionrun', 'transitionstart', 'transitionend', 'transitioncancel']) {
    t.addEventListener(type, (event) => {
      __events.push(`${type}:${event.propertyName}:${event.isTrusted}`);
    });
  }
  getComputedStyle(t).left;
  t.classList.add('on');
  const animations = t.getAnimations();
  const animation = animations[0];
  globalThis.__transition = animation;
  animation.finished.then(() => 'resolved', (error) => error.name).then((outcome) => {
    globalThis.__finished = outcome;
  });
  const timing = animation.effect.getTiming();
  return [
    animations.length,
    animation.constructor.name,
    animation.transitionProperty,
    timing.duration,
    timing.fill,
    timing.easing,
    getComputedStyle(t).left,
    animation.playState,
    t.getAnimations()[0] === animation,
  ].join('|');
})()
"#,
        )
        .expect("transition start should evaluate");
    assert_eq!(
        started,
        "1|CSSTransition|left|3600000|backwards|steps(1, jump-both)|200px|running|true"
    );
    // Layout uses the transition's current value too.
    publish_layout_for_test(&mut vm);
    let x = vm
        .eval("String(document.getElementById('t').getBoundingClientRect().x)")
        .expect("transition geometry should be readable");
    assert_eq!(x, "200");

    let canceled = vm
        .eval(
            r#"
(() => {
  const t = document.getElementById('t');
  __transition.cancel();
  return [getComputedStyle(t).left, __transition.playState, t.getAnimations().length].join('|');
})()
"#,
        )
        .expect("transition cancel should evaluate");
    assert_eq!(canceled, "400px|idle|0");

    vm.advance_timers_until_deadline_for_test(&loader)
        .await
        .expect("transition task should run");
    let delivered = vm
        .eval(r#"[globalThis.__finished, __events.join(',')].join('|')"#)
        .expect("transition events should be readable");
    assert_eq!(
        delivered,
        "AbortError|transitionrun:left:true,transitionstart:left:true,transitioncancel:left:true"
    );
}

/// A color transition repaints: its frames restyle the element, but geometry
/// reads keep the published layout, as no box can have moved. A `left`
/// transition's frames lay out again.
#[tokio::test]
async fn css_color_transition_samples_keep_the_published_layout() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_rendered_test_vm(
        "https://css-color-transition-layout.test/",
        r#"<html><head><style>
#c { position: absolute; left: 0; width: 10px; height: 10px; color: rgb(0, 0, 0);
     transition: color 60ms linear; }
#c.on { color: rgb(255, 0, 0); }
#l { position: absolute; top: 20px; left: 0; width: 10px; height: 10px;
     transition: left 60ms linear; }
#l.on { left: 400px; }
</style></head><body><div id="c"></div><div id="l"></div></body></html>"#,
    );
    vm.set_layout_policy(moli_page_types::LayoutPolicy::Live);
    let start = |vm: &mut StandaloneScriptVmHarness, id: &str| {
        vm.eval(&format!(
            "(() => {{ const e = document.getElementById('{id}'); getComputedStyle(e).left; \
             e.classList.add('on'); e.getBoundingClientRect(); }})()"
        ))
        .expect("transition start should evaluate");
        vm.layout_pass_observability_for_test().1
    };

    let builds = start(&mut vm, "c");
    vm.advance_timers_until_deadline_for_test(&loader)
        .await
        .expect("color transition frames should run");
    let color = vm
        .eval(
            "(() => { const c = document.getElementById('c'); \
             return [c.getBoundingClientRect().x, getComputedStyle(c).color].join('|'); })()",
        )
        .expect("settled color transition should be readable");
    assert_eq!(color, "0|rgb(255, 0, 0)");
    assert_eq!(vm.layout_pass_observability_for_test().1, builds);

    let builds = start(&mut vm, "l");
    vm.advance_timers_until_deadline_for_test(&loader)
        .await
        .expect("left transition frames should run");
    let x = vm
        .eval("String(document.getElementById('l').getBoundingClientRect().x)")
        .expect("settled left transition should be readable");
    assert_eq!(x, "400");
    assert!(vm.layout_pass_observability_for_test().1 > builds);
}

/// An ancestor's `display: none` stops rendering the element and cancels its
/// running transition at the next frame, with no style read in between. As in
/// Chromium 156, that frame first sends the queued run and start events.
#[tokio::test]
async fn css_transition_cancels_when_an_ancestor_stops_rendering() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_rendered_test_vm(
        "https://css-transition-ancestor-display.test/",
        r#"<html><head><style>
#t { position: absolute; left: 0; width: 10px; height: 10px; transition: left 3600s linear; }
#t.on { left: 400px; }
</style></head><body><div id="p"><div id="t"></div></div></body></html>"#,
    );
    vm.eval(
        r#"
(() => {
  const t = document.getElementById('t');
  globalThis.__events = [];
  for (const type of ['transitionrun', 'transitionstart', 'transitioncancel']) {
    t.addEventListener(type, (event) => __events.push(`${type}:${event.propertyName}`));
  }
  getComputedStyle(t).left;
  t.classList.add('on');
  globalThis.__transition = t.getAnimations()[0];
  document.getElementById('p').style.display = 'none';
})()
"#,
    )
    .expect("transition start should evaluate");
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
    vm.advance_timers_until_deadline_for_test_with_deadline(&loader, deadline)
        .await
        .expect("transition frames should run");
    let canceled = vm
        .eval("[__transition.playState, __events.join(',')].join('|')")
        .expect("transition state should be readable");
    assert_eq!(
        canceled,
        "idle|transitionrun:left,transitionstart:left,transitioncancel:left"
    );
}

/// The style changes of a frame's animation frame callbacks share one style
/// update after the last callback: a transition one callback starts runs from
/// the frame's time, and one that a later callback undoes never starts. That
/// update also styles an element a callback inserted, so a class added at the
/// next frame transitions although no script read its style.
#[tokio::test]
async fn css_transition_style_changes_in_animation_frame_callbacks_start_at_that_frame() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_rendered_test_vm(
        "https://css-transition-animation-frame.test/",
        r#"<html><head><style>
#a, #b, .c { position: absolute; left: 0; width: 10px; height: 10px;
             transition: left 1000ms linear; }
#a.on, #b.on, .c.on { left: 1000px; }
</style></head><body><div id="a"></div><div id="b"></div></body></html>"#,
    );
    vm.eval(
        r#"
(() => {
  const a = document.getElementById('a'), b = document.getElementById('b');
  globalThis.__bEvents = [];
  for (const type of ['transitionrun', 'transitioncancel']) {
    b.addEventListener(type, (event) => __bEvents.push(type));
  }
  getComputedStyle(a).left;
  getComputedStyle(b).left;
  const c = document.createElement('div');
  c.className = 'c';
  globalThis.__order = [];
  requestAnimationFrame(() => {
    __order.push('f0');
    requestAnimationFrame(() => {
      __order.push('f1a');
      a.classList.add('on');
      b.classList.add('on');
      document.body.append(c);
      requestAnimationFrame(() => {
        __order.push('f2');
        globalThis.__next = [
          parseFloat(getComputedStyle(a).left) > 0,
          b.getAnimations().length,
        ].join('|');
        c.classList.add('on');
        requestAnimationFrame(() => {
          __order.push('f3');
          globalThis.__inserted = c.getAnimations().length;
        });
      });
    });
    requestAnimationFrame(() => {
      __order.push('f1b');
      b.classList.remove('on');
    });
  });
})()
"#,
    )
    .expect("animation frame callbacks should be requested");
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
    vm.advance_timers_until_deadline_for_test_with_deadline(&loader, deadline)
        .await
        .expect("animation frames should run");
    let observed = vm
        .eval(
            "[__order.join(','), globalThis.__next, __bEvents.join(','), \
             globalThis.__inserted].join('|')",
        )
        .expect("frame observations should be readable");
    assert_eq!(observed, "f0,f1a,f1b,f2,f3|true|0||1");
}

#[tokio::test]
async fn invalid_move_before_does_not_run_plain_transition_runtime() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_rendered_test_vm(
        "https://move-before-invalid-transition.test/",
        r#"<html><head></head><body></body></html>"#,
    );

    vm.eval(
        r#"
(() => {
  document.head.appendChild(document.createElement('style')).textContent = `
    body { margin-left: 0; }
    #item {
      width: 100px;
      height: 100px;
      position: absolute;
      left: 0;
      transition: left 10s;
    }
  `;
  document.body.innerHTML = `<div id="item"></div>`;
  const item = document.getElementById('item');
  globalThis.__lmPlainTransitionEvents = 0;
  item.addEventListener('transitionstart', () => { globalThis.__lmPlainTransitionEvents += 1; });
  item.style.left = '400px';
})()
"#,
    )
    .expect("plain transition setup should evaluate");

    vm.advance_timers_until_deadline_for_test(&loader)
        .await
        .expect("plain transition timers should drain");

    publish_layout_for_test(&mut vm);
    let result = vm
        .eval(
            r#"
(() => {
  const item = document.getElementById('item');
  const doc = document.implementation.createHTMLDocument();
  let error = '';
  try {
    doc.body.moveBefore(item, null);
  } catch (err) {
    error = err.name;
  }
  return [
    globalThis.__lmPlainTransitionEvents,
    error,
    item.getBoundingClientRect().x
  ].join('|');
})()
"#,
        )
        .expect("invalid moveBefore transition state should evaluate");

    assert_eq!(result, "0|HierarchyRequestError|400");
}

#[tokio::test]
async fn zero_duration_transform_transition_applies_final_layout_geometry() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_rendered_test_vm(
        "https://zero-duration-transform-transition.test/",
        r#"<html><head></head><body></body></html>"#,
    );

    vm.eval(
        r#"
(() => {
  document.head.appendChild(document.createElement('style')).textContent = `
    body { margin-left: 0; }
    #item {
      width: 100px;
      height: 100px;
      position: absolute;
      left: 0;
      transition: transform 0s linear 1s;
    }
  `;
  document.body.innerHTML = `<div id="item"></div>`;
  const item = document.getElementById('item');
  globalThis.__lmZeroDurationTransformEvents = 0;
  item.addEventListener('transitionstart', () => {
    globalThis.__lmZeroDurationTransformEvents += 1;
  });
  item.style.transform = 'translateX(400px)';
})()
"#,
    )
    .expect("zero-duration transform transition setup should evaluate");

    vm.advance_timers_until_deadline_for_test(&loader)
        .await
        .expect("zero-duration transform transition timers should drain");

    publish_layout_for_test(&mut vm);
    let result = vm
        .eval(
            r#"
(() => {
  const item = document.getElementById('item');
  return [
    globalThis.__lmZeroDurationTransformEvents,
    item.getBoundingClientRect().x
  ].join('|');
})()
"#,
        )
        .expect("zero-duration transform geometry should evaluate");

    assert_eq!(result, "0|400");
}

#[tokio::test]
async fn child_content_document_created_elements_use_transition_final_state() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_storage_test_vm("https://child-created-transition.test/");

    vm.eval(
        r#"
(() => {
  const frame = document.createElement('iframe');
  (document.body || document.documentElement || document).appendChild(frame);
})()
"#,
    )
    .expect("child document frame setup should evaluate");
    vm.drain_pending_child_frame_work_for_test();

    vm.eval(
        r#"
(() => {
  const frame = document.querySelector('iframe');
  const doc = frame.contentDocument;
  doc.head.appendChild(doc.createElement('style')).textContent = `
    body { margin-left: 0; }
    #item {
      position: absolute;
      left: 0;
      transition: left 10s;
    }
  `;
  const item = doc.createElement('div');
  item.id = 'item';
  doc.body.append(item);
  globalThis.__lmChildTransitionEvents = 0;
  item.addEventListener('transitionstart', () => { globalThis.__lmChildTransitionEvents += 1; });
  item.style.left = '400px';
})()
"#,
    )
    .expect("child document transition setup should evaluate");

    vm.advance_timers_until_deadline_for_test(&loader)
        .await
        .expect("child document transition timers should drain");

    publish_layout_for_test(&mut vm);
    let result = vm
        .eval(
            r#"
(() => {
  const item = document.querySelector('iframe').contentDocument.getElementById('item');
  return [
    globalThis.__lmChildTransitionEvents,
    item.getBoundingClientRect().x
  ].join('|');
})()
"#,
        )
        .expect("child document transition state should evaluate");

    assert_eq!(result, "0|400");
}

#[tokio::test]
async fn child_content_document_animation_start_scans_child_stylesheets() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_storage_page_task_executor_test_vm_with_loader(
        "https://child-animation-source-scope.test/",
        &loader,
    );

    vm.eval(
        r#"
(() => {
  const frame = document.createElement('iframe');
  (document.body || document.documentElement || document).appendChild(frame);
})()
"#,
    )
    .expect("child document frame setup should evaluate");
    while vm
        .run_one_oldest_ready_page_task_executor_turn(&loader)
        .await
        .expect("child frame setup task should run")
    {}

    vm.eval(
        r#"
(() => {
  const frame = document.querySelector('iframe');
  const doc = frame.contentDocument;
  doc.head.appendChild(doc.createElement('style')).textContent = `
    @keyframes childAnim {
      from { left: 100px; }
      to { left: 400px; }
    }
    #item {
      position: relative;
      animation: 1s linear infinite alternate childAnim;
      animation-delay: 100ms;
    }
  `;
  const item = doc.createElement('div');
  item.id = 'item';
  doc.body.append(item);
  globalThis.__lmChildAnimationEvents = 0;
  item.addEventListener('animationstart', () => { globalThis.__lmChildAnimationEvents += 1; });
})()
"#,
    )
    .expect("child document animation setup should evaluate");

    assert!(
        !vm.has_ready_timeout(),
        "child animationstart must not manufacture a PageTimer"
    );
    assert!(
        vm.run_one_rendering_update_executor_turn(&loader)
            .await
            .expect("child animation rendering update should run")
    );

    let result = vm
        .eval(
            r#"
(() => {
  const item = document.querySelector('iframe').contentDocument.getElementById('item');
  return [
    globalThis.__lmChildAnimationEvents,
    getComputedStyle(item).left,
    item.getAnimations().length
  ].join('|');
})()
"#,
        )
        .expect("child document animation state should evaluate");

    assert_eq!(result, "1|250px|1");
}

#[tokio::test]
async fn css_animation_start_and_midpoint_style_are_observable() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_parsed_page_task_executor_test_vm(
        "https://css-animation-midpoint.test/",
        r#"<html><head></head><body></body></html>"#,
        &loader,
    );

    vm.eval(
        r#"
(() => {
  document.head.appendChild(document.createElement('style')).textContent = `
    @keyframes anim {
      from { left: 100px; }
      to { left: 400px; }
    }
    #item {
      position: relative;
      animation: 1s linear infinite alternate anim;
      animation-delay: 100ms;
    }
  `;
  document.body.innerHTML = `<div id="item"></div>`;
  globalThis.__lmAnimationEvents = 0;
  addEventListener('animationstart', () => { globalThis.__lmAnimationEvents += 1; });
})()
"#,
    )
    .expect("animation setup should evaluate");

    assert!(
        !vm.has_ready_timeout(),
        "animationstart must not manufacture a PageTimer"
    );
    assert!(
        vm.run_one_rendering_update_executor_turn(&loader)
            .await
            .expect("animation rendering update should run")
    );

    let result = vm
        .eval(
            r#"
(() => {
  const item = document.getElementById('item');
  return [
    globalThis.__lmAnimationEvents,
    getComputedStyle(item).left,
    item.getAnimations().length
  ].join('|');
})()
"#,
        )
        .expect("animation midpoint state should be readable");

    assert_eq!(result, "1|250px|1");

    crate::live_stylesheet::reset_live_stylesheet_mutation_metrics_for_test();
    let observed = vm
        .eval(
            r#"
(() => {
  const item = document.getElementById('item');
  const animatedLeft = getComputedStyle(item).left;
  const keyframes = document.querySelector('style').sheet.cssRules[0];
  const frame = keyframes[0];
  const keyText = frame.keyText;
  const declaration = frame.style;
  const rules = keyframes.cssRules;
  const ruleCount = keyframes.length;
  const cssText = keyframes.cssText;
  for (let index = 0; index < 20; index++) {
    let error;
    try { frame.keyText = 'body'; } catch (caught) { error = caught; }
    if (error?.name !== 'SyntaxError') throw new Error('invalid keyframe selector');
    keyframes.appendRule('body {}');
  }
  return [
    frame.keyText === keyText,
    keyframes.length === ruleCount,
    keyframes[0] === frame,
    frame.style === declaration,
    keyframes.cssRules === rules,
    keyframes.cssText === cssText,
    item.getAnimations().length,
    getComputedStyle(item).left === animatedLeft,
  ].join('|');
})()
"#,
        )
        .expect("keyframe mutation should leave the running animation available");
    assert_eq!(observed, "true|true|true|true|true|true|1|true");
    let metrics = crate::live_stylesheet::live_stylesheet_mutation_metrics_for_test();
    assert_eq!(metrics.native_keyframe_mutations, 0);
    assert_eq!(metrics.native_rule_value_mutations, 0);
    assert_eq!(metrics.native_top_level_mutations, 0);
    assert_eq!(metrics.native_nested_mutations, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn window_animation_scan_does_not_serialize_installed_stylesheets() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_parsed_page_task_executor_test_vm(
        "https://animation-scan-scaling.test/",
        "<html><head></head><body></body></html>",
        &loader,
    );
    vm.eval(
        r#"
const style = document.head.appendChild(document.createElement('style'));
style.textContent = Array.from({length: 256}, (_, i) =>
  `.unrelated-${i} { color: rgb(1, 2, 3); }`
).join('\n') + `
  @media all { @supports (display: block) {
    @keyframes move { from { left: 0px; } to { left: 20px; } }
    @keyframes shimmer { from { background-position: 0 50%; }
                         to { background-position: -100% 50%; } }
  } }
  .animated { position: relative; animation: move 1s linear; }
  .skeleton { animation: shimmer 1s infinite; }
`;
document.body.innerHTML = Array.from({length: 32}, () =>
  '<div class="animated"></div><div class="skeleton"></div>'
).join('');
globalThis.__animationScanEvents = 0;
addEventListener('animationstart', () => { __animationScanEvents++; });
getComputedStyle(document.querySelector('.animated')).animationName;
"#,
    )
    .expect("large stylesheet and loading skeleton should initialize");
    crate::live_stylesheet::reset_live_stylesheet_css_text_projection_count_for_test();

    assert!(
        vm.run_one_rendering_update_executor_turn(&loader)
            .await
            .expect("window animation scan should complete")
    );
    assert_eq!(
        vm.eval(
            r#"[
              __animationScanEvents,
              getComputedStyle(document.querySelector('.animated')).left,
              document.querySelector('.animated').getAnimations().length,
              document.querySelector('.skeleton').getAnimations().length
            ].join('|')"#,
        )
        .expect("animation events and midpoint should remain observable"),
        "32|10px|1|0"
    );
    assert_eq!(
        crate::live_stylesheet::live_stylesheet_css_text_projection_count_for_test(),
        0,
        "animation scans and value reads must use native rules, without serializing full stylesheets"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn window_animation_scan_revalidates_keyframes_after_listener_cssom_mutation() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_parsed_page_task_executor_test_vm(
        "https://animation-scan-cssom-mutation.test/",
        r#"<html><head><style>
          @keyframes move { from { left: 0px; } to { left: 20px; } }
          .animated { position: relative; animation: move 1s linear; }
        </style></head><body>
          <div id="first" class="animated"></div><div id="second" class="animated"></div>
        </body></html>"#,
        &loader,
    );
    vm.eval(
        r#"
globalThis.__animationScanTargets = [];
addEventListener('animationstart', event => {
  __animationScanTargets.push(event.target.id);
  document.styleSheets[0].deleteRule(0);
});
"#,
    )
    .expect("reentrant animation listener should initialize");

    assert!(
        vm.run_one_rendering_update_executor_turn(&loader)
            .await
            .expect("window animation scan should complete")
    );
    assert_eq!(
        vm.eval("__animationScanTargets.join('|')")
            .expect("animation targets should be readable"),
        "first",
        "removing keyframes in the first listener must suppress the second element's stale event"
    );
}

#[test]
fn raw_keyframe_scanner_respects_owner_media_changes() {
    let mut vm = new_parsed_test_vm(
        "https://keyframe-owner-media.test/",
        r#"<!doctype html>
        <style>
          #item {
            position: relative;
            animation-name: print-only;
            animation-duration: 1s;
          }
        </style>
        <style media="print">
          @keyframes print-only {
            from { left: 100px; }
            to { left: 400px; }
          }
        </style>
        <div id="item"></div>"#,
    );

    assert_eq!(
        vm.eval(
            r#"(() => {
              const item = document.getElementById('item');
              return [getComputedStyle(item).left, item.getAnimations().length].join('|');
            })()"#,
        )
        .expect("screen animation state should evaluate"),
        "0px|0",
        "a print-only keyframe rule must be invisible to the screen raw-CSS scanner",
    );

    vm.set_emulated_media(&crate::protocol_types::EmulatedMediaOverrides {
        media: Some("print".to_owned()),
        ..Default::default()
    });
    assert_eq!(
        vm.eval(
            r#"(() => {
              const item = document.getElementById('item');
              return [getComputedStyle(item).left, item.getAnimations().length].join('|');
            })()"#,
        )
        .expect("print animation state should evaluate"),
        "250px|1",
        "the same retained source must become visible after switching to print",
    );

    vm.set_emulated_media(&crate::protocol_types::EmulatedMediaOverrides::default());
    assert_eq!(
        vm.eval(
            r#"(() => {
              const item = document.getElementById('item');
              return [getComputedStyle(item).left, item.getAnimations().length].join('|');
            })()"#,
        )
        .expect("restored screen animation state should evaluate"),
        "0px|0",
        "leaving print must hide the raw keyframe source again",
    );
}

#[test]
fn raw_scanners_follow_live_owner_media_attribute_mutations() {
    let mut vm = new_parsed_test_vm(
        "https://raw-scanner-owner-media-mutation.test/",
        r#"<!doctype html>
        <style>
          #item {
            position: relative;
            animation: owner-gated 1s;
            --actual: --owner-gated();
          }
        </style>
        <style id="gated" media="print">
          @keyframes owner-gated {
            from { left: 20px; }
            to { left: 60px; }
          }
          @function --owner-gated() { result: 17px; }
        </style>
        <div id="item"></div>"#,
    );
    let probe = r#"(() => {
      const item = document.getElementById('item');
      const computed = getComputedStyle(item);
      return [
        computed.left,
        item.getAnimations().length,
        computed.getPropertyValue('--actual')
      ].join('|');
    })()"#;

    assert_eq!(
        vm.eval(probe).expect("initial raw scanner state"),
        "0px|0|--owner-gated()",
    );
    vm.eval("document.getElementById('gated').media = 'screen'")
        .expect("owner media should become effective");
    assert_eq!(
        vm.eval(probe).expect("effective raw scanner state"),
        "40px|1|17px",
        "owner media mutation must update both raw compatibility scanners",
    );
    vm.eval("document.getElementById('gated').media = 'not all'")
        .expect("owner media should become ineffective");
    assert_eq!(
        vm.eval(probe).expect("ineffective raw scanner state"),
        "0px|0|--owner-gated()",
        "the retained source must not leak after a second owner media mutation",
    );
}

#[tokio::test]
async fn registered_length_custom_property_animation_revert_uses_underlying_value() {
    let mut vm = new_parsed_test_vm(
        "https://registered-custom-animation-revert.test/",
        r#"<html><head></head><body></body></html>"#,
    );

    let result = vm
        .eval(
            r#"
(() => {
  CSS.registerProperty({
    name: "--animated-non-inherited",
    syntax: "<length>",
    initialValue: "0px",
    inherits: false
  });
  CSS.registerProperty({
    name: "--animated-inherited",
    syntax: "<length>",
    initialValue: "10000px",
    inherits: true
  });
  const style = document.createElement("style");
  style.textContent = `
    @keyframes revert_animation {
      from {
        --animated-inherited: revert;
        --animated-non-inherited: revert;
      }
      to {
        --animated-inherited: 100px;
        --animated-non-inherited: 100px;
      }
    }
    #parent {
      --animated-inherited: 0px;
    }
    #child {
      animation: revert_animation 10s -5s linear paused;
    }
  `;
  document.head.append(style);
  document.body.innerHTML = `<div id="parent"><div id="child"></div></div>`;
  const computed = getComputedStyle(document.getElementById("child"));
  return [
    computed.getPropertyValue("--animated-non-inherited"),
    computed.getPropertyValue("--animated-inherited")
  ].join("|");
})()
"#,
        )
        .expect("registered custom property animation revert should evaluate");

    assert_eq!(result, "50px|50px");
}

#[tokio::test]
async fn css_animation_start_and_zero_timeout_use_distinct_task_sources() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_parsed_page_task_executor_test_vm(
        "https://css-animation-window-order.test/",
        r#"<html><head></head><body></body></html>"#,
        &loader,
    );

    vm.eval(
        r#"
(() => {
  document.head.appendChild(document.createElement('style')).textContent = `
    @keyframes anim {
      from { left: 100px; }
      to { left: 400px; }
    }
    #item {
      position: relative;
      animation: 1s linear infinite alternate anim;
    }
  `;
  document.body.innerHTML = `<div id="item"></div>`;
  globalThis.__lmAnimationOrder = [];
  addEventListener('animationstart', () => { globalThis.__lmAnimationOrder.push('animation'); });
  setTimeout(() => { globalThis.__lmAnimationOrder.push('timeout'); }, 0);
})()
"#,
    )
    .expect("animation ordering setup should evaluate");

    assert!(
        vm.has_ready_timeout(),
        "the explicit setTimeout must remain a real PageTimer"
    );
    assert!(
        vm.run_one_rendering_update_executor_turn(&loader)
            .await
            .expect("animation rendering update should run")
    );
    assert_eq!(
        vm.eval("globalThis.__lmAnimationOrder.join(',')")
            .expect("pre-timer animation order should evaluate"),
        "animation"
    );

    vm.advance_timers_until_deadline_for_test(&loader)
        .await
        .expect("the genuine zero timeout should drain");

    let result = vm
        .eval("globalThis.__lmAnimationOrder.join(',')")
        .expect("animation order should evaluate");

    assert_eq!(result, "animation,timeout");
}

#[tokio::test]
async fn css_animation_commit_styles_commits_midpoint_transform() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_parsed_page_task_executor_test_vm(
        "https://css-animation-commit.test/",
        r#"<html><head></head><body></body></html>"#,
        &loader,
    );

    vm.eval(
        r#"
(() => {
  document.head.appendChild(document.createElement('style')).textContent = `
    @keyframes anim {
      from { transform: translateX(100px); }
      to { transform: translateX(400px); }
    }
    #item {
      animation: 1s linear infinite alternate anim;
      animation-delay: 100ms;
    }
  `;
  document.body.innerHTML = `<div id="item"></div>`;
  globalThis.__lmAnimationEvents = 0;
  document.getElementById('item').addEventListener('animationstart', () => {
    globalThis.__lmAnimationEvents += 1;
  });
})()
"#,
    )
    .expect("animation commit setup should evaluate");

    assert!(!vm.has_ready_timeout());
    assert!(
        vm.run_one_rendering_update_executor_turn(&loader)
            .await
            .expect("animation rendering update should run")
    );

    let result = vm
        .eval(
            r#"
(() => {
  const item = document.getElementById('item');
  const animations = item.getAnimations();
  animations[0].commitStyles();
  return [
    globalThis.__lmAnimationEvents,
    animations.length,
    'transform' in item.style,
    item.style.transform
  ].join('|');
})()
"#,
        )
        .expect("animation committed style should be readable");

    assert_eq!(result, "1|1|true|translateX(250px)");
}

#[tokio::test]
async fn css_animation_start_capture_and_bubble_listeners_share_queued_event() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_parsed_page_task_executor_test_vm(
        "https://css-animation-listeners.test/",
        r#"<html><head></head><body></body></html>"#,
        &loader,
    );

    vm.eval(
        r#"
(() => {
  document.head.appendChild(document.createElement('style')).textContent = `
    @keyframes anim {
      from { left: 100px; }
      to { left: 400px; }
    }
    #item {
      position: relative;
      animation: 1s linear infinite alternate anim;
    }
  `;
  document.body.innerHTML = `<div id="item"></div>`;
  const item = document.getElementById('item');
  globalThis.__lmAnimationEvents = 0;
  item.addEventListener('animationstart', () => { globalThis.__lmAnimationEvents += 1; });
  item.addEventListener('animationstart', () => { globalThis.__lmAnimationEvents += 1; }, true);
})()
"#,
    )
    .expect("animation listener setup should evaluate");

    assert!(!vm.has_ready_timeout());
    assert!(
        vm.run_one_rendering_update_executor_turn(&loader)
            .await
            .expect("animation rendering update should run")
    );

    let result = vm
        .eval("globalThis.__lmAnimationEvents")
        .expect("animation event count should evaluate");

    assert_eq!(result, "2");
}

#[tokio::test]
async fn css_animation_start_listener_removed_before_queued_event_still_deduplicates() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_parsed_page_task_executor_test_vm(
        "https://css-animation-listener-remove.test/",
        r#"<html><head></head><body></body></html>"#,
        &loader,
    );

    vm.eval(
        r#"
(() => {
  document.head.appendChild(document.createElement('style')).textContent = `
    @keyframes anim {
      from { left: 100px; }
      to { left: 400px; }
    }
    #item {
      position: relative;
      animation: 1s linear infinite alternate anim;
    }
  `;
  document.body.innerHTML = `<div id="item"></div>`;
  const item = document.getElementById('item');
  globalThis.__lmAnimationEvents = 0;
  function first() { globalThis.__lmAnimationEvents += 100; }
  function second() { globalThis.__lmAnimationEvents += 1; }
  item.addEventListener('animationstart', first);
  item.addEventListener('animationstart', second, true);
  item.removeEventListener('animationstart', first);
})()
"#,
    )
    .expect("animation listener setup should evaluate");

    assert!(!vm.has_ready_timeout());
    assert!(
        vm.run_one_rendering_update_executor_turn(&loader)
            .await
            .expect("animation rendering update should run")
    );

    let result = vm
        .eval("globalThis.__lmAnimationEvents")
        .expect("animation event count should evaluate");

    assert_eq!(result, "1");
}

#[tokio::test]
async fn css_animation_start_later_listener_gets_own_retroactive_scan() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_parsed_page_task_executor_test_vm(
        "https://css-animation-later-listener.test/",
        r#"<html><head></head><body></body></html>"#,
        &loader,
    );

    vm.eval(
        r#"
(() => {
  document.head.appendChild(document.createElement('style')).textContent = `
    @keyframes anim {
      from { left: 100px; }
      to { left: 400px; }
    }
    #item {
      position: relative;
      animation: 1s linear infinite alternate anim;
    }
  `;
  document.body.innerHTML = `<div id="item"></div>`;
  const item = document.getElementById('item');
  globalThis.__lmAnimationFirstEvents = 0;
  globalThis.__lmAnimationSecondEvents = 0;
  item.addEventListener('animationstart', () => { globalThis.__lmAnimationFirstEvents += 1; });
})()
"#,
    )
    .expect("animation listener setup should evaluate");

    assert!(!vm.has_ready_timeout());
    assert!(
        vm.run_one_rendering_update_executor_turn(&loader)
            .await
            .expect("initial animation rendering update should run")
    );
    assert_eq!(
        vm.eval(
            "[globalThis.__lmAnimationFirstEvents, globalThis.__lmAnimationSecondEvents].join('|')"
        )
        .expect("initial animation event count should evaluate"),
        "1|0"
    );

    vm.eval(
        r#"
document.getElementById('item').addEventListener('animationstart', () => {
  globalThis.__lmAnimationSecondEvents += 1;
});
"#,
    )
    .expect("later animation listener should evaluate");

    assert!(!vm.has_ready_timeout());
    assert!(
        vm.run_one_rendering_update_executor_turn(&loader)
            .await
            .expect("later animation rendering update should run")
    );

    let result = vm
        .eval("globalThis.__lmAnimationSecondEvents")
        .expect("later animation event count should evaluate");

    assert_eq!(result, "1");
}

#[test]
fn computed_style_uses_static_color_animation_value_for_inserted_node() {
    let mut vm = new_parsed_test_vm(
        "https://css-animation-static-color.test/",
        r#"<html><head></head><body></body></html>"#,
    );

    let result = vm
        .eval(
            r#"
(() => {
  document.head.appendChild(document.createElement('style')).textContent = `
    @keyframes my-animation {
      from { color: green; }
      to { color: green; }
    }
    div {
      color: red;
      animation: my-animation 1s infinite linear paused;
    }
  `;
  const span = document.body.appendChild(document.createElement('span'));
  const oldDiv = span.appendChild(document.createElement('div'));
  const before = getComputedStyle(oldDiv).color;
  const newDiv = document.createElement('div');
  oldDiv.replaceWith(newDiv);
  return [before, getComputedStyle(newDiv).color].join('|');
})()
"#,
        )
        .expect("static color animation computed style should evaluate");

    assert_eq!(result, "rgb(0, 128, 0)|rgb(0, 128, 0)");
}

#[test]
fn computed_style_serializes_animation_shorthand_from_longhands() {
    let mut vm = new_parsed_test_vm(
        "https://css-animation-computed-shorthand.test/",
        r#"<html><head></head><body><div id="target"></div></body></html>"#,
    );

    let result = vm
        .eval(
            r#"
(() => {
  const target = document.getElementById('target');
  const values = [];

  values.push(getComputedStyle(target).animation);

  target.style.animation = 'anim paused both reverse 4 1s -3s cubic-bezier(0, -2, 1, 3)';
  values.push(getComputedStyle(target).animation);

  target.style.animation = 'anim paused both reverse, 4 1s -3s cubic-bezier(0, -2, 1, 3)';
  values.push(getComputedStyle(target).animation);

  target.style.animation = 'initial';
  target.style.animationDelay = '1s';
  values.push(getComputedStyle(target).animation);

  return values.join('|');
})()
"#,
        )
        .expect("computed animation shorthand should evaluate");

    assert_eq!(
        result,
        "none|1s cubic-bezier(0, -2, 1, 3) -3s 4 reverse both paused anim|reverse both paused anim, 1s cubic-bezier(0, -2, 1, 3) -3s 4|0s 1s"
    );
}

#[test]
fn computed_style_serializes_compositing_longhands() {
    let mut vm = new_parsed_test_vm(
        "https://css-compositing-computed-longhands.test/",
        r#"<html><head><style>#styled { background-blend-mode: screen, overlay; mix-blend-mode: multiply; isolation: isolate; }</style></head><body><div id="initial"></div><div id="inline" style="background-blend-mode: normal, luminosity; mix-blend-mode: color; isolation: auto"></div><div id="styled"></div></body></html>"#,
    );

    let result = vm
        .eval(
            r#"
(() => {
  const initial = getComputedStyle(document.getElementById('initial'));
  const inline = getComputedStyle(document.getElementById('inline'));
  const styled = getComputedStyle(document.getElementById('styled'));
  return [
    initial.backgroundBlendMode,
    initial.mixBlendMode,
    initial.isolation,
    inline.backgroundBlendMode,
    inline.mixBlendMode,
    inline.isolation,
    styled.backgroundBlendMode,
    styled.mixBlendMode,
    styled.isolation
  ].join('|');
})()
"#,
        )
        .expect("compositing computed longhands should evaluate");

    assert_eq!(
        result,
        "normal|normal|auto|normal, luminosity|color|auto|screen, overlay|multiply|isolate"
    );
}

#[test]
fn computed_style_serializes_color_adjust_longhands() {
    let mut vm = new_parsed_test_vm(
        "https://css-color-adjust-computed-longhands.test/",
        r#"<html><head></head><body><div id="container"><div id="target"></div></div></body></html>"#,
    );

    let result = vm
        .eval(
            r#"
(() => {
  const container = document.getElementById('container');
  const target = document.getElementById('target');
  const computed = getComputedStyle(target);
  const values = [];

  values.push('color-scheme' in computed);
  values.push('color-adjust' in computed);
  values.push('forced-color-adjust' in computed);
  values.push(computed.getPropertyValue('color-scheme'));
  values.push(computed.getPropertyValue('color-adjust'));
  values.push(computed.getPropertyValue('forced-color-adjust'));

  target.style.colorScheme = 'only light';
  target.style.colorAdjust = 'exact';
  target.style.forcedColorAdjust = 'preserve-parent-color';
  values.push(computed.getPropertyValue('color-scheme'));
  values.push(computed.getPropertyValue('print-color-adjust'));
  values.push(computed.getPropertyValue('color-adjust'));
  values.push(computed.getPropertyValue('forced-color-adjust'));

  container.style.colorScheme = 'light dark';
  container.style.colorAdjust = 'economy';
  container.style.forcedColorAdjust = 'none';
  target.style.colorScheme = 'unset';
  target.style.colorAdjust = 'unset';
  target.style.forcedColorAdjust = 'unset';
  values.push(computed.getPropertyValue('color-scheme'));
  values.push(computed.getPropertyValue('color-adjust'));
  values.push(computed.getPropertyValue('forced-color-adjust'));

  target.style.colorScheme = 'initial';
  target.style.colorAdjust = 'initial';
  target.style.forcedColorAdjust = 'initial';
  values.push(computed.getPropertyValue('color-scheme'));
  values.push(computed.getPropertyValue('color-adjust'));
  values.push(computed.getPropertyValue('forced-color-adjust'));

  target.style.colorScheme = 'inherit';
  target.style.colorAdjust = 'inherit';
  target.style.forcedColorAdjust = 'inherit';
  values.push(computed.getPropertyValue('color-scheme'));
  values.push(computed.getPropertyValue('color-adjust'));
  values.push(computed.getPropertyValue('forced-color-adjust'));

  return values.join('|');
})()
"#,
        )
        .expect("color-adjust computed longhands should evaluate");

    assert_eq!(
        result,
        "true|true|true|normal|economy|auto|light only|exact|exact|preserve-parent-color|light dark|economy|none|normal|economy|auto|light dark|economy|none"
    );
}

#[test]
fn computed_style_serializes_scrollbar_longhands() {
    let mut vm = new_parsed_test_vm(
        "https://css-scrollbar-computed-longhands.test/",
        r#"<html><head></head><body><div id="container"><div id="target"></div></div></body></html>"#,
    );

    let result = vm
        .eval(
            r#"
(() => {
  const container = document.getElementById('container');
  const target = document.getElementById('target');
  const computed = getComputedStyle(target);
  const values = [];

  values.push('scrollbar-color' in computed);
  values.push('scrollbar-width' in computed);
  values.push(computed.getPropertyValue('scrollbar-color'));
  values.push(computed.getPropertyValue('scrollbar-width'));

  target.style.scrollbarColor = 'red green';
  target.style.scrollbarWidth = 'thin';
  values.push(computed.getPropertyValue('scrollbar-color'));
  values.push(computed.getPropertyValue('scrollbar-width'));

  container.style.scrollbarColor = 'rgb(1, 2, 3) rgb(4, 5, 6)';
  container.style.scrollbarWidth = 'none';
  target.style.scrollbarColor = 'unset';
  target.style.scrollbarWidth = 'unset';
  values.push(computed.getPropertyValue('scrollbar-color'));
  values.push(computed.getPropertyValue('scrollbar-width'));

  target.style.scrollbarColor = 'initial';
  target.style.scrollbarWidth = 'initial';
  values.push(computed.getPropertyValue('scrollbar-color'));
  values.push(computed.getPropertyValue('scrollbar-width'));

  target.style.scrollbarColor = 'inherit';
  target.style.scrollbarWidth = 'inherit';
  values.push(computed.getPropertyValue('scrollbar-color'));
  values.push(computed.getPropertyValue('scrollbar-width'));

  return values.join('|');
})()
"#,
        )
        .expect("scrollbar computed longhands should evaluate");

    assert_eq!(
        result,
        "true|true|auto|auto|rgb(255, 0, 0) rgb(0, 128, 0)|thin|rgb(1, 2, 3) rgb(4, 5, 6)|auto|auto|auto|rgb(1, 2, 3) rgb(4, 5, 6)|none"
    );
}

#[test]
fn computed_style_serializes_text_size_adjust() {
    let mut vm = new_parsed_test_vm(
        "https://css-text-size-adjust-computed.test/",
        r#"<html><head></head><body><div id="container"><div id="target"></div></div></body></html>"#,
    );

    let result = vm
        .eval(
            r#"
(() => {
  const container = document.getElementById('container');
  const target = document.getElementById('target');
  const computed = getComputedStyle(target);
  const values = [];

  values.push('text-size-adjust' in computed);
  values.push(computed.getPropertyValue('text-size-adjust'));

  target.style.textSizeAdjust = 'none';
  values.push(computed.getPropertyValue('text-size-adjust'));

  target.style.textSizeAdjust = '200%';
  values.push(computed.getPropertyValue('text-size-adjust'));

  target.style.textSizeAdjust = 'calc(10% * sibling-index())';
  values.push(computed.getPropertyValue('text-size-adjust'));

  container.style.textSizeAdjust = '10%';
  target.style.textSizeAdjust = 'unset';
  values.push(computed.getPropertyValue('text-size-adjust'));

  target.style.textSizeAdjust = 'initial';
  values.push(computed.getPropertyValue('text-size-adjust'));

  target.style.textSizeAdjust = 'inherit';
  values.push(computed.getPropertyValue('text-size-adjust'));

  return values.join('|');
})()
"#,
        )
        .expect("text-size-adjust computed longhand should evaluate");

    // Tree-counting belongs to Stylo's lazy cascade context. The renderer's
    // post-cascade numeric adapter deliberately does not synthesize sibling
    // state, because doing so would scan siblings for every numeric computed
    // style read. Keep this unresolved fallback as the compatibility baseline.
    assert_eq!(
        result,
        "true|auto|100%|200%|calc(10% * sibling-index())|10%|auto|10%"
    );
}

#[test]
fn computed_style_serializes_link_parameters() {
    let mut vm = new_parsed_test_vm(
        "https://css-link-parameters-computed.test/",
        r#"<html><head></head><body><div id="container"><div id="target"></div></div></body></html>"#,
    );

    let result = vm
        .eval(
            r#"
(() => {
  const container = document.getElementById('container');
  const target = document.getElementById('target');
  const computed = getComputedStyle(target);
  const values = [];

  values.push('link-parameters' in computed);
  values.push(computed.getPropertyValue('link-parameters'));

  target.style.linkParameters = 'param(--a, orange)';
  values.push(computed.getPropertyValue('link-parameters'));

  target.style.linkParameters = 'param(--a, ), param(--b)';
  values.push(computed.getPropertyValue('link-parameters'));

  container.style.linkParameters = 'param(--parent)';
  target.style.linkParameters = 'unset';
  values.push(computed.getPropertyValue('link-parameters'));

  target.style.linkParameters = 'inherit';
  values.push(computed.getPropertyValue('link-parameters'));

  target.style.linkParameters = 'initial';
  values.push(computed.getPropertyValue('link-parameters'));

  return values.join('|');
})()
"#,
        )
        .expect("link-parameters computed longhand should evaluate");

    assert_eq!(
        result,
        "true|none|param(--a, orange)|param(--a, ), param(--b)|none|param(--parent)|none"
    );
}

#[test]
fn computed_style_serializes_content_quotes_and_bookmarks() {
    let mut vm = new_parsed_test_vm(
        "https://css-content-computed.test/",
        r#"<html><head></head><body><div id="container"><div id="target"></div></div></body></html>"#,
    );

    let result = vm
        .eval(
            r#"
(() => {
  const container = document.getElementById('container');
  const target = document.getElementById('target');
  const computed = getComputedStyle(target);
  const before = getComputedStyle(target, '::before');
  const values = [];

  values.push('content' in computed);
  values.push('quotes' in computed);
  values.push('bookmark-level' in computed);
  values.push('bookmark-state' in computed);
  values.push(computed.getPropertyValue('content'));
  values.push(before.getPropertyValue('content'));

  target.style.content = 'counter(counter-name, DECIMAL) / "alt text"';
  values.push(computed.getPropertyValue('content'));

  container.style.quotes = 'none';
  target.style.quotes = 'unset';
  values.push(computed.getPropertyValue('quotes'));

  target.style.quotes = 'initial';
  values.push(computed.getPropertyValue('quotes'));

  container.style.bookmarkLevel = '1';
  target.style.bookmarkLevel = 'unset';
  values.push(computed.getPropertyValue('bookmark-level'));

  target.style.bookmarkLevel = 'inherit';
  values.push(computed.getPropertyValue('bookmark-level'));

  target.style.bookmarkState = 'closed';
  values.push(computed.getPropertyValue('bookmark-state'));

  target.style.bookmarkState = 'initial';
  values.push(computed.getPropertyValue('bookmark-state'));

  return values.join('|');
})()
"#,
        )
        .expect("content computed longhands should evaluate");

    assert_eq!(
        result,
        "true|true|true|true|normal|none|counter(counter-name) / \"alt text\"|none|auto|none|1|closed|open"
    );
}

#[test]
fn computed_style_serializes_will_change() {
    let mut vm = new_parsed_test_vm(
        "https://css-will-change-computed.test/",
        r#"<html><head></head><body><div id="container"><div id="target" style="will-change: inherit"></div></div></body></html>"#,
    );

    let result = vm
        .eval(
            r#"
(() => {
  const container = document.getElementById('container');
  const target = document.getElementById('target');
  const computed = getComputedStyle(target);
  const values = [];

  values.push('will-change' in computed);
  values.push('willChange' in computed);
  values.push(computed.getPropertyValue('will-change'));

  container.style.willChange = 'color';
  values.push(getComputedStyle(container).willChange);
  values.push(computed.getPropertyValue('will-change'));

  target.style.willChange = 'initial';
  values.push(computed.getPropertyValue('will-change'));

  target.style.willChange = 'scroll-position, TRANSFORM';
  values.push(computed.getPropertyValue('will-change'));

  target.style.willChange = 'auto, transform';
  values.push(computed.getPropertyValue('will-change'));

  return values.join('|');
})()
"#,
        )
        .expect("will-change computed longhand should evaluate");

    assert_eq!(
        result,
        "true|true|auto|color|color|auto|scroll-position, TRANSFORM|scroll-position, TRANSFORM"
    );
}

/// Chromium 156: an element in a `display: none` subtree starts no
/// transition and has no before-change style, so showing it shows its final
/// value with no animation and no transition events.
#[tokio::test]
async fn css_transition_does_not_start_in_an_undisplayed_subtree() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_rendered_test_vm(
        "https://css-transition-undisplayed-subtree.test/",
        r#"<html><head><style>
#t { position: absolute; left: 0; width: 10px; height: 10px;
     transition: left 3600s steps(1, jump-both); }
</style></head><body><div id="p" style="display: none"><div id="t"></div></div></body></html>"#,
    );
    let hidden = vm
        .eval(
            r#"
(() => {
  const t = document.getElementById('t');
  globalThis.__events = [];
  for (const type of ['transitionrun', 'transitionstart', 'transitionend', 'transitioncancel']) {
    t.addEventListener(type, () => __events.push(type));
  }
  getComputedStyle(t).left;
  t.style.left = '400px';
  const hidden = t.getAnimations().length;
  document.getElementById('p').style.display = 'block';
  return [hidden, getComputedStyle(t).left, t.getAnimations().length].join('|');
})()
"#,
        )
        .expect("undisplayed transition should evaluate");
    assert_eq!(hidden, "0|400px|0");
    vm.advance_timers_until_deadline_for_test(&loader)
        .await
        .expect("frames should run");
    let events = vm
        .eval("__events.join(',')")
        .expect("events should be readable");
    assert_eq!(events, "");
}

/// Chromium 156: transitions of a parent document and its child frame are
/// distinct; `finish()` on the frame's transition leaves the parent's
/// running at its current value.
#[tokio::test]
async fn css_transition_finish_acts_only_on_its_own_document() {
    let mut vm = new_rendered_test_vm(
        "https://css-transition-two-documents.test/",
        r#"<html><head><style>
#t { position: absolute; left: 0; width: 10px; height: 10px;
     transition: left 3600s steps(1, jump-both); }
#t.on { left: 400px; }
</style></head><body><div id="t"></div></body></html>"#,
    );
    vm.eval("document.body.appendChild(document.createElement('iframe'))")
        .expect("frame should be inserted");
    vm.drain_pending_child_frame_work_for_test();
    let observed = vm
        .eval(
            r#"
(() => {
  const t = document.getElementById('t');
  getComputedStyle(t).left;
  t.classList.add('on');
  const a = t.getAnimations()[0];
  const d = document.querySelector('iframe').contentDocument;
  d.head.appendChild(d.createElement('style')).textContent =
    '#u { opacity: 1; transition: opacity 3600s steps(1, jump-both); } #u.on { opacity: 0; }';
  const u = d.body.appendChild(d.createElement('div'));
  u.id = 'u';
  d.defaultView.getComputedStyle(u).opacity;
  u.classList.add('on');
  const b = u.getAnimations()[0];
  b.finish();
  return [a.playState, getComputedStyle(t).left, t.getAnimations().length, b.playState,
          d.defaultView.getComputedStyle(u).opacity].join('|');
})()
"#,
        )
        .expect("two-document transitions should evaluate");
    assert_eq!(observed, "running|200px|1|finished|0");
}

/// Chromium 156: after `document.open()` replaces the document, a new
/// transition gets a new `CSSTransition`, never the replaced document's.
#[tokio::test]
async fn css_transition_objects_are_not_reused_after_document_open() {
    let mut vm = new_rendered_test_vm(
        "https://css-transition-document-open.test/",
        r#"<html><head><style>
#t { position: absolute; left: 0; width: 10px; height: 10px;
     transition: left 3600s steps(1, jump-both); }
#t.on { left: 400px; }
</style></head><body><div id="t"></div></body></html>"#,
    );
    let observed = vm
        .eval(
            r#"
(() => {
  const t = document.getElementById('t');
  getComputedStyle(t).left;
  t.classList.add('on');
  const saved = t.getAnimations()[0];
  document.open();
  document.write('<style>#v { opacity: 1; transition: opacity 3600s steps(1, jump-both); }' +
                 ' #v.on { opacity: 0; }</style><div id="v">v</div>');
  document.close();
  const v = document.getElementById('v');
  getComputedStyle(v).opacity;
  v.classList.add('on');
  const c = v.getAnimations()[0];
  return [c === saved, c.transitionProperty, v.getAnimations()[0] === c].join('|');
})()
"#,
        )
        .expect("document.open transition should evaluate");
    assert_eq!(observed, "false|opacity|true");
}

/// Chromium 156: removing a child frame whose transition queued work does
/// not hold back the parent's transitions, which still finish.
#[tokio::test]
async fn css_transition_finishes_after_a_child_frame_with_transitions_is_removed() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_rendered_test_vm(
        "https://css-transition-frame-removal.test/",
        r#"<html><head><style>
#t { width: 10px; height: 10px; opacity: 1; transition: opacity 1ms linear; }
#t.on { opacity: 0; }
</style></head><body><div id="t"></div></body></html>"#,
    );
    vm.eval("document.body.appendChild(document.createElement('iframe'))")
        .expect("frame should be inserted");
    vm.drain_pending_child_frame_work_for_test();
    vm.eval(
        r#"
(() => {
  const f = document.querySelector('iframe');
  const d = f.contentDocument;
  d.head.appendChild(d.createElement('style')).textContent =
    '#u { opacity: 1; transition: opacity 3600s linear; } #u.on { opacity: 0; }';
  const u = d.body.appendChild(d.createElement('div'));
  d.defaultView.getComputedStyle(u).opacity;
  u.className = 'on';
  u.getAnimations();
  f.remove();
  const t = document.getElementById('t');
  getComputedStyle(t).opacity;
  t.classList.add('on');
  globalThis.__outcome = 'pending';
  t.getAnimations()[0].finished.then(() => { globalThis.__outcome = 'finished'; });
})()
"#,
    )
    .expect("frame removal should evaluate");
    vm.advance_timers_until_deadline_for_test(&loader)
        .await
        .expect("transition task should run");
    let outcome = vm
        .eval("globalThis.__outcome")
        .expect("outcome should be readable");
    assert_eq!(outcome, "finished");
}

/// Chromium 156: removing an element cancels its transitions and drops its
/// before-change style, so reinserting it in the same task shows the final
/// value with no transition.
#[tokio::test]
async fn css_transition_cancels_when_its_element_is_removed_and_reinserted() {
    let mut vm = new_rendered_test_vm(
        "https://css-transition-reinsert.test/",
        r#"<html><head><style>
#t { position: absolute; left: 0; width: 10px; height: 10px;
     transition: left 3600s steps(1, jump-both); }
#t.on { left: 400px; }
</style></head><body><div id="t"></div></body></html>"#,
    );
    let observed = vm
        .eval(
            r#"
(() => {
  const t = document.getElementById('t');
  getComputedStyle(t).left;
  t.classList.add('on');
  const a = t.getAnimations()[0];
  t.remove();
  document.body.append(t);
  return [t.getAnimations().length, a.playState, getComputedStyle(t).left].join('|');
})()
"#,
        )
        .expect("reinsertion should evaluate");
    assert_eq!(observed, "0|idle|400px");
}

/// Chromium 156: a style change that removes a running transition settles its
/// `CSSTransition` at the style flush, before any transition event.
#[tokio::test]
async fn css_transition_canceled_by_a_style_change_settles_at_the_flush() {
    let mut vm = new_rendered_test_vm(
        "https://css-transition-style-cancel.test/",
        r#"<html><head><style>
#t { position: absolute; left: 0; width: 10px; height: 10px;
     transition: left 3600s steps(1, jump-both); }
#t.on { left: 400px; }
</style></head><body><div id="t"></div></body></html>"#,
    );
    let observed = vm
        .eval(
            r#"
(() => {
  const t = document.getElementById('t');
  getComputedStyle(t).left;
  t.classList.add('on');
  const a = t.getAnimations()[0];
  t.style.transitionProperty = 'none';
  return [t.getAnimations().length, a.playState].join('|');
})()
"#,
        )
        .expect("style cancel should evaluate");
    assert_eq!(observed, "0|idle");
}

/// Chromium 156: the `finished` reactions of an ended transition run before
/// its `transitionend` listeners.
#[tokio::test]
async fn css_transition_finished_reactions_run_before_transitionend() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_rendered_test_vm(
        "https://css-transition-finished-order.test/",
        r#"<html><head><style>
#t { width: 10px; height: 10px; opacity: 1; transition: opacity 1ms linear; }
#t.on { opacity: 0; }
</style></head><body><div id="t"></div></body></html>"#,
    );
    vm.eval(
        r#"
(() => {
  const t = document.getElementById('t');
  getComputedStyle(t).opacity;
  t.classList.add('on');
  let done = false;
  t.getAnimations()[0].finished.then(() => { done = true; });
  t.addEventListener('transitionend', () => { globalThis.__seen = done; });
})()
"#,
    )
    .expect("transition should start");
    vm.advance_timers_until_deadline_for_test(&loader)
        .await
        .expect("transition should end");
    let seen = vm
        .eval("String(globalThis.__seen)")
        .expect("listener observation should be readable");
    assert_eq!(seen, "true");
}

/// Chromium 156: an animation frame callback that a transition event listener
/// requests runs in the same frame as one requested before the event.
#[tokio::test]
async fn css_transition_listener_animation_frame_joins_the_current_frame() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_rendered_test_vm(
        "https://css-transition-listener-raf.test/",
        r#"<html><head><style>
#t { position: absolute; left: 0; width: 10px; height: 10px;
     transition: left 3600s steps(1, jump-both); }
#t.on { left: 400px; }
</style></head><body><div id="t"></div></body></html>"#,
    );
    vm.eval(
        r#"
(() => {
  const t = document.getElementById('t');
  getComputedStyle(t).left;
  t.classList.add('on');
  const a = t.getAnimations()[0];
  let one;
  requestAnimationFrame((ts) => { one = ts; });
  t.ontransitionend = () => requestAnimationFrame((ts) => { globalThis.__same = ts === one; });
  a.finish();
})()
"#,
    )
    .expect("transition should finish");
    vm.advance_timers_until_deadline_for_test(&loader)
        .await
        .expect("frames should run");
    let same = vm
        .eval("String(globalThis.__same)")
        .expect("frame observation should be readable");
    assert_eq!(same, "true");
}

/// Chromium 156: a style that depends on a container query starts no
/// transition at the first layout; the first style already sees the
/// container's used size.
#[tokio::test]
async fn css_transition_does_not_start_from_the_pre_layout_container_estimate() {
    let mut vm = new_rendered_test_vm(
        "https://css-transition-container-query.test/",
        r#"<html><head><style>
#c { container-type: inline-size; width: 500px; }
#k { color: rgb(0, 0, 0); transition: color 3600s steps(1, jump-both); }
@container (min-width: 400px) { #k { color: rgb(255, 0, 0); } }
</style></head><body><div id="c"><div id="k">k</div></div></body></html>"#,
    );
    vm.eval("getComputedStyle(document.getElementById('k')).color")
        .expect("first style should resolve");
    publish_layout_for_test(&mut vm);
    let observed = vm
        .eval(
            "(() => { const k = document.getElementById('k'); \
             return [getComputedStyle(k).color, k.getAnimations().length].join('|'); })()",
        )
        .expect("container style should be readable");
    assert_eq!(observed, "rgb(255, 0, 0)|0");
}

const TRANSITION_STYLE: &str = r#"<html><head><style>
.x { position: relative; left: 0; opacity: 0; transition: opacity 3600s steps(1, jump-both); }
.x.on { opacity: 1; }
</style></head><body><div id="t" class="x"></div></body></html>"#;

fn css_transition_task_queued(vm: &ScriptVm) -> bool {
    vm._context_host
        .borrow()
        .style_engine_ref()
        .css_transition_task_due()
        .get()
        .is_some()
}

/// Chromium 156: a class change in a document none of whose styles specifies
/// a transition starts none, and a rule or `style` attribute added later
/// starts one. Until then neither the style change nor its layout queues a transition task.
#[test]
fn css_transition_frames_wait_for_a_style_that_can_specify_transitions() {
    let mut vm = new_rendered_test_vm(
        "https://css-transition-unspecifiable.test/",
        r#"<html><head><style>div { position: absolute; left: 0; } .on { left: 10px; }</style></head>
<body><div id="t"></div><div id="u"></div></body></html>"#,
    );
    vm.eval(
        "(() => { const t = document.getElementById('t'); getComputedStyle(t).left; \
         t.className = 'on'; })()",
    )
    .expect("class change should evaluate");
    // The layout that follows queues the transition task when a style change
    // may start a transition.
    vm.eval("document.getElementById('t').getBoundingClientRect(); 0")
        .expect("layout should evaluate");
    assert!(!css_transition_task_queued(&vm));
    let observed = vm
        .eval(
            r#"
(() => {
  const t = document.getElementById('t');
  const u = document.getElementById('u');
  const before = [getComputedStyle(t).left, t.getAnimations().length];
  document.styleSheets[0].insertRule('#t { transition: left 3600s steps(1, jump-both); }', 2);
  getComputedStyle(t).left;
  t.className = '';
  const ruled = t.getAnimations().length;
  getComputedStyle(u).left;
  u.setAttribute('style', 'transition: left 3600s steps(1, jump-both)');
  getComputedStyle(u).left;
  u.className = 'on';
  return [...before, ruled, u.getAnimations().length].join('|');
})()
"#,
        )
        .expect("transition styles should evaluate");
    assert_eq!(observed, "10px|0|1|1");
    assert!(css_transition_task_queued(&vm));
}

/// Chromium 156: a style flush that cancels a transition makes its
/// `CSSTransition` idle, and the rejection of its `finished` promise waits
/// for the transition task.
#[tokio::test]
async fn css_transition_style_cancel_is_idle_at_the_flush_and_rejects_later() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_rendered_test_vm("https://css-transition-flush-idle.test/", TRANSITION_STYLE);
    let observed = vm
        .eval(
            r#"
(() => {
  const t = document.getElementById('t');
  getComputedStyle(t).opacity;
  t.classList.add('on');
  const a = t.getAnimations()[0];
  globalThis.__result = 'pending';
  a.finished.catch((error) => { globalThis.__result = error.name; });
  t.style.transitionProperty = 'none';
  const flushed = [getComputedStyle(t).opacity, a.playState, t.getAnimations().length];
  Promise.resolve().then(() => Promise.resolve()).then(() => {
    globalThis.__early = globalThis.__result;
  });
  return flushed.join('|');
})()
"#,
        )
        .expect("style cancel should evaluate");
    assert_eq!(observed, "1|idle|0");
    vm.advance_timers_until_deadline_for_test(&loader)
        .await
        .expect("transition task should run");
    let results = vm
        .eval("[globalThis.__early, globalThis.__result].join('|')")
        .expect("results should be readable");
    assert_eq!(results, "pending|AbortError");
}

/// Chromium 156: every `finished` reaction of an ended transition runs
/// before its `transitionend` listeners, a chained one too.
#[tokio::test]
async fn css_transition_finished_reaction_chain_runs_before_transitionend() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_rendered_test_vm(
        "https://css-transition-finished-chain.test/",
        r#"<html><head><style>
#t { width: 10px; height: 10px; opacity: 1; transition: opacity 5ms linear; }
#t.on { opacity: 0; }
</style></head><body><div id="t"></div></body></html>"#,
    );
    vm.eval(
        r#"
(() => {
  const t = document.getElementById('t');
  getComputedStyle(t).opacity;
  t.classList.add('on');
  let state = 0;
  t.getAnimations()[0].finished.then(() => { state = 1; }).then(() => { state = 2; });
  t.addEventListener('transitionend', () => { globalThis.__state = state; });
})()
"#,
    )
    .expect("transition should start");
    vm.advance_timers_until_deadline_for_test(&loader)
        .await
        .expect("transition should end");
    let state = vm
        .eval("String(globalThis.__state)")
        .expect("listener observation should be readable");
    assert_eq!(state, "2");
}

/// Chromium 156: `finish()` and `cancel()` act on a `CSSTransition` in any
/// state: canceling a finished one replaces its `finished` promise, and
/// finishing a canceled one resolves the new promise.
#[test]
fn css_transition_finish_and_cancel_act_from_any_state() {
    let mut vm = new_rendered_test_vm("https://css-transition-any-state.test/", TRANSITION_STYLE);
    let observed = vm
        .eval(
            r#"
(() => {
  const t = document.getElementById('t');
  getComputedStyle(t).opacity;
  t.classList.add('on');
  const a = t.getAnimations()[0];
  a.finish();
  const old = a.finished;
  a.cancel();
  const replaced = old !== a.finished;
  const canceled = a.playState;
  a.finish();
  const u = document.body.appendChild(document.createElement('div'));
  u.className = 'x';
  getComputedStyle(u).opacity;
  u.classList.add('on');
  const b = u.getAnimations()[0];
  b.cancel();
  globalThis.__resolved = false;
  b.finished.then(() => { globalThis.__resolved = true; });
  b.finish();
  return [replaced, canceled, a.playState, b.playState].join('|');
})()
"#,
        )
        .expect("finish and cancel should evaluate");
    assert_eq!(observed, "true|idle|finished|finished");
    let resolved = vm
        .eval("String(globalThis.__resolved)")
        .expect("promise state should be readable");
    assert_eq!(resolved, "true");
}

/// Chromium 156: showing a `display: none` subtree starts no transition from
/// the styles its elements had while hidden.
#[test]
fn css_transition_does_not_start_from_a_hidden_style_on_reveal() {
    let mut vm = new_rendered_test_vm(
        "https://css-transition-reveal.test/",
        r#"<html><body><div id="p" style="display: none"><div id="t"></div></div></body></html>"#,
    );
    let observed = vm
        .eval(
            r#"
(() => {
  const p = document.getElementById('p');
  const t = document.getElementById('t');
  t.style.opacity = '0';
  getComputedStyle(t).opacity;
  p.style.display = 'block';
  t.style.cssText = 'opacity: 1; transition: opacity 3600s steps(1, jump-both)';
  return [getComputedStyle(t).opacity, t.getAnimations().length].join('|');
})()
"#,
        )
        .expect("reveal should evaluate");
    assert_eq!(observed, "1|0");
}

/// Chromium 156: an element removed while its transition runs, then
/// reinserted, starts a new transition to the same end value.
#[test]
fn css_transition_starts_again_after_its_element_is_reinserted() {
    let mut vm = new_rendered_test_vm(
        "https://css-transition-reinsert-again.test/",
        TRANSITION_STYLE,
    );
    let observed = vm
        .eval(
            r#"
(() => {
  const t = document.getElementById('t');
  getComputedStyle(t).opacity;
  t.classList.add('on');
  t.getAnimations();
  t.remove();
  t.classList.remove('on');
  document.body.append(t);
  const initial = getComputedStyle(t).opacity;
  t.classList.add('on');
  return [initial, getComputedStyle(t).opacity, t.getAnimations().length].join('|');
})()
"#,
        )
        .expect("reinsertion should evaluate");
    assert_eq!(observed, "0|0.5|1");
}

/// Chromium 156: `document.open()` cancels the replaced document's
/// transitions; their `CSSTransition`s become idle.
#[test]
fn css_transition_objects_are_idle_after_document_open() {
    let mut vm = new_rendered_test_vm("https://css-transition-open-idle.test/", TRANSITION_STYLE);
    let observed = vm
        .eval(
            r#"
(() => {
  const saved = [];
  for (let i = 0; i < 3; i++) {
    const t = document.body.appendChild(document.createElement('div'));
    t.style.cssText = 'opacity: 0; transition: opacity 3600s steps(1, jump-both)';
    getComputedStyle(t).opacity;
    t.style.opacity = '1';
    saved.push(t.getAnimations()[0]);
    document.open();
    document.write('<body></body>');
    document.close();
  }
  return [document.getAnimations().length,
          saved.filter((a) => a.playState === 'running').length].join('|');
})()
"#,
        )
        .expect("document.open should evaluate");
    assert_eq!(observed, "0|0");
}

/// Chromium 156: moving an element with a running transition into a child
/// frame's document cancels that transition and starts none there.
#[test]
fn css_transition_is_canceled_when_its_element_moves_to_another_document() {
    let mut vm = new_rendered_test_vm("https://css-transition-adopt.test/", TRANSITION_STYLE);
    vm.eval("document.body.appendChild(document.createElement('iframe'))")
        .expect("frame should be inserted");
    vm.drain_pending_child_frame_work_for_test();
    let observed = vm
        .eval(
            r#"
(() => {
  const f = document.querySelector('iframe');
  const t = document.body.appendChild(document.createElement('div'));
  t.style.cssText = 'opacity: 0; transition: opacity 3600s steps(1, jump-both)';
  getComputedStyle(t).opacity;
  t.style.opacity = '1';
  const a = t.getAnimations()[0];
  f.contentDocument.body.appendChild(t);
  return [f.contentWindow.getComputedStyle(t).opacity, t.getAnimations().length,
          a.playState].join('|');
})()
"#,
        )
        .expect("adoption should evaluate");
    assert_eq!(observed, "1|0|idle");
}

/// Chromium 156: another realm's `getAnimations()` returns the same
/// `CSSTransition` as the element's own realm, and it settles.
#[test]
fn css_transition_object_is_shared_across_realms() {
    let mut vm = new_rendered_test_vm("https://css-transition-realms.test/", TRANSITION_STYLE);
    vm.eval("document.body.appendChild(document.createElement('iframe'))")
        .expect("frame should be inserted");
    vm.drain_pending_child_frame_work_for_test();
    let observed = vm
        .eval(
            r#"
(() => {
  const d = document.querySelector('iframe').contentDocument;
  const t = d.body.appendChild(d.createElement('div'));
  t.style.cssText = 'opacity: 0; transition: opacity 3600s steps(1, jump-both)';
  d.defaultView.getComputedStyle(t).opacity;
  t.style.opacity = '1';
  const a = t.getAnimations()[0];
  const b = Element.prototype.getAnimations.call(t)[0];
  a.finish();
  return [a === b, b.playState].join('|');
})()
"#,
        )
        .expect("cross-realm transition should evaluate");
    assert_eq!(observed, "true|finished");
}

/// Chromium 156: assigning a `CSSTransition`'s effect to itself and then
/// canceling it leaves no animation behind.
#[test]
fn css_transition_effect_assignment_keeps_it_out_of_script_animations() {
    let mut vm = new_rendered_test_vm("https://css-transition-effect-self.test/", TRANSITION_STYLE);
    let observed = vm
        .eval(
            r#"
(() => {
  const t = document.getElementById('t');
  for (let i = 0; i < 3; i++) {
    getComputedStyle(t).opacity;
    t.classList.toggle('on');
    const a = t.getAnimations()[0];
    a.effect = a.effect;
    a.cancel();
    getComputedStyle(t).opacity;
  }
  return [t.getAnimations().length, document.getAnimations().length].join('|');
})()
"#,
        )
        .expect("effect assignment should evaluate");
    assert_eq!(observed, "0|0");
}

/// Chromium 156: transitions that end in the same frame send `transitionend`
/// in their elements' tree order.
#[tokio::test]
async fn css_transition_end_events_follow_tree_order() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_rendered_test_vm(
        "https://css-transition-end-order.test/",
        r#"<html><head><style>
.b { position: relative; left: 0; height: 2px; transition: left 1ms linear; }
</style></head><body><div id="root"></div></body></html>"#,
    );
    vm.eval(
        r#"
(() => {
  const root = document.getElementById('root');
  for (let i = 0; i < 8; i++) {
    const d = root.appendChild(document.createElement('div'));
    d.className = 'b';
    d.id = 'e' + i;
  }
  for (const d of root.children) getComputedStyle(d).left;
  globalThis.__ends = [];
  document.addEventListener('transitionend', (e) => __ends.push(e.target.id));
  for (const d of [...root.children].reverse()) d.style.left = '50px';
  for (const d of root.children) getComputedStyle(d).left;
})()
"#,
    )
    .expect("transitions should start");
    vm.advance_timers_until_deadline_for_test(&loader)
        .await
        .expect("transitions should end");
    let ends = vm
        .eval("__ends.join(' ')")
        .expect("end order should be readable");
    assert_eq!(ends, "e0 e1 e2 e3 e4 e5 e6 e7");
}

/// Chromium 156: `commitStyles()` on a finished transition writes its end
/// value to the element's inline style.
#[test]
fn css_transition_commit_styles_writes_its_value() {
    let mut vm = new_rendered_test_vm("https://css-transition-commit.test/", TRANSITION_STYLE);
    let observed = vm
        .eval(
            r#"
(() => {
  const t = document.getElementById('t');
  getComputedStyle(t).opacity;
  t.classList.add('on');
  const a = t.getAnimations()[0];
  a.finish();
  a.commitStyles();
  t.classList.remove('on');
  return [t.style.opacity, getComputedStyle(t).opacity].join('|');
})()
"#,
        )
        .expect("commitStyles should evaluate");
    assert_eq!(observed, "1|1");
}

/// Chromium 156: `KeyframeEffect.prototype.getTiming` throws on an object
/// that is not a `KeyframeEffect`.
#[test]
fn keyframe_effect_get_timing_rejects_other_receivers() {
    let mut vm = new_rendered_test_vm("https://keyframe-effect-get-timing.test/", TRANSITION_STYLE);
    let observed = vm
        .eval(
            "(() => { try { return String(KeyframeEffect.prototype.getTiming.call({}).duration); } \
             catch (e) { return e.name; } })()",
        )
        .expect("getTiming should evaluate");
    assert_eq!(observed, "TypeError");
}

const BODY_ONLY: &str = "<html><head></head><body></body></html>";

fn eval_string(vm: &mut StandaloneScriptVmHarness, script: &str) -> String {
    vm.eval(script).expect("script should evaluate")
}

async fn run_frames_for_test(vm: &mut StandaloneScriptVmHarness, loader: &ResourceRequestClient) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
    vm.advance_timers_until_deadline_for_test_with_deadline(loader, deadline)
        .await
        .expect("frames should run");
}

/// Chromium 156: an element an animation frame inserts has the frame's
/// style as its before-change style when a later task gives it the
/// document's first transition.
#[tokio::test]
async fn css_transition_first_transition_style_finds_the_frame_style() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_rendered_test_vm("https://css-transition-first-style.test/", BODY_ONLY);
    eval_string(
        &mut vm,
        r#"
(() => {
  getComputedStyle(document.body).display;
  requestAnimationFrame(() => {
    const t = document.body.appendChild(document.createElement('div'));
    t.style.opacity = '0';
    setTimeout(() => {
      t.style.cssText = 'opacity: 1; transition: opacity 3600s linear';
      globalThis.__count = t.getAnimations().length;
    }, 0);
  });
})()
"#,
    );
    run_frames_for_test(&mut vm, &loader).await;
    assert_eq!(eval_string(&mut vm, "String(globalThis.__count)"), "1");
}

/// Chromium 156: a style change in a document that cannot specify a
/// transition is styled by the frame that follows, though no task runs
/// for it, so a later task's first transition finds that style. Reading
/// another element's style resolves only that element. A custom property
/// whose value reads "transition" specifies no transition.
#[test]
fn css_transition_skipped_frame_keeps_the_before_change_style() {
    let mut vm = new_rendered_test_vm("https://css-transition-skipped-frame.test/", BODY_ONLY);
    eval_string(
        &mut vm,
        r#"
(() => {
  const t = document.body.appendChild(document.createElement('div'));
  t.style.opacity = '0';
  globalThis.__t = t;
  document.createElement('div').setAttribute('style', '--label: transition');
  document.body.style.setProperty('--note', 'transition');
  getComputedStyle(document.body).display;
  return '';
})()
"#,
    );
    assert!(!css_transition_task_queued(&vm));
    std::thread::sleep(std::time::Duration::from_millis(20));
    // The next task starts by running the skipped frame's style update.
    vm.resolve_unresolved_css_transition_frame_roots();
    let observed = eval_string(
        &mut vm,
        "(() => { __t.style.cssText = 'opacity: 1; transition: opacity 3600s linear'; \
         return String(__t.getAnimations().length); })()",
    );
    assert_eq!(observed, "1");
}

/// Chromium 156: a flushed `display: none` on an ancestor cancels the
/// transition, though the ancestor shows again before anyone looks.
#[test]
fn css_transition_is_canceled_by_a_flushed_hidden_ancestor() {
    let mut vm = new_rendered_test_vm("https://css-transition-hidden-parent.test/", BODY_ONLY);
    let observed = eval_string(
        &mut vm,
        r#"
(() => {
  const p = document.body.appendChild(document.createElement('div'));
  const t = p.appendChild(document.createElement('div'));
  t.style.cssText = 'opacity: 0; transition: opacity 3600s linear';
  getComputedStyle(t).opacity;
  t.style.opacity = '1';
  const a = t.getAnimations()[0];
  p.style.display = 'none';
  getComputedStyle(p).display;
  p.style.display = 'block';
  return [a.playState, t.getAnimations().includes(a)].join('|');
})()
"#,
    );
    assert_eq!(observed, "idle|false");
}

/// Chromium 156: a removed element's style is discarded, so a transition
/// it was given while removed does not start at its reinsertion.
#[test]
fn css_transition_does_not_start_from_the_style_before_removal() {
    let mut vm = new_rendered_test_vm("https://css-transition-removed-style.test/", BODY_ONLY);
    let observed = eval_string(
        &mut vm,
        r#"
(() => {
  const t = document.body.appendChild(document.createElement('div'));
  t.style.opacity = '0';
  getComputedStyle(t).opacity;
  t.remove();
  document.body.getBoundingClientRect();
  t.style.cssText = 'opacity: 1; transition: opacity 60s steps(1, jump-both)';
  document.body.append(t);
  return [getComputedStyle(t).opacity, t.getAnimations().length].join('|');
})()
"#,
    );
    assert_eq!(observed, "1|0");
}

/// Chromium 156 reports no transitions after removing elements with
/// transition styles; the transitions keep no state for them either.
#[test]
fn css_transition_state_does_not_grow_with_removed_elements() {
    let mut vm = new_rendered_test_vm("https://css-transition-removed-state.test/", BODY_ONLY);
    let observed = eval_string(
        &mut vm,
        r#"
(() => {
  for (let i = 0; i < 1000; i++) {
    const t = document.body.appendChild(document.createElement('div'));
    t.style.transition = 'opacity 60s';
    getComputedStyle(t).opacity;
    t.remove();
    document.getAnimations();
  }
  return String(document.getAnimations().length);
})()
"#,
    );
    assert_eq!(observed, "0");
    let fresh = vm
        ._context_host
        .borrow()
        .style_engine_ref()
        .css_transition_fresh_count_for_test();
    assert!(fresh < 10, "fresh={fresh}");
}

/// Chromium 156: `moveBefore()` keeps the moved element's transition.
#[test]
fn css_transition_survives_move_before() {
    let mut vm = new_rendered_test_vm("https://css-transition-move-before.test/", BODY_ONLY);
    let observed = eval_string(
        &mut vm,
        r#"
(() => {
  const destination = document.body.appendChild(document.createElement('div'));
  const t = document.body.appendChild(document.createElement('div'));
  t.style.cssText = 'opacity: 0; transition: opacity 3600s linear';
  getComputedStyle(t).opacity;
  t.style.opacity = '1';
  const a = t.getAnimations()[0];
  destination.moveBefore(t, null);
  return [a.playState, t.getAnimations()[0] === a].join('|');
})()
"#,
    );
    assert_eq!(observed, "running|true");
}

/// Chromium 156: removing a frame cancels its transitions; the parent's
/// reference to one sees it idle.
#[tokio::test]
async fn css_transition_object_of_a_removed_frame_becomes_idle() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_rendered_test_vm("https://css-transition-removed-frame.test/", BODY_ONLY);
    vm.eval("document.body.appendChild(document.createElement('iframe'))")
        .expect("frame should be inserted");
    vm.drain_pending_child_frame_work_for_test();
    eval_string(
        &mut vm,
        r#"
(() => {
  const d = document.querySelector('iframe').contentDocument;
  const t = d.body.appendChild(d.createElement('div'));
  t.style.cssText = 'opacity: 0; transition: opacity 3600s linear';
  d.defaultView.getComputedStyle(t).opacity;
  t.style.opacity = '1';
  globalThis.__a = t.getAnimations()[0];
  return '';
})()
"#,
    );
    run_frames_for_test(&mut vm, &loader).await;
    eval_string(&mut vm, "document.querySelector('iframe').remove(); ''");
    run_frames_for_test(&mut vm, &loader).await;
    assert_eq!(eval_string(&mut vm, "__a.playState"), "idle");
}

/// Chromium 156: an element's `transitionend` never precedes its own
/// `transitionstart`, whatever the tree order of the transitions ending
/// with it.
#[tokio::test]
async fn css_transition_end_follows_its_own_start() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_rendered_test_vm("https://css-transition-start-end.test/", BODY_ONLY);
    eval_string(
        &mut vm,
        r#"
(() => {
  const start = () => {
    const t = document.body.appendChild(document.createElement('div'));
    t.style.cssText = 'opacity: 0; transition: opacity 60s linear 60s';
    getComputedStyle(t).opacity;
    t.style.opacity = '1';
    return [t, t.getAnimations()[0]];
  };
  [globalThis.__x, globalThis.__a] = start();
  globalThis.__b = start()[1];
  return '';
})()
"#,
    );
    run_frames_for_test(&mut vm, &loader).await;
    eval_string(
        &mut vm,
        r#"
(() => {
  globalThis.__seen = [];
  for (const type of ['transitionstart', 'transitionend']) {
    __x.addEventListener(type, (event) => __seen.push(event.type));
  }
  __b.finish();
  __a.finish();
  return '';
})()
"#,
    );
    run_frames_for_test(&mut vm, &loader).await;
    assert_eq!(
        eval_string(&mut vm, "__seen.join(',')"),
        "transitionstart,transitionend"
    );
}

/// Chromium 156: `cancel()` on a transition a style change already
/// canceled leaves the rejection of `finished` to the transition task.
#[test]
fn css_transition_second_cancel_leaves_the_rejection_to_the_task() {
    let mut vm = new_rendered_test_vm("https://css-transition-second-cancel.test/", BODY_ONLY);
    eval_string(
        &mut vm,
        r#"
(() => {
  const t = document.body.appendChild(document.createElement('div'));
  t.style.cssText = 'opacity: 0; transition: opacity 3600s linear';
  getComputedStyle(t).opacity;
  t.style.opacity = '1';
  const a = t.getAnimations()[0];
  let result = 'pending';
  a.finished.catch((error) => { result = error.name; });
  t.style.transitionProperty = 'none';
  void a.playState;
  a.cancel();
  Promise.resolve().then(() => { globalThis.__early = result; });
  return '';
})()
"#,
    );
    assert_eq!(eval_string(&mut vm, "globalThis.__early"), "pending");
}

/// Chromium 156: a `finished` promise first read after a style change
/// canceled its transition stays pending and keeps its identity.
#[tokio::test]
async fn css_transition_finished_first_read_after_a_style_cancel_stays_pending() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_rendered_test_vm("https://css-transition-late-finished.test/", BODY_ONLY);
    eval_string(
        &mut vm,
        r#"
(() => {
  const t = document.body.appendChild(document.createElement('div'));
  t.style.cssText = 'opacity: 0; transition: opacity 3600s linear';
  getComputedStyle(t).opacity;
  t.style.opacity = '1';
  globalThis.__t = t;
  globalThis.__a = t.getAnimations()[0];
  return '';
})()
"#,
    );
    run_frames_for_test(&mut vm, &loader).await;
    eval_string(
        &mut vm,
        r#"
(() => {
  __t.style.transitionProperty = 'none';
  void __a.playState;
  globalThis.__p = __a.finished;
  globalThis.__result = 'pending';
  __p.catch((error) => { globalThis.__result = error.name; });
  return '';
})()
"#,
    );
    run_frames_for_test(&mut vm, &loader).await;
    assert_eq!(
        eval_string(&mut vm, "[__p === __a.finished, __result].join('|')"),
        "true|pending"
    );
}

/// Chromium 156: `commitStyles()` throws `InvalidStateError` for a target
/// that is not rendered.
#[test]
fn css_transition_commit_styles_rejects_an_unrendered_target() {
    let mut vm = new_rendered_test_vm("https://css-transition-commit-hidden.test/", BODY_ONLY);
    let observed = eval_string(
        &mut vm,
        r#"
(() => {
  const t = document.body.appendChild(document.createElement('div'));
  t.style.cssText = 'opacity: 0; transition: opacity 3600s linear';
  getComputedStyle(t).opacity;
  t.style.opacity = '1';
  const a = t.getAnimations()[0];
  t.style.display = 'none';
  void a.playState;
  try { a.commitStyles(); return 'returned'; } catch (e) { return e.name; }
})()
"#,
    );
    assert_eq!(observed, "InvalidStateError");
}

/// Chromium 156: `commitStyles()` writes the computed value, so a unitless
/// `line-height` stays relative to the font size.
#[test]
fn css_transition_commit_styles_keeps_a_relative_line_height() {
    let mut vm = new_rendered_test_vm("https://css-transition-commit-line-height.test/", BODY_ONLY);
    let observed = eval_string(
        &mut vm,
        r#"
(() => {
  const t = document.body.appendChild(document.createElement('div'));
  t.style.cssText = 'font-size: 10px; line-height: 1; transition: line-height 60s';
  getComputedStyle(t).lineHeight;
  t.style.lineHeight = '1.5';
  const [a] = t.getAnimations();
  a.finish();
  a.commitStyles();
  t.style.transition = 'none';
  t.style.fontSize = '20px';
  return [t.style.lineHeight, getComputedStyle(t).lineHeight].join('|');
})()
"#,
    );
    assert_eq!(observed, "1.5|30px");
}

/// Chromium 156: `commitStyles()` writes through the native declaration,
/// not a `setProperty` that script replaced.
#[test]
fn css_transition_commit_styles_ignores_a_replaced_set_property() {
    let mut vm = new_rendered_test_vm("https://css-transition-commit-native.test/", BODY_ONLY);
    let observed = eval_string(
        &mut vm,
        r#"
(() => {
  const t = document.body.appendChild(document.createElement('div'));
  t.style.cssText = 'opacity: 0; transition: opacity 3600s linear';
  getComputedStyle(t).opacity;
  t.style.opacity = '1';
  const a = t.getAnimations()[0];
  a.finish();
  let calls = 0;
  Object.defineProperty(t.style, 'setProperty', { value() { calls++; } });
  a.commitStyles();
  return String(calls);
})()
"#,
    );
    assert_eq!(observed, "0");
}

/// Chromium 156: `getTiming()` is a method of `AnimationEffect.prototype`.
#[test]
fn animation_effect_prototype_has_get_timing() {
    let mut vm = new_rendered_test_vm("https://animation-effect-get-timing.test/", BODY_ONLY);
    let observed = eval_string(
        &mut vm,
        "[typeof AnimationEffect.prototype.getTiming, \
         Object.hasOwn(KeyframeEffect.prototype, 'getTiming')].join('|')",
    );
    assert_eq!(observed, "function|false");
}

/// Chromium 156: an element a task appends without reading any style is
/// styled by the frame that follows, so a later task's first transition
/// finds that style.
#[test]
fn css_transition_skipped_frame_styles_an_unread_append() {
    let mut vm = new_rendered_test_vm("https://css-transition-unread-append.test/", BODY_ONLY);
    eval_string(
        &mut vm,
        "(() => { globalThis.__d = document.body.appendChild(document.createElement('div')); \
         return ''; })()",
    );
    std::thread::sleep(std::time::Duration::from_millis(20));
    vm.resolve_unresolved_css_transition_frame_roots();
    let observed = eval_string(
        &mut vm,
        "(() => { __d.style.cssText = 'opacity: 0; transition: opacity 3600s linear'; \
         return String(__d.getAnimations().length); })()",
    );
    assert_eq!(observed, "1");
}

/// Chromium 156: a class change no task reads is styled by the frame that
/// follows, so a later task's first transition starts from its style.
#[test]
fn css_transition_skipped_frame_styles_an_unread_class_change() {
    let mut vm = new_rendered_test_vm(
        "https://css-transition-unread-class.test/",
        "<html><head><style>.a{opacity:0}</style></head><body></body></html>",
    );
    eval_string(
        &mut vm,
        "(() => { globalThis.__d = document.body.appendChild(document.createElement('div')); \
         return getComputedStyle(__d).opacity; })()",
    );
    std::thread::sleep(std::time::Duration::from_millis(20));
    vm.resolve_unresolved_css_transition_frame_roots();
    eval_string(&mut vm, "(() => { __d.className = 'a'; return ''; })()");
    std::thread::sleep(std::time::Duration::from_millis(20));
    vm.resolve_unresolved_css_transition_frame_roots();
    let observed = eval_string(
        &mut vm,
        "(() => { __d.style.cssText = 'opacity: 1; transition: opacity 3600s linear'; \
         return String(__d.getAnimations().length); })()",
    );
    assert_eq!(observed, "1");
}

async fn run_frames_for_ms(
    vm: &mut StandaloneScriptVmHarness,
    loader: &ResourceRequestClient,
    milliseconds: u64,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(milliseconds);
    vm.advance_timers_until_deadline_for_test_with_deadline(loader, deadline)
        .await
        .expect("frames should run");
}

/// Chromium 156: while a transition runs, a frame still restyles the
/// candidates under its element: a descendant that inherits a transitioned
/// color, or resolves `em` against a transitioned font size, starts its own
/// transition within the first frames, though no script reads it.
#[tokio::test]
async fn css_transition_frames_restyle_candidates_under_a_running_transition() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_rendered_test_vm(
        "https://css-transition-inherited-sample.test/",
        r#"<html><head><style>
#p { color: rgb(0, 0, 0); font-size: 10px; transition: color 1s linear, font-size 1s linear; }
#p.on { color: rgb(255, 0, 0); font-size: 20px; }
#c { transition: color 3600s linear; }
#e { width: 1em; height: 1px; transition: width 3600s linear; }
</style></head><body><div id="p"><span id="c">x</span><div id="e"></div></div></body></html>"#,
    );
    eval_string(
        &mut vm,
        r#"
(() => {
  const p = document.getElementById('p');
  const c = document.getElementById('c');
  const e = document.getElementById('e');
  getComputedStyle(c).color;
  getComputedStyle(e).width;
  getComputedStyle(p).color;
  globalThis.__runs = { c: 0, e: 0 };
  c.addEventListener('transitionrun', (event) => { if (event.target === c) __runs.c++; });
  e.addEventListener('transitionrun', (event) => { if (event.target === e) __runs.e++; });
  return '';
})()
"#,
    );
    eval_string(
        &mut vm,
        "(() => { document.getElementById('p').className = 'on'; return ''; })()",
    );
    run_frames_for_ms(&mut vm, &loader, 100).await;
    assert_eq!(
        eval_string(&mut vm, "[__runs.c > 0, __runs.e > 0].join('|')"),
        "true|true"
    );
}

/// Chromium 156: a stylesheet change no task reads is applied by the frame
/// that follows. A Document without transitions restyles the elements it
/// changes, so a later task's first transition starts from that style, for
/// an inserted rule and an appended `style` element alike; an element whose
/// style specifies a transition starts it, though no script reads it.
#[tokio::test]
async fn css_transition_frames_apply_unread_stylesheet_changes() {
    let mut vm = new_rendered_test_vm(
        "https://css-transition-unread-sheet.test/",
        r#"<html><head><style>#t { opacity: 1; } #u { opacity: 1; }</style></head>
<body><div id="t"></div><div id="u"></div></body></html>"#,
    );
    eval_string(
        &mut vm,
        "(() => { getComputedStyle(document.getElementById('t')).opacity; \
         getComputedStyle(document.getElementById('u')).opacity; return ''; })()",
    );
    std::thread::sleep(std::time::Duration::from_millis(20));
    vm.resolve_unresolved_css_transition_frame_roots();
    eval_string(
        &mut vm,
        "(() => { document.styleSheets[0].insertRule('#t { opacity: 0; }', 2); \
         document.head.appendChild(document.createElement('style')).textContent = \
         '#u { opacity: 0; }'; return ''; })()",
    );
    std::thread::sleep(std::time::Duration::from_millis(20));
    vm.resolve_unresolved_css_transition_frame_roots();
    let observed = eval_string(
        &mut vm,
        r#"
(() => {
  const t = document.getElementById('t');
  const u = document.getElementById('u');
  t.style.transition = 'opacity 3600s linear';
  u.style.transition = 'opacity 3600s linear';
  return [t.getAnimations().length, u.getAnimations().length,
    getComputedStyle(t).opacity, getComputedStyle(u).opacity].join('|');
})()
"#,
    );
    assert_eq!(observed, "0|0|0|0");

    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_rendered_test_vm(
        "https://css-transition-unread-rule.test/",
        r#"<html><head><style>#t { opacity: 1; transition: opacity 20ms linear; }</style></head>
<body><div id="t"></div></body></html>"#,
    );
    eval_string(
        &mut vm,
        "(() => { const t = document.getElementById('t'); getComputedStyle(t).opacity; \
         globalThis.__ended = 0; t.addEventListener('transitionend', () => { __ended++; }); \
         return ''; })()",
    );
    eval_string(
        &mut vm,
        "(() => { document.styleSheets[0].insertRule('#t { opacity: 0; }', 1); return ''; })()",
    );
    run_frames_for_ms(&mut vm, &loader, 300).await;
    assert_eq!(eval_string(&mut vm, "String(__ended)"), "1");
}

/// Chromium 156: a class change no task reads gets the frame that follows,
/// so the transition it starts runs and ends without another task.
#[tokio::test]
async fn css_transition_unread_class_change_gets_its_frame() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).expect("loader");
    let mut vm = new_rendered_test_vm(
        "https://css-transition-unread-class-frame.test/",
        r#"<html><head><style>
#b { opacity: 1; }
#b.fade { opacity: 0; transition: opacity 20ms linear; }
</style></head><body><div id="b"></div></body></html>"#,
    );
    eval_string(
        &mut vm,
        "(() => { const b = document.getElementById('b'); getComputedStyle(b).opacity; \
         globalThis.__ended = 0; b.addEventListener('transitionend', () => { __ended++; }); \
         return ''; })()",
    );
    eval_string(
        &mut vm,
        "(() => { document.getElementById('b').classList.add('fade'); return ''; })()",
    );
    run_frames_for_ms(&mut vm, &loader, 300).await;
    assert_eq!(eval_string(&mut vm, "String(__ended)"), "1");
}
