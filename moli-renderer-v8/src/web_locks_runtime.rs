//! Renderer-neutral Web Locks lock manager state.
//!
//! One registry is shared by every Page in a browser-context runtime, so locks
//! are scoped to the storage key (the Web Locks "storage bucket") rather than to
//! one Page. The registry owns only queue state: request ids, held locks, and
//! FIFO request queues. JavaScript callbacks and promises stay in the Host that
//! created the request; the generic owner `O` is the embedding layer's handle
//! for waking that Host when its request is granted or stolen.
//!
//! Every mutating method returns the wake actions instead of performing them.
//! The registry Mutex is released before the caller dispatches any action, so
//! a dispatch that posts into another Page (or recursively re-enters the
//! registry after a closed route) can never run while the Mutex is held.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use moli_storage_key::MoliStorageKey;
use parking_lot::Mutex;

use crate::{
    native_bridge::WindowDocumentTaskTarget,
    page_task_queue::{
        RendererPageMiscPlatformApiSender, RendererPageMiscPlatformApiTaskId,
        RendererPageMiscPlatformApiTaskKind,
    },
};

/// Registry-unique id of one lock request (and of the lock it becomes).
pub(crate) type WebLockRequestId = u64;
/// Registry-unique id of one lock-holding client (a Window execution context).
pub(crate) type WebLockClientId = u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default, moli_webidl::WebIdlEnum)]
#[webidl(name = "LockMode", rename_all = "kebab-case")]
pub(crate) enum WebLockMode {
    #[default]
    Exclusive,
    Shared,
}

impl WebLockMode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Exclusive => "exclusive",
            Self::Shared => "shared",
        }
    }
}

/// One request as stored in a queue or the held set.
#[derive(Debug)]
pub(crate) struct WebLockRequest<O> {
    pub(crate) id: WebLockRequestId,
    pub(crate) client: WebLockClientId,
    /// `LockInfo.clientId` reported by `query()`.
    pub(crate) client_label: String,
    pub(crate) name: String,
    pub(crate) mode: WebLockMode,
    pub(crate) owner: O,
}

/// Work the caller must perform after the registry Mutex is released.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum WebLockAction<O> {
    /// The request became a held lock; its callback must run.
    Granted { owner: O, request: WebLockRequestId },
    /// The held lock was released by `steal`; its promise rejects with AbortError.
    Stolen { owner: O, request: WebLockRequestId },
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum WebLockRequestOutcome<O> {
    /// The request entered the queue (and was possibly granted at once).
    Queued(Vec<WebLockAction<O>>),
    /// `ifAvailable` was set and the lock could not be granted now. Nothing
    /// was stored; the callback runs with `null`.
    NotAvailable,
}

/// `LockInfo` snapshot entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WebLockInfo {
    pub(crate) name: String,
    pub(crate) mode: WebLockMode,
    pub(crate) client_id: String,
}

#[derive(Debug)]
struct LockManagerState<O> {
    /// Held locks in grant order (the spec's ordered held lock set).
    held: Vec<WebLockRequest<O>>,
    /// Lock request queue map in name insertion order.
    queues: Vec<(String, VecDeque<WebLockRequest<O>>)>,
}

impl<O> Default for LockManagerState<O> {
    fn default() -> Self {
        Self {
            held: Vec::new(),
            queues: Vec::new(),
        }
    }
}

impl<O> LockManagerState<O> {
    fn queue_mut(&mut self, name: &str) -> &mut VecDeque<WebLockRequest<O>> {
        let index = match self.queues.iter().position(|(queued, _)| queued == name) {
            Some(index) => index,
            None => {
                self.queues.push((name.to_owned(), VecDeque::new()));
                self.queues.len() - 1
            }
        };
        &mut self.queues[index].1
    }

    fn queue_is_empty(&self, name: &str) -> bool {
        self.queues
            .iter()
            .find(|(queued, _)| queued == name)
            .is_none_or(|(_, queue)| queue.is_empty())
    }

    fn compatible_with_held(&self, name: &str, mode: WebLockMode) -> bool {
        let mut held = self.held.iter().filter(|lock| lock.name == name);
        match mode {
            WebLockMode::Exclusive => held.next().is_none(),
            WebLockMode::Shared => held.all(|lock| lock.mode == WebLockMode::Shared),
        }
    }

    /// "Process the lock request queue" for one name.
    fn process(&mut self, name: &str, actions: &mut Vec<WebLockAction<O>>)
    where
        O: Clone,
    {
        loop {
            let Some(index) = self.queues.iter().position(|(queued, _)| queued == name) else {
                return;
            };
            let Some(front) = self.queues[index].1.front() else {
                self.queues.remove(index);
                return;
            };
            if !self.compatible_with_held(name, front.mode) {
                return;
            }
            let request = self.queues[index]
                .1
                .pop_front()
                .expect("front request was just observed");
            actions.push(WebLockAction::Granted {
                owner: request.owner.clone(),
                request: request.id,
            });
            self.held.push(request);
        }
    }

    fn is_empty(&self) -> bool {
        self.held.is_empty() && self.queues.iter().all(|(_, queue)| queue.is_empty())
    }
}

/// Lock managers for every storage key of one browser-context runtime.
#[derive(Debug)]
pub(crate) struct WebLockRegistry<O> {
    managers: Mutex<BTreeMap<MoliStorageKey, LockManagerState<O>>>,
    next_id: AtomicU64,
}

impl<O> Default for WebLockRegistry<O> {
    fn default() -> Self {
        Self {
            managers: Mutex::default(),
            next_id: AtomicU64::default(),
        }
    }
}

impl<O: Clone> WebLockRegistry<O> {
    /// Allocate a registry-unique request or client id.
    pub(crate) fn next_id(&self) -> u64 {
        self.next_id
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1)
    }

    /// "Request a lock" after the binding has validated the options.
    pub(crate) fn request(
        &self,
        storage_key: MoliStorageKey,
        request: WebLockRequest<O>,
        if_available: bool,
        steal: bool,
    ) -> WebLockRequestOutcome<O> {
        let mut managers = self.managers.lock();
        let manager = managers.entry(storage_key).or_default();
        let name = request.name.clone();
        let mut actions = Vec::new();
        if steal {
            let mut index = 0;
            while index < manager.held.len() {
                if manager.held[index].name == name {
                    let stolen = manager.held.remove(index);
                    actions.push(WebLockAction::Stolen {
                        owner: stolen.owner,
                        request: stolen.id,
                    });
                } else {
                    index += 1;
                }
            }
            manager.queue_mut(&name).push_front(request);
        } else if if_available
            && !(manager.queue_is_empty(&name) && manager.compatible_with_held(&name, request.mode))
        {
            return WebLockRequestOutcome::NotAvailable;
        } else {
            manager.queue_mut(&name).push_back(request);
        }
        manager.process(&name, &mut actions);
        WebLockRequestOutcome::Queued(actions)
    }

    /// Remove a still-pending request after its signal aborted.
    ///
    /// Returns `None` when the request is no longer pending (already granted
    /// or released), in which case the abort has no effect.
    pub(crate) fn abort_pending(&self, request: WebLockRequestId) -> Option<Vec<WebLockAction<O>>> {
        let mut managers = self.managers.lock();
        for manager in managers.values_mut() {
            let found = manager.queues.iter_mut().find_map(|(name, queue)| {
                let index = queue.iter().position(|queued| queued.id == request)?;
                queue.remove(index);
                Some(name.clone())
            });
            if let Some(name) = found {
                let mut actions = Vec::new();
                manager.process(&name, &mut actions);
                return Some(actions);
            }
        }
        None
    }

    /// Release one held lock, granting whatever it was blocking.
    pub(crate) fn release(&self, request: WebLockRequestId) -> Vec<WebLockAction<O>> {
        let mut managers = self.managers.lock();
        let mut actions = Vec::new();
        let mut emptied = None;
        for (key, manager) in managers.iter_mut() {
            if let Some(index) = manager.held.iter().position(|lock| lock.id == request) {
                let released = manager.held.remove(index);
                manager.process(&released.name, &mut actions);
                if manager.is_empty() {
                    emptied = Some(key.clone());
                }
                break;
            }
        }
        if let Some(key) = emptied {
            managers.remove(&key);
        }
        actions
    }

    /// Release every held lock and drop every pending request of one client.
    pub(crate) fn release_client(&self, client: WebLockClientId) -> Vec<WebLockAction<O>> {
        let mut managers = self.managers.lock();
        let mut actions = Vec::new();
        for manager in managers.values_mut() {
            let mut names = Vec::new();
            manager.held.retain(|lock| {
                let keep = lock.client != client;
                if !keep {
                    names.push(lock.name.clone());
                }
                keep
            });
            for (name, queue) in &mut manager.queues {
                let before = queue.len();
                queue.retain(|queued| queued.client != client);
                if queue.len() != before {
                    names.push(name.clone());
                }
            }
            names.dedup();
            for name in names {
                manager.process(&name, &mut actions);
            }
        }
        managers.retain(|_, manager| !manager.is_empty());
        actions
    }

    /// `query()` snapshot: (held, pending) for one storage key.
    pub(crate) fn snapshot(
        &self,
        storage_key: &MoliStorageKey,
    ) -> (Vec<WebLockInfo>, Vec<WebLockInfo>) {
        let managers = self.managers.lock();
        let Some(manager) = managers.get(storage_key) else {
            return (Vec::new(), Vec::new());
        };
        let info = |request: &WebLockRequest<O>| WebLockInfo {
            name: request.name.clone(),
            mode: request.mode,
            client_id: request.client_label.clone(),
        };
        let held = manager.held.iter().map(info).collect();
        let pending = manager
            .queues
            .iter()
            .flat_map(|(_, queue)| queue.iter().map(info))
            .collect();
        (held, pending)
    }
}

pub(crate) type SharedWebLockRegistry<O> = Arc<WebLockRegistry<O>>;

/// Renderer wake handle for one Window-owned request: the requesting Page's
/// miscellaneous-platform task route, the exact Window/Document target, and
/// the two task ids its Host reserved for the grant and the steal.
#[derive(Clone, Debug)]
pub(crate) struct WebLockOwner {
    pub(crate) sender: RendererPageMiscPlatformApiSender,
    pub(crate) target: WindowDocumentTaskTarget,
    pub(crate) grant_task: RendererPageMiscPlatformApiTaskId,
    pub(crate) steal_task: RendererPageMiscPlatformApiTaskId,
}

pub(crate) type RendererWebLockRegistry = WebLockRegistry<WebLockOwner>;
/// Registry shared by every Page of one browser-context runtime.
pub(crate) type SharedRendererWebLockRegistry = SharedWebLockRegistry<WebLockOwner>;

pub(crate) fn new_web_lock_registry() -> SharedRendererWebLockRegistry {
    Arc::new(RendererWebLockRegistry::default())
}

/// Post registry actions into their owners' Pages, in order.
///
/// Callers pass actions returned by a registry method, after that method has
/// released the registry Mutex. A grant whose Page route has closed belongs to
/// a client that is gone, so its lock is released and the resulting grants are
/// dispatched in turn.
pub(crate) fn dispatch_web_lock_actions(
    registry: &RendererWebLockRegistry,
    actions: Vec<WebLockAction<WebLockOwner>>,
) {
    let mut actions = VecDeque::from(actions);
    while let Some(action) = actions.pop_front() {
        match action {
            WebLockAction::Granted { owner, request } => {
                if owner
                    .sender
                    .send(
                        owner.target,
                        owner.grant_task,
                        RendererPageMiscPlatformApiTaskKind::WebLockGranted,
                    )
                    .is_err()
                {
                    actions.extend(registry.release(request));
                }
            }
            WebLockAction::Stolen { owner, .. } => {
                let _ = owner.sender.send(
                    owner.target,
                    owner.steal_task,
                    RendererPageMiscPlatformApiTaskKind::WebLockStolen,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    type Registry = WebLockRegistry<&'static str>;

    fn key(origin: &str) -> MoliStorageKey {
        MoliStorageKey::first_party_from_url(&Url::parse(origin).unwrap(), None)
    }

    fn request(
        registry: &Registry,
        client: WebLockClientId,
        name: &str,
        mode: WebLockMode,
        owner: &'static str,
    ) -> WebLockRequest<&'static str> {
        WebLockRequest {
            id: registry.next_id(),
            client,
            client_label: format!("client-{client}"),
            name: name.to_owned(),
            mode,
            owner,
        }
    }

    fn granted(actions: &[WebLockAction<&'static str>]) -> Vec<&'static str> {
        actions
            .iter()
            .filter_map(|action| match action {
                WebLockAction::Granted { owner, .. } => Some(*owner),
                WebLockAction::Stolen { .. } => None,
            })
            .collect()
    }

    fn queued(outcome: WebLockRequestOutcome<&'static str>) -> Vec<WebLockAction<&'static str>> {
        match outcome {
            WebLockRequestOutcome::Queued(actions) => actions,
            WebLockRequestOutcome::NotAvailable => panic!("request should queue"),
        }
    }

    #[test]
    fn exclusive_requests_are_granted_in_fifo_order() {
        let registry = Registry::default();
        let a = request(&registry, 1, "n", WebLockMode::Exclusive, "a");
        let a_id = a.id;
        assert_eq!(
            granted(&queued(registry.request(
                key("https://x.test"),
                a,
                false,
                false
            ))),
            ["a"]
        );
        let b = request(&registry, 1, "n", WebLockMode::Exclusive, "b");
        let b_id = b.id;
        assert!(queued(registry.request(key("https://x.test"), b, false, false)).is_empty());
        let c = request(&registry, 2, "n", WebLockMode::Exclusive, "c");
        assert!(queued(registry.request(key("https://x.test"), c, false, false)).is_empty());
        assert_eq!(granted(&registry.release(a_id)), ["b"]);
        assert_eq!(granted(&registry.release(b_id)), ["c"]);
    }

    #[test]
    fn shared_locks_coexist_and_block_a_later_exclusive_request() {
        let registry = Registry::default();
        let origin = key("https://x.test");
        let s1 = request(&registry, 1, "n", WebLockMode::Shared, "s1");
        let s1_id = s1.id;
        let s2 = request(&registry, 2, "n", WebLockMode::Shared, "s2");
        let s2_id = s2.id;
        assert_eq!(
            granted(&queued(registry.request(origin.clone(), s1, false, false))),
            ["s1"]
        );
        assert_eq!(
            granted(&queued(registry.request(origin.clone(), s2, false, false))),
            ["s2"]
        );
        let ex = request(&registry, 3, "n", WebLockMode::Exclusive, "ex");
        assert!(queued(registry.request(origin.clone(), ex, false, false)).is_empty());
        // A shared request queued behind the exclusive one waits its turn.
        let s3 = request(&registry, 4, "n", WebLockMode::Shared, "s3");
        assert!(queued(registry.request(origin, s3, false, false)).is_empty());
        assert!(registry.release(s1_id).is_empty());
        assert_eq!(granted(&registry.release(s2_id)), ["ex"]);
    }

    #[test]
    fn if_available_and_steal_follow_the_held_set() {
        let registry = Registry::default();
        let origin = key("https://x.test");
        let held = request(&registry, 1, "n", WebLockMode::Exclusive, "held");
        let held_id = held.id;
        queued(registry.request(origin.clone(), held, false, false));
        let probe = request(&registry, 2, "n", WebLockMode::Shared, "probe");
        assert_eq!(
            registry.request(origin.clone(), probe, true, false),
            WebLockRequestOutcome::NotAvailable
        );
        let other = request(&registry, 2, "other", WebLockMode::Exclusive, "other");
        assert_eq!(
            granted(&queued(registry.request(
                origin.clone(),
                other,
                true,
                false
            ))),
            ["other"]
        );

        let thief = request(&registry, 2, "n", WebLockMode::Exclusive, "thief");
        let actions = queued(registry.request(origin.clone(), thief, false, true));
        assert_eq!(
            actions,
            [
                WebLockAction::Stolen {
                    owner: "held",
                    request: held_id
                },
                WebLockAction::Granted {
                    owner: "thief",
                    request: held_id + 3
                },
            ]
        );
        // Releasing the stolen lock later is a no-op.
        assert!(registry.release(held_id).is_empty());
        let (held_names, pending) = registry.snapshot(&origin);
        assert_eq!(
            held_names
                .iter()
                .map(|info| info.name.as_str())
                .collect::<Vec<_>>(),
            ["other", "n"]
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn client_release_and_abort_unblock_waiters_and_storage_keys_are_separate() {
        let registry = Registry::default();
        let origin = key("https://x.test");
        let held = request(&registry, 1, "n", WebLockMode::Exclusive, "held");
        queued(registry.request(origin.clone(), held, false, false));
        let aborted = request(&registry, 2, "n", WebLockMode::Exclusive, "aborted");
        let aborted_id = aborted.id;
        queued(registry.request(origin.clone(), aborted, false, false));
        let waiter = request(&registry, 3, "n", WebLockMode::Exclusive, "waiter");
        queued(registry.request(origin.clone(), waiter, false, false));
        let elsewhere = request(&registry, 4, "n", WebLockMode::Exclusive, "elsewhere");
        assert_eq!(
            granted(&queued(registry.request(
                key("https://y.test"),
                elsewhere,
                false,
                false
            ))),
            ["elsewhere"]
        );
        let (_, pending) = registry.snapshot(&origin);
        assert_eq!(
            pending
                .iter()
                .map(|info| info.client_id.as_str())
                .collect::<Vec<_>>(),
            ["client-2", "client-3"]
        );
        assert_eq!(registry.abort_pending(aborted_id), Some(Vec::new()));
        assert_eq!(registry.abort_pending(aborted_id), None);
        assert_eq!(granted(&registry.release_client(1)), ["waiter"]);
    }
}
