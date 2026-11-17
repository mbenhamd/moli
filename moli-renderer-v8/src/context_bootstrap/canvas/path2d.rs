//! `Path2D` (HTML "Path2D objects"): a reusable path in its own user space.
//!
//! The `CanvasPath` members are the 2D context's own callbacks, which accept
//! a `Path2D` receiver and record into its native path state. A `Path2D`
//! never has a transform, so its geometry stays in the user space it was
//! built in until a context draws it under that context's current transform.

use super::path::Canvas2dPathState;
use super::state::canvas_path_state;
use crate::util::throw_type_error;
use crate::web_api_interfaces;

/// The path state of a `Path2D` argument, or `None` for any other value.
pub(super) fn path2d_argument<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
) -> Option<Canvas2dPathState> {
    let object = v8::Local::<v8::Object>::try_from(value).ok()?;
    if !web_api_interfaces::Path2D::is_instance(scope, object) {
        return None;
    }
    Some(canvas_path_state(scope, object).borrow().clone())
}

/// `new Path2D()`, `new Path2D(path)` and `new Path2D(svgPathData)`.
pub(crate) fn path2d_constructor_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    if !args.is_construct_call() {
        throw_type_error(
            scope,
            "Failed to construct 'Path2D': Please use the 'new' operator.",
        );
        return;
    }
    let source = args.get(0);
    let initial = if source.is_undefined() {
        Canvas2dPathState::default()
    } else if let Some(path) = path2d_argument(scope, source) {
        path
    } else {
        let Some(data) = source.to_string(scope) else {
            return;
        };
        Canvas2dPathState::from_svg_path_data(&data.to_rust_string_lossy(scope))
    };
    *canvas_path_state(scope, args.this()).borrow_mut() = initial;
    rv.set(args.this().into());
}

/// `Path2D.addPath(path, transform)`.
pub(crate) fn path2d_add_path_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'_, v8::Value>,
) {
    const PREFIX: &str = "Path2D.addPath";
    let Some(path) = path2d_argument(scope, args.get(0)) else {
        throw_type_error(
            scope,
            "Failed to execute 'addPath' on 'Path2D': parameter 1 is not of type 'Path2D'.",
        );
        return;
    };
    let Some([a, b, c, d, e, f]) =
        super::transform::matrix_2d_init_argument(scope, args.get(1), PREFIX, 2)
    else {
        return;
    };
    // A non-finite matrix adds nothing (HTML: "If any of a..f are infinite or NaN, return").
    if [a, b, c, d, e, f].into_iter().all(f64::is_finite) {
        canvas_path_state(scope, args.this())
            .borrow_mut()
            .add_path(&path, kurbo::Affine::new([a, b, c, d, e, f]));
    }
    rv.set_undefined();
}
