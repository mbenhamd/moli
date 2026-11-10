//! `navigator.locks`: the Window `LockManager` and `Lock` bindings.
//!
//! Queue state lives in the browser-context Web Locks registry and the V8 half
//! of each request lives in the requesting Window's Host. These bindings
//! validate calls, run granted callbacks from the Window's miscellaneous
//! platform API task source, and settle the request promises.

use anyhow::{Result, anyhow};
use moli_webapi_declare::{WebApiFunctionTemplate, WebApiObject};

use crate::{
    native_bridge::{JsContextHost, NewWebLockRequest, WebLockClient, WebLockClientError},
    page_task_queue::RendererPageMiscPlatformApiTaskKind,
    util::{
        context_host_ptr_from_context_slot, context_host_ptr_from_global_bridge, get_private_value,
        throw_type_error, v8_string, v8str,
    },
    web_api_interfaces,
    web_locks_runtime::{WebLockInfo, WebLockMode, WebLockRequestId},
    webidl,
};

const LOCK_NAME_SLOT: &str = "__moliLockName";
const LOCK_MODE_SLOT: &str = "__moliLockMode";

#[derive(Default, WebApiObject)]
#[webapi(interface = web_api_interfaces::LockManager)]
struct LockManagerObjectDeclaration {}

#[derive(Default, WebApiFunctionTemplate)]
#[webapi(interface = web_api_interfaces::LockManager, enumerable, receiver)]
struct LockManagerPrototypeDeclaration {
    #[webapi(method, length = 2, callback = lock_manager_request, returns_promise)]
    request: (),
    #[webapi(method, length = 0, callback = lock_manager_query, returns_promise)]
    query: (),
}

#[derive(WebApiObject)]
#[webapi(interface = web_api_interfaces::Lock)]
struct LockObjectDeclaration {
    #[webapi(slot = LOCK_NAME_SLOT)]
    name: String,
    #[webapi(slot = LOCK_MODE_SLOT)]
    mode: String,
}

#[derive(Default, WebApiFunctionTemplate)]
#[webapi(interface = web_api_interfaces::Lock, enumerable, receiver)]
struct LockPrototypeDeclaration {
    #[webapi(accessor_property, getter = lock_name_getter)]
    name: (),
    #[webapi(accessor_property, getter = lock_mode_getter)]
    mode: (),
}

#[derive(Default, webidl::WebIdlDictionary)]
#[webidl(prefix = "LockOptions")]
struct LockOptions<'s> {
    #[webidl(converter = "boolean", default = false)]
    if_available: bool,
    #[webidl(converter = "enum", default = WebLockMode::Exclusive)]
    mode: WebLockMode,
    #[webidl(converter = "raw")]
    signal: Option<crate::abort_signal_route::ResolvedAbortSignal<'s>>,
    #[webidl(converter = "boolean", default = false)]
    steal: bool,
}

pub(super) fn install<'s>(
    scope: &mut v8::PinScope<'s, '_, ()>,
    template: v8::Local<'s, v8::FunctionTemplate>,
    interface_name: &str,
) {
    match interface_name {
        "LockManager" => {
            LockManagerPrototypeDeclaration::initialize_prototype_template(
                scope,
                template.prototype_template(scope),
            );
        }
        "Lock" => {
            LockPrototypeDeclaration::initialize_prototype_template(
                scope,
                template.prototype_template(scope),
            );
        }
        _ => {}
    }
}

pub(super) fn build<'s>(scope: &mut v8::PinScope<'s, '_>) -> Result<v8::Local<'s, v8::Object>> {
    LockManagerObjectDeclaration::default()
        .bind(scope)
        .map_err(|error| anyhow!("failed to bind LockManager object: {error}"))
}

fn lock_name_getter<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    rv: v8::ReturnValue<'s, v8::Value>,
) {
    lock_slot_getter(scope, args, rv, LOCK_NAME_SLOT);
}

fn lock_mode_getter<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    rv: v8::ReturnValue<'s, v8::Value>,
) {
    lock_slot_getter(scope, args, rv, LOCK_MODE_SLOT);
}

fn lock_slot_getter<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'s, v8::Value>,
    slot: &'static str,
) {
    let Some(value) = get_private_value(scope, args.this(), slot) else {
        throw_type_error(scope, "Illegal invocation");
        return;
    };
    rv.set(value);
}

fn reject_dom_exception<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    resolver: v8::Local<'s, v8::PromiseResolver>,
    name: &str,
    message: &str,
) {
    let error = crate::context_bootstrap::new_dom_exception_value(scope, message, name);
    let _ = resolver.reject(scope, error);
}

fn reject_client_error<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    resolver: v8::Local<'s, v8::PromiseResolver>,
    error: WebLockClientError,
) {
    match error {
        WebLockClientError::InvalidState => reject_dom_exception(
            scope,
            resolver,
            "InvalidStateError",
            "The document is not fully active.",
        ),
        WebLockClientError::Security => reject_dom_exception(
            scope,
            resolver,
            "SecurityError",
            "Access to the Locks API is denied in this context.",
        ),
    }
}

/// Resolve the lock-manager client of the LockManager's relevant realm.
fn current_web_lock_client(
    scope: &mut v8::PinScope<'_, '_>,
    manager: v8::Local<'_, v8::Object>,
) -> Result<(*mut JsContextHost, WebLockClient), WebLockClientError> {
    let context = manager
        .get_creation_context(scope)
        .ok_or(WebLockClientError::InvalidState)?;
    let host_ptr =
        context_host_ptr_from_context_slot(context).ok_or(WebLockClientError::InvalidState)?;
    let scope = &mut v8::ContextScope::new(scope, context);
    let client = unsafe { &mut *host_ptr }.current_web_lock_client(scope)?;
    Ok((host_ptr, client))
}

fn request_id_value<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    request: WebLockRequestId,
) -> v8::Local<'s, v8::Value> {
    v8::BigInt::new_from_u64(scope, request).into()
}

fn request_id_from_data(data: v8::Local<'_, v8::Value>) -> Option<WebLockRequestId> {
    let big = v8::Local::<v8::BigInt>::try_from(data).ok()?;
    let (request, lossless) = big.u64_value();
    lossless.then_some(request)
}

/// The NotSupportedError message for an invalid option combination, if any.
fn unsupported_request_message(name: &str, options: &LockOptions<'_>) -> Option<&'static str> {
    if name.starts_with('-') {
        return Some("Names cannot start with '-'.");
    }
    if options.steal && options.if_available {
        return Some("The 'steal' and 'ifAvailable' options cannot be used together.");
    }
    if options.steal && options.mode != WebLockMode::Exclusive {
        return Some("The 'steal' option may only be used with 'exclusive' locks.");
    }
    if options.signal.is_some() && options.steal {
        return Some("The 'signal' and 'steal' options cannot be used together.");
    }
    if options.signal.is_some() && options.if_available {
        return Some("The 'signal' and 'ifAvailable' options cannot be used together.");
    }
    None
}

fn lock_manager_request<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'s, v8::Value>,
) {
    // request(name, callback) or request(name, options, callback).
    if args.length() < 2 {
        throw_type_error(
            scope,
            &format!(
                "Failed to execute 'request' on 'LockManager': 2 arguments required, but only {} present.",
                args.length()
            ),
        );
        return;
    }
    let name = match webidl::convert::<webidl::DomString>(
        scope,
        args.get(0),
        webidl::Context::argument("LockManager.request", 1),
    ) {
        Ok(name) => name.0,
        Err(error) => {
            webidl::throw_error(scope, &error);
            return;
        }
    };
    let (options, callback) = if args.length() == 2 {
        (LockOptions::default(), args.get(1))
    } else {
        match webidl::parse_dictionary::<LockOptions>(
            scope,
            args.get(1),
            webidl::Context::argument("LockManager.request", 2),
        ) {
            Ok(options) => (options.unwrap_or_default(), args.get(2)),
            Err(error) => {
                webidl::throw_error(scope, &error);
                return;
            }
        }
    };
    let Ok(callback) = v8::Local::<v8::Function>::try_from(callback) else {
        throw_type_error(
            scope,
            "Failed to execute 'request' on 'LockManager': The callback provided as parameter is not a function.",
        );
        return;
    };

    let Some(resolver) = v8::PromiseResolver::new(scope) else {
        return;
    };
    rv.set(resolver.get_promise(scope).into());
    let (host_ptr, client) = match current_web_lock_client(scope, args.this()) {
        Ok(client) => client,
        Err(error) => {
            reject_client_error(scope, resolver, error);
            return;
        }
    };
    if let Some(message) = unsupported_request_message(&name, &options) {
        reject_dom_exception(scope, resolver, "NotSupportedError", message);
        return;
    }
    if let Some(signal) = options.signal
        && signal.is_aborted(scope)
    {
        let reason = signal
            .reason(scope)
            .unwrap_or_else(|| crate::native_bridge::abort::abort_error_value(scope));
        let _ = resolver.reject(scope, reason);
        return;
    }

    let id = unsafe { &*host_ptr }.next_web_lock_request_id();
    let abort = match options.signal {
        Some(signal) => {
            let data = request_id_value(scope, id);
            let Some(algorithm) = v8::Function::builder(web_lock_abort_algorithm)
                .data(data)
                .build(scope)
            else {
                return;
            };
            // Registration borrows the signal's Host; no Host borrow is held.
            if !signal.register_algorithm(scope, algorithm) {
                reject_dom_exception(
                    scope,
                    resolver,
                    "InvalidStateError",
                    "AbortSignal execution context is detached.",
                );
                return;
            }
            Some((signal.value(), algorithm))
        }
        None => None,
    };
    let request = NewWebLockRequest {
        id,
        name,
        mode: options.mode,
        if_available: options.if_available,
        steal: options.steal,
        callback,
        resolver,
        abort,
    };
    if !unsafe { &mut *host_ptr }.submit_web_lock_request(scope, client, request) {
        reject_client_error(scope, resolver, WebLockClientError::InvalidState);
    }
}

fn lock_info_array<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    infos: &[WebLockInfo],
) -> v8::Local<'s, v8::Array> {
    let values = infos
        .iter()
        .map(|info| {
            let object = v8::Object::new(scope);
            for (key, value) in [
                ("name", info.name.as_str()),
                ("mode", info.mode.as_str()),
                ("clientId", info.client_id.as_str()),
            ] {
                let key = v8str(scope, key);
                let Some(value) = v8_string(scope, value) else {
                    continue;
                };
                let _ = object.create_data_property(scope, key.into(), value.into());
            }
            object.into()
        })
        .collect::<Vec<v8::Local<'s, v8::Value>>>();
    v8::Array::new_with_elements(scope, &values)
}

fn lock_manager_query<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'s, v8::Value>,
) {
    let Some(resolver) = v8::PromiseResolver::new(scope) else {
        return;
    };
    rv.set(resolver.get_promise(scope).into());
    let (held, pending) = match current_web_lock_client(scope, args.this()) {
        Ok((host_ptr, client)) => unsafe { &*host_ptr }.web_lock_snapshot(&client),
        Err(error) => {
            reject_client_error(scope, resolver, error);
            return;
        }
    };
    let snapshot = v8::Object::new(scope);
    for (key, infos) in [("held", held), ("pending", pending)] {
        let key = v8str(scope, key);
        let array = lock_info_array(scope, &infos);
        let _ = snapshot.create_data_property(scope, key.into(), array.into());
    }
    let _ = resolver.resolve(scope, snapshot.into());
}

fn web_lock_abort_algorithm<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    _rv: v8::ReturnValue<'s, v8::Value>,
) {
    let Some(request) = request_id_from_data(args.data()) else {
        return;
    };
    let Some(host_ptr) = context_host_ptr_from_global_bridge(scope) else {
        return;
    };
    let Some((resolver, signal)) =
        unsafe { &mut *host_ptr }.abort_pending_web_lock_request(request)
    else {
        return;
    };
    let resolver = v8::Local::new(scope, &resolver);
    let signal = v8::Local::new(scope, &signal);
    let reason = crate::abort_signal_route::ResolvedAbortSignal::resolve(scope, signal)
        .and_then(|signal| signal.reason(scope))
        .unwrap_or_else(|| crate::native_bridge::abort::abort_error_value(scope));
    let _ = resolver.reject(scope, reason);
}

/// Run one authorized Web Locks task in the requesting Window's realm.
/// Returns whether script ran or a promise settled.
pub(crate) fn run_web_lock_task(
    scope: &mut v8::PinScope<'_, '_>,
    host_ptr: *mut JsContextHost,
    kind: RendererPageMiscPlatformApiTaskKind,
    request: WebLockRequestId,
) -> bool {
    match kind {
        RendererPageMiscPlatformApiTaskKind::WebLockGranted => {
            run_web_lock_grant(scope, host_ptr, request)
        }
        RendererPageMiscPlatformApiTaskKind::WebLockStolen => {
            let Some(resolver) = unsafe { &mut *host_ptr }.steal_web_lock_request(request) else {
                return false;
            };
            let resolver = v8::Local::new(scope, &resolver);
            reject_dom_exception(
                scope,
                resolver,
                "AbortError",
                "Lock broken by another request with the 'steal' option.",
            );
            true
        }
        _ => false,
    }
}

fn run_web_lock_grant(
    scope: &mut v8::PinScope<'_, '_>,
    host_ptr: *mut JsContextHost,
    request: WebLockRequestId,
) -> bool {
    let Some(grant) = unsafe { &mut *host_ptr }.begin_web_lock_grant(request) else {
        return false;
    };
    if let Some((signal, algorithm)) = grant.abort {
        let signal = v8::Local::new(scope, &signal);
        let algorithm = v8::Local::new(scope, &algorithm);
        if let Some(signal) = crate::abort_signal_route::ResolvedAbortSignal::resolve(scope, signal)
        {
            signal.unregister_algorithm(scope, algorithm);
        }
    }
    let lock: v8::Local<'_, v8::Value> = if grant.not_available {
        v8::null(scope).into()
    } else {
        match LockObjectDeclaration::new(grant.name, grant.mode.as_str().to_owned()).bind(scope) {
            Ok(lock) => lock.into(),
            Err(_) => return false,
        }
    };
    let callback = v8::Local::new(scope, &grant.callback);
    let Some(waiting) = v8::PromiseResolver::new(scope) else {
        return false;
    };
    let outcome = {
        let try_catch = std::pin::pin!(v8::TryCatch::new(scope));
        let try_catch = try_catch.init();
        let receiver = v8::undefined(&try_catch).into();
        match callback.call(&try_catch, receiver, &[lock]) {
            Some(result) => Ok(v8::Global::new(&try_catch, result)),
            None => Err(try_catch
                .exception()
                .map(|exception| v8::Global::new(&try_catch, exception))),
        }
    };
    // The lock is released when the callback's result settles; the request
    // promise then adopts that result.
    match outcome {
        Ok(result) => {
            let result = v8::Local::new(scope, &result);
            let _ = waiting.resolve(scope, result);
        }
        Err(exception) => {
            let exception = match exception {
                Some(exception) => v8::Local::new(scope, &exception),
                None => v8::undefined(scope).into(),
            };
            let _ = waiting.reject(scope, exception);
        }
    }
    let data = request_id_value(scope, request);
    let fulfilled = v8::Function::builder(web_lock_callback_fulfilled)
        .data(data)
        .build(scope);
    let rejected = v8::Function::builder(web_lock_callback_rejected)
        .data(data)
        .build(scope);
    if let (Some(fulfilled), Some(rejected)) = (fulfilled, rejected) {
        let _ = waiting.get_promise(scope).then2(scope, fulfilled, rejected);
    }
    true
}

fn web_lock_callback_fulfilled<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    _rv: v8::ReturnValue<'s, v8::Value>,
) {
    settle_web_lock_request(scope, &args, true);
}

fn web_lock_callback_rejected<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    _rv: v8::ReturnValue<'s, v8::Value>,
) {
    settle_web_lock_request(scope, &args, false);
}

fn settle_web_lock_request<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: &v8::FunctionCallbackArguments<'s>,
    fulfilled: bool,
) {
    let Some(request) = request_id_from_data(args.data()) else {
        return;
    };
    let Some(host_ptr) = context_host_ptr_from_global_bridge(scope) else {
        return;
    };
    let Some(resolver) = unsafe { &mut *host_ptr }.finish_web_lock_request(request) else {
        return;
    };
    let resolver = v8::Local::new(scope, &resolver);
    let value = args.get(0);
    let _ = if fulfilled {
        resolver.resolve(scope, value)
    } else {
        resolver.reject(scope, value)
    };
}
