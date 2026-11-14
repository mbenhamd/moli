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
    runtime: &PageScriptHost,
    handle: DomHandle,
    property: &str,
    resolution: StyleResolutionContext<'_>,
) -> Option<String> {
    computed_shorthand(property)?;
    let read = |longhand: &str| {
        let value = resolution.computed_property(runtime, handle, longhand);
        (!value.is_empty()).then_some(value)
    };
    // `grid-template` serializes the computed track lists: the resolved
    // longhands of a grid container are used pixel sizes, which would lose
    // `repeat()`, `fr`, percentages and line names. Only an empty computed
    // list resolves to the container's used (implicit) tracks, as in Chromium.
    let read_tracks = |longhand: &str| {
        let value = resolution.raw_property(runtime, handle, longhand);
        if value == "none" {
            return read(longhand);
        }
        (!value.is_empty()).then_some(value)
    };
    serialize_computed_shorthand(property, &read, &read_tracks)
}

/// Serializes a table shorthand of a pseudo-element from its computed style
/// snapshot's longhands.
pub(in super::super) fn pseudo_computed_shorthand_value(
    property: &str,
    read: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    serialize_computed_shorthand(property, read, read)
}

/// `read` returns a longhand's resolved value and `read_tracks` a
/// `grid-template-{rows,columns}` value to serialize into `grid-template`.
fn serialize_computed_shorthand(
    property: &str,
    read: &dyn Fn(&str) -> Option<String>,
    read_tracks: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let shorthand = computed_shorthand(property)?;
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
            let rows = read_tracks("grid-template-rows")?;
            let columns = read_tracks("grid-template-columns")?;
            let areas = read("grid-template-areas")?;
            serialize_grid_template(&rows, &columns, &areas)
        }
        ComputedShorthand::BorderSide([width, style, color]) => {
            serialize_border_side_components(&read(width)?, &read(style)?, &read(color)?)?
        }
        ComputedShorthand::BorderSidePair([start, end]) => {
            let start = serialize_computed_shorthand(start, read, read_tracks)?;
            let end = serialize_computed_shorthand(end, read, read_tracks)?;
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
    // The areas form is `[<line-names>? <string> <track-size>? <line-names>?]+
    // [/ <explicit-track-list>]?`: one row track per area string, and no
    // `repeat()` on either axis. Anything else has no shorthand serialization.
    if rows == "none" {
        return String::new();
    }
    let (Some(area_rows), Some(row_tokens), Some(column_tokens)) = (
        track_list_tokens(areas),
        track_list_tokens(rows),
        track_list_tokens(columns),
    ) else {
        return String::new();
    };
    if area_rows.iter().any(|area| !area.starts_with('"'))
        || row_tokens
            .iter()
            .chain(&column_tokens)
            .any(|token| token.starts_with("repeat("))
    {
        return String::new();
    }
    let mut tracks = Vec::with_capacity(area_rows.len());
    let mut pending_names = None;
    for token in &row_tokens {
        if token.starts_with('[') {
            if pending_names.replace(token.as_str()).is_some() {
                return String::new();
            }
        } else {
            tracks.push((pending_names.take(), token.as_str()));
        }
    }
    if tracks.len() != area_rows.len() {
        return String::new();
    }
    let mut parts = Vec::with_capacity(row_tokens.len() + area_rows.len() + 2);
    for ((names, size), area) in tracks.into_iter().zip(&area_rows) {
        // Names between two tracks serialize after the earlier track's size,
        // which is the same position as before the later area string.
        parts.extend(names);
        parts.push(area.as_str());
        // `auto` is the omitted `<track-size>` default.
        if size != "auto" {
            parts.push(size);
        }
    }
    parts.extend(pending_names);
    if columns != "none" {
        parts.push("/");
        parts.push(columns);
    }
    parts.join(" ")
}

/// Splits a computed track list or `grid-template-areas` value on top-level
/// whitespace, keeping `[line names]`, `"area strings"` and `function(...)`
/// groups whole.
fn track_list_tokens(value: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    let mut in_string = false;
    for ch in value.chars() {
        if in_string {
            current.push(ch);
            in_string = ch != '"';
            continue;
        }
        match ch {
            '"' => in_string = true,
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.checked_sub(1)?,
            _ if ch.is_whitespace() && depth == 0 => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
                continue;
            }
            _ => {}
        }
        current.push(ch);
    }
    if in_string || depth != 0 {
        return None;
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Some(tokens)
}
