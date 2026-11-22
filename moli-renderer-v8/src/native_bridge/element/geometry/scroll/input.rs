use super::super::provider::{read_element_metrics, read_scroll_into_view_geometry};
use super::*;

pub(crate) fn perform_scrollbar_scroll_default_action(
    scope: &mut v8::PinScope<'_, '_>,
    runtime_ptr: *mut PageScriptHost,
    source: DomHandle,
    axis: moli_layout::LayoutScrollbarAxis,
    target: f64,
) -> bool {
    let (current_x, current_y) = node_scroll_position(unsafe { &*runtime_ptr }, source);
    let (target_x, target_y) = match axis {
        moli_layout::LayoutScrollbarAxis::Horizontal => (target, current_y),
        moli_layout::LayoutScrollbarAxis::Vertical => (current_x, target),
    };
    let document = unsafe { &*runtime_ptr }
        .dom_host()
        .owner_document_handle(source);
    let is_document_scroller = document.is_some_and(|document| {
        unsafe { &*runtime_ptr }
            .dom_host()
            .dom()
            .document_element_handle_for_document(document)
            == Some(source)
    });
    if is_document_scroller {
        apply_observable_window_scroll(
            scope,
            runtime_ptr,
            source,
            target_x,
            target_y,
            current_x,
            current_y,
        )
    } else {
        set_node_scroll_position(scope, runtime_ptr, source, target_x, target_y, true)
    }
}

fn consume_wheel_axis(
    current: f64,
    minimum: f64,
    maximum: f64,
    remaining: f64,
    allows_user_scroll: bool,
) -> (f64, f64) {
    if !allows_user_scroll || remaining == 0.0 {
        return (current, remaining);
    }
    let target = (current + remaining).clamp(minimum, maximum);
    (target, remaining - (target - current))
}

/// The scroll containers a user scroll starting at `handle` may move,
/// innermost first and ending with the root scroller.
fn user_scroll_chain(
    runtime: &PageScriptHost,
    handle: DomHandle,
) -> Result<Vec<moli_layout::LayoutScrollContainerMetrics<DomHandle>>, moli_layout::LayoutError> {
    let Some(target) = resolve_scroll_target(runtime, handle) else {
        return Ok(Vec::new());
    };
    let Some(mut geometry) = read_scroll_into_view_geometry(runtime, target)? else {
        return Ok(Vec::new());
    };

    // ScrollIntoView geometry begins at the target's parent. Include the
    // target itself so an empty overflow scroller still responds when its own
    // background is the hit-test result, and a focused scroller takes its own
    // keyboard scrolls.
    if let Some(metrics) = read_element_metrics(runtime, target)?
        && metrics.is_scroll_container
        && geometry
            .scroll_containers
            .iter()
            .all(|container| container.source != target)
    {
        geometry.scroll_containers.insert(
            0,
            moli_layout::LayoutScrollContainerMetrics {
                source: target,
                metrics,
            },
        );
    }
    Ok(geometry.scroll_containers)
}

fn apply_user_scroll(
    scope: &mut v8::PinScope<'_, '_>,
    runtime_ptr: *mut PageScriptHost,
    source: DomHandle,
    target: (f64, f64),
    current: (f64, f64),
) -> bool {
    let container_document = unsafe { &*runtime_ptr }
        .dom_host()
        .owner_document_handle(source);
    let is_document_scroller = container_document.is_some_and(|document| {
        unsafe { &*runtime_ptr }
            .dom_host()
            .dom()
            .document_element_handle_for_document(document)
            == Some(source)
    });
    if is_document_scroller {
        apply_observable_window_scroll(
            scope,
            runtime_ptr,
            source,
            target.0,
            target.1,
            current.0,
            current.1,
        )
    } else {
        set_node_scroll_position(scope, runtime_ptr, source, target.0, target.1, true);
        true
    }
}

/// Run the uncancelled default action for one pixel-mode WheelEvent.
///
/// Each axis starts at the innermost scroll container under the pointer. Any
/// delta left at that container's boundary continues along the layout ancestor
/// chain, matching the scroll chaining users expect from a trackpad or wheel.
pub(crate) fn perform_wheel_scroll_default_action(
    scope: &mut v8::PinScope<'_, '_>,
    runtime_ptr: *mut PageScriptHost,
    handle: DomHandle,
    delta_x: f64,
    delta_y: f64,
) -> Result<bool, moli_layout::LayoutError> {
    let mut remaining_x = if delta_x.is_finite() { delta_x } else { 0.0 };
    let mut remaining_y = if delta_y.is_finite() { delta_y } else { 0.0 };
    if remaining_x == 0.0 && remaining_y == 0.0 {
        return Ok(false);
    }

    let mut changed = false;
    for container in user_scroll_chain(unsafe { &*runtime_ptr }, handle)? {
        if remaining_x == 0.0 && remaining_y == 0.0 {
            break;
        }
        let (current_x, current_y) =
            node_scroll_position(unsafe { &*runtime_ptr }, container.source);
        let (target_x, next_remaining_x) = consume_wheel_axis(
            current_x,
            f64::from(container.metrics.minimum_scroll_offset.x),
            f64::from(container.metrics.maximum_scroll_offset.x),
            remaining_x,
            container.metrics.allows_user_scroll_x,
        );
        let (target_y, next_remaining_y) = consume_wheel_axis(
            current_y,
            f64::from(container.metrics.minimum_scroll_offset.y),
            f64::from(container.metrics.maximum_scroll_offset.y),
            remaining_y,
            container.metrics.allows_user_scroll_y,
        );
        remaining_x = next_remaining_x;
        remaining_y = next_remaining_y;
        if target_x == current_x && target_y == current_y {
            continue;
        }
        changed |= apply_user_scroll(
            scope,
            runtime_ptr,
            container.source,
            (target_x, target_y),
            (current_x, current_y),
        );
    }
    Ok(changed)
}

/// How far one keyboard scroll moves, following Blink's ScrollableArea steps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum KeyboardScrollStep {
    /// Arrow keys: 40 CSS pixels (`kPixelsPerLineStep`).
    Line,
    /// PageUp/PageDown: 87.5% of the scrollport (`kMinFractionToStepWhenPaging`).
    Page,
    /// Home/End: the scroll range's start or end.
    Document,
}

/// One keyboard scroll request: the axis, direction and step for a key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct KeyboardScroll {
    vertical: bool,
    forward: bool,
    step: KeyboardScrollStep,
}

impl KeyboardScroll {
    /// The keyboard scroll a lower-cased `KeyboardEvent.key` requests.
    pub(crate) fn for_key(key_lower: &str) -> Option<Self> {
        let (vertical, forward, step) = match key_lower {
            "arrowdown" | "down" => (true, true, KeyboardScrollStep::Line),
            "arrowup" | "up" => (true, false, KeyboardScrollStep::Line),
            "arrowright" | "right" => (false, true, KeyboardScrollStep::Line),
            "arrowleft" | "left" => (false, false, KeyboardScrollStep::Line),
            "pagedown" => (true, true, KeyboardScrollStep::Page),
            "pageup" => (true, false, KeyboardScrollStep::Page),
            "end" => (true, true, KeyboardScrollStep::Document),
            "home" => (true, false, KeyboardScrollStep::Document),
            _ => return None,
        };
        Some(Self {
            vertical,
            forward,
            step,
        })
    }
}

/// Run the default action of an uncancelled scrolling keydown.
///
/// Like Chromium 156, the innermost user-scrollable container around the
/// focused element that can still move in the key's direction scrolls by one
/// step, and containers already at that edge pass the scroll on. A focused
/// scroller with overflow on the key's axis keeps the scroll even at its edge.
pub(crate) fn perform_keyboard_scroll_default_action(
    scope: &mut v8::PinScope<'_, '_>,
    runtime_ptr: *mut PageScriptHost,
    handle: DomHandle,
    scroll: KeyboardScroll,
) -> Result<bool, moli_layout::LayoutError> {
    let target = resolve_scroll_target(unsafe { &*runtime_ptr }, handle);
    for container in user_scroll_chain(unsafe { &*runtime_ptr }, handle)? {
        let metrics = &container.metrics;
        let (allows, minimum, maximum, client) = if scroll.vertical {
            (
                metrics.allows_user_scroll_y,
                metrics.minimum_scroll_offset.y,
                metrics.maximum_scroll_offset.y,
                metrics.client_size.height,
            )
        } else {
            (
                metrics.allows_user_scroll_x,
                metrics.minimum_scroll_offset.x,
                metrics.maximum_scroll_offset.x,
                metrics.client_size.width,
            )
        };
        if !allows {
            continue;
        }
        let (minimum, maximum) = (f64::from(minimum), f64::from(maximum));
        let current = node_scroll_position(unsafe { &*runtime_ptr }, container.source);
        let current_axis = if scroll.vertical {
            current.1
        } else {
            current.0
        };
        let distance = match scroll.step {
            KeyboardScrollStep::Line => 40.0,
            // Blink's integer ScrollableArea::PageStep.
            KeyboardScrollStep::Page => (f64::from(client) * 0.875).floor().max(1.0),
            KeyboardScrollStep::Document => maximum - minimum,
        };
        let signed = if scroll.forward { distance } else { -distance };
        let target_axis = (current_axis + signed).clamp(minimum, maximum);
        if target_axis == current_axis {
            if Some(container.source) == target && maximum > minimum {
                return Ok(false);
            }
            continue;
        }
        let next = if scroll.vertical {
            (current.0, target_axis)
        } else {
            (target_axis, current.1)
        };
        return Ok(apply_user_scroll(
            scope,
            runtime_ptr,
            container.source,
            next,
            current,
        ));
    }
    Ok(false)
}
