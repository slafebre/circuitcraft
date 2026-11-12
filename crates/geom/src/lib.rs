//! Planar geometry on integer coordinates.
//!
//! Today this holds bounding boxes. Lines, arcs, polygons with arcs, booleans and offsets arrive
//! with the board model in 0.1 (see ROADMAP.md).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub use circuitcraft_units::{Nm, Point};
use serde::{Deserialize, Serialize};

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
    fn extreme_rectangle_saturates() {
        let r = Rect::from_corners(p(i64::MIN, i64::MIN), p(i64::MAX, i64::MAX));
        assert_eq!(r.width(), Nm(i64::MAX));
    }
}
