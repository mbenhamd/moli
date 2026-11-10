use crate::helpers::svg_number_list;
use crate::matrix::SvgMatrixComponents;
use crate::path::path_geometry;
use kurbo::{
    Arc, BezPath, Circle, Ellipse, Line, ParamCurve, ParamCurveArclen, ParamCurveExtrema,
    ParamCurveNearest, PathEl, PathSeg, Point, Rect, Shape,
};

const SHAPE_PATH_TOLERANCE: f64 = 0.1;
const ARC_LENGTH_ACCURACY: f64 = 1e-6;
const FILL_BOUNDARY_EPSILON: f64 = 1e-9;

#[derive(Clone, Copy, Debug)]
pub struct SvgGeometryPoint {
    pub x: f64,
    pub y: f64,
}

impl SvgGeometryPoint {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SvgGeometrySegment {
    inner: PathSeg,
}

impl SvgGeometrySegment {
    fn new(inner: PathSeg) -> Self {
        Self { inner }
    }

    pub fn length(&self) -> f64 {
        self.inner.arclen(ARC_LENGTH_ACCURACY)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SvgGeometryBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl SvgGeometryBox {
    pub fn union(self, other: Self) -> Self {
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = (self.x + self.width).max(other.x + other.width);
        let y1 = (self.y + self.height).max(other.y + other.height);
        Self {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        }
    }
}

pub enum SvgGeometryElement {
    Circle {
        cx: f64,
        cy: f64,
        r: f64,
    },
    Ellipse {
        cx: f64,
        cy: f64,
        rx: f64,
        ry: f64,
    },
    Line {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
    },
    Path {
        d: String,
    },
    Polygon {
        points: String,
    },
    Polyline {
        points: String,
    },
    Rect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        rx: f64,
        ry: f64,
    },
}

pub fn segments_for_element(element: SvgGeometryElement) -> Vec<SvgGeometrySegment> {
    let path = distance_query_path(&element).unwrap_or_default();
    let segments = path
        .segments()
        .map(SvgGeometrySegment::new)
        .collect::<Vec<_>>();
    if !segments.is_empty() {
        return segments;
    }
    path.elements()
        .iter()
        .find_map(|element| match element {
            PathEl::MoveTo(point) => Some(vec![SvgGeometrySegment::new(
                Line::new(*point, *point).into(),
            )]),
            _ => None,
        })
        .unwrap_or_default()
}

pub fn bounding_box_for_segments(segments: &[SvgGeometrySegment]) -> Option<SvgGeometryBox> {
    let mut segments = segments.iter();
    let first = segments.next()?;
    let bounds = segments.fold(
        ParamCurveExtrema::bounding_box(&first.inner),
        |bounds, segment| bounds.union(ParamCurveExtrema::bounding_box(&segment.inner)),
    );
    Some(SvgGeometryBox {
        x: bounds.x0,
        y: bounds.y0,
        width: bounds.width(),
        height: bounds.height(),
    })
}

pub fn bounding_box_for_element(element: &SvgGeometryElement) -> Option<SvgGeometryBox> {
    bounding_box_for_transformed_element(element, SvgMatrixComponents::identity())
}

pub fn bounding_box_for_transformed_element(
    element: &SvgGeometryElement,
    transform: SvgMatrixComponents,
) -> Option<SvgGeometryBox> {
    let affine = transform.to_affine();
    let analytic_bounds = match element {
        SvgGeometryElement::Circle { cx, cy, r } if *r > 0.0 => {
            Some((affine * Circle::new(Point::new(*cx, *cy), *r)).bounding_box())
        }
        SvgGeometryElement::Ellipse { cx, cy, rx, ry } => {
            let (rx, ry) = normalized_ellipse_radii(*rx, *ry);
            (rx > 0.0 && ry > 0.0).then(|| {
                (affine * Ellipse::new(Point::new(*cx, *cy), (rx, ry), 0.0)).bounding_box()
            })
        }
        _ => None,
    };
    if let Some(bounds) = analytic_bounds {
        return Some(svg_box(bounds));
    }
    let path = geometry_path(element)?;
    if path.elements().is_empty() {
        return None;
    }
    if path.segments().next().is_none() {
        let point = path
            .elements()
            .iter()
            .rev()
            .find_map(|element| match element {
                PathEl::MoveTo(point) => Some(affine * *point),
                _ => None,
            })?;
        return Some(SvgGeometryBox {
            x: point.x,
            y: point.y,
            width: 0.0,
            height: 0.0,
        });
    }
    let bounds = (affine * path).bounding_box();
    Some(svg_box(bounds))
}

fn svg_box(bounds: Rect) -> SvgGeometryBox {
    SvgGeometryBox {
        x: bounds.x0,
        y: bounds.y0,
        width: bounds.width(),
        height: bounds.height(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SvgFillRule {
    NonZero,
    EvenOdd,
}

pub fn is_point_in_fill(element: &SvgGeometryElement, point: SvgGeometryPoint) -> bool {
    is_point_in_fill_with_rule(element, point, SvgFillRule::NonZero)
}

pub fn is_point_in_fill_with_rule(
    element: &SvgGeometryElement,
    point: SvgGeometryPoint,
    fill_rule: SvgFillRule,
) -> bool {
    if !point.x.is_finite()
        || !point.y.is_finite()
        || matches!(element, SvgGeometryElement::Line { .. })
    {
        return false;
    }
    geometry_path(element)
        .is_some_and(|path| path_fill_contains(&path, Point::new(point.x, point.y), fill_rule))
}

fn path_fill_contains(path: &BezPath, point: Point, fill_rule: SvgFillRule) -> bool {
    let path = close_subpaths_for_fill(path);
    let winding = path.winding(point);
    let inside = match fill_rule {
        SvgFillRule::NonZero => winding != 0,
        SvgFillRule::EvenOdd => winding % 2 != 0,
    };
    inside
        || path.segments().any(|segment| {
            segment.nearest(point, FILL_BOUNDARY_EPSILON).distance_sq
                <= FILL_BOUNDARY_EPSILON.powi(2)
        })
}

/// The painted parts of a shape that a hit test considers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SvgShapeHitArea<'a> {
    /// The fill rule when the fill is painted.
    pub fill: Option<SvgFillRule>,
    /// The stroke when it is painted.
    pub stroke: Option<SvgStrokeHitArea<'a>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SvgStrokeLinecap {
    Butt,
    Round,
    Square,
}

/// A painted stroke in the shape's user space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SvgStrokeHitArea<'a> {
    pub width: f64,
    pub linecap: SvgStrokeLinecap,
    /// Resolved `stroke-dasharray` lengths; empty for a solid stroke.
    pub dashes: &'a [f64],
    pub dash_offset: f64,
    /// A positive author `pathLength`. Dash lengths and the offset are scaled
    /// by the computed path length over it.
    pub path_length: Option<f64>,
}

/// Whether `point` lies on the painted fill or stroke of `element`.
///
/// Basic shapes and point lists are rejected on their bounds before any path
/// is built; a `<path>` is parsed once and rejected on its bounds. The path
/// then serves both the fill and the stroke test.
///
/// The stroke outline is flattened with a user-space tolerance of a hundredth
/// of the half width (at least 1e-4, at most 0.05), so the stroke edge is
/// located to within that distance. Each dash and open subpath ends with its
/// `stroke-linecap`. Joins are round, which under-hits miter corners and
/// over-hits bevel corners by at most the half width. A dash array that
/// cannot dash (non-finite, negative or summing to zero) leaves the stroke
/// solid, as SVG requires.
pub fn is_point_in_shape_hit_area(
    element: &SvgGeometryElement,
    point: SvgGeometryPoint,
    area: SvgShapeHitArea<'_>,
) -> bool {
    if !point.x.is_finite() || !point.y.is_finite() {
        return false;
    }
    let fill = area
        .fill
        .filter(|_| !matches!(element, SvgGeometryElement::Line { .. }));
    let stroke = area.stroke.filter(|stroke| stroke.width > 0.0);
    if fill.is_none() && stroke.is_none() {
        return false;
    }
    let point = Point::new(point.x, point.y);
    let half_width = stroke.map_or(0.0, |stroke| stroke.width / 2.0);
    // Square caps reach furthest: half the width along both axes of the cap.
    let reach = (half_width * std::f64::consts::SQRT_2).max(FILL_BOUNDARY_EPSILON);
    let outside = |bounds: Rect| {
        point.x < bounds.x0 - reach
            || point.x > bounds.x1 + reach
            || point.y < bounds.y0 - reach
            || point.y > bounds.y1 + reach
    };
    let path = match element {
        SvgGeometryElement::Polygon { points } | SvgGeometryElement::Polyline { points } => {
            let Some(coordinates) = poly_coordinates(points) else {
                return false;
            };
            if outside(coordinates_bounds(&coordinates)) {
                return false;
            }
            poly_coordinates_path(
                &coordinates,
                matches!(element, SvgGeometryElement::Polygon { .. }),
            )
        }
        SvgGeometryElement::Path { d } => {
            let Some(path) = path_geometry(d) else {
                return false;
            };
            if path.elements().is_empty() || outside(path.control_box()) {
                return false;
            }
            path
        }
        _ => {
            if analytic_bounds(element).is_none_or(outside) {
                // Degenerate basic shapes render nothing, except a
                // zero-length line, whose bounds are a point.
                return false;
            }
            let Some(path) = geometry_path(element) else {
                return false;
            };
            path
        }
    };
    if fill.is_some_and(|rule| path_fill_contains(&path, point, rule)) {
        return true;
    }
    let Some(stroke) = stroke else {
        return false;
    };
    let tolerance = (half_width / 100.0).clamp(1e-4, 0.05);
    let mut subpaths = FlatSubpaths::default();
    if dash_array_dashes(stroke.dashes) {
        let mut scale = 1.0;
        if let Some(path_length) = stroke.path_length.filter(|length| *length > 0.0) {
            let length = path
                .segments()
                .map(|segment| segment.arclen(ARC_LENGTH_ACCURACY))
                .sum::<f64>();
            scale = length / path_length;
        }
        let dashes = stroke
            .dashes
            .iter()
            .map(|dash| dash * scale)
            .collect::<Vec<_>>();
        let offset = if stroke.dash_offset.is_finite() {
            stroke.dash_offset * scale
        } else {
            0.0
        };
        if !dash_array_dashes(&dashes) {
            return false;
        }
        kurbo::flatten(
            kurbo::dash(path.iter(), offset, &dashes),
            tolerance,
            |element| subpaths.push(element),
        );
    } else {
        kurbo::flatten(path.iter(), tolerance, |element| subpaths.push(element));
    }
    subpaths.finish();
    subpaths.subpaths.iter().any(|(points, closed)| {
        flat_subpath_stroke_contains(points, *closed, point, half_width, stroke.linecap)
    })
}

#[derive(Default)]
struct FlatSubpaths {
    subpaths: Vec<(Vec<Point>, bool)>,
    current: Vec<Point>,
    /// A lone move-to paints nothing; a zero-length segment paints caps.
    drawn: bool,
}

impl FlatSubpaths {
    fn push(&mut self, element: PathEl) {
        match element {
            PathEl::MoveTo(point) => {
                self.finish();
                self.current.push(point);
            }
            PathEl::LineTo(point) => {
                self.drawn = true;
                if self.current.last() != Some(&point) {
                    self.current.push(point);
                }
            }
            PathEl::ClosePath => {
                let mut points = std::mem::take(&mut self.current);
                if points.len() > 1 && points.first() == points.last() {
                    points.pop();
                }
                if let Some(start) = points.first().copied() {
                    self.subpaths.push((points, true));
                    // A following segment without a move starts here.
                    self.current.push(start);
                }
                self.drawn = false;
            }
            PathEl::QuadTo(..) | PathEl::CurveTo(..) => {}
        }
    }

    fn finish(&mut self) {
        let points = std::mem::take(&mut self.current);
        if self.drawn && !points.is_empty() {
            self.subpaths.push((points, false));
        }
        self.drawn = false;
    }
}

/// The stroke of one flattened subpath (consecutive points distinct).
fn flat_subpath_stroke_contains(
    points: &[Point],
    closed: bool,
    point: Point,
    half_width: f64,
    linecap: SvgStrokeLinecap,
) -> bool {
    let limit = half_width * half_width;
    let [first, .., last] = points else {
        // A zero-length subpath paints only its caps, aligned with the x axis.
        let Some(only) = points.first() else {
            return false;
        };
        return match linecap {
            SvgStrokeLinecap::Butt => false,
            SvgStrokeLinecap::Round => (point - *only).hypot2() <= limit,
            SvgStrokeLinecap::Square => {
                (point.x - only.x).abs() <= half_width && (point.y - only.y).abs() <= half_width
            }
        };
    };
    let within_segment = |start: Point, end: Point| {
        let direction = end - start;
        let along = (point - start).dot(direction) / direction.hypot2();
        (0.0..=1.0).contains(&along) && (point - (start + direction * along)).hypot2() <= limit
    };
    if points
        .windows(2)
        .any(|pair| within_segment(pair[0], pair[1]))
        || (closed && points.len() > 2 && within_segment(*last, *first))
    {
        return true;
    }
    let joins = if closed {
        points
    } else {
        &points[1..points.len() - 1]
    };
    if joins
        .iter()
        .any(|vertex| (point - *vertex).hypot2() <= limit)
    {
        return true;
    }
    if closed {
        return false;
    }
    let cap_contains = |end: Point, outward: kurbo::Vec2| match linecap {
        SvgStrokeLinecap::Butt => false,
        SvgStrokeLinecap::Round => (point - end).hypot2() <= limit,
        SvgStrokeLinecap::Square => {
            let outward = outward / outward.hypot();
            let offset = point - end;
            let along = offset.dot(outward);
            (0.0..=half_width).contains(&along) && offset.cross(outward).abs() <= half_width
        }
    };
    cap_contains(*first, *first - points[1])
        || cap_contains(*last, *last - points[points.len() - 2])
}

fn dash_array_dashes(dashes: &[f64]) -> bool {
    let total = dashes.iter().sum::<f64>();
    !dashes.is_empty()
        && dashes.iter().all(|dash| dash.is_finite() && *dash >= 0.0)
        && total.is_finite()
        && total > 0.0
}

/// Bounds of the basic shapes without building their path.
fn analytic_bounds(element: &SvgGeometryElement) -> Option<Rect> {
    match element {
        SvgGeometryElement::Circle { cx, cy, r } if *r > 0.0 => {
            Some(Rect::new(cx - r, cy - r, cx + r, cy + r))
        }
        SvgGeometryElement::Ellipse { cx, cy, rx, ry } => {
            let (rx, ry) = normalized_ellipse_radii(*rx, *ry);
            (rx > 0.0 && ry > 0.0).then(|| Rect::new(cx - rx, cy - ry, cx + rx, cy + ry))
        }
        SvgGeometryElement::Line { x1, y1, x2, y2 } => {
            Some(Rect::from_points((*x1, *y1), (*x2, *y2)))
        }
        SvgGeometryElement::Rect {
            x,
            y,
            width,
            height,
            ..
        } if *width > 0.0 && *height > 0.0 => Some(Rect::new(*x, *y, x + width, y + height)),
        _ => None,
    }
}

fn coordinates_bounds(coordinates: &[f64]) -> Rect {
    coordinates.as_chunks::<2>().0.iter().fold(
        Rect::new(
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ),
        |bounds, [x, y]| {
            Rect::new(
                bounds.x0.min(*x),
                bounds.y0.min(*y),
                bounds.x1.max(*x),
                bounds.y1.max(*y),
            )
        },
    )
}

pub fn point_at_length(segments: &[SvgGeometrySegment], distance: f64) -> SvgGeometryPoint {
    let Some(first) = segments.first() else {
        return SvgGeometryPoint::new(0.0, 0.0);
    };
    if distance <= 0.0 {
        return svg_point(first.inner.start());
    }
    let mut remaining = distance;
    for segment in segments {
        let length = segment.length();
        if length == 0.0 {
            continue;
        }
        if remaining <= length {
            let parameter = segment.inner.inv_arclen(remaining, ARC_LENGTH_ACCURACY);
            return svg_point(segment.inner.eval(parameter));
        }
        remaining -= length;
    }
    segments
        .last()
        .map(|segment| svg_point(segment.inner.end()))
        .unwrap_or(SvgGeometryPoint::new(0.0, 0.0))
}

fn distance_query_path(element: &SvgGeometryElement) -> Option<BezPath> {
    // A zero axis disables rendering, but distance queries can still compute
    // the equivalent path along the other axis.
    match element {
        SvgGeometryElement::Rect {
            x,
            y,
            width,
            height,
            ..
        } if (*width == 0.0 && *height > 0.0) || (*height == 0.0 && *width > 0.0) => {
            Some(Rect::new(*x, *y, *x + *width, *y + *height).to_path(SHAPE_PATH_TOLERANCE))
        }
        SvgGeometryElement::Ellipse { cx, cy, rx, ry }
            if (*rx == 0.0 && *ry > 0.0) || (*ry == 0.0 && *rx > 0.0) =>
        {
            // Collapsed quarter arcs are lines. Preserve SVG's starting point
            // and sweep instead of normalizing the ellipse's axes.
            let mut path = BezPath::new();
            path.move_to((*cx + *rx, *cy));
            path.line_to((*cx, *cy + *ry));
            path.line_to((*cx - *rx, *cy));
            path.line_to((*cx, *cy - *ry));
            path.close_path();
            Some(path)
        }
        _ => geometry_path(element),
    }
}

fn geometry_path(element: &SvgGeometryElement) -> Option<BezPath> {
    match element {
        SvgGeometryElement::Circle { cx, cy, r } => Some(if *r > 0.0 {
            Circle::new(Point::new(*cx, *cy), *r).to_path(SHAPE_PATH_TOLERANCE)
        } else {
            BezPath::new()
        }),
        SvgGeometryElement::Ellipse { cx, cy, rx, ry } => {
            let (rx, ry) = normalized_ellipse_radii(*rx, *ry);
            Some(if rx > 0.0 && ry > 0.0 {
                Ellipse::new(Point::new(*cx, *cy), (rx, ry), 0.0).to_path(SHAPE_PATH_TOLERANCE)
            } else {
                BezPath::new()
            })
        }
        SvgGeometryElement::Line { x1, y1, x2, y2 } => {
            let mut path = BezPath::new();
            path.move_to((*x1, *y1));
            path.line_to((*x2, *y2));
            Some(path)
        }
        SvgGeometryElement::Path { d } => path_geometry(d),
        SvgGeometryElement::Polygon { points } => poly_points_geometry_path(points, true),
        SvgGeometryElement::Polyline { points } => poly_points_geometry_path(points, false),
        SvgGeometryElement::Rect {
            x,
            y,
            width,
            height,
            rx,
            ry,
        } => Some(rect_path(*x, *y, *width, *height, *rx, *ry)),
    }
}

fn rect_path(x: f64, y: f64, width: f64, height: f64, rx: f64, ry: f64) -> BezPath {
    if width <= 0.0 || height <= 0.0 {
        return BezPath::new();
    }
    let (rx, ry) = normalized_rect_radii(width, height, rx, ry);
    if rx > 0.0 && ry > 0.0 {
        return rounded_rect_path(x, y, width, height, rx, ry);
    }
    Rect::new(x, y, x + width, y + height).to_path(SHAPE_PATH_TOLERANCE)
}

fn normalized_ellipse_radii(rx: f64, ry: f64) -> (f64, f64) {
    match (rx.is_sign_negative(), ry.is_sign_negative()) {
        (true, false) => (ry, ry),
        (false, true) => (rx, rx),
        _ => (rx, ry),
    }
}

fn normalized_rect_radii(width: f64, height: f64, rx: f64, ry: f64) -> (f64, f64) {
    if rx <= 0.0 || ry <= 0.0 {
        return (0.0, 0.0);
    }
    (rx.min(width / 2.0), ry.min(height / 2.0))
}

fn rounded_rect_path(x: f64, y: f64, width: f64, height: f64, rx: f64, ry: f64) -> BezPath {
    let mut path = BezPath::new();
    path.move_to((x + rx, y));
    path.line_to((x + width - rx, y));
    append_quarter_ellipse(&mut path, x + width - rx, y + ry, rx, ry, -90.0);
    path.line_to((x + width, y + height - ry));
    append_quarter_ellipse(&mut path, x + width - rx, y + height - ry, rx, ry, 0.0);
    path.line_to((x + rx, y + height));
    append_quarter_ellipse(&mut path, x + rx, y + height - ry, rx, ry, 90.0);
    path.line_to((x, y + ry));
    append_quarter_ellipse(&mut path, x + rx, y + ry, rx, ry, 180.0);
    path.close_path();
    path
}

fn append_quarter_ellipse(
    path: &mut BezPath,
    cx: f64,
    cy: f64,
    rx: f64,
    ry: f64,
    start_degrees: f64,
) {
    path.extend(
        Arc::new(
            Point::new(cx, cy),
            (rx, ry),
            start_degrees.to_radians(),
            std::f64::consts::FRAC_PI_2,
            0.0,
        )
        .append_iter(SHAPE_PATH_TOLERANCE),
    );
}

fn poly_points_geometry_path(raw: &str, close: bool) -> Option<BezPath> {
    Some(poly_coordinates_path(&poly_coordinates(raw)?, close))
}

/// A point list's coordinates, when it has at least one complete pair.
fn poly_coordinates(raw: &str) -> Option<Vec<f64>> {
    svg_number_list(raw)
        .filter(|coordinates| coordinates.len() >= 2 && coordinates.len().is_multiple_of(2))
}

fn poly_coordinates_path(coordinates: &[f64], close: bool) -> BezPath {
    let mut path = BezPath::new();
    let mut pairs = coordinates.as_chunks::<2>().0.iter();
    if let Some([x, y]) = pairs.next() {
        path.move_to((*x, *y));
    }
    for [x, y] in pairs {
        path.line_to((*x, *y));
    }
    if close {
        path.close_path();
    }
    path
}

fn close_subpaths_for_fill(path: &BezPath) -> BezPath {
    let mut closed = BezPath::new();
    let mut subpath_is_open = false;
    for element in path.elements() {
        match *element {
            PathEl::MoveTo(point) => {
                if subpath_is_open {
                    closed.close_path();
                }
                closed.move_to(point);
                subpath_is_open = true;
            }
            PathEl::ClosePath => {
                closed.close_path();
                subpath_is_open = false;
            }
            element => {
                closed.push(element);
                subpath_is_open = true;
            }
        }
    }
    if subpath_is_open {
        closed.close_path();
    }
    closed
}

fn svg_point(point: Point) -> SvgGeometryPoint {
    SvgGeometryPoint::new(point.x, point.y)
}

#[cfg(test)]
mod fill_containment_rejects_nonfinite_coordinates_tests {
    use super::*;
    #[test]
    fn fill_containment_rejects_nonfinite_coordinates() {
        let circle = SvgGeometryElement::Circle {
            cx: 0.0,
            cy: 0.0,
            r: 10.0,
        };
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for point in [
                SvgGeometryPoint::new(value, 0.0),
                SvgGeometryPoint::new(0.0, value),
            ] {
                for rule in [SvgFillRule::NonZero, SvgFillRule::EvenOdd] {
                    assert!(!is_point_in_fill_with_rule(&circle, point, rule));
                }
            }
        }
    }
}

#[cfg(test)]
mod evenodd_fill_excludes_nested_interiors_but_includes_boundaries_tests {
    use super::*;
    #[test]
    fn evenodd_fill_excludes_nested_interiors_but_includes_boundaries() {
        let rings = SvgGeometryElement::Path {
            d: "M0 0H10V10H0Z M2 2H8V8H2Z".to_owned(),
        };
        let center = SvgGeometryPoint::new(5.0, 5.0);
        assert!(is_point_in_fill(&rings, center));
        assert!(!is_point_in_fill_with_rule(
            &rings,
            center,
            SvgFillRule::EvenOdd
        ));
        for point in [
            SvgGeometryPoint::new(1.0, 5.0),
            SvgGeometryPoint::new(2.0, 5.0),
            SvgGeometryPoint::new(8.0, 5.0),
            SvgGeometryPoint::new(10.0, 5.0),
        ] {
            assert!(is_point_in_fill_with_rule(
                &rings,
                point,
                SvgFillRule::EvenOdd
            ));
        }
    }
}
