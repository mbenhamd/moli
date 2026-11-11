use super::*;

#[tokio::test]
async fn pointer_capture_arguments_use_native_receiver_and_webidl_conversion() {
    let loader = ResourceRequestClient::new(&moli_fetch::FetchConfig::default()).unwrap();
    let mut vm = new_storage_page_task_executor_test_vm_with_loader(
        "https://pointer-capture-arguments.test/",
        &loader,
    );
    vm.eval(
        r#"
        if (!document.documentElement) document.appendChild(document.createElement('html'));
        if (!document.body) document.documentElement.appendChild(document.createElement('body'));
        const frame = document.body.appendChild(document.createElement('iframe'));
        frame.id = 'child'; frame.srcdoc = '<head></head><body></body>'; void frame.contentWindow;
        globalThis.nativeCaptureReceiver = document.implementation.createHTMLDocument('').createElement('select');
        'ready'
    "#,
    )
    .unwrap();
    assert!(
        vm.run_one_child_frame_task_executor_turn(
            ChildFrameSemanticTurnKind::RealmMaterialization,
            &loader
        )
        .await
        .unwrap()
    );
    vm.drain_ready_page_task_executor_turns_for_setup(&loader, 128)
        .await
        .unwrap();
    vm.with_default_context_scope_and_checkpoint_for_test(|scope, _host_ptr| {
        let global = scope.get_current_context().global(scope);
        let key = v8::String::new(scope, "nativeCaptureReceiver").unwrap();
        let value = global.get(scope, key.into()).unwrap();
        assert!(value.is_proxy(), "fixture must exercise a native Proxy");
        let object = v8::Local::<v8::Object>::try_from(value).unwrap();
        assert!(crate::web_api_interfaces::Element::is_instance(
            scope, object
        ));
        Ok(())
    })
    .unwrap();
    let source = format!(
        "{}; globalThis.__captureResults = __pointerCaptureProbe([globalThis, document.getElementById('child').contentWindow]); __captureResults.complete",
        include_str!("pointer_capture.js"),
    );
    assert_eq!(vm.eval(&source).unwrap(), "true", "{}",
        vm.eval("JSON.stringify({errors:__captureResults.errors,failures:__captureResults.rows.filter(row=>Object.values(row.checks).some(value=>value!==true))})").unwrap());
}

#[test]
fn pointer_capture_queries_pending_target_before_native_dispatch() {
    let mut vm = new_rendered_test_vm(
        "https://pointer-capture-pending-query.test/",
        r#"<html><body><div id="first" style="position:absolute;left:40px;top:40px;width:120px;height:120px">first</div><div id="second" style="position:absolute;left:220px;top:40px;width:120px;height:120px">second</div></body></html>"#,
    );
    let source = format!(
        "{}; globalThis.__captureResults = __installPointerCaptureStateProbe(document.getElementById('first'), document.getElementById('second')); 'ready'",
        include_str!("pointer_capture.js"),
    );
    vm.eval(&source).unwrap();
    vm.publish_layout_for_test().unwrap();
    for (kind, x, button) in [
        ("mousedown", 80.0, 0),
        ("mousemove", 82.0, -1),
        ("mousemove", 84.0, -1),
        ("mousemove", 86.0, -1),
        ("mouseup", 86.0, 0),
    ] {
        vm.dispatch_mouse_event_at_point(x, 80.0, kind, button, None, 0.0, 0.0)
            .unwrap();
    }
    assert_eq!(vm.eval("__captureResults.errors.length === 0 && __captureResults.rows.every(row=>Object.values(row.checks).every(value=>value===true))").unwrap(), "true", "{}", vm.eval("JSON.stringify(__captureResults)").unwrap());
    assert_eq!(
        vm.eval("__captureResults.events.join('|')").unwrap(),
        "pointerdown@first|gotpointercapture@first|pointermove@first|lostpointercapture@first|gotpointercapture@second|pointermove@second|lostpointercapture@second|pointermove@first|pointerup@first"
    );
}

#[test]
fn pointer_capture_accepts_child_frame_nodes_with_detached_identity() {
    // A child browsing context's Document is not the root Document, but it
    // has a browsing context. Nodes in it that went through an inert
    // Document, or were returned from a detached receiver, carry a detached
    // native handle and must still accept pointer capture.
    for variant in ["plain", "inert-roundtrip", "detached-closest"] {
        let mut vm = new_storage_html_test_vm("https://pointer-capture-child-frame.test/");
        vm.eval(&format!(
            r#"
              document.body.style.margin = '0';
              const frame = document.createElement('iframe');
              frame.style.cssText = 'position:fixed;left:0;top:0;width:240px;height:220px;border:0';
              document.body.appendChild(frame);
              const doc = frame.contentWindow.document;
              doc.body.style.margin = '0';
              let node = doc.createElement('div');
              node.style.cssText = 'position:fixed;left:50px;top:50px;width:100px;height:100px';
              doc.body.appendChild(node);
              const inert = doc.implementation.createHTMLDocument('');
              if ('{variant}' === 'inert-roundtrip') {{
                inert.body.appendChild(node);
                doc.body.appendChild(node);
              }}
              if ('{variant}' === 'detached-closest') {{
                const inner = inert.createElement('span');
                inert.body.appendChild(inner);
                node.appendChild(inner);
                node = inner.closest('div');
              }}
              globalThis.__childCaptureLog = [];
              for (const type of ['pointerdown', 'gotpointercapture']) {{
                node.addEventListener(type, event => {{
                  __childCaptureLog.push(type);
                  if (type === 'pointerdown') {{
                    node.setPointerCapture(event.pointerId);
                    __childCaptureLog.push(`has:${{node.hasPointerCapture(event.pointerId)}}`);
                  }}
                }});
              }}
              'ready'
            "#
        ))
        .unwrap();
        vm.publish_layout_for_test().unwrap();
        vm.dispatch_mouse_event_at_point(80.0, 80.0, "mousedown", 0, None, 0.0, 0.0)
            .unwrap();
        vm.dispatch_mouse_event_at_point(200.0, 80.0, "mousemove", -1, None, 0.0, 0.0)
            .unwrap();
        assert_eq!(
            vm.eval("__childCaptureLog.join('|')").unwrap(),
            "pointerdown|has:true|gotpointercapture",
            "{variant}"
        );
    }
}
