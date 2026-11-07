use crate::{
    context_bootstrap::{
        CanvasContextKind, attach_canvas_like_context_object,
        build_canvas_rendering_context_2d_object, build_offscreen_canvas_object,
        build_webgl_context_object, build_webgl2_context_object, canvas_like_to_data_url,
        canvas_like_to_image_file,
    },
    util::{
        context_host_ptr_from_global_bridge, get_own_static_property,
        object_own_static_string_property, v8_string, v8str,
    },
    webidl,
};

use super::super::node::{node_relevant_context, node_runtime_and_handle_from_object_or_detached};
use super::{element_attribute, set_reflected_attribute};

const CANVAS_CONTEXT_KIND_SLOT: &str = "__moliCanvasContextKind";
const CANVAS_CONTEXT_2D_SLOT: &str = "__moliCanvasContext2D";
const CANVAS_CONTEXT_WEBGL_SLOT: &str = "__moliCanvasContextWebGL";
const CANVAS_CONTEXT_WEBGL2_SLOT: &str = "__moliCanvasContextWebGL2";

#[derive(webidl::WebIdlArgs)]
#[webidl(prefix = "HTMLCanvasElement.getContext")]
struct HtmlCanvasGetContextArgs {
    #[webidl(required, converter = "enum")]
    kind: CanvasContextKind,
}

#[derive(webidl::WebIdlArgs)]
#[webidl(prefix = "HTMLCanvasElement.toBlob")]
struct HtmlCanvasToBlobArgs {
    #[webidl(required, converter = "callback_function")]
    callback: webidl::WebIdlCallbackFunction,
    // Converted for its side effects; every requested type falls back to the
    // single supported serialization, as HTML permits.
    #[webidl(default = String::new())]
    _type: String,
}

fn canvas_context_slot(kind: CanvasContextKind) -> &'static str {
    match kind {
        CanvasContextKind::TwoD => CANVAS_CONTEXT_2D_SLOT,
        CanvasContextKind::WebGl => CANVAS_CONTEXT_WEBGL_SLOT,
        CanvasContextKind::WebGl2 => CANVAS_CONTEXT_WEBGL2_SLOT,
    }
}

fn build_canvas_context_for_kind<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    kind: CanvasContextKind,
) -> Option<v8::Local<'s, v8::Value>> {
    match kind {
        CanvasContextKind::TwoD => build_canvas_rendering_context_2d_object(scope).map(Into::into),
        CanvasContextKind::WebGl => build_webgl_context_object(scope).map(Into::into),
        CanvasContextKind::WebGl2 => build_webgl2_context_object(scope).map(Into::into),
    }
}

pub(crate) fn html_canvas_width_getter_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    rv.set_uint32(canvas_dimension_value(scope, args.this(), "width", 300));
}

pub(crate) fn html_canvas_width_setter_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'s, v8::Value>,
) {
    let canvas = args.this();
    let _ = set_canvas_dimension_attribute(
        scope,
        canvas,
        "width",
        args.get(0),
        "HTMLCanvasElement",
        "width",
        300,
    );
    rv.set_undefined();
}

pub(crate) fn html_canvas_height_getter_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    rv.set_uint32(canvas_dimension_value(scope, args.this(), "height", 150));
}

pub(crate) fn html_canvas_height_setter_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'s, v8::Value>,
) {
    let canvas = args.this();
    let _ = set_canvas_dimension_attribute(
        scope,
        canvas,
        "height",
        args.get(0),
        "HTMLCanvasElement",
        "height",
        150,
    );
    rv.set_undefined();
}

fn set_canvas_dimension_attribute<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    canvas: v8::Local<'s, v8::Object>,
    name: &str,
    value: v8::Local<'s, v8::Value>,
    owner: &'static str,
    property: &'static str,
    default_value: u32,
) -> bool {
    let converted = match webidl::convert::<webidl::UnsignedLong>(
        scope,
        value,
        webidl::Context::member(owner, property),
    ) {
        Ok(value) if value.0 <= i32::MAX as u32 => value.0,
        Ok(_) => default_value,
        Err(error) => {
            webidl::throw_error(scope, &error);
            return false;
        }
    };
    let Ok((runtime_ptr, handle)) = node_runtime_and_handle_from_object_or_detached(scope, canvas)
    else {
        return false;
    };
    set_reflected_attribute(scope, runtime_ptr, handle, name, &converted.to_string());
    true
}

fn canvas_dimension_value<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
    name: &str,
    default: u32,
) -> u32 {
    let Ok((runtime_ptr, handle)) = node_runtime_and_handle_from_object_or_detached(scope, object)
    else {
        return default;
    };
    element_attribute(unsafe { &*runtime_ptr }, handle, name)
        .and_then(|value| parse_unsigned_long_prefix(&value))
        .filter(|value| *value <= i32::MAX as u32)
        .unwrap_or(default)
}

fn parse_unsigned_long_prefix(value: &str) -> Option<u32> {
    let value = value.trim_start_matches(|ch: char| ch.is_ascii_whitespace());
    let (value, negative) = if let Some(value) = value.strip_prefix('+') {
        (value, false)
    } else if let Some(value) = value.strip_prefix('-') {
        (value, true)
    } else {
        (value, false)
    };
    let digits = value
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    if digits.is_empty() {
        return None;
    }
    let value = digits.parse::<u32>().ok()?;
    if negative && value != 0 {
        return None;
    }
    Some(value)
}

pub(crate) fn canvas_get_context_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'s, v8::Value>,
) {
    let Some(kind) = webidl::try_parse_args::<HtmlCanvasGetContextArgs>(scope, &args)
        .ok()
        .map(|parsed| parsed.kind)
    else {
        rv.set_null();
        return;
    };
    let canvas = args.this();
    if let Some(existing_kind) =
        object_own_static_string_property(scope, canvas, CANVAS_CONTEXT_KIND_SLOT)
            .and_then(|value| CanvasContextKind::parse(&value))
    {
        if existing_kind != kind {
            rv.set_null();
            return;
        }
        let slot = canvas_context_slot(kind);
        if let Some(existing) = get_own_static_property(scope, canvas, slot) {
            rv.set(existing);
            return;
        }
    }

    let relevant_context =
        node_relevant_context(scope, canvas).unwrap_or_else(|| scope.get_current_context());
    let target_scope = &mut v8::ContextScope::new(scope, relevant_context);
    let Some(value) = build_canvas_context_for_kind(target_scope, kind) else {
        rv.set_null();
        return;
    };
    let Some(kind_value) = v8_string(target_scope, kind.label()) else {
        rv.set_null();
        return;
    };
    let _ = canvas.define_own_property(
        target_scope,
        v8str(target_scope, CANVAS_CONTEXT_KIND_SLOT).into(),
        kind_value.into(),
        v8::PropertyAttribute::DONT_ENUM,
    );
    let slot = canvas_context_slot(kind);
    let _ = canvas.define_own_property(
        target_scope,
        v8str(target_scope, slot).into(),
        value,
        v8::PropertyAttribute::DONT_ENUM,
    );
    if matches!(
        kind,
        CanvasContextKind::TwoD | CanvasContextKind::WebGl | CanvasContextKind::WebGl2
    ) && let Ok(context) = v8::Local::<v8::Object>::try_from(value)
    {
        attach_canvas_like_context_object(target_scope, canvas, context);
    }
    rv.set(value);
}

pub(crate) fn canvas_transfer_control_to_offscreen_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Ok((runtime_ptr, handle)) =
        node_runtime_and_handle_from_object_or_detached(scope, args.this())
    else {
        rv.set_null();
        return;
    };
    let width = element_attribute(unsafe { &*runtime_ptr }, handle, "width")
        .and_then(|value| parse_unsigned_long_prefix(&value))
        .filter(|value| *value <= i32::MAX as u32)
        .unwrap_or(300);
    let height = element_attribute(unsafe { &*runtime_ptr }, handle, "height")
        .and_then(|value| parse_unsigned_long_prefix(&value))
        .filter(|value| *value <= i32::MAX as u32)
        .unwrap_or(150);
    let value = build_offscreen_canvas_object(scope, width, height)
        .map(Into::into)
        .unwrap_or_else(|| v8::null(scope).into());
    rv.set(value);
}

pub(crate) fn canvas_to_data_url_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'s, v8::Value>,
) {
    let Ok((_runtime_ptr, _handle)) =
        node_runtime_and_handle_from_object_or_detached(scope, args.this())
    else {
        rv.set(v8::undefined(scope).into());
        return;
    };
    let Some(data_url) = canvas_like_to_data_url(scope, args.this()) else {
        rv.set(v8::undefined(scope).into());
        return;
    };
    if let Some(value) = v8_string(scope, &data_url) {
        rv.set(value.into());
    } else {
        rv.set(v8::undefined(scope).into());
    }
}

/// HTML `HTMLCanvasElement.toBlob(callback, type, quality)`.
///
/// The bitmap is copied and serialized with the `toDataURL` encoder before
/// returning; the callback then runs from a queued task in the canvas's
/// relevant realm with that Blob, or with null when the bitmap has no pixels
/// or serialization fails. Moli does not model the bitmap origin-clean flag
/// (no canvas operation taints it), so the SecurityError step never applies.
pub(crate) fn canvas_to_blob_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    _rv: v8::ReturnValue<'s, v8::Value>,
) {
    if node_runtime_and_handle_from_object_or_detached(scope, args.this()).is_err() {
        return;
    }
    let Some(parsed) = webidl::parse_args::<HtmlCanvasToBlobArgs>(scope, &args) else {
        return;
    };
    let canvas = args.this();
    let image_file = canvas_like_to_image_file(scope, canvas);
    let Some(context) = canvas.get_creation_context(scope) else {
        return;
    };
    let scope = &mut v8::ContextScope::new(scope, context);
    let Some(host_ptr) = context_host_ptr_from_global_bridge(scope) else {
        return;
    };
    let result: v8::Local<v8::Value> = image_file
        .and_then(|bytes| {
            crate::blob::build_blob_object(
                scope,
                bytes,
                moli_canvas::CANVAS_SERIALIZATION_MIME_TYPE.to_owned(),
            )
        })
        .map(Into::into)
        .unwrap_or_else(|| v8::null(scope).into());
    let callback = parsed.callback.value(scope);
    let data = v8::Array::new_with_elements(scope, &[callback, result]);
    let Some(task) = v8::Function::builder(run_canvas_to_blob_task)
        .data(data.into())
        .build(scope)
    else {
        return;
    };
    // The internal task scheduler binds the task to this realm's exact
    // Window/Document and discards it when that owner retires.
    unsafe { &mut *host_ptr }.queue_internal_task(scope, task);
}

fn run_canvas_to_blob_task<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    _rv: v8::ReturnValue<'_, v8::Value>,
) {
    let Ok(data) = v8::Local::<v8::Array>::try_from(args.data()) else {
        return;
    };
    let Some(callback) = data
        .get_index(scope, 0)
        .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok())
    else {
        return;
    };
    let Some(result) = data.get_index(scope, 1) else {
        return;
    };
    // An exception propagates to the task invocation, which reports it.
    let receiver = v8::undefined(scope);
    let _ = callback.call(scope, receiver.into(), &[result]);
}
