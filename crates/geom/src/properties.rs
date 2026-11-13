//! Property tests for the geometry operations: facts that hold for every input, checked on
//! thousands of random ones made from a fixed seed, so every run and machine tries the same.

use crate::{Arc, BoolOp, Contour, FillRule, Piece, Polygon, Rect, area2, boolean, chain, offset, ring_within, stroke};
use circuitcraft_units::{Nm, Point};

/// A small random number generator (xorshift64*), seeded per test.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A whole number from `low` to `high`, both included.
    fn between(&mut self, low: i64, high: i64) -> i64 {
        low + (self.next() % (high - low + 1) as u64) as i64
    }
}

fn p(x: i64, y: i64) -> Point {
    Point::new(Nm(x), Nm(y))
}

/// A rectangle with whole-micrometre corners within a few millimetres of the origin.
fn rectangle(random: &mut Random) -> Polygon {
    let (x, y) = (random.between(-5_000, 5_000) * 1_000, random.between(-5_000, 5_000) * 1_000);
    let (w, h) = (random.between(1, 4_000) * 1_000, random.between(1, 4_000) * 1_000);
    Polygon::new(Contour::rect(Rect::from_corners(p(x, y), p(x + w, y + h))).flatten(Nm(1)).expect("flatten"))
}

/// The corners of a polygon that is seen whole from its centre: one corner in each of `n` equal
/// slices of the turn, at a random distance. Such a polygon never crosses itself.
fn star(random: &mut Random, n: usize, center: Point) -> Vec<Point> {
    (0..n)
        .map(|i| {
            let angle = (i as f64 + 0.1 + 0.8 * (random.between(0, 1_000) as f64 / 1_000.0)) * std::f64::consts::TAU / n as f64;
            let radius = random.between(200_000, 5_000_000) as f64;
            p(center.x.0 + (radius * angle.cos()).round() as i64, center.y.0 + (radius * angle.sin()).round() as i64)
        })
        .collect()
}

fn area(polygons: &[Polygon]) -> i128 {
    polygons.iter().map(|polygon| polygon.area2().expect("area")).sum()
}

#[test]
fn the_areas_of_boolean_results_add_up() {
    let mut random = Random(0x9e0_0001);
    for round in 0..2_000 {
        let (a, b) = (rectangle(&mut random), rectangle(&mut random));
        let op = |op| area(&boolean(std::slice::from_ref(&a), std::slice::from_ref(&b), op, FillRule::NonZero).expect("boolean"));
        let (union, both, a_only, either) = (op(BoolOp::Union), op(BoolOp::Intersection), op(BoolOp::Difference), op(BoolOp::Xor));
        let (area_a, area_b) = (area(std::slice::from_ref(&a)), area(std::slice::from_ref(&b)));
        // Rectangles on whole coordinates meet at whole coordinates, so these are exact.
        assert_eq!(union + both, area_a + area_b, "round {round}: union and intersection");
        assert_eq!(a_only, area_a - both, "round {round}: difference");
        assert_eq!(either, union - both, "round {round}: exclusive or");
        assert!(both <= area_a.min(area_b) && union >= area_a.max(area_b), "round {round}");
        // A shape with itself: all of it, all of it, nothing.
        let same = |op| area(&boolean(std::slice::from_ref(&a), std::slice::from_ref(&a), op, FillRule::NonZero).expect("boolean"));
        assert_eq!((same(BoolOp::Union), same(BoolOp::Intersection), same(BoolOp::Difference)), (area_a, area_a, 0), "round {round}");
    }
}

#[test]
fn growing_and_shrinking_change_the_area_as_the_formula_says() {
    let mut random = Random(0x9e0_0002);
    for round in 0..300 {
        let rect = rectangle(&mut random);
        let bounds = rect.bounds().expect("bounds");
        let (w, h) = (bounds.width().0 as f64, bounds.height().0 as f64);
        let d = random.between(1, 500) * 1_000;
        // Grown by d with round corners: the rectangle, a strip along each side and a quarter
        // disc at each corner.
        let grown = area(&offset(std::slice::from_ref(&rect), Nm(d), Nm(100)).expect("offset")) as f64 / 2.0;
        let expected = w * h + 2.0 * d as f64 * (w + h) + std::f64::consts::PI * (d * d) as f64;
        assert!((grown - expected).abs() <= expected * 1e-3, "round {round}: grown to {grown}, expected {expected}");
        // Shrunk by d: a smaller rectangle, or nothing left.
        let shrunk = area(&offset(std::slice::from_ref(&rect), Nm(-d), Nm(100)).expect("offset")) as f64 / 2.0;
        let expected = ((w - 2.0 * d as f64).max(0.0)) * ((h - 2.0 * d as f64).max(0.0));
        assert!((shrunk - expected).abs() <= expected * 1e-6 + 1.0, "round {round}: shrunk to {shrunk}, expected {expected}");
        // A track's copper: its length times its width plus the two round ends.
        let (from, to, width) =
            (p(0, 0), p(random.between(-9_000, 9_000) * 1_000, random.between(-9_000, 9_000) * 1_000), random.between(50, 2_000) * 1_000);
        let length = (to.x.0 as f64).hypot(to.y.0 as f64);
        let copper = area(&stroke(&[from, to], Nm(width), Nm(100)).expect("stroke")) as f64 / 2.0;
        let expected = length * width as f64 + std::f64::consts::PI * (width as f64 / 2.0).powi(2);
        assert!((copper - expected).abs() <= expected * 2e-3, "round {round}: a track of {copper}, expected {expected}");
    }
}

#[test]
fn an_outline_comes_out_the_same_however_its_pieces_are_drawn() {
    let mut random = Random(0x9e0_0003);
    for round in 0..500 {
        let sides = 3 + (random.next() % 12) as usize;
        let center = p(random.between(-1_000_000, 1_000_000), random.between(-1_000_000, 1_000_000));
        let corners = star(&mut random, sides, center);
        let n = corners.len();
        // Every side as a line, except some as an arc through a point pushed off the side's middle.
        let mut pieces: Vec<Piece> = (0..n)
            .map(|i| {
                let (a, b) = (corners[i], corners[(i + 1) % n]);
                match random.between(0, 3) {
                    0 => {
                        let mid = p((a.x.0 + b.x.0) / 2 + (b.y.0 - a.y.0) / 16, (a.y.0 + b.y.0) / 2 - (b.x.0 - a.x.0) / 16);
                        Piece::Arc(Arc::new(a, mid, b))
                    }
                    _ => Piece::Line { from: a, to: b },
                }
            })
            .collect();
        let first = chain(&pieces, Nm(0)).expect("chain");
        assert_eq!(first.len(), 1, "round {round}: one closed outline");
        let outline = &first[0];
        // Canonical: counter-clockwise from the lowest of the leftmost corners, and the ring is
        // the outline's own.
        let start = corners.iter().copied().min_by_key(|c| (c.x, c.y)).expect("corners");
        assert_eq!(outline.contour.start, start, "round {round}");
        assert!(area2(&outline.ring).is_some_and(|a| a > 0 && a == outline.area2), "round {round}");
        // Shuffle the pieces and turn some around: the same outline comes out.
        for _ in 0..3 {
            for i in (1..n).rev() {
                pieces.swap(i, (random.next() % (i as u64 + 1)) as usize);
            }
            for piece in pieces.iter_mut().filter(|_| random.between(0, 1) == 1) {
                *piece = match *piece {
                    Piece::Line { from, to } => Piece::Line { from: to, to: from },
                    Piece::Arc(arc) => Piece::Arc(Arc::new(arc.end, arc.mid, arc.start)),
                };
            }
            let again = chain(&pieces, Nm(0)).expect("chain");
            assert_eq!(again.len(), 1, "round {round}");
            assert_eq!((&again[0].contour, &again[0].ring, again[0].area2), (&outline.contour, &outline.ring, outline.area2), "round {round}");
        }
        // With one piece taken away nothing closes.
        pieces.pop();
        assert!(chain(&pieces, Nm(0)).expect("chain").is_empty(), "round {round}");
    }
}

/// Whether a speck at `q` (a triangle a nanometre across) lies wholly inside `ring`.
fn speck_within(ring: &[Point], q: Point) -> bool {
    let speck = [q, p(q.x.0 + 1, q.y.0), p(q.x.0, q.y.0 + 1)];
    let mut work = usize::MAX;
    ring_within(&speck, ring, &mut work).expect("room to compare")
}

#[test]
fn a_speck_is_inside_a_polygon_when_a_ray_from_it_crosses_an_odd_number_of_sides() {
    let mut random = Random(0x9e0_0004);
    for round in 0..300 {
        // From four corners on, neighbouring corners are less than half a turn apart, so the
        // centre is inside.
        let sides = 4 + (random.next() % 10) as usize;
        let ring = star(&mut random, sides, p(0, 0));
        for _ in 0..200 {
            // Any point will do: one on a side is rare, and the two answers may disagree by
            // rounding only there, so points within a few nanometres of a side are skipped.
            let q = p(random.between(-5_500_000, 5_500_000), random.between(-5_500_000, 5_500_000));
            let (x, y) = (q.x.0 as f64, q.y.0 as f64);
            let mut crossings = 0;
            let mut near_a_side = false;
            for i in 0..ring.len() {
                let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
                let (ax, ay, bx, by) = (a.x.0 as f64, a.y.0 as f64, b.x.0 as f64, b.y.0 as f64);
                let along = ((x - ax) * (bx - ax) + (y - ay) * (by - ay)) / ((bx - ax).powi(2) + (by - ay).powi(2));
                let (nx, ny) = (ax + along.clamp(0.0, 1.0) * (bx - ax), ay + along.clamp(0.0, 1.0) * (by - ay));
                near_a_side |= (x - nx).hypot(y - ny) < 4.0;
                if (ay > y) != (by > y) && x < ax + (y - ay) * (bx - ax) / (by - ay) {
                    crossings += 1;
                }
            }
            if !near_a_side {
                assert_eq!(speck_within(&ring, q), crossings % 2 == 1, "round {round}: {q:?} in {ring:?}");
            }
        }
        // A speck on a corner touches the ring, so it is not inside; one at the centre is.
        assert!(ring.iter().all(|&corner| !speck_within(&ring, corner)), "round {round}");
        assert!(speck_within(&ring, p(0, 0)), "round {round}");
    }
}

#[test]
fn an_arc_is_flattened_to_points_that_stay_on_it() {
    let mut random = Random(0x9e0_0005);
    for round in 0..2_000 {
        let point = |random: &mut Random| p(random.between(-20_000_000, 20_000_000), random.between(-20_000_000, 20_000_000));
        let arc = Arc::new(point(&mut random), point(&mut random), point(&mut random));
        let tolerance = Nm(random.between(1, 50_000));
        let Ok(points) = arc.flatten(tolerance) else { continue };
        // The ends are exact, whatever is between.
        assert_eq!((points.first(), points.last()), (Some(&arc.start), Some(&arc.end)), "round {round}");
        let Some(circle) = arc.circle() else { continue };
        for q in &points {
            let off = ((q.x.0 as f64 - circle.cx).hypot(q.y.0 as f64 - circle.cy) - circle.r).abs();
            // On the circle, to the rounding of a point to whole nanometres and of the centre.
            assert!(off <= 2.0 + circle.r * 1e-9, "round {round}: {q:?} is {off} nm off {arc:?}");
        }
        // No chord strays further from the arc than asked (when the arc didn't need more
        // segments than one arc may have).
        if points.len() <= crate::MAX_ARC_SEGMENTS {
            for pair in points.windows(2) {
                let (mx, my) = ((pair[0].x.0 + pair[1].x.0) as f64 / 2.0, (pair[0].y.0 + pair[1].y.0) as f64 / 2.0);
                let sagitta = circle.r - (mx - circle.cx).hypot(my - circle.cy);
                assert!(sagitta <= tolerance.0 as f64 + 2.0 + circle.r * 1e-9, "round {round}: a chord {sagitta} nm from {arc:?} at {tolerance:?}");
            }
        }
    }
}
