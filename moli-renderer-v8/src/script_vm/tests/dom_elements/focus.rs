use super::*;

#[test]
fn editing_focus_initializes_selection_before_events_and_preserves_existing_ranges() {
    let mut vm = new_storage_html_test_vm("https://example.com/");
    let result = vm.eval(r#"(() => {
        document.body.innerHTML='<div id=a contenteditable><span>abcd</span></div><div id=b contenteditable>wxyz</div>';
        const a=document.getElementById('a'),b=document.getElementById('b'),s=getSelection();
        const log=[];
        for(const node of [a,b])node.addEventListener('focus',()=>{
            log.push([node.id,s.anchorNode===node.querySelector('span')?.firstChild || s.anchorNode===node.firstChild,s.anchorOffset,s.focusOffset,s.toString()]);
        });
        a.focus();s.collapse(a.firstChild.firstChild,2);b.focus();a.focus();
        s.setBaseAndExtent(b.firstChild,1,b.firstChild,3);b.focus();
        a.addEventListener('focus',()=>s.collapse(a.firstChild.firstChild,3),{once:true});
        a.focus();
        const selectedByAuthor=s.anchorNode===a.firstChild.firstChild && s.anchorOffset===3;
        return JSON.stringify({log,selectedByAuthor});
    })()"#).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&result).unwrap(),
        serde_json::json!({
            "log":[["a",true,0,0,""],["b",true,0,0,""],["a",true,0,0,""],
                ["b",true,1,3,"xy"],["a",true,0,0,""]],"selectedByAuthor":true,
        })
    );
}

#[test]
fn editing_focus_uses_native_selection_boundaries_without_author_accessors() {
    let mut vm = new_storage_html_test_vm("https://example.com/");
    assert_eq!(vm.eval(r#"(() => {
        document.body.innerHTML='<div id=editor contenteditable>text</div>';
        const e=document.getElementById('editor'),text=e.firstChild,s=getSelection();
        const anchor=Object.getOwnPropertyDescriptor(Selection.prototype,'anchorNode').get;
        let calls=0;const fail=()=>{calls++;throw new Error('author getter');};
        for(const [node,key] of [[e,'ownerDocument'],[text,'ownerDocument'],[text,'nodeType'],[s,'anchorNode'],[s,'focusNode']])
            Object.defineProperty(node,key,{configurable:true,get:fail});
        window.getSelection=fail;window.Range=fail;s.collapse=fail;e.focus();
        return String(calls===0 && anchor.call(s)===text);
    })()"#).unwrap(),"true");
}

#[test]
fn editing_focus_skips_hidden_text_and_empty_markup_but_stops_before_noneditable_content() {
    for (prefix, at_host) in [
        ("<span style='display:none'>hidden</span>", false),
        ("<span style='visibility:hidden'>hidden</span>", false),
        (
            "<span style='display:block;content-visibility:hidden'>hidden</span>",
            false,
        ),
        ("<span inert>hidden</span>", true),
        ("<span></span>", false),
        ("<!--comment-->", false),
        ("<span contenteditable=false>skip</span>", true),
        ("<br>", true),
    ] {
        let mut vm = new_rendered_test_vm(
            "https://example.com/",
            &format!("<!doctype html><div id=editor contenteditable>{prefix}visible</div>"),
        );
        assert_eq!(vm.eval(&format!(r#"(() => {{
            const e=document.getElementById('editor');e.focus();const s=getSelection();
            return String(s.anchorNode===({at_host}?e:e.lastChild) && s.anchorOffset===0 && s.isCollapsed);
        }})()"#)).unwrap(), "true", "{prefix}");
    }
}

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
            <details><summary id="summary">s</summary><iframe id="closed-details"></iframe></details>
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
    for shift in [false, false, true, true] {
        press_tab(&mut vm, shift);
        path.push(focused(&mut vm));
    }

    // Chromium 156 stops on the closed details' main summary, not its frame.
    assert_eq!(path.join("|"), "pre|summary|post|summary|pre");
}

fn press_activation_key(vm: &mut StandaloneScriptVmHarness, key: &str) {
    let (code, text) = if key == "Enter" {
        ("Enter", "\r")
    } else {
        ("Space", " ")
    };
    vm.dispatch_key_event("keydown", key, code, text, 0, false, true)
        .expect("activation keydown should dispatch");
    vm.dispatch_key_event("keyup", key, code, "", 0, false, false)
        .expect("activation keyup should dispatch");
}

#[test]
fn main_summary_takes_tab_focus_and_toggles_details_from_enter_and_space() {
    let mut vm = new_rendered_test_vm(
        "https://summary-keyboard.test/",
        r#"<html><body>
            <button id="pre">pre</button>
            <details id="details"><summary id="main">main</summary><p>body</p></details>
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
    assert_eq!(path.join("|"), "pre|main|post|main");

    let state = |vm: &mut StandaloneScriptVmHarness| {
        vm.eval("`${document.getElementById('details').open}:${document.activeElement.id}`")
            .expect("details state should evaluate")
    };
    let mut states = Vec::new();
    for key in ["Enter", " ", "Enter"] {
        press_activation_key(&mut vm, key);
        states.push(state(&mut vm));
    }
    // Chromium 156: each Enter or Space toggles and focus stays on the summary.
    assert_eq!(states.join("|"), "true:main|false:main|true:main");
}

#[test]
fn only_the_main_summary_of_a_details_is_focusable() {
    let mut vm = new_rendered_test_vm(
        "https://summary-focusable.test/",
        r#"<html><body>
            <button id="pre">pre</button>
            <details open><summary id="main">main</summary><summary id="second">second</summary></details>
            <summary id="loose">loose</summary>
        </body></html>"#,
    );
    let result = vm
        .eval(
            r#"(() => ["main", "second", "loose"].map(id => {
  document.getElementById("pre").focus();
  const summary = document.getElementById(id);
  summary.focus();
  return `${id}:${document.activeElement.id}:${summary.tabIndex}`;
}).join(","))()"#,
        )
        .expect("summary focus probe should evaluate");

    // Chromium 156 focuses only the first summary child of a details.
    assert_eq!(result, "main:main:0,second:pre:-1,loose:pre:-1");
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

#[test]
fn redispatching_a_saved_focus_event_makes_it_untrusted() {
    let mut vm = new_storage_test_vm("https://focus-redispatch-untrusted.test/");

    // dispatchEvent initializes isTrusted to false, so a page cannot replay
    // a user-agent event as trusted.
    let result = vm
        .eval(
            r#"
(() => {
  const root = document.documentElement ||
    document.appendChild(document.createElement('html'));
  const body = document.body || root.appendChild(document.createElement('body'));
  body.innerHTML = '<button id="a">a</button><button id="b">b</button>';
  const a = document.getElementById('a');
  const b = document.getElementById('b');
  const log = [];
  let saved = null;
  a.addEventListener('focus', (event) => { saved = event; log.push('ua:' + event.isTrusted); });
  b.addEventListener('focus', (event) => log.push('replayed:' + event.isTrusted));
  a.focus();
  b.dispatchEvent(saved);
  log.push('after:' + saved.isTrusted);
  return log.join('|');
})()
"#,
        )
        .expect("focus redispatch probe should evaluate");
    assert_eq!(result, "ua:true|replayed:false|after:false");
}

#[test]
fn key_events_reach_focused_frame_document_body_and_frame_document_has_focus() {
    let mut vm = new_parsed_test_vm(
        "https://key-target-focused-frame.test/",
        r#"<html><body>
            <input id="outside">
            <iframe id="frame"></iframe>
            <div id="host"></div>
        </body></html>"#,
    );
    vm.eval(
        r#"
(() => {
  const frame = document.getElementById('frame');
  const childDocument = frame.contentDocument;
  childDocument.open();
  childDocument.write('<body><p>slide</p></body>');
  childDocument.close();
  const name = target => target.id || target.localName;
  window.__keys = [];
  window.addEventListener('keydown', e => __keys.push(`top:${e.type}:${name(e.target)}`), true);
  window.addEventListener('keyup', e => __keys.push(`top:${e.type}:${name(e.target)}`), true);
  childDocument.addEventListener('keydown', e => __keys.push(`child:${e.type}:${name(e.target)}`), true);
  childDocument.addEventListener('keyup', e => __keys.push(`child:${e.type}:${name(e.target)}`), true);
  // Ask the child Document from its own realm, as frame content does.
  const childHasFocus = frame.contentWindow.Function('return document.hasFocus()');
  window.__focusState = () => [
    document.activeElement.id,
    childDocument.activeElement.localName,
    document.hasFocus(),
    childHasFocus(),
  ].join(',');
  // A frame inside a shadow tree: the parent Document's activeElement is
  // the shadow host, not the frame.
  const shadowFrame = document.createElement('iframe');
  document.getElementById('host').attachShadow({ mode: 'open' }).append(shadowFrame);
  const shadowDocument = shadowFrame.contentDocument;
  shadowDocument.open();
  shadowDocument.write('<body><p>shadow</p></body>');
  shadowDocument.close();
  shadowDocument.addEventListener('keydown', e => __keys.push(`shadow:${e.type}:${name(e.target)}`), true);
  shadowDocument.addEventListener('keyup', e => __keys.push(`shadow:${e.type}:${name(e.target)}`), true);
  const shadowHasFocus = shadowFrame.contentWindow.Function('return document.hasFocus()');
  window.__focusShadowFrame = () => {
    shadowFrame.focus();
    return [document.activeElement.id, shadowHasFocus(), childHasFocus()].join(',');
  };
  // A classic iframe editor: the focused frame's body is the editing host.
  window.__focusEditableFrame = () => {
    childDocument.body.contentEditable = 'true';
    childDocument.body.innerHTML = '<b>x</b>';
    frame.focus();
  };
  window.__editableFrameKeptMarkup = () =>
    childDocument.body.querySelector('b') !== null;
  frame.focus();
})()
"#,
    )
    .expect("focused frame fixture should initialize");

    let press = |vm: &mut StandaloneScriptVmHarness| {
        vm.dispatch_key_event("keydown", "ArrowRight", "ArrowRight", "", 1, false, false)
            .expect("ArrowRight keydown should dispatch");
        vm.dispatch_key_event("keyup", "ArrowRight", "ArrowRight", "", 1, false, false)
            .expect("ArrowRight keyup should dispatch");
        vm.eval("__keys.splice(0).join('|')")
            .expect("key log should evaluate")
    };

    let frame_state = vm.eval("__focusState()").expect("frame focus state");
    let frame_keys = press(&mut vm);
    vm.eval("document.getElementById('outside').focus()")
        .expect("outside input should focus");
    let outside_state = vm.eval("__focusState()").expect("outside focus state");
    let outside_keys = press(&mut vm);
    let shadow_state = vm
        .eval("__focusShadowFrame()")
        .expect("shadow frame focus state");
    let shadow_keys = press(&mut vm);
    vm.eval("__focusEditableFrame()")
        .expect("editable frame should focus");
    vm.dispatch_key_event("keydown", "y", "KeyY", "y", 0, false, true)
        .expect("text keydown should dispatch");
    vm.dispatch_key_event("keyup", "y", "KeyY", "", 0, false, false)
        .expect("text keyup should dispatch");
    let editable_keys = vm
        .eval("__keys.splice(0).filter(k => k.startsWith('child:')).join('|')")
        .expect("editable key log should evaluate");
    let editable_markup = vm
        .eval("String(__editableFrameKeptMarkup())")
        .expect("editable markup should evaluate");

    assert_eq!(
        [
            frame_state,
            frame_keys,
            outside_state,
            outside_keys,
            shadow_state,
            shadow_keys,
            editable_keys,
            editable_markup
        ]
        .join(" / "),
        "frame,body,true,true / child:keydown:body|child:keyup:body / \
         outside,body,true,false / top:keydown:outside|top:keyup:outside / \
         host,true,false / shadow:keydown:body|shadow:keyup:body / \
         child:keydown:body|child:keyup:body / true"
    );
}

fn keyboard_scroll_vm() -> StandaloneScriptVmHarness {
    new_rendered_test_vm(
        "https://keyboard-scroll.test/",
        r#"<html><body style="margin:0">
            <div id="scroller" tabindex="0" style="overflow:auto;width:200px;height:100px">
              <div style="height:1000px;width:150px"></div>
            </div>
            <div style="height:3000px"></div>
        </body></html>"#,
    )
}

fn press_scroll_key(vm: &mut StandaloneScriptVmHarness, key: &str) -> String {
    vm.dispatch_key_event("keydown", key, key, "", 0, false, false)
        .expect("scroll keydown should dispatch");
    vm.dispatch_key_event("keyup", key, key, "", 0, false, false)
        .expect("scroll keyup should dispatch");
    vm.eval("`${document.getElementById('scroller').scrollTop}:${scrollY}`")
        .expect("scroll offsets should evaluate")
}

#[test]
fn scrolling_keys_scroll_the_focused_scroller_by_line_page_and_document_steps() {
    let mut vm = keyboard_scroll_vm();
    vm.eval("document.getElementById('scroller').focus()")
        .expect("scroller should focus");
    let offsets = ["PageDown", "ArrowDown", "End", "ArrowDown", "Home"]
        .map(|key| press_scroll_key(&mut vm, key));

    // Chromium 156: a 40px line, floor(87.5%) of the 100px scrollport per
    // page, and a focused scroller at its end does not scroll the viewport.
    assert_eq!(offsets.join("|"), "87:0|127:0|900:0|900:0|0:0");
}

#[test]
fn canceled_scrolling_keydown_does_not_scroll() {
    let mut vm = keyboard_scroll_vm();
    vm.eval(
        r#"(() => {
  const scroller = document.getElementById('scroller');
  scroller.addEventListener('keydown', event => event.preventDefault());
  scroller.focus();
})()"#,
    )
    .expect("canceling scroller should focus");

    assert_eq!(press_scroll_key(&mut vm, "PageDown"), "0:0");
}

#[test]
fn range_keys_step_the_focused_slider_instead_of_scrolling() {
    let mut vm = new_rendered_test_vm(
        "https://range-keyboard.test/",
        r#"<html><body style="margin:0">
            <input id="range" type="range" min="0" max="360" value="180">
            <div style="height:3000px"></div>
        </body></html>"#,
    );
    vm.eval(
        r#"(() => {
  window.log = [];
  const range = document.getElementById('range');
  for (const type of ['input', 'change']) {
    range.addEventListener(type, () => log.push(`${type}:${range.value}`));
  }
  range.focus();
})()"#,
    )
    .expect("range should focus");
    for key in ["ArrowRight", "End", "ArrowDown", "PageDown"] {
        vm.dispatch_key_event("keydown", key, key, "", 0, false, false)
            .expect("range keydown should dispatch");
        vm.dispatch_key_event("keyup", key, key, "", 0, false, false)
            .expect("range keyup should dispatch");
    }

    // Chromium 156: one step per arrow, End jumps to max, PageDown moves a
    // tenth of the range, each change fires input then change, and the page
    // does not scroll.
    assert_eq!(
        vm.eval("`${log.join('|')} ${scrollY}`")
            .expect("range log should evaluate"),
        "input:181|change:181|input:360|change:360|input:359|change:359|input:323|change:323 0"
    );
}
