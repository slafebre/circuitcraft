//! What the readers of other programs' board files share: the result of an import, the notes
//! about what could not be kept, ids, layer stacks and the geometry that source files leave to
//! the reader.
//!
//! An import is read-only and lossy by design: a reader keeps what the model can hold, says in
//! [`Notes`] what it left out or approximated, and never guesses silently. The board it returns
//! has passed [`Board::validate`].

use circuitcraft_geom::{Arc, Contour, Edge};
use circuitcraft_model::{
    Board, Footprint, GraphicShape, ItemId, Layer, LayerId, LayerKind, LayerStack, MAX_LAYERS, MAX_OUTLINE_GAP, ModelError, NetId, Rotation, Side,
};
use circuitcraft_units::{Nm, Point};
use std::collections::BTreeMap;

/// A board read from another program's file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    /// The board; it passes [`Board::validate`].
    pub board: Board,
    /// What the reader left out or approximated.
    pub notes: Notes,
}

/// What a reader left out or approximated, each kind of thing once, with how often it happened.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Notes(BTreeMap<String, usize>);

/// The note for text a file anchors at its top, middle or bottom: the model holds how text is
/// anchored from side to side only, so such text keeps its anchor point and loses that.
pub(crate) const RAISED_TEXT: &str = "text anchored at a height of its own: the anchoring from top to bottom is left out";

/// The most kinds of note kept; a hostile file can't grow the list without limit.
pub const MAX_NOTES: usize = 200;

impl Notes {
    /// Records one more of `what`.
    pub fn add(&mut self, what: &str) {
        let what = if self.0.len() >= MAX_NOTES && !self.0.contains_key(what) { "more kinds of things than are listed here" } else { what };
        let count = self.0.entry(what.chars().take(200).collect()).or_default();
        *count = count.saturating_add(1);
    }

    /// Whether nothing was left out or approximated.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Each note with how often it happened, in alphabetical order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, usize)> {
        self.0.iter().map(|(what, &count)| (what.as_str(), count))
    }

    /// The notes as sentences: `"3 × dimension left out"`.
    pub fn lines(&self) -> Vec<String> {
        self.iter().map(|(what, count)| if count == 1 { what.to_owned() } else { format!("{count} × {what}") }).collect()
    }
}

/// A file could not be imported.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ImportError {
    /// The text is not well formed.
    #[error("line {line}: {message}")]
    Syntax {
        /// The line the problem is on, counted from 1.
        line: usize,
        /// What is wrong.
        message: String,
    },
    /// The file is well formed but is not the kind of file this reader reads.
    #[error("not {0}")]
    WrongFormat(&'static str),
    /// The file says something this reader can't make a board from.
    #[error("{0}")]
    Unsupported(String),
    /// The board that came out is not one the model accepts.
    #[error("the file doesn't make a valid board: {0}")]
    Invalid(#[from] ModelError),
}

impl ImportError {
    pub(crate) fn unsupported(message: impl Into<String>) -> ImportError {
        ImportError::Unsupported(message.into())
    }
}

/// Ids for the items of an imported board. They follow from the file's text and the order of
/// its items, so importing the same file twice gives the same board.
#[derive(Debug)]
pub(crate) struct Ids {
    seed: u64,
    count: u64,
}

impl Ids {
    pub(crate) fn new(text: &str) -> Ids {
        // FNV-1a over the text: a different file gives different ids.
        let seed = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3));
        Ids { seed, count: 0 }
    }

    /// The next 128 bits: two rounds of SplitMix64 over the seed and a counter, shaped like the
    /// random (version 4) UUIDs the model hands out itself. Never zero.
    fn next_bits(&mut self) -> u128 {
        let mut mix = |salt: u64| {
            self.count = self.count.wrapping_add(1);
            let mut z = self.seed.wrapping_add(self.count.wrapping_mul(0x9e37_79b9_7f4a_7c15)).wrapping_add(salt);
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        };
        let bits = (u128::from(mix(1)) << 64) | u128::from(mix(2));
        // Version 4, variant 1: the bits a UUID reserves, which also keep the id from being zero.
        (bits & !(0xf000_u128 << 64) & !(0xc000_u128 << 48)) | (0x4000_u128 << 64) | (0x8000_u128 << 48)
    }

    pub(crate) fn item(&mut self) -> ItemId {
        ItemId::from_u128(self.next_bits())
    }

    pub(crate) fn net(&mut self) -> NetId {
        NetId::from_u128(self.next_bits())
    }
}

/// An angle in degrees from a file, counter-clockwise, within one turn.
pub(crate) fn degrees(value: f64) -> Result<Rotation, ImportError> {
    if !value.is_finite() || value.abs() > 1.0e9 {
        return Err(ImportError::unsupported(format!("an angle of {value} degrees")));
    }
    Ok(Rotation::from_millidegrees((value * 1_000.0).round() as i64))
}

/// The layers of an imported board, built up in the order layer lists show them.
#[derive(Debug, Default)]
pub(crate) struct Layers {
    layers: Vec<Layer>,
    /// The names taken so far.
    names: std::collections::BTreeSet<String>,
}

/// The layers every imported board gets, whether or not its file names them: a board needs both
/// sides of each to be valid, and footprints flip between them.
#[derive(Debug, Clone)]
pub(crate) struct Standard {
    /// Copper layers, top to bottom.
    pub(crate) copper: Vec<LayerId>,
    /// Top and bottom, in that order.
    pub(crate) paste: [LayerId; 2],
    pub(crate) silk: [LayerId; 2],
    pub(crate) mask: [LayerId; 2],
    pub(crate) courtyard: [LayerId; 2],
    pub(crate) fabrication: [LayerId; 2],
    pub(crate) outline: LayerId,
}

impl Layers {
    /// Adds a layer and returns its id. A name that is taken or blank gets a number after it.
    /// Callers make sure there is room: the stack never grows past [`MAX_LAYERS`], so ids fit
    /// and finding a free name stays cheap.
    fn push(&mut self, name: &str, kind: LayerKind, thickness: Option<Nm>, side: Option<Side>, pair: Option<LayerId>) -> LayerId {
        let id = LayerId(u16::try_from(self.layers.len().saturating_add(1)).unwrap_or(u16::MAX));
        let base: String = if name.trim().is_empty() { "Layer".to_owned() } else { name.chars().take(200).collect() };
        let mut unique = base.clone();
        let mut n = 2_usize;
        while self.names.contains(&unique) {
            unique = format!("{base} {n}");
            n = n.saturating_add(1);
        }
        self.names.insert(unique.clone());
        self.layers.push(Layer { id, name: unique, kind, thickness, side, pair });
        id
    }

    /// Whether `more` layers still fit in a stack.
    fn room_for(&self, more: usize) -> bool {
        self.layers.len().saturating_add(more) <= MAX_LAYERS
    }

    /// A layer on neither side; `None` when the stack is full.
    pub(crate) fn single(&mut self, name: &str, kind: LayerKind) -> Option<LayerId> {
        self.room_for(1).then(|| self.push(name, kind, None, None, None))
    }

    /// A layer on the top and its counterpart on the bottom, named `Top …` and `Bottom …`;
    /// `None` when the stack has no room for both.
    pub(crate) fn pair(&mut self, name: &str, kind: LayerKind) -> Option<[LayerId; 2]> {
        self.room_for(2).then(|| self.sides(name, kind))
    }

    fn sides(&mut self, name: &str, kind: LayerKind) -> [LayerId; 2] {
        let top = self.push(&format!("Top {name}"), kind, None, Some(Side::Top), None);
        let bottom = self.push(&format!("Bottom {name}"), kind, None, Some(Side::Bottom), Some(top));
        if let Some(layer) = self.layers.iter_mut().find(|l| l.id == top) {
            layer.pair = Some(bottom);
        }
        [top, bottom]
    }

    /// The layers every board gets, in the order a new board lists them, with `copper` copper
    /// layers (at least one, at most 64) spread over a board `thickness` thick. Called first,
    /// on an empty stack, they always fit.
    pub(crate) fn standard(&mut self, copper: usize, thickness: Nm) -> Standard {
        const COPPER: i64 = 35_000;
        const MASK: i64 = 10_000;
        let copper = copper.clamp(1, 64);
        let top_paste = self.push("Top Paste", LayerKind::Paste, None, Some(Side::Top), None);
        let top_silk = self.push("Top Silkscreen", LayerKind::Silkscreen, None, Some(Side::Top), None);
        let top_mask = self.push("Top Solder Mask", LayerKind::SolderMask, Some(Nm(MASK)), Some(Side::Top), None);
        // What is left of the thickness after copper and mask, shared by the layers between.
        let gaps = i64::try_from(copper.saturating_sub(1).max(1)).unwrap_or(1);
        let layers = i64::try_from(copper).unwrap_or(1);
        let between = Nm((thickness.0.saturating_sub(layers * COPPER + 2 * MASK) / gaps).max(1_000));
        let mut ids = Vec::with_capacity(copper);
        for i in 0..copper {
            let name = match i {
                0 => "Top Copper".to_owned(),
                _ if i + 1 == copper => "Bottom Copper".to_owned(),
                _ => format!("Inner Copper {i}"),
            };
            ids.push(self.push(&name, LayerKind::Copper, Some(Nm(COPPER)), None, None));
            if i + 1 < copper {
                self.push(&format!("Dielectric {}", i + 1), LayerKind::Dielectric, Some(between), None, None);
            }
        }
        let bottom_mask = self.push("Bottom Solder Mask", LayerKind::SolderMask, Some(Nm(MASK)), Some(Side::Bottom), Some(top_mask));
        let bottom_silk = self.push("Bottom Silkscreen", LayerKind::Silkscreen, None, Some(Side::Bottom), Some(top_silk));
        let bottom_paste = self.push("Bottom Paste", LayerKind::Paste, None, Some(Side::Bottom), Some(top_paste));
        for (top, bottom) in [(top_paste, bottom_paste), (top_silk, bottom_silk), (top_mask, bottom_mask)] {
            if let Some(layer) = self.layers.iter_mut().find(|l| l.id == top) {
                layer.pair = Some(bottom);
            }
        }
        let outline = self.push("Outline", LayerKind::Outline, None, None, None);
        let courtyard = self.sides("Courtyard", LayerKind::Courtyard);
        let fabrication = self.sides("Fabrication", LayerKind::Fabrication);
        Standard {
            copper: ids,
            paste: [top_paste, bottom_paste],
            silk: [top_silk, bottom_silk],
            mask: [top_mask, bottom_mask],
            courtyard,
            fabrication,
            outline,
        }
    }

    /// The layer stack.
    pub(crate) fn finish(self) -> LayerStack {
        LayerStack { layers: self.layers }
    }
}

/// The point halfway along the arc from `a` to `b` that turns through `sweep` degrees,
/// counter-clockwise when positive: how EAGLE writes a curved wire. `None` when the points
/// coincide or the sweep is no arc (zero, or a full turn or more).
pub(crate) fn chord_arc(a: Point, b: Point, sweep: f64) -> Option<Arc> {
    if a == b || !sweep.is_finite() || sweep == 0.0 || sweep.abs() >= 360.0 {
        return None;
    }
    // The middle of the arc lies off the middle of the chord by the sagitta, to the right of the
    // direction of travel for a counter-clockwise arc.
    let (dx, dy) = (b.x.0 as f64 - a.x.0 as f64, b.y.0 as f64 - a.y.0 as f64);
    let bulge = (sweep.to_radians() / 4.0).tan() / 2.0;
    let x = (a.x.0 as f64 + b.x.0 as f64) / 2.0 + bulge * dy;
    let y = (a.y.0 as f64 + b.y.0 as f64) / 2.0 - bulge * dx;
    let mid = Point::new(Nm::from_f64_nm(x).ok()?, Nm::from_f64_nm(y).ok()?);
    (mid != a && mid != b).then_some(Arc::new(a, mid, b))
}

/// The arc that starts at `start` and turns about `center` through `sweep` degrees,
/// counter-clockwise when positive: how KiCad 5 writes an arc. A full turn or more is a circle.
/// `None` when `start` is the centre or the sweep is zero.
pub(crate) fn center_arc(center: Point, start: Point, sweep: f64) -> Option<Arc> {
    if center == start || !sweep.is_finite() || sweep == 0.0 {
        return None;
    }
    let (cx, cy) = (center.x.0 as f64, center.y.0 as f64);
    let (vx, vy) = (start.x.0 as f64 - cx, start.y.0 as f64 - cy);
    let at = |angle: f64| {
        let (sin, cos) = angle.to_radians().sin_cos();
        Some(Point::new(Nm::from_f64_nm(cx + vx * cos - vy * sin).ok()?, Nm::from_f64_nm(cy + vx * sin + vy * cos).ok()?))
    };
    if sweep.abs() >= 360.0 {
        return Some(Arc::new(start, at(180.0)?, start));
    }
    let (mid, end) = (at(sweep / 2.0)?, at(sweep)?);
    (mid != start && mid != end && end != start).then_some(Arc::new(start, mid, end))
}

/// `p`, if it is within the design range: what an importer works out (a corner turned about a
/// centre, a footprint's drawing put on the board) may leave it though what the file says is
/// inside, and such a thing is left out, not allowed to make the board invalid.
pub(crate) fn in_range(p: Point) -> Option<Point> {
    let max = circuitcraft_units::MAX_ABS.unsigned_abs();
    (p.x.0.unsigned_abs() <= max && p.y.0.unsigned_abs() <= max).then_some(p)
}

/// A size an importer worked out, if it is positive and within the design range.
pub(crate) fn size(value: Nm) -> Option<Nm> {
    (1..=circuitcraft_units::MAX_ABS).contains(&value.0).then_some(value)
}

/// A closed outline through `points`, or `None` when fewer than three differ. A last point that
/// repeats the first is dropped: the outline closes by itself.
pub(crate) fn polygon(mut points: Vec<Point>) -> Option<Contour> {
    points.dedup();
    if points.len() > 1 && points.first() == points.last() {
        points.pop();
    }
    if points.len() < 3 { None } else { Contour::polyline(&points) }
}

/// A rectangle `width` by `height` around `center`, turned by `rotation` about its centre.
pub(crate) fn rectangle(center: Point, width: Nm, height: Nm, rotation: Rotation) -> Option<Contour> {
    let (w, h) = (width.0 / 2, height.0 / 2);
    let corner = |x: i64, y: i64| {
        let p = rotation.turn(Point::new(Nm(x), Nm(y)))?;
        in_range(Point::new(Nm(p.x.0.checked_add(center.x.0)?), Nm(p.y.0.checked_add(center.y.0)?)))
    };
    polygon(vec![corner(-w, -h)?, corner(w, -h)?, corner(w, h)?, corner(-w, h)?])
}

/// A circle as a closed outline, or `None` when it has no radius or leaves the design range.
pub(crate) fn disc(center: Point, radius: Nm) -> Option<Contour> {
    if radius.0 <= 0 { None } else { Contour::circle(center, radius).ok() }
}

/// A cubic Bézier curve from `p[0]` to `p[3]` as the points of sixteen straight pieces, both ends
/// exact. The model holds lines and arcs; a curve becomes a run of short lines.
pub(crate) fn bezier(p: [Point; 4]) -> Vec<Point> {
    const PIECES: u32 = 16;
    let coordinate = |t: f64, of: fn(Point) -> i64| {
        let [a, b, c, d] = p.map(|q| of(q) as f64);
        let u = 1.0 - t;
        u * u * u * a + 3.0 * u * u * t * b + 3.0 * u * t * t * c + t * t * t * d
    };
    let mut points = vec![p[0]];
    for i in 1..PIECES {
        let t = f64::from(i) / f64::from(PIECES);
        if let (Ok(x), Ok(y)) = (Nm::from_f64_nm(coordinate(t, |q| q.x.0)), Nm::from_f64_nm(coordinate(t, |q| q.y.0))) {
            points.push(Point::new(x, y));
        }
    }
    points.push(p[3]);
    points.dedup();
    points
}

/// The contour `outline`, each of its points moved by `place`. `None` when a point can't be placed.
pub(crate) fn placed(outline: &Contour, place: impl Fn(Point) -> Option<Point>) -> Option<Contour> {
    let edges = outline
        .edges
        .iter()
        .map(|edge| match *edge {
            Edge::Line { to } => Some(Edge::Line { to: place(to)? }),
            Edge::Arc { mid, to } => Some(Edge::Arc { mid: place(mid)?, to: place(to)? }),
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Contour { start: place(outline.start)?, edges })
}

/// Moves a footprint's drawing to where it is on the board; `None` when that is outside the
/// design range.
pub(crate) fn lift(footprint: &Footprint, shape: GraphicShape) -> Option<GraphicShape> {
    let place = |p: Point| footprint.to_board(p).and_then(in_range);
    Some(match shape {
        GraphicShape::Line { from, to } => GraphicShape::Line { from: place(from)?, to: place(to)? },
        GraphicShape::Arc { arc } => GraphicShape::Arc { arc: Arc::new(place(arc.start)?, place(arc.mid)?, place(arc.end)?) },
        GraphicShape::Circle { center, radius } => GraphicShape::Circle { center: place(center)?, radius },
        GraphicShape::Polygon { outline, filled } => GraphicShape::Polygon { outline: placed(&outline, place)?, filled },
    })
}

/// The lines, arcs and circles on the outline layer that are drawn exactly where an earlier one
/// is, in either direction.
fn repeated_outline_drawings(board: &Board) -> Vec<ItemId> {
    let xy = |p: Point| (p.x.0, p.y.0);
    let mut seen = std::collections::BTreeSet::new();
    let mut repeated = Vec::new();
    for g in board.graphics.iter().filter(|g| board.stack.get(g.layer).is_some_and(|l| l.kind == LayerKind::Outline)) {
        // A line or an arc is the same drawn from either end.
        let key = match &g.shape {
            GraphicShape::Line { from, to } => (0_u8, xy(*from).min(xy(*to)), xy(*from).max(xy(*to)), (0, 0)),
            GraphicShape::Arc { arc } => (1, xy(arc.start).min(xy(arc.end)), xy(arc.start).max(xy(arc.end)), xy(arc.mid)),
            GraphicShape::Circle { center, radius } => (2, xy(*center), (radius.0, 0), (0, 0)),
            GraphicShape::Polygon { .. } => continue,
        };
        if !seen.insert(key) {
            repeated.push(g.id);
        }
    }
    repeated
}

/// Turns the drawings on the outline layer into the board outline when they close, checks the
/// board, and returns it with the notes.
pub(crate) fn finish(mut board: Board, mut notes: Notes) -> Result<Imported, ImportError> {
    let on_outline = |b: &Board| b.graphics.iter().any(|g| b.stack.get(g.layer).is_some_and(|l| l.kind == LayerKind::Outline));
    if on_outline(&board) {
        // The same line drawn twice makes three ends meet where two should, and no outline
        // closes through such a corner. Real outlines have them; the copy says nothing new.
        for id in repeated_outline_drawings(&board) {
            board.graphics.remove(id);
            notes.add("drawing on the outline layer that repeats another left out");
        }
        // A micrometre closes the rounding of drawings that were meant to meet; a tenth of a
        // millimetre closes the gaps people leave when they draw an outline by eye.
        let mut refused = None;
        for gap in [Nm(1_000), Nm(MAX_OUTLINE_GAP.0 / 10)] {
            refused = board.assemble_outline(gap).err();
            if refused.is_none() {
                break;
            }
        }
        // The note says why, in the model's words: drawings that don't close, a shape outside
        // the outline, or more of them than can be worked through.
        match refused {
            Some(ModelError::BadOutline(why)) => notes.add(&format!("board outline left as drawings: {why}")),
            Some(other) => notes.add(&format!("board outline left as drawings: {other}")),
            None => {}
        }
    }
    board.validate()?;
    Ok(Imported { board, notes })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: i64, y: i64) -> Point {
        Point::new(Nm(x), Nm(y))
    }

    #[test]
    fn ids_follow_from_the_text_and_never_repeat() {
        let mut a = Ids::new("(kicad_pcb)");
        let mut b = Ids::new("(kicad_pcb)");
        let mut other = Ids::new("(kicad_pcb )");
        let first: Vec<ItemId> = (0..10_000).map(|_| a.item()).collect();
        assert!(first.iter().all(|id| !id.is_nil()));
        assert_eq!(first.iter().collect::<std::collections::BTreeSet<_>>().len(), 10_000, "no id twice");
        assert_eq!((0..10_000).map(|_| b.item()).collect::<Vec<_>>(), first, "the same text gives the same ids");
        assert_ne!(other.item(), first[0], "another text gives other ids");
        // Shaped like the model's own ids: a version 4 UUID.
        let text = first[0].to_string();
        assert_eq!((text.len(), text.chars().nth(14)), (36, Some('4')));
        assert!(matches!(text.chars().nth(19), Some('8' | '9' | 'a' | 'b')));
        assert_ne!(a.net().as_u128(), a.item().as_u128());
    }

    #[test]
    fn notes_count_each_kind_and_stay_bounded() {
        let mut notes = Notes::default();
        assert!(notes.is_empty());
        notes.add("dimension left out");
        notes.add("dimension left out");
        notes.add("image left out");
        assert_eq!(notes.lines(), ["2 × dimension left out", "image left out"]);
        for i in 0..10_000 {
            notes.add(&format!("thing {i}"));
        }
        assert_eq!(notes.iter().count(), MAX_NOTES + 1, "the rest is counted under one note");
        assert!(notes.iter().any(|(what, count)| what.starts_with("more kinds") && count > 9_000));
        notes.add(&"x".repeat(100_000));
        assert!(notes.iter().all(|(what, _)| what.len() <= 200));
    }

    #[test]
    fn the_standard_layers_make_a_valid_stack() {
        for copper in [0, 1, 2, 4, 16, 1_000] {
            let mut layers = Layers::default();
            let standard = layers.standard(copper, Nm(1_600_000));
            assert!(layers.pair("Adhesive", LayerKind::Mechanical).is_some());
            assert!(layers.single("Outline", LayerKind::Mechanical).is_some());
            assert!(layers.single("  ", LayerKind::Mechanical).is_some());
            let stack = layers.finish();
            assert_eq!(stack.validate(), Ok(()), "{copper} copper layers");
            assert_eq!(stack.copper_count(), copper.clamp(1, 64));
            assert_eq!(stack.copper().map(|l| l.id).collect::<Vec<_>>(), standard.copper);
            assert_eq!(stack.flipped(standard.silk[0]), Some(standard.silk[1]));
            assert_eq!(stack.get(standard.outline).map(|l| l.kind), Some(LayerKind::Outline));
            assert!(stack.by_name("Outline 2").is_some() && stack.by_name("Layer").is_some(), "names stay unique and never blank");
        }
        // Two copper layers on a 1.6 mm board come out as thick as the board.
        let mut layers = Layers::default();
        layers.standard(2, Nm(1_600_000));
        assert_eq!(layers.finish().thickness(), Nm(1_600_000));
    }

    #[test]
    fn a_stack_takes_layers_until_it_is_full_and_no_longer() {
        let mut layers = Layers::default();
        layers.standard(2, Nm(1_600_000));
        // Thousands of layers of one name: each gets a number, quickly, until the stack is full.
        let start = std::time::Instant::now();
        let added = (0..50_000).filter(|_| layers.single("Same", LayerKind::Mechanical).is_some()).count();
        assert!(start.elapsed() < std::time::Duration::from_secs(5), "{:?}", start.elapsed());
        assert_eq!(added, MAX_LAYERS - 14, "what the fourteen standard layers leave");
        assert!(layers.pair("More", LayerKind::Mechanical).is_none() && layers.single("More", LayerKind::Mechanical).is_none());
        let stack = layers.finish();
        assert_eq!((stack.layers.len(), stack.validate()), (MAX_LAYERS, Ok(())));
        assert!(stack.by_name("Same").is_some() && stack.by_name("Same 2").is_some() && stack.by_name("More").is_none());
        // A pair needs room for both of its layers.
        let mut layers = Layers::default();
        layers.standard(2, Nm(1_600_000));
        for _ in 0..MAX_LAYERS - 15 {
            let _ = layers.single("x", LayerKind::Mechanical);
        }
        assert!(layers.pair("Glue", LayerKind::Mechanical).is_none() && layers.single("y", LayerKind::Mechanical).is_some());
    }

    /// A board with `shapes` drawn on its outline layer, finished as an import is.
    fn outlined(shapes: Vec<GraphicShape>) -> Imported {
        let mut board = Board::new("x");
        for (n, shape) in shapes.into_iter().enumerate() {
            let graphic = circuitcraft_model::Graphic {
                id: ItemId::from_u128(n as u128 + 1),
                layer: circuitcraft_model::layer_ids::OUTLINE,
                width: Nm(0),
                shape,
            };
            board.graphics.insert(graphic).expect("room");
        }
        finish(board, Notes::default()).expect("a valid board")
    }

    #[test]
    fn an_outline_closes_over_repeated_drawings_and_small_gaps() {
        let line = |a: Point, b: Point| GraphicShape::Line { from: a, to: b };
        let bulge = Arc::new(p(0, 1_000_000), p(-200_000, 500_000), p(0, 0));
        let sides =
            |last: Point| vec![line(p(0, 0), p(1_000_000, 0)), line(p(1_000_000, 0), p(1_000_000, 1_000_000)), line(p(1_000_000, 1_000_000), last)];
        // Three lines and an arc, with the arc drawn again the other way round and a line again.
        let mut shapes = sides(p(0, 1_000_000));
        shapes.push(GraphicShape::Arc { arc: bulge });
        shapes.push(GraphicShape::Arc { arc: Arc::new(bulge.end, bulge.mid, bulge.start) });
        shapes.push(line(p(1_000_000, 0), p(0, 0)));
        let read = outlined(shapes);
        assert_eq!(read.notes.lines(), ["2 × drawing on the outline layer that repeats another left out"]);
        assert!(read.board.outline.as_ref().is_some_and(|o| o.edges.len() == 4) && read.board.graphics.is_empty());
        // An arc with another middle is another arc: nothing repeats, and three ends meet.
        let mut shapes = sides(p(0, 1_000_000));
        shapes.push(GraphicShape::Arc { arc: bulge });
        shapes.push(GraphicShape::Arc { arc: Arc::new(bulge.start, p(-300_000, 500_000), bulge.end) });
        let read = outlined(shapes);
        assert_eq!(
            read.notes.lines(),
            ["board outline left as drawings: a drawing on the outline layer is part of no closed shape; close it, allow a wider gap or remove it"]
        );
        assert_eq!((read.board.outline.is_some(), read.board.graphics.len()), (false, 5));
        // A corner drawn 0.05 mm short closes, though not within a micrometre; 0.2 mm short does not.
        let closing = |short: i64| {
            let mut shapes = sides(p(short, 1_000_000));
            shapes.push(line(p(0, 1_000_000), p(0, 0)));
            outlined(shapes).board.outline.is_some()
        };
        assert_eq!((closing(0), closing(50_000), closing(200_000)), (true, true, false));
    }

    #[test]
    fn what_an_import_works_out_stays_in_the_design_range() {
        let max = circuitcraft_units::MAX_ABS;
        assert_eq!(in_range(p(max, -max)), Some(p(max, -max)));
        assert!(in_range(p(max + 1, 0)).is_none() && in_range(p(0, i64::MIN)).is_none());
        assert_eq!([size(Nm(1)), size(Nm(max)), size(Nm(0)), size(Nm(-5)), size(Nm(max + 1))], [Some(Nm(1)), Some(Nm(max)), None, None, None]);
        // A rectangle whose corners turn out of range, though its centre and sides are inside.
        assert!(rectangle(p(max - 10, 0), Nm(100), Nm(100), Rotation(45_000)).is_none());
        assert!(rectangle(p(max - 100, 0), Nm(100), Nm(100), Rotation(0)).is_some());
    }

    #[test]
    fn arcs_from_a_chord_and_from_a_centre() {
        // A half circle from (10, 0) to (-10, 0), counter-clockwise: over the top.
        assert_eq!(chord_arc(p(10, 0), p(-10, 0), 180.0), Some(Arc::new(p(10, 0), p(0, 10), p(-10, 0))));
        assert_eq!(chord_arc(p(10, 0), p(-10, 0), -180.0), Some(Arc::new(p(10, 0), p(0, -10), p(-10, 0))));
        // A quarter circle of radius 1000 about the origin.
        assert_eq!(chord_arc(p(1_000, 0), p(0, 1_000), 90.0), Some(Arc::new(p(1_000, 0), p(707, 707), p(0, 1_000))));
        for no_arc in [0.0, 360.0, -400.0, f64::NAN, f64::INFINITY] {
            assert_eq!(chord_arc(p(0, 0), p(10, 0), no_arc), None, "{no_arc}");
        }
        assert_eq!(chord_arc(p(5, 5), p(5, 5), 90.0), None);
        // The same quarter circle from its centre and start; a full turn is a circle.
        assert_eq!(center_arc(p(0, 0), p(1_000, 0), 90.0), Some(Arc::new(p(1_000, 0), p(707, 707), p(0, 1_000))));
        assert_eq!(center_arc(p(0, 0), p(1_000, 0), -90.0), Some(Arc::new(p(1_000, 0), p(707, -707), p(0, -1_000))));
        assert_eq!(center_arc(p(0, 0), p(1_000, 0), 360.0), Some(Arc::new(p(1_000, 0), p(-1_000, 0), p(1_000, 0))));
        assert_eq!(center_arc(p(0, 0), p(0, 0), 90.0), None);
        assert_eq!(center_arc(p(0, 0), p(1_000, 0), 0.0), None);
        assert_eq!(center_arc(p(0, 0), p(i64::MAX, 0), 90.0), None, "out of range is no arc, not a crash");
    }

    #[test]
    fn shapes_that_enclose_nothing_are_no_shapes() {
        assert_eq!(polygon(vec![p(0, 0), p(10, 0), p(10, 0), p(0, 10), p(0, 0)]).map(|c| c.edges.len()), Some(2));
        assert_eq!(polygon(vec![p(0, 0), p(10, 0), p(0, 0)]), None);
        assert_eq!(polygon(Vec::new()), None);
        let square = rectangle(p(100, 100), Nm(20), Nm(10), Rotation(90_000)).expect("rectangle");
        assert_eq!(square.flatten(Nm(1)).ok(), Some(vec![p(105, 90), p(105, 110), p(95, 110), p(95, 90)]));
        assert_eq!(rectangle(p(0, 0), Nm(0), Nm(10), Rotation(0)), None);
        assert_eq!(rectangle(p(i64::MAX, 0), Nm(20), Nm(10), Rotation(0)), None);
        assert!(disc(p(0, 0), Nm(5)).is_some());
        assert!(disc(p(0, 0), Nm(0)).is_none() && disc(p(0, 0), Nm(-5)).is_none() && disc(p(0, 0), Nm(i64::MAX)).is_none());
    }

    #[test]
    fn a_curve_becomes_short_lines_between_its_exact_ends() {
        let points = bezier([p(0, 0), p(0, 1_000), p(1_000, 1_000), p(1_000, 0)]);
        assert_eq!((points.first(), points.last(), points.len()), (Some(&p(0, 0)), Some(&p(1_000, 0)), 17));
        assert_eq!(points.get(8), Some(&p(500, 750)), "the middle of the curve");
        assert_eq!(bezier([p(7, 7); 4]), [p(7, 7)]);
        assert_eq!(bezier([p(i64::MIN, 0), p(i64::MAX, 0), p(0, i64::MAX), p(1, 1)]).last(), Some(&p(1, 1)));
    }
}
