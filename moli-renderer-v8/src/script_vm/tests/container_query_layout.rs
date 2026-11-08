//! CSS container queries answered from used layout sizes (PF-7851).
//!
//! Each test uses stylesheet-only container sizes: the store, not a `style=`
//! attribute, must supply them.

use moli_page_types::LayoutPolicy;

use super::*;

fn live_vm(markup: &str) -> StandaloneScriptVmHarness {
    let mut vm = new_parsed_test_vm("https://container-queries.test/", markup);
    vm.set_layout_policy(LayoutPolicy::Live);
    vm
}

fn iterations(vm: &StandaloneScriptVmHarness) -> Option<usize> {
    vm._context_host
        .borrow()
        .layout_pass_observability_for_test()
        .3
        .map(|metrics| metrics.container_query_iteration_count)
}

fn passes(vm: &StandaloneScriptVmHarness) -> u64 {
    vm._context_host
        .borrow()
        .layout_pass_observability_for_test()
        .1
}

const STYLES: &str = r#"
<style>
  #outer { container-type: inline-size; width: 500px; }
  #inner { container: named / size; width: 200px; height: 100px; }
  #innermost { container-type: inline-size; width: 80px; }
  .color { color: rgb(0, 0, 0); }
  @container (max-width: 600px) { #direct { color: rgb(1, 2, 3); } }
  @container (min-width: 600px) { #direct { color: rgb(9, 9, 9); } }
  #direct { display: block; height: 10px; }
  @container (max-width: 600px) { #direct { height: 20px; } }
  /* Skips #innermost, whose width alone would match. */
  @container named (min-width: 150px) { #nested { color: rgb(4, 5, 6); } }
  @container (max-width: 100px) { #nearest { color: rgb(7, 8, 9); } }
  /* inline-size containers answer no block-axis query; #inner does. */
  @container (min-height: 50px) { #height { color: rgb(10, 20, 30); } }
  #units { width: 50cqw; height: 25cqh; }
  @container (max-width: 600px) { .gone { display: none; } }
  #toggle { width: 100px; }
  #toggle.wide { width: 700px; }
  @container (min-width: 650px) { #dep { color: rgb(70, 70, 70); } }
</style>
<div id="outer">
  <div id="direct" class="color">direct</div>
  <div id="inner">
    <div id="innermost">
      <div id="nested" class="color">n</div>
      <div id="nearest" class="color">n</div>
    </div>
    <div id="height" class="color">h</div>
    <div id="units">u</div>
  </div>
</div>
<div id="toggle" style="container-type:inline-size"><div id="dep">d</div></div>
<div id="hidden-host" style="container-type:inline-size;display:none"><div id="in-hidden">x</div></div>
"#;

#[test]
fn stylesheet_container_width_answers_immediate_computed_style_and_box_height() {
    let mut vm = live_vm(STYLES);
    let result = vm
        .eval(
            "(() => { const d = document.getElementById('direct'); return [getComputedStyle(d).color, d.getBoundingClientRect().height].join('|'); })()",
        )
        .expect("eval");
    assert_eq!(result, "rgb(1, 2, 3)|20");
}

#[test]
fn nested_named_size_and_units_resolve_from_used_sizes() {
    let mut vm = live_vm(STYLES);
    let result = vm
        .eval(
            r#"(() => {
              const value = (id, prop) => getComputedStyle(document.getElementById(id))[prop];
              return [value('nested','color'), value('nearest','color'), value('height','color'), value('units','width'), value('units','height')].join('|');
            })()"#,
        )
        .expect("eval");
    assert_eq!(
        result,
        "rgb(4, 5, 6)|rgb(7, 8, 9)|rgb(10, 20, 30)|100px|25px"
    );
}

#[test]
fn display_none_container_answers_no_size() {
    let mut vm = live_vm(
        r#"<style>
          @container (min-width: 0px) { #inner { color: rgb(1, 1, 1); } }
          #inner { color: rgb(2, 2, 2); }
        </style>
        <div id="host" style="container-type:inline-size;display:none;width:300px"><div id="inner">x</div></div>"#,
    );
    let result = vm
        .eval("getComputedStyle(document.getElementById('inner')).color")
        .expect("eval");
    assert_eq!(result, "rgb(2, 2, 2)");
}

#[test]
fn resizing_a_container_restyles_descendants_and_settles_without_extra_passes() {
    let mut vm = live_vm(STYLES);
    let dep = "getComputedStyle(document.getElementById('dep')).color";
    assert_eq!(vm.eval(dep).expect("eval"), "rgb(0, 0, 0)");
    let result = vm
        .eval(
            r#"(() => {
              const toggle = document.getElementById('toggle');
              toggle.classList.add('wide');
              return getComputedStyle(document.getElementById('dep')).color;
            })()"#,
        )
        .expect("eval");
    assert_eq!(result, "rgb(70, 70, 70)");

    let settled = passes(&vm);
    assert_eq!(vm.eval(dep).expect("eval"), "rgb(70, 70, 70)");
    assert_eq!(
        passes(&vm),
        settled,
        "an unchanged document keeps the published layout"
    );
}

#[test]
fn class_toggle_on_a_descendant_adds_no_container_iteration() {
    let mut vm = live_vm(STYLES);
    vm.eval("document.getElementById('direct').getBoundingClientRect().height")
        .expect("eval");
    let result = vm
        .eval(
            r#"(() => {
              const dep = document.getElementById('dep');
              dep.classList.add('marker');
              return dep.getBoundingClientRect().height;
            })()"#,
        )
        .expect("eval");
    assert_ne!(result, "0");
    assert_eq!(
        iterations(&vm),
        Some(1),
        "unchanged container sizes take one iteration"
    );
}

#[test]
fn print_pass_does_not_poison_screen_container_sizes() {
    let mut vm = live_vm(
        r#"<style>
          #c { container-type: inline-size; width: 500px; }
          @media print { #c { width: 100px; } }
          @container (max-width: 200px) { #t { color: rgb(1, 1, 1); } }
          #t { color: rgb(2, 2, 2); }
        </style><div id="c"><div id="t">x</div></div>"#,
    );
    let read = "getComputedStyle(document.getElementById('t')).color";
    assert_eq!(vm.eval(read).expect("eval"), "rgb(2, 2, 2)");
    {
        let host = vm._context_host.borrow();
        let environment = crate::style_engine::StyloStyleEnvironment::from_emulated_media(
            &crate::protocol_types::EmulatedMediaOverrides {
                media: Some("print".to_owned()),
                ..Default::default()
            },
        );
        let document = host.document_handle();
        host.build_layout_pass_for_document_in_environment(
            document,
            moli_layout::LayoutPassRequest::new(
                moli_layout::LayoutViewport::new(800, 600, 1.0),
                moli_layout::LayoutFlushReason::Test,
            ),
            Some(environment),
        )
        .expect("print layout")
        .expect("root");
    }
    assert_eq!(vm.eval(read).expect("eval"), "rgb(2, 2, 2)");
}

#[test]
fn an_attribute_no_selector_reads_needs_no_pass_but_a_size_change_does() {
    let mut vm = live_vm(STYLES);
    vm.eval("getComputedStyle(document.getElementById('dep')).color")
        .expect("eval");
    let before = passes(&vm);
    let result = vm
        .eval(
            r#"(() => {
              const dep = document.getElementById('dep');
              for (let i = 0; i < 5; i++) { dep.setAttribute('data-x', String(i)); getComputedStyle(dep).color; }
              return getComputedStyle(dep).color;
            })()"#,
        )
        .expect("eval");
    assert_eq!(result, "rgb(0, 0, 0)");
    assert_eq!(passes(&vm), before);
    let grown = vm
        .eval(
            r#"(() => {
              document.getElementById('toggle').classList.add('wide');
              return getComputedStyle(document.getElementById('dep')).color;
            })()"#,
        )
        .expect("eval");
    assert_eq!(grown, "rgb(70, 70, 70)");
}

#[test]
fn slotted_content_queries_and_follows_its_shadow_tree_container() {
    let mut vm = live_vm(
        r#"<style>
          @container (min-width: 300px) { #slotted { color: rgb(1, 1, 1); } }
          #slotted { color: rgb(0, 0, 0); }
        </style>
        <div id="host"><span id="slotted">s</span></div>"#,
    );
    let result = vm
        .eval(
            r#"(() => {
              const root = document.getElementById('host').attachShadow({ mode: 'open' });
              root.innerHTML = '<div id="in" style="container-type:inline-size;width:100px"><slot></slot></div>';
              const slotted = document.getElementById('slotted');
              const small = getComputedStyle(slotted).color;
              root.getElementById('in').style.width = '400px';
              document.body.getBoundingClientRect();
              return small + '|' + getComputedStyle(slotted).color;
            })()"#,
        )
        .expect("eval");
    assert_eq!(result, "rgb(0, 0, 0)|rgb(1, 1, 1)");
}

#[test]
fn a_pseudo_element_read_flushes_container_sizes() {
    let mut vm = live_vm(
        r#"<style>
          #c { container-type: inline-size; width: 100px; }
          #k::before { content: ""; display: block; width: 50cqw; height: 1px; }
          #k::after { content: ""; display: block; width: 20cqi; height: 1px; }
        </style>
        <div id="c"><div id="k" style="display:inline-block"></div></div>"#,
    );
    let result = vm
        .eval(
            r#"(() => {
              const k = document.getElementById('k');
              const first = getComputedStyle(k, '::before').width;
              document.getElementById('c').style.width = '400px';
              return [first, getComputedStyle(k, '::before').width, getComputedStyle(k, '::after').width].join('|');
            })()"#,
        )
        .expect("eval");
    assert_eq!(result, "50px|200px|80px");
}
