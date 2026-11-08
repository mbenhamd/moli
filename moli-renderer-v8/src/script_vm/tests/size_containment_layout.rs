//! Size containment on real elements, with expected values measured in
//! Chromium (PF-7851).

use moli_page_types::LayoutPolicy;

use super::*;

fn measure(markup: &str, script: &str) -> String {
    let mut vm = new_parsed_test_vm("https://size-containment.test/", markup);
    vm.set_layout_policy(LayoutPolicy::Live);
    vm.eval(script).expect("eval")
}

const HEIGHT: &str = "String(document.getElementById('c').getBoundingClientRect().height)";

#[test]
fn a_query_container_keeps_child_margins_and_floats_inside() {
    let margin = measure(
        r#"<style>body{margin:0}p{margin:30px 0;height:18px}</style>
        <div id="c" style="container-type:inline-size"><p></p></div>"#,
        HEIGHT,
    );
    assert_eq!(margin, "78");
    let float = measure(
        r#"<style>body{margin:0}</style>
        <div id="c" style="container-type:inline-size"><div style="float:left;width:10px;height:50px"></div></div>"#,
        HEIGHT,
    );
    assert_eq!(float, "50");
}

#[test]
fn native_text_controls_keep_their_control_size_under_size_containment() {
    let result = measure(
        r#"<input id="plain"><input id="contained" style="contain:size">
        <textarea id="plain-area"></textarea><textarea id="contained-area" style="contain:size"></textarea>"#,
        r#"(() => {
          const size = (id) => { const r = document.getElementById(id).getBoundingClientRect(); return r.width + 'x' + r.height; };
          return [size('plain') === size('contained'), size('plain-area') === size('contained-area')].join('|');
        })()"#,
    );
    assert_eq!(result, "true|true");
}

#[test]
fn a_size_contained_image_drops_its_natural_ratio_but_keeps_an_explicit_one() {
    let svg =
        "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='200' height='100'/%3E";
    let result = measure(
        &format!(
            r#"<img id="natural" src="{svg}" style="contain:size;width:100px;display:block">
            <img id="explicit" src="{svg}" style="contain:size;width:100px;aspect-ratio:4/1;display:block">"#
        ),
        r#"['natural','explicit'].map((id) => { const r = document.getElementById(id).getBoundingClientRect(); return r.width + 'x' + r.height; }).join('|')"#,
    );
    assert_eq!(result, "100x0|100x25");
}
