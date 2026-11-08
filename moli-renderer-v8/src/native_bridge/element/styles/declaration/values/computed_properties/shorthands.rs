//! Resolved-value serialization for shorthands that Stylo's
//! `computed_or_resolved_shorthand_value` leaves empty (it only serializes
//! four-side and two-property shorthands). Each shorthand is serialized from
//! its resolved longhands; Chromium's `getComputedStyle()` output is the
//! reference for component order and omission rules.

use super::*;

#[derive(Clone, Copy)]
enum ComputedShorthand {
    /// Every longhand in order, joined by `separator`.
    Sequence {
        longhands: &'static [&'static str],
        separator: &'static str,
    },
    /// One value when both longhands match, otherwise both.
    CollapsingPair([&'static str; 2]),
    /// The non-`auto` longhands, or `auto` when every longhand is `auto`.
    OmitAuto([&'static str; 2]),
    /// `width style color`, omitting a `none` style.
    ColumnRule,
    /// Horizontal radii, then ` / ` vertical radii when they differ.
    BorderRadius,
    /// `start / end`, omitting an end that the start implies.
    GridLine {
        start: &'static str,
        end: &'static str,
    },
    /// `row-start / column-start / row-end / column-end`, omitting implied lines.
    GridArea,
    /// Areas interleaved with row tracks, then ` / ` columns.
    GridTemplate,
    /// A border side's `width style color`.
    BorderSide([&'static str; 3]),
    /// Two border-side shorthands that serialize only when they match.
    BorderSidePair([&'static str; 2]),
}

fn computed_shorthand(property: &str) -> Option<ComputedShorthand> {
    use ComputedShorthand::*;
    Some(match property {
        "border-radius" | "-webkit-border-radius" => BorderRadius,
        "gap" | "grid-gap" => CollapsingPair(["row-gap", "column-gap"]),
        "place-content" => CollapsingPair(["align-content", "justify-content"]),
        "place-items" => CollapsingPair(["align-items", "justify-items"]),
        "place-self" => CollapsingPair(["align-self", "justify-self"]),
        "flex" | "-webkit-flex" => Sequence {
            longhands: &["flex-grow", "flex-shrink", "flex-basis"],
            separator: " ",
        },
        "outline" => Sequence {
            longhands: &["outline-color", "outline-style", "outline-width"],
            separator: " ",
        },
        "list-style" => Sequence {
            longhands: &["list-style-position", "list-style-image", "list-style-type"],
            separator: " ",
        },
        "grid" => Sequence {
            longhands: &[
                "grid-template-rows",
                "grid-template-columns",
                "grid-template-areas",
                "grid-auto-flow",
                "grid-auto-rows",
                "grid-auto-columns",
            ],
            separator: " / ",
        },
        "columns" | "-webkit-columns" => OmitAuto(["column-width", "column-count"]),
        "column-rule" | "-webkit-column-rule" => ColumnRule,
        "grid-row" => GridLine {
            start: "grid-row-start",
            end: "grid-row-end",
        },
        "grid-column" => GridLine {
            start: "grid-column-start",
            end: "grid-column-end",
        },
        "grid-area" => GridArea,
        "grid-template" => GridTemplate,
        "border-block-start" => BorderSide([
            "border-block-start-width",
            "border-block-start-style",
            "border-block-start-color",
        ]),
        "border-block-end" => BorderSide([
            "border-block-end-width",
            "border-block-end-style",
            "border-block-end-color",
        ]),
        "border-inline-start" => BorderSide([
            "border-inline-start-width",
            "border-inline-start-style",
            "border-inline-start-color",
        ]),
        "border-inline-end" => BorderSide([
            "border-inline-end-width",
            "border-inline-end-style",
            "border-inline-end-color",
        ]),
        "border-block" => BorderSidePair(["border-block-start", "border-block-end"]),
        "border-inline" => BorderSidePair(["border-inline-start", "border-inline-end"]),
        _ => return None,
    })
}

/// Returns `None` when `property` is not a table shorthand or a longhand has
/// no resolved value, so the caller keeps its existing fallback.
pub(super) fn computed_shorthand_value(
    runtime: &JsContextHost,
    handle: DomHandle,
    property: &str,
    resolution: StyleResolutionContext<'_>,
) -> Option<String> {
    let shorthand = computed_shorthand(property)?;
    let read = |longhand: &str| {
        let value = resolution.computed_property(runtime, handle, longhand);
        (!value.is_empty()).then_some(value)
    };
    Some(match shorthand {
        ComputedShorthand::Sequence {
            longhands,
            separator,
        } => longhands
            .iter()
            .map(|longhand| read(longhand))
            .collect::<Option<Vec<_>>>()?
            .join(separator),
        ComputedShorthand::CollapsingPair([first, second]) => {
            let (first, second) = (read(first)?, read(second)?);
            if first == second {
                first
            } else {
                format!("{first} {second}")
            }
        }
        ComputedShorthand::OmitAuto(longhands) => {
            let values = longhands
                .iter()
                .map(|longhand| read(longhand))
                .collect::<Option<Vec<_>>>()?;
            let values = values
                .into_iter()
                .filter(|value| value != "auto")
                .collect::<Vec<_>>();
            if values.is_empty() {
                "auto".to_owned()
            } else {
                values.join(" ")
            }
        }
        ComputedShorthand::ColumnRule => {
            let width = read("column-rule-width")?;
            let style = read("column-rule-style")?;
            let color = read("column-rule-color")?;
            if style == "none" {
                format!("{width} {color}")
            } else {
                format!("{width} {style} {color}")
            }
        }
        ComputedShorthand::BorderRadius => {
            let mut horizontal = Vec::with_capacity(4);
            let mut vertical = Vec::with_capacity(4);
            for corner in [
                "border-top-left-radius",
                "border-top-right-radius",
                "border-bottom-right-radius",
                "border-bottom-left-radius",
            ] {
                let radii = box_shorthand_value_components(&read(corner)?)?;
                let (x, y) = match radii.as_slice() {
                    [radius] => (radius.clone(), radius.clone()),
                    [x, y] => (x.clone(), y.clone()),
                    _ => return None,
                };
                horizontal.push(x);
                vertical.push(y);
            }
            let horizontal = compress_box_components(&horizontal)?;
            let vertical = compress_box_components(&vertical)?;
            if horizontal == vertical {
                horizontal
            } else {
                format!("{horizontal} / {vertical}")
            }
        }
        ComputedShorthand::GridLine { start, end } => {
            let (start, end) = (read(start)?, read(end)?);
            if grid_line_is_omittable(&start, &end) {
                start
            } else {
                format!("{start} / {end}")
            }
        }
        ComputedShorthand::GridArea => {
            let row_start = read("grid-row-start")?;
            let column_start = read("grid-column-start")?;
            let row_end = read("grid-row-end")?;
            let column_end = read("grid-column-end")?;
            let mut lines = 4;
            if grid_line_is_omittable(&column_start, &column_end) {
                lines = 3;
                if grid_line_is_omittable(&row_start, &row_end) {
                    lines = 2;
                    if grid_line_is_omittable(&row_start, &column_start) {
                        lines = 1;
                    }
                }
            }
            [row_start, column_start, row_end, column_end][..lines].join(" / ")
        }
        ComputedShorthand::GridTemplate => {
            let rows = read("grid-template-rows")?;
            let columns = read("grid-template-columns")?;
            let areas = read("grid-template-areas")?;
            serialize_grid_template(&rows, &columns, &areas)
        }
        ComputedShorthand::BorderSide([width, style, color]) => {
            serialize_border_side_components(&read(width)?, &read(style)?, &read(color)?)?
        }
        ComputedShorthand::BorderSidePair([start, end]) => {
            let start = computed_shorthand_value(runtime, handle, start, resolution)?;
            let end = computed_shorthand_value(runtime, handle, end, resolution)?;
            if start == end { start } else { String::new() }
        }
    })
}

/// A grid line is implied by the line it pairs with: a `<custom-ident>` start
/// implies the same ident, anything else implies `auto` (CSS Grid 2 §8.4).
fn grid_line_is_omittable(start: &str, end: &str) -> bool {
    if grid_line_is_custom_ident(start) {
        end == start
    } else {
        end == "auto"
    }
}

fn grid_line_is_custom_ident(value: &str) -> bool {
    let mut chars = value.chars();
    let starts_like_number = match chars.next() {
        Some(first) if first.is_ascii_digit() || first == '+' => true,
        Some('-') => chars.next().is_some_and(|second| second.is_ascii_digit()),
        Some(_) => false,
        None => return false,
    };
    value != "auto" && !starts_like_number && !value.contains(char::is_whitespace)
}

fn serialize_grid_template(rows: &str, columns: &str, areas: &str) -> String {
    if areas == "none" {
        return if rows == "none" && columns == "none" {
            "none".to_owned()
        } else {
            format!("{rows} / {columns}")
        };
    }
    // `<grid-template-areas>` needs exactly one plain track per area row;
    // anything else (line names, `repeat()`, a count mismatch) has no
    // shorthand serialization here.
    let (Some(area_rows), Some(tracks)) = (
        box_shorthand_value_components(areas),
        box_shorthand_value_components(rows),
    ) else {
        return String::new();
    };
    if area_rows.len() != tracks.len()
        || tracks
            .iter()
            .any(|track| track.starts_with('[') || track.starts_with("repeat("))
    {
        return String::new();
    }
    let mut serialized = area_rows
        .iter()
        .zip(&tracks)
        .map(|(area, track)| format!("{area} {track}"))
        .collect::<Vec<_>>()
        .join(" ");
    if columns != "none" {
        serialized.push_str(" / ");
        serialized.push_str(columns);
    }
    serialized
}
