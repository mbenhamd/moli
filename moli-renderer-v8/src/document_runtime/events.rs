use super::*;
use crate::host::HostTimerOwner;
use crate::host::PublicEventDispatchResult;
use crate::script_provenance::CompiledStringProvenance;

// This module is the first extraction slice from `document_runtime.rs`.
//
// The goal of this move is intentionally narrow: pull event delivery, public dispatch
// plumbing, and timer queue entrypoints into one place without changing ownership of
// `DocumentRuntime.events` or `DocumentRuntime.timeouts`, and without renaming the public
// methods that the rest of the runtime already calls.
//
// In other words, this is not yet a full "event runtime" abstraction. It is a boundary-
// shaping step that makes the next refactors cheaper:
// - callers still talk to `DocumentRuntime`
// - behavior remains anchored in the same state objects
// - but the code is no longer mixed into unrelated DOM / lifecycle / script logic
//
// That keeps this commit structural rather than semantic, which is the safest way to start
// shrinking `document_runtime.rs`.
impl DocumentRuntime {
    /// A reused Window and retained nodes keep their existing EventTargets.
    /// The new Document has a fresh handle and receives no old listeners.
    pub(crate) fn retain_event_targets_from_initial_document(&mut self, initial: &mut Self) {
        self.events = std::mem::take(&mut initial.events);
        // Timers belong to the retained LocalWindow and concrete realm. Child
        // and isolated-realm timers have already been canceled by retirement.
        self.timeouts = std::mem::take(&mut initial.timeouts);
    }

    pub(crate) fn clear_event_state_for_document_replacement(
        &mut self,
        document_handle: DomHandle,
        clear_window: bool,
    ) -> std::collections::HashSet<crate::native_bridge::EventCallbackId> {
        // `Document::open()` clears the shadow-including current document tree,
        // the Document, and its Window. A node that was already detached is not
        // reached by that traversal and keeps its listeners.
        let dom_host = &self.dom_host;
        self.events.clear_targets_matching(|target| match target {
            EventTargetHandle::Window => clear_window,
            EventTargetHandle::ChildWindow(_) => clear_window,
            EventTargetHandle::Node(handle) => {
                handle == document_handle
                    || (dom_host.owner_document_handle(handle) == Some(document_handle)
                        && dom_host.is_connected(handle))
            }
        })
    }

    fn dispatch_error_already_reported(message: &str) -> bool {
        message.starts_with("event handler `") || message.starts_with("callback `")
    }

    pub(crate) fn dispatch_public_event_best_effort<'s, 'i>(
        &mut self,
        scope: &mut v8::PinScope<'s, 'i>,
        host_ptr: *mut PageScriptHost,
        dispatch_target: EventTargetHandle,
        event: v8::Local<'s, v8::Object>,
        context: &str,
    ) -> std::result::Result<PublicEventDispatchResult, String> {
        match self.dispatch_public_event(scope, host_ptr, dispatch_target, event) {
            Ok(dispatched) => Ok(dispatched),
            Err(message) => {
                if !Self::dispatch_error_already_reported(&message) {
                    tracing::error!(
                        context,
                        message = message.as_str(),
                        "public event dispatch failed"
                    );
                }
                Err(message)
            }
        }
    }

    pub(crate) fn queue_timeout<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        callback: v8::Local<'s, v8::Function>,
        delay_ms: u64,
        owner: HostTimerOwner,
        extra_args: Vec<v8::Global<v8::Value>>,
    ) -> u32 {
        self.timeouts
            .queue_once(scope, callback, delay_ms, owner, extra_args)
    }

    pub(crate) fn queue_internal_task<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        callback: v8::Local<'s, v8::Function>,
    ) -> bool {
        self.queue_internal_task_after(scope, callback, 0)
    }

    pub(crate) fn queue_internal_task_after<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        callback: v8::Local<'s, v8::Function>,
        delay_ms: u64,
    ) -> bool {
        self.timeouts
            .queue_internal_task_after(scope, callback, delay_ms)
    }

    pub(crate) fn queue_timeout_with_receiver<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        callback: v8::Local<'s, v8::Function>,
        receiver: v8::Local<'s, v8::Object>,
        delay_ms: u32,
        owner: HostTimerOwner,
        extra_args: Vec<v8::Global<v8::Value>>,
    ) -> u32 {
        self.timeouts.queue_once_with_receiver(
            scope,
            callback,
            receiver,
            u64::from(delay_ms),
            owner,
            extra_args,
        )
    }

    pub(crate) fn queue_window_timer_callback<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        callback: moli_webidl_callback::WebIdlCallbackFunction,
        target_receiver: v8::Local<'s, v8::Object>,
        delay_ms: u32,
        owner: HostTimerOwner,
        extra_args: Vec<v8::Global<v8::Value>>,
    ) -> u32 {
        self.timeouts.queue_window_timer_callback(
            scope,
            callback,
            target_receiver,
            delay_ms,
            owner,
            extra_args,
        )
    }

    pub(crate) fn queue_window_timer_callback_interval<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        callback: moli_webidl_callback::WebIdlCallbackFunction,
        target_receiver: v8::Local<'s, v8::Object>,
        delay_ms: u32,
        owner: HostTimerOwner,
        extra_args: Vec<v8::Global<v8::Value>>,
    ) -> u32 {
        self.timeouts.queue_window_timer_callback_interval(
            scope,
            callback,
            target_receiver,
            delay_ms,
            owner,
            extra_args,
        )
    }

    pub(crate) fn queue_window_animation_frame_callback<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        callback: moli_webidl_callback::WebIdlCallbackFunction,
        target_receiver: v8::Local<'s, v8::Object>,
        timestamp: f64,
        delay_ms: u32,
        overdue: std::time::Duration,
        owner: HostTimerOwner,
    ) -> u32 {
        self.timeouts.queue_window_animation_frame_callback(
            scope,
            callback,
            target_receiver,
            timestamp,
            delay_ms,
            overdue,
            owner,
        )
    }

    pub(crate) fn has_pending_animation_frame(&self) -> bool {
        self.timeouts.has_pending_animation_frame()
    }

    pub(crate) fn queue_window_idle_callback<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        callback: moli_webidl_callback::WebIdlCallbackFunction,
        target_receiver: v8::Local<'s, v8::Object>,
        timeout_deadline_ms: f64,
        delay_ms: u32,
        owner: HostTimerOwner,
    ) -> u32 {
        self.timeouts.queue_window_idle_callback(
            scope,
            callback,
            target_receiver,
            timeout_deadline_ms,
            delay_ms,
            owner,
        )
    }

    pub(crate) fn queue_window_geolocation_callback<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        callback: moli_webidl_callback::WebIdlCallbackFunction,
        geolocation: v8::Local<'s, v8::Object>,
        result: v8::Global<v8::Value>,
        owner: HostTimerOwner,
        watch_id: Option<i32>,
    ) -> u32 {
        self.timeouts.queue_window_geolocation_callback(
            scope,
            callback,
            geolocation,
            result,
            owner,
            watch_id,
        )
    }

    pub(crate) fn queue_source_timeout_with_receiver<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        context: v8::Local<'s, v8::Context>,
        receiver: v8::Local<'s, v8::Object>,
        source: String,
        provenance: CompiledStringProvenance,
        delay_ms: u32,
        owner: HostTimerOwner,
        extra_args: Vec<v8::Global<v8::Value>>,
    ) -> u32 {
        self.timeouts.queue_source_once_with_receiver(
            scope, context, receiver, source, provenance, delay_ms, owner, extra_args,
        )
    }

    pub(crate) fn queue_source_interval_with_receiver<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        context: v8::Local<'s, v8::Context>,
        receiver: v8::Local<'s, v8::Object>,
        source: String,
        provenance: CompiledStringProvenance,
        delay_ms: u32,
        owner: HostTimerOwner,
        extra_args: Vec<v8::Global<v8::Value>>,
    ) -> u32 {
        self.timeouts.queue_source_interval_with_receiver(
            scope, context, receiver, source, provenance, delay_ms, owner, extra_args,
        )
    }

    pub(crate) fn queue_networking_task<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        callback: v8::Local<'s, v8::Function>,
    ) -> u32 {
        self.timeouts.queue_networking_task(scope, callback)
    }

    pub(crate) fn queue_resource_timing_buffer_full_task<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        context: v8::Local<'s, v8::Context>,
        performance: v8::Local<'s, v8::Object>,
        buffer_id: crate::native_bridge::ResourceTimingBufferId,
    ) -> u32 {
        self.timeouts
            .queue_resource_timing_buffer_full(scope, context, performance, buffer_id)
    }

    pub(crate) fn cancel_timer(&mut self, id: u32) {
        self.timeouts.cancel(id);
    }

    pub(crate) fn cancel_window_timer_for_receiver<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        receiver: v8::Local<'s, v8::Object>,
        id: u32,
    ) -> bool {
        self.timeouts
            .cancel_window_timer_for_receiver(scope, receiver, id)
    }

    pub(crate) fn cancel_geolocation_watch<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        geolocation: v8::Local<'s, v8::Object>,
        watch_id: i32,
    ) -> bool {
        self.timeouts
            .cancel_geolocation_watch(scope, geolocation, watch_id)
    }

    pub(crate) fn cancel_window_execution_context_timers(
        &mut self,
        owner: crate::native_bridge::WindowExecutionContextOwner,
    ) -> usize {
        self.timeouts.cancel_window_execution_context_timers(owner)
    }

    pub(crate) fn cancel_timers_for_context_token(
        &mut self,
        context_token: crate::native_bridge::RuntimeObservableContextToken,
    ) -> usize {
        self.timeouts.cancel_timers_for_context_token(context_token)
    }

    pub(crate) fn run_next_timeout_body(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
        selection: crate::page_task_queue::RendererPageTimerSelection,
    ) -> HostTimeoutRunResult {
        self.timeouts.run_next_body(scope, selection)
    }

    #[cfg(test)]
    pub(crate) fn has_ready_timeout(&self) -> bool {
        self.timeouts.has_ready_timer()
    }

    pub(crate) fn next_ready_timeout_deadline(
        &self,
        selection: crate::page_task_queue::RendererPageTimerSelection,
    ) -> Option<std::time::Instant> {
        self.timeouts.next_ready_timer_deadline(selection)
    }

    pub(crate) fn has_ready_animation_frame_callback(&self) -> bool {
        self.timeouts.has_ready_animation_frame_callback()
    }

    pub(crate) fn ready_animation_frame_deadline(&self) -> Option<std::time::Instant> {
        self.timeouts.ready_animation_frame_deadline()
    }

    pub(crate) fn ms_to_next_timeout(&self) -> Option<u64> {
        self.timeouts.ms_to_next()
    }

    pub(crate) fn has_keep_alive_timers(&self) -> bool {
        self.timeouts.has_keep_alive_timers()
    }

    pub(crate) fn next_timeout_deadline(&self) -> Option<std::time::Instant> {
        self.timeouts.next_deadline()
    }

    pub(crate) fn event_listener_callback_ids(
        &self,
        target: EventTargetHandle,
        event_type: &str,
        capture: bool,
    ) -> Vec<crate::native_bridge::EventCallbackId> {
        self.events
            .listener_callback_ids(target, event_type, capture)
    }

    #[cfg(feature = "devtools")]
    pub(crate) fn inspector_event_listener_snapshots(
        &self,
        target: EventTargetHandle,
    ) -> Vec<crate::host::EventListenerInspectorSnapshot> {
        self.events.inspector_listener_snapshots(target)
    }

    pub(crate) fn insert_event_listener(
        &mut self,
        target: EventTargetHandle,
        event_type: &str,
        registration: crate::host::EventListenerRegistration,
    ) {
        self.events
            .insert_listener(target, event_type, registration);
    }

    pub(crate) fn remove_event_listener_by_id(
        &mut self,
        target: EventTargetHandle,
        event_type: &str,
        callback_id: crate::native_bridge::EventCallbackId,
        capture: bool,
    ) -> bool {
        self.events
            .remove_listener_by_id(target, event_type, callback_id, capture)
    }

    pub(crate) fn remove_event_callback_registrations(
        &mut self,
        retired: &std::collections::HashSet<crate::native_bridge::EventCallbackId>,
    ) {
        self.events.remove_callback_registrations(retired);
    }

    pub(crate) fn set_event_handler_property(
        &mut self,
        target: EventTargetHandle,
        event_type: &str,
        callback_id: Option<crate::native_bridge::EventCallbackId>,
    ) -> Option<crate::native_bridge::EventCallbackId> {
        self.events
            .set_event_handler_property(target, event_type, callback_id)
    }

    pub(crate) fn clear_event_handler_property(
        &mut self,
        target: EventTargetHandle,
        event_type: &str,
    ) -> Option<crate::native_bridge::EventCallbackId> {
        self.events.clear_event_handler_property(target, event_type)
    }

    pub(crate) fn event_handler_property_callback_id(
        &self,
        target: EventTargetHandle,
        event_type: &str,
    ) -> Option<Option<crate::native_bridge::EventCallbackId>> {
        self.events
            .event_handler_property_callback_id(target, event_type)
    }

    pub(crate) fn has_event_listener(&self, target: EventTargetHandle, event_type: &str) -> bool {
        self.events.has_listener(target, event_type)
    }

    pub(crate) fn dispatch_document_event(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
        host_ptr: *mut PageScriptHost,
        event_type: &str,
    ) -> std::result::Result<(), String> {
        let target = EventTargetHandle::Node(self.document_handle());
        let event = self.prepare_native_event_dispatch(target, target, event_type);
        self.dispatch_native_event_in_scope(scope, host_ptr, event)
    }

    pub(crate) fn dispatch_public_event<'s, 'i>(
        &mut self,
        scope: &mut v8::PinScope<'s, 'i>,
        host_ptr: *mut PageScriptHost,
        dispatch_target: EventTargetHandle,
        event: v8::Local<'s, v8::Object>,
    ) -> std::result::Result<PublicEventDispatchResult, String> {
        let event_type = public_event_type(scope, event);
        let composed = crate::context_bootstrap::event_backing(scope, event)
            .get(scope, v8str(scope, "composed").into())
            .is_some_and(|value| value.boolean_value(scope));
        let reference_related_target = reference_related_target_for_event(scope, host_ptr, event);
        let (mut path, entry_related_targets) = match reference_related_target {
            Some(Some(related_target)) => {
                // Blink composes the source-bearing events it fires itself
                // (popover toggles, command, interest, submit) when it creates
                // them, so a redispatch keeps that path too.
                let source_composed = crate::context_bootstrap::event_private_value(
                    scope,
                    event,
                    EVENT_SOURCE_COMPOSED_SLOT,
                )
                .is_some_and(|value| value.boolean_value(scope));
                self.build_reference_target_propagation_path(
                    host_ptr,
                    dispatch_target,
                    related_target,
                    composed || source_composed,
                )
            }
            Some(None) | None => (self.build_propagation_path(dispatch_target, composed), None),
        };
        self.append_child_document_window_to_path(scope, host_ptr, &mut path);
        trim_window_from_subresource_load_path(dispatch_target, event_type.as_deref(), &mut path);
        let result = dispatch_public_event(
            &mut self.events,
            scope,
            host_ptr,
            dispatch_target,
            &path,
            event,
        )?;
        if reference_related_target.is_some()
            && matches!(dispatch_target, EventTargetHandle::Node(_))
            && !result.canceled_before_dispatch()
        {
            self.record_reference_related_target(
                scope,
                host_ptr,
                event,
                &path,
                entry_related_targets.as_deref(),
                result.last_handled_entry(),
            );
        }
        Ok(result)
    }

    /// Blink's `EventPath::AdjustForReferenceTarget`: the related target is
    /// retargeted into each entry's tree, and the path ends before the first
    /// entry outside both original trees where the retargeted target and
    /// related target meet. Returns the path and, when the related target
    /// applies, each entry's retargeted related target, fixed when the path
    /// is built as Blink's event contexts are.
    fn build_reference_target_propagation_path(
        &self,
        host_ptr: *mut PageScriptHost,
        target: EventTargetHandle,
        related_target: EventTargetHandle,
        composed: bool,
    ) -> (Vec<EventTargetHandle>, Option<Vec<EventTargetHandle>>) {
        let mut path = self.build_propagation_path(target, composed);
        let (EventTargetHandle::Node(target_handle), EventTargetHandle::Node(source_handle)) =
            (target, related_target)
        else {
            return (path, None);
        };
        let target_document = self.dom_host.owner_document_handle(target_handle);
        if target_document.is_none()
            || target_document != self.dom_host.owner_document_handle(source_handle)
        {
            return (path, None);
        }
        // A source whose retargeting against the target is the target itself
        // (a shadow-internal source of its own host) would remove the whole
        // path, so Blink uses the target as the related target.
        let related_handle =
            if crate::host::retarget_event_target(host_ptr, related_target, target) == target {
                target_handle
            } else {
                source_handle
            };
        let target_root = self.dom_host.root_node_handle(target_handle);
        let related_root = self.dom_host.root_node_handle(related_handle);
        let stop = path.iter().position(|entry| {
            if !matches!(entry, EventTargetHandle::Node(_)) {
                return false;
            }
            let adjusted_target = crate::host::retarget_event_target(host_ptr, target, *entry);
            let adjusted_related = crate::host::retarget_event_target(
                host_ptr,
                EventTargetHandle::Node(related_handle),
                *entry,
            );
            let (
                EventTargetHandle::Node(adjusted_target),
                EventTargetHandle::Node(adjusted_related),
            ) = (adjusted_target, adjusted_related)
            else {
                return false;
            };
            adjusted_target == adjusted_related
                && !(self.dom_host.root_node_handle(adjusted_target) == target_root
                    && self.dom_host.root_node_handle(adjusted_related) == related_root)
        });
        if let Some(stop) = stop {
            path.truncate(stop);
        }
        let entry_related_targets = path
            .iter()
            .map(|entry| {
                crate::host::retarget_event_target(
                    host_ptr,
                    EventTargetHandle::Node(related_handle),
                    *entry,
                )
            })
            .collect();
        (path, Some(entry_related_targets))
    }

    /// The related target a Node dispatch leaves on a source-bearing event,
    /// as Blink's `HandleLocalEvents` and final clearing do: null when the
    /// dispatch ends with a null target (the path ends at a shadow root),
    /// otherwise the related target of the last entry the dispatch reached.
    /// A dispatch that reached no entry keeps the previous one.
    fn record_reference_related_target(
        &self,
        scope: &mut v8::PinScope<'_, '_>,
        host_ptr: *mut PageScriptHost,
        event: v8::Local<'_, v8::Object>,
        path: &[EventTargetHandle],
        entry_related_targets: Option<&[EventTargetHandle]>,
        last_handled_entry: Option<EventTargetHandle>,
    ) {
        let ends_at_shadow_root = matches!(
            path.last(),
            Some(EventTargetHandle::Node(handle)) if self.dom_host.is_shadow_root(*handle)
        );
        let value = if ends_at_shadow_root {
            v8::null(scope).into()
        } else {
            let Some(related_target) = entry_related_targets.and_then(|related_targets| {
                let entry = last_handled_entry?;
                let index = path.iter().position(|candidate| *candidate == entry)?;
                related_targets.get(index).copied()
            }) else {
                return;
            };
            match crate::host::event_target_value(scope, host_ptr, related_target) {
                Ok(value) => value,
                Err(_) => return,
            }
        };
        crate::context_bootstrap::set_event_private_value(
            scope,
            event,
            EVENT_REFERENCE_RELATED_TARGET_SLOT,
            value,
        );
    }

    pub(crate) fn build_propagation_path(
        &self,
        target: EventTargetHandle,
        composed: bool,
    ) -> Vec<EventTargetHandle> {
        // Returns [target, direct_parent_or_shadow_root, ..., Window] (ascending).
        let mut path = vec![target];
        if let EventTargetHandle::Node(mut current) = target {
            loop {
                if let Some(slot) = self.dom_host.assigned_slot_for_node(current) {
                    path.push(EventTargetHandle::Node(slot));
                    current = slot;
                    continue;
                }
                if let Some(parent) = self.parent_node(current) {
                    path.push(EventTargetHandle::Node(parent));
                    current = parent;
                    continue;
                }
                if self.dom_host.is_shadow_root(current) {
                    // A non-composed event stops only at the shadow root of
                    // its target's own tree; slotted paths cross the others.
                    if !composed
                        && matches!(
                            target,
                            EventTargetHandle::Node(original_target)
                                if self.dom_host.root_node_handle(original_target) == Some(current)
                        )
                    {
                        break;
                    }
                    let Some(host) = self.dom_host.shadow_root_host(current) else {
                        break;
                    };
                    path.push(EventTargetHandle::Node(host));
                    current = host;
                    continue;
                }
                break;
            }
        }
        let ended_at_shadow_root = matches!(
            path.last(),
            Some(EventTargetHandle::Node(handle)) if self.dom_host.is_shadow_root(*handle)
        );
        let ended_at_connected_tree = matches!(
            path.last(),
            Some(EventTargetHandle::Node(handle)) if self.dom_host.is_connected(*handle)
        );
        if !target.is_window() && !ended_at_shadow_root && ended_at_connected_tree {
            path.push(EventTargetHandle::Window);
        }
        path
    }

    fn append_child_document_window_to_path(
        &self,
        scope: &mut v8::PinScope<'_, '_>,
        host_ptr: *mut PageScriptHost,
        path: &mut Vec<EventTargetHandle>,
    ) {
        if path
            .iter()
            .any(|target| matches!(target, EventTargetHandle::ChildWindow(_)))
        {
            return;
        }
        let trailing_main_window = path.last() == Some(&EventTargetHandle::Window);
        let document_target = if trailing_main_window {
            path.get(path.len().saturating_sub(2)).copied()
        } else {
            path.last().copied()
        };
        let Some(EventTargetHandle::Node(document_handle)) = document_target else {
            return;
        };
        if !self
            .dom_host()
            .node(document_handle)
            .is_some_and(moli_dom::native::Node::is_document)
        {
            return;
        }
        let host = unsafe { &*host_ptr };
        if let Some(child_handle) =
            host.child_browsing_context_handle_by_document_handle(scope, document_handle)
            && let Some(target) = host.current_child_window_event_target(child_handle)
        {
            if trailing_main_window {
                path.pop();
            }
            path.push(EventTargetHandle::ChildWindow(target));
        }
    }
}

fn public_event_type(
    scope: &mut v8::PinScope<'_, '_>,
    event: v8::Local<'_, v8::Object>,
) -> Option<String> {
    crate::context_bootstrap::event_attribute(scope, event, "type")
        .and_then(|value| value.to_string(scope))
        .map(|value| value.to_rust_string_lossy(scope))
}

fn trim_window_from_subresource_load_path(
    dispatch_target: EventTargetHandle,
    event_type: Option<&str>,
    path: &mut Vec<EventTargetHandle>,
) {
    // Chromium keeps element/resource `load` events off Window even for capture
    // listeners. The document/window lifecycle load is dispatched with Window
    // as the concrete target and must keep its Window target path.
    if event_type != Some("load") || dispatch_target.is_window() {
        return;
    }
    if path.last().is_some_and(|target| target.is_window()) {
        path.pop();
    }
}

/// Private slot for the related target that shapes a source-bearing
/// event's path: Blink's `related_target_` on CommandEvent, ToggleEvent,
/// InterestEvent and SubmitEvent. It starts as the source or submitter and
/// each Node dispatch updates it. The `source` getter never reads it.
const EVENT_REFERENCE_RELATED_TARGET_SLOT: &str = "__moliEventReferenceRelatedTarget";

/// Private slot set when Moli fires a source-bearing event with a source,
/// as Blink's `SetComposed` at creation: its path is composed on every
/// dispatch, whatever the public `composed` attribute says.
const EVENT_SOURCE_COMPOSED_SLOT: &str = "__moliEventSourceComposed";

pub(crate) fn mark_event_source_composed(
    scope: &mut v8::PinScope<'_, '_>,
    event: v8::Local<'_, v8::Object>,
    source: v8::Local<'_, v8::Value>,
) {
    if source.is_object() {
        crate::context_bootstrap::set_event_private_value(
            scope,
            event,
            EVENT_SOURCE_COMPOSED_SLOT,
            v8::Boolean::new(scope, true).into(),
        );
    }
}

/// `None` for an event whose interface has no reference related target,
/// otherwise that target when it is a Node of this runtime. Blink selects
/// the adjustment by interface, whatever the event's type.
fn reference_related_target_for_event<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    host_ptr: *mut PageScriptHost,
    event: v8::Local<'s, v8::Object>,
) -> Option<Option<EventTargetHandle>> {
    let interface = crate::context_bootstrap::reference_target_event_interface(scope, event)?;
    let value = crate::context_bootstrap::event_private_value(
        scope,
        event,
        EVENT_REFERENCE_RELATED_TARGET_SLOT,
    )
    .or_else(|| match interface {
        crate::context_bootstrap::ReferenceTargetEventInterface::Submit => {
            crate::context_bootstrap::submit_event_submitter_value(scope, event)
        }
        crate::context_bootstrap::ReferenceTargetEventInterface::Source => {
            crate::context_bootstrap::event_attribute(scope, event, "source")
        }
    });
    let Some(object) = value.and_then(|value| v8::Local::<v8::Object>::try_from(value).ok()) else {
        return Some(None);
    };
    Some(
        crate::native_bridge::node_runtime_and_handle_from_object_or_detached(scope, object)
            .ok()
            .filter(|(runtime_ptr, _)| *runtime_ptr == host_ptr)
            .map(|(_, handle)| EventTargetHandle::Node(handle)),
    )
}
