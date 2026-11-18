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
const CSS_TRANSITION_FINISHED_REJECT_SLOT: &str = "__moliCssTransitionFinishedReject";
const PROMISE_CAPABILITY_RESOLVE_SLOT: &str = "__moliPromiseCapabilityResolve";
const PROMISE_CAPABILITY_REJECT_SLOT: &str = "__moliPromiseCapabilityReject";

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
    let entry = v8::Object::new(scope);
    let Some(executor) = v8::Function::builder(promise_capability_executor)
        .data(entry.into())
        .length(2)
        .build(scope)
    else {
        return;
    };
    let Some(promise) = global_constructor_object(scope, "Promise")
        .and_then(|constructor| v8::Local::<v8::Function>::try_from(constructor).ok())
        .and_then(|constructor| constructor.new_instance(scope, &[executor.into()]))
    else {
        return;
    };
    let undefined = v8::undefined(scope).into();
    let resolve =
        get_private_value(scope, entry, PROMISE_CAPABILITY_RESOLVE_SLOT).unwrap_or(undefined);
    let reject =
        get_private_value(scope, entry, PROMISE_CAPABILITY_REJECT_SLOT).unwrap_or(undefined);
    set_private_value(scope, animation, ANIMATION_FINISHED_SLOT, promise.into());
    set_private_value(scope, animation, ANIMATION_FINISHED_RESOLVE_SLOT, resolve);
    set_private_value(
        scope,
        animation,
        CSS_TRANSITION_FINISHED_REJECT_SLOT,
        reject,
    );
}

fn promise_capability_executor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    if let Ok(entry) = v8::Local::<v8::Object>::try_from(args.data()) {
        set_private_value(scope, entry, PROMISE_CAPABILITY_RESOLVE_SLOT, args.get(0));
        set_private_value(scope, entry, PROMISE_CAPABILITY_REJECT_SLOT, args.get(1));
    }
    rv.set_undefined();
}

/// Settles a `CSSTransition` whose transition ended: `finished` resolves
/// with it.
fn settle_finished<'s>(scope: &mut v8::PinScope<'s, '_>, animation: v8::Local<'s, v8::Object>) {
    if animation_play_state(scope, animation) != "running" {
        return;
    }
    set_animation_play_state(scope, animation, "finished");
    if let Some(resolve) = get_private_value(scope, animation, ANIMATION_FINISHED_RESOLVE_SLOT)
        .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok())
    {
        let undefined = v8::undefined(scope).into();
        let _ = resolve.call(scope, undefined, &[animation.into()]);
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
    if let Some(reject) = get_private_value(scope, animation, CSS_TRANSITION_FINISHED_REJECT_SLOT)
        .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok())
    {
        let error = super::super::new_dom_exception_value(
            scope,
            "The user aborted a request.",
            "AbortError",
        );
        let undefined = v8::undefined(scope).into();
        let _ = reject.call(scope, undefined, &[error]);
    }
    if let Some(promise) = get_private_value(scope, animation, ANIMATION_FINISHED_SLOT)
        .and_then(|value| v8::Local::<v8::Promise>::try_from(value).ok())
    {
        promise.mark_as_handled();
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

/// The `CSSTransition`s of `element`, in composite order, after flushing its
/// style so a pending style change starts its transitions first.
pub(super) fn element_css_transitions<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    runtime: &PageScriptHost,
    handle: DomHandle,
    target: v8::Local<'s, v8::Object>,
) -> Vec<v8::Local<'s, v8::Object>> {
    let records = runtime
        .style_engine_ref()
        .css_transitions_for_element(runtime.dom_host(), handle);
    let animations = records
        .iter()
        .filter_map(|record| css_transition_object(scope, record, target))
        .collect::<Vec<_>>();
    if !animations.is_empty() {
        queue_css_transition_task_if_due(scope);
    }
    animations
}

/// Queues the transition task when transition events are pending or a
/// running transition next changes phase, unless a queued one runs first.
pub(crate) fn queue_css_transition_task_if_due(scope: &mut v8::PinScope<'_, '_>) {
    let Some(host_ptr) = crate::util::context_host_ptr_from_global_bridge(scope) else {
        return;
    };
    let host = unsafe { &mut *host_ptr };
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
    let Some(callback) = v8::FunctionTemplate::builder(css_transition_task_callback)
        .build(scope)
        .get_function(scope)
    else {
        return;
    };
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
    {
        let host = unsafe { &*host_ptr };
        let engine = host.style_engine_ref();
        engine.css_transition_task_due().set(None);
        engine.sample_css_transitions(host.dom_host());
    }
    let events = unsafe { &*host_ptr }
        .style_engine_ref()
        .take_css_transition_events();
    for event in events {
        deliver_css_transition_event(scope, host_ptr, &event);
    }
    queue_css_transition_task_if_due(scope);
}

fn deliver_css_transition_event(
    scope: &mut v8::PinScope<'_, '_>,
    host_ptr: *mut PageScriptHost,
    event: &CssTransitionEvent,
) {
    // The transition task can run in any Window of this host. The
    // `CSSTransition` objects and the event belong to the realm of the
    // element's Document (a same-origin child frame has its own).
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
    if !event.dispatch {
        return;
    }
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
