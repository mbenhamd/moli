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
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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

/// Seconds since the Unix epoch, read from a monotonic clock: the time base
/// of the document animation clock. The epoch offset is taken once, so
/// wall-clock adjustments do not move animations (High Resolution Time §4:
/// document timelines use a monotonic clock).
pub(crate) fn animation_clock_now() -> f64 {
    static ANCHOR: OnceLock<(Instant, f64)> = OnceLock::new();
    let (instant, epoch) = ANCHOR.get_or_init(|| {
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs_f64();
        (Instant::now(), epoch)
    });
    epoch + instant.elapsed().as_secs_f64()
}

/// Transition ids are unique across Documents and Document replacements:
/// `CSSTransition` objects and `finish()`/`cancel()` find their transition by
/// id alone.
fn next_transition_id() -> u64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
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
    /// The Document whose style world ran the transition: its realm holds
    /// the transition's `CSSTransition`, even after the element moved.
    pub(crate) document: DomHandle,
    pub(crate) target: DomHandle,
    /// The style change event that started the transition, for composite
    /// order.
    pub(crate) generation: u64,
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
    /// The Document whose style world runs this transition.
    pub(crate) document: DomHandle,
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
            document: self.document,
            target: self.target,
            generation: self.generation,
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
    document: DomHandle,
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
    /// A frame's style update has resolved the document's initial content.
    document_styled: Cell<bool>,
    /// Subtrees the last frame's style update marked for styling but left
    /// unresolved: they are resolved before anything else runs, so their
    /// before-change styles are those of that frame.
    unresolved_frame_roots: RefCell<Vec<DomHandle>>,
    /// Elements whose published primary style is not a before-change style:
    /// it was resolved while not rendered, from the pre-layout container
    /// size estimate, or before a removal. Their next style change event starts
    /// no transition (CSS Transitions Level 1 §3: no before-change style).
    fresh: RefCell<HashSet<DomHandle>>,
    /// Some resolved style in this document specified transitions.
    transition_styles_seen: Cell<bool>,
    /// The document's style changes wait for a skipped frame's style
    /// update (`take_skipped_css_transition_frame_roots`).
    skipped_frame_armed: Cell<bool>,
}

impl DocumentCssTransitions {
    pub(super) fn new(document: DomHandle) -> Self {
        Self {
            document,
            animations: DocumentAnimationSet::default(),
            clock: Cell::new(animation_clock_now()),
            store: RefCell::default(),
            candidates: RefCell::default(),
            updated_generation: Cell::new(None),
            in_frame: Cell::new(false),
            document_styled: Cell::new(false),
            unresolved_frame_roots: RefCell::default(),
            fresh: RefCell::default(),
            transition_styles_seen: Cell::new(false),
            skipped_frame_armed: Cell::new(false),
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

    /// Drops every transition, for a Document replacement. The running ones
    /// are canceled: their `CSSTransition`s settle at the next transition
    /// task, without a `transitioncancel` (their elements left the
    /// Document with its content).
    pub(super) fn clear(&self) {
        self.animations.sets.write().clear();
        let mut store = self.store.borrow_mut();
        let previous = std::mem::take(&mut *store);
        store.events = previous.events;
        let now = self.clock.get();
        for record in previous.records.into_values() {
            let elapsed = (now - record.start_time).clamp(0.0, record.duration);
            store
                .events
                .push(record.event(CssTransitionEventKind::Cancel, elapsed, false));
        }
        drop(store);
        self.candidates.borrow_mut().clear();
        self.updated_generation.set(None);
        self.in_frame.set(false);
        self.document_styled.set(false);
        self.skipped_frame_armed.set(false);
        self.unresolved_frame_roots.borrow_mut().clear();
        self.fresh.borrow_mut().clear();
        self.transition_styles_seen.set(false);
    }

    #[cfg(test)]
    pub(super) fn fresh_count(&self) -> usize {
        self.fresh.borrow().len()
    }

    /// Marks `elements` as having no before-change style.
    pub(super) fn mark_fresh(&self, elements: impl IntoIterator<Item = DomHandle>) {
        self.fresh.borrow_mut().extend(elements);
    }

    fn consume_fresh(&self, element: DomHandle) {
        let mut fresh = self.fresh.borrow_mut();
        if !fresh.is_empty() {
            fresh.remove(&element);
        }
    }

    /// Marks the flat-tree descendants of `element` that have a resolved
    /// style as having no before-change style.
    fn mark_styled_descendants_fresh<E: TElement>(&self, element: E) {
        let mut pending = vec![element];
        let mut fresh = self.fresh.borrow_mut();
        while let Some(current) = pending.pop() {
            for child in current.traversal_children() {
                let Some(child) = child.as_element() else {
                    continue;
                };
                if child.has_data() {
                    fresh.insert(DomHandle::new(child.as_node().debug_id()));
                    pending.push(child);
                }
            }
        }
    }

    /// Cancels the running transitions of `element`'s flat-tree descendants.
    /// The walk stops once every running target is found.
    fn cancel_in_flat_tree_descendants<E: TElement>(&self, element: E) {
        let mut remaining = self
            .store
            .borrow()
            .records
            .values()
            .map(|record| record.target)
            .collect::<HashSet<_>>();
        let mut pending = vec![element];
        while !remaining.is_empty()
            && let Some(current) = pending.pop()
        {
            for child in current.traversal_children() {
                let Some(child) = child.as_element() else {
                    continue;
                };
                let handle = DomHandle::new(child.as_node().debug_id());
                if remaining.remove(&handle) {
                    self.cancel_for_element(handle);
                }
                pending.push(child);
            }
        }
    }

    /// Drops the finished and canceled transitions Stylo still keeps for
    /// `key`, with their ids.
    fn prune_settled(&self, key: &AnimationSetKey) {
        let mut sets = self.animations.sets.write();
        let Some(set) = sets.get_mut(key) else {
            return;
        };
        let mut store = self.store.borrow_mut();
        let running = |state: &AnimationState| {
            matches!(state, AnimationState::Pending | AnimationState::Running)
        };
        if let Some(ids) = store.ids.get_mut(&key.node) {
            let mut index = 0;
            ids.retain(|_| {
                let keep = set
                    .transitions
                    .get(index)
                    .is_some_and(|transition| running(&transition.state));
                index += 1;
                keep
            });
        }
        set.transitions
            .retain(|transition| running(&transition.state));
        if set.transitions.is_empty() {
            sets.remove(key);
            store.ids.remove(&key.node);
        }
    }

    /// Whether any resolved style of the document specified transitions, so
    /// a frame must run style change events.
    pub(super) fn transition_styles_seen(&self) -> bool {
        self.transition_styles_seen.get()
    }

    /// The elements whose style specifies transitions, which a removal of
    /// their subtree leaves without a before-change style.
    pub(super) fn candidates_and_running_targets(&self) -> Vec<DomHandle> {
        let mut targets = self.candidates.borrow().clone();
        targets.extend(
            self.store
                .borrow()
                .records
                .values()
                .map(|record| record.target),
        );
        targets.into_iter().collect()
    }

    /// Whether the document's initial content still awaits its first frame
    /// style update; marks it as taken.
    pub(super) fn take_unstyled_document(&self) -> bool {
        !self.document_styled.replace(true)
    }

    pub(super) fn arm_skipped_frame(&self) {
        self.skipped_frame_armed.set(true);
    }

    pub(super) fn take_skipped_frame_armed(&self) -> bool {
        self.skipped_frame_armed.replace(false)
    }

    pub(super) fn defer_unresolved_frame_roots(&self, roots: impl IntoIterator<Item = DomHandle>) {
        self.unresolved_frame_roots.borrow_mut().extend(roots);
    }

    pub(super) fn take_unresolved_frame_roots(&self) -> Vec<DomHandle> {
        std::mem::take(&mut *self.unresolved_frame_roots.borrow_mut())
    }

    pub(super) fn has_unresolved_frame_roots(&self) -> bool {
        !self.unresolved_frame_roots.borrow().is_empty()
    }

    /// Notes `target` when its newly resolved style specifies transitions.
    /// It stays a candidate while connected: a later style change, such as
    /// restoring a `transition: none` override, may start a transition.
    fn note_candidate(&self, target: DomHandle, specifies_transitions: bool) {
        if specifies_transitions {
            self.candidates.borrow_mut().insert(target);
            self.transition_styles_seen.set(true);
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
        // A disconnected element's next insertion marks it again.
        self.fresh.borrow_mut().retain(|&target| connected(target));
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
                .ids
                .get(&OpaqueNode(target.index()))
                .into_iter()
                .flatten()
                .copied()
                .filter(|id| store.records.contains_key(id))
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

    /// The undelivered end and cancel events, which settle `CSSTransition`s.
    pub(crate) fn pending_settlements(&self) -> Vec<CssTransitionEvent> {
        self.store
            .borrow()
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event.kind,
                    CssTransitionEventKind::End | CssTransitionEventKind::Cancel
                )
            })
            .cloned()
            .collect()
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
        let store = self.store.borrow();
        let mut records = store
            .ids
            .get(&OpaqueNode(target.index()))
            .into_iter()
            .flatten()
            .filter_map(|id| store.records.get(id))
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
            // Settled transitions stay in Stylo's set until their element's
            // next style change; a removed element has none.
            if !store.ids.is_empty() {
                let mut sets = self.animations.sets.write();
                sets.retain(|key, _| connected(DomHandle::new(key.node.0)));
                store
                    .ids
                    .retain(|node, _| connected(DomHandle::new(node.0)));
            }
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
            let id = next_transition_id();
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
                    document: self.document,
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
    let handle = DomHandle::new(element.as_node().debug_id());
    let key = AnimationSetKey::new_for_non_pseudo(element.as_node().opaque());
    // An element that stops being `display: none` reveals a subtree whose
    // styles were resolved while not rendered: none of them is a
    // before-change style.
    if old
        .as_ref()
        .is_some_and(|old| old.get_box().clone_display().is_none())
        && !new_primary.get_box().clone_display().is_none()
    {
        transitions.mark_styled_descendants_fresh(element);
    }
    // An element that becomes `display: none` stops rendering its subtree:
    // the transitions there are canceled, as Blink cancels them when it
    // detaches the subtree's layout.
    if new_primary.get_box().clone_display().is_none()
        && old
            .as_ref()
            .is_some_and(|old| !old.get_box().clone_display().is_none())
    {
        transitions.cancel_in_flat_tree_descendants(element);
    }
    let specifies_transitions = new_primary.get_ui().specifies_transitions();
    transitions.note_candidate(handle, specifies_transitions);
    if !specifies_transitions && !context.shared.animations.sets.read().contains_key(&key) {
        // This style is a before-change style from now on.
        transitions.consume_fresh(handle);
        styles.primary = Some(new_primary);
        return;
    }
    // An element in a `display: none` subtree is not rendered: it has no
    // before-change style and starts no transition. The frame's style update
    // cancels the transitions it still runs.
    if has_undisplayed_flat_tree_ancestor(element) {
        transitions.fresh.borrow_mut().insert(handle);
        styles.primary = Some(new_primary);
        return;
    }
    let fresh = transitions.fresh.borrow_mut().remove(&handle);
    if old.is_none() || fresh {
        // Without a before-change style, the transitions this element's
        // earlier styles settled no longer match a style change.
        transitions.prune_settled(&key);
        styles.primary = Some(new_primary);
        return;
    }
    let primary =
        element.process_transitions_for_resolved_style(context, old.as_ref(), new_primary);
    styles.primary = Some(primary);
    transitions.sync_after_style_change(&key, DomHandle::new(element.as_node().debug_id()));
}

/// Whether a flat-tree ancestor of `element` has a resolved `display: none`
/// style. Ancestors resolve before their descendants.
fn has_undisplayed_flat_tree_ancestor<E: TElement>(element: E) -> bool {
    let mut current = element.traversal_parent();
    while let Some(ancestor) = current {
        let undisplayed = ancestor.borrow_data().is_some_and(|data| {
            data.styles
                .primary
                .as_ref()
                .is_some_and(|style| style.get_box().clone_display().is_none())
        });
        if undisplayed {
            return true;
        }
        current = ancestor.traversal_parent();
    }
    false
}
