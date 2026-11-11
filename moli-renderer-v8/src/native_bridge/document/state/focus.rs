use super::*;

pub(in crate::native_bridge) fn node_document_active_element_getter_function<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Ok((runtime_ptr, handle)) =
        node_runtime_and_handle_from_object_or_detached(scope, args.this())
    else {
        rv.set_null();
        return;
    };
    if !node_is_document(unsafe { &*runtime_ptr }, handle) {
        rv.set_null();
        return;
    }
    let runtime = unsafe { &mut *runtime_ptr };
    let handle = runtime
        .active_element_handle()
        .and_then(|active| retarget_active_element_to_document(runtime, active, handle))
        .or_else(|| {
            runtime
                .dom_host()
                .document_body_handle_for_document(handle)
                .or_else(|| {
                    runtime
                        .dom_host()
                        .document_element_handle_for_document(handle)
                })
        });
    set_wrapped_handle_or_null_for_receiver(scope, &mut rv, runtime_ptr, args.this(), handle);
}

/// Whether `document` is in the focus chain of the focused area, the part of
/// HTML's "has focus steps" that a same-runtime frame tree decides: the
/// focused element lives in `document` or in a Document nested under one of
/// its navigable containers, or the focused element is the container of
/// `document` with nothing focused inside, which makes `document`'s viewport
/// the focused area. With nothing focused only the runtime's top Document
/// (the viewport) is focused.
pub(crate) fn document_is_in_focus_chain(runtime: &JsContextHost, document: DomHandle) -> bool {
    match runtime.active_element_handle() {
        Some(active) => {
            runtime.child_browsing_context_document_handle(active) == Some(document)
                || retarget_active_element_to_document(runtime, active, document).is_some()
        }
        None => document == runtime.dom_host().document_handle(),
    }
}

fn retarget_active_element_to_document(
    runtime: &JsContextHost,
    active: DomHandle,
    target_document: DomHandle,
) -> Option<DomHandle> {
    let mut current = active;
    loop {
        while let Some(root) = runtime.dom_host().containing_shadow_root(current) {
            current = runtime.dom_host().shadow_root_host(root)?;
        }
        let owner_document = runtime.dom_host().owner_document_handle(current)?;
        if owner_document == target_document {
            return Some(current);
        }
        current = runtime.child_browsing_context_host_for_document_handle(owner_document)?;
    }
}
