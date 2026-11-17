//! Path2D painting, SVG CTM queries, logical inline margins and the
//! `webkitdirectory` reflection, with expected values measured in Chromium
//! 156 (PF-9187).

use moli_page_types::LayoutPolicy;

use super::*;

fn evaluate(markup: &str, script: &str) -> String {
    let mut vm = new_parsed_test_vm("https://chromium-web-api-parity.test/", markup);
    vm.set_layout_policy(LayoutPolicy::Live);
    vm.eval(script).expect("eval")
}

#[test]
fn path2d_fills_by_rule_and_bounds_huge_arcs_but_rejects_unknown_rules() {
    let result = evaluate(
        "<canvas id=c width=40 height=40></canvas>",
        r#"(() => {
          const ctx = document.getElementById('c').getContext('2d');
          const path = new Path2D('M0 0H40V40H0Z M10 10H30V30H10Z');
          ctx.fill(path, 'evenodd');
          const alpha = (x, y) => ctx.getImageData(x, y, 1, 1).data[3];
          const huge = new Path2D('M0 0 A1e60 1e60 0 1 1 1e50 0');
          ctx.stroke(huge);
          let error = 'none';
          try { ctx.fill(path, 'bogus'); } catch (caught) { error = caught.name; }
          return [alpha(5, 5), alpha(20, 20), error].join('|');
        })()"#,
    );
    assert_eq!(result, "255|0|TypeError");
}

#[test]
fn svg_ctm_maps_the_untransformed_viewport_and_unrendered_elements_report_identity() {
    let result = evaluate(
        r#"<style>body{margin:0}</style>
        <svg width=200 height=100 viewBox="0 0 400 100"><rect id=a width=1 height=1/></svg>
        <svg width=100 height=100 viewBox="0 0 50 200" preserveAspectRatio="xMaxYMax slice"
          style="transform:scale(2)"><rect id=b width=1 height=1/></svg>
        <svg width=100 height=100 style="display:none"><rect id=c width=1 height=1/></svg>"#,
        r#"(() => {
          const f = (m) => m ? [m.a, m.b, m.c, m.d, m.e, m.f].join(',') : String(m);
          const ctm = (e) => f(e.getCTM()) + ' ' + f(e.getScreenCTM());
          const detached = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
          return [f(document.getElementById('a').getCTM()), f(document.getElementById('b').getCTM()),
            ctm(document.getElementById('c')), ctm(detached)].join('|');
        })()"#,
    );
    assert_eq!(
        result,
        "0.5,0,0,0.5,0,25|2,0,0,2,0,-300|1,0,0,1,0,0 1,0,0,1,0,0|1,0,0,1,0,0 1,0,0,1,0,0"
    );
}

#[test]
fn inline_margins_follow_direction_and_stay_auto_without_a_box() {
    let result = evaluate(
        r#"<style>body{margin:0}</style>
        <div style="width:600px;direction:rtl"><div id=rtl style="width:200px;height:10px;margin-inline-start:10px;margin-inline-end:auto"></div></div>
        <div style="width:600px"><div id=none style="width:200px;margin-inline:auto;display:none"></div></div>"#,
        r#"['rtl', 'none'].map((id) => {
          const style = getComputedStyle(document.getElementById(id));
          return [style.marginInlineStart, style.marginInlineEnd, style.marginLeft].join(' ');
        }).join('|')"#,
    );
    assert_eq!(result, "10px 390px 390px|auto auto auto");
}

#[test]
fn webkitdirectory_reflects_its_attribute_and_checks_the_receiver() {
    let result = evaluate(
        "<input id=f type=file>",
        r#"(() => {
          const input = document.getElementById('f');
          input.webkitdirectory = true;
          const set = input.getAttribute('webkitdirectory') + '/' + input.webkitdirectory;
          input.webkitdirectory = false;
          const removed = input.hasAttribute('webkitdirectory') + '/' + input.webkitdirectory;
          let error = 'none';
          const getter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'webkitdirectory').get;
          try { getter.call(document.body); } catch (caught) { error = caught.name; }
          return [set, removed, error].join('|');
        })()"#,
    );
    assert_eq!(result, "/true|false/false|TypeError");
}
