use super::*;

#[test]
fn focus_prevent_scroll_controls_real_nested_scroll_container_reveal() {
    let mut vm = new_storage_test_vm("https://focus-prevent-scroll.test/");

    vm.eval(
        r#"
(() => {
  const root = document.documentElement ||
    document.appendChild(document.createElement('html'));
  const body = document.body || root.appendChild(document.createElement('body'));
  body.innerHTML = `
    <button id="first">first</button>
    <div id="scroller" style="width:100px;height:100px;overflow:auto">
      <div style="width:500px;height:400px"></div>
      <button id="target" style="margin-left:400px">target</button>
    </div>`;
  return 'installed';
})()
"#,
    )
    .expect("focus scroll fixture should initialize");
    refresh_layout_for_test(&mut vm);

    vm.eval(
        r#"
(() => {
  const first = document.getElementById('first');
  const scroller = document.getElementById('scroller');
  const target = document.getElementById('target');

  target.focus({ preventScroll: true });
  const prevented = scroller.scrollLeft === 0 && scroller.scrollTop === 0;
  const focused = document.activeElement === target;
  first.focus();
  target.focus();
  window.beforeReveal = [prevented, focused];
})()
"#,
    )
    .expect("focus preventScroll probe should evaluate");

    publish_layout_for_test(&mut vm);
    let result = vm
        .eval("beforeReveal.concat(scroller.scrollLeft > 0, scroller.scrollTop > 0).join('|')")
        .unwrap();
    assert_eq!(result, "true|true|true|true");
}

#[test]
fn focusing_contenteditable_in_child_frame_reveals_authored_frame_position() {
    let mut vm = new_storage_test_vm("https://focus-scroll.test/");

    vm.eval(
        r#"
  const root = document.documentElement ||
    document.appendChild(document.createElement('html'));
  const head = document.head || root.appendChild(document.createElement('head'));
  const body = document.body || root.appendChild(document.createElement('body'));
  const style = document.createElement('style');
  style.textContent = `
    iframe { position: absolute; left: 250vw; }
    .spacer { width: 100vw; height: 250vh; }
  `;
  head.appendChild(style);

  const first = document.createElement('div');
  first.contentEditable = 'true';
  const spacer = document.createElement('div');
  spacer.className = 'spacer';
  const frame = document.createElement('iframe');
  body.append(first, spacer, frame);

  const childDocument = frame.contentDocument;
  childDocument.open();
  childDocument.write('<div id="target" contenteditable="true">target</div>');
  childDocument.close();
  const target = childDocument.getElementById('target');
"#,
    )
    .expect("prepare child focus fixture");
    publish_layout_for_test(&mut vm);
    let result = vm
        .eval(
            r#"(() => {
  first.focus();
  target.focus();
  const firstX = window.scrollX;
  const firstY = window.scrollY;

  window.scroll(0, 0);
  first.focus();
  target.focus();
  return JSON.stringify({
    beyondViewport: firstX > window.innerWidth && firstY > window.innerHeight,
    repeated: firstX === window.scrollX && firstY === window.scrollY,
    parentRetargeted: document.activeElement === frame,
    childFocused: childDocument.activeElement === target
  });
})()"#,
        )
        .expect("child contenteditable focus scroll probe should evaluate");

    assert_eq!(
        result,
        r#"{"beyondViewport":true,"repeated":true,"parentRetargeted":true,"childFocused":true}"#
    );
}

#[test]
fn focusing_frame_owner_then_input_dispatches_child_window_focus_and_blur() {
    let mut vm = new_storage_test_vm("https://focus-frame-window.test/");

    let result = vm
        .eval(
            r#"
(() => {
  const root = document.documentElement ||
    document.appendChild(document.createElement('html'));
  const body = document.body || root.appendChild(document.createElement('body'));
  const input = document.createElement('input');
  const frame = document.createElement('iframe');
  body.append(input, frame);
  const log = [];
  window.onblur = () => log.push('top-window-blur');
  window.onfocus = () => log.push('top-window-focus');
  frame.onfocus = () => log.push('frame-focus');
  frame.onblur = () => log.push('frame-blur');
  frame.contentWindow.onfocus = () => log.push('child-window-focus');
  frame.contentWindow.onblur = () => log.push('child-window-blur');
  input.onfocus = () => log.push('input-focus');

  frame.focus();
  input.focus();
  return `${log.join(',')}|${document.activeElement === input}`;
})()
"#,
        )
        .expect("frame window focus transition probe should evaluate");

    assert_eq!(
        result,
        "top-window-blur,frame-focus,child-window-focus,frame-blur,child-window-blur,input-focus,top-window-focus|true"
    );
}

fn press_tab(vm: &mut StandaloneScriptVmHarness, shift: bool) {
    let modifiers = if shift { 8 } else { 0 };
    vm.dispatch_key_event("keydown", "Tab", "Tab", "", modifiers, false, false)
        .expect("Tab keydown should dispatch");
    vm.dispatch_key_event("keyup", "Tab", "Tab", "", modifiers, false, false)
        .expect("Tab keyup should dispatch");
}

fn focused_path(vm: &mut StandaloneScriptVmHarness) -> String {
    vm.eval(
        r#"(() => {
  const name = element => element ? element.id || element.localName : 'null';
  const frame = document.getElementById('frame');
  const child = frame && frame.contentDocument;
  return child && document.activeElement === frame
    ? `frame>${name(child.activeElement)}`
    : name(document.activeElement);
})()"#,
    )
    .expect("focused path should evaluate")
}

#[test]
fn tab_key_moves_focus_between_controls_inside_child_frame_and_back_out() {
    let mut vm = new_parsed_test_vm(
        "https://tab-child-frame.test/",
        r#"<html><body>
            <button id="pre">pre</button>
            <iframe id="frame"></iframe>
            <button id="post">post</button>
        </body></html>"#,
    );
    vm.eval(
        r#"
(() => {
  const childDocument = document.getElementById('frame').contentDocument;
  childDocument.open();
  childDocument.write('<button id="a">a</button><button id="b">b</button><input id="e">');
  childDocument.close();
  window.__childKeydowns = [];
  childDocument.addEventListener('keydown', event => __childKeydowns.push(event.target.id));
  childDocument.getElementById('a').focus();
})()
"#,
    )
    .expect("child frame fixture should initialize");

    let mut path = vec![focused_path(&mut vm)];
    for shift in [false, false, false, true, true, true, true] {
        press_tab(&mut vm, shift);
        path.push(focused_path(&mut vm));
    }
    let child_keydowns = vm
        .eval("__childKeydowns.join(',')")
        .expect("child keydown log should evaluate");

    assert_eq!(
        path.join("|"),
        "frame>a|frame>b|frame>e|post|frame>e|frame>b|frame>a|pre"
    );
    assert_eq!(child_keydowns, "a,b,e,e,b,a");
}

#[test]
fn tab_key_skips_visibility_hidden_and_display_none_controls() {
    let mut vm = new_parsed_test_vm(
        "https://tab-unrendered-controls.test/",
        r#"<html><body>
            <button id="first">first</button>
            <button id="hidden" style="visibility:hidden">hidden</button>
            <button id="none" style="display:none">none</button>
            <div style="display:none"><input id="nested"></div>
            <div style="visibility:hidden"><button id="shown" style="visibility:visible">shown</button></div>
            <button id="last">last</button>
        </body></html>"#,
    );
    vm.eval("document.getElementById('first').focus()")
        .expect("first control should focus");

    let mut path = vec![focused_path(&mut vm)];
    for shift in [false, false, true, true] {
        press_tab(&mut vm, shift);
        path.push(focused_path(&mut vm));
    }

    assert_eq!(path.join("|"), "first|shown|last|shown|first");
}

#[test]
fn tab_key_does_not_enter_inert_hidden_or_closed_details_frames() {
    let mut vm = new_parsed_test_vm(
        "https://tab-unfocusable-frames.test/",
        r#"<html><body>
            <button id="pre">pre</button>
            <div inert><iframe id="inert"></iframe></div>
            <iframe id="hidden" style="visibility:hidden"></iframe>
            <div style="visibility:hidden"><iframe id="hidden-parent"></iframe></div>
            <details><summary>s</summary><iframe id="closed-details"></iframe></details>
            <button id="post">post</button>
        </body></html>"#,
    );
    vm.eval(
        r#"
(() => {
  for (const frame of document.querySelectorAll('iframe')) {
    const childDocument = frame.contentDocument;
    childDocument.open();
    childDocument.write(`<button id="${frame.id}-button">q</button>`);
    childDocument.close();
  }
  document.getElementById('pre').focus();
})()
"#,
    )
    .expect("unfocusable frame fixture should initialize");

    let focused = |vm: &mut StandaloneScriptVmHarness| {
        vm.eval(
            r#"(() => {
  const active = document.activeElement;
  return active.localName === 'iframe'
    ? `${active.id}>${active.contentDocument.activeElement.id}`
    : active.id;
})()"#,
        )
        .expect("focused element should evaluate")
    };
    let mut path = vec![focused(&mut vm)];
    for shift in [false, true] {
        press_tab(&mut vm, shift);
        path.push(focused(&mut vm));
    }

    assert_eq!(path.join("|"), "pre|post|pre");
}

#[test]
fn tab_key_stops_on_scroller_whose_only_focusable_descendant_is_hidden() {
    let mut vm = new_rendered_test_vm(
        "https://tab-scroller-hidden-descendant.test/",
        r#"<html><body>
            <button id="pre">pre</button>
            <div id="scroller" style="overflow:auto;height:40px">
              <div style="height:200px"></div>
              <button style="visibility:hidden">h</button>
            </div>
            <button id="post">post</button>
        </body></html>"#,
    );
    vm.eval("document.getElementById('pre').focus()")
        .expect("pre should focus");

    let mut path = vec![focused_path(&mut vm)];
    for shift in [false, false, true] {
        press_tab(&mut vm, shift);
        path.push(focused_path(&mut vm));
    }

    assert_eq!(path.join("|"), "pre|scroller|post|scroller");
}
