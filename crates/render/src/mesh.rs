//! Triangles for drawing a display list on a GPU.
//!
//! A GPU draws triangles. An opaque group's shapes are cut into triangles one by one: a line
//! into a rectangle for each segment and a fan of triangles at each point (its round ends and
//! corners), a disc into a fan, an area with its holes by a triangulator. Where they overlap
//! nothing shows, since the group is opaque. A see-through group (solder mask, paste, the
//! highlight) must not grow stronger where its shapes overlap, so its shapes are first turned
//! into areas, merged into areas that don't overlap, and then cut into triangles; such groups
//! hold few shapes. The work follows the size of the board, not of the picture, so a front end
//! does it when the board or the view changes and keeps the triangles while it pans and zooms.

use crate::Rgba;
use crate::display::{DisplayList, Group, GroupKind, Shape};
use circuitcraft_geom::{Contour, Polygon, area2, stroke};
use circuitcraft_model::LayerId;
use circuitcraft_units::{Nm, Point};
use i_triangle::i_overlay::i_float::int::point::IntPoint;
use i_triangle::int::triangulatable::IntTriangulatable;

/// One group of a display list as triangles: three indices into `points` for each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMesh {
    /// The layer, as in the display list.
    pub layer: Option<LayerId>,
    /// What the group is.
    pub kind: GroupKind,
    /// The colour, with the group's opacity.
    pub color: Rgba,
    /// The corners of the triangles, in board coordinates.
    pub points: Vec<Point>,
    /// Three indices into `points` for each triangle.
    pub indices: Vec<u32>,
}

/// The display list as triangles, a mesh for each group that covers any area, in drawing order.
/// `tolerance` is how far the edge of a round end or a disc may stray from the true circle.
pub fn meshes(list: &DisplayList, tolerance: Nm) -> Vec<GroupMesh> {
    list.groups.iter().filter_map(|group| mesh(group, Nm(tolerance.0.max(1)))).collect()
}

fn mesh(group: &Group, tolerance: Nm) -> Option<GroupMesh> {
    let [.., alpha] = group.color.0;
    let (points, indices) = if alpha == 255 { opaque(&group.shapes, tolerance) } else { merged(&group.shapes, tolerance) };
    if indices.is_empty() {
        return None;
    }
    Some(GroupMesh { layer: group.layer, kind: group.kind, color: group.color, points, indices })
}

/// Triangles a mesh is being built from.
#[derive(Default)]
struct Builder {
    points: Vec<Point>,
    indices: Vec<u32>,
}

impl Builder {
    /// Adds a point and returns its index, or `None` past what a 32-bit index can address.
    fn point(&mut self, p: Point) -> Option<u32> {
        let i = u32::try_from(self.points.len()).ok()?;
        self.points.push(p);
        Some(i)
    }

    fn triangle(&mut self, a: u32, b: u32, c: u32) {
        self.indices.extend([a, b, c]);
    }

    /// A disc as a fan of triangles around its centre.
    fn disc(&mut self, center: Point, radius: f64, tolerance: Nm) -> Option<()> {
        if radius.is_nan() || radius <= 0.0 {
            return Some(());
        }
        let n = segments(radius, tolerance);
        let rim: Vec<Point> = (0..n)
            .map(|k| {
                let angle = std::f64::consts::TAU * f64::from(k) / f64::from(n);
                offset(center, radius * angle.cos(), radius * angle.sin())
            })
            .collect::<Option<_>>()?;
        let middle = self.point(center)?;
        let first = self.point(*rim.first()?)?;
        for &p in rim.iter().skip(1) {
            self.point(p)?;
        }
        for k in 0..n {
            self.triangle(middle, first + k, first + (k + 1) % n);
        }
        Some(())
    }

    /// A segment `width` wide without its ends, as a rectangle of two triangles. Returns the
    /// indices of its corners, the start's two first (`a` + normal, then the end's two, then `a` −
    /// normal), or `None` inside for a segment of no length, which draws nothing.
    fn segment(&mut self, a: Point, b: Point, width: f64) -> Option<Option<[u32; 4]>> {
        let (dx, dy) = ((b.x.0 as f64 - a.x.0 as f64), (b.y.0 as f64 - a.y.0 as f64));
        let length = dx.hypot(dy);
        if length.is_nan() || length <= 0.0 {
            return Some(None);
        }
        let (nx, ny) = (-dy / length * width / 2.0, dx / length * width / 2.0);
        let [c0, c1, c2, c3] = [offset(a, nx, ny)?, offset(b, nx, ny)?, offset(b, -nx, -ny)?, offset(a, -nx, -ny)?];
        let [p0, p1, p2, p3] = [self.point(c0)?, self.point(c1)?, self.point(c2)?, self.point(c3)?];
        self.triangle(p0, p1, p2);
        self.triangle(p0, p2, p3);
        Some(Some([p0, p1, p2, p3]))
    }
}

/// How many sides the polygon standing in for a circle of `radius` has, so its edges stay within
/// `tolerance` of the circle: from 6 to 128.
fn segments(radius: f64, tolerance: Nm) -> u32 {
    let tolerance = tolerance.0.max(1) as f64;
    // An edge spanning angle θ strays r(1 − cos θ/2) ≈ rθ²/8 from the circle.
    let step = (8.0 * tolerance / radius).sqrt();
    let n = if step.is_finite() && step > 0.0 { (std::f64::consts::TAU / step).ceil() } else { 128.0 };
    n.clamp(6.0, 128.0) as u32
}

/// `p` moved by `(dx, dy)` nanometres, rounded; `None` beyond what a coordinate holds.
fn offset(p: Point, dx: f64, dy: f64) -> Option<Point> {
    let x = (p.x.0 as f64 + dx).round();
    let y = (p.y.0 as f64 + dy).round();
    let fits = |v: f64| v.abs() < 4.0e18;
    (fits(x) && fits(y)).then(|| Point::new(Nm(x as i64), Nm(y as i64)))
}

/// An opaque group's shapes, each cut into triangles on its own. Past what 32-bit indices can
/// address, the rest is left out.
fn opaque(shapes: &[Shape], tolerance: Nm) -> (Vec<Point>, Vec<u32>) {
    let mut b = Builder::default();
    for shape in shapes {
        let done = match shape {
            Shape::Stroke { points, width } => stroke_triangles(&mut b, points, width.0 as f64, tolerance),
            Shape::Disc { center, radius } => b.disc(*center, radius.0 as f64, tolerance),
            Shape::Area(polygon) => {
                let triangulation = vec![rings(polygon)].triangulate().into_triangulation::<u32>();
                u32::try_from(b.points.len()).ok().and_then(|base| {
                    let indices = triangulation.indices.iter().map(|&i| base.checked_add(i)).collect::<Option<Vec<u32>>>()?;
                    b.points.extend(triangulation.points.iter().map(|p| Point::new(Nm(p.x), Nm(p.y))));
                    b.indices.extend(indices);
                    Some(())
                })
            }
        };
        if done.is_none() {
            break;
        }
    }
    (b.points, b.indices)
}

/// A line `width` wide through `points`: a rectangle for each segment, and a disc at each end and
/// at each corner where the line turns enough for the rectangles to leave a gap wider than the
/// tolerance. Where it turns less (a flattened arc turns a little at each of its many points), the
/// thin gap between the rectangles is filled with the two triangles that join their corners.
fn stroke_triangles(b: &mut Builder, points: &[Point], width: f64, tolerance: Nm) -> Option<()> {
    if width.is_nan() || width <= 0.0 {
        return Some(());
    }
    // The corners of the last rectangle drawn.
    let mut last: Option<[u32; 4]> = None;
    for (k, pair) in points.windows(2).enumerate() {
        if let [p, q] = pair
            && let Some(corners) = b.segment(*p, *q, width)?
        {
            // A joint the line passes straight through: fill the wedge between the rectangles.
            if let (Some([_, e1, e2, _]), Some(&a)) = (last, k.checked_sub(1).and_then(|i| points.get(i)))
                && a != *p
                && straight(a, *p, *q, width, tolerance)
            {
                let [s0, _, _, s3] = corners;
                b.triangle(e1, s0, s3);
                b.triangle(e1, s3, e2);
            }
            last = Some(corners);
        }
    }
    let closed = points.len() > 2 && points.first() == points.last();
    for (k, &p) in points.iter().enumerate() {
        let before = k.checked_sub(1).and_then(|i| points.get(i));
        let after = points.get(k + 1);
        // A point repeated, or the end of a closed line, which its start already rounds.
        if before == Some(&p) || (closed && k + 1 == points.len()) {
            continue;
        }
        if let (Some(&a), Some(&c)) = (before, after)
            && straight(a, p, c, width, tolerance)
        {
            continue;
        }
        b.disc(p, width / 2.0, tolerance)?;
    }
    Some(())
}

/// Whether a line `width` wide turns so little at `p` that the two rectangles meeting there leave
/// a gap no wider than `tolerance` on the outside of the turn: half the width times the angle.
fn straight(a: Point, p: Point, c: Point, width: f64, tolerance: Nm) -> bool {
    let (ux, uy) = (p.x.0 as f64 - a.x.0 as f64, p.y.0 as f64 - a.y.0 as f64);
    let (vx, vy) = (c.x.0 as f64 - p.x.0 as f64, c.y.0 as f64 - p.y.0 as f64);
    let dot = ux * vx + uy * vy;
    if dot.is_nan() || dot <= 0.0 {
        return false;
    }
    let angle = (ux * vy - uy * vx).abs().atan2(dot);
    width / 2.0 * angle <= tolerance.0.max(1) as f64
}

/// A see-through group's shapes as areas, merged so none overlaps another, then cut into
/// triangles.
fn merged(shapes: &[Shape], tolerance: Nm) -> (Vec<Point>, Vec<u32>) {
    let mut all: Vec<Vec<Vec<IntPoint<i64>>>> = Vec::with_capacity(shapes.len());
    for shape in shapes {
        for polygon in areas(shape, tolerance) {
            all.push(rings(&polygon));
        }
    }
    // Triangulating resolves overlaps with the non-zero rule: the shapes are merged, and each
    // area's holes stay holes because they run the other way round.
    let triangulation = all.triangulate().into_triangulation::<u32>();
    (triangulation.points.into_iter().map(|p| Point::new(Nm(p.x), Nm(p.y))).collect(), triangulation.indices)
}

/// The areas a shape covers.
fn areas(shape: &Shape, tolerance: Nm) -> Vec<Polygon> {
    match shape {
        Shape::Stroke { points, width } => stroke(points, *width, tolerance).unwrap_or_default(),
        Shape::Disc { center, radius } => Contour::circle(*center, *radius)
            .and_then(|c| c.flatten(tolerance))
            .ok()
            .filter(|ring| ring.len() >= 3)
            .map(|ring| vec![Polygon::new(ring)])
            .unwrap_or_default(),
        Shape::Area(polygon) => vec![polygon.clone()],
    }
}

/// A polygon's rings for the triangulator: the outer ring counter-clockwise, its holes clockwise,
/// whichever way they ran.
fn rings(polygon: &Polygon) -> Vec<Vec<IntPoint<i64>>> {
    let oriented = |ring: &[Point], counter_clockwise: bool| {
        let mut out: Vec<IntPoint<i64>> = ring.iter().map(|p| IntPoint::new(p.x.0, p.y.0)).collect();
        if area2(ring).is_some_and(|a| (a > 0) != counter_clockwise) {
            out.reverse();
        }
        out
    };
    std::iter::once(oriented(&polygon.outer, true)).chain(polygon.holes.iter().map(|h| oriented(h, false))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use circuitcraft_geom::Rect;

    const MM: i64 = 1_000_000;

    fn p(x: i64, y: i64) -> Point {
        Point::new(Nm(x), Nm(y))
    }

    /// A see-through group of these shapes.
    fn group(shapes: Vec<Shape>) -> DisplayList {
        coloured(shapes, Rgba([1, 2, 3, 128]))
    }

    fn coloured(shapes: Vec<Shape>, color: Rgba) -> DisplayList {
        let group = Group { layer: None, name: "g".to_owned(), kind: GroupKind::Layer, color, shapes };
        DisplayList { groups: vec![group], bounds: None }
    }

    /// The same shapes in an opaque group.
    fn solid(shapes: Vec<Shape>) -> DisplayList {
        coloured(shapes, Rgba([1, 2, 3, 255]))
    }

    /// Twice the area the triangles cover, exactly.
    fn area(mesh: &GroupMesh) -> i128 {
        mesh.indices
            .chunks(3)
            .filter_map(|t| {
                let corner = |i: usize| t.get(i).and_then(|&k| mesh.points.get(k as usize)).copied();
                Some(area2(&[corner(0)?, corner(1)?, corner(2)?])?.abs())
            })
            .sum()
    }

    fn square(x: i64, y: i64, side: i64) -> Vec<Point> {
        vec![p(x, y), p(x + side, y), p(x + side, y + side), p(x, y + side)]
    }

    #[test]
    fn an_area_with_a_hole_keeps_its_hole_whichever_way_its_rings_run() {
        // A 10 mm square with a 4 mm hole: 84 mm². Both rings drawn the same way round.
        let polygon = Polygon { outer: square(0, 0, 10 * MM), holes: vec![square(3 * MM, 3 * MM, 4 * MM)] };
        let meshes = meshes(&group(vec![Shape::Area(polygon)]), Nm(1_000));
        let mesh = meshes.first().expect("a mesh");
        assert_eq!(area(mesh), i128::from(2 * 84 * MM * MM));
        assert_eq!((mesh.color, mesh.kind), (Rgba([1, 2, 3, 128]), GroupKind::Layer));
    }

    #[test]
    fn overlapping_shapes_are_merged_so_the_opacity_does_not_add_up() {
        // Two 10 mm squares overlapping by half: 150 mm², not 200.
        let shapes = vec![Shape::Area(Polygon::new(square(0, 0, 10 * MM))), Shape::Area(Polygon::new(square(5 * MM, 0, 10 * MM)))];
        let meshes = meshes(&group(shapes), Nm(1_000));
        assert_eq!(meshes.first().map(area), Some(i128::from(2 * 150 * MM * MM)));
    }

    #[test]
    fn lines_and_discs_become_their_areas() {
        // A 10 mm line 1 mm wide: a 10 by 1 mm rectangle and two half discs of 0.5 mm.
        let line = Shape::Stroke { points: vec![p(0, 0), p(10 * MM, 0)], width: Nm(MM) };
        let mesh = meshes(&group(vec![line]), Nm(100)).into_iter().next().expect("a mesh");
        let exact = 10.0 + std::f64::consts::PI * 0.25;
        let found = area(&mesh) as f64 / 2.0 / 1e12;
        assert!(found < exact && exact - found < 0.01, "{found} against {exact}");
        let bounds = Rect::bounding(mesh.points.iter().copied()).expect("points");
        // The round ends' corners lie on the circle, within the tolerance of its far points.
        assert!((bounds.min.x.0 + MM / 2).abs() <= 100 && (bounds.max.x.0 - 10 * MM - MM / 2).abs() <= 100, "{bounds:?}");
        assert_eq!(bounds.max.y.0, MM / 2);
        let disc = Shape::Disc { center: p(0, 0), radius: Nm(MM) };
        let mesh = meshes(&group(vec![disc]), Nm(100)).into_iter().next().expect("a mesh");
        let found = area(&mesh) as f64 / 2.0 / 1e12;
        assert!((found - std::f64::consts::PI).abs() < 0.01, "{found}");
    }

    #[test]
    fn an_opaque_group_cuts_each_shape_on_its_own() {
        // The same two overlapping squares, opaque: each is cut into triangles, 200 mm² in all.
        let shapes = vec![Shape::Area(Polygon::new(square(0, 0, 10 * MM))), Shape::Area(Polygon::new(square(5 * MM, 0, 10 * MM)))];
        assert_eq!(meshes(&solid(shapes), Nm(1_000)).first().map(area), Some(i128::from(2 * 200 * MM * MM)));
        // A hole stays a hole.
        let polygon = Polygon { outer: square(0, 0, 10 * MM), holes: vec![square(3 * MM, 3 * MM, 4 * MM)] };
        assert_eq!(meshes(&solid(vec![Shape::Area(polygon)]), Nm(1_000)).first().map(area), Some(i128::from(2 * 84 * MM * MM)));
        // A line: its rectangles and the fans at its points reach as far as the round pen.
        let line = Shape::Stroke { points: vec![p(0, 0), p(10 * MM, 0), p(10 * MM, 10 * MM)], width: Nm(MM) };
        let mesh = meshes(&solid(vec![line]), Nm(100)).into_iter().next().expect("a mesh");
        let bounds = Rect::bounding(mesh.points.iter().copied()).expect("points");
        assert_eq!(bounds, Rect::from_corners(p(-MM / 2, -MM / 2), p(10 * MM + MM / 2, 10 * MM + MM / 2)));
        assert!(mesh.indices.len().is_multiple_of(3) && mesh.indices.iter().all(|&i| (i as usize) < mesh.points.len()));
        // A disc is a fan of triangles: within the tolerance of a circle's area.
        let disc = Shape::Disc { center: p(0, 0), radius: Nm(MM) };
        let found = meshes(&solid(vec![disc]), Nm(100)).first().map(area).unwrap_or(0) as f64 / 2.0 / 1e12;
        assert!(found < std::f64::consts::PI && std::f64::consts::PI - found < 0.01, "{found}");
    }

    #[test]
    fn a_line_is_rounded_only_where_it_turns() {
        // A gentle arc of many points needs no discs between its ends; a right angle needs one.
        let arc: Vec<Point> = (0..=100).map(|i| p(i * MM / 10, i * i * 10)).collect();
        let gentle = meshes(&solid(vec![Shape::Stroke { points: arc.clone(), width: Nm(MM / 4) }]), Nm(2_000)).into_iter().next().expect("mesh");
        let corner = meshes(&solid(vec![Shape::Stroke { points: vec![p(0, 0), p(MM, 0), p(MM, MM)], width: Nm(MM / 4) }]), Nm(2_000))
            .into_iter()
            .next()
            .expect("mesh");
        let fan = segments(MM as f64 / 8.0, Nm(2_000)) as usize + 1;
        assert_eq!(gentle.points.len(), 100 * 4 + 2 * fan, "a rectangle per segment and the two round ends");
        assert_eq!(corner.points.len(), 2 * 4 + 3 * fan, "both ends and the corner");
        assert!(!straight(p(0, 0), p(MM, 0), p(0, 0), 1.0, Nm(1)), "a line turning back is no straight line");
        // A closed line is rounded once where it starts and ends.
        let square = vec![p(0, 0), p(MM, 0), p(MM, MM), p(0, MM), p(0, 0)];
        let closed = meshes(&solid(vec![Shape::Stroke { points: square, width: Nm(MM / 4) }]), Nm(2_000)).into_iter().next().expect("mesh");
        assert_eq!(closed.points.len(), 4 * 4 + 4 * fan);
    }

    /// Whether a triangle of the mesh covers `q`.
    fn covered(mesh: &GroupMesh, q: (f64, f64)) -> bool {
        mesh.indices.chunks(3).any(|t| {
            let corner = |i: usize| t.get(i).and_then(|&k| mesh.points.get(k as usize)).map(|p| (p.x.0 as f64, p.y.0 as f64));
            let (Some(a), Some(b), Some(c)) = (corner(0), corner(1), corner(2)) else { return false };
            let side = |p: (f64, f64), r: (f64, f64)| (r.0 - p.0) * (q.1 - p.1) - (r.1 - p.1) * (q.0 - p.0);
            let (d1, d2, d3) = (side(a, b), side(b, c), side(c, a));
            !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
        })
    }

    #[test]
    fn a_joint_without_a_round_corner_leaves_no_gap() {
        // A 0.25 mm line turning left by just under what needs a round corner at 2 µm: the outside
        // of the turn, to its right, is covered all the way from the centre line to the edge.
        let theta: f64 = 0.0152;
        let end = p((1e6 * theta.cos()).round() as i64, (1e6 * theta.sin()).round() as i64);
        let line = Shape::Stroke { points: vec![p(-MM, 0), p(0, 0), end], width: Nm(MM / 4) };
        let mesh = meshes(&solid(vec![line]), Nm(2_000)).into_iter().next().expect("a mesh");
        let fan = segments(MM as f64 / 8.0, Nm(2_000)) as usize + 1;
        assert_eq!(mesh.points.len(), 2 * 4 + 2 * fan, "no disc at the joint");
        let (sx, sy) = ((theta / 2.0).sin(), -(theta / 2.0).cos());
        for distance in [1_000.0, 12_500.0, 31_000.0, 62_000.0, 112_000.0, 124_000.0] {
            assert!(covered(&mesh, (sx * distance, sy * distance)), "{distance} nm out from the joint");
        }
    }

    #[test]
    fn circles_get_enough_sides_and_no_more_than_allowed() {
        assert_eq!(segments(1.0, Nm(1_000)), 6);
        assert_eq!(segments(1e12, Nm(1)), 128);
        assert_eq!(segments(f64::NAN, Nm(1)), 128);
        let n = segments(500_000.0, Nm(2_000));
        assert!((20..=40).contains(&n), "{n} sides for a 1 mm circle at 2 µm");
    }

    #[test]
    fn a_group_that_covers_nothing_has_no_mesh() {
        let nothing = vec![
            Shape::Area(Polygon::new(vec![p(0, 0), p(MM, 0)])),
            Shape::Area(Polygon::default()),
            Shape::Disc { center: p(0, 0), radius: Nm(0) },
            Shape::Stroke { points: vec![p(0, 0), p(MM, 0)], width: Nm(0) },
        ];
        assert!(meshes(&group(nothing.clone()), Nm(1_000)).is_empty());
        assert!(meshes(&solid(nothing), Nm(1_000)).is_empty());
        // Shapes far beyond any coordinate are left out, not a crash.
        let far = vec![
            Shape::Disc { center: p(i64::MAX, 0), radius: Nm(i64::MAX) },
            Shape::Stroke { points: vec![p(i64::MIN, 0), p(i64::MAX, 0)], width: Nm(i64::MAX) },
        ];
        let _ = meshes(&solid(far), Nm(0));
        assert!(meshes(&group(Vec::new()), Nm(1_000)).is_empty());
    }

    #[test]
    fn a_board_becomes_one_mesh_for_each_group_that_covers_anything() {
        use circuitcraft_model::Board;
        use circuitcraft_model::layer_ids::TOP_COPPER;
        let mut b = Board::new("x");
        b.add_track(TOP_COPPER, Nm(MM / 4), circuitcraft_model::TrackPath::Line { from: p(0, 0), to: p(5 * MM, 0) }, None).expect("track");
        b.add_via(p(5 * MM, 0), Nm(600_000), Nm(300_000), None).expect("via");
        let list = crate::display_list(&b, &crate::Options::all(&b));
        let meshes = meshes(&list, crate::TOLERANCE);
        let kinds: Vec<(Option<LayerId>, GroupKind)> = meshes.iter().map(|m| (m.layer, m.kind)).collect();
        assert_eq!(kinds, list.groups.iter().map(|g| (g.layer, g.kind)).collect::<Vec<_>>());
        assert!(meshes.iter().all(|m| m.indices.len().is_multiple_of(3) && m.indices.iter().all(|&i| (i as usize) < m.points.len())));
    }
}
