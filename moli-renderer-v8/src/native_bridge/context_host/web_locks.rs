//! Window-side Web Locks request state.
//!
//! The browser-context registry owns lock queues; this Host owns the V8 half of
//! each request it created: the callback, the request promise, the optional
//! abort registration, and the two miscellaneous-platform task ids reserved
//! for the grant and the steal. Registry calls return wake actions, and every
//! dispatch below runs after the registry call has returned, so no grant or
//! steal is posted while the registry Mutex is held.

use super::*;
use crate::{
    page_task_queue::{RendererPageMiscPlatformApiTaskId, RendererPageMiscPlatformApiTaskKind},
    web_locks_runtime::{
        WebLockAction, WebLockClientId, WebLockInfo, WebLockMode, WebLockOwner, WebLockRequest,
        WebLockRequestId, WebLockRequestOutcome, dispatch_web_lock_actions,
    },
    window_webidl_callback::WindowWebIdlCallbackFunction,
};
use moli_storage_key::MoliStorageKey;

/// Why the current realm cannot use the lock manager.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WebLockClientError {
    /// No fully active Window/Document owns the calling realm.
    InvalidState,
    /// The calling Document has an opaque origin.
    Security,
    /// No random client id could be generated for the calling Window.
    ClientIdUnavailable,
}

/// The calling Window's lock-manager address.
pub(crate) struct WebLockClient {
    target: WindowDocumentTaskTarget,
    storage_key: MoliStorageKey,
    client: WebLockClientId,
    label: String,
}

/// A validated `LockManager.request()` call ready to enter the queue.
pub(crate) struct NewWebLockRequest<'s> {
    pub(crate) id: WebLockRequestId,
    pub(crate) name: String,
    pub(crate) mode: WebLockMode,
    pub(crate) if_available: bool,
    pub(crate) steal: bool,
    /// The page-supplied `LockGrantedCallback`, bound to its Window.
    pub(crate) callback: WindowWebIdlCallbackFunction,
    pub(crate) resolver: v8::Local<'s, v8::PromiseResolver>,
    pub(crate) abort: Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>)>,
}

/// What the grant task needs to invoke one request's callback.
pub(crate) struct WebLockGrant {
    pub(crate) name: String,
    pub(crate) mode: WebLockMode,
    pub(crate) not_available: bool,
    pub(crate) callback: WindowWebIdlCallbackFunction,
    pub(crate) abort: Option<(v8::Global<v8::Object>, v8::Global<v8::Function>)>,
}

struct WebLockRequestState {
    client: WebLockClientId,
    name: String,
    mode: WebLockMode,
    /// Taken by the grant task; the callback is invoked at most once.
    callback: Option<WindowWebIdlCallbackFunction>,
    resolver: v8::Global<v8::PromiseResolver>,
    abort: Option<(v8::Global<v8::Object>, v8::Global<v8::Function>)>,
    grant_task: RendererPageMiscPlatformApiTaskId,
    steal_task: RendererPageMiscPlatformApiTaskId,
    /// `ifAvailable` found the lock unavailable: the callback receives `null`
    /// and the registry holds nothing for this request.
    not_available: bool,
    granted: bool,
    stolen: bool,
}

#[derive(Default)]
pub(crate) struct WebLockHostState {
    requests: HashMap<WebLockRequestId, WebLockRequestState>,
    tasks: HashMap<
        RendererPageMiscPlatformApiTaskId,
        (
            WindowDocumentTaskTarget,
            RendererPageMiscPlatformApiTaskKind,
            WebLockRequestId,
        ),
    >,
    clients: HashMap<WindowExecutionContextIdentity, (WebLockClientId, String)>,
}

impl WebLockHostState {
    fn forget_request(&mut self, request: WebLockRequestId) -> Option<WebLockRequestState> {
        let state = self.requests.remove(&request)?;
        self.tasks.remove(&state.grant_task);
        self.tasks.remove(&state.steal_task);
        Some(state)
    }
}

fn new_client_label() -> Option<String> {
    let mut random_bytes = [0_u8; 16];
    moli_crypto::fill_secure_random(&mut random_bytes).ok()?;
    Some(
        uuid::Builder::from_random_bytes(random_bytes)
            .into_uuid()
            .to_string(),
    )
}

impl JsContextHost {
    #[cfg(test)]
    pub(crate) fn web_lock_request_count(&self) -> usize {
        self.web_locks.requests.len()
    }

    pub(crate) fn next_web_lock_request_id(&self) -> WebLockRequestId {
        self.web_lock_registry.next_id()
    }

    /// Resolve the current realm's lock-manager client. The caller enters the
    /// LockManager's relevant realm first.
    pub(crate) fn current_web_lock_client(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
    ) -> Result<WebLockClient, WebLockClientError> {
        let identity = self
            .current_runtime_window_execution_context_identity(scope)
            .ok_or(WebLockClientError::InvalidState)?;
        let target = self
            .current_window_document_task_target_for_dispatch_scope(identity.dispatch_scope())
            .ok_or(WebLockClientError::InvalidState)?;
        let storage_key = self
            .storage_context_for_window_execution_context_identity(identity)
            .map(|context| context.storage_key().clone())
            .ok_or(WebLockClientError::InvalidState)?;
        if storage_key.opaque_nonce().is_some() || storage_key.origin() == "null" {
            return Err(WebLockClientError::Security);
        }
        let (client, label) = match self.web_locks.clients.get(&identity) {
            Some(entry) => entry.clone(),
            None => {
                let label = new_client_label().ok_or(WebLockClientError::ClientIdUnavailable)?;
                let entry = (self.web_lock_registry.next_id(), label);
                self.web_locks.clients.insert(identity, entry.clone());
                entry
            }
        };
        Ok(WebLockClient {
            target,
            storage_key,
            client,
            label,
        })
    }

    pub(crate) fn web_lock_snapshot(
        &self,
        client: &WebLockClient,
    ) -> (Vec<WebLockInfo>, Vec<WebLockInfo>) {
        self.web_lock_registry.snapshot(&client.storage_key)
    }

    /// Queue one request. Returns false when the Page task route is closed and
    /// the request was dropped.
    pub(crate) fn submit_web_lock_request(
        &mut self,
        scope: &mut v8::PinScope<'_, '_>,
        client: WebLockClient,
        request: NewWebLockRequest<'_>,
    ) -> bool {
        let grant_task = self
            .misc_platform_api_tasks
            .allocate_task_id(RendererPageMiscPlatformApiTaskId::from_raw);
        let steal_task = self
            .misc_platform_api_tasks
            .allocate_task_id(RendererPageMiscPlatformApiTaskId::from_raw);
        let sender = self.page_misc_platform_api_sender();
        let id = request.id;
        self.web_locks.tasks.insert(
            grant_task,
            (
                client.target,
                RendererPageMiscPlatformApiTaskKind::WebLockGranted,
                id,
            ),
        );
        self.web_locks.tasks.insert(
            steal_task,
            (
                client.target,
                RendererPageMiscPlatformApiTaskKind::WebLockStolen,
                id,
            ),
        );
        self.web_locks.requests.insert(
            id,
            WebLockRequestState {
                client: client.client,
                name: request.name.clone(),
                mode: request.mode,
                callback: Some(request.callback),
                resolver: v8::Global::new(scope, request.resolver),
                abort: request.abort.map(|(signal, algorithm)| {
                    (
                        v8::Global::new(scope, signal),
                        v8::Global::new(scope, algorithm),
                    )
                }),
                grant_task,
                steal_task,
                not_available: false,
                granted: false,
                stolen: false,
            },
        );
        let owner = WebLockOwner {
            sender: sender.clone(),
            target: client.target,
            grant_task,
            steal_task,
        };
        let outcome = self.web_lock_registry.request(
            client.storage_key,
            WebLockRequest {
                id,
                client: client.client,
                client_label: client.label,
                name: request.name,
                mode: request.mode,
                owner,
            },
            request.if_available,
            request.steal,
        );
        match outcome {
            WebLockRequestOutcome::Queued(actions) => {
                // A request whose own grant could not be posted was released
                // by the registry and forgotten here; reject it.
                !self.post_web_lock_actions(actions).contains(&id)
            }
            WebLockRequestOutcome::NotAvailable => {
                if let Some(state) = self.web_locks.requests.get_mut(&id) {
                    state.not_available = true;
                }
                if sender
                    .send(
                        client.target,
                        grant_task,
                        RendererPageMiscPlatformApiTaskKind::WebLockGranted,
                    )
                    .is_ok()
                {
                    return true;
                }
                self.web_locks.forget_request(id);
                false
            }
        }
    }

    pub(super) fn current_pending_web_lock_task(
        &self,
        task_id: RendererPageMiscPlatformApiTaskId,
    ) -> Option<(
        WindowDocumentTaskTarget,
        RendererPageMiscPlatformApiTaskKind,
    )> {
        let (target, kind, _) = self.web_locks.tasks.get(&task_id)?;
        let current_target =
            self.current_window_document_task_target_for_dispatch_scope(target.dispatch_scope())?;
        Some((current_target, *kind))
    }

    pub(crate) fn take_pending_web_lock_task_for_exact_target(
        &mut self,
        task_id: RendererPageMiscPlatformApiTaskId,
        target: WindowDocumentTaskTarget,
        kind: RendererPageMiscPlatformApiTaskKind,
    ) -> Option<WebLockRequestId> {
        let (pending_target, pending_kind, request) = *self.web_locks.tasks.get(&task_id)?;
        if pending_target != target || pending_kind != kind {
            return None;
        }
        self.web_locks.tasks.remove(&task_id);
        Some(request)
    }

    /// Post registry actions after the registry call returned. A grant that
    /// could not be posted was already released by the registry; its request
    /// is forgotten here too when this Host created it. Returns those requests.
    fn post_web_lock_actions(
        &mut self,
        actions: Vec<WebLockAction<WebLockOwner>>,
    ) -> Vec<WebLockRequestId> {
        let unsent = dispatch_web_lock_actions(&self.web_lock_registry, actions);
        for request in &unsent {
            self.web_locks.forget_request(*request);
        }
        unsent
    }

    /// Drop one request whose callback will never run and return any lock it
    /// held or waited for to the queue. Used for a stale task, and for an
    /// authorized grant task (already taken from the task table) that found no
    /// realm: its request promise cannot be rejected without that realm.
    pub(super) fn retire_web_lock_request(&mut self, request: WebLockRequestId) {
        let Some(state) = self.web_locks.forget_request(request) else {
            return;
        };
        let mut actions = self
            .web_lock_registry
            .abort_pending(request)
            .unwrap_or_default();
        if !state.not_available {
            actions.extend(self.web_lock_registry.release(request));
        }
        self.post_web_lock_actions(actions);
    }

    /// A grant or steal task reached a stale Document: drop its request and
    /// return any lock it held to the queue.
    pub(super) fn discard_pending_web_lock_task(
        &mut self,
        task_id: RendererPageMiscPlatformApiTaskId,
    ) -> bool {
        let Some((_, _, request)) = self.web_locks.tasks.remove(&task_id) else {
            return false;
        };
        self.retire_web_lock_request(request);
        true
    }

    pub(crate) fn begin_web_lock_grant(
        &mut self,
        request: WebLockRequestId,
    ) -> Option<WebLockGrant> {
        let state = self.web_locks.requests.get_mut(&request)?;
        if state.granted {
            return None;
        }
        let callback = state.callback.take()?;
        state.granted = true;
        Some(WebLockGrant {
            name: state.name.clone(),
            mode: state.mode,
            not_available: state.not_available,
            callback,
            abort: state.abort.take(),
        })
    }

    /// The callback's result settled: release the lock and hand back the
    /// request promise unless a steal already rejected it.
    pub(crate) fn finish_web_lock_request(
        &mut self,
        request: WebLockRequestId,
    ) -> Option<v8::Global<v8::PromiseResolver>> {
        let state = self.web_locks.forget_request(request)?;
        if !state.not_available {
            let actions = self.web_lock_registry.release(request);
            self.post_web_lock_actions(actions);
        }
        (!state.stolen).then_some(state.resolver)
    }

    pub(crate) fn steal_web_lock_request(
        &mut self,
        request: WebLockRequestId,
    ) -> Option<v8::Global<v8::PromiseResolver>> {
        let state = self.web_locks.requests.get_mut(&request)?;
        if state.stolen {
            return None;
        }
        state.stolen = true;
        Some(state.resolver.clone())
    }

    /// The request's signal aborted. A request whose callback has not run yet
    /// is withdrawn: still queued, or already granted in the registry with its
    /// grant task not yet run (that task then finds no request and is
    /// discarded). Returns its promise resolver and signal.
    pub(crate) fn abort_pending_web_lock_request(
        &mut self,
        request: WebLockRequestId,
    ) -> Option<(v8::Global<v8::PromiseResolver>, v8::Global<v8::Object>)> {
        if self.web_locks.requests.get(&request)?.granted {
            return None;
        }
        let state = self.web_locks.forget_request(request)?;
        let mut actions = self
            .web_lock_registry
            .abort_pending(request)
            .unwrap_or_default();
        if !state.not_available {
            actions.extend(self.web_lock_registry.release(request));
        }
        self.post_web_lock_actions(actions);
        let (signal, _) = state.abort?;
        Some((state.resolver, signal))
    }

    fn release_web_lock_clients(
        &mut self,
        matches: impl Fn(WindowExecutionContextIdentity) -> bool,
    ) -> usize {
        let identities = self
            .web_locks
            .clients
            .keys()
            .copied()
            .filter(|identity| matches(*identity))
            .collect::<Vec<_>>();
        let mut actions = Vec::new();
        for identity in &identities {
            let Some((client, _)) = self.web_locks.clients.remove(identity) else {
                continue;
            };
            actions.extend(self.web_lock_registry.release_client(client));
            let requests = &mut self.web_locks.requests;
            requests.retain(|_, state| state.client != client);
            let requests = &self.web_locks.requests;
            self.web_locks
                .tasks
                .retain(|_, (_, _, request)| requests.contains_key(&*request));
        }
        self.post_web_lock_actions(actions);
        identities.len()
    }

    pub(crate) fn release_web_locks_for_child_context(
        &mut self,
        handle: crate::document_runtime::DomHandle,
    ) -> usize {
        self.release_web_lock_clients(|identity| {
            matches!(
                identity.dispatch_scope(),
                OwnerDispatchScope::Child(child_handle) if child_handle == handle
            )
        })
    }

    pub(crate) fn release_web_locks_for_execution_context_owner(
        &mut self,
        owner: WindowExecutionContextOwner,
    ) -> usize {
        self.release_web_lock_clients(|identity| identity.owner() == owner)
    }

    pub(crate) fn release_web_locks_for_context_token(
        &mut self,
        context_token: RuntimeObservableContextToken,
    ) -> usize {
        self.release_web_lock_clients(|identity| identity.realm_token() == context_token)
    }

    pub(crate) fn release_owned_web_locks(&mut self) {
        self.release_web_lock_clients(|_| true);
        self.web_locks = WebLockHostState::default();
    }
}
