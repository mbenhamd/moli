//! CSS Transitions for one Document (CSS Transitions Level 1 and 2).
//!
//! Stylo's Servo transition engine decides, at each style change event, which
//! transitions start, reverse or cancel, and interpolates their values. Moli
//! resolves styles on demand rather than in a traversal, so the resolver
//! entry points hand each published primary style to
//! [`process_css_transitions`], and this store adds what the traversal's
//! embedder owns: the document animation clock, transition identity, the
//! transition events, and the `Animation` state script observes.
//!
//! Moli has no continuous rendering loop, so a frame is either the start of
//! an animation frame callback batch or, when no callback is queued, the
//! transition task, which runs at a 60Hz frame interval while a transition
//! runs or a style change may start one. Each frame first runs the style
//! change events of the elements whose styles specify transitions, as the
//! rendering update's style recalc does, and then samples the clock (the
//! "update animations and send events" step). Style and geometry reads
//! between frames see the last frame's time, as in Chromium.
//!
//! A transition's start time is the time of the first frame that processes
//! its style change event: a style change made in an animation frame callback
//! belongs to that frame, any other one to the next frame, which shows the
//! transition's start value. A sample restyles every element whose transition
//! value moved, so a read after it shows the values at that time. Like Chromium's animation updates, a sample of colors alone repaints
//! without invalidating layout.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashMap, HashSet},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use style::{
    animation::{AnimationSetKey, AnimationState, DocumentAnimationSet},
    context::StyleContext,
    data::ElementStyles,
    dom::{OpaqueNode, TElement, TNode},
    matching::MatchMethods,
    properties::ComputedValues,
    servo_arc::Arc as ServoArc,
};
use style_traits::ToCss;

use crate::document_runtime::DomHandle;

/// Whether animating `property` leaves every box and hit-test result as is.
fn is_paint_only_property(property: &str) -> bool {
    matches!(
        property,
        "color"
            | "background-color"
            | "border-top-color"
            | "border-right-color"
            | "border-bottom-color"
            | "border-left-color"
            | "outline-color"
            | "text-decoration-color"
            | "caret-color"
            | "column-rule-color"
    )
}

/// The elements one clock sample must restyle.
#[derive(Default)]
pub(super) struct TransitionSample {
    /// Elements with a moved value that can affect layout or hit testing.
    pub(super) layout_roots: Vec<DomHandle>,
    /// Elements whose moved values only change how they paint.
    pub(super) paint_roots: Vec<DomHandle>,
}

/// Seconds since the Unix epoch: the time base of the document animation clock.
pub(crate) fn animation_clock_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs_f64()
}

/// Stylo keeps `<time>` values as `f32` seconds; script sees the specified
/// value (Chromium 156: `getTiming().duration` is 400 for `400ms`, not
/// 400.0000059604645).
fn specified_seconds(seconds: f64) -> f64 {
    let specified = seconds as f32;
    specified.to_string().parse().unwrap_or(seconds)
}

/// The transition events of CSS Transitions Level 2 §6.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CssTransitionEventKind {
    Run,
    Start,
    End,
    Cancel,
}

impl CssTransitionEventKind {
    pub(crate) fn event_type(self) -> &'static str {
        match self {
            Self::Run => "transitionrun",
            Self::Start => "transitionstart",
            Self::End => "transitionend",
            Self::Cancel => "transitioncancel",
        }
    }
}

/// A transition event, or for a cancel that script cannot observe as an
/// event, only the `Animation` state change (`dispatch` is false).
#[derive(Clone, Debug)]
pub(crate) struct CssTransitionEvent {
    pub(crate) id: u64,
    pub(crate) target: DomHandle,
    pub(crate) kind: CssTransitionEventKind,
    pub(crate) property: String,
    /// Seconds, as `TransitionEvent.elapsedTime`.
    pub(crate) elapsed_time: f64,
    pub(crate) dispatch: bool,
}

/// The phase of a transition's animation effect, which decides the
/// transition events (CSS Transitions Level 2 §6.1).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransitionPhase {
    /// No event has been queued yet.
    Idle,
    Before,
    Active,
    After,
}

/// What script can observe of one CSS transition.
#[derive(Clone, Debug)]
pub(crate) struct CssTransitionRecord {
    pub(crate) id: u64,
    pub(crate) target: DomHandle,
    pub(crate) property: String,
    /// The style change event that started this transition; transitions
    /// started by the same one sort by property name (CSS Transitions
    /// Level 2 §5.1, "animation composite order").
    pub(crate) generation: u64,
    /// Clock seconds at which the active interval starts: the style change
    /// event plus the transition delay.
    pub(crate) start_time: f64,
    /// Seconds.
    pub(crate) delay: f64,
    /// Seconds.
    pub(crate) duration: f64,
    pub(crate) easing: String,
    pub(crate) from: String,
    pub(crate) to: String,
    phase: TransitionPhase,
    /// The start time still waits for the first sample after the style
    /// change event.
    start_pending: bool,
}

impl CssTransitionRecord {
    fn phase_at(&self, now: f64) -> TransitionPhase {
        if now < self.start_time {
            TransitionPhase::Before
        } else if now < self.start_time + self.duration {
            TransitionPhase::Active
        } else {
            TransitionPhase::After
        }
    }

    /// `elapsedTime` at the start (`at_end == false`) or end of the active
    /// interval.
    fn interval_elapsed_time(&self, at_end: bool) -> f64 {
        let start = (-self.delay).clamp(0.0, self.duration);
        if at_end { self.duration } else { start }
    }

    fn event(
        &self,
        kind: CssTransitionEventKind,
        elapsed_time: f64,
        dispatch: bool,
    ) -> CssTransitionEvent {
        CssTransitionEvent {
            id: self.id,
            target: self.target,
            kind,
            property: self.property.clone(),
            elapsed_time,
            dispatch,
        }
    }

    /// Queues the events for a phase change to `phase`.
    fn advance_to(&mut self, phase: TransitionPhase, events: &mut Vec<CssTransitionEvent>) {
        use CssTransitionEventKind::{End, Run, Start};
        use TransitionPhase::{Active, After, Before, Idle};
        let start = self.interval_elapsed_time(false);
        let end = self.interval_elapsed_time(true);
        match (self.phase, phase) {
            (Idle, Before) => events.push(self.event(Run, start, true)),
            (Idle, Active) => {
                events.push(self.event(Run, start, true));
                events.push(self.event(Start, start, true));
            }
            (Idle, After) => {
                events.push(self.event(Run, start, true));
                events.push(self.event(Start, start, true));
                events.push(self.event(End, end, true));
            }
            (Before, Active) => events.push(self.event(Start, start, true)),
            (Before, After) => {
                events.push(self.event(Start, start, true));
                events.push(self.event(End, end, true));
            }
            (Active, After) => events.push(self.event(End, end, true)),
            _ => {}
        }
        if phase != Idle {
            self.phase = phase;
        }
    }
}

#[derive(Default)]
struct TransitionStore {
    next_id: u64,
    next_generation: u64,
    /// Per element, the record ids of Stylo's `ElementAnimationSet`
    /// transitions, index for index.
    ids: HashMap<OpaqueNode, Vec<u64>>,
    /// Transitions that have not finished or been canceled.
    records: BTreeMap<u64, CssTransitionRecord>,
    events: Vec<CssTransitionEvent>,
}

/// One Document's CSS transitions.
pub(crate) struct DocumentCssTransitions {
    animations: DocumentAnimationSet,
    clock: Cell<f64>,
    store: RefCell<TransitionStore>,
    /// Elements whose last resolved style specifies transitions: a frame
    /// runs their style change events.
    candidates: RefCell<HashSet<DomHandle>>,
    /// The style invalidation generation the last frame's style update saw.
    updated_generation: Cell<Option<u64>>,
    /// An animation frame callback batch ran since the last style update, so
    /// transitions that start now belong to that frame.
    in_frame: Cell<bool>,
}

impl Default for DocumentCssTransitions {
    fn default() -> Self {
        Self {
            animations: DocumentAnimationSet::default(),
            clock: Cell::new(animation_clock_now()),
            store: RefCell::default(),
            candidates: RefCell::default(),
            updated_generation: Cell::new(None),
            in_frame: Cell::new(false),
        }
    }
}

impl DocumentCssTransitions {
    /// The set and time a style resolution context resolves with. While no
    /// transition runs, the clock follows real time, so a transition starts
    /// at its style change event; while one runs, it only advances when
    /// sampled, so every element restyled at a sample shows the same time.
    pub(super) fn resolution_inputs(&self) -> (DocumentAnimationSet, f64) {
        if self.store.borrow().records.is_empty() && !self.in_frame.get() {
            self.clock.set(animation_clock_now());
        }
        (self.animations.clone(), self.clock.get())
    }

    /// Drops every transition, for a Document replacement.
    pub(super) fn clear(&self) {
        self.animations.sets.write().clear();
        *self.store.borrow_mut() = TransitionStore::default();
        self.candidates.borrow_mut().clear();
        self.updated_generation.set(None);
        self.in_frame.set(false);
    }

    /// Notes `target` when its newly resolved style specifies transitions.
    /// It stays a candidate while connected: a later style change, such as
    /// restoring a `transition: none` override, may start a transition.
    fn note_candidate(&self, target: DomHandle, specifies_transitions: bool) {
        if specifies_transitions {
            self.candidates.borrow_mut().insert(target);
        }
    }

    /// The elements whose style change events a frame runs: the connected
    /// candidates and those with running transitions. Disconnected
    /// candidates are forgotten.
    pub(super) fn style_update_targets(
        &self,
        connected: impl Fn(DomHandle) -> bool,
    ) -> Vec<DomHandle> {
        self.candidates
            .borrow_mut()
            .retain(|&target| connected(target));
        let mut targets = self
            .candidates
            .borrow()
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        targets.extend(
            self.store
                .borrow()
                .records
                .values()
                .map(|record| record.target),
        );
        targets.into_iter().collect()
    }

    /// The elements with running transitions.
    pub(super) fn running_targets(&self) -> Vec<DomHandle> {
        self.store
            .borrow()
            .records
            .values()
            .map(|record| record.target)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }

    pub(super) fn has_candidates(&self) -> bool {
        !self.candidates.borrow().is_empty() || !self.store.borrow().records.is_empty()
    }

    pub(super) fn updated_generation(&self) -> Option<u64> {
        self.updated_generation.get()
    }

    pub(super) fn set_updated_generation(&self, generation: u64) {
        self.updated_generation.set(Some(generation));
    }

    pub(super) fn begin_animation_frame(&self) {
        self.in_frame.set(true);
    }

    /// Ends the animation frame callback batch's part of a frame: the
    /// transitions its callbacks started start at the frame's time, which the
    /// clock still holds.
    pub(super) fn end_animation_frame(&self) {
        if self.in_frame.replace(false) {
            for record in self.store.borrow_mut().records.values_mut() {
                record.start_pending = false;
            }
        }
    }

    pub(super) fn in_animation_frame(&self) -> bool {
        self.in_frame.get()
    }

    /// Cancels the transitions of `target`, whose element is no longer
    /// rendered (CSS Transitions Level 1 §3: an element that is not
    /// rendered, through `display: none` on it or an ancestor, or removal,
    /// cancels its running transitions). Returns whether any was canceled.
    pub(super) fn cancel_for_element(&self, target: DomHandle) -> bool {
        let ids = {
            let store = self.store.borrow();
            store
                .records
                .values()
                .filter(|record| record.target == target)
                .map(|record| record.id)
                .collect::<Vec<_>>()
        };
        for id in &ids {
            self.settle(*id, false);
        }
        !ids.is_empty()
    }

    pub(crate) fn has_pending_events(&self) -> bool {
        !self.store.borrow().events.is_empty()
    }

    pub(crate) fn take_events(&self) -> Vec<CssTransitionEvent> {
        std::mem::take(&mut self.store.borrow_mut().events)
    }

    /// Clock seconds of the next phase change, when a transition event or
    /// a finished `Animation` is due.
    pub(crate) fn next_phase_change(&self) -> Option<f64> {
        self.store
            .borrow()
            .records
            .values()
            .map(|record| {
                if self.clock.get() < record.start_time {
                    record.start_time
                } else {
                    record.start_time + record.duration
                }
            })
            .min_by(f64::total_cmp)
    }

    /// The running transitions of `target`, in composite order.
    pub(crate) fn records_for_element(&self, target: DomHandle) -> Vec<CssTransitionRecord> {
        let mut records = self
            .store
            .borrow()
            .records
            .values()
            .filter(|record| record.target == target)
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|left, right| {
            left.generation
                .cmp(&right.generation)
                .then_with(|| left.property.cmp(&right.property))
        });
        records
    }

    /// Advances the clock to `now`, finishes the transitions that ended, and
    /// cancels those of elements for which `connected` is false. Returns the
    /// elements whose transition values changed, which must restyle.
    pub(super) fn sample(
        &self,
        now: f64,
        connected: impl Fn(DomHandle) -> bool,
    ) -> TransitionSample {
        let mut store = self.store.borrow_mut();
        if store.records.is_empty() {
            self.clock.set(now);
            return TransitionSample::default();
        }
        let advanced = now > self.clock.get();
        if advanced {
            self.clock.set(now);
        }
        let now = self.clock.get();
        let store = &mut *store;
        let mut restyle = TransitionSample::default();
        let mut sets = self.animations.sets.write();
        let mut removed = Vec::new();
        for (key, set) in sets.iter_mut() {
            let Some(ids) = store.ids.get(&key.node) else {
                continue;
            };
            let target = DomHandle::new(key.node.0);
            if !connected(target) {
                for (transition, id) in set.transitions.iter_mut().zip(ids) {
                    transition.state = AnimationState::Canceled;
                    if let Some(record) = store.records.remove(id) {
                        // Chromium 156 fires transitioncancel at a removed
                        // element whose transition had started.
                        let elapsed = (now - record.start_time).clamp(0.0, record.duration);
                        let dispatch = record.phase != TransitionPhase::Idle;
                        store.events.push(record.event(
                            CssTransitionEventKind::Cancel,
                            elapsed,
                            dispatch,
                        ));
                    }
                }
                removed.push(key.clone());
                continue;
            }
            let mut repaint = false;
            let mut relayout = false;
            for (transition, id) in set.transitions.iter_mut().zip(ids) {
                if !matches!(
                    transition.state,
                    AnimationState::Pending | AnimationState::Running
                ) {
                    continue;
                }
                let Some(record) = store.records.get_mut(id) else {
                    continue;
                };
                // A new transition's pending play task resolves its start
                // time at the next frame (Web Animations, "play an
                // animation"), not at the last sampled time, so its value
                // stays the one its style change event resolved.
                let start_resolved = record.start_pending;
                if start_resolved {
                    transition.start_time = now + transition.delay;
                    record.start_time = transition.start_time;
                    record.start_pending = false;
                }
                let phase = record.phase_at(now);
                let paint_only = is_paint_only_property(&record.property);
                let value_moved = advanced && !start_resolved && now > record.start_time;
                record.advance_to(phase, &mut store.events);
                if phase == TransitionPhase::Active {
                    transition.state = AnimationState::Running;
                }
                if phase == TransitionPhase::After {
                    // The end event queued above also settles `finished`.
                    transition.state = AnimationState::Finished;
                    store.records.remove(id);
                }
                if phase == TransitionPhase::After || value_moved {
                    repaint |= paint_only;
                    relayout |= !paint_only;
                }
            }
            if relayout {
                restyle.layout_roots.push(target);
            } else if repaint {
                restyle.paint_roots.push(target);
            }
        }
        for key in removed {
            sets.remove(&key);
            store.ids.remove(&key.node);
        }
        restyle
    }

    /// Ends transition `id` now, as `Animation.finish()` does. Returns its
    /// element, which must restyle.
    pub(crate) fn finish(&self, id: u64) -> Option<DomHandle> {
        self.settle(id, true)
    }

    /// Cancels transition `id`, as `Animation.cancel()` does. Returns its
    /// element, which must restyle.
    pub(crate) fn cancel(&self, id: u64) -> Option<DomHandle> {
        self.settle(id, false)
    }

    fn settle(&self, id: u64, finish: bool) -> Option<DomHandle> {
        let mut store = self.store.borrow_mut();
        let store = &mut *store;
        let mut record = store.records.remove(&id)?;
        let node = OpaqueNode(record.target.index());
        let mut sets = self.animations.sets.write();
        let key = AnimationSetKey::new_for_non_pseudo(node);
        if let (Some(set), Some(ids)) = (sets.get_mut(&key), store.ids.get(&node))
            && let Some(index) = ids.iter().position(|candidate| *candidate == id)
            && let Some(transition) = set.transitions.get_mut(index)
        {
            // A completed transition, either way: it stops contributing its
            // value, and the restyle that follows does not start a new
            // transition to the same end value from the stale before-change
            // style (Chromium 156 keeps `left: 400px` after `cancel()`).
            transition.state = AnimationState::Finished;
        }
        let now = self.clock.get();
        if finish {
            record.advance_to(TransitionPhase::After, &mut store.events);
        } else {
            let elapsed = (now - record.start_time).clamp(0.0, record.duration);
            let dispatch = record.phase != TransitionPhase::Idle;
            store
                .events
                .push(record.event(CssTransitionEventKind::Cancel, elapsed, dispatch));
        }
        Some(record.target)
    }

    /// Records the transitions that the style change event just processed
    /// for `key` started or canceled, and drops the finished and canceled
    /// ones from Stylo's set, as Servo's `process_animations_for_style` does.
    fn sync_after_style_change(&self, key: &AnimationSetKey, target: DomHandle) {
        let mut store = self.store.borrow_mut();
        let store = &mut *store;
        let mut sets = self.animations.sets.write();
        let Some(set) = sets.get_mut(key) else {
            if let Some(ids) = store.ids.remove(&key.node) {
                for id in ids {
                    store.records.remove(&id);
                }
            }
            return;
        };
        let now = self.clock.get();
        let mut ids = store.ids.remove(&key.node).unwrap_or_default();
        let generation = store.next_generation;
        let mut started = false;
        for transition in set.transitions.iter_mut().skip(ids.len()) {
            transition.is_new = false;
            store.next_id += 1;
            let id = store.next_id;
            ids.push(id);
            started = true;
            let animation = &transition.property_animation;
            let mut from = String::new();
            let _ = animation.from_value().uncompute().to_css(&mut from);
            let mut to = String::new();
            let _ = animation.to_value().uncompute().to_css(&mut to);
            store.records.insert(
                id,
                CssTransitionRecord {
                    id,
                    target,
                    property: animation.property_id().name().into_owned(),
                    generation,
                    start_time: transition.start_time,
                    delay: specified_seconds(transition.delay),
                    duration: specified_seconds(animation.duration),
                    easing: animation.timing_function().to_css_string(),
                    from,
                    to,
                    phase: TransitionPhase::Idle,
                    start_pending: true,
                },
            );
        }
        if started {
            store.next_generation += 1;
        }
        for (transition, id) in set.transitions.iter().zip(&ids) {
            if transition.state != AnimationState::Canceled {
                continue;
            }
            if let Some(record) = store.records.remove(id) {
                let elapsed = (now - record.start_time).clamp(0.0, record.duration);
                let dispatch = record.phase != TransitionPhase::Idle;
                store
                    .events
                    .push(record.event(CssTransitionEventKind::Cancel, elapsed, dispatch));
            }
        }
        // A transition that starts queues its run event (and any later one
        // that is already due) at its style change event.
        for id in &ids {
            if let Some(record) = store.records.get_mut(id)
                && record.phase == TransitionPhase::Idle
            {
                let phase = record.phase_at(now);
                record.advance_to(phase, &mut store.events);
            }
        }
        let mut index = 0;
        ids.retain(|_| {
            let keep = matches!(
                set.transitions[index].state,
                AnimationState::Pending | AnimationState::Running
            );
            index += 1;
            keep
        });
        set.transitions.retain(|transition| {
            matches!(
                transition.state,
                AnimationState::Pending | AnimationState::Running
            )
        });
        if set.transitions.is_empty() {
            sets.remove(key);
        } else {
            store.ids.insert(key.node, ids);
        }
    }
}

/// Runs the style change event for `element`, whose primary style changed
/// from `old` to the one in `styles`: starts, reverses or cancels its
/// transitions and replaces the primary style with one that carries their
/// current values.
pub(super) fn process_css_transitions<E>(
    transitions: &DocumentCssTransitions,
    context: &mut StyleContext<E>,
    element: E,
    old: Option<ServoArc<ComputedValues>>,
    styles: &mut ElementStyles,
) where
    E: TElement,
{
    let Some(new_primary) = styles.primary.take() else {
        return;
    };
    let key = AnimationSetKey::new_for_non_pseudo(element.as_node().opaque());
    transitions.note_candidate(
        DomHandle::new(element.as_node().debug_id()),
        new_primary.get_ui().specifies_transitions(),
    );
    if old.is_none()
        || (!new_primary.get_ui().specifies_transitions()
            && !context.shared.animations.sets.read().contains_key(&key))
    {
        styles.primary = Some(new_primary);
        return;
    }
    let primary =
        element.process_transitions_for_resolved_style(context, old.as_ref(), new_primary);
    styles.primary = Some(primary);
    transitions.sync_after_style_change(&key, DomHandle::new(element.as_node().debug_id()));
}
