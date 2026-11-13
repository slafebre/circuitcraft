//! Polygon booleans, offsets and strokes, on `i_overlay`'s 64-bit integer engine.
//!
//! Everything stays in whole nanometres and integer arithmetic, so the same input gives the same
//! output on every platform (ROADMAP.md, decided 2026-11-13).

use crate::arc::segments_for;
use crate::contour::area2;
use crate::{GeomError, Polygon, check_point};
use circuitcraft_units::{MAX_ABS, Nm, Point};
use i_overlay::core::fill_rule::FillRule as IoFill;
use i_overlay::core::overlay::Overlay;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::i_float::int::angle::Angle;
use i_overlay::i_float::int::point::IntPoint;
use i_overlay::i_shape::int::shape::{IntContour, IntShape, IntShapes};
use i_overlay::mesh::int::arc::ArcOptions;
use i_overlay::mesh::int::outline::offset::IntOutlineOffset;
use i_overlay::mesh::int::stroke::offset::IntStrokeOffset;
use i_overlay::mesh::int::style::{IntLineCap, IntLineJoin, IntOutlineStyle, IntStrokeStyle};

/// A boolean operation between a subject and a clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoolOp {
    /// Area in either.
    Union,
    /// Area in both.
    Intersection,
    /// Area in the subject but not the clip.
    Difference,
    /// Area in exactly one.
    Xor,
}

/// Which areas of overlapping or self-intersecting rings count as filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FillRule {
    /// Filled where the winding number is non-zero: overlapping rings add up.
    #[default]
    NonZero,
    /// Filled where an odd number of rings overlap.
    EvenOdd,
}

fn to_int(p: Point) -> IntPoint<i64> {
    IntPoint::new(p.x.0, p.y.0)
}

fn from_int(p: IntPoint<i64>) -> Point {
    Point::new(Nm(p.x), Nm(p.y))
}

/// A ring for the engine, checked against the design range. Simple rings are turned the given
/// way (outer rings counter-clockwise, holes clockwise). Degenerate rings, with fewer than three
/// distinct points or all points on one line, are dropped. A self-intersecting ring whose signed
/// area adds up to zero (a bow tie) still encloses area and is kept as it is.
fn ring(points: &[Point], ccw: bool) -> Result<Option<IntContour<i64>>, GeomError> {
    for &p in points {
        check_point(p)?;
    }
    if is_degenerate(points)? {
        return Ok(None);
    }
    let a = area2(points).ok_or(GeomError::OutOfRange)?;
    let mut out: IntContour<i64> = points.iter().map(|&p| to_int(p)).collect();
    if a != 0 && (a > 0) != ccw {
        out.reverse();
    }
    Ok(Some(out))
}

/// Whether a ring encloses nothing: fewer than three distinct points, or all on one line.
fn is_degenerate(points: &[Point]) -> Result<bool, GeomError> {
    let Some(&first) = points.first() else { return Ok(true) };
    let Some(&second) = points.iter().find(|&&q| q != first) else { return Ok(true) };
    for &q in points {
        if crate::arc::orientation(first, second, q).ok_or(GeomError::OutOfRange)? != 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

fn shapes(polys: &[Polygon]) -> Result<IntShapes<i64>, GeomError> {
    let mut out = Vec::with_capacity(polys.len());
    for poly in polys {
        let Some(outer) = ring(&poly.outer, true)? else { continue };
        let mut shape: IntShape<i64> = vec![outer];
        for h in &poly.holes {
            if let Some(hole) = ring(h, false)? {
                shape.push(hole);
            }
        }
        out.push(shape);
    }
    Ok(out)
}

/// Engine output back to polygons. Every point is checked: growing or stroking near the edge
/// of the design range can produce points beyond it, which must not pass as valid geometry.
fn polygons(shapes: IntShapes<i64>) -> Result<Vec<Polygon>, GeomError> {
    let mut out = Vec::with_capacity(shapes.len());
    for shape in shapes {
        let mut rings = Vec::with_capacity(shape.len());
        for contour in shape {
            let ring: Vec<Point> = contour.into_iter().map(from_int).collect();
            for &p in &ring {
                check_point(p)?;
            }
            rings.push(ring);
        }
        let mut rings = rings.into_iter();
        if let Some(outer) = rings.next() {
            out.push(Polygon { outer, holes: rings.collect() });
        }
    }
    Ok(out)
}

/// Combines two sets of polygons. Overlaps within each set are resolved with `fill`.
pub fn boolean(subject: &[Polygon], clip: &[Polygon], op: BoolOp, fill: FillRule) -> Result<Vec<Polygon>, GeomError> {
    let rule = match op {
        BoolOp::Union => OverlayRule::Union,
        BoolOp::Intersection => OverlayRule::Intersect,
        BoolOp::Difference => OverlayRule::Difference,
        BoolOp::Xor => OverlayRule::Xor,
    };
    let fill = match fill {
        FillRule::NonZero => IoFill::NonZero,
        FillRule::EvenOdd => IoFill::EvenOdd,
    };
    let (s, c) = (shapes(subject)?, shapes(clip)?);
    polygons(Overlay::from_subj_and_clip(&s, &c).overlay(rule, fill))
}

/// Merges overlapping polygons into clean, non-overlapping ones (union with nothing).
pub fn simplify(polys: &[Polygon]) -> Result<Vec<Polygon>, GeomError> {
    boolean(polys, &[], BoolOp::Union, FillRule::NonZero)
}

/// Round joins whose chords stay within `tolerance` of a circle of radius `r`.
fn round_join(r: Nm, tolerance: Nm) -> IntLineJoin {
    let r = r.0.unsigned_abs() as f64;
    // The angle per segment that segments_for would use, expressed for the engine.
    let n = segments_for(r.max(1.0), std::f64::consts::TAU, tolerance).max(1);
    let step = Angle::from_radians(std::f64::consts::TAU / n as f64).unwrap_or(ArcOptions::MIN_STEP);
    IntLineJoin::Round(ArcOptions { max_step: step, rotation_precision: 32 })
}

fn check_distance(d: Nm) -> Result<(), GeomError> {
    if d.0.unsigned_abs() > MAX_ABS.unsigned_abs() { Err(GeomError::OutOfRange) } else { Ok(()) }
}

/// Grows (positive `distance`) or shrinks (negative) polygons by `distance`, with round corners
/// approximated within `tolerance`. Clearances, solder-mask expansion and pour pull-back are
/// offsets. Overlapping input is merged first.
pub fn offset(polys: &[Polygon], distance: Nm, tolerance: Nm) -> Result<Vec<Polygon>, GeomError> {
    check_distance(distance)?;
    let clean = shapes(&simplify(polys)?)?;
    let style = IntOutlineStyle::new(distance.0).line_join(round_join(distance, tolerance));
    clean.validate_outline(&style).map_err(|_| GeomError::OutOfRange)?;
    let result = clean.outline(&style).map_err(|_| GeomError::OutOfRange)?;
    polygons(result)
}

/// The area covered by drawing `path` with a round pen `width` wide: round ends and round corners,
/// approximated within `tolerance`. A track is a stroke. A path of one point is a round dot.
pub fn stroke(path: &[Point], width: Nm, tolerance: Nm) -> Result<Vec<Polygon>, GeomError> {
    check_distance(width)?;
    if width.0 <= 0 {
        return Err(GeomError::InvalidWidth(width));
    }
    for &p in path {
        check_point(p)?;
    }
    let mut pts: Vec<IntPoint<i64>> = path.iter().map(|&p| to_int(p)).collect();
    pts.dedup();
    let radius = Nm((width.0 + 1) / 2);
    let join = round_join(radius, tolerance);
    let IntLineJoin::Round(arc) = join else { return Err(GeomError::OutOfRange) };
    // A single point: a circle of the pen's radius.
    if pts.len() == 1 {
        let c = pts.first().map(|&p| from_int(p)).ok_or(GeomError::OutOfRange)?;
        let circle = crate::Contour::circle(c, radius)?;
        return Ok(vec![Polygon::new(circle.flatten(tolerance)?)]);
    }
    if pts.is_empty() {
        return Ok(Vec::new());
    }
    let style = IntStrokeStyle::new(width.0).line_join(join).start_cap(IntLineCap::Round(arc)).end_cap(IntLineCap::Round(arc));
    pts.validate_stroke(&style).map_err(|_| GeomError::OutOfRange)?;
    polygons(pts.stroke(&style, false).map_err(|_| GeomError::OutOfRange)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Contour, Rect};

    const MM: i64 = 1_000_000;
    const TOL: Nm = Nm(1_000);

    fn p(x: i64, y: i64) -> Point {
        Point::new(Nm(x), Nm(y))
    }

    fn square(x: i64, y: i64, s: i64) -> Polygon {
        Polygon::new(Contour::rect(Rect::from_corners(p(x, y), p(x + s, y + s))).flatten(TOL).expect("square"))
    }

    fn total_area_mm2(polys: &[Polygon]) -> f64 {
        polys.iter().map(Polygon::area_mm2).sum()
    }

    #[test]
    fn booleans_on_overlapping_squares() {
        // Two 10 mm squares overlapping in a 5 × 5 mm corner.
        let a = [square(0, 0, 10 * MM)];
        let b = [square(5 * MM, 5 * MM, 10 * MM)];
        let area = |op| total_area_mm2(&boolean(&a, &b, op, FillRule::NonZero).expect("boolean"));
        assert!((area(BoolOp::Union) - 175.0).abs() < 1e-9);
        assert!((area(BoolOp::Intersection) - 25.0).abs() < 1e-9);
        assert!((area(BoolOp::Difference) - 75.0).abs() < 1e-9);
        assert!((area(BoolOp::Xor) - 150.0).abs() < 1e-9);
    }

    #[test]
    fn difference_makes_a_hole() {
        let outer = [square(0, 0, 10 * MM)];
        let inner = [square(4 * MM, 4 * MM, 2 * MM)];
        let r = boolean(&outer, &inner, BoolOp::Difference, FillRule::NonZero).expect("boolean");
        assert_eq!(r.len(), 1);
        assert_eq!(r.first().map(|p| p.holes.len()), Some(1));
        assert!((total_area_mm2(&r) - 96.0).abs() < 1e-9);
    }

    #[test]
    fn works_far_from_the_origin() {
        // ±9 m: inside the design range, beyond what a 32-bit engine could hold.
        let far = 9_000 * MM;
        let a = [square(far, -far, 10 * MM)];
        let b = [square(far + 5 * MM, -far + 5 * MM, 10 * MM)];
        let r = boolean(&a, &b, BoolOp::Union, FillRule::NonZero).expect("boolean");
        assert!((total_area_mm2(&r) - 175.0).abs() < 1e-9);
    }

    #[test]
    fn offset_grows_and_shrinks_a_square() {
        let sq = [square(0, 0, 10 * MM)];
        // Grown by 1 mm with round corners: 10² + 4·10·1 + π·1² mm².
        let grown = total_area_mm2(&offset(&sq, Nm(MM), TOL).expect("grow"));
        assert!((grown - (140.0 + std::f64::consts::PI)).abs() < 0.01, "{grown}");
        // Shrunk by 1 mm: an 8 mm square.
        let shrunk = total_area_mm2(&offset(&sq, Nm(-MM), TOL).expect("shrink"));
        assert!((shrunk - 64.0).abs() < 0.01, "{shrunk}");
        // Shrunk past nothing: empty.
        assert!(offset(&sq, Nm(-6 * MM), TOL).expect("vanish").is_empty());
    }

    #[test]
    fn offset_is_deterministic() {
        let sq = [square(0, 0, 10 * MM), square(3 * MM, 3 * MM, 10 * MM)];
        assert_eq!(offset(&sq, Nm(250_000), TOL).expect("a"), offset(&sq, Nm(250_000), TOL).expect("b"));
    }

    #[test]
    fn stroke_is_a_track_with_round_ends() {
        // A 10 mm track, 0.5 mm wide: 10 × 0.5 mm² plus a 0.25 mm radius circle.
        let r = stroke(&[p(0, 0), p(10 * MM, 0)], Nm(500_000), TOL).expect("stroke");
        let expected = 5.0 + std::f64::consts::PI * 0.25 * 0.25;
        assert!((total_area_mm2(&r) - expected).abs() < 0.005, "{}", total_area_mm2(&r));
        let b = r.first().and_then(Polygon::bounds).expect("bounds");
        assert!(b.min.x.0 <= -249_000 && b.max.x.0 >= 10 * MM + 249_000);
    }

    #[test]
    fn stroke_of_one_point_is_a_dot() {
        let r = stroke(&[p(0, 0)], Nm(2 * MM), Nm(100)).expect("dot");
        assert!((total_area_mm2(&r) - std::f64::consts::PI).abs() < 0.001);
    }

    #[test]
    fn self_intersecting_ring_with_zero_net_area_is_kept() {
        // A bow tie: its two lobes wind opposite ways, so the signed area is zero, but both
        // lobes are filled under either fill rule (1 mm² each).
        let bow = [Polygon::new(vec![p(0, 0), p(2 * MM, 2 * MM), p(0, 2 * MM), p(2 * MM, 0)])];
        for fill in [FillRule::EvenOdd, FillRule::NonZero] {
            let r = boolean(&bow, &[], BoolOp::Union, fill).expect("boolean");
            assert!((total_area_mm2(&r) - 2.0).abs() < 1e-9, "{fill:?}: {}", total_area_mm2(&r));
        }
    }

    #[test]
    fn results_outside_the_design_range_are_errors() {
        let max = MAX_ABS;
        // A square touching the edge of the design range, grown past it.
        let edge = [square(max - MM, 0, MM)];
        assert_eq!(offset(&edge, Nm(MM), TOL), Err(GeomError::OutOfRange));
        assert_eq!(stroke(&[p(max - MM, 0), p(max, 0)], Nm(MM), TOL), Err(GeomError::OutOfRange));
        // Shrinking it is fine.
        assert!(offset(&edge, Nm(-1_000), TOL).is_ok());
    }

    #[test]
    fn hostile_input_is_an_error_or_empty() {
        let bad = [Polygon::new(vec![p(i64::MAX, 0), p(0, 1), p(1, 0)])];
        assert_eq!(boolean(&bad, &[], BoolOp::Union, FillRule::NonZero), Err(GeomError::OutOfRange));
        assert_eq!(offset(&[square(0, 0, MM)], Nm(i64::MAX), TOL), Err(GeomError::OutOfRange));
        assert_eq!(stroke(&[p(0, 0), p(1, 1)], Nm(0), TOL), Err(GeomError::InvalidWidth(Nm(0))));
        assert_eq!(stroke(&[], Nm(10), TOL), Ok(Vec::new()));
        // Degenerate rings (too few points, no area) are dropped, not fed to the engine.
        let flat = [Polygon::new(vec![p(0, 0), p(5, 0), p(10, 0)]), Polygon::new(vec![p(0, 0), p(1, 1)])];
        assert_eq!(boolean(&flat, &[], BoolOp::Union, FillRule::NonZero), Ok(Vec::new()));
    }
}
