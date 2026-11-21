use super::*;
use crate::{
    context_bootstrap::{
        CHILD_BROWSING_CONTEXT_HANDLE_SLOT, clear_event_composed_path,
        error_event_handler_arguments, mark_event_trusted, set_event_composed_path,
    },
    dom_parser::DOM_PARSER_FOREIGN_NODE_SLOT,
    util::{get_private_object, get_private_value, serialize_v8_iter_array},
};
use moli_webapi_declare::WebApiObject;
use std::collections::HashSet;

#[derive(WebApiObject)]
#[webapi(plain, data_properties, enumerable)]
struct HostErrorEventInitDeclaration<'scope> {
    cancelable: bool,
    bubbles: bool,
    message: v8::Local<'scope, v8::String>,
    filename: v8::Local<'scope, v8::String>,
    lineno: f64,
    colno: f64,
    error: v8::Local<'scope, v8::Value>,
}

#[derive(WebApiObject)]
#[webapi(plain, data_properties, enumerable)]
struct HostEventInitDeclaration {
    bubbles: bool,
    cancelable: bool,
}

struct EventListenerEntry {
    id: u64,
    callback_id: crate::native_bridge::EventCallbackId,
    function_name: String,
    script_id: i32,
    script_url: Option<String>,
    line_number: Option<u32>,
    column_number: Option<u32>,
    capture: bool,
    once: bool,
    passive: bool,
    removed: bool,
}

#[derive(Clone, Copy, Debug)]
enum ListenerFilter {
    Capturing,
    Bubbling,
    All,
}

impl ListenerFilter {
    /// Whether a listener registered with `capture` runs in this pass. The
    /// dispatch skip check and the listener snapshot share it.
    fn matches(self, capture: bool) -> bool {
        match self {
            Self::Capturing => capture,
            Self::Bubbling => !capture,
            Self::All => true,
        }
    }
}

pub(crate) struct EventListenerRegistration {
    callback_id: crate::native_bridge::EventCallbackId,
    function_name: String,
    script_id: i32,
    script_url: Option<String>,
    line_number: Option<u32>,
    column_number: Option<u32>,
    capture: bool,
    once: bool,
    passive: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct EventListenerInspectorSnapshot {
    pub(crate) registration_id: u64,
    pub(crate) event_type: String,
    pub(crate) callback_id: crate::native_bridge::EventCallbackId,
    pub(crate) capture: bool,
    pub(crate) once: bool,
    pub(crate) passive: bool,
}

impl EventListenerRegistration {
    pub(crate) fn new<'s>(
        scope: &mut v8::PinScope<'s, '_>,
        callback_id: crate::native_bridge::EventCallbackId,
        callback: v8::Local<'s, v8::Object>,
        capture: bool,
        once: bool,
        passive: bool,
    ) -> Self {
        let (function_name, script_id, script_url, line_number, column_number) =
            v8::Local::<v8::Function>::try_from(callback)
                .map(|listener| HostEventTargetRegistry::describe_listener(scope, listener))
                .unwrap_or_else(|_| ("handleEvent".to_owned(), -1, None, None, None));
        Self {
            callback_id,
            function_name,
            script_id,
            script_url,
            line_number,
            column_number,
            capture,
            once,
            passive,
        }
    }
}

#[derive(Clone, Copy)]
enum EventHandlerPropertyEntry {
    Callback {
        callback_id: crate::native_bridge::EventCallbackId,
        registration_id: u64,
    },
    Null,
}

enum EventHandlerPropertyValue {
    Callback(crate::native_bridge::EventCallbackId),
    Null,
}

pub(crate) struct HostEventTargetRegistry {
    listeners: HashMap<EventTargetHandle, HashMap<String, Vec<EventListenerEntry>>>,
    handler_properties: HashMap<EventTargetHandle, HashMap<String, EventHandlerPropertyEntry>>,
    event_type_registration_ids: HashMap<EventTargetHandle, HashMap<String, u64>>,
    next_listener_id: u64,
}

impl Default for HostEventTargetRegistry {
    fn default() -> Self {
        Self {
            listeners: HashMap::new(),
            handler_properties: HashMap::new(),
            event_type_registration_ids: HashMap::new(),
            next_listener_id: 1,
        }
    }
}

impl fmt::Debug for HostEventTargetRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostEventTargetRegistry")
            .field("target_count", &self.listeners.len())
            .field(
                "handler_property_target_count",
                &self.handler_properties.len(),
            )
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DispatchStatus {
    Continue,
    StopPropagation,
    StopImmediate,
}

pub(crate) fn invoke_prepared_event_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    host_ptr: *mut PageScriptHost,
    invocation_target_in_shadow_tree: bool,
    event_type: &str,
    callback_name: &str,
    callback: crate::native_bridge::PreparedEventCallback,
    target: EventTargetHandle,
    event: v8::Local<'s, v8::Object>,
    arguments: &[v8::Local<'s, v8::Value>],
) -> Option<v8::Global<v8::Value>> {
    let receiver = event_target_receiver(scope, host_ptr, target, event);
    let has_event_argument = arguments.len() == 1 && arguments[0].strict_equals(event.into());
    let (receiver, event) = {
        let context = callback.relevant_context(scope);
        let scope = &mut v8::ContextScope::new(scope, context);
        let catch = std::pin::pin!(v8::TryCatch::new(scope));
        let scope = &mut catch.init();
        // The current native ownerDocument supplies a Node's execution
        // context. Its wrapper can have been created in another Window or
        // adopted there, and an inert Document need not have a frame.
        let node_context = if let EventTargetHandle::Node(node) = target {
            Some(unsafe { &*host_ptr }.node_event_execution_context(scope, node)?)
        } else {
            None
        };
        let window = node_context
            .map(|context| context.global(scope))
            .or_else(|| v8::Local::<v8::Object>::try_from(receiver).ok());
        if let Some(window) = window {
            match crate::native_bridge::window_event_access(scope, window, context) {
                crate::native_bridge::WindowEventAccess::Retired => return None,
                crate::native_bridge::WindowEventAccess::Denied => {
                    let exception = crate::context_bootstrap::new_dom_exception_value(
                        scope,
                        "Blocked an event listener from accessing a cross-origin target.",
                        "SecurityError",
                    );
                    return Some(v8::Global::new(scope, exception));
                }
                crate::native_bridge::WindowEventAccess::Allowed
                | crate::native_bridge::WindowEventAccess::Unmanaged => {}
            }
        }
        let projection = if let Some(node_context) = node_context {
            crate::context_bootstrap::node_event_callback_values(
                scope,
                receiver,
                event,
                node_context,
                context,
            )
        } else {
            crate::context_bootstrap::event_callback_values(scope, receiver, event, context)
        };
        if let Some(exception) = scope.exception() {
            return Some(v8::Global::new(scope, exception));
        }
        projection?
    };
    // Window.onerror's five arguments are JS values, not an Event conversion.
    let event_arguments = [event.into()];
    let arguments = if has_event_argument {
        &event_arguments
    } else {
        arguments
    };
    let _dom_debugger_pause = unsafe { &*host_ptr }
        .schedule_dom_debugger_event_listener_pause_for_target(event_type, target);
    invoke_prepared_event_callback_with_receiver(
        scope,
        host_ptr,
        event_type,
        callback_name,
        callback,
        receiver,
        (!invocation_target_in_shadow_tree).then_some(event),
        arguments,
    )
}

#[allow(clippy::too_many_arguments)]
fn invoke_prepared_event_callback_with_receiver<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    host_ptr: *mut PageScriptHost,
    event_type: &str,
    callback_name: &str,
    callback: crate::native_bridge::PreparedEventCallback,
    receiver: v8::Local<'s, v8::Value>,
    current_event: Option<v8::Local<'s, v8::Object>>,
    arguments: &[v8::Local<'s, v8::Value>],
) -> Option<v8::Global<v8::Value>> {
    let relevant_identity = callback.relevant_identity();
    let invocation = CallbackInvocation::new(
        callback.callback(scope),
        receiver,
        callback.relevant_context(scope),
        callback.incumbent_context(scope),
        callback.is_callable(),
        "handleEvent",
        arguments,
        current_event,
    )
    .with_execution_context_currentness(host_ptr, relevant_identity);
    let invocation = if let Some(initiator) = callback.resource_initiator() {
        invocation.with_resource_initiator(initiator.clone())
    } else {
        invocation
    };
    match CallbackInvoker::invoke(
        scope,
        "event listener",
        "host event listener threw",
        crate::exception_reporting::CallbackExceptionLogLevel::Debug,
        callback_name,
        invocation,
    ) {
        CallbackInvocationOutcome::Returned(value) => Some(value),
        CallbackInvocationOutcome::Threw(report) => {
            report_event_callback_exception(
                scope,
                host_ptr,
                event_type,
                relevant_identity,
                None,
                &report,
            );
            None
        }
        CallbackInvocationOutcome::Retired => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn invoke_event_handler_property<'s>(
    registry: &mut HostEventTargetRegistry,
    scope: &mut v8::PinScope<'s, '_>,
    host_ptr: *mut PageScriptHost,
    target: EventTargetHandle,
    invocation_target_in_shadow_tree: bool,
    target_object: v8::Local<'s, v8::Object>,
    event_type: &str,
    event: v8::Local<'s, v8::Object>,
    path_entry: Option<&PathEntryEventState<'s, '_>>,
) -> std::result::Result<(), String> {
    let Some(handler_name) =
        crate::native_bridge::element::event_handler_content_attribute_name(event_type)
    else {
        return Ok(());
    };
    let (callback_id, temporary) = match registry.event_handler_property_value(target, event_type) {
        Some(EventHandlerPropertyValue::Callback(callback_id)) => (callback_id, false),
        Some(EventHandlerPropertyValue::Null) => {
            return Ok(());
        }
        None => {
            if !crate::context_bootstrap::event_handler_property_is_exposed(
                scope,
                target_object,
                &handler_name,
            ) {
                return Ok(());
            }
            let wrapper_handler = matches!(target, EventTargetHandle::Node(_))
                .then(|| {
                    crate::native_bridge::element::node_event_handler_value_from_wrapper(
                        scope,
                        target_object,
                        &handler_name,
                    )
                })
                .flatten();
            let native_inline = if wrapper_handler.is_some() {
                None
            } else {
                // Compiling a content attribute can report a window `error`
                // synchronously, so the entry's state is set first.
                if unsafe { &*host_ptr }
                    .event_handler_content_attribute_node(target, event_type)
                    .is_some()
                {
                    apply_path_entry(scope, host_ptr, path_entry)?;
                }
                match target {
                EventTargetHandle::Window if matches!(event_type, "load" | "pageshow") => Some(
                    crate::native_bridge::element::compile_window_body_event_attribute(
                        scope, host_ptr, event_type,
                    ),
                ),
                EventTargetHandle::Node(handle)
                    if crate::native_bridge::element::is_global_event_handler_attribute(
                        &handler_name,
                    ) && unsafe { &*host_ptr }
                        .dom_host()
                        .node(handle)
                        .and_then(|node| node.as_element())
                        .is_some_and(|element| {
                            element.namespace() == "http://www.w3.org/1999/xhtml"
                        }) =>
                {
                    Some(
                        crate::native_bridge::element::compile_node_content_attribute_for_dispatch(
                            scope, host_ptr, handle, event_type,
                        ),
                    )
                }
                _ => None,
            }
            };
            let (handler, persistent) = if let Some(value) = wrapper_handler {
                let Ok(handler) = v8::Local::<v8::Function>::try_from(value) else {
                    return Ok(());
                };
                // A foreign wrapper can still be updated in its producer realm.
                // Do not cache a second registration in its new document.
                (handler, false)
            } else if let Some(handler) = native_inline {
                let Some(handler) = handler else {
                    return Ok(());
                };
                (handler, true)
            } else {
                let Some(key) = v8_string(scope, &handler_name) else {
                    return Ok(());
                };
                // The property can be an author accessor.
                apply_path_entry(scope, host_ptr, path_entry)?;
                let handler = target_object
                    .get(scope, key.into())
                    .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok());
                match handler {
                    Some(handler) => (handler, false),
                    None if target == EventTargetHandle::Window && event_type == "messageerror" => {
                        let Some(handler) = crate::native_bridge::element::compile_window_body_onmessageerror_attribute(
                            scope, host_ptr,
                        ) else { return Ok(()); };
                        (handler, true)
                    }
                    None => return Ok(()),
                }
            };
            // Native inline compilation already installed its exact callback
            // and realm. Reuse that identity rather than registering a second
            // temporary callback for the same function.
            if persistent
                && let Some(EventHandlerPropertyValue::Callback(callback_id)) =
                    registry.event_handler_property_value(target, event_type)
            {
                (callback_id, false)
            } else {
                let callback = v8::Local::<v8::Object>::from(handler);
                let relevant_context = callback
                    .get_creation_context(scope)
                    .unwrap_or_else(|| scope.get_current_context());
                let incumbent_context = scope
                    .get_incumbent_context()
                    .unwrap_or_else(|| scope.get_current_context());
                let callback_id = unsafe { &mut *host_ptr }.register_event_callback(
                    scope,
                    callback,
                    relevant_context,
                    incumbent_context,
                );
                if persistent {
                    if let Some(previous) =
                        registry.set_event_handler_property(target, event_type, Some(callback_id))
                    {
                        unsafe { &mut *host_ptr }.release_event_callback(previous);
                    }
                    (callback_id, false)
                } else {
                    (callback_id, true)
                }
            }
        }
    };
    let Some(prepared) = unsafe { &*host_ptr }.prepare_event_callback(scope, callback_id) else {
        if temporary {
            unsafe { &mut *host_ptr }.release_event_callback(callback_id);
        }
        return Ok(());
    };
    if let Err(error) = apply_path_entry(scope, host_ptr, path_entry) {
        if temporary {
            unsafe { &mut *host_ptr }.release_event_callback(callback_id);
        }
        return Err(error);
    }

    let timing_started = moli_trace::cdp_nav_timing_enabled().then(Instant::now);
    if target == EventTargetHandle::Window
        && event_type == "error"
        && let Some(arguments) = error_event_handler_arguments(scope, event)
    {
        let returned = invoke_prepared_event_callback(
            scope,
            host_ptr,
            invocation_target_in_shadow_tree,
            event_type,
            &handler_name,
            prepared,
            target,
            event,
            &arguments,
        );
        if temporary {
            unsafe { &mut *host_ptr }.release_event_callback(callback_id);
        }
        if let Some(returned) = returned {
            let returned = v8::Local::new(scope, returned);
            if returned.boolean_value(scope) {
                let _ = crate::context_bootstrap::event_backing(scope, event).set(
                    scope,
                    v8str(scope, "defaultPrevented").into(),
                    v8::Boolean::new(scope, true).into(),
                );
            }
        }
        if let Some(timing_started) = timing_started {
            tracing::info!(
                target: "moli_cdp_nav_timing",
                stage = "event_handler_property_invoked",
                event_type,
                handler_name,
                ?target,
                elapsed_ms = timing_started.elapsed().as_millis(),
            );
        }
        return Ok(());
    }

    let returned = invoke_prepared_event_callback(
        scope,
        host_ptr,
        invocation_target_in_shadow_tree,
        event_type,
        &handler_name,
        prepared,
        target,
        event,
        &[event.into()],
    );
    if temporary {
        unsafe { &mut *host_ptr }.release_event_callback(callback_id);
    }
    if let Some(returned) = returned {
        let returned = v8::Local::new(scope, returned);
        if returned.is_boolean() && !returned.boolean_value(scope) {
            let _ = crate::context_bootstrap::event_backing(scope, event).set(
                scope,
                v8str(scope, "defaultPrevented").into(),
                v8::Boolean::new(scope, true).into(),
            );
        }
    }
    if let Some(timing_started) = timing_started {
        tracing::info!(
            target: "moli_cdp_nav_timing",
            stage = "event_handler_property_invoked",
            event_type,
            handler_name,
            ?target,
            elapsed_ms = timing_started.elapsed().as_millis(),
        );
    }
    Ok(())
}

fn apply_path_entry<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    host_ptr: *mut PageScriptHost,
    path_entry: Option<&PathEntryEventState<'s, '_>>,
) -> std::result::Result<(), String> {
    match path_entry {
        Some(path_entry) => path_entry.apply(scope, host_ptr),
        None => Ok(()),
    }
}

#[allow(clippy::too_many_arguments)]
fn invoke_event_target_handler_property<'s>(
    registry: &mut HostEventTargetRegistry,
    scope: &mut v8::PinScope<'s, '_>,
    host_ptr: *mut PageScriptHost,
    target: EventTargetHandle,
    invocation_target_in_shadow_tree: bool,
    target_object: v8::Local<'s, v8::Object>,
    event_type: &str,
    event: v8::Local<'s, v8::Object>,
    path_entry: &PathEntryEventState<'s, '_>,
) -> std::result::Result<(), String> {
    if matches!(target, EventTargetHandle::ChildWindow(_)) {
        // Child Window event-handler properties share the native listener
        // registry and are invoked in registration order by the listener phase.
        return Ok(());
    }
    invoke_event_handler_property(
        registry,
        scope,
        host_ptr,
        target,
        invocation_target_in_shadow_tree,
        target_object,
        event_type,
        event,
        Some(path_entry),
    )
}

/// Whether a listener of `target` matching `filter` can run for
/// `event_type`. A child Window's path listeners live in its own registry,
/// so it always counts as listening.
fn path_entry_has_listeners(
    registry: &HostEventTargetRegistry,
    target: EventTargetHandle,
    event_type: &str,
    filter: ListenerFilter,
) -> bool {
    matches!(target, EventTargetHandle::ChildWindow(_))
        || registry.has_listener_matching(target, event_type, filter)
}

/// What a dispatch's every propagation path entry shares.
#[derive(Clone, Copy)]
struct PathEventContext<'s, 'p> {
    event: v8::Local<'s, v8::Object>,
    propagation_path: &'p [EventTargetHandle],
    tree_scopes: &'p PathTreeScopes,
}

/// The tree scopes of a propagation path, fixed when the path is built, as
/// Blink's NodeEventContext and TreeScopeEventContext fix them. Entries of
/// one tree scope share its retargeted target and relatedTarget, so a
/// listener that moves the target or a host between trees changes nothing a
/// later entry sees. The visible composedPath is built only when an entry
/// runs script, once per tree scope, or once for the whole path when no
/// closed shadow root is on it.
struct PathTreeScopes {
    /// Each path entry's index into `scopes`.
    entry_scope: Vec<usize>,
    scopes: Vec<PathTreeScope>,
    target_steps: RetargetSteps,
    related_target_steps: Option<RetargetSteps>,
    /// The closed shadow roots on the path.
    closed_roots: Vec<NativeNodeId>,
    whole_path: std::cell::OnceCell<std::rc::Rc<[EventTargetHandle]>>,
}

struct PathTreeScope {
    /// The shadow roots that shadow-including contain this scope, innermost
    /// first. Empty outside any shadow tree, and for a Window.
    roots: Vec<NativeNodeId>,
    target: EventTargetHandle,
    related_target: Option<EventTargetHandle>,
    visible_path: std::cell::OnceCell<std::rc::Rc<[EventTargetHandle]>>,
}

/// A target and the (shadow root, host) steps that retargeting it can take,
/// innermost first.
struct RetargetSteps {
    original: EventTargetHandle,
    steps: Vec<(NativeNodeId, NativeNodeId)>,
}

impl RetargetSteps {
    fn new(dom: &moli_dom::native::DomHost, original: EventTargetHandle) -> Self {
        let mut steps = Vec::new();
        if let EventTargetHandle::Node(mut candidate) = original {
            while let Some(root) = dom.containing_shadow_root(candidate)
                && let Some(host) = dom.shadow_root_host(root)
            {
                steps.push((root, host));
                candidate = host;
            }
        }
        Self { original, steps }
    }

    /// The target as seen from a scope with these containing shadow roots.
    fn retarget(&self, roots: &[NativeNodeId]) -> EventTargetHandle {
        let mut candidate = self.original;
        for (root, host) in &self.steps {
            if roots.contains(root) {
                return candidate;
            }
            candidate = EventTargetHandle::Node(*host);
        }
        candidate
    }
}

#[cfg(test)]
thread_local! {
    static VISIBLE_PATH_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many visible composedPath vectors this thread's dispatches built.
#[cfg(test)]
pub(crate) fn visible_event_path_builds() -> usize {
    VISIBLE_PATH_BUILDS.with(std::cell::Cell::get)
}

impl PathTreeScopes {
    fn build(
        host_ptr: *mut PageScriptHost,
        original_target: EventTargetHandle,
        original_related_target: Option<EventTargetHandle>,
        propagation_path: &[EventTargetHandle],
    ) -> Self {
        let dom = unsafe { &*host_ptr }.dom_host();
        let mut tree_scopes = Self {
            entry_scope: Vec::with_capacity(propagation_path.len()),
            scopes: Vec::new(),
            target_steps: RetargetSteps::new(dom, original_target),
            related_target_steps: original_related_target
                .map(|related_target| RetargetSteps::new(dom, related_target)),
            closed_roots: Vec::new(),
            whole_path: std::cell::OnceCell::new(),
        };
        let mut previous: Option<(NativeNodeId, usize)> = None;
        for entry in propagation_path {
            let index = match *entry {
                EventTargetHandle::Node(handle) => {
                    // A parent shares its child's tree scope. Only a step
                    // into another tree (a host, a slot) looks up its root.
                    let index = previous
                        .filter(|(child, _)| {
                            !dom.is_shadow_root(*child)
                                && dom
                                    .node(*child)
                                    .and_then(moli_dom::native::Node::parent_node)
                                    == Some(handle)
                        })
                        .map_or_else(
                            || tree_scopes.scope_index(dom, dom.containing_shadow_root(handle)),
                            |(_, index)| index,
                        );
                    if dom.is_shadow_root(handle)
                        && dom.shadow_root_mode(handle).as_deref() == Some("closed")
                    {
                        tree_scopes.closed_roots.push(handle);
                    }
                    previous = Some((handle, index));
                    index
                }
                EventTargetHandle::Window | EventTargetHandle::ChildWindow(_) => {
                    previous = None;
                    tree_scopes.scope_index(dom, None)
                }
            };
            tree_scopes.entry_scope.push(index);
        }
        tree_scopes
    }

    fn scope_index(
        &mut self,
        dom: &moli_dom::native::DomHost,
        innermost_root: Option<NativeNodeId>,
    ) -> usize {
        if let Some(index) = self
            .scopes
            .iter()
            .position(|scope| scope.roots.first().copied() == innermost_root)
        {
            return index;
        }
        let scope = self.scope_with_innermost_root(dom, innermost_root);
        self.scopes.push(scope);
        self.scopes.len() - 1
    }

    fn scope_with_innermost_root(
        &self,
        dom: &moli_dom::native::DomHost,
        innermost_root: Option<NativeNodeId>,
    ) -> PathTreeScope {
        let mut roots = Vec::new();
        let mut root = innermost_root;
        while let Some(current) = root {
            roots.push(current);
            root = dom
                .shadow_root_host(current)
                .and_then(|host| dom.containing_shadow_root(host));
        }
        PathTreeScope {
            target: self.target_steps.retarget(&roots),
            related_target: self
                .related_target_steps
                .as_ref()
                .map(|steps| steps.retarget(&roots)),
            roots,
            visible_path: std::cell::OnceCell::new(),
        }
    }

    /// The scope of an entry the path does not list, built on demand.
    fn scope_outside_path(
        &self,
        host_ptr: *mut PageScriptHost,
        current_target: EventTargetHandle,
    ) -> PathTreeScope {
        let dom = unsafe { &*host_ptr }.dom_host();
        let innermost_root = match current_target {
            EventTargetHandle::Node(handle) => dom.containing_shadow_root(handle),
            EventTargetHandle::Window | EventTargetHandle::ChildWindow(_) => None,
        };
        self.scope_with_innermost_root(dom, innermost_root)
    }

    fn is_at_target(&self, index: usize, entry: EventTargetHandle) -> bool {
        self.scopes[self.entry_scope[index]].target == entry
    }

    /// The path an entry of this scope sees: every entry except those inside
    /// a closed shadow root that does not contain the scope.
    fn visible_path(
        &self,
        propagation_path: &[EventTargetHandle],
        scope: &PathTreeScope,
    ) -> std::rc::Rc<[EventTargetHandle]> {
        let build = |hidden: Option<Vec<bool>>| {
            #[cfg(test)]
            VISIBLE_PATH_BUILDS.with(|builds| builds.set(builds.get() + 1));
            propagation_path
                .iter()
                .zip(&self.entry_scope)
                .filter(|(_, index)| !hidden.as_ref().is_some_and(|hidden| hidden[**index]))
                .map(|(entry, _)| *entry)
                .collect::<std::rc::Rc<[EventTargetHandle]>>()
        };
        if self.closed_roots.is_empty() {
            return self.whole_path.get_or_init(|| build(None)).clone();
        }
        scope
            .visible_path
            .get_or_init(|| {
                build(Some(
                    self.scopes
                        .iter()
                        .map(|other| {
                            other.roots.iter().any(|root| {
                                self.closed_roots.contains(root) && !scope.roots.contains(root)
                            })
                        })
                        .collect(),
                ))
            })
            .clone()
    }
}

#[derive(Clone, Copy)]
enum PathEntryPhase {
    AtTarget,
    /// Capturing (1) or bubbling (3), unless the entry is the target after
    /// retargeting.
    Around(u32),
}

/// The event's currentTarget, target, relatedTarget, source, composedPath
/// and eventPhase as seen at one propagation path entry. Dispatch sets them
/// before any script of that entry can run: a listener, an event handler, or
/// the handler's content-attribute compile or property lookup. Only script
/// can observe them during dispatch, and an entry where nothing runs leaves
/// nothing to observe them. A failed set is not recorded as applied.
struct PathEntryEventState<'s, 'p> {
    context: PathEventContext<'s, 'p>,
    current_target: EventTargetHandle,
    current_target_object: Option<v8::Local<'s, v8::Object>>,
    phase: PathEntryPhase,
    applied: std::cell::Cell<bool>,
}

impl<'s, 'p> PathEntryEventState<'s, 'p> {
    fn new(
        context: PathEventContext<'s, 'p>,
        current_target: EventTargetHandle,
        current_target_object: Option<v8::Local<'s, v8::Object>>,
        phase: PathEntryPhase,
    ) -> Self {
        Self {
            context,
            current_target,
            current_target_object,
            phase,
            applied: std::cell::Cell::new(false),
        }
    }

    fn apply(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        host_ptr: *mut PageScriptHost,
    ) -> std::result::Result<(), String> {
        if self.applied.get() {
            return Ok(());
        }
        let context = self.context;
        let current_target_object = match self.current_target_object {
            Some(object) => object,
            None => event_target_object(scope, host_ptr, self.current_target)?,
        };
        let tree_scopes = context.tree_scopes;
        let outside_scope;
        let tree_scope = match context
            .propagation_path
            .iter()
            .position(|entry| *entry == self.current_target)
        {
            Some(index) => &tree_scopes.scopes[tree_scopes.entry_scope[index]],
            None => {
                outside_scope = tree_scopes.scope_outside_path(host_ptr, self.current_target);
                &outside_scope
            }
        };
        let visible_path = tree_scopes.visible_path(context.propagation_path, tree_scope);
        set_event_current_target(scope, context.event, current_target_object.into());
        let target_value = event_target_value(scope, host_ptr, tree_scope.target)?;
        set_event_target(scope, context.event, target_value);
        set_event_related_target_for_current_target(
            scope,
            host_ptr,
            context.event,
            tree_scope.related_target,
        )?;
        set_event_composed_path_for_current_target(
            scope,
            host_ptr,
            context.event,
            context.propagation_path,
            &visible_path,
        )?;
        let phase = match self.phase {
            PathEntryPhase::AtTarget => 2,
            PathEntryPhase::Around(_) if tree_scope.target == self.current_target => 2,
            PathEntryPhase::Around(phase) => phase,
        };
        set_event_phase(scope, context.event, phase);
        self.applied.set(true);
        Ok(())
    }
}

fn call_event_target_listeners_filtered<'s>(
    registry: &mut HostEventTargetRegistry,
    scope: &mut v8::PinScope<'s, '_>,
    host_ptr: *mut PageScriptHost,
    target: EventTargetHandle,
    invocation_target_in_shadow_tree: bool,
    event_type: &str,
    event: v8::Local<'s, v8::Object>,
    capture_only: bool,
    at_target: bool,
) -> std::result::Result<DispatchStatus, String> {
    if let EventTargetHandle::ChildWindow(target) = target {
        return Ok(
            unsafe { &mut *host_ptr }.call_child_window_event_path_listeners(
                scope,
                target,
                event_type,
                event,
                capture_only,
                at_target,
            ),
        );
    }
    registry.call_listeners_filtered(
        scope,
        host_ptr,
        target,
        invocation_target_in_shadow_tree,
        event_type,
        event,
        if capture_only {
            ListenerFilter::Capturing
        } else {
            ListenerFilter::Bubbling
        },
        at_target,
    )
}

fn event_target_is_current(host_ptr: *mut PageScriptHost, target: EventTargetHandle) -> bool {
    match target {
        EventTargetHandle::ChildWindow(target) => {
            unsafe { &*host_ptr }.child_window_event_target_is_current(target)
        }
        EventTargetHandle::Window | EventTargetHandle::Node(_) => true,
    }
}

pub(crate) fn report_event_listener_exception<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    host_ptr: *mut PageScriptHost,
    event_type: &str,
    handler: v8::Local<'s, v8::Function>,
    report: &V8ExceptionReport,
) {
    let relevant_identity = handler.get_creation_context(scope).and_then(|context| {
        unsafe { &*host_ptr }.window_execution_context_identity_for_v8_context(scope, context)
    });
    let child_handle = callback_error_window_handle(scope, handler);
    report_event_callback_exception(
        scope,
        host_ptr,
        event_type,
        relevant_identity,
        child_handle,
        report,
    );
}

pub(crate) fn report_event_callback_exception<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    host_ptr: *mut PageScriptHost,
    event_type: &str,
    relevant_identity: Option<crate::native_bridge::WindowExecutionContextIdentity>,
    child_handle: Option<DomHandle>,
    report: &V8ExceptionReport,
) {
    if event_type == "error" {
        return;
    }

    let child_handle = child_handle.or_else(|| {
        relevant_identity.and_then(|identity| match identity.dispatch_scope() {
            crate::native_bridge::OwnerDispatchScope::Child(handle) => Some(handle),
            crate::native_bridge::OwnerDispatchScope::Top
            | crate::native_bridge::OwnerDispatchScope::LightweightPopup(_) => None,
        })
    });
    if let Some(handle) = child_handle {
        let event = {
            let host = unsafe { &mut *host_ptr };
            host.child_browsing_context_window_wrapper(scope, handle)
                .and_then(|window| event_listener_error_event_from_report(scope, window, report))
        };
        if let Some(event) = event {
            unsafe { &mut *host_ptr }.dispatch_child_window_event(scope, handle, "error", event);
            return;
        }
    }

    let error_value = report
        .exception
        .as_ref()
        .map(|exception| v8::Local::new(scope, exception));
    let _ = dispatch_window_error_event_with_details(
        scope,
        host_ptr,
        &report.summary,
        report.source.as_deref().unwrap_or(""),
        report.line.unwrap_or(0) as u32,
        report.column.unwrap_or(0) as u32,
        error_value,
    );
}

fn callback_error_window_handle<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    callback: v8::Local<'s, v8::Function>,
) -> Option<DomHandle> {
    if let Some(handle) =
        get_private_value(scope, callback.into(), CALLBACK_ERROR_WINDOW_HANDLE_SLOT)
            .and_then(|value| dom_handle_from_value(scope, value))
    {
        return Some(handle);
    }
    let context = callback.get_creation_context(scope)?;
    let global = context.global(scope);
    if let Some(handle) = get_private_value(scope, global, CHILD_BROWSING_CONTEXT_HANDLE_SLOT)
        .and_then(|value| dom_handle_from_value(scope, value))
    {
        return Some(handle);
    }
    None
}

fn dom_handle_from_value(
    scope: &mut v8::PinScope<'_, '_>,
    value: v8::Local<'_, v8::Value>,
) -> Option<DomHandle> {
    if let Ok(big) = v8::Local::<v8::BigInt>::try_from(value) {
        let (index, lossless) = big.u64_value();
        return lossless.then(|| DomHandle::new(index as usize));
    }
    let handle = value.number_value(scope)?;
    (handle.is_finite() && handle >= 0.0 && handle.fract() == 0.0)
        .then(|| DomHandle::new(handle as usize))
}

fn event_listener_error_event_from_report<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    report: &V8ExceptionReport,
) -> Option<v8::Local<'s, v8::Object>> {
    let error_value = report
        .exception
        .as_ref()
        .map(|exception| v8::Local::new(scope, exception))
        .unwrap_or_else(|| v8::null(scope).into());

    let message = v8_string(scope, &report.summary).unwrap_or_else(|| v8str(scope, ""));
    let filename = v8_string(scope, report.source.as_deref().unwrap_or(""))
        .unwrap_or_else(|| v8str(scope, ""));
    let init = HostErrorEventInitDeclaration::new(
        true,
        false,
        message,
        filename,
        report.line.unwrap_or(0) as f64,
        report.column.unwrap_or(0) as f64,
        error_value,
    )
    .bind(scope)
    .ok()?;

    global
        .get(scope, v8str(scope, "ErrorEvent").into())
        .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok())
        .and_then(|constructor| {
            constructor.new_instance(scope, &[v8str(scope, "error").into(), init.into()])
        })
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PublicEventDispatchResult {
    pub default_prevented: bool,
    canceled_before_dispatch: bool,
    /// The path entry whose local event handling ran last, as Blink's
    /// `HandleLocalEvents` reaches each entry whether or not it has
    /// listeners. `None` when no entry was reached.
    last_handled_entry: Option<EventTargetHandle>,
}

impl PublicEventDispatchResult {
    pub(crate) fn canceled_before_dispatch(self) -> bool {
        self.canceled_before_dispatch
    }

    pub(crate) fn last_handled_entry(self) -> Option<EventTargetHandle> {
        self.last_handled_entry
    }

    pub(crate) fn allows_default(self) -> bool {
        !self.default_prevented && !self.canceled_before_dispatch
    }

    pub(crate) fn dispatch_event_return_value(self) -> bool {
        self.allows_default()
    }
}

impl HostEventTargetRegistry {
    pub(crate) fn clear_targets_matching(
        &mut self,
        mut should_clear: impl FnMut(EventTargetHandle) -> bool,
    ) -> HashSet<crate::native_bridge::EventCallbackId> {
        let mut retired = HashSet::new();
        let targets_to_clear = self
            .listeners
            .keys()
            .chain(self.handler_properties.keys())
            .chain(self.event_type_registration_ids.keys())
            .copied()
            .collect::<HashSet<_>>()
            .into_iter()
            .filter(|target| should_clear(*target))
            .collect::<HashSet<_>>();
        self.listeners.retain(|target, listeners| {
            if targets_to_clear.contains(target) {
                retired.extend(listeners.values().flatten().map(|entry| entry.callback_id));
                false
            } else {
                true
            }
        });
        for (target, handlers) in &mut self.handler_properties {
            if targets_to_clear.contains(target) {
                for handler in handlers.values_mut() {
                    if let EventHandlerPropertyEntry::Callback { callback_id, .. } = *handler {
                        retired.insert(callback_id);
                    }
                    *handler = EventHandlerPropertyEntry::Null;
                }
            }
        }
        self.event_type_registration_ids
            .retain(|target, _| !targets_to_clear.contains(target));
        retired
    }

    fn describe_listener(
        scope: &mut v8::PinScope<'_, '_>,
        listener: v8::Local<'_, v8::Function>,
    ) -> (String, i32, Option<String>, Option<u32>, Option<u32>) {
        let function_name = v8::Local::<v8::String>::try_from(listener.get_name(scope))
            .map(|name| name.to_rust_string_lossy(scope))
            .unwrap_or_default();
        let script_id = listener.script_id();
        let script_url = listener
            .get_script_origin(scope)
            .resource_name()
            .map(|value| value.to_rust_string_lossy(scope))
            .filter(|value| !value.is_empty());
        let line_number = listener.get_script_line_number();
        let column_number = listener.get_script_column_number();
        (
            function_name,
            script_id,
            script_url,
            line_number,
            column_number,
        )
    }

    pub(crate) fn set_event_handler_property(
        &mut self,
        target: EventTargetHandle,
        event_type: &str,
        callback_id: Option<crate::native_bridge::EventCallbackId>,
    ) -> Option<crate::native_bridge::EventCallbackId> {
        let existing_registration_id = self
            .handler_properties
            .get(&target)
            .and_then(|handlers| handlers.get(event_type))
            .and_then(|entry| match entry {
                EventHandlerPropertyEntry::Callback {
                    registration_id, ..
                } => Some(*registration_id),
                EventHandlerPropertyEntry::Null => None,
            });
        let entry = match callback_id {
            Some(callback_id) => {
                let registration_id =
                    existing_registration_id.unwrap_or_else(|| self.allocate_listener_id());
                self.ensure_event_type_registration_id(target, event_type, registration_id);
                EventHandlerPropertyEntry::Callback {
                    callback_id,
                    registration_id,
                }
            }
            None => EventHandlerPropertyEntry::Null,
        };
        let previous = self
            .handler_properties
            .entry(target)
            .or_default()
            .insert(event_type.to_owned(), entry);
        let previous_callback = match previous {
            Some(EventHandlerPropertyEntry::Callback { callback_id, .. }) => Some(callback_id),
            Some(EventHandlerPropertyEntry::Null) | None => None,
        };
        if callback_id.is_none() {
            self.remove_event_type_registration_id_if_unused(target, event_type);
        }
        previous_callback
    }

    pub(crate) fn clear_event_handler_property(
        &mut self,
        target: EventTargetHandle,
        event_type: &str,
    ) -> Option<crate::native_bridge::EventCallbackId> {
        let removed = self
            .handler_properties
            .get_mut(&target)
            .and_then(|handlers| handlers.remove(event_type));
        let remove_target = self
            .handler_properties
            .get(&target)
            .is_some_and(HashMap::is_empty);
        if remove_target {
            self.handler_properties.remove(&target);
        }
        let callback_id = match removed {
            Some(EventHandlerPropertyEntry::Callback { callback_id, .. }) => Some(callback_id),
            Some(EventHandlerPropertyEntry::Null) | None => None,
        };
        self.remove_event_type_registration_id_if_unused(target, event_type);
        callback_id
    }

    fn event_handler_property_value(
        &self,
        target: EventTargetHandle,
        event_type: &str,
    ) -> Option<EventHandlerPropertyValue> {
        match self
            .handler_properties
            .get(&target)
            .and_then(|target_handlers| target_handlers.get(event_type))?
        {
            EventHandlerPropertyEntry::Callback { callback_id, .. } => {
                Some(EventHandlerPropertyValue::Callback(*callback_id))
            }
            EventHandlerPropertyEntry::Null => Some(EventHandlerPropertyValue::Null),
        }
    }

    pub(crate) fn event_handler_property_callback_id(
        &self,
        target: EventTargetHandle,
        event_type: &str,
    ) -> Option<Option<crate::native_bridge::EventCallbackId>> {
        match self
            .handler_properties
            .get(&target)
            .and_then(|target_handlers| target_handlers.get(event_type))?
        {
            EventHandlerPropertyEntry::Callback { callback_id, .. } => Some(Some(*callback_id)),
            EventHandlerPropertyEntry::Null => Some(None),
        }
    }

    pub(crate) fn listener_callback_ids(
        &self,
        target: EventTargetHandle,
        event_type: &str,
        capture: bool,
    ) -> Vec<crate::native_bridge::EventCallbackId> {
        self.listeners
            .get(&target)
            .and_then(|target_listeners| target_listeners.get(event_type))
            .into_iter()
            .flatten()
            .filter(|entry| !entry.removed && entry.capture == capture)
            .map(|entry| entry.callback_id)
            .collect()
    }

    #[cfg(feature = "devtools")]
    pub(crate) fn inspector_listener_snapshots(
        &self,
        target: EventTargetHandle,
    ) -> Vec<EventListenerInspectorSnapshot> {
        let mut snapshots = Vec::new();
        if let Some(target_listeners) = self.listeners.get(&target) {
            for (event_type, entries) in target_listeners {
                let event_type_registration_id = self
                    .event_type_registration_id(target, event_type)
                    .unwrap_or_else(|| entries.first().map(|entry| entry.id).unwrap_or(u64::MAX));
                snapshots.extend(entries.iter().filter(|entry| !entry.removed).map(|entry| {
                    (
                        event_type_registration_id,
                        EventListenerInspectorSnapshot {
                            registration_id: entry.id,
                            event_type: event_type.clone(),
                            callback_id: entry.callback_id,
                            capture: entry.capture,
                            once: entry.once,
                            passive: entry.passive,
                        },
                    )
                }));
            }
        }
        if let Some(target_handlers) = self.handler_properties.get(&target) {
            for (event_type, entry) in target_handlers {
                let EventHandlerPropertyEntry::Callback {
                    callback_id,
                    registration_id,
                } = *entry
                else {
                    continue;
                };
                snapshots.push((
                    self.event_type_registration_id(target, event_type)
                        .unwrap_or(registration_id),
                    EventListenerInspectorSnapshot {
                        registration_id,
                        event_type: event_type.clone(),
                        callback_id,
                        capture: false,
                        once: false,
                        passive: false,
                    },
                ));
            }
        }
        snapshots.sort_by_key(|(event_type_registration_id, snapshot)| {
            (*event_type_registration_id, snapshot.registration_id)
        });
        snapshots
            .into_iter()
            .map(|(_, snapshot)| snapshot)
            .collect()
    }

    pub(crate) fn insert_listener(
        &mut self,
        target: EventTargetHandle,
        event_type: &str,
        registration: EventListenerRegistration,
    ) {
        let id = self.allocate_listener_id();
        self.ensure_event_type_registration_id(target, event_type, id);
        let entries = self
            .listeners
            .entry(target)
            .or_default()
            .entry(event_type.to_owned())
            .or_default();
        if moli_trace::cdp_nav_timing_enabled() {
            tracing::info!(
                target: "moli_cdp_nav_timing",
                stage = "event_listener_registered",
                id,
                event_type,
                ?target,
                capture = registration.capture,
                passive = registration.passive,
                function_name = registration.function_name.as_str(),
                script_id = registration.script_id,
                script_url = registration.script_url.as_deref().unwrap_or(""),
                line_number = registration.line_number.map(u64::from),
                column_number = registration.column_number.map(u64::from),
            );
        }
        entries.push(EventListenerEntry {
            id,
            callback_id: registration.callback_id,
            function_name: registration.function_name,
            script_id: registration.script_id,
            script_url: registration.script_url,
            line_number: registration.line_number,
            column_number: registration.column_number,
            capture: registration.capture,
            once: registration.once,
            passive: registration.passive,
            removed: false,
        });
    }

    pub(crate) fn remove_listener_by_id(
        &mut self,
        target: EventTargetHandle,
        event_type: &str,
        callback_id: crate::native_bridge::EventCallbackId,
        capture: bool,
    ) -> bool {
        let Some(target_listeners) = self.listeners.get_mut(&target) else {
            return false;
        };
        let Some(entries) = target_listeners.get_mut(event_type) else {
            return false;
        };
        let before = entries.len();
        entries.retain(|entry| entry.callback_id != callback_id || entry.capture != capture);
        let removed = entries.len() != before;
        if entries.is_empty() {
            target_listeners.remove(event_type);
        }
        if target_listeners.is_empty() {
            self.listeners.remove(&target);
        }
        self.remove_event_type_registration_id_if_unused(target, event_type);
        removed
    }

    pub(crate) fn remove_callback_registrations(
        &mut self,
        retired: &std::collections::HashSet<crate::native_bridge::EventCallbackId>,
    ) {
        for target_listeners in self.listeners.values_mut() {
            target_listeners.retain(|_, entries| {
                entries.retain(|entry| !retired.contains(&entry.callback_id));
                !entries.is_empty()
            });
        }
        self.listeners
            .retain(|_, target_listeners| !target_listeners.is_empty());
        for target_handlers in self.handler_properties.values_mut() {
            for entry in target_handlers.values_mut() {
                if matches!(
                    entry,
                    EventHandlerPropertyEntry::Callback { callback_id, .. }
                        if retired.contains(callback_id)
                ) {
                    // Absence enables the legacy wrapper fallback, which can
                    // rediscover a callback from its retired realm.
                    *entry = EventHandlerPropertyEntry::Null;
                }
            }
        }
        self.retain_active_event_type_registration_ids();
    }

    pub(crate) fn has_listener(&self, target: EventTargetHandle, event_type: &str) -> bool {
        self.listeners
            .get(&target)
            .and_then(|target_listeners| target_listeners.get(event_type))
            .is_some_and(|entries| entries.iter().any(|entry| !entry.removed))
    }

    fn has_listener_matching(
        &self,
        target: EventTargetHandle,
        event_type: &str,
        filter: ListenerFilter,
    ) -> bool {
        self.listeners
            .get(&target)
            .and_then(|listeners| listeners.get(event_type))
            .is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| !entry.removed && filter.matches(entry.capture))
            })
    }

    pub(crate) fn has_listener_in_dispatch_path(
        &self,
        target: EventTargetHandle,
        event_type: &str,
        include_bubbling: bool,
    ) -> bool {
        self.listeners
            .get(&target)
            .and_then(|listeners| listeners.get(event_type))
            .is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| !entry.removed && (entry.capture || include_bubbling))
            })
    }

    fn allocate_listener_id(&mut self) -> u64 {
        let id = self.next_listener_id;
        self.next_listener_id = self
            .next_listener_id
            .checked_add(1)
            .expect("event listener registration id overflow");
        id
    }

    fn ensure_event_type_registration_id(
        &mut self,
        target: EventTargetHandle,
        event_type: &str,
        registration_id: u64,
    ) {
        self.event_type_registration_ids
            .entry(target)
            .or_default()
            .entry(event_type.to_owned())
            .or_insert(registration_id);
    }

    #[cfg(feature = "devtools")]
    fn event_type_registration_id(
        &self,
        target: EventTargetHandle,
        event_type: &str,
    ) -> Option<u64> {
        self.event_type_registration_ids
            .get(&target)
            .and_then(|event_types| event_types.get(event_type))
            .copied()
    }

    fn event_type_has_live_registration(
        &self,
        target: EventTargetHandle,
        event_type: &str,
    ) -> bool {
        self.listeners
            .get(&target)
            .and_then(|listeners| listeners.get(event_type))
            .is_some_and(|entries| entries.iter().any(|entry| !entry.removed))
            || self
                .handler_properties
                .get(&target)
                .and_then(|handlers| handlers.get(event_type))
                .is_some_and(|entry| matches!(entry, EventHandlerPropertyEntry::Callback { .. }))
    }

    fn remove_event_type_registration_id_if_unused(
        &mut self,
        target: EventTargetHandle,
        event_type: &str,
    ) {
        if self.event_type_has_live_registration(target, event_type) {
            return;
        }
        let Some(event_types) = self.event_type_registration_ids.get_mut(&target) else {
            return;
        };
        event_types.remove(event_type);
        if event_types.is_empty() {
            self.event_type_registration_ids.remove(&target);
        }
    }

    fn retain_active_event_type_registration_ids(&mut self) {
        let listeners = &self.listeners;
        let handler_properties = &self.handler_properties;
        self.event_type_registration_ids
            .retain(|target, event_types| {
                event_types.retain(|event_type, _| {
                    listeners
                        .get(target)
                        .and_then(|target_listeners| target_listeners.get(event_type))
                        .is_some_and(|entries| entries.iter().any(|entry| !entry.removed))
                        || handler_properties
                            .get(target)
                            .and_then(|handlers| handlers.get(event_type))
                            .is_some_and(|entry| {
                                matches!(entry, EventHandlerPropertyEntry::Callback { .. })
                            })
                });
                !event_types.is_empty()
            });
    }

    fn call_listeners_filtered<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        host_ptr: *mut PageScriptHost,
        target: EventTargetHandle,
        invocation_target_in_shadow_tree: bool,
        event_type: &str,
        event: v8::Local<'s, v8::Object>,
        filter: ListenerFilter,
        at_target: bool,
    ) -> std::result::Result<DispatchStatus, String> {
        let snapshot: Vec<(
            u64,
            crate::native_bridge::EventCallbackId,
            String,
            i32,
            Option<String>,
            Option<u32>,
            Option<u32>,
            bool,
            bool,
            bool,
        )> = {
            let Some(target_map) = self.listeners.get(&target) else {
                return Ok(DispatchStatus::Continue);
            };
            let Some(entries) = target_map.get(event_type) else {
                return Ok(DispatchStatus::Continue);
            };
            entries
                .iter()
                .filter(|entry| !entry.removed)
                .filter(|entry| filter.matches(entry.capture))
                .map(|entry| {
                    (
                        entry.id,
                        entry.callback_id,
                        entry.function_name.clone(),
                        entry.script_id,
                        entry.script_url.clone(),
                        entry.line_number,
                        entry.column_number,
                        entry.capture,
                        entry.once,
                        entry.passive,
                    )
                })
                .collect()
        };

        if snapshot.is_empty() {
            return Ok(DispatchStatus::Continue);
        }

        let timing_enabled = moli_trace::cdp_nav_timing_enabled();
        if timing_enabled {
            tracing::info!(
                target: "moli_cdp_nav_timing",
                stage = "event_listener_batch_start",
                event_type,
                ?target,
                ?filter,
                at_target,
                listener_count = snapshot.len(),
            );
        }
        for (
            listener_index,
            (
                listener_id,
                callback_id,
                function_name,
                script_id,
                script_url,
                line_number,
                column_number,
                capture_flag,
                is_once,
                is_passive,
            ),
        ) in snapshot.into_iter().enumerate()
        {
            let still_registered = self
                .listeners
                .get(&target)
                .and_then(|listeners| listeners.get(event_type))
                .is_some_and(|entries| {
                    entries.iter().any(|entry| {
                        !entry.removed
                            && entry.capture == capture_flag
                            && entry.callback_id == callback_id
                    })
                });

            if !still_registered {
                continue;
            }

            let Some(prepared) = unsafe { &*host_ptr }.prepare_event_callback(scope, callback_id)
            else {
                self.remove_listener_by_id(target, event_type, callback_id, capture_flag);
                unsafe {
                    (&mut *host_ptr).unregister_abort_target_listener(callback_id);
                    (&mut *host_ptr).release_event_callback(callback_id);
                }
                tracing::debug!(
                    listener_id,
                    ?target,
                    event_type,
                    "skipped event listener owned by a retired execution context"
                );
                continue;
            };

            if is_once {
                self.remove_listener_by_id(target, event_type, callback_id, capture_flag);
                unsafe {
                    (&mut *host_ptr).unregister_abort_target_listener(callback_id);
                    (&mut *host_ptr).release_event_callback(callback_id);
                }
            }

            let listener_name = format!("{event_type} listener");
            set_event_internal_flag(scope, event, EVENT_PASSIVE_SLOT, is_passive);
            let listener_started = timing_enabled.then(Instant::now);
            let layout_trace_before =
                timing_enabled.then(|| unsafe { &*host_ptr }.layout_metric_trace_snapshot());
            let _ = invoke_prepared_event_callback(
                scope,
                host_ptr,
                invocation_target_in_shadow_tree,
                event_type,
                &listener_name,
                prepared,
                target,
                event,
                &[event.into()],
            );
            if let Some(listener_started) = listener_started {
                let layout_trace = layout_trace_before
                    .map(|before| {
                        unsafe { &*host_ptr }
                            .layout_metric_trace_snapshot()
                            .saturating_delta(before)
                    })
                    .unwrap_or_default();
                tracing::info!(
                    target: "moli_cdp_nav_timing",
                    stage = "event_listener_invoked",
                    listener_id,
                    event_type,
                    ?target,
                    ?filter,
                    at_target,
                    listener_index,
                    function_name = function_name.as_str(),
                    script_id,
                    script_url = script_url.as_deref().unwrap_or(""),
                    line_number = line_number.map(u64::from),
                    column_number = column_number.map(u64::from),
                    capture_flag,
                    is_once,
                    is_passive,
                    layout_client_rect_count = layout_trace.client_rect_count,
                    layout_client_rect_ms = layout_trace.client_rect_ms(),
                    layout_offset_parent_count = layout_trace.offset_parent_count,
                    layout_offset_parent_ms = layout_trace.offset_parent_ms(),
                    layout_offset_position_count = layout_trace.offset_position_count,
                    layout_offset_position_ms = layout_trace.offset_position_ms(),
                    elapsed_ms = listener_started.elapsed().as_millis(),
                );
            }
            set_event_internal_flag(scope, event, EVENT_PASSIVE_SLOT, false);

            if event_internal_bool_flag(scope, event, EVENT_STOP_IMMEDIATE_SLOT) {
                return Ok(DispatchStatus::StopImmediate);
            }
        }
        if event_internal_bool_flag(scope, event, EVENT_STOP_PROPAGATION_SLOT) {
            Ok(DispatchStatus::StopPropagation)
        } else {
            Ok(DispatchStatus::Continue)
        }
    }
}

pub(crate) fn dispatch_public_event<'s, 'i>(
    registry: &mut HostEventTargetRegistry,
    scope: &mut v8::PinScope<'s, 'i>,
    host_ptr: *mut PageScriptHost,
    dispatch_target: EventTargetHandle,
    propagation_path: &[EventTargetHandle],
    event: v8::Local<'s, v8::Object>,
) -> std::result::Result<PublicEventDispatchResult, String> {
    dispatch_public_event_with_original_target(
        registry,
        scope,
        host_ptr,
        dispatch_target,
        dispatch_target,
        propagation_path,
        event,
    )
}

pub(crate) fn dispatch_public_event_with_original_target<'s, 'i>(
    registry: &mut HostEventTargetRegistry,
    scope: &mut v8::PinScope<'s, 'i>,
    host_ptr: *mut PageScriptHost,
    dispatch_target: EventTargetHandle,
    original_target: EventTargetHandle,
    propagation_path: &[EventTargetHandle],
    event: v8::Local<'s, v8::Object>,
) -> std::result::Result<PublicEventDispatchResult, String> {
    let result = dispatch_public_event_with_original_target_impl(
        registry,
        scope,
        host_ptr,
        dispatch_target,
        original_target,
        propagation_path,
        event,
    );
    // InputEvent's live ranges end with Node dispatch, including error exits.
    // Direct Window and ordinary EventTarget dispatch retain them in Chromium.
    if matches!(dispatch_target, EventTargetHandle::Node(_)) {
        crate::context_bootstrap::clear_input_event_target_ranges(scope, event);
    }
    result
}

fn dispatch_public_event_with_original_target_impl<'s, 'i>(
    registry: &mut HostEventTargetRegistry,
    scope: &mut v8::PinScope<'s, 'i>,
    host_ptr: *mut PageScriptHost,
    dispatch_target: EventTargetHandle,
    original_target: EventTargetHandle,
    propagation_path: &[EventTargetHandle],
    event: v8::Local<'s, v8::Object>,
) -> std::result::Result<PublicEventDispatchResult, String> {
    unsafe { &*host_ptr }.debug_assert_not_in_structural_mutation("public event dispatch");
    if let EventTargetHandle::Node(node) = dispatch_target
        && unsafe { &*host_ptr }
            .document_execution_context_for_node(node)
            .is_none()
    {
        return Ok(PublicEventDispatchResult {
            canceled_before_dispatch: true,
            default_prevented: event_default_prevented(scope, event),
            last_handled_entry: None,
        });
    }
    let target_object = event_target_object(scope, host_ptr, dispatch_target)?;
    let event_type = normalize_public_event_record(scope, event, target_object.into())?;
    // Keyboard input belongs to its dispatch document even if a listener adopts
    // the target into another document before the default action runs.
    let keyboard_dispatch_document = if event_type == "keydown" {
        let host = unsafe { &*host_ptr };
        match dispatch_target {
            EventTargetHandle::Node(handle) => host.dom_host().owner_document_handle(handle),
            EventTargetHandle::Window => Some(host.document_handle()),
            EventTargetHandle::ChildWindow(window) => {
                host.child_browsing_context_document_handle(window.child_handle())
            }
        }
    } else {
        None
    };

    let bubbles = event_bool_property(scope, event, "bubbles");
    let original_related_target = event_related_target_handle(scope, host_ptr, event);
    let filtered_path;
    let propagation_path = if let Some(related_target) = original_related_target {
        filtered_path = propagation_path_for_related_target(
            host_ptr,
            original_target,
            related_target,
            propagation_path,
        );
        filtered_path.as_slice()
    } else {
        propagation_path
    };
    // Blink's Window event context skips load, so a Node load dispatch ends
    // with no target or relatedTarget.
    let node_load_dispatch =
        matches!(dispatch_target, EventTargetHandle::Node(_)) && event_type == "load";
    let post_dispatch_target = if node_load_dispatch {
        None
    } else {
        post_dispatch_retargeted_target(host_ptr, original_target, propagation_path)
    };
    let post_dispatch_related_target = original_related_target.map(|original_related_target| {
        if node_load_dispatch {
            None
        } else {
            post_dispatch_retargeted_target(host_ptr, original_related_target, propagation_path)
        }
    });
    let invocation_targets_in_shadow_tree = propagation_path
        .iter()
        .copied()
        .filter(|target| invocation_target_is_in_shadow_tree(host_ptr, *target))
        .collect::<Vec<_>>();
    let tree_scopes = PathTreeScopes::build(
        host_ptr,
        original_target,
        original_related_target,
        propagation_path,
    );
    let path_context = PathEventContext {
        event,
        propagation_path,
        tree_scopes: &tree_scopes,
    };

    set_event_internal_flag(scope, event, EVENT_DISPATCHING_SLOT, true);
    // Per DOM spec § "event dispatch": each invocation of the inner-invoke
    // algorithm bails out when the stop-propagation (or stop-immediate-
    // propagation) flag is set. A test that calls stopPropagation() (or
    // assigns cancelBubble = true) BEFORE dispatchEvent() expects the very
    // first phase to short-circuit, so seed `stopped` from the pre-existing
    // flag rather than always starting at false.
    let mut stopped = event_internal_bool_flag(scope, event, EVENT_STOP_PROPAGATION_SLOT)
        || event_internal_bool_flag(scope, event, EVENT_STOP_IMMEDIATE_SLOT);
    let mut last_handled_entry = None;

    if !stopped && propagation_path.len() > 1 {
        for ancestor in propagation_path[1..].iter().rev() {
            if !event_target_is_current(host_ptr, *ancestor) {
                stopped = true;
                break;
            }
            last_handled_entry = Some(*ancestor);
            if !path_entry_has_listeners(
                registry,
                *ancestor,
                &event_type,
                ListenerFilter::Capturing,
            ) {
                continue;
            }
            PathEntryEventState::new(path_context, *ancestor, None, PathEntryPhase::Around(1))
                .apply(scope, host_ptr)?;

            let status = call_event_target_listeners_filtered(
                registry,
                scope,
                host_ptr,
                *ancestor,
                invocation_targets_in_shadow_tree.contains(ancestor),
                &event_type,
                event,
                true,
                false,
            )?;
            if status == DispatchStatus::StopImmediate || status == DispatchStatus::StopPropagation
            {
                stopped = true;
                break;
            }
        }
    }

    if !stopped && !propagation_path.is_empty() {
        last_handled_entry = Some(dispatch_target);
        let target_entry = PathEntryEventState::new(
            path_context,
            dispatch_target,
            Some(target_object),
            PathEntryPhase::AtTarget,
        );
        invoke_event_target_handler_property(
            registry,
            scope,
            host_ptr,
            dispatch_target,
            invocation_targets_in_shadow_tree.contains(&dispatch_target),
            target_object,
            &event_type,
            event,
            &target_entry,
        )?;
        if path_entry_has_listeners(registry, dispatch_target, &event_type, ListenerFilter::All) {
            target_entry.apply(scope, host_ptr)?;
        }

        let stop_immediate = event_internal_bool_flag(scope, event, EVENT_STOP_IMMEDIATE_SLOT);
        let stop_prop = event_internal_bool_flag(scope, event, EVENT_STOP_PROPAGATION_SLOT);
        if !stop_immediate && dispatch_target == EventTargetHandle::Window {
            // A Window dispatches to itself once, in registration order. DOM
            // Node paths retain their capture/bubble and retargeting passes.
            let status = registry.call_listeners_filtered(
                scope,
                host_ptr,
                dispatch_target,
                invocation_targets_in_shadow_tree.contains(&dispatch_target),
                &event_type,
                event,
                ListenerFilter::All,
                true,
            )?;
            stopped = matches!(
                status,
                DispatchStatus::StopPropagation | DispatchStatus::StopImmediate
            );
        } else if !stop_immediate {
            if stop_prop {
                stopped = true;
            }
            let status = call_event_target_listeners_filtered(
                registry,
                scope,
                host_ptr,
                dispatch_target,
                invocation_targets_in_shadow_tree.contains(&dispatch_target),
                &event_type,
                event,
                true,
                true,
            )?;
            if status == DispatchStatus::StopPropagation {
                stopped = true;
            }
            if status == DispatchStatus::StopImmediate {
                stopped = true;
            } else if !stopped {
                let status = call_event_target_listeners_filtered(
                    registry,
                    scope,
                    host_ptr,
                    dispatch_target,
                    invocation_targets_in_shadow_tree.contains(&dispatch_target),
                    &event_type,
                    event,
                    false,
                    true,
                )?;
                if status == DispatchStatus::StopImmediate
                    || status == DispatchStatus::StopPropagation
                {
                    stopped = true;
                }
            }
        } else {
            stopped = true;
        }
    }

    if !stopped && !bubbles && propagation_path.len() > 1 {
        // The hosts a non-bubbling event revisits at target are the entries
        // its target retargeted to when the path was built.
        for (index, ancestor) in propagation_path.iter().enumerate().skip(1) {
            if !event_target_is_current(host_ptr, *ancestor) {
                break;
            }
            if !tree_scopes.is_at_target(index, *ancestor) {
                continue;
            }
            last_handled_entry = Some(*ancestor);
            // Blink's bubbling pass revisits a host the target retargets to
            // with only its non-capture listeners; the capture pass already
            // ran its capture listeners.
            if !path_entry_has_listeners(registry, *ancestor, &event_type, ListenerFilter::Bubbling)
            {
                continue;
            }
            PathEntryEventState::new(path_context, *ancestor, None, PathEntryPhase::AtTarget)
                .apply(scope, host_ptr)?;

            let status = call_event_target_listeners_filtered(
                registry,
                scope,
                host_ptr,
                *ancestor,
                invocation_targets_in_shadow_tree.contains(ancestor),
                &event_type,
                event,
                false,
                true,
            )?;
            if status == DispatchStatus::StopImmediate || status == DispatchStatus::StopPropagation
            {
                stopped = true;
                break;
            }
        }
    }

    if !stopped && bubbles && propagation_path.len() > 1 {
        for ancestor in propagation_path[1..].iter() {
            if !event_target_is_current(host_ptr, *ancestor) {
                break;
            }
            last_handled_entry = Some(*ancestor);
            // HTMLFormElement's local event handling stops a nested submit/reset before the
            // ancestor form's bubble listeners run. Capture still traverses the full path.
            if stops_submit_or_reset_at_ancestor_form(
                host_ptr,
                original_target,
                *ancestor,
                &event_type,
            ) {
                break;
            }
            let ancestor_obj = event_target_object(scope, host_ptr, *ancestor)?;
            let ancestor_entry = PathEntryEventState::new(
                path_context,
                *ancestor,
                Some(ancestor_obj),
                PathEntryPhase::Around(3),
            );

            invoke_event_target_handler_property(
                registry,
                scope,
                host_ptr,
                *ancestor,
                invocation_targets_in_shadow_tree.contains(ancestor),
                ancestor_obj,
                &event_type,
                event,
                &ancestor_entry,
            )?;
            if event_internal_bool_flag(scope, event, EVENT_STOP_IMMEDIATE_SLOT) {
                break;
            }
            if path_entry_has_listeners(registry, *ancestor, &event_type, ListenerFilter::Bubbling)
            {
                ancestor_entry.apply(scope, host_ptr)?;
            }
            let status = call_event_target_listeners_filtered(
                registry,
                scope,
                host_ptr,
                *ancestor,
                invocation_targets_in_shadow_tree.contains(ancestor),
                &event_type,
                event,
                false,
                false,
            )?;
            if status == DispatchStatus::StopImmediate
                || status == DispatchStatus::StopPropagation
                || event_internal_bool_flag(scope, event, EVENT_STOP_PROPAGATION_SLOT)
            {
                break;
            }
        }
    }

    set_event_post_dispatch_targets(
        scope,
        host_ptr,
        event,
        post_dispatch_target,
        post_dispatch_related_target,
    )?;
    if matches!(dispatch_target, EventTargetHandle::Node(_)) {
        record_event_path_final_targets(scope, event);
    }
    // Default actions decide from the dispatched event as it ended, before
    // any script they reach can reinitialize or redispatch it.
    let default_action = DispatchedEventDefaultAction::select(
        scope,
        event,
        &event_type,
        dispatch_target,
        keyboard_dispatch_document,
    );
    // The dispatch ends before any default action runs. Script a default
    // action reaches (a focus handler, a cancel or interest listener) sees no
    // currentTarget, phase or composedPath, whichever entries had listeners.
    set_event_phase(scope, event, 0);
    set_event_internal_flag(scope, event, EVENT_DISPATCHING_SLOT, false);
    set_event_internal_flag(scope, event, EVENT_STOP_PROPAGATION_SLOT, false);
    set_event_internal_flag(scope, event, EVENT_STOP_IMMEDIATE_SLOT, false);
    set_event_internal_flag(scope, event, EVENT_PASSIVE_SLOT, false);
    let current_target_key = v8str(scope, "currentTarget");
    let _ = crate::context_bootstrap::event_backing(scope, event).set(
        scope,
        current_target_key.into(),
        v8::null(scope).into(),
    );
    clear_event_composed_path(scope, event);
    if let Some(default_action) = default_action {
        let restores_targets = default_action.restores_targets_after_perform();
        default_action.perform(scope, host_ptr);
        // Script reached by a keyboard default action can redispatch the
        // event. Like Blink, a Node dispatch ends with the final targets of
        // the event's latest Node path: its own, or a nested Node dispatch's,
        // which rebuilt that path. A plain EventTarget or Window redispatch
        // builds no path, so its target is overwritten. Hover interest is a
        // later task in Blink, so what its listeners redispatch keeps its own
        // targets.
        if restores_targets {
            if matches!(dispatch_target, EventTargetHandle::Node(_)) {
                restore_event_path_final_targets(scope, event);
            } else {
                set_event_post_dispatch_targets(
                    scope,
                    host_ptr,
                    event,
                    post_dispatch_target,
                    post_dispatch_related_target,
                )?;
            }
        }
    }
    Ok(PublicEventDispatchResult {
        default_prevented: event_default_prevented(scope, event),
        canceled_before_dispatch: false,
        last_handled_entry,
    })
}

fn stops_submit_or_reset_at_ancestor_form(
    host_ptr: *mut PageScriptHost,
    original_target: EventTargetHandle,
    current_target: EventTargetHandle,
    event_type: &str,
) -> bool {
    if !matches!(event_type, "submit" | "reset") || current_target == original_target {
        return false;
    }
    let EventTargetHandle::Node(handle) = current_target else {
        return false;
    };
    unsafe { &*host_ptr }
        .dom_host()
        .is_html_element_named(handle, "form")
}

pub(crate) fn dispatch_host_event(
    registry: &mut HostEventTargetRegistry,
    scope: &mut v8::PinScope<'_, '_>,
    host_ptr: *mut PageScriptHost,
    dispatch_target: EventTargetHandle,
    event_target: EventTargetHandle,
    event_type: &str,
    bubbles: bool,
    cancelable: bool,
) -> std::result::Result<(), String> {
    unsafe { &*host_ptr }.debug_assert_not_in_structural_mutation("host event dispatch");
    let current_target_key = v8str(scope, "currentTarget");
    let target_object = event_target_object(scope, host_ptr, dispatch_target)?;
    let target_value = event_target_value(scope, host_ptr, event_target)?;
    let event = create_host_event(
        scope,
        event_type,
        target_object.into(),
        target_value,
        bubbles,
        cancelable,
    )?;

    let invocation_target_in_shadow_tree =
        invocation_target_is_in_shadow_tree(host_ptr, dispatch_target);
    let dispatch_result = run_host_event_dispatch(
        registry,
        scope,
        host_ptr,
        dispatch_target,
        invocation_target_in_shadow_tree,
        target_object,
        event_type,
        event,
    );
    let _ = crate::context_bootstrap::event_backing(scope, event).set(
        scope,
        current_target_key.into(),
        v8::null(scope).into(),
    );
    dispatch_result
}

fn run_host_event_dispatch<'s>(
    registry: &mut HostEventTargetRegistry,
    scope: &mut v8::PinScope<'s, '_>,
    host_ptr: *mut PageScriptHost,
    dispatch_target: EventTargetHandle,
    invocation_target_in_shadow_tree: bool,
    target_object: v8::Local<'s, v8::Object>,
    event_type: &str,
    event: v8::Local<'s, v8::Object>,
) -> std::result::Result<(), String> {
    invoke_event_handler_property(
        registry,
        scope,
        host_ptr,
        dispatch_target,
        invocation_target_in_shadow_tree,
        target_object,
        event_type,
        event,
        None,
    )?;

    if dispatch_target == EventTargetHandle::Window {
        registry.call_listeners_filtered(
            scope,
            host_ptr,
            dispatch_target,
            invocation_target_in_shadow_tree,
            event_type,
            event,
            ListenerFilter::All,
            true,
        )?;
        return Ok(());
    }

    let status = registry.call_listeners_filtered(
        scope,
        host_ptr,
        dispatch_target,
        invocation_target_in_shadow_tree,
        event_type,
        event,
        ListenerFilter::Capturing,
        true,
    )?;
    if status != DispatchStatus::StopImmediate {
        let _ = registry.call_listeners_filtered(
            scope,
            host_ptr,
            dispatch_target,
            invocation_target_in_shadow_tree,
            event_type,
            event,
            ListenerFilter::Bubbling,
            true,
        )?;
    }
    Ok(())
}

fn set_event_current_target(
    scope: &mut v8::PinScope<'_, '_>,
    event: v8::Local<'_, v8::Object>,
    value: v8::Local<'_, v8::Value>,
) {
    let key = v8str(scope, "currentTarget");
    let _ = crate::context_bootstrap::event_backing(scope, event).set(scope, key.into(), value);
}

fn set_event_target(
    scope: &mut v8::PinScope<'_, '_>,
    event: v8::Local<'_, v8::Object>,
    value: v8::Local<'_, v8::Value>,
) {
    let key = v8str(scope, "target");
    let _ = crate::context_bootstrap::event_backing(scope, event).set(scope, key.into(), value);
    let src_element_key = v8str(scope, "srcElement");
    let _ = crate::context_bootstrap::event_backing(scope, event).set(
        scope,
        src_element_key.into(),
        value,
    );
}

fn set_event_phase(scope: &mut v8::PinScope<'_, '_>, event: v8::Local<'_, v8::Object>, phase: u32) {
    let key = v8str(scope, "eventPhase");
    let _ = crate::context_bootstrap::event_backing(scope, event).set(
        scope,
        key.into(),
        v8::Integer::new_from_unsigned(scope, phase).into(),
    );
}

fn set_event_internal_flag(
    scope: &mut v8::PinScope<'_, '_>,
    event: v8::Local<'_, v8::Object>,
    key: &str,
    value: bool,
) {
    crate::context_bootstrap::set_event_private_value(
        scope,
        event,
        key,
        v8::Boolean::new(scope, value).into(),
    );
}

fn event_internal_bool_flag<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    event: v8::Local<'s, v8::Object>,
    key: &str,
) -> bool {
    crate::context_bootstrap::event_private_value(scope, event, key)
        .is_some_and(|value| value.boolean_value(scope))
}

pub(crate) fn event_dispatch_status<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    event: v8::Local<'s, v8::Object>,
) -> DispatchStatus {
    if event_internal_bool_flag(scope, event, EVENT_STOP_IMMEDIATE_SLOT) {
        DispatchStatus::StopImmediate
    } else if event_internal_bool_flag(scope, event, EVENT_STOP_PROPAGATION_SLOT) {
        DispatchStatus::StopPropagation
    } else {
        DispatchStatus::Continue
    }
}

/// Per-spec `(bubbles, cancelable)` defaults for host-dispatched events.
pub(crate) fn host_event_defaults(event_type: &str) -> (bool, bool) {
    match event_type {
        // Events that bubble per spec
        "DOMContentLoaded" => (true, false),
        "readystatechange" => (false, false),
        "scroll" => (true, false),
        "scrollend" => (true, false),
        "visibilitychange" => (true, false),
        // Non-bubbling events (default)
        "load" | "error" | "abort" | "unload" | "beforeunload" => (false, false),
        _ => (false, false),
    }
}

pub(crate) fn create_host_event<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    event_type: &str,
    current_target: v8::Local<'s, v8::Value>,
    target: v8::Local<'s, v8::Value>,
    bubbles: bool,
    cancelable: bool,
) -> std::result::Result<v8::Local<'s, v8::Object>, String> {
    // The intrinsic constructor owns the complete Event initialization and
    // prototype surface. Host dispatch only adds browser-controlled trust and
    // target state; installing methods or accessors here would incorrectly
    // turn prototype members into per-event own properties.
    let event = construct_event_instance(scope, event_type, bubbles, cancelable)?;
    mark_event_trusted(scope, event);
    set_event_target(scope, event, target);
    set_event_current_target(scope, event, current_target);
    clear_event_composed_path(scope, event);

    Ok(event)
}

fn construct_event_instance<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    event_type: &str,
    bubbles: bool,
    cancelable: bool,
) -> std::result::Result<v8::Local<'s, v8::Object>, String> {
    let constructor =
        crate::context_bootstrap::ensure_intrinsic_interface_constructor(scope, "Event")
            .map_err(|error| format!("failed to resolve intrinsic Event constructor: {error}"))?;
    let init = HostEventInitDeclaration::new(bubbles, cancelable)
        .bind(scope)
        .map_err(|error| format!("failed to build EventInit dictionary: {error}"))?;
    let event_type = v8_string(scope, event_type)
        .ok_or_else(|| format!("failed to allocate event type `{event_type}`"))?;
    let try_catch = std::pin::pin!(v8::TryCatch::new(scope));
    let scope = try_catch.init();
    constructor
        .new_instance(&scope, &[event_type.into(), init.into()])
        .ok_or_else(|| "intrinsic Event constructor failed for a host event".to_owned())
}

fn normalize_public_event_record<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    event: v8::Local<'s, v8::Object>,
    default_target: v8::Local<'s, v8::Value>,
) -> std::result::Result<String, String> {
    let type_key = v8str(scope, "type");
    let target_key = v8str(scope, "target");
    let current_target_key = v8str(scope, "currentTarget");

    let event_type_value = crate::context_bootstrap::event_backing(scope, event)
        .get(scope, type_key.into())
        .ok_or_else(|| "Failed to execute 'dispatchEvent': event type is required.".to_owned())?;
    if event_type_value.is_null_or_undefined() {
        return Err("Failed to execute 'dispatchEvent': event type is required.".to_owned());
    }
    let event_type = event_type_value
        .to_string(scope)
        .map(|value| value.to_rust_string_lossy(scope))
        .unwrap_or_default();

    let target_value =
        crate::context_bootstrap::event_backing(scope, event).get(scope, target_key.into());
    if target_value.is_none() || target_value.is_some_and(|value| value.is_null_or_undefined()) {
        let _ = crate::context_bootstrap::event_backing(scope, event).set(
            scope,
            target_key.into(),
            default_target,
        );
        let _ = crate::context_bootstrap::event_backing(scope, event).set(
            scope,
            v8str(scope, "srcElement").into(),
            default_target,
        );
    }
    let _ = crate::context_bootstrap::event_backing(scope, event).set(
        scope,
        current_target_key.into(),
        default_target,
    );
    // `dispatchEvent()` brand-checks the initialized Event private slot before
    // reaching this function. Its methods and legacy accessors therefore come
    // from the intrinsic Event prototype and must not be shadowed here.
    clear_event_composed_path(scope, event);

    Ok(event_type)
}

fn event_default_prevented(
    scope: &mut v8::PinScope<'_, '_>,
    event: v8::Local<'_, v8::Object>,
) -> bool {
    let default_prevented_key = v8str(scope, "defaultPrevented");
    crate::context_bootstrap::event_backing(scope, event)
        .get(scope, default_prevented_key.into())
        .is_some_and(|value| value.boolean_value(scope))
}

fn event_target_object<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    host_ptr: *mut PageScriptHost,
    target: EventTargetHandle,
) -> std::result::Result<v8::Local<'s, v8::Object>, String> {
    match target {
        EventTargetHandle::Window => Ok(scope.get_current_context().global(scope)),
        EventTargetHandle::ChildWindow(target) => unsafe { &mut *host_ptr }
            .child_window_event_target_wrapper(scope, target)
            .ok_or_else(|| format!("stale child Window event target `{target:?}`")),
        EventTargetHandle::Node(handle) => {
            let wrapper = unsafe { &mut *host_ptr }
                .native_bridge_mut()
                .wrap_handle(scope, host_ptr, handle)
                .ok_or_else(|| format!("failed to resolve event target wrapper `{handle:?}`"))?;
            Ok(get_private_object(scope, wrapper, DOM_PARSER_FOREIGN_NODE_SLOT).unwrap_or(wrapper))
        }
    }
}

pub(crate) fn event_target_value<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    host_ptr: *mut PageScriptHost,
    target: EventTargetHandle,
) -> std::result::Result<v8::Local<'s, v8::Value>, String> {
    Ok(match target {
        EventTargetHandle::Window => scope.get_current_context().global(scope).into(),
        EventTargetHandle::ChildWindow(target) => {
            event_target_object(scope, host_ptr, EventTargetHandle::ChildWindow(target))?.into()
        }
        EventTargetHandle::Node(handle) => {
            event_target_object(scope, host_ptr, EventTargetHandle::Node(handle))?.into()
        }
    })
}

fn event_target_receiver<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    host_ptr: *mut PageScriptHost,
    target: EventTargetHandle,
    event: v8::Local<'s, v8::Object>,
) -> v8::Local<'s, v8::Value> {
    match target {
        EventTargetHandle::Window => scope.get_current_context().global(scope).into(),
        EventTargetHandle::ChildWindow(target) => unsafe { &mut *host_ptr }
            .child_window_event_target_wrapper(scope, target)
            .map(Into::into)
            .unwrap_or_else(|| v8::undefined(scope).into()),
        EventTargetHandle::Node(_) => {
            let key = v8str(scope, "currentTarget");
            crate::context_bootstrap::event_backing(scope, event)
                .get(scope, key.into())
                .unwrap_or_else(|| v8::undefined(scope).into())
        }
    }
}

fn set_event_related_target_for_current_target<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    host_ptr: *mut PageScriptHost,
    event: v8::Local<'s, v8::Object>,
    related_target: Option<EventTargetHandle>,
) -> std::result::Result<(), String> {
    let Some(related_target) = related_target else {
        return Ok(());
    };
    let related_value = event_target_value(scope, host_ptr, related_target)?;
    let _ = crate::context_bootstrap::event_backing(scope, event).set(
        scope,
        v8str(scope, "relatedTarget").into(),
        related_value,
    );
    Ok(())
}

fn set_event_composed_path_for_current_target<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    host_ptr: *mut PageScriptHost,
    event: v8::Local<'s, v8::Object>,
    propagation_path: &[EventTargetHandle],
    visible_path: &[EventTargetHandle],
) -> std::result::Result<(), String> {
    let path = visible_path
        .iter()
        .copied()
        .map(|handle| match handle {
            EventTargetHandle::Window => Ok(event_window_value_for_path(
                scope,
                host_ptr,
                propagation_path,
            )),
            EventTargetHandle::ChildWindow(_) => event_target_value(scope, host_ptr, handle),
            _ => event_target_value(scope, host_ptr, handle),
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let path = serialize_v8_iter_array(scope, path).unwrap_or_else(|| v8::Array::new(scope, 0));
    set_event_composed_path(scope, event, path);
    Ok(())
}

fn event_window_value_for_path<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    host_ptr: *mut PageScriptHost,
    propagation_path: &[EventTargetHandle],
) -> v8::Local<'s, v8::Value> {
    let document_handle = {
        let runtime = unsafe { &*host_ptr };
        propagation_path.iter().find_map(|entry| {
            let EventTargetHandle::Node(handle) = entry else {
                return None;
            };
            runtime
                .dom_host()
                .node(*handle)
                .is_some_and(moli_dom::native::Node::is_document)
                .then_some(*handle)
        })
    };
    if let Some(document_handle) = document_handle {
        let runtime = unsafe { &mut *host_ptr };
        if let Some(child_handle) =
            runtime.child_browsing_context_handle_by_document_handle(scope, document_handle)
            && let Some(window) = runtime.child_browsing_context_window_wrapper(scope, child_handle)
        {
            return window.into();
        }
    }
    scope.get_current_context().global(scope).into()
}

fn set_event_post_dispatch_targets<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    host_ptr: *mut PageScriptHost,
    event: v8::Local<'s, v8::Object>,
    post_dispatch_target: Option<EventTargetHandle>,
    post_dispatch_related_target: Option<Option<EventTargetHandle>>,
) -> std::result::Result<(), String> {
    let target_value = match post_dispatch_target {
        Some(target) => event_target_value(scope, host_ptr, target)?,
        None => v8::null(scope).into(),
    };
    set_event_target(scope, event, target_value);

    if let Some(related_target) = post_dispatch_related_target {
        let related_value = match related_target {
            Some(related_target) => event_target_value(scope, host_ptr, related_target)?,
            None => v8::null(scope).into(),
        };
        let _ = crate::context_bootstrap::event_backing(scope, event).set(
            scope,
            v8str(scope, "relatedTarget").into(),
            related_value,
        );
    }

    Ok(())
}

/// Private slots for the target and relatedTarget a Node dispatch leaves on
/// the event, Blink's final read of the event path's Window context. They
/// stay off every world's wrapper.
const EVENT_PATH_FINAL_TARGET_SLOT: &str = "__moliEventPathFinalTarget";
const EVENT_PATH_FINAL_RELATED_TARGET_SLOT: &str = "__moliEventPathFinalRelatedTarget";

fn record_event_path_final_targets(
    scope: &mut v8::PinScope<'_, '_>,
    event: v8::Local<'_, v8::Object>,
) {
    let backing = crate::context_bootstrap::event_backing(scope, event);
    let target = backing
        .get(scope, v8str(scope, "target").into())
        .filter(|value| !value.is_undefined())
        .unwrap_or_else(|| v8::null(scope).into());
    crate::context_bootstrap::set_event_private_value(
        scope,
        event,
        EVENT_PATH_FINAL_TARGET_SLOT,
        target,
    );
    // Only an event with a relatedTarget attribute keeps one; undefined
    // leaves the slot unset, so the restore adds no relatedTarget field.
    let related_key = v8str(scope, "relatedTarget");
    let related_target = if backing.has_own_property(scope, related_key.into()) == Some(true) {
        backing
            .get(scope, related_key.into())
            .unwrap_or_else(|| v8::undefined(scope).into())
    } else {
        v8::undefined(scope).into()
    };
    crate::context_bootstrap::set_event_private_value(
        scope,
        event,
        EVENT_PATH_FINAL_RELATED_TARGET_SLOT,
        related_target,
    );
}

fn restore_event_path_final_targets(
    scope: &mut v8::PinScope<'_, '_>,
    event: v8::Local<'_, v8::Object>,
) {
    let target =
        crate::context_bootstrap::event_private_value(scope, event, EVENT_PATH_FINAL_TARGET_SLOT)
            .unwrap_or_else(|| v8::null(scope).into());
    set_event_target(scope, event, target);
    if let Some(related_target) = crate::context_bootstrap::event_private_value(
        scope,
        event,
        EVENT_PATH_FINAL_RELATED_TARGET_SLOT,
    ) {
        let _ = crate::context_bootstrap::event_backing(scope, event).set(
            scope,
            v8str(scope, "relatedTarget").into(),
            related_target,
        );
    }
}

fn post_dispatch_retargeted_target(
    host_ptr: *mut PageScriptHost,
    original_target: EventTargetHandle,
    propagation_path: &[EventTargetHandle],
) -> Option<EventTargetHandle> {
    let current_target = *propagation_path.last()?;
    let runtime = unsafe { &*host_ptr };
    if matches!(
        current_target,
        EventTargetHandle::Node(handle) if runtime.dom_host().is_shadow_root(handle)
    ) {
        return None;
    }
    Some(retarget_event_target(
        host_ptr,
        original_target,
        current_target,
    ))
}

fn invocation_target_is_in_shadow_tree(
    host_ptr: *mut PageScriptHost,
    target: EventTargetHandle,
) -> bool {
    let EventTargetHandle::Node(handle) = target else {
        return false;
    };
    let runtime = unsafe { &*host_ptr };
    runtime.dom_host().is_shadow_root(handle)
        || runtime.dom_host().containing_shadow_root(handle).is_some()
}

fn event_related_target_handle(
    scope: &mut v8::PinScope<'_, '_>,
    host_ptr: *mut PageScriptHost,
    event: v8::Local<'_, v8::Object>,
) -> Option<EventTargetHandle> {
    let value = crate::context_bootstrap::event_backing(scope, event)
        .get(scope, v8str(scope, "relatedTarget").into())?;
    if value.is_null_or_undefined() || !value.is_object() {
        return None;
    }
    let object = v8::Local::<v8::Object>::try_from(value).ok()?;
    if let Ok((runtime_ptr, handle)) =
        crate::native_bridge::node_runtime_and_handle_from_object(scope, object)
        && runtime_ptr == host_ptr
    {
        return Some(EventTargetHandle::Node(handle));
    }
    crate::native_bridge::document::detached_native_handle_for_runtime(scope, host_ptr, object)
        .map(EventTargetHandle::Node)
}

/// An event's `source` (CommandEvent, ToggleEvent, InterestEvent) as its
/// getter returns it: the stored source node retargeted against the event's
/// currentTarget, or against the document when there is none. The stored
/// value stays the original, so every read and every later dispatch sees the
/// tree as it is then. A value that is not a Node is returned unchanged.
pub(crate) fn retargeted_event_source<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    event: v8::Local<'s, v8::Object>,
    source: v8::Local<'s, v8::Value>,
) -> v8::Local<'s, v8::Value> {
    let Ok(object) = v8::Local::<v8::Object>::try_from(source) else {
        return source;
    };
    let Ok((host_ptr, handle)) =
        crate::native_bridge::node_runtime_and_handle_from_object_or_detached(scope, object)
    else {
        return source;
    };
    let current_target = crate::context_bootstrap::event_backing(scope, event)
        .get(scope, v8str(scope, "currentTarget").into())
        .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())
        // Only a Node currentTarget has a tree to retarget against. The brand
        // check reads no author-visible state; another EventTarget (an XHR, a
        // plain EventTarget) takes the document path.
        .filter(|object| crate::web_api_interfaces::Node::is_instance(scope, *object))
        .and_then(|object| {
            crate::native_bridge::node_runtime_and_handle_from_object_or_detached(scope, object)
                .ok()
        })
        .filter(|(runtime_ptr, _)| *runtime_ptr == host_ptr)
        .map_or(EventTargetHandle::Window, |(_, current)| {
            EventTargetHandle::Node(current)
        });
    let source_target = EventTargetHandle::Node(handle);
    let retargeted = retarget_event_target(host_ptr, source_target, current_target);
    if retargeted == source_target {
        return source;
    }
    // The wrapper script already holds belongs to the source's realm, which
    // can outlive its window (a closed popup), so look there first. Handles
    // are Page-local and that realm's cache can hold another Page's nodes, so
    // only a wrapper whose native node is the retargeted one is returned.
    if let EventTargetHandle::Node(retargeted_handle) = retargeted
        && let Some(creation_context) = object.get_creation_context(scope)
    {
        let cached = {
            let scope = &mut v8::ContextScope::new(scope, creation_context);
            unsafe { &*host_ptr }
                .native_bridge()
                .cached_handle_wrapper(scope, retargeted_handle)
                .map(|wrapper| {
                    get_private_object(scope, wrapper, DOM_PARSER_FOREIGN_NODE_SLOT)
                        .unwrap_or(wrapper)
                })
                .filter(|wrapper| {
                    crate::native_bridge::node_runtime_and_handle_from_object_or_detached(
                        scope, *wrapper,
                    )
                    .is_ok_and(|node| node == (host_ptr, retargeted_handle))
                })
                .map(|wrapper| v8::Global::new(scope, wrapper))
        };
        if let Some(wrapper) = cached {
            return v8::Local::new(scope, &wrapper).into();
        }
    }
    // Never hand script the unretargeted node, which can be inside a closed
    // shadow tree. A host whose realm is gone has no wrapper to return.
    event_target_value(scope, host_ptr, retargeted).unwrap_or_else(|_| v8::null(scope).into())
}

/// The one default action a dispatched event selects, from its state when
/// the dispatch ended.
enum DispatchedEventDefaultAction {
    HoverInterest(NativeNodeId),
    Keyboard(crate::native_bridge::element::KeyboardDefaultAction),
}

impl DispatchedEventDefaultAction {
    fn select(
        scope: &mut v8::PinScope<'_, '_>,
        event: v8::Local<'_, v8::Object>,
        event_type: &str,
        dispatch_target: EventTargetHandle,
        keyboard_dispatch_document: Option<NativeNodeId>,
    ) -> Option<Self> {
        if !event_bool_property(scope, event, "isTrusted") || event_default_prevented(scope, event)
        {
            return None;
        }
        match event_type {
            "mouseover" => match dispatch_target {
                EventTargetHandle::Node(handle) => Some(Self::HoverInterest(handle)),
                _ => None,
            },
            "keydown" => crate::native_bridge::element::KeyboardDefaultAction::select(
                scope,
                event,
                keyboard_dispatch_document,
            )
            .map(Self::Keyboard),
            _ => None,
        }
    }

    /// Whether the dispatch restores its final targets after the action.
    /// Hover interest is a separate task in Blink (PF-9505 owns its delay),
    /// so its listeners run after the dispatch has already ended.
    fn restores_targets_after_perform(&self) -> bool {
        matches!(self, Self::Keyboard(_))
    }

    fn perform(self, scope: &mut v8::PinScope<'_, '_>, host_ptr: *mut PageScriptHost) {
        match self {
            Self::HoverInterest(handle) => {
                crate::native_bridge::element::perform_hover_interest_default_action_for_dispatched_event(
                    scope, host_ptr, handle,
                );
            }
            Self::Keyboard(action) => action.perform(scope, host_ptr),
        }
    }
}

fn propagation_path_for_related_target(
    host_ptr: *mut PageScriptHost,
    original_target: EventTargetHandle,
    original_related_target: EventTargetHandle,
    propagation_path: &[EventTargetHandle],
) -> Vec<EventTargetHandle> {
    let mut path = Vec::new();
    for &current_target in propagation_path {
        let target = retarget_event_target(host_ptr, original_target, current_target);
        let related_target =
            retarget_event_target(host_ptr, original_related_target, current_target);
        if target == related_target
            && retargets_are_hidden_by_shadow_boundary(
                host_ptr,
                original_target,
                original_related_target,
                current_target,
            )
        {
            continue;
        }
        path.push(current_target);
    }
    path
}

fn retargets_are_hidden_by_shadow_boundary(
    host_ptr: *mut PageScriptHost,
    original_target: EventTargetHandle,
    original_related_target: EventTargetHandle,
    current_target: EventTargetHandle,
) -> bool {
    [original_target, original_related_target]
        .into_iter()
        .any(|target| target_has_inaccessible_shadow_root(host_ptr, target, current_target))
}

fn target_has_inaccessible_shadow_root(
    host_ptr: *mut PageScriptHost,
    target: EventTargetHandle,
    current_target: EventTargetHandle,
) -> bool {
    let EventTargetHandle::Node(handle) = target else {
        return false;
    };
    let runtime = unsafe { &*host_ptr };
    let mut current = handle;
    loop {
        let Some(root) = runtime.dom_host().containing_shadow_root(current) else {
            return false;
        };
        if !current_target_can_access_shadow_root(host_ptr, current_target, root) {
            return true;
        }
        let Some(host) = runtime.dom_host().shadow_root_host(root) else {
            return false;
        };
        current = host;
    }
}

pub(crate) fn retarget_event_target(
    host_ptr: *mut PageScriptHost,
    original_target: EventTargetHandle,
    current_target: EventTargetHandle,
) -> EventTargetHandle {
    let EventTargetHandle::Node(mut candidate) = original_target else {
        return original_target;
    };
    let runtime = unsafe { &*host_ptr };
    loop {
        let Some(shadow_root) = runtime.dom_host().containing_shadow_root(candidate) else {
            return EventTargetHandle::Node(candidate);
        };
        if current_target_can_access_shadow_root(host_ptr, current_target, shadow_root) {
            return EventTargetHandle::Node(candidate);
        }
        let Some(host) = runtime.dom_host().shadow_root_host(shadow_root) else {
            return EventTargetHandle::Node(candidate);
        };
        candidate = host;
    }
}

fn current_target_can_access_shadow_root(
    host_ptr: *mut PageScriptHost,
    current_target: EventTargetHandle,
    shadow_root: NativeNodeId,
) -> bool {
    let runtime = unsafe { &*host_ptr };
    match current_target {
        EventTargetHandle::Window | EventTargetHandle::ChildWindow(_) => false,
        EventTargetHandle::Node(handle) => {
            shadow_including_tree_contains(runtime.dom_host(), shadow_root, handle)
        }
    }
}

fn shadow_including_tree_contains(
    dom_host: &moli_dom::native::DomHost,
    root: NativeNodeId,
    handle: NativeNodeId,
) -> bool {
    let mut current = handle;
    loop {
        if current == root {
            return true;
        }
        if let Some(parent) = dom_host
            .node(current)
            .and_then(moli_dom::native::Node::parent_node)
        {
            current = parent;
            continue;
        }
        if dom_host.is_shadow_root(current)
            && let Some(host) = dom_host.shadow_root_host(current)
        {
            current = host;
            continue;
        }
        return false;
    }
}

fn event_bool_property(
    scope: &mut v8::PinScope<'_, '_>,
    event: v8::Local<'_, v8::Object>,
    key: &str,
) -> bool {
    let Some(key) = v8_string(scope, key) else {
        return false;
    };
    crate::context_bootstrap::event_backing(scope, event)
        .get(scope, key.into())
        .is_some_and(|value| value.boolean_value(scope))
}
