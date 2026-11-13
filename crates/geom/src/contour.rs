//! Closed outlines made of straight and arc edges, and polygons with holes.

use crate::{Arc, GeomError, MAX_POINTS, Rect, check_point};
use circuitcraft_units::{Nm, Point};
use serde::{Deserialize, Serialize};

/// One edge of a [`Contour`], from the previous point to `to`. A file writes it under its kind:
/// `{"line": {"to": …}}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Edge {
    /// A straight edge.
    Line {
        /// End point.
        to: Point,
    },
    /// An arc through `mid`.
    Arc {
        /// A point on the arc between its ends.
        mid: Point,
        /// End point.
        to: Point,
    },
}

impl Edge {
    /// Where the edge ends.
    pub fn end(&self) -> Point {
        match *self {
            Edge::Line { to } | Edge::Arc { to, .. } => to,
        }
    }
}

/// A closed outline: from `start` along each edge, and back to `start` with a straight edge if
/// the last edge doesn't already end there. Board outlines, pours and pad shapes are contours.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contour {
    /// The first point.
    pub start: Point,
    /// The edges, in order.
    pub edges: Vec<Edge>,
}

impl Contour {
    /// A contour of straight edges through `points` (closed automatically).
    pub fn polyline(points: &[Point]) -> Option<Contour> {
        let (&start, rest) = points.split_first()?;
        Some(Contour { start, edges: rest.iter().map(|&to| Edge::Line { to }).collect() })
    }

    /// An axis-aligned rectangle, counter-clockwise.
    pub fn rect(r: Rect) -> Contour {
        let (a, b) = (r.min, r.max);
        Contour { start: a, edges: vec![Edge::Line { to: Point::new(b.x, a.y) }, Edge::Line { to: b }, Edge::Line { to: Point::new(a.x, b.y) }] }
    }

    /// A circle of radius `r` around `center`, as a single full-circle arc.
    pub fn circle(center: Point, r: Nm) -> Result<Contour, GeomError> {
        check_point(center)?;
        let start = Point::new(center.x.checked_add(r)?, center.y);
        let mid = Point::new(center.x.checked_sub(r)?, center.y);
        // checked_add/sub only catch i64 overflow; the design range is narrower.
        check_point(start)?;
        check_point(mid)?;
        Ok(Contour { start, edges: vec![Edge::Arc { mid, to: start }] })
    }

    /// The outline as points, arcs approximated within `tolerance` (see [`Arc::flatten`]).
    /// Consecutive duplicates are removed and the closing point is not repeated.
    pub fn flatten(&self, tolerance: Nm) -> Result<Vec<Point>, GeomError> {
        check_point(self.start)?;
        let mut out = vec![self.start];
        let mut at = self.start;
        for edge in &self.edges {
            match *edge {
                Edge::Line { to } => {
                    check_point(to)?;
                    push(&mut out, to)?;
                }
                Edge::Arc { mid, to } => {
                    for p in Arc::new(at, mid, to).flatten(tolerance)?.into_iter().skip(1) {
                        push(&mut out, p)?;
                    }
                }
            }
            at = edge.end();
        }
        if out.len() > 1 && out.last() == Some(&self.start) {
            out.pop();
        }
        Ok(out)
    }
}

fn push(out: &mut Vec<Point>, p: Point) -> Result<(), GeomError> {
    if out.last() != Some(&p) {
        if out.len() >= MAX_POINTS {
            return Err(GeomError::TooManyPoints);
        }
        out.push(p);
    }
    Ok(())
}

/// Twice the signed area enclosed by a closed point ring, exactly: positive when the ring runs
/// counter-clockwise (y up). Returns `None` only for coordinates far outside the design range.
pub fn area2(ring: &[Point]) -> Option<i128> {
    let n = ring.len();
    let mut sum: i128 = 0;
    for i in 0..n {
        let a = ring.get(i)?;
        let b = ring.get((i + 1) % n)?;
        let term = i128::from(a.x.0).checked_mul(i128::from(b.y.0))?.checked_sub(i128::from(b.x.0).checked_mul(i128::from(a.y.0))?)?;
        sum = sum.checked_add(term)?;
    }
    Some(sum)
}

/// A filled area: an outer ring and the holes cut out of it, with straight edges only (arcs have
/// been approximated). This is what booleans and offsets work on and return.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Polygon {
    /// The outer boundary.
    pub outer: Vec<Point>,
    /// Holes inside it.
    pub holes: Vec<Vec<Point>>,
}

impl Polygon {
    /// A polygon without holes.
    pub fn new(outer: Vec<Point>) -> Polygon {
        Polygon { outer, holes: Vec::new() }
    }

    /// A polygon from contours (the first is the outer boundary), arcs approximated within
    /// `tolerance`.
    pub fn from_contours(outer: &Contour, holes: &[Contour], tolerance: Nm) -> Result<Polygon, GeomError> {
        // One point budget for all rings together: many small arc holes must not add up to
        // far more memory than their input suggests.
        let outer = outer.flatten(tolerance)?;
        let mut total = outer.len();
        let mut rings = Vec::with_capacity(holes.len().min(MAX_POINTS));
        for h in holes {
            let ring = h.flatten(tolerance)?;
            total = total.saturating_add(ring.len());
            if total > MAX_POINTS {
                return Err(GeomError::TooManyPoints);
            }
            rings.push(ring);
        }
        Ok(Polygon { outer, holes: rings })
    }

    /// Twice the filled area (outer minus holes), exactly, whatever the rings' directions.
    pub fn area2(&self) -> Option<i128> {
        let mut a = area2(&self.outer)?.checked_abs()?;
        for h in &self.holes {
            a = a.checked_sub(area2(h)?.checked_abs()?)?;
        }
        Some(a)
    }

    /// The filled area in square millimetres (for display and tests).
    pub fn area_mm2(&self) -> f64 {
        self.area2().map_or(0.0, |a| a as f64 / 2.0 / 1e12)
    }

    /// The bounding box of the outer ring.
    pub fn bounds(&self) -> Option<Rect> {
        Rect::bounding(self.outer.iter().copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: i64, y: i64) -> Point {
        Point::new(Nm(x), Nm(y))
    }

    #[test]
    fn rectangle_area_and_direction() {
        let c = Contour::rect(Rect::from_corners(p(0, 0), p(10, 20)));
        let ring = c.flatten(Nm(1)).expect("flatten");
        assert_eq!(ring.len(), 4);
        assert_eq!(area2(&ring), Some(400));
        let mut cw = ring.clone();
        cw.reverse();
        assert_eq!(area2(&cw), Some(-400));
    }

    #[test]
    fn circle_area_is_close_to_pi_r_squared() {
        let r = 1_000_000; // 1 mm
        let poly = Polygon::new(Contour::circle(p(0, 0), Nm(r)).expect("circle").flatten(Nm(100)).expect("flatten"));
        let exact = std::f64::consts::PI; // mm²
        assert!((poly.area_mm2() - exact).abs() / exact < 1e-3, "{}", poly.area_mm2());
    }

    #[test]
    fn rounded_slot_mixes_lines_and_arcs() {
        // 4 mm long, 2 mm wide slot: two half circles joined by straight edges.
        let mm = 1_000_000;
        let c = Contour {
            start: p(0, 0),
            edges: vec![
                Edge::Line { to: p(4 * mm, 0) },
                Edge::Arc { mid: p(5 * mm, mm), to: p(4 * mm, 2 * mm) },
                Edge::Line { to: p(0, 2 * mm) },
                Edge::Arc { mid: p(-mm, mm), to: p(0, 0) },
            ],
        };
        let poly = Polygon::new(c.flatten(Nm(100)).expect("flatten"));
        let exact = 8.0 + std::f64::consts::PI;
        assert!((poly.area_mm2() - exact).abs() < 0.01, "{}", poly.area_mm2());
    }

    #[test]
    fn polygon_with_hole() {
        let outer = Contour::rect(Rect::from_corners(p(0, 0), p(10, 10)));
        let hole = Contour::rect(Rect::from_corners(p(2, 2), p(4, 4)));
        let poly = Polygon::from_contours(&outer, &[hole], Nm(1)).expect("polygon");
        assert_eq!(poly.area2(), Some(200 - 8));
    }

    #[test]
    fn circle_points_must_be_in_the_design_range() {
        let max = circuitcraft_units::MAX_ABS;
        assert_eq!(Contour::circle(p(max, 0), Nm(1)), Err(GeomError::OutOfRange));
        assert_eq!(Contour::circle(p(-max, 0), Nm(1)), Err(GeomError::OutOfRange));
        assert_eq!(Contour::circle(p(0, max + 1), Nm(1)), Err(GeomError::OutOfRange));
        assert!(Contour::circle(p(max - 1, 0), Nm(1)).is_ok());
    }

    #[test]
    fn polygon_point_budget_covers_all_rings() {
        let outer = Contour::rect(Rect::from_corners(p(0, 0), p(1_000_000_000, 1_000_000_000)));
        // A 10 mm hole at 1 nm tolerance would need ~7000 segments, so it is capped at
        // MAX_ARC_SEGMENTS points; enough of them exceed the shared budget although each ring
        // alone is well under it.
        let hole = Contour::circle(p(500_000_000, 500_000_000), Nm(10_000_000)).expect("hole");
        assert_eq!(hole.flatten(Nm(1)).expect("flatten").len(), crate::MAX_ARC_SEGMENTS);
        let n = MAX_POINTS / crate::MAX_ARC_SEGMENTS + 2;
        let holes = vec![hole; n];
        assert_eq!(Polygon::from_contours(&outer, &holes, Nm(1)), Err(GeomError::TooManyPoints));
    }

    #[test]
    fn hostile_contours_are_errors() {
        assert!(Contour { start: p(i64::MAX, 0), edges: vec![] }.flatten(Nm(1)).is_err());
        assert!(Contour::circle(p(i64::MAX, 0), Nm(10)).is_err());
        assert_eq!(Contour::polyline(&[]), None);
    }
}
