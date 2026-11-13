//! The board outline, assembled from the loose lines and arcs drawn on the outline layer.

use crate::{Board, Graphic, GraphicShape, ItemId, LayerKind, MAX_ITEMS, ModelError};
use circuitcraft_geom::{Arc, Contour, Edge, GeomError, MAX_POINTS, Piece, chain, ring_contains};
use circuitcraft_units::{Nm, Point};
use std::collections::BTreeSet;

/// The widest gap between the ends of two drawings that [`Board::assemble_outline`] closes.
pub const MAX_OUTLINE_GAP: Nm = Nm(1_000_000);

/// How many corners of the outline [`Board::assemble_outline`] compares cutouts with, all cutouts
/// together: each cutout is looked up against every corner of the outline, so the cost is their
/// product. A board with a thousand holes inside an outline of ten thousand corners uses a tenth
/// of this.
const MAX_OUTLINE_WORK: usize = 100_000_000;

/// What [`Board::assemble_outline`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Assembled {
    /// How many drawings became the outline and the cutouts; they are no longer drawings.
    pub used: usize,
    /// How many cutouts the board now has.
    pub cutouts: usize,
}

impl Board {
    /// Makes the board outline and its cutouts from the drawings on the outline layer, replacing
    /// any the board had.
    ///
    /// Lines and arcs join end to end where the ends are within `gap` of each other (at most
    /// [`MAX_OUTLINE_GAP`]); circles and polygons are closed already. The largest closed shape
    /// becomes the outline and every other one a cutout, which must start inside the outline (its
    /// lowest leftmost corner is the one that is looked at): a board has one outline. The drawings
    /// are removed, so the outline is held once.
    ///
    /// Fails, changing nothing, when the layer has no drawings, when a drawing is part of no
    /// closed shape, when a closed shape lies outside the largest, or when the drawings are more
    /// than can be worked through in a moment (their points, or the corners of the outline times
    /// the number of cutouts). A drawing left open is
    /// refused, not skipped: without the line that was meant to close the board's edge, the
    /// largest closed shape would be a mounting hole.
    pub fn assemble_outline(&mut self, gap: Nm) -> Result<Assembled, ModelError> {
        if !(0..=MAX_OUTLINE_GAP.0).contains(&gap.0) {
            return Err(ModelError::OutOfRange("outline gap (0 to 1 mm)"));
        }
        // Each piece and the drawing it came from.
        let mut pieces = Vec::new();
        let mut owners = Vec::new();
        let on_outline = |g: &&Graphic| self.stack.get(g.layer).is_some_and(|l| l.kind == LayerKind::Outline);
        let mut drawings = 0_usize;
        for g in self.graphics.iter().filter(on_outline) {
            drawings += 1;
            pieces_of(&g.shape, &mut pieces)?;
            owners.resize(pieces.len(), g.id);
        }
        let loops = chain(&pieces, gap).map_err(geometry)?;
        let used: BTreeSet<ItemId> = owners.iter().copied().collect();
        // Every piece is in a loop, and every drawing has a piece (a polygon without edges has none).
        if loops.iter().map(|l| l.pieces.len()).sum::<usize>() != pieces.len() || used.len() != drawings {
            return Err(ModelError::BadOutline(
                "a drawing on the outline layer is part of no closed shape; close it, allow a wider gap or remove it",
            ));
        }
        let outer = loops.iter().max_by_key(|l| l.area2).ok_or(ModelError::BadOutline("there are no drawings on the outline layer"))?;
        let inside = |l: &&circuitcraft_geom::Loop| l.ring.first().is_some_and(|&p| ring_contains(&outer.ring, p));
        let others = loops.iter().filter(|l| !std::ptr::eq(*l, outer));
        if outer.ring.len().saturating_mul(loops.len()) > MAX_OUTLINE_WORK {
            return Err(ModelError::TooMany("cutouts for an outline with this many corners"));
        }
        if !others.clone().all(|l| inside(&l)) {
            return Err(ModelError::BadOutline("a closed shape lies outside the largest one; a board has one outline"));
        }
        let cutouts: Vec<Contour> = others.map(|l| l.contour.clone()).collect();
        if cutouts.len() > MAX_ITEMS {
            return Err(ModelError::TooMany("cutouts"));
        }
        let done = Assembled { used: used.len(), cutouts: cutouts.len() };
        self.outline = Some(outer.contour.clone());
        self.cutouts = cutouts;
        for id in used {
            self.graphics.remove(id);
        }
        Ok(done)
    }
}

/// Adds the lines and arcs a drawing is made of: itself, a circle as one full-circle arc, or the
/// edges of a polygon, closed. Together the drawings may have as many pieces as a contour has
/// points.
fn pieces_of(shape: &GraphicShape, pieces: &mut Vec<Piece>) -> Result<(), ModelError> {
    let mut push = |piece| {
        if pieces.len() >= MAX_POINTS {
            return Err(ModelError::TooMany("outline drawings"));
        }
        pieces.push(piece);
        Ok(())
    };
    match shape {
        GraphicShape::Line { from, to } => push(Piece::Line { from: *from, to: *to }),
        GraphicShape::Arc { arc } => push(Piece::Arc(*arc)),
        GraphicShape::Circle { center, radius } => {
            let across = |r: i64| center.x.0.checked_add(r).map(|x| Point::new(Nm(x), center.y)).ok_or(ModelError::OutOfRange("circle radius"));
            let (start, mid) = (across(radius.0)?, across(radius.0.checked_neg().unwrap_or(0))?);
            push(Piece::Arc(Arc::new(start, mid, start)))
        }
        GraphicShape::Polygon { outline, .. } => {
            let mut at = outline.start;
            for edge in &outline.edges {
                push(match *edge {
                    Edge::Line { to } => Piece::Line { from: at, to },
                    Edge::Arc { mid, to } => Piece::Arc(Arc::new(at, mid, to)),
                })?;
                at = edge.end();
            }
            if at == outline.start { Ok(()) } else { push(Piece::Line { from: at, to: outline.start }) }
        }
    }
}

fn geometry(e: GeomError) -> ModelError {
    match e {
        GeomError::OutOfRange | GeomError::InvalidWidth(_) => ModelError::OutOfRange("drawing on the outline layer"),
        GeomError::TooManyPoints => ModelError::TooMany("outline points"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer_ids::{OUTLINE, TOP_SILK};

    const MM: i64 = 1_000_000;

    fn p(x: i64, y: i64) -> Point {
        Point::new(Nm(x), Nm(y))
    }

    fn draw(b: &mut Board, layer: crate::LayerId, shape: GraphicShape) -> ItemId {
        let id = b.fresh_id().expect("id");
        b.graphics.insert(Graphic { id, layer, width: Nm(100_000), shape }).expect("room");
        id
    }

    fn line(a: Point, b: Point) -> GraphicShape {
        GraphicShape::Line { from: a, to: b }
    }

    /// A 50 by 30 mm rectangle drawn as four lines on the outline layer.
    fn board_with_rectangle() -> Board {
        let mut b = Board::new("x");
        let corners = [p(0, 0), p(50 * MM, 0), p(50 * MM, 30 * MM), p(0, 30 * MM)];
        for (i, &a) in corners.iter().enumerate() {
            draw(&mut b, OUTLINE, line(a, corners[(i + 1) % 4]));
        }
        b
    }

    #[test]
    fn lines_on_the_outline_layer_become_the_outline() {
        let mut b = board_with_rectangle();
        // A mounting hole drawn as a circle, a slot drawn as a polygon, and a silkscreen line that
        // has nothing to do with the outline.
        let hole = draw(&mut b, OUTLINE, GraphicShape::Circle { center: p(5 * MM, 5 * MM), radius: Nm(MM) });
        let slot = Contour::polyline(&[p(20 * MM, 10 * MM), p(30 * MM, 10 * MM), p(30 * MM, 12 * MM), p(20 * MM, 12 * MM)]).expect("slot");
        draw(&mut b, OUTLINE, GraphicShape::Polygon { outline: slot, filled: false });
        let silk = draw(&mut b, TOP_SILK, line(p(0, 0), p(MM, 0)));

        assert_eq!(b.assemble_outline(Nm(0)), Ok(Assembled { used: 6, cutouts: 2 }));
        let outline = b.outline.as_ref().expect("an outline");
        assert_eq!(outline.start, p(0, 0));
        let ends: Vec<Point> = outline.edges.iter().map(Edge::end).collect();
        assert_eq!(ends, [p(50 * MM, 0), p(50 * MM, 30 * MM), p(0, 30 * MM), p(0, 0)], "counter-clockwise from the lower left");
        assert_eq!(b.cutouts.len(), 2);
        assert!(b.cutouts.iter().any(|c| c.start == p(20 * MM, 10 * MM) && c.edges.len() == 4), "the slot");
        assert!(b.cutouts.iter().any(|c| matches!(c.edges.as_slice(), [Edge::Arc { .. }])), "the round hole");
        // What was used is no longer a drawing; the rest is untouched.
        assert!(!b.graphics.contains(hole));
        assert_eq!(b.graphics.iter().map(|g| g.id).collect::<Vec<_>>(), [silk]);
        assert_eq!(b.validate(), Ok(()));
        // Nothing is left to assemble, and the outline stays.
        assert_eq!(b.assemble_outline(Nm(0)), Err(ModelError::BadOutline("there are no drawings on the outline layer")));
        assert!(b.outline.is_some() && b.cutouts.len() == 2);
    }

    #[test]
    fn a_drawing_left_open_is_refused_not_skipped() {
        const OPEN: &str = "a drawing on the outline layer is part of no closed shape; close it, allow a wider gap or remove it";
        // Three sides of the board and a mounting hole: the hole must not become the outline.
        let mut b = board_with_rectangle();
        let side = b.graphics.iter().next().map(|g| g.id).expect("a line");
        b.graphics.remove(side);
        draw(&mut b, OUTLINE, GraphicShape::Circle { center: p(5 * MM, 5 * MM), radius: Nm(MM) });
        let before = b.clone();
        assert_eq!(b.assemble_outline(Nm(0)), Err(ModelError::BadOutline(OPEN)));
        assert_eq!(b, before);
        // A closed board with a stray line on the layer, and a line from one of its corners.
        for stray in [line(p(40 * MM, 20 * MM), p(45 * MM, 20 * MM)), line(p(0, 0), p(-5 * MM, -5 * MM))] {
            let mut b = board_with_rectangle();
            draw(&mut b, OUTLINE, stray);
            assert_eq!(b.assemble_outline(Nm(0)), Err(ModelError::BadOutline(OPEN)));
            assert!(b.outline.is_none() && b.graphics.len() == 5);
        }
    }

    #[test]
    fn ends_that_almost_meet_join_within_the_gap() {
        let mut b = Board::new("x");
        // The last corner is drawn 400 nm short.
        draw(&mut b, OUTLINE, line(p(0, 0), p(10 * MM, 0)));
        draw(&mut b, OUTLINE, line(p(10 * MM, 0), p(10 * MM, 10 * MM)));
        draw(&mut b, OUTLINE, line(p(10 * MM, 10 * MM), p(400, 400)));
        let before = b.clone();
        assert!(matches!(b.assemble_outline(Nm(399)), Err(ModelError::BadOutline(_))));
        assert_eq!(b, before, "a refused outline changes nothing");
        assert_eq!(b.assemble_outline(Nm(400)), Ok(Assembled { used: 3, cutouts: 0 }));
        assert_eq!(b.outline.as_ref().map(|o| o.start), Some(p(0, 0)));
    }

    #[test]
    fn a_board_has_one_outline() {
        // Two rectangles side by side: neither is inside the other.
        let mut b = board_with_rectangle();
        let other = Contour::polyline(&[p(60 * MM, 0), p(70 * MM, 0), p(70 * MM, 10 * MM), p(60 * MM, 10 * MM)]).expect("rectangle");
        draw(&mut b, OUTLINE, GraphicShape::Polygon { outline: other, filled: false });
        let before = b.clone();
        assert_eq!(b.assemble_outline(Nm(0)), Err(ModelError::BadOutline("a closed shape lies outside the largest one; a board has one outline")));
        assert_eq!(b, before);
        // Nothing on the outline layer at all, and a gap that is no gap.
        let mut empty = Board::new("x");
        draw(&mut empty, TOP_SILK, GraphicShape::Circle { center: p(0, 0), radius: Nm(MM) });
        assert_eq!(empty.assemble_outline(Nm(0)), Err(ModelError::BadOutline("there are no drawings on the outline layer")));
        for gap in [-1, MAX_OUTLINE_GAP.0 + 1, i64::MIN, i64::MAX] {
            assert_eq!(empty.assemble_outline(Nm(gap)), Err(ModelError::OutOfRange("outline gap (0 to 1 mm)")));
        }
    }

    #[test]
    fn assembling_replaces_the_outline_the_board_had() {
        let mut b = board_with_rectangle();
        b.outline = Contour::polyline(&[p(0, 0), p(MM, 0), p(MM, MM)]);
        b.cutouts = vec![Contour::circle(p(0, 0), Nm(1_000)).expect("circle")];
        assert_eq!(b.assemble_outline(Nm(0)), Ok(Assembled { used: 4, cutouts: 0 }));
        assert_eq!(b.outline.as_ref().map(|o| o.edges.len()), Some(4));
        assert!(b.cutouts.is_empty());
    }

    #[test]
    fn a_polygon_without_edges_is_refused_not_skipped() {
        const OPEN: &str = "a drawing on the outline layer is part of no closed shape; close it, allow a wider gap or remove it";
        let mut b = board_with_rectangle();
        draw(&mut b, OUTLINE, GraphicShape::Polygon { outline: Contour { start: p(MM, MM), edges: Vec::new() }, filled: false });
        assert_eq!(b.assemble_outline(Nm(0)), Err(ModelError::BadOutline(OPEN)));
        assert!(b.outline.is_none() && b.graphics.len() == 5);
        // Such a board doesn't pass validation in the first place.
        assert_eq!(b.validate(), Err(ModelError::BadArea("drawing outline", "an outline needs an edge")));
    }

    #[test]
    fn an_outline_with_too_many_corners_and_cutouts_is_refused_before_it_takes_long() {
        // 10,000 corners on a circle of 1 m, and 10,000 triangles inside it.
        let corner = |i: u32| {
            let a = f64::from(i) * std::f64::consts::TAU / 10_000.0;
            p((1e9 * a.cos()).round() as i64, (1e9 * a.sin()).round() as i64)
        };
        let mut b = Board::new("x");
        let outline = Contour::polyline(&(0..10_000).map(corner).collect::<Vec<_>>()).expect("outline");
        draw(&mut b, OUTLINE, GraphicShape::Polygon { outline, filled: false });
        let mut last = None;
        for i in 0..10_000_i64 {
            let (x, y) = ((i % 100 - 50) * MM, (i / 100 - 50) * MM);
            let triangle = Contour::polyline(&[p(x, y), p(x + MM / 2, y), p(x, y + MM / 2)]).expect("triangle");
            last = Some(draw(&mut b, OUTLINE, GraphicShape::Polygon { outline: triangle, filled: false }));
        }
        assert_eq!(b.assemble_outline(Nm(0)), Err(ModelError::TooMany("cutouts for an outline with this many corners")));
        assert!(b.outline.is_none() && b.graphics.len() == 10_001);
        // With one cutout fewer it is within the budget.
        b.graphics.remove(last.expect("a triangle"));
        assert_eq!(b.assemble_outline(Nm(0)), Ok(Assembled { used: 10_000, cutouts: 9_999 }));
    }

    #[test]
    fn hostile_drawings_are_errors() {
        let mut b = Board::new("x");
        draw(&mut b, OUTLINE, GraphicShape::Circle { center: p(i64::MAX, 0), radius: Nm(i64::MAX) });
        assert_eq!(b.assemble_outline(Nm(0)), Err(ModelError::OutOfRange("circle radius")));
        let mut b = Board::new("x");
        draw(&mut b, OUTLINE, GraphicShape::Circle { center: p(0, 0), radius: Nm(i64::MIN) });
        assert!(b.assemble_outline(Nm(0)).is_err());
        let mut b = Board::new("x");
        draw(&mut b, OUTLINE, line(p(i64::MIN, 0), p(0, 0)));
        assert_eq!(b.assemble_outline(Nm(0)), Err(ModelError::OutOfRange("drawing on the outline layer")));
    }
}
