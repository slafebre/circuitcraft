//! Circular arcs given by three points, and their approximation by line segments.

use crate::{GeomError, MAX_ARC_SEGMENTS, check_point};
use circuitcraft_units::{Nm, Point};
use serde::{Deserialize, Serialize};
use std::f64::consts::{PI, TAU};

/// A circular arc from `start` through `mid` to `end`.
///
/// Three points keep both endpoints exact, which matters for connectivity: a track ending on an
/// arc's endpoint touches it exactly. When `start == end` (and `mid` differs) the arc is a full
/// circle whose diameter runs from `start` to `mid`. Three collinear points describe a straight
/// line from `start` to `end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Arc {
    /// Where the arc begins.
    pub start: Point,
    /// Any point on the arc strictly between the ends; it decides the side and direction.
    pub mid: Point,
    /// Where the arc ends.
    pub end: Point,
}

/// The circle an [`Arc`] lies on, and how far the arc turns, in floating point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Circle {
    /// Centre, in nanometres.
    pub cx: f64,
    /// Centre, in nanometres.
    pub cy: f64,
    /// Radius, in nanometres.
    pub r: f64,
    /// Angle of `start` seen from the centre, in radians.
    pub start_angle: f64,
    /// Signed sweep in radians: positive counter-clockwise (y up), negative clockwise.
    pub sweep: f64,
}

fn xy(p: Point) -> (f64, f64) {
    (p.x.0 as f64, p.y.0 as f64)
}

impl Arc {
    /// An arc through three points.
    pub const fn new(start: Point, mid: Point, end: Point) -> Arc {
        Arc { start, mid, end }
    }

    /// The circle and sweep, or `None` when the three points are collinear or coincide (a
    /// straight line, or nothing).
    pub fn circle(&self) -> Option<Circle> {
        let (sx, sy) = xy(self.start);
        let (mx, my) = xy(self.mid);
        let (ex, ey) = xy(self.end);
        if self.start == self.end {
            if self.mid == self.start {
                return None;
            }
            let (cx, cy) = ((sx + mx) / 2.0, (sy + my) / 2.0);
            let r = (sx - cx).hypot(sy - cy);
            return Some(Circle { cx, cy, r, start_angle: (sy - cy).atan2(sx - cx), sweep: TAU });
        }
        // Exact orientation test in integers: zero means collinear, positive a left turn
        // start → mid → end, which makes the arc run counter-clockwise.
        let cross = orientation(self.start, self.mid, self.end)?;
        if cross == 0 {
            return None;
        }
        let ccw = cross > 0;
        // Circumcentre relative to `start`, for precision far from the origin.
        let (bx, by) = (mx - sx, my - sy);
        let (qx, qy) = (ex - sx, ey - sy);
        let d = 2.0 * (bx * qy - by * qx);
        let (b2, q2) = (bx * bx + by * by, qx * qx + qy * qy);
        let cx = sx + (qy * b2 - by * q2) / d;
        let cy = sy + (bx * q2 - qx * b2) / d;
        let r = (sx - cx).hypot(sy - cy);
        if !(cx.is_finite() && cy.is_finite() && r.is_finite()) || r == 0.0 {
            return None;
        }
        let a0 = (sy - cy).atan2(sx - cx);
        let a1 = (ey - cy).atan2(ex - cx);
        let mut sweep = a1 - a0;
        if ccw {
            while sweep <= 0.0 {
                sweep += TAU;
            }
        } else {
            while sweep >= 0.0 {
                sweep -= TAU;
            }
        }
        Some(Circle { cx, cy, r, start_angle: a0, sweep })
    }

    /// Points along the arc, `start` and `end` included and exact, no two consecutive points
    /// further than `tolerance` (at least 1 nm) from the true arc. At most [`MAX_ARC_SEGMENTS`]
    /// segments are used, so a tiny tolerance on a huge arc is coarser than asked rather than
    /// unbounded.
    pub fn flatten(&self, tolerance: Nm) -> Result<Vec<Point>, GeomError> {
        for p in [self.start, self.mid, self.end] {
            check_point(p)?;
        }
        let Some(c) = self.circle() else {
            return Ok(if self.start == self.end { vec![self.start] } else { vec![self.start, self.end] });
        };
        let segments = segments_for(c.r, c.sweep.abs(), tolerance);
        let mut out = Vec::with_capacity(segments + 1);
        out.push(self.start);
        for i in 1..segments {
            let a = c.start_angle + c.sweep * (i as f64 / segments as f64);
            let p = Point::new(Nm::from_f64_nm(c.cx + c.r * a.cos())?, Nm::from_f64_nm(c.cy + c.r * a.sin())?);
            if out.last() != Some(&p) {
                out.push(p);
            }
        }
        if out.last() != Some(&self.end) {
            out.push(self.end);
        }
        Ok(out)
    }
}

/// `(b - a) × (c - a)` computed exactly, or `None` if it overflows (only possible outside the
/// design range).
pub(crate) fn orientation(a: Point, b: Point, c: Point) -> Option<i128> {
    let d = |p: Point, q: Point| (i128::from(p.x.0) - i128::from(q.x.0), i128::from(p.y.0) - i128::from(q.y.0));
    let (bx, by) = d(b, a);
    let (cx, cy) = d(c, a);
    bx.checked_mul(cy)?.checked_sub(by.checked_mul(cx)?)
}

/// How many straight segments approximate `sweep` radians of a circle of radius `r` within
/// `tolerance`: the sagitta of each chord is at most the tolerance.
pub(crate) fn segments_for(r: f64, sweep: f64, tolerance: Nm) -> usize {
    let tol = (tolerance.0.max(1)) as f64;
    let step = if tol >= r { PI / 2.0 } else { 2.0 * (1.0 - tol / r).acos() };
    let n = if step.is_finite() && step > 0.0 { (sweep / step).ceil() } else { MAX_ARC_SEGMENTS as f64 };
    // A full circle needs at least a triangle; any arc at least one segment.
    let min = if sweep >= TAU - 1e-9 { 3.0 } else { 1.0 };
    n.clamp(min, MAX_ARC_SEGMENTS as f64) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: i64, y: i64) -> Point {
        Point::new(Nm(x), Nm(y))
    }

    #[test]
    fn quarter_circle() {
        // Radius 1 mm, counter-clockwise from (1, 0) to (0, 1) through 45°.
        let r = 1_000_000;
        let m = (r as f64 / 2f64.sqrt()).round() as i64;
        let arc = Arc::new(p(r, 0), p(m, m), p(0, r));
        let c = arc.circle().expect("circle");
        assert!(c.cx.abs() < 1.0 && c.cy.abs() < 1.0 && (c.r - r as f64).abs() < 1.0);
        // The midpoint is rounded to a whole nanometre, which moves the centre by under 1 nm.
        assert!((c.sweep - PI / 2.0).abs() < 1e-5, "{c:?}");
        let pts = arc.flatten(Nm(1_000)).expect("flatten");
        assert_eq!(pts.first(), Some(&p(r, 0)));
        assert_eq!(pts.last(), Some(&p(0, r)));
        // 1 µm tolerance on a 1 mm radius: step = 2·acos(0.999) ≈ 5.1°, so 18 segments.
        assert_eq!(pts.len(), 19);
        for q in &pts {
            let d = (q.x.0 as f64).hypot(q.y.0 as f64);
            assert!((d - r as f64).abs() <= 1.0, "{q:?} off the circle by {}", d - r as f64);
        }
    }

    #[test]
    fn clockwise_arc_has_negative_sweep() {
        let r = 1_000_000;
        let m = (r as f64 / 2f64.sqrt()).round() as i64;
        let arc = Arc::new(p(0, r), p(m, m), p(r, 0));
        assert!(arc.circle().expect("circle").sweep < 0.0);
    }

    #[test]
    fn full_circle_from_start_equals_end() {
        let arc = Arc::new(p(1_000, 0), p(-1_000, 0), p(1_000, 0));
        let c = arc.circle().expect("circle");
        assert!((c.r - 1_000.0).abs() < 1e-9 && (c.sweep - TAU).abs() < 1e-12);
        let pts = arc.flatten(Nm(10)).expect("flatten");
        assert_eq!(pts.first(), pts.last());
        assert!(pts.len() >= 4);
    }

    #[test]
    fn collinear_points_are_a_line() {
        let arc = Arc::new(p(0, 0), p(5, 5), p(10, 10));
        assert!(arc.circle().is_none());
        assert_eq!(arc.flatten(Nm(1)).expect("flatten"), vec![p(0, 0), p(10, 10)]);
    }

    #[test]
    fn hostile_arcs_are_bounded_or_errors() {
        // Out of the design range.
        assert!(Arc::new(p(i64::MAX, 0), p(0, 1), p(0, 0)).flatten(Nm(1)).is_err());
        // Huge arc, tiny tolerance: capped, not unbounded.
        let big = 9_000_000_000;
        let pts = Arc::new(p(big, 0), p(0, big), p(-big, 0)).flatten(Nm(0)).expect("flatten");
        assert!(pts.len() <= MAX_ARC_SEGMENTS + 1);
        // All three points equal.
        assert_eq!(Arc::new(p(3, 3), p(3, 3), p(3, 3)).flatten(Nm(1)).expect("flatten"), vec![p(3, 3)]);
    }
}
