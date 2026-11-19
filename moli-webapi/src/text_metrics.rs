use crate::interfaces as web_api_interfaces;
use moli_v8_util::{get_private_value, throw_type_error};
use moli_webapi_declare::{WebApiFunctionTemplate, WebApiObject};
const TEXT_METRICS_WIDTH_SLOT: &str = "__moliTextMetricsWidth";
const TEXT_METRICS_ACTUAL_BOUNDING_BOX_LEFT_SLOT: &str = "__moliTextMetricsActualBoundingBoxLeft";
const TEXT_METRICS_ACTUAL_BOUNDING_BOX_RIGHT_SLOT: &str = "__moliTextMetricsActualBoundingBoxRight";
const TEXT_METRICS_FONT_BOUNDING_BOX_ASCENT_SLOT: &str = "__moliTextMetricsFontBoundingBoxAscent";
const TEXT_METRICS_FONT_BOUNDING_BOX_DESCENT_SLOT: &str = "__moliTextMetricsFontBoundingBoxDescent";
const TEXT_METRICS_ACTUAL_BOUNDING_BOX_ASCENT_SLOT: &str =
    "__moliTextMetricsActualBoundingBoxAscent";
const TEXT_METRICS_ACTUAL_BOUNDING_BOX_DESCENT_SLOT: &str =
    "__moliTextMetricsActualBoundingBoxDescent";
const TEXT_METRICS_HANGING_BASELINE_SLOT: &str = "__moliTextMetricsHangingBaseline";
const TEXT_METRICS_ALPHABETIC_BASELINE_SLOT: &str = "__moliTextMetricsAlphabeticBaseline";
const TEXT_METRICS_IDEOGRAPHIC_BASELINE_SLOT: &str = "__moliTextMetricsIdeographicBaseline";

#[derive(Default, WebApiObject)]
#[webapi(interface = web_api_interfaces::TextMetrics)]
pub struct CanvasTextMetricsDeclaration {
    #[webapi(slot = TEXT_METRICS_WIDTH_SLOT)]
    pub width: f64,
    #[webapi(slot = TEXT_METRICS_ACTUAL_BOUNDING_BOX_LEFT_SLOT)]
    pub actual_bounding_box_left: f64,
    #[webapi(slot = TEXT_METRICS_ACTUAL_BOUNDING_BOX_RIGHT_SLOT)]
    pub actual_bounding_box_right: f64,
    #[webapi(slot = TEXT_METRICS_FONT_BOUNDING_BOX_ASCENT_SLOT)]
    pub font_bounding_box_ascent: f64,
    #[webapi(slot = TEXT_METRICS_FONT_BOUNDING_BOX_DESCENT_SLOT)]
    pub font_bounding_box_descent: f64,
    #[webapi(slot = TEXT_METRICS_ACTUAL_BOUNDING_BOX_ASCENT_SLOT)]
    pub actual_bounding_box_ascent: f64,
    #[webapi(slot = TEXT_METRICS_ACTUAL_BOUNDING_BOX_DESCENT_SLOT)]
    pub actual_bounding_box_descent: f64,
    #[webapi(slot = TEXT_METRICS_HANGING_BASELINE_SLOT)]
    pub hanging_baseline: f64,
    #[webapi(slot = TEXT_METRICS_ALPHABETIC_BASELINE_SLOT)]
    pub alphabetic_baseline: f64,
    #[webapi(slot = TEXT_METRICS_IDEOGRAPHIC_BASELINE_SLOT)]
    pub ideographic_baseline: f64,
}

#[derive(WebApiFunctionTemplate)]
#[webapi(interface = web_api_interfaces::TextMetrics, enumerable)]
struct TextMetricsPrototypeDeclaration {
    #[webapi(accessor_property, getter = text_metrics_width_getter)]
    width: (),
    #[webapi(accessor_property, getter = text_metrics_actual_bounding_box_left_getter)]
    actual_bounding_box_left: (),
    #[webapi(accessor_property, getter = text_metrics_actual_bounding_box_right_getter)]
    actual_bounding_box_right: (),
    #[webapi(accessor_property, getter = text_metrics_font_bounding_box_ascent_getter)]
    font_bounding_box_ascent: (),
    #[webapi(accessor_property, getter = text_metrics_font_bounding_box_descent_getter)]
    font_bounding_box_descent: (),
    #[webapi(accessor_property, getter = text_metrics_actual_bounding_box_ascent_getter)]
    actual_bounding_box_ascent: (),
    #[webapi(accessor_property, getter = text_metrics_actual_bounding_box_descent_getter)]
    actual_bounding_box_descent: (),
    #[webapi(accessor_property, getter = text_metrics_hanging_baseline_getter)]
    hanging_baseline: (),
    #[webapi(accessor_property, getter = text_metrics_alphabetic_baseline_getter)]
    alphabetic_baseline: (),
    #[webapi(accessor_property, getter = text_metrics_ideographic_baseline_getter)]
    ideographic_baseline: (),
}

pub fn install_text_metrics_template_bindings<'s>(
    scope: &mut v8::PinScope<'s, '_, ()>,
    template: v8::Local<'s, v8::FunctionTemplate>,
) {
    TextMetricsPrototypeDeclaration::initialize_prototype_template(
        scope,
        template.prototype_template(scope),
    );
}

macro_rules! text_metrics_getter {
    ($getter:ident, $slot:ident, $name:literal) => {
        fn $getter<'s>(
            scope: &mut v8::PinScope<'s, '_>,
            args: v8::FunctionCallbackArguments<'s>,
            mut rv: v8::ReturnValue<'_, v8::Value>,
        ) {
            let Some(value) = get_private_value(scope, args.this(), $slot) else {
                throw_type_error(
                    scope,
                    concat!("TextMetrics.", $name, " called on incompatible receiver."),
                );
                return;
            };
            rv.set(value);
        }
    };
}

text_metrics_getter!(text_metrics_width_getter, TEXT_METRICS_WIDTH_SLOT, "width");
text_metrics_getter!(
    text_metrics_actual_bounding_box_left_getter,
    TEXT_METRICS_ACTUAL_BOUNDING_BOX_LEFT_SLOT,
    "actualBoundingBoxLeft"
);
text_metrics_getter!(
    text_metrics_actual_bounding_box_right_getter,
    TEXT_METRICS_ACTUAL_BOUNDING_BOX_RIGHT_SLOT,
    "actualBoundingBoxRight"
);
text_metrics_getter!(
    text_metrics_font_bounding_box_ascent_getter,
    TEXT_METRICS_FONT_BOUNDING_BOX_ASCENT_SLOT,
    "fontBoundingBoxAscent"
);
text_metrics_getter!(
    text_metrics_font_bounding_box_descent_getter,
    TEXT_METRICS_FONT_BOUNDING_BOX_DESCENT_SLOT,
    "fontBoundingBoxDescent"
);
text_metrics_getter!(
    text_metrics_actual_bounding_box_ascent_getter,
    TEXT_METRICS_ACTUAL_BOUNDING_BOX_ASCENT_SLOT,
    "actualBoundingBoxAscent"
);
text_metrics_getter!(
    text_metrics_actual_bounding_box_descent_getter,
    TEXT_METRICS_ACTUAL_BOUNDING_BOX_DESCENT_SLOT,
    "actualBoundingBoxDescent"
);
text_metrics_getter!(
    text_metrics_hanging_baseline_getter,
    TEXT_METRICS_HANGING_BASELINE_SLOT,
    "hangingBaseline"
);
text_metrics_getter!(
    text_metrics_alphabetic_baseline_getter,
    TEXT_METRICS_ALPHABETIC_BASELINE_SLOT,
    "alphabeticBaseline"
);
text_metrics_getter!(
    text_metrics_ideographic_baseline_getter,
    TEXT_METRICS_IDEOGRAPHIC_BASELINE_SLOT,
    "ideographicBaseline"
);
