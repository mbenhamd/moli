//! Script objects and events of the CSS transitions the style engine runs
//! (CSS Transitions Level 2 §5 and §6).
//!
//! Each running transition is one `CSSTransition`, kept per realm by id so
//! `getAnimations()` returns the same object while it runs. A transition
//! task delivers the engine's transition events: it dispatches the
//! `TransitionEvent`s and settles the objects' `playState` and `finished`.

use super::*;
use crate::document_runtime::EventTargetHandle;
use crate::style_engine::transitions::{
    CssTransitionEvent, CssTransitionEventKind, CssTransitionRecord, animation_clock_now,
};

const CSS_TRANSITION_OBJECTS_SLOT: &str = "__moliCssTransitionObjects";
pub(super) const CSS_TRANSITION_ID_SLOT: &str = "__moliCssTransitionId";
const CSS_TRANSITION_PROPERTY_SLOT: &str = "__moliCssTransitionProperty";
const CSS_TRANSITION_FINISHED_RESOLVER_SLOT: &str = "__moliCssTransitionFinishedResolver";

#[derive(WebApiFunctionTemplate)]
#[webapi(interface = web_api_interfaces::CSSTransition, enumerable)]
pub(super) struct CssTransitionPrototypeDeclaration {
    #[webapi(
        accessor_property = "transitionProperty",
        getter = css_transition_property_getter
    )]
    transition_property: (),
}

fn css_transition_property_getter<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Some(property) = get_private_value(scope, args.this(), CSS_TRANSITION_PROPERTY_SLOT) else {
        throw_type_error(
            scope,
            "CSSTransition.transitionProperty getter called on incompatible receiver.",
        );
        return;
    };
    rv.set(property);
}

/// The transition id of `animation`, when it is a `CSSTransition`.
pub(super) fn css_transition_id<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    animation: v8::Local<'s, v8::Object>,
) -> Option<u64> {
    private_number(scope, animation, CSS_TRANSITION_ID_SLOT).map(|id| id as u64)
}

fn transition_objects<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Map> {
    let global = scope.get_current_context().global(scope);
    if let Some(map) = get_private_value(scope, global, CSS_TRANSITION_OBJECTS_SLOT)
        .and_then(|value| v8::Local::<v8::Map>::try_from(value).ok())
    {
        return map;
    }
    let map = v8::Map::new(scope);
    set_private_value(scope, global, CSS_TRANSITION_OBJECTS_SLOT, map.into());
    map
}

fn existing_transition_object<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    id: u64,
) -> Option<v8::Local<'s, v8::Object>> {
    let map = transition_objects(scope);
    let key = v8::Number::new(scope, id as f64);
    map.get(scope, key.into())
        .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())
}

fn forget_transition_object(scope: &mut v8::PinScope<'_, '_>, id: u64) {
    let map = transition_objects(scope);
    let key = v8::Number::new(scope, id as f64);
    let _ = map.delete(scope, key.into());
}

/// The `CSSTransition` of a running transition of element `target`.
pub(super) fn css_transition_object<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    record: &CssTransitionRecord,
    target: v8::Local<'s, v8::Object>,
) -> Option<v8::Local<'s, v8::Object>> {
    if let Some(existing) = existing_transition_object(scope, record.id) {
        return Some(existing);
    }
    let keyframes = transition_keyframes(scope, record);
    let timing = transition_timing(scope, record);
    let effect = new_keyframe_effect_for_target(scope, target, keyframes.into(), timing.into())?;
    let constructor = global_constructor_object(scope, "Animation")
        .and_then(|constructor| v8::Local::<v8::Function>::try_from(constructor).ok())?;
    let timeline = v8::null(scope);
    let animation = constructor.new_instance(scope, &[effect.into(), timeline.into()])?;
    let _ = web_api_interfaces::CSSTransition::DESCRIPTOR.initialize(scope, animation);
    if let Some(prototype) = crate::util::global_constructor_prototype(scope, "CSSTransition") {
        let _ = animation.set_prototype(scope, prototype.into());
    }
    let id = v8::Number::new(scope, record.id as f64);
    set_private_value(scope, animation, CSS_TRANSITION_ID_SLOT, id.into());
    if let Some(property) = v8_string(scope, &record.property) {
        set_private_value(
            scope,
            animation,
            CSS_TRANSITION_PROPERTY_SLOT,
            property.into(),
        );
    }
    set_animation_play_state(scope, animation, "running");
    // The animation's current time is zero at the style change event; the
    // transition delay is part of its effect's timing.
    let origin = super::super::performance_runtime::current_performance_time_origin(scope);
    let start_time = v8::Number::new(scope, (record.start_time - record.delay) * 1000.0 - origin);
    set_private_value(
        scope,
        animation,
        ANIMATION_START_TIME_SLOT,
        start_time.into(),
    );
    renew_finished_promise(scope, animation);
    let map = transition_objects(scope);
    let _ = map.set(scope, id.into(), animation.into());
    Some(animation)
}

/// The transition's two keyframes, as `getKeyframes()` reports them.
fn transition_keyframes<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    record: &CssTransitionRecord,
) -> v8::Local<'s, v8::Array> {
    let property = css_property_to_idl_attribute(&record.property);
    let frames = [(0.0, record.from.as_str()), (1.0, record.to.as_str())]
        .into_iter()
        .map(|(offset, value)| {
            let frame = v8::Object::new(scope);
            let offset = v8::Number::new(scope, offset);
            let _ = frame.set(scope, v8str(scope, "offset").into(), offset.into());
            if let (Some(name), Some(value)) =
                (v8_string(scope, &property), v8_string(scope, value))
            {
                let _ = frame.set(scope, name.into(), value.into());
            }
            frame.into()
        })
        .collect::<Vec<v8::Local<v8::Value>>>();
    v8::Array::new_with_elements(scope, &frames)
}

/// CSS Transitions Level 2 §5.1: a transition's effect has the transition's
/// delay, duration and timing function, and fills backwards.
fn transition_timing<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    record: &CssTransitionRecord,
) -> v8::Local<'s, v8::Object> {
    let timing = v8::Object::new(scope);
    // Seconds to milliseconds without binary noise (0.07 s is 70 ms).
    let milliseconds = |seconds: f64| (seconds * 1e9).round() / 1e6;
    let numbers = [
        ("delay", milliseconds(record.delay)),
        ("duration", milliseconds(record.duration)),
    ];
    for (name, value) in numbers {
        let value = v8::Number::new(scope, value);
        let _ = timing.set(scope, v8str(scope, name).into(), value.into());
    }
    let strings = [("easing", record.easing.as_str()), ("fill", "backwards")];
    for (name, value) in strings {
        if let Some(value) = v8_string(scope, value) {
            let _ = timing.set(scope, v8str(scope, name).into(), value.into());
        }
    }
    timing
}

/// `background-color` to `backgroundColor`; custom properties stay as they
/// are (Web Animations §6.6.2, "property name conversion").
fn css_property_to_idl_attribute(property: &str) -> String {
    if property.starts_with("--") {
        return property.to_owned();
    }
    let mut attribute = String::with_capacity(property.len());
    let mut upper = false;
    for character in property.chars() {
        if character == '-' {
            upper = true;
        } else if upper {
            attribute.push(character.to_ascii_uppercase());
            upper = false;
        } else {
            attribute.push(character);
        }
    }
    attribute
}

/// Gives `animation` a new pending `finished` promise, which a cancel
/// rejects and an end resolves.
fn renew_finished_promise<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    animation: v8::Local<'s, v8::Object>,
) {
    let Some(resolver) = v8::PromiseResolver::new(scope) else {
        return;
    };
    let promise = resolver.get_promise(scope);
    set_private_value(scope, animation, ANIMATION_FINISHED_SLOT, promise.into());
    set_private_value(
        scope,
        animation,
        CSS_TRANSITION_FINISHED_RESOLVER_SLOT,
        resolver.into(),
    );
}

fn finished_resolver<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    animation: v8::Local<'s, v8::Object>,
) -> Option<v8::Local<'s, v8::PromiseResolver>> {
    let resolver = get_private_value(scope, animation, CSS_TRANSITION_FINISHED_RESOLVER_SLOT)
        .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())?;
    // SAFETY: only `renew_finished_promise` writes this private slot, and it
    // always stores a `PromiseResolver`.
    Some(unsafe { v8::Local::<v8::PromiseResolver>::cast_unchecked(resolver) })
}

/// Settles a `CSSTransition` whose transition ended: `finished` resolves
/// with it.
fn settle_finished<'s>(scope: &mut v8::PinScope<'s, '_>, animation: v8::Local<'s, v8::Object>) {
    if animation_play_state(scope, animation) != "running" {
        return;
    }
    set_animation_play_state(scope, animation, "finished");
    if let Some(resolver) = finished_resolver(scope, animation) {
        let _ = resolver.resolve(scope, animation.into());
    }
}

/// Settles a `CSSTransition` whose transition was canceled: it becomes idle
/// and its `finished` promise rejects with an `AbortError`, handled, and is
/// replaced (Web Animations §4.4.15, "reset an animation's pending tasks"
/// and "cancel an animation").
fn settle_canceled<'s>(scope: &mut v8::PinScope<'s, '_>, animation: v8::Local<'s, v8::Object>) {
    if animation_play_state(scope, animation) == "idle" {
        return;
    }
    let was_running = animation_play_state(scope, animation) == "running";
    set_animation_play_state(scope, animation, "idle");
    let null = v8::null(scope).into();
    set_private_value(scope, animation, ANIMATION_START_TIME_SLOT, null);
    if !was_running {
        return;
    }
    if let Some(resolver) = finished_resolver(scope, animation) {
        // Handled first, so the rejection is never reported as unhandled.
        resolver.get_promise(scope).mark_as_handled();
        let error = super::super::new_dom_exception_value(
            scope,
            "The user aborted a request.",
            "AbortError",
        );
        let _ = resolver.reject(scope, error);
    }
    renew_finished_promise(scope, animation);
}

/// `Animation.finish()` and `Animation.cancel()` on a `CSSTransition`: the
/// style engine ends or cancels the transition, the object settles now, and
/// its events follow in the transition task.
pub(super) fn settle_css_transition_from_script<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    animation: v8::Local<'s, v8::Object>,
    id: u64,
    finish: bool,
) {
    if let Some(host_ptr) = crate::util::context_host_ptr_from_global_bridge(scope) {
        let host = unsafe { &*host_ptr };
        host.style_engine_ref()
            .settle_css_transition(host.dom_host(), id, finish);
    }
    if finish {
        settle_finished(scope, animation);
    } else {
        settle_canceled(scope, animation);
    }
    forget_transition_object(scope, id);
    queue_css_transition_task_if_due(scope);
}

/// The `CSSTransition`s of `element`, in composite order. The caller flushed
/// its style, so a pending style change started its transitions, and queues
/// the transition task once it no longer holds `runtime`.
pub(super) fn element_css_transitions<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    runtime: &PageScriptHost,
    handle: DomHandle,
    target: v8::Local<'s, v8::Object>,
) -> Vec<v8::Local<'s, v8::Object>> {
    let records = runtime
        .style_engine_ref()
        .css_transitions_for_element(runtime.dom_host(), handle);
    records
        .iter()
        .filter_map(|record| css_transition_object(scope, record, target))
        .collect()
}

/// Queues the transition task when transition events are pending or a
/// running transition next changes phase, unless a queued one runs first.
///
/// The task belongs to the page's main Window, which outlives every child
/// frame: a task owned by a child Window would be dropped with it while the
/// page-wide due time still holds back the next one.
pub(crate) fn queue_css_transition_task_if_due(scope: &mut v8::PinScope<'_, '_>) {
    let Some(host_ptr) = crate::util::context_host_ptr_from_global_bridge(scope) else {
        return;
    };
    let delay_ms = {
        let host = unsafe { &*host_ptr };
        // A queued animation frame runs the transitions' frame work and
        // queues the task after it.
        if host.has_pending_animation_frame() {
            return;
        }
        let engine = host.style_engine_ref();
        let Some(delay_ms) = engine.css_transition_task_delay_ms() else {
            return;
        };
        let due = animation_clock_now() + delay_ms / 1000.0;
        if engine
            .css_transition_task_due()
            .get()
            .is_some_and(|queued| queued <= due)
        {
            return;
        }
        delay_ms
    };
    let Some(main_context) = ({
        let host = unsafe { &*host_ptr };
        host.node_event_execution_context(scope, host.document_handle())
    }) else {
        return;
    };
    let scope = &mut v8::ContextScope::new(scope, main_context);
    let Some(callback) = v8::FunctionTemplate::builder(css_transition_task_callback)
        .build(scope)
        .get_function(scope)
    else {
        return;
    };
    let due = animation_clock_now() + delay_ms / 1000.0;
    let host = unsafe { &mut *host_ptr };
    if host.queue_internal_task_after(scope, callback, delay_ms.ceil() as u64) {
        host.style_engine_ref()
            .css_transition_task_due()
            .set(Some(due));
    }
}

fn css_transition_task_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    rv.set_undefined();
    let Some(host_ptr) = crate::util::context_host_ptr_from_global_bridge(scope) else {
        return;
    };
    unsafe { &*host_ptr }
        .style_engine_ref()
        .css_transition_task_due()
        .set(None);
    // A queued animation frame runs this frame's work and sends its events
    // at its start, as Chromium's rendering update does; it queues the next
    // task.
    if unsafe { &*host_ptr }.has_pending_animation_frame() {
        return;
    }
    // Events a style change queued outside a frame (`transitionrun`, a
    // removal's `transitioncancel`) are delivered without a frame: the
    // next frame resolves the new transitions' start times.
    if unsafe { &*host_ptr }
        .style_engine_ref()
        .has_pending_css_transition_events()
    {
        deliver_pending_css_transition_events(scope, host_ptr);
        queue_css_transition_task_if_due(scope);
        return;
    }
    run_css_transition_frame(scope, host_ptr, true);
}

/// One frame of the CSS transitions: "update animations and send events" at
/// the frame's time. Without animation frame callbacks (`update_styles`),
/// the frame first runs the style change events of the elements whose
/// styles changed. An animation frame runs them after its callbacks instead
/// (`end_css_transition_animation_frame`), as Chromium's style update
/// follows the callbacks, so the callbacks start without that work.
pub(crate) fn run_css_transition_frame(
    scope: &mut v8::PinScope<'_, '_>,
    host_ptr: *mut PageScriptHost,
    update_styles: bool,
) {
    {
        let host = unsafe { &*host_ptr };
        resolve_unresolved_css_transition_frame_roots(host);
        if update_styles {
            update_css_transition_styles(host, false);
        } else {
            // Removals since the last update cancel their transitions before
            // the sample (`update_css_transition_styles` drains them too).
            host.drain_pending_style_invalidations_for_computed_style_read();
        }
        let engine = host.style_engine_ref();
        engine.sample_css_transitions(host.dom_host());
        if update_styles {
            engine.note_css_transition_style_update();
        }
    }
    deliver_pending_css_transition_events(scope, host_ptr);
    queue_css_transition_task_if_due(scope);
}

fn deliver_pending_css_transition_events(
    scope: &mut v8::PinScope<'_, '_>,
    host_ptr: *mut PageScriptHost,
) {
    let events = unsafe { &*host_ptr }
        .style_engine_ref()
        .take_css_transition_events();
    if events.is_empty() {
        return;
    }
    // "Update animations and send events": the animations settle, a
    // microtask checkpoint runs their `finished` reactions, then the events
    // dispatch (Web Animations §4.4.21).
    for event in &events {
        settle_css_transition_object_for_event(scope, host_ptr, event);
    }
    if crate::custom_elements::can_perform_custom_element_microtask_checkpoint(scope) {
        crate::script_vm::perform_microtask_checkpoint_and_report_pending_promise_rejections(scope);
        for event in &events {
            dispatch_css_transition_event(scope, host_ptr, event);
        }
        return;
    }
    // Inside the transition task's callback no checkpoint can run: the
    // events dispatch from a microtask queued after the settlement's
    // reactions, so `finished` reactions still run first.
    unsafe { &*host_ptr }
        .style_engine_ref()
        .defer_css_transition_dispatch(events);
    if let Some(callback) = v8::Function::builder(css_transition_dispatch_microtask).build(scope) {
        scope.enqueue_microtask(callback);
    }
}

fn css_transition_dispatch_microtask<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _args: v8::FunctionCallbackArguments<'s>,
    _rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Some(host_ptr) = crate::util::context_host_ptr_from_global_bridge(scope) else {
        return;
    };
    flush_deferred_css_transition_dispatch(scope, host_ptr);
}

/// Dispatches the settled events still waiting for their microtask. An
/// animation frame dispatches them before its callback batch closes, so a
/// callback that a listener requests joins the frame; there the `finished`
/// reactions run after the events, as no checkpoint can run inside the
/// frame's callback task.
pub(crate) fn flush_deferred_css_transition_dispatch(
    scope: &mut v8::PinScope<'_, '_>,
    host_ptr: *mut PageScriptHost,
) {
    let events = unsafe { &*host_ptr }
        .style_engine_ref()
        .take_deferred_css_transition_dispatch();
    for event in &events {
        dispatch_css_transition_event(scope, host_ptr, event);
    }
}

/// Settles the `CSSTransition`s of the engine's transitions that ended or
/// were canceled and whose events still wait for delivery, so script that
/// flushes style sees them settled at once, as the style update that
/// settled them does in Chromium.
pub(super) fn settle_pending_css_transition_objects(
    scope: &mut v8::PinScope<'_, '_>,
    host_ptr: *mut PageScriptHost,
) {
    let events = unsafe { &*host_ptr }
        .style_engine_ref()
        .pending_css_transition_settlements();
    for event in &events {
        settle_css_transition_object_for_event(scope, host_ptr, event);
    }
}

/// Ends the animation frame callbacks' part of a frame: runs the style
/// change events of their style changes, so the transitions those start at
/// the frame's time, as Chromium's style update after the callbacks does.
pub(crate) fn end_css_transition_animation_frame(host: &PageScriptHost) {
    let engine = host.style_engine_ref();
    if !engine.css_transition_animation_frame_open() {
        return;
    }
    // Later callbacks of the same frame still run first: the style update
    // follows the frame's last callback.
    if engine
        .css_transition_frame_timestamp()
        .is_some_and(|timestamp| host.has_pending_animation_frame_at(timestamp))
    {
        return;
    }
    update_css_transition_styles(host, false);
    engine.note_css_transition_style_update();
    engine.end_css_transition_animation_frame();
}

/// Runs the style change event of every element whose style specifies
/// transitions, when any style changed since the last update, and cancels
/// the transitions of elements an ancestor's `display: none` stopped
/// rendering.
///
/// The subtrees whose styles changed get their styles too. A style flush
/// (`resolve_subtrees`) resolves them now. A frame's update leaves them to
/// `resolve_unresolved_css_transition_frame_roots`, which the next task,
/// command or frame runs before anything else: no script, DOM or style change
/// can run in between, so their before-change styles are still the frame's,
/// and the frame, which the tasks it queued wait on, ends without that work.
pub(crate) fn update_css_transition_styles(host: &PageScriptHost, resolve_subtrees: bool) {
    resolve_unresolved_css_transition_frame_roots(host);
    let engine = host.style_engine_ref();
    let dom = host.dom_host();
    // The frame's DOM changes become invalidation roots before the update
    // takes them, so a node inserted during this frame is styled now.
    host.drain_pending_style_invalidations_for_computed_style_read();
    let targets = engine.css_transition_style_update_targets(dom);
    let mut roots = engine.take_css_transition_style_roots();
    if let Some(document) = engine.take_unstyled_css_transition_document(host.document_handle()) {
        let mut child = dom.first_child(document);
        while let Some(current) = child {
            roots.push(current);
            child = dom.next_sibling(current);
        }
    }
    if targets.is_empty() && roots.is_empty() {
        return;
    }
    if resolve_subtrees {
        update_changed_subtree_styles(host, roots);
    } else {
        engine.defer_unresolved_css_transition_frame_roots(dom, roots);
    }
    for handle in targets {
        if dom.is_connected(handle) {
            let _ = crate::native_bridge::element::computed_style_property_for_handle(
                host,
                handle,
                "transition-property",
            );
        }
    }
    for target in engine.css_transition_running_targets() {
        if has_undisplayed_ancestor(host, target) {
            engine.cancel_css_transitions_for_element(dom, target);
        }
    }
}

/// Resolves the subtrees the last frame's style update left unresolved.
/// Every task, command and frame runs this first: it must run before any DOM
/// or style change after that frame.
pub(crate) fn resolve_unresolved_css_transition_frame_roots(host: &PageScriptHost) {
    let engine = host.style_engine_ref();
    if !engine.has_unresolved_css_transition_frame_roots() {
        return;
    }
    update_changed_subtree_styles(host, engine.take_unresolved_css_transition_frame_roots());
}

/// Resolves the styles of the subtrees whose styles changed, as the
/// rendering update's style recalc does, so their elements have a
/// before-change style for later style changes. Subtrees that are not
/// rendered (`display: none`) stay unstyled, as in Chromium. Style only: no
/// layout runs. One update resolves at most `MAX_ELEMENTS_PER_UPDATE`
/// elements; the nodes it did not reach carry over to the next frame's
/// update as roots, so a large insertion is styled over several frames
/// rather than dropped.
fn update_changed_subtree_styles(host: &PageScriptHost, roots: Vec<DomHandle>) {
    const MAX_ELEMENTS_PER_UPDATE: usize = 4096;
    let dom = host.dom_host();
    let mut pending = roots
        .into_iter()
        .filter(|&root| dom.is_connected(root))
        .collect::<Vec<_>>();
    let mut budget = MAX_ELEMENTS_PER_UPDATE;
    while let Some(handle) = pending.pop() {
        let Some(node) = dom.node(handle) else {
            continue;
        };
        if node.is_element() {
            if budget == 0 {
                pending.push(handle);
                host.style_engine_ref()
                    .defer_css_transition_style_roots(dom, pending);
                return;
            }
            budget -= 1;
            if crate::native_bridge::element::computed_style_property_for_handle(
                host, handle, "display",
            ) == "none"
            {
                continue;
            }
        }
        let mut child = dom.first_child(handle);
        while let Some(current) = child {
            pending.push(current);
            child = dom.next_sibling(current);
        }
    }
}

/// Flushes `element`'s style for `Element.getAnimations()`: a pending style
/// change starts or cancels its transitions, and an ancestor's
/// `display: none` cancels them.
pub(super) fn flush_element_css_transitions(host: &PageScriptHost, element: DomHandle) {
    resolve_unresolved_css_transition_frame_roots(host);
    let _ = crate::native_bridge::element::computed_style_property_for_handle(
        host,
        element,
        "transition-property",
    );
    let engine = host.style_engine_ref();
    if !engine
        .css_transitions_for_element(host.dom_host(), element)
        .is_empty()
        && has_undisplayed_ancestor(host, element)
    {
        engine.cancel_css_transitions_for_element(host.dom_host(), element);
    }
}

fn has_undisplayed_ancestor(host: &PageScriptHost, element: DomHandle) -> bool {
    let dom = host.dom_host();
    // The flat tree: a shadow root's children are rendered under its host.
    let mut current = dom.flat_tree_parent(element);
    while let Some(handle) = current {
        if !dom.node(handle).is_some_and(|node| node.is_element()) {
            return false;
        }
        if crate::native_bridge::element::computed_style_property_for_handle(
            host, handle, "display",
        ) == "none"
        {
            return true;
        }
        current = dom.flat_tree_parent(handle);
    }
    false
}

fn settle_css_transition_object_for_event(
    scope: &mut v8::PinScope<'_, '_>,
    host_ptr: *mut PageScriptHost,
    event: &CssTransitionEvent,
) {
    if !matches!(
        event.kind,
        CssTransitionEventKind::End | CssTransitionEventKind::Cancel
    ) {
        return;
    }
    // The `CSSTransition` objects belong to the realm of the element's
    // Document (a same-origin child frame has its own).
    let Some(context) = unsafe { &*host_ptr }.node_event_execution_context(scope, event.target)
    else {
        return;
    };
    let scope = &mut v8::ContextScope::new(scope, context);
    if let Some(animation) = existing_transition_object(scope, event.id) {
        match event.kind {
            CssTransitionEventKind::End => {
                settle_finished(scope, animation);
                forget_transition_object(scope, event.id);
            }
            CssTransitionEventKind::Cancel => {
                settle_canceled(scope, animation);
                forget_transition_object(scope, event.id);
            }
            CssTransitionEventKind::Run | CssTransitionEventKind::Start => {}
        }
    }
}

fn dispatch_css_transition_event(
    scope: &mut v8::PinScope<'_, '_>,
    host_ptr: *mut PageScriptHost,
    event: &CssTransitionEvent,
) {
    if !event.dispatch {
        return;
    }
    // The transition task can run in any Window of this host. The event
    // belongs to the realm of the element's Document.
    let Some(context) = unsafe { &*host_ptr }.node_event_execution_context(scope, event.target)
    else {
        return;
    };
    let scope = &mut v8::ContextScope::new(scope, context);
    let Some(constructor) = global_constructor_object(scope, "TransitionEvent")
        .and_then(|constructor| v8::Local::<v8::Function>::try_from(constructor).ok())
    else {
        return;
    };
    let init = v8::Object::new(scope);
    let true_value = v8::Boolean::new(scope, true);
    let _ = init.set(scope, v8str(scope, "bubbles").into(), true_value.into());
    let _ = init.set(scope, v8str(scope, "cancelable").into(), true_value.into());
    if let Some(property) = v8_string(scope, &event.property) {
        let _ = init.set(scope, v8str(scope, "propertyName").into(), property.into());
    }
    let elapsed_time = v8::Number::new(scope, event.elapsed_time);
    let _ = init.set(
        scope,
        v8str(scope, "elapsedTime").into(),
        elapsed_time.into(),
    );
    let Some(event_type) = v8_string(scope, event.kind.event_type()) else {
        return;
    };
    let Some(dom_event) = constructor.new_instance(scope, &[event_type.into(), init.into()]) else {
        return;
    };
    // The user agent fires transition events, so they are trusted.
    super::super::mark_event_trusted(scope, dom_event);
    let runtime = unsafe { &mut *host_ptr };
    let _ = runtime.dispatch_public_event_best_effort(
        scope,
        host_ptr,
        EventTargetHandle::Node(event.target),
        dom_event,
        "transition event",
    );
}
