//! Planar geometry on integer coordinates.
//!
//! - [`Rect`]: bounding boxes.
//! - [`Arc`]: circular arcs through three points, and their approximation by segments.
//! - [`Contour`]: closed outlines of straight and arc edges; [`Polygon`]: filled areas with holes.
//! - [`boolean`], [`offset`], [`stroke`]: polygon booleans, clearance offsets and track shapes,
//!   on `i_overlay`'s 64-bit integer engine, deterministic on every platform.
//!
//! All coordinates are [`Nm`]; anything outside the ±10 m design range is an error, never a panic.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod arc;
mod contour;
mod region;

pub use arc::{Arc, Circle};
pub use circuitcraft_units::{Nm, Point, UnitError};
pub use contour::{Contour, Edge, Polygon, area2};
pub use region::{BoolOp, FillRule, boolean, offset, simplify, stroke};
use serde::{Deserialize, Serialize};

/// The most segments one arc is approximated with.
pub const MAX_ARC_SEGMENTS: usize = 4_096;

/// The most points one flattened contour may have; hostile input can't exhaust memory.
pub const MAX_POINTS: usize = 4_000_000;

/// A geometry operation failed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum GeomError {
    /// A coordinate or distance is outside the ±10 m design range.
    #[error("coordinate or distance outside the design range")]
    OutOfRange,
    /// A contour would need more than [`MAX_POINTS`] points.
    #[error("too many points (more than {MAX_POINTS})")]
    TooManyPoints,
    /// A stroke width must be positive.
    #[error("invalid width {0:?}")]
    InvalidWidth(Nm),
}

impl From<UnitError> for GeomError {
    fn from(_: UnitError) -> Self {
        GeomError::OutOfRange
    }
}

/// Checks that a point lies inside the design range.
pub(crate) fn check_point(p: Point) -> Result<(), GeomError> {
    // unsigned_abs: abs() of i64::MIN would overflow.
    let max = circuitcraft_units::MAX_ABS.unsigned_abs();
    if p.x.0.unsigned_abs() > max || p.y.0.unsigned_abs() > max { Err(GeomError::OutOfRange) } else { Ok(()) }
}

/// An axis-aligned rectangle, `min` at the lower left, `max` at the upper right (inclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rect {
    /// Lower-left corner.
    pub min: Point,
    /// Upper-right corner.
    pub max: Point,
}

impl Rect {
    /// The rectangle spanned by two corners in any order.
    pub fn from_corners(a: Point, b: Point) -> Rect {
        Rect { min: Point::new(a.x.min(b.x), a.y.min(b.y)), max: Point::new(a.x.max(b.x), a.y.max(b.y)) }
    }

    /// The smallest rectangle holding every point, or `None` for no points.
    pub fn bounding(points: impl IntoIterator<Item = Point>) -> Option<Rect> {
        let mut it = points.into_iter();
        let first = it.next()?;
        Some(it.fold(Rect { min: first, max: first }, |r, p| r.including(p)))
    }

    /// This rectangle grown to include `p`.
    pub fn including(self, p: Point) -> Rect {
        Rect::from_corners(Point::new(self.min.x.min(p.x), self.min.y.min(p.y)), Point::new(self.max.x.max(p.x), self.max.y.max(p.y)))
    }

    /// The smallest rectangle holding both.
    pub fn union(self, other: Rect) -> Rect {
        self.including(other.min).including(other.max)
    }

    /// Whether `p` lies inside or on the edge.
    pub fn contains(self, p: Point) -> bool {
        (self.min.x..=self.max.x).contains(&p.x) && (self.min.y..=self.max.y).contains(&p.y)
    }

    /// Width, `max.x - min.x`. Coordinates are bounded by the design range, so this cannot overflow
    /// for rectangles built from valid points; an out-of-range rectangle saturates.
    pub fn width(self) -> Nm {
        Nm(self.max.x.0.saturating_sub(self.min.x.0))
    }

    /// Height, `max.y - min.y` (saturating, like [`Rect::width`]).
    pub fn height(self) -> Nm {
        Nm(self.max.y.0.saturating_sub(self.min.y.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: i64, y: i64) -> Point {
        Point::new(Nm(x), Nm(y))
    }

    #[test]
    fn corners_in_any_order() {
        let r = Rect::from_corners(p(10, -5), p(-2, 7));
        assert_eq!(r.min, p(-2, -5));
        assert_eq!(r.max, p(10, 7));
        assert_eq!(r.width(), Nm(12));
        assert_eq!(r.height(), Nm(12));
    }

    #[test]
    fn bounding_and_contains() {
        assert_eq!(Rect::bounding([]), None);
        let r = Rect::bounding([p(1, 1), p(4, -3), p(0, 2)]).expect("points");
        assert_eq!(r, Rect::from_corners(p(0, -3), p(4, 2)));
        assert!(r.contains(p(4, 2)));
        assert!(!r.contains(p(5, 0)));
        assert_eq!(r.union(Rect::from_corners(p(9, 9), p(8, 8))).max, p(9, 9));
    }

    #[test]
    fn i64_min_coordinates_are_rejected_not_a_panic() {
        // abs() of i64::MIN overflows: this panicked before check_point used unsigned_abs.
        assert_eq!(check_point(p(i64::MIN, 0)), Err(GeomError::OutOfRange));
        assert_eq!(check_point(p(0, i64::MIN)), Err(GeomError::OutOfRange));
        assert!(Contour::polyline(&[p(i64::MIN, i64::MIN)]).expect("contour").flatten(Nm(1)).is_err());
    }

    #[test]
    fn extreme_rectangle_saturates() {
        let r = Rect::from_corners(p(i64::MIN, i64::MIN), p(i64::MAX, i64::MAX));
        assert_eq!(r.width(), Nm(i64::MAX));
    }
}
