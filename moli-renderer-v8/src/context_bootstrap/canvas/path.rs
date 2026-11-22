//! Path geometry shared by Canvas 2D contexts and detached Path2D objects.

use moli_layout::{
    LayoutPoint, LayoutRect, LayoutTransform2D, PaintPath, PaintPathElement, PaintRect,
};

/// A Bezier path in the same flat form as a kurbo path: one `MoveTo` starts a
/// subpath, `Close` ends it, and a new `MoveTo` begins the next one.
#[derive(Clone, Debug)]
pub(crate) struct Canvas2dPathState {
    // Default-path geometry is already in canvas coordinates. The current
    // transform only affects newly recorded commands, never earlier elements.
    elements: Vec<PaintPathElement>,
    current: (f64, f64),
    current_subpath_start: (f64, f64),
    has_subpath: bool,
    just_closed: bool,
    transform: LayoutTransform2D,
}

impl Default for Canvas2dPathState {
    fn default() -> Self {
        Self {
            elements: Vec::new(),
            current: (0.0, 0.0),
            current_subpath_start: (0.0, 0.0),
            has_subpath: false,
            just_closed: false,
            transform: LayoutTransform2D::IDENTITY,
        }
    }
}

const TWO_PI: f64 = std::f64::consts::TAU;

impl Canvas2dPathState {
    /// Like Blink, a Path2D built from SVG path data keeps its last subpath
    /// open, so later commands continue its contour.
    pub(super) fn from_svg(data: &str) -> Self {
        Self::from_svg_path_data(data)
    }

    pub(super) fn add_path(&mut self, path: &Self, transform: [f64; 6]) {
        if path.is_empty() || !transform.into_iter().all(f64::is_finite) {
            return;
        }
        let transform = kurbo::Affine::new(transform);
        self.elements.extend(path.transformed_elements(transform));
        // Like Blink, the added path's open contour stays current: later
        // commands continue it rather than starting a new subpath.
        let mapped = |(x, y): (f64, f64)| {
            let p = transform * kurbo::Point::new(x, y);
            (p.x, p.y)
        };
        self.current = mapped(path.current);
        self.current_subpath_start = mapped(path.current_subpath_start);
        self.has_subpath = path.has_subpath;
        self.just_closed = path.just_closed;
    }

    pub(super) fn begin_path(&mut self) {
        self.elements.clear();
        self.current = (0.0, 0.0);
        self.current_subpath_start = (0.0, 0.0);
        self.has_subpath = false;
        self.just_closed = false;
    }

    pub(super) fn move_to(&mut self, x: f64, y: f64) {
        if ![x, y].into_iter().all(f64::is_finite) {
            return;
        }
        let (x, y) = self.map_point(x, y);
        self.elements.push(PaintPathElement::MoveTo(point(x, y)));
        self.current = (x, y);
        self.current_subpath_start = (x, y);
        self.has_subpath = true;
        self.just_closed = false;
    }

    /// Reopens a closed subpath; callers establish the initial point for an empty path.
    fn ensure_open_subpath(&mut self) -> bool {
        if !self.has_subpath {
            return false;
        }
        if self.just_closed {
            self.elements.push(PaintPathElement::MoveTo(point(
                self.current.0,
                self.current.1,
            )));
            self.current_subpath_start = self.current;
            self.just_closed = false;
        }
        true
    }

    pub(super) fn line_to(&mut self, x: f64, y: f64) {
        if ![x, y].into_iter().all(f64::is_finite) {
            return;
        }
        if !self.ensure_open_subpath() {
            self.move_to(x, y);
            return;
        }
        let (x, y) = self.map_point(x, y);
        self.elements.push(PaintPathElement::LineTo(point(x, y)));
        self.current = (x, y);
    }

    pub(super) fn quadratic_curve_to(&mut self, cpx: f64, cpy: f64, x: f64, y: f64) {
        if ![cpx, cpy, x, y].into_iter().all(f64::is_finite) {
            return;
        }
        if !self.ensure_open_subpath() {
            self.move_to(cpx, cpy);
        }
        let (cpx, cpy) = self.map_point(cpx, cpy);
        let (x, y) = self.map_point(x, y);
        self.elements
            .push(PaintPathElement::QuadTo(point(cpx, cpy), point(x, y)));
        self.current = (x, y);
    }

    pub(super) fn bezier_curve_to(
        &mut self,
        c1x: f64,
        c1y: f64,
        c2x: f64,
        c2y: f64,
        x: f64,
        y: f64,
    ) {
        if ![c1x, c1y, c2x, c2y, x, y].into_iter().all(f64::is_finite) {
            return;
        }
        if !self.ensure_open_subpath() {
            self.move_to(c1x, c1y);
        }
        let (c1x, c1y) = self.map_point(c1x, c1y);
        let (c2x, c2y) = self.map_point(c2x, c2y);
        let (x, y) = self.map_point(x, y);
        self.elements.push(PaintPathElement::CubicTo(
            point(c1x, c1y),
            point(c2x, c2y),
            point(x, y),
        ));
        self.current = (x, y);
    }

    pub(super) fn close_path(&mut self) {
        if !self.has_subpath || self.just_closed {
            return;
        }
        self.elements.push(PaintPathElement::Close);
        self.current = self.current_subpath_start;
        self.just_closed = true;
    }

    pub(super) fn rect(&mut self, x: f64, y: f64, width: f64, height: f64) {
        if ![x, y, width, height].into_iter().all(f64::is_finite)
            || self.inverse_transform().is_none()
        {
            return;
        }
        self.move_to(x, y);
        self.line_to(x + width, y);
        self.line_to(x + width, y + height);
        self.line_to(x, y + height);
        self.close_path();
    }

    /// Canvas arcs use positive angles clockwise in the screen's y-down space.
    pub(super) fn arc(
        &mut self,
        x: f64,
        y: f64,
        radius: f64,
        start: f64,
        end: f64,
        ccw: bool,
    ) -> bool {
        self.ellipse(x, y, radius, radius, 0.0, start, end, ccw)
    }

    pub(super) fn ellipse(
        &mut self,
        x: f64,
        y: f64,
        radius_x: f64,
        radius_y: f64,
        rotation: f64,
        start: f64,
        end: f64,
        ccw: bool,
    ) -> bool {
        if ![x, y, radius_x, radius_y, rotation, start, end]
            .into_iter()
            .all(f64::is_finite)
            || radius_x < 0.0
            || radius_y < 0.0
            || self.inverse_transform().is_none()
        {
            return false;
        }
        let (start, sweep) = arc_angles(start, end, ccw);
        self.append_arc(kurbo::Arc::new(
            (x, y),
            (radius_x, radius_y),
            start,
            sweep,
            rotation.rem_euclid(TWO_PI),
        ));
        true
    }

    pub(super) fn arc_to(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, radius: f64) -> bool {
        if ![x1, y1, x2, y2, radius].into_iter().all(f64::is_finite) || radius < 0.0 {
            return false;
        }
        if !self.has_subpath {
            self.move_to(x1, y1);
            return true;
        }
        let Some(inverse) = self.inverse_transform() else {
            self.line_to(x1, y1);
            return true;
        };
        // arcTo constructs a circular arc in the current user coordinate
        // system. The previous point was recorded under a possibly older CTM.
        let p0 = inverse * kurbo::Point::from(self.current);
        let p1 = kurbo::Point::new(x1, y1);
        let p2 = kurbo::Point::new(x2, y2);
        let incoming = p0 - p1;
        let outgoing = p2 - p1;
        if incoming.hypot() == 0.0 || outgoing.hypot() == 0.0 || radius == 0.0 {
            self.line_to(x1, y1);
            return true;
        }
        // Both rays originate at the corner. There is no radius clamp: the
        // tangent points may lie beyond either of the supplied line segments.
        let u = incoming / incoming.hypot();
        let v = outgoing / outgoing.hypot();
        let cross = u.cross(v);
        if cross == 0.0 || !cross.is_finite() {
            self.line_to(x1, y1);
            return true;
        }
        let tangent_distance = radius * ((1.0 + u.dot(v).clamp(-1.0, 1.0)) / cross.abs());
        let tangent = p1 + u * tangent_distance;
        let center = tangent + kurbo::Vec2::new(-u.y, u.x) * (radius * cross.signum());
        let end = p1 + v * tangent_distance;
        let start_angle = (tangent - center).atan2();
        let end_angle = (end - center).atan2();
        let (start, sweep) = arc_angles(start_angle, end_angle, cross > 0.0);
        self.append_arc(kurbo::Arc::new(center, (radius, radius), start, sweep, 0.0));
        true
    }

    fn append_arc(&mut self, arc: kurbo::Arc) {
        self.append_arc_ending_at(arc, None);
    }

    /// `end`, when given, replaces the final cubic's endpoint. Ellipse-center
    /// arithmetic loses low coordinate bits at large radii, while an SVG arc's
    /// authored endpoint is exact and anchors the next command.
    fn append_arc_ending_at(&mut self, arc: kurbo::Arc, end: Option<(f64, f64)>) {
        use kurbo::{PathEl, Shape};

        // A relative floor also bounds subdivision for enormous but finite
        // radii. It prevents resource usage growing without bound with radius.
        let tolerance = 0.01_f64.max(arc.radii.x.max(arc.radii.y) / 4096.0);
        let mut elements: Vec<PathEl> = arc.path_elements(tolerance).collect();
        if let (Some((x, y)), Some(PathEl::CurveTo(_, _, last))) = (end, elements.last_mut()) {
            *last = kurbo::Point::new(x, y);
        }
        for element in elements {
            match element {
                PathEl::MoveTo(p) => self.line_to(p.x, p.y),
                PathEl::CurveTo(a, b, p) => self.bezier_curve_to(a.x, a.y, b.x, b.y, p.x, p.y),
                _ => unreachable!("kurbo arcs contain only a start point and cubic segments"),
            }
        }
    }

    pub(super) fn set_transform(&mut self, a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) {
        if [a, b, c, d, e, f].into_iter().all(f64::is_finite) {
            self.transform = LayoutTransform2D::new([a, b, c, d, e, f]);
        }
    }

    pub(super) fn reset_transform(&mut self) {
        self.transform = LayoutTransform2D::IDENTITY;
    }

    pub(super) fn translate(&mut self, x: f64, y: f64) {
        self.concatenate_transform(1.0, 0.0, 0.0, 1.0, x, y);
    }

    pub(super) fn scale(&mut self, x: f64, y: f64) {
        self.concatenate_transform(x, 0.0, 0.0, y, 0.0, 0.0);
    }

    pub(super) fn rotate(&mut self, radians: f64) {
        let (sin, cos) = radians.sin_cos();
        self.concatenate_transform(cos, sin, -sin, cos, 0.0, 0.0);
    }

    pub(super) fn concatenate_transform(&mut self, a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) {
        if [a, b, c, d, e, f].into_iter().all(f64::is_finite) {
            self.transform = self
                .transform
                .concatenate(LayoutTransform2D::new([a, b, c, d, e, f]));
        }
    }

    pub(super) fn transform(&self) -> LayoutTransform2D {
        self.transform
    }

    fn map_point(&self, x: f64, y: f64) -> (f64, f64) {
        let p = kurbo::Affine::new(self.transform.coefficients) * kurbo::Point::new(x, y);
        (p.x, p.y)
    }

    pub(super) fn inverse_transform(&self) -> Option<kurbo::Affine> {
        let transform = kurbo::Affine::new(self.transform.coefficients);
        let determinant = transform.determinant();
        // A small nonzero scale is still invertible; do not use EPSILON as
        // a singularity threshold (e.g. scale(1e-9,1e-9) is valid).
        (determinant != 0.0 && determinant.is_finite()).then(|| transform.inverse())
    }

    /// Stroke metrics are in the *current* user space. Bring the frozen path
    /// back to that space and let the painter transform the resulting stroke,
    /// so anisotropic line widths, joins and dashes change without moving it.
    pub(super) fn stroke_path(&self) -> Option<PaintPath> {
        let elements: Vec<_> = self
            .transformed_elements(self.inverse_transform()?)
            .collect();
        Some(PaintPath {
            bounds: path_bounds(&elements),
            elements,
        })
    }

    fn transformed_elements(
        &self,
        transform: kurbo::Affine,
    ) -> impl Iterator<Item = PaintPathElement> + '_ {
        let map = move |p: LayoutPoint| {
            let p = transform * kurbo::Point::new(f64::from(p.x), f64::from(p.y));
            point(p.x, p.y)
        };
        self.elements.iter().map(move |element| match *element {
            PaintPathElement::MoveTo(p) => PaintPathElement::MoveTo(map(p)),
            PaintPathElement::LineTo(p) => PaintPathElement::LineTo(map(p)),
            PaintPathElement::QuadTo(a, p) => PaintPathElement::QuadTo(map(a), map(p)),
            PaintPathElement::CubicTo(a, b, p) => PaintPathElement::CubicTo(map(a), map(b), map(p)),
            PaintPathElement::Close => PaintPathElement::Close,
        })
    }

    /// Builds an owned `PaintPath` with conservative local bounds.
    pub(super) fn paint_path(&self) -> PaintPath {
        PaintPath {
            elements: self.elements.clone(),
            bounds: path_bounds(&self.elements),
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// The path described by SVG path data (the `Path2D(DOMString)`
    /// constructor). As SVG 2 error handling requires, the segments before
    /// the first error are kept. Arcs go through [`Self::append_arc`], whose
    /// tolerance grows with the radius, so huge radii stay bounded work.
    fn from_svg_path_data(data: &str) -> Self {
        use svgtypes::PathSegment;

        let mut state = Self::default();
        let mut current = (0.0, 0.0);
        let mut start = (0.0, 0.0);
        // The control point the next smooth curve reflects, when the
        // previous segment was the matching curve kind.
        let mut cubic_control = None;
        let mut quadratic_control = None;
        for segment in svgtypes::PathParser::from(data) {
            let Ok(segment) = segment else {
                break;
            };
            let absolute = |abs: bool, x: f64, y: f64| {
                if abs {
                    (x, y)
                } else {
                    (current.0 + x, current.1 + y)
                }
            };
            let reflect = |control: Option<(f64, f64)>| {
                control.map_or(current, |(x, y)| (2.0 * current.0 - x, 2.0 * current.1 - y))
            };
            let (mut next_cubic, mut next_quadratic) = (None, None);
            match segment {
                PathSegment::MoveTo { abs, x, y } => {
                    current = absolute(abs, x, y);
                    start = current;
                    state.move_to(current.0, current.1);
                }
                PathSegment::LineTo { abs, x, y } => {
                    current = absolute(abs, x, y);
                    state.line_to(current.0, current.1);
                }
                PathSegment::HorizontalLineTo { abs, x } => {
                    current.0 = if abs { x } else { current.0 + x };
                    state.line_to(current.0, current.1);
                }
                PathSegment::VerticalLineTo { abs, y } => {
                    current.1 = if abs { y } else { current.1 + y };
                    state.line_to(current.0, current.1);
                }
                PathSegment::CurveTo {
                    abs,
                    x1,
                    y1,
                    x2,
                    y2,
                    x,
                    y,
                } => {
                    let c1 = absolute(abs, x1, y1);
                    let c2 = absolute(abs, x2, y2);
                    current = absolute(abs, x, y);
                    state.bezier_curve_to(c1.0, c1.1, c2.0, c2.1, current.0, current.1);
                    next_cubic = Some(c2);
                }
                PathSegment::SmoothCurveTo { abs, x2, y2, x, y } => {
                    let c1 = reflect(cubic_control);
                    let c2 = absolute(abs, x2, y2);
                    current = absolute(abs, x, y);
                    state.bezier_curve_to(c1.0, c1.1, c2.0, c2.1, current.0, current.1);
                    next_cubic = Some(c2);
                }
                PathSegment::Quadratic { abs, x1, y1, x, y } => {
                    let c = absolute(abs, x1, y1);
                    current = absolute(abs, x, y);
                    state.quadratic_curve_to(c.0, c.1, current.0, current.1);
                    next_quadratic = Some(c);
                }
                PathSegment::SmoothQuadratic { abs, x, y } => {
                    let c = reflect(quadratic_control);
                    current = absolute(abs, x, y);
                    state.quadratic_curve_to(c.0, c.1, current.0, current.1);
                    next_quadratic = Some(c);
                }
                PathSegment::EllipticalArc {
                    abs,
                    rx,
                    ry,
                    x_axis_rotation,
                    large_arc,
                    sweep,
                    x,
                    y,
                } => {
                    let from = current;
                    current = absolute(abs, x, y);
                    // Identical endpoints omit the arc; zero radii make it a
                    // straight line (SVG 2 "Out-of-range elliptical arc
                    // parameters"). from_svg_arc covers both by returning None.
                    let arc = kurbo::Arc::from_svg_arc(&kurbo::SvgArc {
                        from: kurbo::Point::new(from.0, from.1),
                        to: kurbo::Point::new(current.0, current.1),
                        radii: kurbo::Vec2::new(rx, ry),
                        x_rotation: x_axis_rotation.to_radians(),
                        large_arc,
                        sweep,
                    });
                    match arc {
                        Some(arc) => state.append_arc_ending_at(arc, Some(current)),
                        None if from != current => state.line_to(current.0, current.1),
                        None => {}
                    }
                }
                PathSegment::ClosePath { .. } => {
                    state.close_path();
                    current = start;
                }
            }
            cubic_control = next_cubic;
            quadratic_control = next_quadratic;
        }
        state
    }
}

fn path_bounds(elements: &[PaintPathElement]) -> PaintRect {
    if elements.is_empty() {
        return LayoutRect::ZERO;
    }
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for element in elements {
        match *element {
            PaintPathElement::MoveTo(point) | PaintPathElement::LineTo(point) => {
                expand(&mut min_x, &mut min_y, &mut max_x, &mut max_y, point);
            }
            PaintPathElement::QuadTo(first, second) => {
                expand(&mut min_x, &mut min_y, &mut max_x, &mut max_y, first);
                expand(&mut min_x, &mut min_y, &mut max_x, &mut max_y, second);
            }
            PaintPathElement::CubicTo(first, second, third) => {
                expand(&mut min_x, &mut min_y, &mut max_x, &mut max_y, first);
                expand(&mut min_x, &mut min_y, &mut max_x, &mut max_y, second);
                expand(&mut min_x, &mut min_y, &mut max_x, &mut max_y, third);
            }
            PaintPathElement::Close => {}
        }
    }
    LayoutRect::new(
        min_x as f32,
        min_y as f32,
        enclosing_extent(min_x, max_x),
        enclosing_extent(min_y, max_y),
    )
}

fn enclosing_extent(start: f64, end: f64) -> f32 {
    let extent = (end - start) as f32;
    // Reconstruct the far edge the same way as the raster crop. A rounded
    // extent can lose pixels near the end of a path spanning large coordinates.
    if start + f64::from(extent) < end {
        extent.next_up()
    } else {
        extent
    }
}

fn point(x: f64, y: f64) -> LayoutPoint {
    LayoutPoint::new(x as f32, y as f32)
}

fn expand(min_x: &mut f64, min_y: &mut f64, max_x: &mut f64, max_y: &mut f64, point: LayoutPoint) {
    let x = f64::from(point.x);
    let y = f64::from(point.y);
    *min_x = (*min_x).min(x);
    *min_y = (*min_y).min(y);
    *max_x = (*max_x).max(x);
    *max_y = (*max_y).max(y);
}

/// Normalize in constant time, including opposite-sign finite angles whose
/// subtraction overflows. Never repeatedly add/subtract TAU: at large f64
/// magnitudes that operation does not advance at all.
fn arc_angles(start: f64, end: f64, ccw: bool) -> (f64, f64) {
    let difference = end - start;
    let normalized_start = start.rem_euclid(TWO_PI);
    let sweep = if !ccw && difference >= TWO_PI {
        TWO_PI
    } else if ccw && difference <= -TWO_PI {
        -TWO_PI
    } else if difference == 0.0 {
        0.0
    } else {
        let remainder = if difference.is_finite() {
            difference.rem_euclid(TWO_PI)
        } else {
            (end.rem_euclid(TWO_PI) - normalized_start).rem_euclid(TWO_PI)
        };
        if ccw {
            // Like Blink's AdjustEndAngle, preserve a whole turn for opposite-
            // direction endpoints separated by an exact multiple of TAU.
            if remainder == 0.0 && difference < 0.0 {
                0.0
            } else {
                remainder - TWO_PI
            }
        } else if remainder == 0.0 && difference < 0.0 {
            TWO_PI
        } else {
            remainder
        }
    };
    (normalized_start, sweep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::FRAC_PI_2;

    #[test]
    fn arc_angles_preserve_direction_full_turns_and_equal_endpoints() {
        for (start, end, ccw, expected) in [
            (0.0, FRAC_PI_2, false, FRAC_PI_2),
            (0.0, FRAC_PI_2, true, -3.0 * FRAC_PI_2),
            (0.0, -FRAC_PI_2, true, -FRAC_PI_2),
            (0.0, -FRAC_PI_2, false, 3.0 * FRAC_PI_2),
            (0.0, TWO_PI, false, TWO_PI),
            (0.0, -TWO_PI, true, -TWO_PI),
            (0.0, TWO_PI, true, -TWO_PI),
            (0.0, -TWO_PI, false, TWO_PI),
            (2.0, 2.0, true, 0.0),
            (2.0, 2.0, false, 0.0),
        ] {
            assert!((arc_angles(start, end, ccw).1 - expected).abs() < 1e-12);
        }
    }

    #[test]
    fn finite_extreme_angles_produce_bounded_path_work() {
        for start in [0.0, 1e20, -1e20, f64::MAX, -f64::MAX] {
            for end in [0.0, 1e20, -1e20, f64::MAX, -f64::MAX] {
                for ccw in [false, true] {
                    let (angle, sweep) = arc_angles(start, end, ccw);
                    assert!(angle.is_finite() && (0.0..TWO_PI).contains(&angle));
                    assert!(sweep.is_finite() && sweep.abs() <= TWO_PI);
                    assert!(if ccw { sweep <= 0.0 } else { sweep >= 0.0 });
                    let mut state = Canvas2dPathState::default();
                    assert!(state.arc(10.0, 10.0, 5.0, start, end, ccw));
                    assert!(state.elements.len() <= 7);
                }
            }
        }
    }

    #[test]
    fn arc_to_has_the_correct_tangent_and_endpoint() {
        let mut state = Canvas2dPathState::default();
        state.move_to(0.0, 0.0);
        assert!(state.arc_to(10.0, 0.0, 10.0, 10.0, 5.0));
        let PaintPathElement::LineTo(tangent) = state.elements[1] else {
            panic!("arcTo must connect to its first tangent");
        };
        assert!((tangent.x - 5.0).abs() < 1e-5 && tangent.y.abs() < 1e-5);
        assert!((state.current.0 - 10.0).abs() < 1e-5);
        assert!((state.current.1 - 5.0).abs() < 1e-5);
    }

    #[test]
    fn added_paths_are_mapped_and_move_the_current_point() {
        let mut source = Canvas2dPathState::from_svg_path_data("M1 2 L3 4 Z");
        source.line_to(5.0, 6.0);
        let mut target = Canvas2dPathState::default();
        target.add_path(&source, [1.0, 0.0, 0.0, 1.0, 10.0, 20.0]);
        assert!(matches!(
            target.elements[..],
            [
                PaintPathElement::MoveTo(a),
                PaintPathElement::LineTo(b),
                PaintPathElement::Close,
                PaintPathElement::MoveTo(c),
                PaintPathElement::LineTo(d),
            ] if (a.x, a.y, b.x, b.y, c.x, c.y, d.x, d.y)
                == (11.0, 22.0, 13.0, 24.0, 11.0, 22.0, 15.0, 26.0)
        ));
        assert_eq!(target.current, (15.0, 26.0));
    }

    #[test]
    fn svg_path_data_arcs_are_bounded_and_errors_keep_the_prefix() {
        // kurbo's own SVG parser flattens this arc at an absolute tolerance
        // into ~1e10 cubics; the radius-relative tolerance keeps it small.
        let huge = Canvas2dPathState::from_svg_path_data("M0 0 A1e60 1e60 0 1 1 1e50 0");
        assert!((2..=64).contains(&huge.elements.len()));
        let partial = Canvas2dPathState::from_svg_path_data("M10 10 L50 10 L50 50 Z X 1 2");
        assert_eq!(partial.elements.len(), 4);
        assert!(matches!(partial.elements[3], PaintPathElement::Close));
        let far = Canvas2dPathState::from_svg_path_data("M10 20 A1e17 1e17 0 0 1 45 20");
        assert_eq!(far.current, (45.0, 20.0));
    }

    #[test]
    fn rejected_arc_geometry_does_not_mutate_the_path() {
        let mut state = Canvas2dPathState::default();
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
            assert!(!state.arc(0.0, 0.0, invalid, 0.0, 1.0, false));
            assert!(!state.ellipse(0.0, 0.0, 5.0, invalid, 0.0, 0.0, 1.0, false));
            assert!(!state.arc_to(0.0, 0.0, 1.0, 1.0, invalid));
            assert!(state.is_empty());
        }
    }
}
