//! Display lists: a board as layers of simple shapes, in the order they are drawn.
//!
//! Everything a board holds becomes a [`Shape`] on a layer: round-ended strokes, discs and filled
//! areas, in board coordinates (nanometres, y up). Footprints are placed, mirrored and turned
//! here, texts are laid out in the stroke font, pads are cut into their copper, mask and paste
//! shapes. A front end draws a [`DisplayList`] without knowing what a board is: the SVG and PNG
//! writers in this crate, and the board view of the app.

use crate::font;
use crate::{Rgba, layer_color};
use circuitcraft_geom::{Contour, Edge, Polygon, Rect, offset};
use circuitcraft_model::{
    Board, Footprint, Graphic, GraphicShape, Hole, Justify, LayerId, LayerKind, LayerStack, NetId, Pad, PadShape, Rotation, Side,
};
use circuitcraft_units::{Nm, Point};
use std::collections::{BTreeMap, BTreeSet};

/// How far a flattened arc or circle strays from the true curve, at most, by default.
pub const TOLERANCE: Nm = Nm(2_000);

/// The width of the line the board outline and its cutouts are drawn with.
pub const OUTLINE_WIDTH: Nm = Nm(150_000);

/// The width of the line a zone that has not been poured, or a keepout, is drawn with.
pub const AREA_LINE_WIDTH: Nm = Nm(100_000);

/// The width a drawing without one is drawn with.
pub const HAIRLINE: Nm = Nm(50_000);

/// What the canvas behind the board looks like, and what a hole through the board shows.
pub const BACKGROUND: Rgba = Rgba([16, 18, 22, 255]);

/// The colour keepout areas on a copper layer are drawn in: that of keepout layers.
const KEEPOUT: Rgba = Rgba([60, 200, 220, 255]);

/// The colour laid over the copper of the highlighted net.
pub const HIGHLIGHT: Rgba = Rgba([255, 255, 255, 150]);

/// One thing to draw, in board coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Shape {
    /// A line through `points` (two or more), `width` wide, with round ends and round corners.
    /// A closed line repeats its first point at the end.
    Stroke {
        /// The points, in order.
        points: Vec<Point>,
        /// The width of the line.
        width: Nm,
    },
    /// A filled disc.
    Disc {
        /// Its centre.
        center: Point,
        /// Its radius.
        radius: Nm,
    },
    /// A filled area: inside the outer ring and outside its holes.
    Area(Polygon),
}

impl Shape {
    /// The smallest rectangle that holds the shape, if it holds any point.
    pub fn bounds(&self) -> Option<Rect> {
        let grown = |r: Rect, by: i64| {
            let by = by.max(0);
            Rect::from_corners(
                Point::new(Nm(r.min.x.0.saturating_sub(by)), Nm(r.min.y.0.saturating_sub(by))),
                Point::new(Nm(r.max.x.0.saturating_add(by)), Nm(r.max.y.0.saturating_add(by))),
            )
        };
        match self {
            Shape::Stroke { points, width } => Rect::bounding(points.iter().copied()).map(|r| grown(r, width.0 / 2)),
            Shape::Disc { center, radius } => Some(grown(Rect::from_corners(*center, *center), radius.0)),
            Shape::Area(polygon) => polygon.bounds(),
        }
    }
}

/// What a group of shapes is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupKind {
    /// What is drawn on a layer, in its colour.
    Layer,
    /// The copper of the highlighted net on a layer, laid over that layer.
    Highlight,
    /// Keepout areas on a copper layer.
    Keepouts,
    /// The holes through the board, in the colour of the background, over everything else.
    Holes,
}

/// Shapes drawn together in one colour. Shapes in a group don't show through each other: the
/// colour's opacity applies to the group as a whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// The layer the shapes are on; `None` for the holes, which go through every layer.
    pub layer: Option<LayerId>,
    /// What the group is called, for a person: the layer's name, and what it holds when that is
    /// not all of the layer (`Top Copper: highlight`).
    pub name: String,
    /// What the group is.
    pub kind: GroupKind,
    /// The colour, with the group's opacity.
    pub color: Rgba,
    /// The shapes.
    pub shapes: Vec<Shape>,
}

/// A board ready to draw: groups in drawing order, the first drawn first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayList {
    /// The groups, bottom first.
    pub groups: Vec<Group>,
    /// The smallest rectangle that holds every shape, if there is any.
    pub bounds: Option<Rect>,
}

/// What to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// The layers to draw. Holes are drawn whatever layers are.
    pub visible: BTreeSet<LayerId>,
    /// The layer being worked on, drawn over the others.
    pub active: Option<LayerId>,
    /// The net whose copper is laid over with [`HIGHLIGHT`].
    pub highlight: Option<NetId>,
    /// How far a flattened curve may stray from the true curve.
    pub tolerance: Nm,
}

impl Options {
    /// Every layer of the board in the order a board is built, none raised over the others,
    /// nothing highlighted.
    pub fn all(board: &Board) -> Options {
        Options { visible: board.stack.layers.iter().map(|l| l.id).collect(), active: None, highlight: None, tolerance: TOLERANCE }
    }
}

/// The board as a display list.
pub fn display_list(board: &Board, options: &Options) -> DisplayList {
    let mut sink = Sink::new(board, options);
    sink.board_items();
    sink.finish()
}

/// Where each kind of layer goes among the layers on its side, the copper first.
fn kind_rank(kind: LayerKind) -> u8 {
    match kind {
        LayerKind::Copper | LayerKind::Dielectric => 0,
        LayerKind::SolderMask => 1,
        LayerKind::Paste => 2,
        LayerKind::Silkscreen => 3,
        LayerKind::Courtyard => 4,
        LayerKind::Fabrication => 5,
        LayerKind::Keepout => 6,
        LayerKind::Mechanical => 7,
        LayerKind::Outline => 8,
    }
}

/// The layers in the order they are drawn, seen from the top: the bottom side's layers from the
/// outside in, the copper from the bottom up, the top side's layers from the inside out, then
/// the layers on neither side, and the active layer last of all. Dielectric layers hold nothing
/// to draw and are left out.
fn drawing_order(stack: &LayerStack, active: Option<LayerId>) -> Vec<LayerId> {
    let copper: Vec<LayerId> = stack.copper().map(|l| l.id).collect();
    let mut keyed: Vec<((u8, i64), usize, LayerId)> = stack
        .layers
        .iter()
        .enumerate()
        .filter(|(_, l)| l.kind != LayerKind::Dielectric)
        .map(|(i, l)| {
            let rank = i64::from(kind_rank(l.kind));
            let key = match (l.kind, stack.side(l.id)) {
                (LayerKind::Copper, _) => (1, -(copper.iter().position(|&c| c == l.id).and_then(|p| i64::try_from(p).ok()).unwrap_or(0))),
                (_, Some(Side::Bottom)) => (0, -rank),
                (_, Some(Side::Top)) => (2, rank),
                (_, None) => (3, rank),
            };
            (key, i, l.id)
        })
        .collect();
    keyed.sort_unstable();
    let mut order: Vec<LayerId> = keyed.into_iter().map(|(_, _, id)| id).collect();
    if let Some(i) = active.and_then(|a| order.iter().position(|&id| id == a)) {
        let id = order.remove(i);
        order.push(id);
    }
    order
}

/// Collects shapes by layer as the board's items are walked once.
struct Sink<'a> {
    board: &'a Board,
    options: &'a Options,
    layers: BTreeMap<LayerId, Vec<Shape>>,
    highlight: BTreeMap<LayerId, Vec<Shape>>,
    keepouts: BTreeMap<LayerId, Vec<Shape>>,
    holes: Vec<Shape>,
}

impl<'a> Sink<'a> {
    fn new(board: &'a Board, options: &'a Options) -> Sink<'a> {
        Sink { board, options, layers: BTreeMap::new(), highlight: BTreeMap::new(), keepouts: BTreeMap::new(), holes: Vec::new() }
    }

    fn tolerance(&self) -> Nm {
        Nm(self.options.tolerance.0.max(1))
    }

    /// Adds a shape to a layer, and to the highlight over it when it is on the highlighted net.
    fn put(&mut self, layer: LayerId, net: Option<NetId>, shape: Shape) {
        if !self.options.visible.contains(&layer) {
            return;
        }
        if net.is_some() && net == self.options.highlight {
            self.highlight.entry(layer).or_default().push(shape.clone());
        }
        self.layers.entry(layer).or_default().push(shape);
    }

    fn board_items(&mut self) {
        let board = self.board;
        self.outline();
        for t in board.tracks.iter() {
            let points = match t.path {
                circuitcraft_model::TrackPath::Line { from, to } => vec![from, to],
                circuitcraft_model::TrackPath::Arc { arc } => arc.flatten(self.tolerance()).unwrap_or_default(),
            };
            if let Some(shape) = stroke(points, t.width) {
                self.put(t.layer, t.net, shape);
            }
        }
        for v in board.vias.iter() {
            self.via(v);
        }
        for z in board.zones.iter() {
            if z.filled.is_empty() {
                // Not poured: its outline and holes, so it can be seen where it is.
                for ring in std::iter::once(&z.outline).chain(&z.holes) {
                    if let Some(shape) = self.closed(ring, AREA_LINE_WIDTH) {
                        self.put(z.layer, z.net, shape);
                    }
                }
            } else {
                for area in &z.filled {
                    self.put(z.layer, z.net, Shape::Area(area.clone()));
                }
            }
        }
        for k in board.keepouts.iter() {
            if self.options.visible.contains(&k.layer)
                && let Some(shape) = self.closed(&k.outline, AREA_LINE_WIDTH)
            {
                self.keepouts.entry(k.layer).or_default().push(shape);
            }
        }
        for h in board.holes.iter() {
            self.hole(h.hole, &|p| add(p, h.position), Rotation(0));
        }
        for t in board.texts.iter() {
            self.text(t.layer, &t.text, t.height, t.stroke, t.justify, t.mirrored, t.rotation, t.position, &Some);
        }
        for g in board.graphics.iter() {
            self.graphic(g, g.layer, &Some);
        }
        for f in board.footprints.iter() {
            self.footprint(f);
        }
    }

    /// The board outline and its cutouts, on the first outline layer.
    fn outline(&mut self) {
        let Some(layer) = self.board.stack.layers.iter().find(|l| l.kind == LayerKind::Outline).map(|l| l.id) else { return };
        for ring in self.board.outline.iter().chain(&self.board.cutouts) {
            if let Some(shape) = self.closed(ring, OUTLINE_WIDTH) {
                self.put(layer, None, shape);
            }
        }
    }

    /// A closed contour as a line `width` wide.
    fn closed(&self, contour: &Contour, width: Nm) -> Option<Shape> {
        let mut points = contour.flatten(self.tolerance()).ok()?;
        points.push(*points.first()?);
        stroke(points, width)
    }

    fn via(&mut self, v: &circuitcraft_model::Via) {
        let stack = &self.board.stack;
        let copper: Vec<LayerId> = stack.copper().map(|l| l.id).collect();
        let (Some(a), Some(b)) = (copper.iter().position(|&l| l == v.from), copper.iter().position(|&l| l == v.to)) else { return };
        let disc = |d: Nm| Shape::Disc { center: v.position, radius: Nm(d.0 / 2) };
        for &layer in copper.get(a.min(b)..=a.max(b)).unwrap_or_default() {
            self.put(layer, v.net, disc(v.diameter));
        }
        // The solder mask over each side whose outer copper the via reaches, as the design
        // settings resolve it. A board with one copper layer has it on the top only.
        let joined = copper.get(a.min(b)..=a.max(b)).unwrap_or_default();
        for (side, opening) in [(Side::Top, v.top_mask), (Side::Bottom, v.bottom_mask)] {
            let reaches = joined.iter().any(|&l| stack.side(l) == Some(side));
            if let (true, Some(mask), Some(by)) = (reaches, coating(stack, LayerKind::SolderMask, side), self.board.design.via_mask(opening)) {
                for opening in self.grown(&disc(v.diameter), by) {
                    self.put(mask, None, opening);
                }
            }
        }
        self.holes.push(disc(v.drill));
    }

    /// A hole through the board at `place(0, 0)`, turned by `turn` (a slot's own turn adds to it).
    fn hole(&mut self, hole: Hole, place: &dyn Fn(Point) -> Option<Point>, turn: Rotation) {
        let radius = Nm(hole.diameter.0 / 2);
        let shape = match hole.slot {
            None => place(Point::ORIGIN).map(|center| Shape::Disc { center, radius }),
            Some(slot) => {
                let half = slot.length.0.saturating_sub(hole.diameter.0).max(0) / 2;
                let turn = Rotation::from_millidegrees(i64::from(turn.0) + i64::from(slot.rotation.0));
                let end = |x: i64| turn.turn(Point::new(Nm(x), Nm(0))).and_then(place);
                end(-half).zip(end(half)).map(|(a, b)| Shape::Stroke { points: vec![a, b], width: hole.diameter })
            }
        };
        self.holes.extend(shape);
    }

    /// A text laid out in the stroke font at its place: mirrored about its anchor, turned, moved
    /// to `position`, and then put on the board by `place` (a footprint's placement, or none).
    #[allow(clippy::too_many_arguments)] // every argument is a field of the text
    fn text(
        &mut self,
        layer: LayerId,
        words: &str,
        height: Nm,
        width: Nm,
        justify: Justify,
        mirrored: bool,
        rotation: Rotation,
        position: Point,
        place: &dyn Fn(Point) -> Option<Point>,
    ) {
        if !self.options.visible.contains(&layer) {
            return;
        }
        for line in font::layout(words, height, justify) {
            let points = line.into_iter().filter_map(|p| {
                let p = if mirrored { Point::new(Nm(p.x.0.checked_neg()?), p.y) } else { p };
                place(add(rotation.turn(p)?, position)?)
            });
            if let Some(shape) = stroke(points.collect(), width) {
                self.put(layer, None, shape);
            }
        }
    }

    /// A drawing, its points put on the board by `place`, on `layer`.
    fn graphic(&mut self, g: &Graphic, layer: LayerId, place: &dyn Fn(Point) -> Option<Point>) {
        if !self.options.visible.contains(&layer) {
            return;
        }
        let tolerance = self.tolerance();
        let placed = |points: Vec<Point>| points.into_iter().map(place).collect::<Option<Vec<Point>>>();
        // A line drawn without a width is drawn as a hairline: a line that thin would not show.
        let line_width = if g.width.0 == 0 { HAIRLINE } else { g.width };
        let shapes: Vec<Shape> = match &g.shape {
            GraphicShape::Line { from, to } => placed(vec![*from, *to]).and_then(|p| stroke(p, line_width)).into_iter().collect(),
            GraphicShape::Arc { arc } => arc.flatten(tolerance).ok().and_then(placed).and_then(|p| stroke(p, line_width)).into_iter().collect(),
            // A circle drawn without a width is a filled disc: an outline that thin would not show.
            GraphicShape::Circle { center, radius } if g.width.0 == 0 => {
                place(*center).map(|center| Shape::Disc { center, radius: *radius }).into_iter().collect()
            }
            GraphicShape::Circle { center, radius } => Contour::circle(*center, *radius)
                .ok()
                .and_then(|c| c.flatten(tolerance).ok())
                .and_then(placed)
                .and_then(|mut p| {
                    p.push(*p.first()?);
                    stroke(p, g.width)
                })
                .into_iter()
                .collect(),
            GraphicShape::Polygon { outline, filled } => {
                let Some(ring) = outline.flatten(tolerance).ok().and_then(placed) else { return };
                let mut shapes = Vec::new();
                if *filled && ring.len() >= 3 {
                    shapes.push(Shape::Area(Polygon::new(ring.clone())));
                }
                // A filled area without a width has no outline; an outline alone is a hairline.
                if g.width.0 > 0 || !*filled {
                    let mut line = ring;
                    if let Some(&first) = line.first() {
                        line.push(first);
                    }
                    shapes.extend(stroke(line, line_width));
                }
                shapes
            }
        };
        for shape in shapes {
            self.put(layer, None, shape);
        }
    }

    fn footprint(&mut self, f: &Footprint) {
        let stack = &self.board.stack;
        let on_board = |p: Point| f.to_board(p);
        // A footprint's drawings and texts are named as for the top side.
        let layer = |l: LayerId| if f.side == Side::Bottom { stack.flipped(l).unwrap_or(l) } else { l };
        for g in &f.graphics {
            self.graphic(g, layer(g.layer), &on_board);
        }
        for t in f.texts.iter().filter(|t| t.visible) {
            self.text(layer(t.layer), f.wording(t), t.height, t.stroke, t.justify, t.mirrored, t.rotation, t.position, &on_board);
        }
        for pad in &f.pads {
            self.pad(f, pad);
        }
    }

    fn pad(&mut self, f: &Footprint, pad: &Pad) {
        let stack = &self.board.stack;
        let design = self.board.design;
        // A point of the copper, relative to its centre, on the board.
        let copper = |p: Point| f.to_board(add(pad.rotation.turn(add(p, pad.copper_offset)?)?, pad.position)?);
        let copper_layers: Vec<LayerId> = stack.copper().map(|l| l.id).collect();
        let inner = copper_layers.get(1..copper_layers.len().saturating_sub(1)).unwrap_or_default();
        let faces = [(f.side, &pad.front), (f.side.opposite(), &pad.back)];
        for (side, face) in faces {
            let Some(face) = face else { continue };
            // A board with one copper layer has it on the top only: a face on the bottom has
            // neither copper nor openings there.
            let Some(outer) = stack.copper().find(|l| stack.side(l.id) == Some(side)).map(|l| l.id) else { continue };
            let Some(shape) = self.pad_shape(&face.shape, &copper) else { continue };
            self.put(outer, pad.net, shape.clone());
            let openings = [(LayerKind::SolderMask, design.pad_mask(face.mask)), (LayerKind::Paste, design.pad_paste(face.paste))];
            for (kind, by) in openings {
                if let (Some(layer), Some(by)) = (coating(stack, kind, side), by) {
                    for opening in self.grown(&shape, by) {
                        self.put(layer, None, opening);
                    }
                }
            }
        }
        if let Some(shape) = pad.inner.as_ref().and_then(|s| self.pad_shape(s, &copper)) {
            for &layer in inner {
                self.put(layer, pad.net, shape.clone());
            }
        }
        if let Some(hole) = pad.hole {
            // The hole is centred on the pad's position, not on its copper.
            let centre = |p: Point| f.to_board(add(p, pad.position)?);
            self.hole(hole, &centre, pad.rotation);
        }
    }

    /// A pad's copper shape, its points put on the board by `place` (centred on the copper).
    fn pad_shape(&self, shape: &PadShape, place: &dyn Fn(Point) -> Option<Point>) -> Option<Shape> {
        let p = |x: i64, y: i64| Point::new(Nm(x), Nm(y));
        match *shape {
            PadShape::Circle { diameter } => Some(Shape::Disc { center: place(Point::ORIGIN)?, radius: Nm(diameter.0 / 2) }),
            PadShape::Rect { width, height } => {
                let (w, h) = (width.0 / 2, height.0 / 2);
                let ring = [p(-w, -h), p(w, -h), p(w, h), p(-w, h)].into_iter().map(place).collect::<Option<Vec<_>>>()?;
                Some(Shape::Area(Polygon::new(ring)))
            }
            PadShape::Oval { width, height } => {
                let (w, h) = (width.0, height.0);
                let half = i64::try_from(w.abs_diff(h) / 2).ok()?;
                let (a, b) = if w >= h { (p(-half, 0), p(half, 0)) } else { (p(0, -half), p(0, half)) };
                if half == 0 {
                    return Some(Shape::Disc { center: place(Point::ORIGIN)?, radius: Nm(w / 2) });
                }
                Some(Shape::Stroke { points: vec![place(a)?, place(b)?], width: Nm(w.min(h)) })
            }
            PadShape::RoundRect { width, height, radius } => {
                let ring = rounded_rect(width, height, radius).flatten(self.tolerance()).ok()?;
                Some(Shape::Area(Polygon::new(ring.into_iter().map(place).collect::<Option<Vec<_>>>()?)))
            }
            PadShape::Custom { ref outline } => {
                let ring = outline.flatten(self.tolerance()).ok()?;
                Some(Shape::Area(Polygon::new(ring.into_iter().map(place).collect::<Option<Vec<_>>>()?)))
            }
        }
    }

    /// A shape grown on every side by `by` (shrunk when negative): a mask opening or a paste
    /// aperture. A shape shrunk to nothing is no shape.
    fn grown(&self, shape: &Shape, by: Nm) -> Vec<Shape> {
        match shape {
            Shape::Disc { center, radius } => {
                let r = radius.0.saturating_add(by.0);
                if r > 0 { vec![Shape::Disc { center: *center, radius: Nm(r) }] } else { Vec::new() }
            }
            Shape::Stroke { points, width } => {
                let w = width.0.saturating_add(by.0.saturating_mul(2));
                if w > 0 { vec![Shape::Stroke { points: points.clone(), width: Nm(w) }] } else { Vec::new() }
            }
            Shape::Area(polygon) if by.0 == 0 => vec![Shape::Area(polygon.clone())],
            Shape::Area(polygon) => {
                offset(std::slice::from_ref(polygon), by, self.tolerance()).unwrap_or_default().into_iter().map(Shape::Area).collect()
            }
        }
    }

    /// The groups in drawing order, and their bounds.
    fn finish(mut self) -> DisplayList {
        let stack = &self.board.stack;
        let mut groups = Vec::new();
        for layer in drawing_order(stack, self.options.active) {
            let Some(l) = stack.get(layer) else { continue };
            let copper_index = stack.copper().position(|c| c.id == layer).unwrap_or(0);
            let color = layer_color(l, copper_index);
            let mut push = |kind, color, what: &str, shapes: Option<Vec<Shape>>| {
                if let Some(shapes) = shapes.filter(|s| !s.is_empty()) {
                    groups.push(Group { layer: Some(layer), name: format!("{}{what}", l.name), kind, color, shapes });
                }
            };
            push(GroupKind::Layer, color, "", self.layers.remove(&layer));
            push(GroupKind::Highlight, HIGHLIGHT, ": highlight", self.highlight.remove(&layer));
            push(GroupKind::Keepouts, KEEPOUT, ": keepouts", self.keepouts.remove(&layer));
        }
        if !self.holes.is_empty() {
            let shapes = std::mem::take(&mut self.holes);
            groups.push(Group { layer: None, name: "Holes".to_owned(), kind: GroupKind::Holes, color: BACKGROUND, shapes });
        }
        let bounds = groups.iter().flat_map(|g| &g.shapes).filter_map(Shape::bounds).reduce(Rect::union);
        DisplayList { groups, bounds }
    }
}

/// The solder mask or paste layer on a side of the board, if it has one.
fn coating(stack: &LayerStack, kind: LayerKind, side: Side) -> Option<LayerId> {
    stack.layers.iter().find(|l| l.kind == kind && l.side == Some(side)).map(|l| l.id)
}

/// A stroke through `points`, or `None` when there are none. A single point is a dot.
fn stroke(mut points: Vec<Point>, width: Nm) -> Option<Shape> {
    let first = *points.first()?;
    if points.len() == 1 {
        points.push(first);
    }
    Some(Shape::Stroke { points, width })
}

/// `p + q`, or `None` when it overflows.
fn add(p: Point, q: Point) -> Option<Point> {
    Some(Point::new(Nm(p.x.0.checked_add(q.x.0)?), Nm(p.y.0.checked_add(q.y.0)?)))
}

/// A rectangle with rounded corners centred on the origin, as a contour with arcs. The radius is
/// at most half the shorter side, as the model checks.
fn rounded_rect(width: Nm, height: Nm, radius: Nm) -> Contour {
    let (w, h) = (width.0 / 2, height.0 / 2);
    let r = radius.0.clamp(0, w.min(h).max(0));
    let p = |x: i64, y: i64| Point::new(Nm(x), Nm(y));
    // The middle of each corner's arc: the corner's centre plus r/√2 outwards on both axes.
    let d = (r as f64 / std::f64::consts::SQRT_2).round() as i64;
    let corner = |cx: i64, cy: i64, sx: i64, sy: i64| p(cx + sx * d, cy + sy * d);
    let (iw, ih) = (w - r, h - r);
    let edges = vec![
        Edge::Line { to: p(iw, -h) },
        Edge::Arc { mid: corner(iw, -ih, 1, -1), to: p(w, -ih) },
        Edge::Line { to: p(w, ih) },
        Edge::Arc { mid: corner(iw, ih, 1, 1), to: p(iw, h) },
        Edge::Line { to: p(-iw, h) },
        Edge::Arc { mid: corner(-iw, ih, -1, 1), to: p(-w, ih) },
        Edge::Line { to: p(-w, -ih) },
        Edge::Arc { mid: corner(-iw, -ih, -1, -1), to: p(-iw, -h) },
    ];
    // Zero-length lines and arcs between equal points are dropped by flattening.
    Contour { start: p(-iw, -h), edges }
}

#[cfg(test)]
mod tests {
    use super::*;
    use circuitcraft_model::layer_ids::{
        BOTTOM_COPPER, BOTTOM_MASK, BOTTOM_PASTE, BOTTOM_SILK, CORE, KEEPOUT, MECHANICAL, OUTLINE, TOP_COPPER, TOP_MASK, TOP_PASTE, TOP_SILK,
    };
    use circuitcraft_model::{Assembly, FootprintText, ItemId, Keepout, Mounting, PadConnection, Slot, Text, TextContent, TrackPath, Zone, ZoneFill};

    const MM: i64 = 1_000_000;

    fn p(x: i64, y: i64) -> Point {
        Point::new(Nm(x), Nm(y))
    }

    fn group(list: &DisplayList, layer: Option<LayerId>, kind: GroupKind) -> Option<&Group> {
        list.groups.iter().find(|g| g.layer == layer && g.kind == kind)
    }

    fn shapes(list: &DisplayList, layer: LayerId) -> Vec<Shape> {
        group(list, Some(layer), GroupKind::Layer).map(|g| g.shapes.clone()).unwrap_or_default()
    }

    fn footprint(side: Side, pads: Vec<Pad>) -> Footprint {
        Footprint {
            id: ItemId::from_u128(1),
            reference: "R1".to_owned(),
            value: "10k".to_owned(),
            link: None,
            assembly: Assembly { mounting: Mounting::SurfaceMount, in_bom: true, in_placement: true, fitted: true },
            position: p(10 * MM, 10 * MM),
            rotation: Rotation(0),
            side,
            pads,
            graphics: vec![Graphic {
                id: ItemId::from_u128(2),
                layer: TOP_SILK,
                width: Nm(MM / 10),
                shape: GraphicShape::Line { from: p(0, 0), to: p(MM, 0) },
            }],
            texts: vec![
                FootprintText {
                    id: ItemId::from_u128(3),
                    content: TextContent::Reference,
                    visible: true,
                    layer: TOP_SILK,
                    position: p(0, 2 * MM),
                    height: Nm(MM),
                    stroke: Nm(MM / 10),
                    rotation: Rotation(0),
                    mirrored: false,
                    justify: Justify::Left,
                },
                FootprintText {
                    id: ItemId::from_u128(4),
                    content: TextContent::Value,
                    visible: false,
                    layer: TOP_SILK,
                    position: p(0, -2 * MM),
                    height: Nm(MM),
                    stroke: Nm(MM / 10),
                    rotation: Rotation(0),
                    mirrored: false,
                    justify: Justify::Left,
                },
            ],
        }
    }

    fn rect(w: i64, h: i64) -> PadShape {
        PadShape::Rect { width: Nm(w), height: Nm(h) }
    }

    #[test]
    fn layers_are_drawn_from_the_bottom_up_with_the_active_one_last() {
        let stack = LayerStack::two_layer();
        let order = drawing_order(&stack, Some(BOTTOM_COPPER));
        let at = |id| order.iter().position(|&l| l == id).unwrap_or(usize::MAX);
        assert!(!order.contains(&CORE), "the core holds nothing to draw");
        assert!(at(BOTTOM_SILK) < at(BOTTOM_MASK) && at(BOTTOM_MASK) < at(TOP_COPPER), "the bottom side, from the outside in");
        assert!(at(TOP_COPPER) < at(TOP_MASK) && at(TOP_MASK) < at(TOP_PASTE) && at(TOP_PASTE) < at(TOP_SILK), "the top side, inside out");
        assert!(at(TOP_SILK) < at(KEEPOUT) && at(KEEPOUT) < at(MECHANICAL) && at(MECHANICAL) < at(OUTLINE), "then the layers on neither side");
        assert_eq!(order.last(), Some(&BOTTOM_COPPER), "the active layer over everything");
        let order = drawing_order(&stack, None);
        assert!(order.iter().position(|&l| l == BOTTOM_COPPER) < order.iter().position(|&l| l == TOP_COPPER));
    }

    #[test]
    fn tracks_go_on_their_layer_and_the_highlighted_net_is_laid_over() {
        let mut b = Board::new("x");
        let gnd = b.add_net("GND").expect("net");
        b.add_track(TOP_COPPER, Nm(MM / 4), TrackPath::Line { from: p(0, 0), to: p(5 * MM, 0) }, Some(gnd)).expect("track");
        b.add_track(TOP_COPPER, Nm(MM / 4), TrackPath::Line { from: p(0, MM), to: p(5 * MM, MM) }, None).expect("track");
        let mut options = Options::all(&b);
        let list = display_list(&b, &options);
        assert_eq!(shapes(&list, TOP_COPPER).len(), 2);
        assert!(group(&list, Some(TOP_COPPER), GroupKind::Highlight).is_none());
        assert_eq!(list.bounds, Some(Rect::from_corners(p(-MM / 8, -MM / 8), p(5 * MM + MM / 8, MM + MM / 8))));
        options.highlight = Some(gnd);
        let list = display_list(&b, &options);
        let over = group(&list, Some(TOP_COPPER), GroupKind::Highlight).expect("a highlight");
        assert_eq!((over.shapes.len(), over.color, over.name.as_str()), (1, HIGHLIGHT, "Top Copper: highlight"));
        // Drawn right after its layer.
        let at = |kind| list.groups.iter().position(|g| g.layer == Some(TOP_COPPER) && g.kind == kind);
        assert_eq!(at(GroupKind::Highlight), at(GroupKind::Layer).map(|i| i + 1));
        // A hidden layer draws nothing, its highlight neither.
        options.visible.remove(&TOP_COPPER);
        let list = display_list(&b, &options);
        assert!(list.groups.iter().all(|g| g.layer != Some(TOP_COPPER)) && list.bounds.is_none());
    }

    #[test]
    fn a_via_is_copper_on_every_layer_it_joins_a_hole_and_mask_openings_where_not_tented() {
        let mut b = Board::new("x");
        b.add_via(p(MM, MM), Nm(600_000), Nm(300_000), None).expect("via");
        let list = display_list(&b, &Options::all(&b));
        let disc = |r| vec![Shape::Disc { center: p(MM, MM), radius: Nm(r) }];
        assert_eq!((shapes(&list, TOP_COPPER), shapes(&list, BOTTOM_COPPER)), (disc(300_000), disc(300_000)));
        assert_eq!(group(&list, None, GroupKind::Holes).map(|g| g.shapes.clone()), Some(disc(150_000)));
        assert_eq!(list.groups.last().map(|g| g.kind), Some(GroupKind::Holes), "holes over everything");
        assert!(shapes(&list, TOP_MASK).is_empty(), "the default settings tent vias");
        b.design.tent_vias = false;
        let list = display_list(&b, &Options::all(&b));
        assert_eq!((shapes(&list, TOP_MASK), shapes(&list, BOTTOM_MASK)), (disc(350_000), disc(350_000)), "grown by the mask expansion");
    }

    #[test]
    fn a_footprint_on_the_bottom_is_mirrored_onto_the_bottom_layers() {
        let mut b = Board::new("x");
        let pad = Pad::smd("1", p(2 * MM, 0), rect(MM, 2 * MM));
        b.footprints.insert(footprint(Side::Bottom, vec![pad])).expect("room");
        let list = display_list(&b, &Options::all(&b));
        // The pad is 2 mm to the right in the footprint, so 2 mm to the left on the board.
        let copper = shapes(&list, BOTTOM_COPPER);
        assert_eq!(copper.first().and_then(Shape::bounds), Some(Rect::from_corners(p(7_500_000, 9 * MM), p(8_500_000, 11 * MM))));
        assert!(shapes(&list, TOP_COPPER).is_empty());
        // Its mask opening is grown by 0.05 mm, its paste aperture is the size of the copper.
        let mask = shapes(&list, BOTTOM_MASK).first().and_then(Shape::bounds);
        assert_eq!(mask, Some(Rect::from_corners(p(7_450_000, 8_950_000), p(8_550_000, 11_050_000))));
        assert_eq!(shapes(&list, BOTTOM_PASTE).first().and_then(Shape::bounds), copper.first().and_then(Shape::bounds));
        // The drawing and the visible reference go to the bottom silkscreen, mirrored; the hidden
        // value is not drawn.
        let silk = shapes(&list, BOTTOM_SILK);
        assert!(shapes(&list, TOP_SILK).is_empty());
        assert!(silk.contains(&Shape::Stroke { points: vec![p(10 * MM, 10 * MM), p(9 * MM, 10 * MM)], width: Nm(MM / 10) }));
        let text = silk.iter().filter_map(Shape::bounds).filter(|r| r.min.y.0 >= 11 * MM).reduce(Rect::union).expect("the reference");
        assert!(text.max.x.0 <= 10 * MM + MM / 20 && text.min.x.0 < 9 * MM, "read from the bottom: it runs to the left, {text:?}");
        assert!(silk.iter().filter_map(Shape::bounds).all(|r| r.min.y.0 >= 10 * MM - MM / 20), "nothing of the value below");
    }

    #[test]
    fn through_hole_pads_have_copper_on_every_layer_and_a_hole_or_slot() {
        let mut b = Board::new("x");
        let slot = Hole { slot: Some(Slot { length: Nm(3 * MM), rotation: Rotation(90_000) }), ..Hole::round(Nm(MM), true) };
        let oval = PadShape::Oval { width: Nm(2 * MM), height: Nm(4 * MM) };
        let pads = vec![
            Pad::through_hole("1", p(0, 0), oval, slot),
            Pad::through_hole("2", p(5 * MM, 0), PadShape::Circle { diameter: Nm(2 * MM) }, Hole::round(Nm(MM), true)),
        ];
        b.footprints.insert(footprint(Side::Top, pads)).expect("room");
        let list = display_list(&b, &Options::all(&b));
        let oval = Shape::Stroke { points: vec![p(10 * MM, 9 * MM), p(10 * MM, 11 * MM)], width: Nm(2 * MM) };
        let round = Shape::Disc { center: p(15 * MM, 10 * MM), radius: Nm(MM) };
        for layer in [TOP_COPPER, BOTTOM_COPPER] {
            assert_eq!(shapes(&list, layer), vec![oval.clone(), round.clone()]);
        }
        // The slot runs along y, turned a quarter turn with the pad's own rotation of nothing.
        let holes = group(&list, None, GroupKind::Holes).map(|g| g.shapes.clone()).unwrap_or_default();
        let slot = Shape::Stroke { points: vec![p(10 * MM, 9 * MM), p(10 * MM, 11 * MM)], width: Nm(MM) };
        assert_eq!(holes, vec![slot, Shape::Disc { center: p(15 * MM, 10 * MM), radius: Nm(MM / 2) }]);
        assert!(shapes(&list, TOP_PASTE).is_empty(), "no paste on a through-hole pad");
    }

    #[test]
    fn rounded_rectangles_stay_within_their_size() {
        let ring = rounded_rect(Nm(2 * MM), Nm(MM), Nm(MM / 4)).flatten(TOLERANCE).expect("flatten");
        let r = Rect::bounding(ring.iter().copied()).expect("points");
        assert_eq!(r, Rect::from_corners(p(-MM, -MM / 2), p(MM, MM / 2)));
        let area = Polygon::new(ring).area_mm2();
        let exact = 2.0 - (4.0 - std::f64::consts::PI) * 0.25 * 0.25;
        // Each corner is flattened to within 2 µm, inside the curve: about half a thousandth of
        // a square millimetre each.
        assert!(area < exact && exact - area < 0.003, "{area} against {exact}");
        // Fully rounded on the short side, and not rounded at all.
        assert!(rounded_rect(Nm(MM), Nm(MM), Nm(MM / 2)).flatten(TOLERANCE).is_ok_and(|r| r.len() > 8));
        assert_eq!(rounded_rect(Nm(MM), Nm(MM), Nm(0)).flatten(TOLERANCE).map(|r| r.len()), Ok(4));
    }

    #[test]
    fn texts_zones_keepouts_and_the_outline_have_their_places() {
        let mut b = Board::new("x");
        let text = Text {
            id: ItemId::from_u128(9),
            layer: BOTTOM_SILK,
            text: "L".to_owned(),
            position: p(5 * MM, 5 * MM),
            height: Nm(6 * MM),
            stroke: Nm(MM / 5),
            rotation: Rotation(0),
            mirrored: true,
            justify: Justify::Left,
        };
        b.texts.insert(text).expect("room");
        let square = |x: i64| Contour::polyline(&[p(x, 0), p(x + MM, 0), p(x + MM, MM), p(x, MM)]).expect("square");
        let zone = |id, filled| Zone {
            id: ItemId::from_u128(id),
            layer: BOTTOM_COPPER,
            net: None,
            outline: square(0),
            holes: Vec::new(),
            clearance: Nm(0),
            min_width: Nm(0),
            priority: 0,
            fill: ZoneFill::Solid,
            pads: PadConnection::Solid,
            keep_islands: false,
            filled,
        };
        b.zones.insert(zone(10, Vec::new())).expect("room");
        b.zones.insert(zone(11, vec![Polygon::new(vec![p(0, 0), p(MM, 0), p(0, MM)])])).expect("room");
        b.keepouts
            .insert(Keepout { id: ItemId::from_u128(12), layer: TOP_COPPER, outline: square(3 * MM), tracks: true, vias: false, zones: false })
            .expect("room");
        b.outline = Some(square(-MM));
        b.cutouts = vec![square(-MM)];
        let list = display_list(&b, &Options::all(&b));
        // Mirrored: the foot of the L runs to the left of its anchor.
        let l = shapes(&list, BOTTOM_SILK);
        assert_eq!(l, vec![Shape::Stroke { points: vec![p(5 * MM, 11 * MM), p(5 * MM, 5 * MM), p(MM, 5 * MM)], width: Nm(MM / 5) }]);
        let zones = shapes(&list, BOTTOM_COPPER);
        assert!(matches!(zones.as_slice(), [Shape::Stroke { width, .. }, Shape::Area(_)] if *width == AREA_LINE_WIDTH), "{zones:?}");
        let keepouts = group(&list, Some(TOP_COPPER), GroupKind::Keepouts).expect("keepouts");
        assert_eq!((keepouts.shapes.len(), keepouts.color), (1, super::KEEPOUT));
        let outline = shapes(&list, OUTLINE);
        assert_eq!(outline.len(), 2);
        assert!(outline.iter().all(
            |s| matches!(s, Shape::Stroke { points, width } if points.len() == 5 && *width == OUTLINE_WIDTH && points.first() == points.last())
        ));
    }

    /// A four-layer stack: two inner copper layers between the outer ones.
    fn four_layers(b: &mut Board) -> (LayerId, LayerId) {
        let inner = |id: u16, name: &str| circuitcraft_model::Layer {
            id: LayerId(id),
            name: name.to_owned(),
            kind: LayerKind::Copper,
            thickness: Some(Nm(35_000)),
            side: None,
            pair: None,
        };
        let at = b.stack.layers.iter().position(|l| l.id == CORE).expect("the core");
        b.stack.layers.insert(at, inner(100, "Inner 1"));
        b.stack.layers.insert(at + 1, inner(101, "Inner 2"));
        (LayerId(100), LayerId(101))
    }

    fn centre(shape: Option<&Shape>) -> Option<Point> {
        let r = shape.and_then(Shape::bounds)?;
        Some(p((r.min.x.0 + r.max.x.0) / 2, (r.min.y.0 + r.max.y.0) / 2))
    }

    #[test]
    fn a_pad_is_moved_off_centre_before_it_is_turned_and_its_slot_turns_with_it() {
        let mut b = Board::new("x");
        let slot = Hole { slot: Some(Slot { length: Nm(3 * MM), rotation: Rotation(0) }), ..Hole::round(Nm(MM), true) };
        let mut pad = Pad::through_hole("1", p(0, 0), rect(MM / 2, MM / 2), slot);
        pad.rotation = Rotation(90_000);
        pad.copper_offset = p(MM, 0);
        b.footprints.insert(footprint(Side::Top, vec![pad])).expect("room");
        let list = display_list(&b, &Options::all(&b));
        // The copper's centre is 1 mm along the pad's own x axis, which the pad turns to point up.
        assert_eq!(centre(shapes(&list, TOP_COPPER).first()), Some(p(10 * MM, 11 * MM)));
        // The slot runs along the pad's x axis, so up as well, and is centred on the pad itself.
        let holes = group(&list, None, GroupKind::Holes).map(|g| g.shapes.clone()).unwrap_or_default();
        assert_eq!(holes, vec![Shape::Stroke { points: vec![p(10 * MM, 9 * MM), p(10 * MM, 11 * MM)], width: Nm(MM) }]);
    }

    #[test]
    fn a_mirrored_text_is_mirrored_before_it_is_turned() {
        let mut b = Board::new("x");
        let text = Text {
            id: ItemId::from_u128(9),
            layer: TOP_SILK,
            text: "L".to_owned(),
            position: p(0, 0),
            height: Nm(6 * MM),
            stroke: Nm(MM / 5),
            rotation: Rotation(90_000),
            mirrored: true,
            justify: Justify::Left,
        };
        b.texts.insert(text).expect("room");
        let list = display_list(&b, &Options::all(&b));
        // The L's stem (up 6), corner and foot (right 4), mirrored (foot left 4), then turned a
        // quarter turn: the stem points left and the foot down.
        let l = vec![Shape::Stroke { points: vec![p(-6 * MM, 0), p(0, 0), p(0, -4 * MM)], width: Nm(MM / 5) }];
        assert_eq!(shapes(&list, TOP_SILK), l);
    }

    #[test]
    fn inner_copper_and_blind_vias_on_four_layers() {
        let mut b = Board::new("x");
        let (inner1, inner2) = four_layers(&mut b);
        let pad = Pad::through_hole("1", p(0, 0), PadShape::Circle { diameter: Nm(2 * MM) }, Hole::round(Nm(MM), true));
        b.footprints.insert(footprint(Side::Top, vec![pad])).expect("room");
        b.design.tent_vias = false;
        // A blind via from the top to the first inner layer.
        let id = b.add_via(p(0, 0), Nm(600_000), Nm(300_000), None).expect("via");
        if let Some(v) = b.vias.get_mut(id) {
            v.to = inner1;
        }
        let list = display_list(&b, &Options::all(&b));
        let pad = Shape::Disc { center: p(10 * MM, 10 * MM), radius: Nm(MM) };
        let via = Shape::Disc { center: p(0, 0), radius: Nm(300_000) };
        assert!(shapes(&list, inner1).contains(&pad) && shapes(&list, inner2) == vec![pad.clone()], "the pad on both inner layers");
        assert!(shapes(&list, inner1).contains(&via) && !shapes(&list, inner2).contains(&via), "the via down to the first inner layer only");
        assert!(shapes(&list, BOTTOM_COPPER) == vec![pad], "the via doesn't reach the bottom");
        let opening = Shape::Disc { center: p(0, 0), radius: Nm(350_000) };
        assert!(shapes(&list, TOP_MASK).contains(&opening));
        assert!(!shapes(&list, BOTTOM_MASK).contains(&opening), "no opening on a side the via doesn't reach");
    }

    #[test]
    fn a_board_with_one_copper_layer_has_nothing_on_the_bottom() {
        let mut b = Board::new("x");
        b.stack.layers.retain(|l| l.id != BOTTOM_COPPER);
        b.design.tent_vias = false;
        b.footprints.insert(footprint(Side::Bottom, vec![Pad::smd("1", p(0, 0), rect(MM, MM))])).expect("room");
        let id = b.add_via(p(0, 0), Nm(600_000), Nm(300_000), None).expect("via");
        assert_eq!(b.vias.get(id).map(|v| (v.from, v.to)), Some((TOP_COPPER, TOP_COPPER)));
        let list = display_list(&b, &Options::all(&b));
        assert!(shapes(&list, BOTTOM_MASK).is_empty() && shapes(&list, BOTTOM_PASTE).is_empty());
        assert_eq!(shapes(&list, TOP_MASK), vec![Shape::Disc { center: p(0, 0), radius: Nm(350_000) }]);
        assert_eq!(shapes(&list, TOP_COPPER), vec![Shape::Disc { center: p(0, 0), radius: Nm(300_000) }], "the bottom pad has no copper");
    }

    #[test]
    fn drawings_without_a_width_still_show() {
        let mut b = Board::new("x");
        let shapes_of = [
            GraphicShape::Line { from: p(0, 0), to: p(MM, 0) },
            GraphicShape::Circle { center: p(5 * MM, 0), radius: Nm(MM) },
            GraphicShape::Polygon { outline: Contour::polyline(&[p(0, 0), p(MM, 0), p(0, MM)]).expect("ring"), filled: false },
            GraphicShape::Polygon { outline: Contour::polyline(&[p(0, 0), p(MM, 0), p(0, MM)]).expect("ring"), filled: true },
        ];
        for (i, shape) in shapes_of.into_iter().enumerate() {
            b.graphics.insert(Graphic { id: ItemId::from_u128(20 + i as u128), layer: TOP_SILK, width: Nm(0), shape }).expect("room");
        }
        let list = display_list(&b, &Options::all(&b));
        let silk = shapes(&list, TOP_SILK);
        assert_eq!(silk.len(), 4);
        assert_eq!(silk.first(), Some(&Shape::Stroke { points: vec![p(0, 0), p(MM, 0)], width: HAIRLINE }), "a line as a hairline");
        assert_eq!(silk.get(1), Some(&Shape::Disc { center: p(5 * MM, 0), radius: Nm(MM) }), "a circle as a disc");
        assert!(matches!(silk.get(2), Some(Shape::Stroke { width, points }) if *width == HAIRLINE && points.len() == 4), "an outline as a hairline");
        assert!(matches!(silk.get(3), Some(Shape::Area(_))), "a filled area without an outline");
        assert!(silk.iter().all(|s| !matches!(s, Shape::Stroke { width, .. } if width.0 == 0)), "no line is drawn zero wide");
    }

    #[test]
    fn hostile_items_are_left_out_not_a_crash() {
        let mut b = Board::new("x");
        let oval = Pad::smd("2", p(0, 0), PadShape::Oval { width: Nm(i64::MAX), height: Nm(-5) });
        b.footprints.insert(Footprint { id: ItemId::from_u128(5), ..footprint(Side::Top, vec![oval]) }).expect("room");
        let mut f = footprint(Side::Bottom, vec![Pad::smd("1", p(i64::MAX, 0), rect(MM, MM))]);
        f.position = p(i64::MAX, i64::MIN);
        f.rotation = Rotation(45_000);
        b.footprints.insert(f).expect("room");
        b.texts
            .insert(Text {
                id: ItemId::from_u128(9),
                layer: TOP_SILK,
                text: "W".repeat(1_000),
                position: p(i64::MAX, 0),
                height: Nm(i64::MAX),
                stroke: Nm(i64::MAX),
                rotation: Rotation(12_345),
                mirrored: true,
                justify: Justify::Right,
            })
            .expect("room");
        let list = display_list(&b, &Options { tolerance: Nm(0), ..Options::all(&b) });
        let _ = list.bounds;
        let _ = crate::svg(&list);
    }
}
