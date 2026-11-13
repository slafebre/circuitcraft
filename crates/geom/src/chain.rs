//! Closed outlines assembled from loose lines and arcs.
//!
//! Board outlines are often drawn piece by piece. [`chain`] joins pieces end to end and returns
//! every closed outline they form, each in one canonical form, so the result doesn't depend on
//! the order or the direction the pieces were drawn in.

use crate::arc::orientation;
use crate::{Arc, Contour, Edge, GeomError, MAX_POINTS, Rect, area2, check_point};
use circuitcraft_units::{MAX_ABS, Nm, Point};
use std::collections::BTreeMap;

/// A line or an arc, to be joined to others at its two ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Piece {
    /// A straight line.
    Line {
        /// One end.
        from: Point,
        /// The other end.
        to: Point,
    },
    /// An arc. One whose ends coincide is a full circle, closed on its own.
    Arc(Arc),
}

impl Piece {
    fn ends(&self) -> (Point, Point) {
        match *self {
            Piece::Line { from, to } => (from, to),
            Piece::Arc(arc) => (arc.start, arc.end),
        }
    }
}

/// A closed outline found by [`chain`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loop {
    /// The outline: counter-clockwise, starting at its lowest leftmost corner.
    pub contour: Contour,
    /// The pieces it is made of, as indices into the input, in the order they were joined.
    pub pieces: Vec<usize>,
    /// The outline as points, arcs approximated within [`CHAIN_TOLERANCE`]: enough to compare
    /// outlines by area and to tell which lies inside which.
    pub ring: Vec<Point>,
    /// Twice the area of `ring`; always positive.
    pub area2: i128,
}

/// How closely [`Loop::ring`] follows the arcs of its outline.
pub const CHAIN_TOLERANCE: Nm = Nm(10_000);

/// Joins `pieces` end to end and returns the closed outlines they form, in the order of each
/// outline's first piece.
///
/// Two ends join when they are within `gap` of each other along both axes, and neither is that
/// close to any third end: where three pieces meet, there is no telling which two belong
/// together, so none of them joins there. A joint that isn't exact is put at the lower left of
/// its two ends. Pieces in an open chain, pieces shorter than `gap` and outlines that enclose no
/// area are left out; the caller sees which by the indices no [`Loop`] lists.
///
/// The cost is linear in the number of pieces, however they crowd together, and there can be
/// at most [`MAX_POINTS`] of them.
pub fn chain(pieces: &[Piece], gap: Nm) -> Result<Vec<Loop>, GeomError> {
    if !(0..=MAX_ABS).contains(&gap.0) {
        return Err(GeomError::OutOfRange);
    }
    if pieces.len() > MAX_POINTS {
        return Err(GeomError::TooManyPoints);
    }
    let mut budget = MAX_POINTS;
    let mut loops = Vec::new();
    // The ends that can join, two per piece: end `2 * i` is where piece `i` starts.
    let mut ends: Vec<Option<Point>> = Vec::with_capacity(pieces.len().saturating_mul(2));
    for (i, piece) in pieces.iter().enumerate() {
        let (a, b) = piece.ends();
        check_point(a)?;
        check_point(b)?;
        match *piece {
            Piece::Arc(arc) if a == b => {
                check_point(arc.mid)?;
                let contour = Contour { start: a, edges: vec![Edge::Arc { mid: arc.mid, to: a }] };
                loops.extend(closed(contour, vec![i], &mut budget)?);
                ends.extend([None, None]);
            }
            // Shorter than the gap: its two ends would join each other.
            _ if near(a, b, gap) => ends.extend([None, None]),
            _ => ends.extend([Some(a), Some(b)]),
        }
    }
    let joints = Joints::new(&ends, gap);
    let mut seen = vec![false; pieces.len()];
    for first in 0..pieces.len() {
        if seen.get(first).copied().unwrap_or(true) || ends.get(first * 2).copied().flatten().is_none() {
            continue;
        }
        if let Some(walk) = joints.walk(first, &mut seen) {
            let contour = contour_of(pieces, &walk);
            loops.extend(closed(contour, walk.iter().map(|&(piece, _)| piece).collect(), &mut budget)?);
        }
    }
    loops.sort_by_key(|l| l.pieces.iter().copied().min());
    Ok(loops)
}

/// Whether two points are within `gap` of each other along both axes. Both are in the design
/// range, so the differences cannot overflow.
fn near(a: Point, b: Point, gap: Nm) -> bool {
    (a.x.0 - b.x.0).abs() <= gap.0 && (a.y.0 - b.y.0).abs() <= gap.0
}

/// Which end joins which.
struct Joints {
    /// For each end, the one end it joins, if exactly one other end is near it and that end has
    /// no other end near it either.
    partner: Vec<Option<usize>>,
}

impl Joints {
    fn new(ends: &[Option<Point>], gap: Nm) -> Joints {
        // A grid of cells as wide as the gap: ends that are near each other are in the same cell
        // or in neighbouring ones, and all the ends in one cell are near each other.
        let size = gap.0.max(1);
        let cell = |p: Point| (p.x.0.div_euclid(size), p.y.0.div_euclid(size));
        let mut grid: BTreeMap<(i64, i64), Vec<usize>> = BTreeMap::new();
        for (e, p) in ends.iter().enumerate() {
            if let Some(p) = p {
                grid.entry(cell(*p)).or_default().push(e);
            }
        }
        // The one end near `e`, or `None` when there is none or more than one. Its own cell is
        // searched first and the search stops at the second end found, so an end in a crowded
        // cell costs two comparisons, and a crowded cell is searched in full only from the few
        // ends in its sparse neighbours.
        let only_neighbour = |e: usize| -> Option<usize> {
            let p = ends.get(e).copied().flatten()?;
            let (cx, cy) = cell(p);
            let mut found = None;
            for (dx, dy) in [(0, 0), (-1, -1), (-1, 0), (-1, 1), (0, -1), (0, 1), (1, -1), (1, 0), (1, 1)] {
                for &other in grid.get(&(cx + dx, cy + dy)).map(Vec::as_slice).unwrap_or_default() {
                    if other != e && ends.get(other).copied().flatten().is_some_and(|q| near(p, q, gap)) {
                        if found.is_some() {
                            return None;
                        }
                        found = Some(other);
                    }
                }
            }
            found
        };
        let nearest: Vec<Option<usize>> = (0..ends.len()).map(only_neighbour).collect();
        let partner = nearest.iter().enumerate().map(|(e, &f)| f.filter(|&f| nearest.get(f).copied().flatten() == Some(e))).collect();
        Joints { partner }
    }

    /// Follows the joints from the far end of piece `first`. Returns the pieces of a closed chain
    /// in order, each with whether it runs from its start to its end, or `None` for an open chain.
    /// Every piece it passes is marked as seen, and it stops at one that already is, so all the
    /// walks together take one step per piece.
    fn walk(&self, first: usize, seen: &mut [bool]) -> Option<Vec<(usize, bool)>> {
        let mut pieces = vec![(first, true)];
        mark(seen, first);
        let mut leaving = first * 2 + 1;
        // Each step marks a new piece, so the loop ends after at most one step per piece.
        while let Some(arriving) = self.partner.get(leaving).copied().flatten() {
            let piece = arriving / 2;
            if piece == first {
                // Back where the walk began: closed, as long as it came in by the other end.
                return (arriving == first * 2).then_some(pieces);
            }
            if seen.get(piece).copied().unwrap_or(true) {
                return None;
            }
            mark(seen, piece);
            pieces.push((piece, arriving % 2 == 0));
            leaving = arriving ^ 1;
        }
        None
    }
}

fn mark(seen: &mut [bool], piece: usize) {
    if let Some(s) = seen.get_mut(piece) {
        *s = true;
    }
}

/// The contour through a closed chain of pieces, each joint at the lower left of the two ends
/// that meet there.
fn contour_of(pieces: &[Piece], walk: &[(usize, bool)]) -> Contour {
    // Each piece as it is walked: where it is entered, its arc's middle if it is one, and where
    // it is left.
    let steps: Vec<(Point, Option<Point>, Point)> = walk
        .iter()
        .filter_map(|&(i, forward)| {
            let piece = pieces.get(i)?;
            let (a, b) = piece.ends();
            let mid = match piece {
                Piece::Line { .. } => None,
                Piece::Arc(arc) => Some(arc.mid),
            };
            Some(if forward { (a, mid, b) } else { (b, mid, a) })
        })
        .collect();
    let joint = |k: usize| {
        let left = steps.get(k).map(|s| s.2);
        let entered = steps.get((k + 1) % steps.len().max(1)).map(|s| s.0);
        left.zip(entered).map(|(a, b)| if (a.x, a.y) <= (b.x, b.y) { a } else { b }).unwrap_or(Point::ORIGIN)
    };
    let edges = steps
        .iter()
        .enumerate()
        .map(|(k, &(_, mid, _))| match mid {
            None => Edge::Line { to: joint(k) },
            Some(mid) => Edge::Arc { mid, to: joint(k) },
        })
        .collect();
    Contour { start: joint(steps.len().saturating_sub(1)), edges }
}

/// The loop for a contour whose last edge ends where it starts, in canonical form; `None` when it
/// encloses no area. `budget` is how many ring points the loops of one call may still use.
fn closed(contour: Contour, pieces: Vec<usize>, budget: &mut usize) -> Result<Option<Loop>, GeomError> {
    let ring = contour.flatten(CHAIN_TOLERANCE)?;
    *budget = budget.checked_sub(ring.len()).ok_or(GeomError::TooManyPoints)?;
    let Some(area) = area2(&ring).filter(|a| *a != 0) else {
        return Ok(None);
    };
    let contour = canonical(contour, area < 0);
    // Flattened again, so the ring starts and runs as the canonical contour does.
    let ring = contour.flatten(CHAIN_TOLERANCE)?;
    Ok(Some(Loop { contour, pieces, ring, area2: area.abs() }))
}

/// The same closed outline, turned around if `reverse`, starting at its lowest leftmost corner.
fn canonical(contour: Contour, reverse: bool) -> Contour {
    // The corners in order: corner `k` is where edge `k` starts.
    let mut corners: Vec<Point> = std::iter::once(contour.start).chain(contour.edges.iter().map(Edge::end)).collect();
    corners.pop(); // the last edge ends at the first corner
    let mut edges = contour.edges;
    if reverse {
        // Edge `k` ran from corner `k` to the next; turned around, it ends at corner `k`.
        edges = edges
            .iter()
            .zip(&corners)
            .rev()
            .map(|(edge, &to)| match *edge {
                Edge::Line { .. } => Edge::Line { to },
                Edge::Arc { mid, .. } => Edge::Arc { mid, to },
            })
            .collect();
    }
    // After turning around, the first edge still starts at the first corner; now start at the
    // lowest leftmost corner instead. Edge `k` ends at that corner when the next edge starts there.
    let start = corners.iter().copied().min_by_key(|p| (p.x, p.y)).unwrap_or(contour.start);
    let first = edges.iter().position(|e| e.end() == start).map_or(0, |k| (k + 1) % edges.len().max(1));
    edges.rotate_left(first);
    Contour { start, edges }
}

/// Whether the closed ring `inner` lies wholly inside the closed ring `outer`, touching it
/// nowhere: one of its corners is strictly inside, and none of its edges meets an edge of `outer`.
///
/// `work` is how many comparisons the caller can still afford: each corner of `outer` costs one,
/// and each edge of `outer` that comes near `inner` costs one per edge of `inner`. When it runs
/// out the answer is [`GeomError::TooManyPoints`], so many rings checked against one outline take
/// a bounded time together.
pub fn ring_within(inner: &[Point], outer: &[Point], work: &mut usize) -> Result<bool, GeomError> {
    let (Some(&corner), Some(bounds)) = (inner.first(), Rect::bounding(inner.iter().copied())) else {
        return Ok(false);
    };
    let mut spend = |n: usize| {
        *work = work.checked_sub(n).ok_or(GeomError::TooManyPoints)?;
        Ok::<(), GeomError>(())
    };
    spend(outer.len())?;
    if !ring_contains(outer, corner) {
        return Ok(false);
    }
    for (a, b) in edges(outer) {
        if Rect::from_corners(a, b).overlaps(bounds) {
            spend(inner.len())?;
            if edges(inner).any(|(c, d)| segments_touch(a, b, c, d)) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// The edges of a closed ring, the one from its last point back to its first included.
fn edges(ring: &[Point]) -> impl Iterator<Item = (Point, Point)> + '_ {
    ring.iter().enumerate().filter_map(|(i, &a)| ring.get((i + 1) % ring.len()).map(|&b| (a, b)))
}

/// Whether `v` is between `lo` and `hi`, given in either order, or equal to one of them.
fn between(lo: Nm, v: Nm, hi: Nm) -> bool {
    (lo.min(hi)..=lo.max(hi)).contains(&v)
}

/// Whether the segments from `a` to `b` and from `c` to `d` share a point, their ends included.
/// Segments too far outside the design range to compare count as touching.
fn segments_touch(a: Point, b: Point, c: Point, d: Point) -> bool {
    let sides = (orientation(c, d, a), orientation(c, d, b), orientation(a, b, c), orientation(a, b, d));
    let (Some(a_side), Some(b_side), Some(c_side), Some(d_side)) = sides else { return true };
    // They cross when the ends of each lie on opposite sides of the other.
    if a_side.signum() * b_side.signum() < 0 && c_side.signum() * d_side.signum() < 0 {
        return true;
    }
    // Or an end of one lies on the other: in line with it, and within its extent.
    let on = |side: i128, p: Point, from: Point, to: Point| side == 0 && between(from.x, p.x, to.x) && between(from.y, p.y, to.y);
    on(a_side, a, c, d) || on(b_side, b, c, d) || on(c_side, c, a, b) || on(d_side, d, a, b)
}

/// Whether `p` lies strictly inside the closed `ring` (a point on an edge is not inside).
fn ring_contains(ring: &[Point], p: Point) -> bool {
    let mut inside = false;
    for (i, &a) in ring.iter().enumerate() {
        let Some(&b) = ring.get((i + 1) % ring.len()) else { return false };
        let Some(side) = orientation(a, b, p) else { return false };
        if side == 0 && between(a.x, p.x, b.x) && between(a.y, p.y, b.y) {
            return false;
        }
        // A ray from `p` to the right crosses an edge going up when `p` is on its left, and an
        // edge going down when `p` is on its right.
        if (a.y > p.y) != (b.y > p.y) && (side > 0) == (b.y > a.y) {
            inside = !inside;
        }
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: i64, y: i64) -> Point {
        Point::new(Nm(x), Nm(y))
    }

    fn line(a: Point, b: Point) -> Piece {
        Piece::Line { from: a, to: b }
    }

    /// A 10 by 6 rectangle drawn as four lines, in no order and in mixed directions.
    fn rectangle() -> Vec<Piece> {
        vec![line(p(10, 6), p(10, 0)), line(p(0, 0), p(10, 0)), line(p(0, 6), p(0, 0)), line(p(0, 6), p(10, 6))]
    }

    fn corners(c: &Contour) -> Vec<Point> {
        std::iter::once(c.start).chain(c.edges.iter().map(Edge::end)).collect()
    }

    #[test]
    fn four_lines_in_any_order_make_one_counter_clockwise_rectangle() {
        let loops = chain(&rectangle(), Nm(0)).expect("chain");
        assert_eq!(loops.len(), 1);
        let l = loops.first().expect("a loop");
        assert_eq!(corners(&l.contour), [p(0, 0), p(10, 0), p(10, 6), p(0, 6), p(0, 0)]);
        assert_eq!(l.area2, 120);
        assert_eq!(l.ring, [p(0, 0), p(10, 0), p(10, 6), p(0, 6)]);
        let mut used = l.pieces.clone();
        used.sort_unstable();
        assert_eq!(used, [0, 1, 2, 3]);
        // Every order and direction of the same four lines gives the same contour.
        let mut pieces = rectangle();
        for turn in 0..4 {
            pieces.rotate_left(1);
            if let Some(Piece::Line { from, to }) = pieces.get_mut(turn) {
                std::mem::swap(from, to);
            }
            assert_eq!(chain(&pieces, Nm(0)).expect("chain").first().map(|l| &l.contour), Some(&l.contour));
        }
    }

    #[test]
    fn arcs_keep_their_shape_in_either_direction() {
        // A slot: two lines joined by two half circles, the arcs drawn clockwise.
        let pieces = [
            line(p(0, 0), p(40, 0)),
            Piece::Arc(Arc::new(p(40, 20), p(50, 10), p(40, 0))),
            line(p(40, 20), p(0, 20)),
            Piece::Arc(Arc::new(p(0, 0), p(-10, 10), p(0, 20))),
        ];
        let loops = chain(&pieces, Nm(0)).expect("chain");
        let l = loops.first().expect("a loop");
        assert_eq!(l.contour.start, p(0, 0));
        assert_eq!(
            l.contour.edges,
            [
                Edge::Line { to: p(40, 0) },
                Edge::Arc { mid: p(50, 10), to: p(40, 20) },
                Edge::Line { to: p(0, 20) },
                Edge::Arc { mid: p(-10, 10), to: p(0, 0) }
            ]
        );
        // 40 × 20 plus a circle of radius 10, the arcs flattened to their chords at this size.
        assert!(l.area2 > 2 * 800 && l.area2 <= 2 * (800 + 315), "{}", l.area2);
        // The same slot drawn clockwise is turned around, arcs and all.
        let clockwise = [
            line(p(40, 0), p(0, 0)),
            Piece::Arc(Arc::new(p(0, 0), p(-10, 10), p(0, 20))),
            line(p(0, 20), p(40, 20)),
            Piece::Arc(Arc::new(p(40, 20), p(50, 10), p(40, 0))),
        ];
        assert_eq!(chain(&clockwise, Nm(0)).expect("chain").first().map(|l| (&l.contour, l.area2)), Some((&l.contour, l.area2)));
        // Two half circles make a circle; one full-circle arc is closed on its own.
        let halves = [Piece::Arc(Arc::new(p(-10, 0), p(0, 10), p(10, 0))), Piece::Arc(Arc::new(p(-10, 0), p(0, -10), p(10, 0)))];
        assert_eq!(chain(&halves, Nm(0)).expect("chain").len(), 1);
        let full = [Piece::Arc(Arc::new(p(10, 0), p(-10, 0), p(10, 0)))];
        let loops = chain(&full, Nm(0)).expect("chain");
        assert_eq!(loops.first().map(|l| (l.contour.start, l.pieces.clone())), Some((p(10, 0), vec![0])));
    }

    #[test]
    fn ends_join_across_a_gap_no_wider_than_allowed() {
        // The corner at (10, 0) is drawn 3 apart.
        let mut pieces = rectangle();
        if let Some(Piece::Line { to, .. }) = pieces.get_mut(1) {
            *to = p(10, -3);
        }
        assert!(chain(&pieces, Nm(2)).expect("chain").is_empty(), "too far apart to join");
        assert!(chain(&pieces, Nm(0)).expect("chain").is_empty());
        let loops = chain(&pieces, Nm(3)).expect("chain");
        // The joint is at the lower left of the two ends.
        assert_eq!(loops.first().map(|l| corners(&l.contour)), Some(vec![p(0, 0), p(10, -3), p(10, 6), p(0, 6), p(0, 0)]));
    }

    #[test]
    fn open_chains_branches_and_slivers_are_left_out() {
        // Three sides of a rectangle.
        assert!(chain(rectangle().get(..3).unwrap_or_default(), Nm(0)).expect("chain").is_empty());
        // A fifth line from a corner: three ends meet there, so nothing joins at that corner.
        let mut branched = rectangle();
        branched.push(line(p(10, 6), p(20, 20)));
        assert!(chain(&branched, Nm(0)).expect("chain").is_empty());
        // The same with the fifth line drawn first, so the walk starts from it.
        branched.rotate_right(1);
        assert!(chain(&branched, Nm(0)).expect("chain").is_empty());
        // Within a gap of 3, the corner at (100, 0) to (100, 2) has a third end at (100, 4): it is
        // near (100, 2) and not near (100, 0), and still nothing joins there.
        let third = [
            line(p(0, 0), p(100, 0)),
            line(p(100, 2), p(100, 60)),
            line(p(100, 60), p(0, 60)),
            line(p(0, 60), p(0, 0)),
            line(p(100, 4), p(200, 100)),
        ];
        assert!(chain(&third, Nm(3)).expect("chain").is_empty());
        assert_eq!(chain(third.get(..4).unwrap_or_default(), Nm(3)).expect("chain").len(), 1, "it joins without the third end");
        // A line there and back encloses nothing; neither does a piece shorter than the gap.
        assert!(chain(&[line(p(0, 0), p(10, 0)), line(p(10, 0), p(0, 0))], Nm(0)).expect("chain").is_empty());
        assert!(chain(&[line(p(0, 0), p(0, 0))], Nm(0)).expect("chain").is_empty());
        assert!(chain(&[line(p(0, 0), p(1, 1))], Nm(1)).expect("chain").is_empty());
        // A closed outline next to an open one is still found, and says which pieces it used.
        let mut both = vec![line(p(100, 100), p(200, 100)), line(p(200, 100), p(200, 200))];
        both.extend(rectangle());
        let loops = chain(&both, Nm(0)).expect("chain");
        let mut used = loops.first().map(|l| l.pieces.clone()).unwrap_or_default();
        used.sort_unstable();
        assert_eq!((loops.len(), used), (1, vec![2, 3, 4, 5]));
    }

    #[test]
    fn several_outlines_come_back_in_the_order_of_their_first_piece() {
        let square =
            |x: i64, s: i64| [line(p(x, 0), p(x + s, 0)), line(p(x + s, 0), p(x + s, s)), line(p(x + s, s), p(x, s)), line(p(x, s), p(x, 0))];
        let pieces: Vec<Piece> = square(100, 5).into_iter().chain(square(0, 50)).collect();
        let loops = chain(&pieces, Nm(0)).expect("chain");
        assert_eq!(loops.iter().map(|l| (l.contour.start, l.area2)).collect::<Vec<_>>(), [(p(100, 0), 50), (p(0, 0), 5_000)]);
        // A circle, closed on its own, keeps its place among them.
        let mut pieces = pieces;
        pieces.insert(4, Piece::Arc(Arc::new(p(520, 0), p(480, 0), p(520, 0))));
        let loops = chain(&pieces, Nm(0)).expect("chain");
        assert_eq!(loops.iter().map(|l| l.contour.start).collect::<Vec<_>>(), [p(100, 0), p(520, 0), p(0, 0)]);
    }

    #[test]
    fn the_rings_of_one_call_share_a_point_budget() {
        // A 9 m circle takes about two thousand points; two thousand of them are over the budget.
        let circle = Piece::Arc(Arc::new(p(9_000_000_000, 0), p(-9_000_000_000, 0), p(9_000_000_000, 0)));
        assert_eq!(chain(&[circle; 1_000], Nm(0)).map(|l| l.len()), Ok(1_000));
        assert_eq!(chain(&[circle; 2_000], Nm(0)), Err(GeomError::TooManyPoints));
    }

    #[test]
    fn hostile_pieces_are_errors_or_left_out() {
        assert_eq!(chain(&[line(p(i64::MIN, 0), p(0, 0))], Nm(0)), Err(GeomError::OutOfRange));
        assert_eq!(chain(&[Piece::Arc(Arc::new(p(0, 0), p(i64::MAX, 0), p(0, 0)))], Nm(0)), Err(GeomError::OutOfRange));
        assert_eq!(chain(&rectangle(), Nm(-1)), Err(GeomError::OutOfRange));
        assert_eq!(chain(&rectangle(), Nm(i64::MAX)), Err(GeomError::OutOfRange));
        assert_eq!(chain(&[], Nm(0)), Ok(Vec::new()));
        // A hundred thousand lines from one point, and as many end to end: quick, and no outline.
        let star: Vec<Piece> = (1..=100_000).map(|i| line(p(0, 0), p(i, 1_000))).collect();
        assert!(chain(&star, Nm(10)).expect("chain").is_empty());
        let path: Vec<Piece> = (0..100_000).map(|i| line(p(i * 100, 0), p(i * 100 + 100, 0))).collect();
        assert!(chain(&path, Nm(10)).expect("chain").is_empty());
    }

    #[test]
    fn more_pieces_than_a_contour_has_points_are_refused() {
        // The pieces are out of range, so a call that gets past the count stops at the first one
        // instead of chaining four million of them, which would take most of a gigabyte.
        let pieces = vec![line(p(i64::MAX, 0), p(0, 0)); MAX_POINTS + 1];
        assert_eq!(chain(&pieces, Nm(0)), Err(GeomError::TooManyPoints));
        assert_eq!(chain(pieces.get(..MAX_POINTS).unwrap_or_default(), Nm(0)), Err(GeomError::OutOfRange));
    }

    fn square(x: i64, y: i64, side: i64) -> Vec<Point> {
        vec![p(x, y), p(x + side, y), p(x + side, y + side), p(x, y + side)]
    }

    #[test]
    fn segments_touch_where_they_cross_or_meet() {
        let touch = |a: (i64, i64), b: (i64, i64), c: (i64, i64), d: (i64, i64)| {
            let hit = segments_touch(p(a.0, a.1), p(b.0, b.1), p(c.0, c.1), p(d.0, d.1));
            // The same answer whichever segment comes first and whichever way each runs.
            assert_eq!(hit, segments_touch(p(d.0, d.1), p(c.0, c.1), p(b.0, b.1), p(a.0, a.1)));
            hit
        };
        assert!(touch((0, 0), (10, 10), (0, 10), (10, 0)), "crossing");
        assert!(touch((0, 0), (10, 0), (5, 0), (5, 9)), "an end on the other segment");
        assert!(touch((0, 0), (10, 0), (10, 0), (20, 5)), "end to end");
        assert!(touch((0, 0), (10, 0), (5, 0), (20, 0)), "overlapping in one line");
        assert!(touch((0, 0), (0, 0), (0, 0), (5, 5)), "a point on a segment");
        assert!(!touch((0, 0), (10, 0), (11, 0), (20, 0)), "in one line, apart");
        assert!(!touch((0, 0), (10, 0), (0, 1), (10, 1)), "side by side");
        assert!(!touch((0, 0), (10, 0), (5, 1), (5, 9)), "pointing at it, stopping short");
        assert!(!touch((0, 0), (10, 10), (11, 11), (11, 20)), "past its end");
        assert!(!touch((0, 0), (0, 10), (0, 11), (0, 20)), "in one upright line, apart");
        // Each end in turn is the one that lies on the other segment.
        assert!(segments_touch(p(5, 0), p(5, 9), p(0, 0), p(10, 0)) && segments_touch(p(5, 9), p(5, 0), p(0, 0), p(10, 0)));
        assert!(segments_touch(p(0, 0), p(10, 0), p(5, 0), p(5, 9)) && segments_touch(p(0, 0), p(10, 0), p(5, 9), p(5, 0)));
        assert!(touch((i64::MIN, i64::MIN), (i64::MAX, i64::MAX), (i64::MIN, i64::MAX), (i64::MAX, i64::MIN)), "too large to compare");
    }

    #[test]
    fn a_ring_is_wholly_inside_another_or_not() {
        let outer = square(0, 0, 100);
        let within = |inner: &[Point], outer: &[Point]| ring_within(inner, outer, &mut usize::MAX.clone());
        assert_eq!(within(&square(10, 10, 20), &outer), Ok(true));
        assert_eq!(within(&square(90, 40, 20), &outer), Ok(false), "starts inside and sticks out");
        assert_eq!(within(&square(80, 40, 20), &outer), Ok(false), "touches the edge from inside");
        assert_eq!(within(&square(0, 0, 100), &outer), Ok(false), "the same ring");
        assert_eq!(within(&square(200, 0, 20), &outer), Ok(false), "outside");
        assert_eq!(within(&square(-10, -10, 120), &outer), Ok(false), "around it");
        assert_eq!(within(&outer, &square(-10, -10, 120)), Ok(true));
        assert_eq!(within(&[], &outer), Ok(false));
        assert_eq!(within(&square(10, 10, 20), &[]), Ok(false));
        // Every corner inside a notched outline, with the notch cutting through between two of them.
        let notched = [p(0, 0), p(100, 0), p(100, 100), p(50, 20), p(0, 100)];
        assert_eq!(within(&[p(10, 10), p(90, 10), p(90, 60), p(10, 60)], &notched), Ok(false));
        assert_eq!(within(&[p(10, 5), p(90, 5), p(90, 15), p(10, 15)], &notched), Ok(true));
        // The edge that closes a ring counts like any other: the outer ring's, from its last
        // corner back to its first, and the inner ring's, which the tip of the notch touches.
        assert_eq!(within(&[p(10, 40), p(10, 60), p(-10, 60), p(-10, 40)], &outer), Ok(false));
        assert_eq!(within(&[p(10, 20), p(10, 5), p(90, 5), p(90, 20)], &notched), Ok(false));
    }

    #[test]
    fn comparing_rings_stops_when_the_work_runs_out() {
        let outer = square(0, 0, 100);
        // Four corners of the outer ring; none of its edges comes near the inner ring.
        let mut work = 8;
        assert_eq!(ring_within(&square(10, 10, 20), &outer, &mut work), Ok(true));
        assert_eq!(ring_within(&square(40, 40, 20), &outer, &mut work), Ok(true));
        assert_eq!((work, ring_within(&square(70, 70, 20), &outer, &mut work)), (0, Err(GeomError::TooManyPoints)));
        // Each edge of a diamond comes near a ring at its centre (their bounding boxes overlap),
        // and each then costs the ring's four edges: 4 + 4 × 4.
        let diamond = [p(50, 0), p(100, 50), p(50, 100), p(0, 50)];
        let mut work = 19;
        assert_eq!(ring_within(&square(45, 45, 10), &diamond, &mut work), Err(GeomError::TooManyPoints));
        let mut work = 20;
        assert_eq!((ring_within(&square(45, 45, 10), &diamond, &mut work), work), (Ok(true), 0));
    }

    #[test]
    fn a_point_is_inside_a_ring_or_not() {
        let ring = [p(0, 0), p(10, 0), p(10, 10), p(0, 10)];
        assert!(ring_contains(&ring, p(5, 5)));
        assert!(ring_contains(&ring, p(1, 9)));
        for outside in [p(-1, 5), p(11, 5), p(5, -1), p(5, 11), p(20, 20)] {
            assert!(!ring_contains(&ring, outside), "{outside:?}");
        }
        for on_the_edge in [p(0, 0), p(5, 0), p(10, 5), p(0, 10)] {
            assert!(!ring_contains(&ring, on_the_edge), "{on_the_edge:?}");
        }
        // A notch: the point level with a corner is judged by the edges, not the corner.
        let notched = [p(0, 0), p(10, 0), p(10, 10), p(5, 5), p(0, 10)];
        assert!(ring_contains(&notched, p(2, 5)) && ring_contains(&notched, p(8, 5)));
        assert!(!ring_contains(&notched, p(5, 8)));
        // The same ring clockwise, and rings that are no rings.
        let clockwise: Vec<Point> = ring.iter().rev().copied().collect();
        assert!(ring_contains(&clockwise, p(5, 5)));
        assert!(!ring_contains(&[], p(0, 0)) && !ring_contains(&[p(0, 0)], p(0, 0)));
    }
}
