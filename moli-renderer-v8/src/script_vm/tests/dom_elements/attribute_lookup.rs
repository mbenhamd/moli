use super::*;

#[test]
fn attribute_node_lookup_uses_native_metadata_and_namespace_identity() {
    let mut vm = new_storage_test_vm("https://attribute-lookup.test/");
    assert_eq!(
        vm.eval(
            r#"(() => {
                const failures = [];
                const check = (value, label) => { if (!value) failures.push(label); };
                let reads = 0;
                const documents = [document, document.implementation.createHTMLDocument(''),
                    new DOMParser().parseFromString('<root/>', 'application/xml')];
                for (const [index, doc] of documents.entries()) {
                    const element = doc.createElement('section');
                    element.setAttribute('data-value', 'first');
                    element.setAttributeNS('urn:one', 'p:value', 'one');
                    element.setAttributeNS('urn:two', 'p:value', 'two');
                    for (const name of ['getAttribute', 'getAttributeNS']) {
                        Object.defineProperty(element, name, {configurable: true, get() {
                            ++reads;
                            throw new Error('author attribute getter');
                        }});
                    }
                    const attr = element.getAttributeNode('data-value');
                    check(attr?.ownerElement === element, index + '/owner');
                    check(attr === element.attributes.getNamedItem('data-value'), index + '/named-map');
                    check(attr === element.getAttributeNodeNS(null, 'data-value'), index + '/null-namespace');
                    check(element.getAttributeNode('missing') === null, index + '/missing');
                    check(element.getAttributeNodeNS('urn:missing', 'value') === null, index + '/missing-ns');
                    if (index < 2) {
                        check(element.getAttributeNode('DATA-VALUE') === attr, index + '/html-case');
                    } else {
                        check(element.getAttributeNode('DATA-VALUE') === null, index + '/xml-case');
                    }
                    const one = element.getAttributeNodeNS('urn:one', 'value');
                    const two = element.getAttributeNodeNS('urn:two', 'value');
                    check(one !== two && one.namespaceURI === 'urn:one' && two.namespaceURI === 'urn:two', index + '/ns-identity');
                    check(one === element.attributes.getNamedItemNS('urn:one', 'value'), index + '/map-one');
                    check(two === element.attributes.getNamedItemNS('urn:two', 'value'), index + '/map-two');
                    check(element.getAttributeNode('p:value') === one, index + '/first-qualified');
                    check(reads === 0, index + '/lookup-getters');
                    // Poison lookup entry points only; value/mutation fallbacks
                    // are separate paths from attribute-node lookup.
                    delete element.getAttribute;
                    delete element.getAttributeNS;
                    check(attr.value === 'first', index + '/value');
                    check(one.value === 'one' && two.value === 'two', index + '/ns-values');
                    element.setAttribute('data-value', 'second');
                    check(element.getAttributeNode('data-value') === attr && attr.value === 'second', index + '/update');
                    element.removeAttributeNS('urn:one', 'value');
                    check(one.ownerElement === null, index + '/detached-owner');
                    check(element.getAttributeNode('p:value') === two, index + '/remaining-qualified');
                    check(element.getAttributeNodeNS('urn:two', 'value') === two, index + '/retained-ns');
                }
                check(reads === 0, 'public getters not invoked');
                return JSON.stringify(failures);
            })()"#,
        )
        .unwrap(),
        "[]",
    );
}

#[test]
fn named_node_map_access_materializes_only_the_attrs_it_reads() {
    use crate::native_bridge::document::take_named_node_map_attr_materializations_for_test;
    let mut vm = new_storage_html_test_vm("https://attribute-map-cost.test/");
    vm.eval(
        r#"window.element = document.createElement('div');
        for (let index = 0; index < 20; index++) element.setAttribute('data-a' + index, String(index));
        document.body.appendChild(element);
        'ready'"#,
    )
    .expect("element setup");
    take_named_node_map_attr_materializations_for_test();
    assert_eq!(
        vm.eval("let total = 0; for (let i = 0; i < 10; i++) total += element.attributes.length; String(total)")
            .expect("length reads"),
        "200"
    );
    assert_eq!(
        take_named_node_map_attr_materializations_for_test(),
        0,
        "reading the map length must not build Attr objects"
    );
    assert_eq!(
        vm.eval(
            r#"const seen = []; for (const attr of element.attributes) seen.push(attr.name + '=' + attr.value);
            const map = element.attributes;
            element.setAttribute('data-a3', 'changed');
            JSON.stringify([seen.length, seen[3], map === element.attributes, map[3].value, map.length])"#
        )
        .expect("iteration"),
        r#"[20,"data-a3=3",true,"changed",20]"#
    );
    let iteration = take_named_node_map_attr_materializations_for_test();
    assert!(
        iteration <= 21,
        "iteration reads each Attr once and the indexed read one more, not {iteration}"
    );
}
