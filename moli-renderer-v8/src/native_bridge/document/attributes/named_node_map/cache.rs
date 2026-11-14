use super::*;
use crate::util::{get_private_object, get_private_value, set_private_value};

const NAMED_NODE_MAP_CACHE_SLOT: &str = "__moliNamedNodeMapCache";
pub(super) const NAMED_NODE_MAP_ELEMENT_SLOT: &str = "__moliNamedNodeMapElement";

fn live_named_node_map_cache<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    element: v8::Local<'s, v8::Object>,
) -> Option<v8::Local<'s, v8::Object>> {
    let element = crate::native_bridge::node::node_backing_object(scope, element)?;
    get_private_value(scope, element, NAMED_NODE_MAP_CACHE_SLOT)
        .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())
}

fn set_live_named_node_map_cache<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    element: v8::Local<'s, v8::Object>,
    wrapper: v8::Local<'s, v8::Object>,
) {
    if let Some(backing) = crate::native_bridge::node::node_backing_object(scope, element) {
        set_private_value(scope, backing, NAMED_NODE_MAP_CACHE_SLOT, wrapper.into());
    }
}

pub(crate) fn live_named_node_map_wrapper<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    runtime_ptr: *mut PageScriptHost,
    element: v8::Local<'s, v8::Object>,
) -> v8::Local<'s, v8::Object> {
    let element = crate::native_bridge::document::canonical_detached_node_object(scope, element)
        .expect("validated attributes receiver must have a backing");
    if let Some(wrapper) = live_named_node_map_cache(scope, element) {
        // The map is live: its interceptors read the element's current
        // attributes on every access, so a cached wrapper only needs to
        // follow its canonical owner.
        let owner_is_current = get_private_object(scope, wrapper, NAMED_NODE_MAP_ELEMENT_SLOT)
            .is_some_and(|owner| owner.strict_equals(element.into()));
        if !owner_is_current {
            set_private_value(scope, wrapper, NAMED_NODE_MAP_ELEMENT_SLOT, element.into());
        }
        return wrapper;
    }
    // The attributes getter already resolved this host from the receiver.
    // The public bridge can be absent while that host is still installed.
    let template = unsafe { &mut *runtime_ptr }
        .native_bridge_mut()
        .named_node_map_wrapper_template();
    let wrapper = template
        .new_instance(scope)
        .expect("failed to instantiate NamedNodeMap wrapper");
    assert!(
        wrapper.set_internal_field(0, element.into()),
        "NamedNodeMap wrapper must expose its owner element field"
    );
    web_api_interfaces::NamedNodeMap::DESCRIPTOR
        .initialize(scope, wrapper)
        .expect("NamedNodeMap wrapper must have native identity");
    super::super::super::super::bindings::set_named_constructor_prototype(
        scope,
        wrapper,
        "NamedNodeMap",
    );
    set_private_value(scope, wrapper, NAMED_NODE_MAP_ELEMENT_SLOT, element.into());
    set_live_named_node_map_cache(scope, element, wrapper);
    wrapper
}
