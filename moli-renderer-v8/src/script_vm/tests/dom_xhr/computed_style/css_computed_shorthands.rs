use super::*;

#[test]
fn css_computed_shorthands_serialize_from_computed_longhands() {
    let mut vm = new_parsed_test_vm("https://computed-shorthands.test/", "<!doctype html><body>");
    assert_eq!(
        vm.eval(include_str!("css_computed_shorthands.js")).unwrap(),
        "true"
    );
}
