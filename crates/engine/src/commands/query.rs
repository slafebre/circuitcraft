//! Looking inside a board: `board.query` finds items and `item.info` describes one in full.
//!
//! Both answer in JSON with lengths in millimetres and ids as the UUID strings other commands
//! take. Boards hold up to a million items of each kind, so neither reply grows with the board:
//! `board.query` returns a page of at most [`MAX_LIMIT`] items, and both stop adding to a reply
//! once it holds [`MAX_REPLY_BYTES`] of JSON and say where they stopped.

use super::{bad, layer, mm, net};
use crate::{EngineError, Session};
use circuitcraft_geom::{Arc, Contour, Edge, Rect};
use circuitcraft_model::{
    Board, Footprint, Graphic, GraphicShape, Hole, ItemId, Justify, LayerId, LayerStack, NetId, Opening, Pad, PadShape, Rotation, Side, TrackPath,
    text_point,
};
use circuitcraft_units::{Nm, Point};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::f64::consts::{FRAC_PI_2, TAU};

/// The most items one `board.query` returns.
pub(super) const MAX_LIMIT: u32 = 1_000;

/// How many items `board.query` returns when no limit is given.
const DEFAULT_LIMIT: u32 = 100;

/// The most JSON a reply carries in its lists, in bytes. MCP sends a reply twice (as text and as
/// structured content) with the text escaped, so a reply stays a few megabytes, well within what
/// clients read and what `circuitcraft-mcp` accepts in one message (16 MiB).
pub(super) const MAX_REPLY_BYTES: usize = 1 << 20;

/// A kind of thing on a board, as `board.query` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(super) enum Kind {
    /// A net.
    Net,
    /// A placed footprint.
    Footprint,
    /// A pad of a footprint.
    Pad,
    /// A copper track.
    Track,
    /// A via.
    Via,
    /// A copper zone.
    Zone,
    /// A keepout area.
    Keepout,
    /// A hole that belongs to no footprint.
    Hole,
    /// Free text.
    Text,
    /// A free drawing.
    Drawing,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct BoardQuery {
    /// The kinds of item to list: "net", "footprint", "pad", "track", "via", "zone", "keepout",
    /// "hole", "text" or "drawing"; every kind when left out.
    kinds: Option<Vec<Kind>>,
    /// Only what is on this net, by name: the net itself, its tracks, vias, zones and pads, and
    /// the footprints with a pad on it.
    net: Option<String>,
    /// Only what is on this layer, by name: tracks, zones, keepouts, texts and drawings on it,
    /// vias and pads with copper on it, and footprints mounted on its side of the board.
    layer: Option<String>,
    /// Only the footprint with this reference (exactly, such as "U1") and its pads.
    reference: Option<String>,
    /// Only what touches this rectangle, given by two opposite corners [[x1, y1], [x2, y2]] in
    /// millimetres (the same corner twice finds what covers a point). A straight track or line
    /// covers its stroke, a via or hole its copper or drill, a zone or keepout the area inside its
    /// outline (and outside a zone's holes); any
    /// other item the box around its copper or lines, widths included, and a footprint the box
    /// around its pads and drawings. Nets cover nothing.
    area: Option<[[f64; 2]; 2]>,
    /// The most items to return, 1 to 1000; 100 when left out. A reply also stops before its
    /// items pass 1 MiB of JSON.
    limit: Option<u32>,
    /// How many matching items to skip, for the next page; 0 when left out.
    offset: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ItemInfo {
    /// The id of an item or a net, a UUID, as board.query gives it.
    id: String,
}

/// What `board.query` is asked for, resolved against the board.
struct Filter {
    kinds: Vec<Kind>,
    net: Option<NetId>,
    layer: Option<LayerId>,
    reference: Option<String>,
    area: Option<Rect>,
}

impl Filter {
    fn wants(&self, kind: Kind) -> bool {
        self.kinds.contains(&kind)
    }

    /// Whether an item on `net` passes the net filter.
    fn net(&self, net: Option<NetId>) -> bool {
        self.net.is_none_or(|wanted| net == Some(wanted))
    }

    /// Whether an item on `layer` passes the layer filter.
    fn layer(&self, layer: LayerId) -> bool {
        self.layer.is_none_or(|wanted| layer == wanted)
    }

    /// Whether an item outside any footprint passes the reference filter: only when there is none.
    fn loose(&self) -> bool {
        self.reference.is_none()
    }

    /// Whether an item passes the area filter: its box `extent` touches the rectangle, and so does
    /// its own shape, as `exact` says (`|_| true` for an item matched by its box). One that covers
    /// nothing doesn't.
    fn area(&self, extent: impl FnOnce() -> Option<Rect>, exact: impl FnOnce(Rect) -> bool) -> bool {
        self.area.is_none_or(|area| extent().is_some_and(|e| e.overlaps(area)) && exact(area))
    }
}

/// For an item the area filter matches by its box alone.
fn by_box(_: Rect) -> bool {
    true
}

/// One page of matches: those from `offset` on, up to `limit` and the byte budget, and how many
/// there are in all.
struct Page {
    offset: u64,
    limit: usize,
    budget: Budget,
    total: u64,
    items: Vec<Value>,
    /// Whether the byte budget ran out before the limit was reached.
    full: bool,
}

impl Page {
    /// Counts a match and keeps it if it belongs on this page; `make` runs only then.
    fn offer(&mut self, make: impl FnOnce() -> Value) {
        let index = self.total;
        self.total = self.total.saturating_add(1);
        if index < self.offset || self.items.len() >= self.limit || self.full {
            return;
        }
        let item = make();
        // Every page holds at least one item, so paging always moves on.
        if self.budget.take(&item) || self.items.is_empty() {
            self.items.push(item);
        } else {
            self.full = true;
        }
    }
}

/// What is left of the bytes a reply may carry.
struct Budget(usize);

impl Budget {
    /// Takes the bytes `v` needs as JSON, if there are that many left.
    fn take(&mut self, v: &Value) -> bool {
        let n = json_len(v);
        match self.0.checked_sub(n) {
            Some(left) => {
                self.0 = left;
                true
            }
            None => false,
        }
    }

    /// The values `items` makes, while they fit, and whether all of them did.
    fn list<T>(&mut self, items: impl IntoIterator<Item = T>, make: impl Fn(T) -> Value) -> (Vec<Value>, bool) {
        let mut out = Vec::new();
        for item in items {
            let v = make(item);
            if !self.take(&v) {
                return (out, false);
            }
            out.push(v);
        }
        (out, true)
    }
}

/// How many bytes `v` takes as compact JSON, without writing it anywhere.
fn json_len(v: &Value) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    // Writing a `Value` to a writer that never fails cannot fail; if it did, the length counted
    // so far still bounds the reply, as the budget only ever shrinks.
    let _ = serde_json::to_writer(&mut count, v);
    count.0
}

pub(super) fn board_query(s: &mut Session, p: BoardQuery) -> Result<Value, EngineError> {
    let board = s.board();
    let kinds = match p.kinds {
        None => ALL_KINDS.to_vec(),
        Some(kinds) if kinds.is_empty() => return Err(bad("`kinds` names at least one kind; leave it out for every kind")),
        Some(kinds) => ALL_KINDS.iter().copied().filter(|k| kinds.contains(k)).collect(),
    };
    let limit = p.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(bad(format!("`limit` must be 1 to {MAX_LIMIT}")));
    }
    let area = match p.area {
        None => None,
        Some([a, b]) => Some(Rect::from_corners(Point::new(mm(a[0], "area")?, mm(a[1], "area")?), Point::new(mm(b[0], "area")?, mm(b[1], "area")?))),
    };
    let filter = Filter {
        kinds,
        net: net(board, p.net.as_deref())?,
        layer: p.layer.as_deref().map(|name| layer(board, name)).transpose()?,
        reference: p.reference,
        area,
    };
    let offset = p.offset.unwrap_or(0);
    let mut page = Page {
        offset,
        limit: usize::try_from(limit).unwrap_or(usize::MAX),
        budget: Budget(MAX_REPLY_BYTES),
        total: 0,
        items: Vec::new(),
        full: false,
    };
    collect(board, &filter, &mut page);
    let shown = u64::try_from(page.items.len()).unwrap_or(u64::MAX);
    let next = offset.checked_add(shown).filter(|&n| n < page.total);
    Ok(json!({ "total": page.total, "offset": offset, "items": page.items, "next": next }))
}

/// Every kind, in the order `board.query` lists them.
const ALL_KINDS: [Kind; 10] =
    [Kind::Net, Kind::Footprint, Kind::Pad, Kind::Track, Kind::Via, Kind::Zone, Kind::Keepout, Kind::Hole, Kind::Text, Kind::Drawing];

/// Offers every match to `page`, kind by kind in [`ALL_KINDS`] order, each in id order.
fn collect(board: &Board, f: &Filter, page: &mut Page) {
    collect_parts(board, f, page);
    collect_copper(board, f, page);
    collect_rest(board, f, page);
}

/// Nets, footprints and pads.
fn collect_parts(board: &Board, f: &Filter, page: &mut Page) {
    let stack = &board.stack;
    let same_reference = |fp: &Footprint| f.reference.as_ref().is_none_or(|r| *r == fp.reference);
    if f.wants(Kind::Net) && f.loose() && f.layer.is_none() && f.area.is_none() {
        for n in board.nets.iter().filter(|n| f.net(Some(n.id))) {
            page.offer(|| json!({ "kind": "net", "id": n.id.to_string(), "name": n.name }));
        }
    }
    if f.wants(Kind::Footprint) {
        for fp in board.footprints.iter() {
            let on_layer = f.layer.is_none_or(|l| stack.side(l) == Some(fp.side));
            let on_net = f.net.is_none_or(|n| fp.pads.iter().any(|pad| pad.net == Some(n)));
            if same_reference(fp) && on_layer && on_net && f.area(|| footprint_extent(fp), by_box) {
                page.offer(|| footprint_summary(fp));
            }
        }
    }
    if f.wants(Kind::Pad) {
        for fp in board.footprints.iter().filter(|fp| same_reference(fp)) {
            for (index, pad) in fp.pads.iter().enumerate() {
                let on_layer = f.layer.is_none_or(|l| pad_layers(stack, fp, pad).contains(&l));
                if f.net(pad.net) && on_layer && f.area(|| pad_extent(fp, pad), by_box) {
                    page.offer(|| pad_json(board, fp, index, pad, false));
                }
            }
        }
    }
}

/// Tracks, vias and zones.
fn collect_copper(board: &Board, f: &Filter, page: &mut Page) {
    let stack = &board.stack;
    if f.wants(Kind::Track) {
        for t in board.tracks.iter() {
            if f.net(t.net) && f.loose() && f.layer(t.layer) && f.area(|| track_extent(t.path, t.width), |a| track_touches(a, t.path, t.width)) {
                page.offer(|| track_json(board, t, false));
            }
        }
    }
    if f.wants(Kind::Via) {
        for v in board.vias.iter() {
            let on_layer = f.layer.is_none_or(|l| via_layers(stack, v.from, v.to).contains(&l));
            if f.net(v.net)
                && f.loose()
                && on_layer
                && f.area(|| Some(grow(spot(v.position), half(v.diameter))), |a| capsule_touches(a, v.position, v.position, half(v.diameter)))
            {
                page.offer(|| via_json(board, v, false));
            }
        }
    }
    if f.wants(Kind::Zone) {
        for z in board.zones.iter() {
            if f.net(z.net)
                && f.loose()
                && f.layer(z.layer)
                && f.area(|| contour_extent(&z.outline, &Some), |a| outline_touches(a, &z.outline, &z.holes))
            {
                page.offer(|| {
                    json!({
                        "kind": "zone", "id": z.id.to_string(), "layer": layer_name(stack, z.layer), "net": net_name(board, z.net),
                        "priority": z.priority, "bounds": rect_json(contour_extent(&z.outline, &Some)),
                    })
                });
            }
        }
    }
}

/// Keepouts, holes, texts and drawings, none of which has a net or belongs to a footprint.
fn collect_rest(board: &Board, f: &Filter, page: &mut Page) {
    let stack = &board.stack;
    if !(f.net(None) && f.loose()) {
        return;
    }
    if f.wants(Kind::Keepout) {
        for k in board.keepouts.iter() {
            if f.layer(k.layer) && f.area(|| contour_extent(&k.outline, &Some), |a| outline_touches(a, &k.outline, &[])) {
                page.offer(|| {
                    json!({
                        "kind": "keepout", "id": k.id.to_string(), "layer": layer_name(stack, k.layer),
                        "tracks": k.tracks, "vias": k.vias, "zones": k.zones, "bounds": rect_json(contour_extent(&k.outline, &Some)),
                    })
                });
            }
        }
    }
    if f.wants(Kind::Hole) && f.layer.is_none() {
        for h in board.holes.iter() {
            let place = |p: Point| p.shifted(h.position);
            let exact = |a: Rect| hole_ends(h.hole, &place, Rotation(0)).is_some_and(|(x, y)| capsule_touches(a, x, y, half(h.hole.diameter)));
            if f.area(|| hole_extent(h.hole, &place, Rotation(0)), exact) {
                page.offer(|| hole_json(h.id, h.position, h.hole));
            }
        }
    }
    if f.wants(Kind::Text) {
        for t in board.texts.iter() {
            let extent = || text_extent(&t.text, t.height, t.stroke, t.justify, t.mirrored, t.rotation, t.position, &Some);
            if f.layer(t.layer) && f.area(extent, by_box) {
                page.offer(|| {
                    json!({
                        "kind": "text", "id": t.id.to_string(), "layer": layer_name(stack, t.layer), "text": t.text,
                        "at": xy(t.position), "height": t.height.to_mm(),
                    })
                });
            }
        }
    }
    if f.wants(Kind::Drawing) {
        for g in board.graphics.iter() {
            if f.layer(g.layer)
                && f.area(
                    || graphic_extent(g, &Some),
                    |a| match g.shape {
                        GraphicShape::Line { from, to } => capsule_touches(a, from, to, half(g.width)),
                        _ => true,
                    },
                )
            {
                page.offer(|| free_drawing(stack, g));
            }
        }
    }
}

pub(super) fn item_info(s: &mut Session, p: ItemInfo) -> Result<Value, EngineError> {
    let board = s.board();
    let id: ItemId = p.id.parse()?;
    let stack = &board.stack;
    let mut budget = Budget(MAX_REPLY_BYTES);
    if let Some(fp) = board.footprints.get(id) {
        return Ok(footprint_json(board, fp, &mut budget));
    }
    if let Some(t) = board.tracks.get(id) {
        return Ok(track_json(board, t, true));
    }
    if let Some(v) = board.vias.get(id) {
        return Ok(via_json(board, v, true));
    }
    if let Some(z) = board.zones.get(id) {
        let fill = match z.fill {
            circuitcraft_model::ZoneFill::Solid => json!("solid"),
            circuitcraft_model::ZoneFill::Hatched { width, gap } => json!({ "hatched": { "width": width.to_mm(), "gap": gap.to_mm() } }),
        };
        let pads = match z.pads {
            circuitcraft_model::PadConnection::Solid => json!("solid"),
            circuitcraft_model::PadConnection::Relief { gap, spoke } => json!({ "relief": { "gap": gap.to_mm(), "spoke": spoke.to_mm() } }),
            circuitcraft_model::PadConnection::None => json!("none"),
        };
        let (outline, mut whole) = contour_json(&z.outline, &mut budget);
        let mut holes = Vec::new();
        for hole in &z.holes {
            if !whole {
                break;
            }
            let (hole, all) = contour_json(hole, &mut budget);
            holes.push(hole);
            whole = all;
        }
        return Ok(json!({
            "kind": "zone", "id": z.id.to_string(), "layer": layer_name(stack, z.layer), "net": net_name(board, z.net),
            "priority": z.priority, "clearance": z.clearance.to_mm(), "minWidth": z.min_width.to_mm(), "fill": fill, "pads": pads,
            "keepIslands": z.keep_islands, "bounds": rect_json(contour_extent(&z.outline, &Some)),
            "outline": outline, "holes": holes, "counts": { "holes": z.holes.len(), "filledAreas": z.filled.len() },
            "truncated": !whole || holes.len() < z.holes.len(),
        }));
    }
    if let Some(k) = board.keepouts.get(id) {
        let (outline, whole) = contour_json(&k.outline, &mut budget);
        return Ok(json!({
            "kind": "keepout", "id": k.id.to_string(), "layer": layer_name(stack, k.layer), "tracks": k.tracks, "vias": k.vias,
            "zones": k.zones, "bounds": rect_json(contour_extent(&k.outline, &Some)), "outline": outline, "truncated": !whole,
        }));
    }
    if let Some(h) = board.holes.get(id) {
        return Ok(hole_json(h.id, h.position, h.hole));
    }
    if let Some(t) = board.texts.get(id) {
        let extent = text_extent(&t.text, t.height, t.stroke, t.justify, t.mirrored, t.rotation, t.position, &Some);
        return Ok(json!({
            "kind": "text", "id": t.id.to_string(), "layer": layer_name(stack, t.layer), "text": t.text, "at": xy(t.position),
            "height": t.height.to_mm(), "stroke": t.stroke.to_mm(), "rotation": t.rotation.degrees(), "mirrored": t.mirrored,
            "justify": justify(t.justify), "bounds": rect_json(extent),
        }));
    }
    if let Some(g) = board.graphics.get(id) {
        let mut v = free_drawing(stack, g);
        let (geometry, whole) = shape_json(&g.shape, &mut budget);
        v["geometry"] = geometry;
        v["truncated"] = json!(!whole);
        return Ok(v);
    }
    let net_id = NetId::from_u128(id.as_u128());
    if let Some(n) = board.nets.get(net_id) {
        let on = |net: Option<NetId>| net == Some(net_id);
        let pads: usize = board.footprints.iter().map(|fp| fp.pads.iter().filter(|p| on(p.net)).count()).sum();
        return Ok(json!({
            "kind": "net", "id": n.id.to_string(), "name": n.name,
            "counts": {
                "pads": pads,
                "tracks": board.tracks.iter().filter(|t| on(t.net)).count(),
                "vias": board.vias.iter().filter(|v| on(v.net)).count(),
                "zones": board.zones.iter().filter(|z| on(z.net)).count(),
            },
        }));
    }
    Err(bad(format!("no item or net with id {id}")))
}

// JSON for what a board holds. Lengths are millimetres, points [x, y], layers and nets by name.

/// A point as `[x, y]` in millimetres.
fn xy(p: Point) -> Value {
    json!([p.x.to_mm(), p.y.to_mm()])
}

/// A rectangle as `[left, bottom, right, top]` in millimetres, as `view.render` gives bounds.
fn rect_json(r: Option<Rect>) -> Value {
    r.map_or(Value::Null, |r| json!([r.min.x.to_mm(), r.min.y.to_mm(), r.max.x.to_mm(), r.max.y.to_mm()]))
}

fn layer_name(stack: &LayerStack, id: LayerId) -> Option<&str> {
    stack.get(id).map(|l| l.name.as_str())
}

fn net_name(board: &Board, net: Option<NetId>) -> Option<&str> {
    net.and_then(|n| board.nets.get(n)).map(|n| n.name.as_str())
}

fn side(side: Side) -> &'static str {
    match side {
        Side::Top => "top",
        Side::Bottom => "bottom",
    }
}

fn justify(j: Justify) -> &'static str {
    match j {
        Justify::Left => "left",
        Justify::Center => "center",
        Justify::Right => "right",
    }
}

/// A solder mask or paste opening: "rules" (the design settings decide), "closed", or
/// `{"open": by}` with how much larger than the copper it is (null: as the settings say).
fn opening(o: Opening) -> Value {
    match o {
        Opening::Rules => json!("rules"),
        Opening::Closed => json!("closed"),
        Opening::Open { by } => json!({ "open": by.map(Nm::to_mm) }),
    }
}

fn track_json(board: &Board, t: &circuitcraft_model::Track, full: bool) -> Value {
    let (from, to) = t.path.ends();
    let through = match t.path {
        TrackPath::Line { .. } => Value::Null,
        TrackPath::Arc { arc } => xy(arc.mid),
    };
    let mut v = json!({
        "kind": "track", "id": t.id.to_string(), "layer": layer_name(&board.stack, t.layer), "width": t.width.to_mm(),
        "from": xy(from), "to": xy(to), "through": through, "net": net_name(board, t.net),
    });
    if full {
        v["length"] = json!(track_length(t.path));
    }
    v
}

/// How long a track is along its middle, in millimetres.
fn track_length(path: TrackPath) -> f64 {
    const NM_PER_MM: f64 = 1.0e6;
    let (from, to) = path.ends();
    let straight = (to.x.0 as f64 - from.x.0 as f64).hypot(to.y.0 as f64 - from.y.0 as f64);
    let nm = match path {
        TrackPath::Line { .. } => straight,
        TrackPath::Arc { arc } => arc.circle().map_or(straight, |c| c.r * c.sweep.abs()),
    };
    nm / NM_PER_MM
}

fn via_json(board: &Board, v: &circuitcraft_model::Via, full: bool) -> Value {
    let stack = &board.stack;
    let mut out = json!({
        "kind": "via", "id": v.id.to_string(), "at": xy(v.position), "diameter": v.diameter.to_mm(), "drill": v.drill.to_mm(),
        "layers": via_layers(stack, v.from, v.to).iter().filter_map(|&l| layer_name(stack, l)).collect::<Vec<_>>(),
        "net": net_name(board, v.net),
    });
    if full {
        out["mask"] = json!({ "top": opening(v.top_mask), "bottom": opening(v.bottom_mask) });
    }
    out
}

fn hole_json(id: ItemId, at: Point, hole: Hole) -> Value {
    let mut v = hole_fields(hole, |r| r);
    (v["kind"], v["id"], v["at"]) = (json!("hole"), json!(id.to_string()), xy(at));
    v
}

/// A hole's size: `{diameter, plated, slot}`, where slot is `{length, rotation}` or null, its
/// rotation on the board (`on_board` turns the slot's own), below half a turn.
fn hole_fields(hole: Hole, on_board: impl Fn(Rotation) -> Rotation) -> Value {
    let slot = |s: circuitcraft_model::Slot| {
        let rotation = Rotation(on_board(s.rotation).0 % 180_000);
        json!({ "length": s.length.to_mm(), "rotation": rotation.degrees() })
    };
    json!({ "diameter": hole.diameter.to_mm(), "plated": hole.plated, "slot": hole.slot.map(slot) })
}

fn footprint_summary(fp: &Footprint) -> Value {
    json!({
        "kind": "footprint", "id": fp.id.to_string(), "reference": fp.reference, "value": fp.value, "at": xy(fp.position),
        "rotation": fp.rotation.degrees(), "side": side(fp.side), "counts": { "pads": fp.pads.len() }, "bounds": rect_json(footprint_extent(fp)),
        "library": fp.link.as_ref().map(|l| json!({ "library": l.library, "footprint": l.footprint })),
    })
}

/// A footprint in full: what [`footprint_summary`] gives, its assembly attributes, and its pads,
/// texts and drawings on the board, for as many as fit in the budget.
fn footprint_json(board: &Board, fp: &Footprint, budget: &mut Budget) -> Value {
    let stack = &board.stack;
    let mut v = footprint_summary(fp);
    let on_board = |p: Point| fp.to_board(p);
    let flip = |l: LayerId| if fp.side == Side::Bottom { stack.flipped(l).unwrap_or(l) } else { l };
    let a = fp.assembly;
    v["assembly"] = json!({
        "mounting": match a.mounting {
            circuitcraft_model::Mounting::SurfaceMount => "surface_mount",
            circuitcraft_model::Mounting::ThroughHole => "through_hole",
            circuitcraft_model::Mounting::Other => "other",
        },
        "inBom": a.in_bom, "inPlacement": a.in_placement, "fitted": a.fitted,
    });
    v["counts"] = json!({ "pads": fp.pads.len(), "texts": fp.texts.len(), "drawings": fp.graphics.len() });
    // Texts and drawings first: pads are the one list that board.query can page through.
    let (texts, texts_whole) = budget.list(&fp.texts, |t| {
        json!({
            "content": match t.content {
                circuitcraft_model::TextContent::Reference => "reference",
                circuitcraft_model::TextContent::Value => "value",
                circuitcraft_model::TextContent::Fixed(_) => "fixed",
            },
            "text": fp.wording(t), "visible": t.visible, "layer": layer_name(stack, flip(t.layer)),
            "at": fp.to_board(t.position).map(xy), "height": t.height.to_mm(),
            "rotation": on_board_rotation(fp, t.rotation).degrees(), "mirrored": t.mirrored != (fp.side == Side::Bottom),
            "justify": justify(t.justify),
            "bounds": rect_json(text_extent(fp.wording(t), t.height, t.stroke, t.justify, t.mirrored, t.rotation, t.position, &on_board)),
        })
    });
    // A footprint's drawings are part of it, not items of their own with ids to pass around.
    let (drawings, drawings_whole) = budget.list(&fp.graphics, |g| drawing_summary(stack, g, flip(g.layer), &on_board));
    let (pads, pads_whole) = budget.list(fp.pads.iter().enumerate(), |(i, pad)| pad_json(board, fp, i, pad, true));
    v["pads"] = json!(pads);
    v["texts"] = json!(texts);
    v["drawings"] = json!(drawings);
    v["truncated"] = json!(!(pads_whole && texts_whole && drawings_whole));
    v
}

/// A rotation relative to a footprint, as it is on the board: on the bottom the footprint is
/// mirrored first, which turns the other way.
fn on_board_rotation(fp: &Footprint, r: Rotation) -> Rotation {
    let own = i64::from(r.0);
    let own = if fp.side == Side::Bottom { -own } else { own };
    Rotation::from_millidegrees(i64::from(fp.rotation.0) + own)
}

/// A pad: its footprint (by id and reference), its number and place in the footprint's list,
/// where it is on the board, its net and the copper layers it has copper on; `full` adds its
/// rotation on the board, its copper shapes, openings and hole.
fn pad_json(board: &Board, fp: &Footprint, index: usize, pad: &Pad, full: bool) -> Value {
    let stack = &board.stack;
    let mut v = json!({
        "kind": "pad", "footprint": fp.id.to_string(), "reference": fp.reference, "number": pad.number, "index": index,
        "at": fp.pad_to_board(pad, Point::ORIGIN).map(xy), "net": net_name(board, pad.net),
        "layers": pad_layers(stack, fp, pad).iter().filter_map(|&l| layer_name(stack, l)).collect::<Vec<_>>(),
    });
    if full {
        let face = |f: &Option<circuitcraft_model::PadFace>| {
            f.as_ref().map(|f| json!({ "shape": pad_shape_json(&f.shape), "mask": opening(f.mask), "paste": opening(f.paste) }))
        };
        // The faces by the side of the board they are on, not by the side of the footprint.
        let (top, bottom) = if fp.side == Side::Top { (&pad.front, &pad.back) } else { (&pad.back, &pad.front) };
        v["rotation"] = json!(on_board_rotation(fp, pad.rotation).degrees());
        v["top"] = face(top).unwrap_or(Value::Null);
        v["inner"] = pad.inner.as_ref().map_or(Value::Null, pad_shape_json);
        v["bottom"] = face(bottom).unwrap_or(Value::Null);
        let slot_on_board = |r: Rotation| on_board_rotation(fp, Rotation::from_millidegrees(i64::from(pad.rotation.0) + i64::from(r.0)));
        v["hole"] = pad.hole.map_or(Value::Null, |h| hole_fields(h, slot_on_board));
        v["bounds"] = rect_json(pad_extent(fp, pad));
    }
    v
}

/// A pad's copper shape, under its kind as the file format writes it.
fn pad_shape_json(shape: &PadShape) -> Value {
    match *shape {
        PadShape::Circle { diameter } => json!({ "circle": { "diameter": diameter.to_mm() } }),
        PadShape::Rect { width, height } => json!({ "rect": { "width": width.to_mm(), "height": height.to_mm() } }),
        PadShape::RoundRect { width, height, radius } => {
            json!({ "round_rect": { "width": width.to_mm(), "height": height.to_mm(), "radius": radius.to_mm() } })
        }
        PadShape::Oval { width, height } => json!({ "oval": { "width": width.to_mm(), "height": height.to_mm() } }),
        PadShape::Custom { ref outline } => json!({ "custom": { "edges": outline.edges.len() } }),
    }
}

/// A drawing in brief: its layer (`layer`, flipped already for a footprint on the bottom), its
/// line width, its kind of shape and what it covers on the board. A free drawing adds its kind
/// and id ([`free_drawing`]).
fn drawing_summary(stack: &LayerStack, g: &Graphic, layer: LayerId, place: &dyn Fn(Point) -> Option<Point>) -> Value {
    let shape = match g.shape {
        GraphicShape::Line { .. } => "line",
        GraphicShape::Arc { .. } => "arc",
        GraphicShape::Circle { .. } => "circle",
        GraphicShape::Polygon { .. } => "polygon",
    };
    json!({
        "layer": layer_name(stack, layer), "width": g.width.to_mm(), "shape": shape, "bounds": rect_json(graphic_extent(g, place)),
    })
}

/// A drawing that belongs to no footprint, in brief, with its kind and id.
fn free_drawing(stack: &LayerStack, g: &Graphic) -> Value {
    let mut v = drawing_summary(stack, g, g.layer, &Some);
    (v["kind"], v["id"]) = (json!("drawing"), json!(g.id.to_string()));
    v
}

/// A free drawing's geometry, and whether all of it fit in the budget.
fn shape_json(shape: &GraphicShape, budget: &mut Budget) -> (Value, bool) {
    match shape {
        GraphicShape::Line { from, to } => (json!({ "line": { "from": xy(*from), "to": xy(*to) } }), true),
        GraphicShape::Arc { arc } => (json!({ "arc": { "from": xy(arc.start), "through": xy(arc.mid), "to": xy(arc.end) } }), true),
        GraphicShape::Circle { center, radius } => (json!({ "circle": { "center": xy(*center), "radius": radius.to_mm() } }), true),
        GraphicShape::Polygon { outline, filled } => {
            let (outline, whole) = contour_json(outline, budget);
            (json!({ "polygon": { "outline": outline, "filled": filled } }), whole)
        }
    }
}

/// A closed outline as `{start, edges}`, each edge `{to}` or, for an arc, `{to, through}`, for
/// as many edges as fit in the budget, and whether all of them did.
fn contour_json(c: &Contour, budget: &mut Budget) -> (Value, bool) {
    let start = xy(c.start);
    if !budget.take(&start) {
        return (json!({ "start": start, "edges": [] }), false);
    }
    let (edges, whole) = budget.list(&c.edges, |e| match *e {
        Edge::Line { to } => json!({ "to": xy(to) }),
        Edge::Arc { mid, to } => json!({ "to": xy(to), "through": xy(mid) }),
    });
    (json!({ "start": start, "edges": edges }), whole)
}

// Where things are: copper layers, and the boxes around what items cover.

/// The copper layers a via joins, top to bottom.
fn via_layers(stack: &LayerStack, from: LayerId, to: LayerId) -> Vec<LayerId> {
    let copper: Vec<LayerId> = stack.copper().map(|l| l.id).collect();
    let (Some(a), Some(b)) = (copper.iter().position(|&l| l == from), copper.iter().position(|&l| l == to)) else { return Vec::new() };
    copper.get(a.min(b)..=a.max(b)).unwrap_or_default().to_vec()
}

/// The copper layers a pad has copper on, top to bottom: its front face on the outer layer of
/// the footprint's side, its back face on the other, and its inner shape on every inner layer.
fn pad_layers(stack: &LayerStack, fp: &Footprint, pad: &Pad) -> Vec<LayerId> {
    let copper: Vec<LayerId> = stack.copper().map(|l| l.id).collect();
    let last = copper.len().saturating_sub(1);
    let outer = |side: Side| stack.copper().find(|l| stack.side(l.id) == Some(side)).map(|l| l.id);
    let front = pad.front.as_ref().and_then(|_| outer(fp.side));
    let back = pad.back.as_ref().and_then(|_| outer(fp.side.opposite()));
    copper
        .iter()
        .enumerate()
        .filter(|&(i, &l)| Some(l) == front || Some(l) == back || (pad.inner.is_some() && i > 0 && i < last))
        .map(|(_, &l)| l)
        .collect()
}

fn half(n: Nm) -> Nm {
    Nm(n.0 / 2)
}

/// The rectangle that is just `p`.
fn spot(p: Point) -> Rect {
    Rect { min: p, max: p }
}

/// `r` grown by `by` on every side (a negative `by` leaves it as it is).
fn grow(r: Rect, by: Nm) -> Rect {
    let by = by.0.max(0);
    Rect {
        min: Point::new(Nm(r.min.x.0.saturating_sub(by)), Nm(r.min.y.0.saturating_sub(by))),
        max: Point::new(Nm(r.max.x.0.saturating_add(by)), Nm(r.max.y.0.saturating_add(by))),
    }
}

fn union(a: Option<Rect>, b: Option<Rect>) -> Option<Rect> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.union(b)),
        (a, b) => a.or(b),
    }
}

/// The box around an arc from `a` through `mid` to `b`: its ends, and the points due east,
/// north, west and south of its centre that it passes.
fn arc_extent(a: Point, mid: Point, b: Point) -> Rect {
    let mut r = Rect::from_corners(a, b).including(mid);
    let Some(c) = Arc::new(a, mid, b).circle() else { return r };
    for (quarter, (dx, dy)) in [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0)].into_iter().enumerate() {
        let angle = FRAC_PI_2 * quarter as f64;
        // How far the arc turns from its start to reach this angle, in its own direction.
        let turn = if c.sweep >= 0.0 { (angle - c.start_angle).rem_euclid(TAU) } else { (c.start_angle - angle).rem_euclid(TAU) };
        if let (true, Some(x), Some(y)) = (turn <= c.sweep.abs(), to_nm(c.cx + dx * c.r), to_nm(c.cy + dy * c.r)) {
            r = r.including(Point::new(x, y));
        }
    }
    r
}

/// A coordinate in floating point as whole nanometres, if it is one a coordinate can hold.
fn to_nm(v: f64) -> Option<Nm> {
    // Beyond 2^62 a float no longer converts to a coordinate safely; NaN fails the test too.
    (v.abs() < 4.0e18).then(|| Nm(v.round() as i64))
}

/// The box around a closed outline whose points `place` puts on the board.
fn contour_extent(c: &Contour, place: &dyn Fn(Point) -> Option<Point>) -> Option<Rect> {
    let mut prev = place(c.start)?;
    let mut r = spot(prev);
    for edge in &c.edges {
        let to = place(edge.end())?;
        r = match *edge {
            Edge::Line { .. } => r.including(to),
            Edge::Arc { mid, .. } => r.union(arc_extent(prev, place(mid)?, to)),
        };
        prev = to;
    }
    Some(r)
}

fn track_extent(path: TrackPath, width: Nm) -> Option<Rect> {
    let r = match path {
        TrackPath::Line { from, to } => Rect::from_corners(from, to),
        TrackPath::Arc { arc } => arc_extent(arc.start, arc.mid, arc.end),
    };
    Some(grow(r, half(width)))
}

fn graphic_extent(g: &Graphic, place: &dyn Fn(Point) -> Option<Point>) -> Option<Rect> {
    let r = match g.shape {
        GraphicShape::Line { from, to } => Rect::from_corners(place(from)?, place(to)?),
        GraphicShape::Arc { arc } => arc_extent(place(arc.start)?, place(arc.mid)?, place(arc.end)?),
        GraphicShape::Circle { center, radius } => grow(spot(place(center)?), radius),
        GraphicShape::Polygon { ref outline, .. } => contour_extent(outline, place)?,
    };
    Some(grow(r, half(g.width)))
}

/// The ends of the line a hole at `place(0, 0)` is drilled along ([`Hole::ends`]), on the board.
fn hole_ends(hole: Hole, place: &dyn Fn(Point) -> Option<Point>, turn: Rotation) -> Option<(Point, Point)> {
    let (a, b) = hole.ends(turn)?;
    Some((place(a)?, place(b)?))
}

/// The box around a hole at `place(0, 0)`, a slot turned by `turn` and its own rotation.
fn hole_extent(hole: Hole, place: &dyn Fn(Point) -> Option<Point>, turn: Rotation) -> Option<Rect> {
    let (a, b) = hole_ends(hole, place, turn)?;
    Some(grow(Rect::from_corners(a, b), half(hole.diameter)))
}

/// The box around a pad's copper and hole on the board; just its place when it has neither.
fn pad_extent(fp: &Footprint, pad: &Pad) -> Option<Rect> {
    let copper = |p: Point| fp.pad_copper_to_board(pad, p);
    let faces = [pad.front.as_ref().map(|f| &f.shape), pad.inner.as_ref(), pad.back.as_ref().map(|f| &f.shape)];
    let mut r = faces.into_iter().flatten().fold(None, |r, shape| union(r, shape_extent(shape, &copper)));
    if let Some(hole) = pad.hole {
        let centre = |p: Point| fp.pad_to_board(pad, p);
        r = union(r, hole_extent(hole, &centre, pad.rotation));
    }
    r.or_else(|| fp.pad_to_board(pad, Point::ORIGIN).map(spot))
}

/// The box around a pad shape whose points, relative to its centre, `place` puts on the board.
/// A rectangle's corners bound the rounded shapes too.
fn shape_extent(shape: &PadShape, place: &dyn Fn(Point) -> Option<Point>) -> Option<Rect> {
    let corners = |w: Nm, h: Nm| {
        let (w, h) = (w.0 / 2, h.0 / 2);
        let ring = [(-w, -h), (w, -h), (w, h), (-w, h)].map(|(x, y)| place(Point::new(Nm(x), Nm(y))));
        Rect::bounding(ring.into_iter().collect::<Option<Vec<_>>>()?)
    };
    match *shape {
        PadShape::Circle { diameter } => Some(grow(spot(place(Point::ORIGIN)?), half(diameter))),
        PadShape::Rect { width, height } | PadShape::RoundRect { width, height, .. } | PadShape::Oval { width, height } => corners(width, height),
        PadShape::Custom { ref outline } => contour_extent(outline, place),
    }
}

/// The box around a footprint's pads and drawings (and its origin) on the board.
fn footprint_extent(fp: &Footprint) -> Option<Rect> {
    let on_board = |p: Point| fp.to_board(p);
    let pads = fp.pads.iter().map(|pad| pad_extent(fp, pad));
    let drawings = fp.graphics.iter().map(|g| graphic_extent(g, &on_board));
    pads.chain(drawings).fold(Some(spot(fp.position)), union)
}

/// The box around a text laid out in the stroke font: mirrored about its anchor, turned, moved
/// to `position` and put on the board by `place`, its strokes' width included; just its anchor
/// when it draws nothing.
#[allow(clippy::too_many_arguments)] // every argument is a field of the text
fn text_extent(
    words: &str,
    height: Nm,
    stroke: Nm,
    justify: Justify,
    mirrored: bool,
    rotation: Rotation,
    position: Point,
    place: &dyn Fn(Point) -> Option<Point>,
) -> Option<Rect> {
    let laid = circuitcraft_render::font::layout(words, height, justify);
    let points = laid.into_iter().flatten().filter_map(|p| place(text_point(p, mirrored, rotation, position)?));
    match Rect::bounding(points) {
        Some(r) => Some(grow(r, half(stroke))),
        None => place(position).map(spot),
    }
}

// Whether an item's own shape touches a rectangle, for the items whose box says too little: a
// diagonal track's box covers far more than the track. In floating point nanometres, which hold
// every coordinate of the design range exactly.

fn fxy(p: Point) -> (f64, f64) {
    (p.x.0 as f64, p.y.0 as f64)
}

/// How far `p` is from the rectangle; 0 inside it.
fn rect_distance(r: Rect, (x, y): (f64, f64)) -> f64 {
    let ((lx, ly), (hx, hy)) = (fxy(r.min), fxy(r.max));
    (lx - x).max(x - hx).max(0.0).hypot((ly - y).max(y - hy).max(0.0))
}

/// How far `p` is from the segment from `a` to `b`.
fn segment_distance((px, py): (f64, f64), (ax, ay): (f64, f64), (bx, by): (f64, f64)) -> f64 {
    let (dx, dy) = (bx - ax, by - ay);
    let length2 = dx * dx + dy * dy;
    let t = if length2 > 0.0 { (((px - ax) * dx + (py - ay) * dy) / length2).clamp(0.0, 1.0) } else { 0.0 };
    (px - (ax + t * dx)).hypot(py - (ay + t * dy))
}

/// Whether the segment from `a` to `b` has a point in the rectangle (Liang and Barsky's clip).
fn segment_crosses(r: Rect, (ax, ay): (f64, f64), (bx, by): (f64, f64)) -> bool {
    let ((lx, ly), (hx, hy)) = (fxy(r.min), fxy(r.max));
    let (dx, dy) = (bx - ax, by - ay);
    let (mut enter, mut leave) = (0.0_f64, 1.0_f64);
    for (towards, room) in [(-dx, ax - lx), (dx, hx - ax), (-dy, ay - ly), (dy, hy - ay)] {
        if towards == 0.0 {
            if room < 0.0 {
                return false;
            }
        } else if towards < 0.0 {
            enter = enter.max(room / towards);
        } else {
            leave = leave.min(room / towards);
        }
    }
    enter <= leave
}

/// Whether a stroke `radius` either side of the segment from `a` to `b`, round at its ends,
/// touches the rectangle: a straight track, a line, a via (`a` = `b`) or a slot.
fn capsule_touches(r: Rect, a: Point, b: Point, radius: Nm) -> bool {
    let (a, b) = (fxy(a), fxy(b));
    if segment_crosses(r, a, b) {
        return true;
    }
    // Apart, a segment and a rectangle are closest at an end of the one or a corner of the other.
    let radius = radius.0.max(0) as f64;
    let ((lx, ly), (hx, hy)) = (fxy(r.min), fxy(r.max));
    rect_distance(r, a) <= radius
        || rect_distance(r, b) <= radius
        || [(lx, ly), (hx, ly), (hx, hy), (lx, hy)].into_iter().any(|corner| segment_distance(corner, a, b) <= radius)
}

/// Whether a track touches the rectangle; an arc is taken as the box around it.
fn track_touches(r: Rect, path: TrackPath, width: Nm) -> bool {
    match path {
        TrackPath::Line { from, to } => capsule_touches(r, from, to, half(width)),
        TrackPath::Arc { .. } => true,
    }
}

/// Whether the area inside a closed outline and outside its holes, or an edge of either, touches
/// the rectangle. An outline too large to flatten counts as touching wherever its box does.
fn outline_touches(r: Rect, outline: &Contour, holes: &[Contour]) -> bool {
    let mut rings = Vec::new();
    let mut points = 0_usize;
    for c in std::iter::once(outline).chain(holes) {
        let Ok(ring) = c.flatten(circuitcraft_render::TOLERANCE) else { return true };
        // Each ring is bounded on its own, and thousands of holes could still add up.
        points = points.saturating_add(ring.len());
        if points > circuitcraft_geom::MAX_POINTS {
            return true;
        }
        rings.push(ring.into_iter().map(fxy).collect::<Vec<_>>());
    }
    let edges = || rings.iter().flat_map(|ring| ring.iter().zip(ring.iter().cycle().skip(1)));
    if edges().any(|(&a, &b)| segment_crosses(r, a, b)) {
        return true;
    }
    // No edge reaches the rectangle: it is wholly inside the area or wholly outside, and a corner
    // of it says which (even-odd rule, which counts the holes' edges too).
    let (x, y) = fxy(r.min);
    edges().filter(|&(&(ax, ay), &(bx, by))| (ay > y) != (by > y) && x < ax + (y - ay) * (bx - ax) / (by - ay)).count() % 2 == 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use circuitcraft_model::{Assembly, FootprintText, Mounting, Text, TextContent, layer_ids};

    const MM: i64 = 1_000_000;

    fn p(x: f64, y: f64) -> Point {
        Point::new(Nm((x * MM as f64) as i64), Nm((y * MM as f64) as i64))
    }

    fn footprint(id: u128, reference: &str, at: Point, rotation: i32, side: Side, pads: Vec<Pad>) -> Footprint {
        Footprint {
            id: ItemId::from_u128(id),
            reference: reference.to_owned(),
            value: "10k".to_owned(),
            link: None,
            assembly: Assembly { mounting: Mounting::SurfaceMount, in_bom: true, in_placement: true, fitted: true },
            position: at,
            rotation: Rotation(rotation),
            side,
            pads,
            graphics: Vec::new(),
            texts: Vec::new(),
        }
    }

    fn rect(w: f64, h: f64) -> PadShape {
        PadShape::Rect { width: Nm((w * MM as f64) as i64), height: Nm((h * MM as f64) as i64) }
    }

    /// A session holding `board`.
    fn holding(board: Board) -> Session {
        let mut s = Session::in_memory();
        s.replace_board("Test", board, None, false);
        s
    }

    /// U1 on the top at (10, 5), turned a quarter, with pads on GND and VCC; R1 on the bottom at
    /// (20, 0), turned a quarter, with a slotted through-hole pad on GND, a turned SMD pad and its
    /// value; a track on each net, a via and a hole.
    fn sample() -> (Session, Value, Value) {
        let mut b = Board::new("Sample");
        let gnd = NetId::from_u128(1);
        let vcc = NetId::from_u128(2);
        for (id, name) in [(gnd, "GND"), (vcc, "VCC")] {
            b.nets.insert(circuitcraft_model::Net { id, name: name.to_owned() }).expect("net");
        }
        let mut u1_pads = vec![Pad::smd("1", p(-1.0, 0.0), rect(0.6, 0.4)), Pad::smd("2", p(1.0, 0.0), rect(0.6, 0.4))];
        u1_pads[0].net = Some(gnd);
        u1_pads[1].net = Some(vcc);
        let mut u1 = footprint(10, "U1", p(10.0, 5.0), 90_000, Side::Top, u1_pads);
        u1.texts.push(FootprintText {
            id: ItemId::from_u128(11),
            content: TextContent::Reference,
            visible: true,
            layer: layer_ids::TOP_SILK,
            position: p(0.0, 2.0),
            height: Nm(MM),
            stroke: Nm(MM / 10),
            rotation: Rotation(0),
            mirrored: false,
            justify: Justify::Center,
        });
        b.footprints.insert(u1).expect("U1");
        let slot = Hole { diameter: Nm(MM), slot: Some(circuitcraft_model::Slot { length: Nm(2 * MM), rotation: Rotation(0) }), plated: true };
        let mut th = Pad::through_hole("1", p(1.0, 0.0), PadShape::Circle { diameter: Nm(3 * MM) }, slot);
        th.net = Some(gnd);
        let mut smd = Pad::smd("2", p(0.0, 1.0), rect(0.6, 0.4));
        smd.rotation = Rotation(30_000);
        let mut r1 = footprint(20, "R1", p(20.0, 0.0), 90_000, Side::Bottom, vec![th, smd]);
        r1.texts.push(FootprintText {
            id: ItemId::from_u128(21),
            content: TextContent::Value,
            visible: true,
            layer: layer_ids::TOP_SILK,
            position: p(0.0, 2.0),
            height: Nm(MM),
            stroke: Nm(MM / 10),
            rotation: Rotation(0),
            mirrored: false,
            justify: Justify::Center,
        });
        b.footprints.insert(r1).expect("R1");
        let mut s = holding(b);
        let gnd_track =
            s.execute("track.add", &json!({ "layer": "Top Copper", "width": 0.2, "from": [0, 0], "to": [10, 0], "net": "GND" })).expect("track");
        let vcc_track =
            s.execute("track.add", &json!({ "layer": "Bottom Copper", "width": 0.2, "from": [0, 10], "to": [10, 10], "net": "VCC" })).expect("track");
        s.execute("via.add", &json!({ "at": [10, 0], "net": "GND" })).expect("via");
        s.execute("hole.add", &json!({ "at": [30, 30], "diameter": 3 })).expect("hole");
        (s, gnd_track["id"].clone(), vcc_track["id"].clone())
    }

    fn query(s: &mut Session, params: Value) -> Value {
        s.execute("board.query", &params).expect("board.query")
    }

    /// The kinds of the items a query returned, in order.
    fn kinds(reply: &Value) -> Vec<String> {
        reply["items"].as_array().into_iter().flatten().filter_map(|i| i["kind"].as_str().map(str::to_owned)).collect()
    }

    #[test]
    fn a_footprint_is_found_by_reference_with_its_pads_on_the_board() {
        let (mut s, _, _) = sample();
        let found = query(&mut s, json!({ "reference": "U1" }));
        assert_eq!((kinds(&found), &found["total"], &found["next"]), (vec!["footprint".into(), "pad".into(), "pad".into()], &json!(3), &Value::Null));
        let u1 = &found["items"][0];
        assert_eq!((&u1["id"], &u1["reference"], &u1["value"]), (&json!(ItemId::from_u128(10).to_string()), &json!("U1"), &json!("10k")));
        assert_eq!(
            (&u1["at"], &u1["rotation"], &u1["side"], &u1["counts"]),
            (&json!([10.0, 5.0]), &json!(90.0), &json!("top"), &json!({ "pads": 2 }))
        );
        // A quarter turn counter-clockwise puts pad 1, left of the origin, below it.
        let pad = &found["items"][1];
        assert_eq!(
            pad,
            &json!({ "kind": "pad", "footprint": u1["id"], "reference": "U1", "number": "1", "index": 0, "at": [10.0, 4.0], "net": "GND", "layers": ["Top Copper"] })
        );
        assert_eq!(found["items"][2]["at"], json!([10.0, 6.0]));
        // In full: the pads with their copper on the board, and the texts placed.
        let info = s.execute("item.info", &json!({ "id": u1["id"] })).expect("info");
        assert_eq!(
            (&info["kind"], &info["truncated"], &info["counts"]),
            (&json!("footprint"), &json!(false), &json!({ "pads": 2, "texts": 1, "drawings": 0 }))
        );
        let pad = &info["pads"][0];
        assert_eq!(
            (&pad["rotation"], &pad["top"]["shape"], &pad["bottom"], &pad["hole"]),
            (&json!(90.0), &json!({ "rect": { "width": 0.6, "height": 0.4 } }), &Value::Null, &Value::Null)
        );
        // Turned a quarter, the pad is 0.4 wide and 0.6 high around (10, 4).
        let bounds: Vec<f64> = pad["bounds"].as_array().expect("bounds").iter().filter_map(Value::as_f64).collect();
        let want = [9.8, 3.7, 10.2, 4.3];
        assert!(bounds.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-9), "{bounds:?}");
        let text = &info["texts"][0];
        assert_eq!(
            (&text["content"], &text["text"], &text["layer"], &text["at"]),
            (&json!("reference"), &json!("U1"), &json!("Top Silkscreen"), &json!([8.0, 5.0]))
        );
        assert_eq!(info["assembly"], json!({ "mounting": "surface_mount", "inBom": true, "inPlacement": true, "fitted": true }));

        // On the bottom a footprint is mirrored left to right, then turned: R1's pad 1, right of
        // its origin, ends up below it, and pad 2, above its origin, ends up left of it.
        let r1 = query(&mut s, json!({ "reference": "R1", "kinds": ["pad"] }));
        assert_eq!((&r1["items"][0]["at"], &r1["items"][0]["layers"]), (&json!([20.0, -1.0]), &json!(["Top Copper", "Bottom Copper"])));
        assert_eq!((&r1["items"][1]["at"], &r1["items"][1]["layers"]), (&json!([19.0, 0.0]), &json!(["Bottom Copper"])));
        let info = s.execute("item.info", &json!({ "id": ItemId::from_u128(20).to_string() })).expect("info");
        assert_eq!(info["side"], json!("bottom"));
        // Its pads' faces are named by the side of the board they are on, and mirroring turns the
        // other way: a pad turned 30 degrees on a footprint turned 90 is at 60 on the board.
        let smd = &info["pads"][1];
        assert_eq!(
            (&smd["top"], &smd["bottom"]["shape"], &smd["rotation"]),
            (&Value::Null, &json!({ "rect": { "width": 0.6, "height": 0.4 } }), &json!(60.0))
        );
        // The slot, along its pad, runs up and down the board.
        assert_eq!(info["pads"][0]["hole"], json!({ "diameter": 1.0, "plated": true, "slot": { "length": 2.0, "rotation": 90.0 } }));
        let text = &info["texts"][0];
        assert_eq!(
            (&text["text"], &text["layer"], &text["at"], &text["mirrored"], &text["rotation"]),
            (&json!("10k"), &json!("Bottom Silkscreen"), &json!([18.0, 0.0]), &json!(true), &json!(90.0))
        );
        assert!(text.get("id").is_none(), "a footprint's text is not an item of its own");
        assert_eq!(query(&mut s, json!({ "reference": "U9" }))["total"], 0);
    }

    #[test]
    fn items_are_found_by_net_layer_and_place() {
        let (mut s, gnd_track, vcc_track) = sample();
        let gnd = query(&mut s, json!({ "net": "GND" }));
        assert_eq!(kinds(&gnd), ["net", "footprint", "footprint", "pad", "pad", "track", "via"]);
        let tracks = query(&mut s, json!({ "kinds": ["track"], "net": "GND" }));
        assert_eq!(
            tracks["items"],
            json!([{
                "kind": "track", "id": gnd_track, "layer": "Top Copper", "width": 0.2, "from": [0.0, 0.0], "to": [10.0, 0.0], "through": null, "net": "GND",
            }])
        );
        // The bottom copper: the VCC track, the via through it, R1's through-hole pad and R1,
        // which is mounted on that side.
        let bottom = query(&mut s, json!({ "layer": "Bottom Copper" }));
        assert_eq!(kinds(&bottom), ["footprint", "pad", "pad", "track", "via"]);
        assert_eq!((&bottom["items"][0]["reference"], &bottom["items"][3]["id"]), (&json!("R1"), &vcc_track));
        // What covers a point: U1's pad 2 and U1 itself; the middle of a track; nothing at all.
        let at = |s: &mut Session, x: f64, y: f64| query(s, json!({ "area": [[x, y], [x, y]] }));
        let pad = at(&mut s, 10.1, 6.2);
        assert_eq!((kinds(&pad), &pad["items"][1]["number"]), (vec!["footprint".into(), "pad".into()], &json!("2")));
        assert_eq!(at(&mut s, 5.0, 0.1)["items"][0]["id"], gnd_track);
        assert_eq!(at(&mut s, 5.0, 0.2)["total"], 0, "past the track's edge");
        assert_eq!(kinds(&at(&mut s, 31.4, 30.0)), ["hole"]);
        assert_eq!(at(&mut s, 31.1, 31.1)["total"], 0, "inside the hole's box, outside the hole");
        // Corners in either order, filters together.
        let box_ = query(&mut s, json!({ "area": [[11, 11], [-1, -1]], "kinds": ["track", "via"] }));
        assert_eq!(box_["total"], 3);
        assert_eq!(query(&mut s, json!({ "area": [[11, 11], [-1, -1]], "net": "VCC", "kinds": ["track"] }))["items"][0]["id"], vcc_track);
        // item.info on a net counts what is on it.
        let net = &gnd["items"][0];
        let info = s.execute("item.info", &json!({ "id": net["id"] })).expect("net");
        assert_eq!(info, json!({ "kind": "net", "id": net["id"], "name": "GND", "counts": { "pads": 2, "tracks": 1, "vias": 1, "zones": 0 } }));
    }

    #[test]
    fn an_arc_covers_the_far_side_of_its_circle() {
        let mut b = Board::new("Arc");
        let arc = Arc::new(p(4.0, 3.0), p(-4.0, 3.0), p(-4.0, -3.0));
        let id = {
            let mut doc = circuitcraft_model::Document::new(b.clone());
            doc.edit("Arc", |e| e.add_track(layer_ids::TOP_COPPER, Nm(MM / 5), TrackPath::Arc { arc }, None)).expect("arc");
            b = doc.board().clone();
            b.tracks.iter().next().map(|t| t.id).expect("a track")
        };
        let mut s = holding(b);
        // The arc turns counter-clockwise through the top and the left of its circle (radius 5),
        // past every point it is given by.
        for (x, y) in [(0.0, 5.05), (-5.05, 0.0)] {
            assert_eq!(query(&mut s, json!({ "area": [[x, y], [x, y]] }))["total"], 1, "({x}, {y})");
        }
        assert_eq!(query(&mut s, json!({ "area": [[0, -5.05], [0, -5.05]] }))["total"], 0, "the bottom of the circle is not on the arc");
        let info = s.execute("item.info", &json!({ "id": id.to_string() })).expect("info");
        assert_eq!(info["through"], json!([-4.0, 3.0]));
        let length = info["length"].as_f64().expect("length");
        assert!((length - 5.0 * std::f64::consts::PI).abs() < 1e-6, "half a turn of radius 5: {length}");
    }

    #[test]
    fn a_diagonal_track_and_an_outline_cover_only_themselves() {
        let mut b = Board::new("Shapes");
        // An L: the square from (0, 0) to (10, 10) without its top right quarter.
        let corners = [p(0.0, 0.0), p(10.0, 0.0), p(10.0, 5.0), p(5.0, 5.0), p(5.0, 10.0), p(0.0, 10.0)];
        let outline = Contour::polyline(&corners).expect("corners");
        let keepout =
            circuitcraft_model::Keepout { id: ItemId::from_u128(5), layer: layer_ids::TOP_COPPER, outline, tracks: true, vias: true, zones: true };
        b.keepouts.insert(keepout).expect("keepout");
        // A zone over (20, 20) to (30, 30) with a hole from (24, 24) to (26, 26).
        let square = |a: f64, z: f64| Contour::rect(Rect::from_corners(p(a, a), p(z, z)));
        b.zones
            .insert(circuitcraft_model::Zone {
                id: ItemId::from_u128(6),
                layer: layer_ids::TOP_COPPER,
                net: None,
                outline: square(20.0, 30.0),
                holes: vec![square(24.0, 26.0)],
                clearance: Nm(MM / 5),
                min_width: Nm(MM / 5),
                priority: 0,
                fill: circuitcraft_model::ZoneFill::Solid,
                pads: circuitcraft_model::PadConnection::Solid,
                keep_islands: false,
                filled: Vec::new(),
            })
            .expect("zone");
        let mut s = holding(b);
        s.execute("track.add", &json!({ "layer": "Bottom Copper", "width": 0.2, "from": [20, 0], "to": [30, 10] })).expect("track");
        let at = |s: &mut Session, area: Value| kinds(&query(s, json!({ "area": area })));
        // Inside the L, in its notch (inside its box), and a rectangle wholly inside it.
        assert_eq!(at(&mut s, json!([[2, 2], [2, 2]])), ["keepout"]);
        assert_eq!(at(&mut s, json!([[7, 7], [8, 8]])), Vec::<String>::new());
        assert_eq!(at(&mut s, json!([[1, 1], [4, 4]])), ["keepout"]);
        assert_eq!(at(&mut s, json!([[4.9, 6], [5.1, 6]])), ["keepout"], "across its edge");
        // A zone covers what is inside it, but not what is in its hole.
        assert_eq!(at(&mut s, json!([[22, 22], [22, 22]])), ["zone"]);
        assert_eq!(at(&mut s, json!([[25, 25], [25, 25]])), Vec::<String>::new());
        assert_eq!(at(&mut s, json!([[25, 25], [27, 25]])), ["zone"], "out of the hole");
        // On the diagonal track, within half its width of it, and off it but inside its box.
        assert_eq!(at(&mut s, json!([[25, 5], [25, 5]])), ["track"]);
        assert_eq!(at(&mut s, json!([[25.05, 4.95], [25.05, 4.95]])), ["track"]);
        assert_eq!(at(&mut s, json!([[28, 2], [28, 2]])), Vec::<String>::new());
        // A rectangle the track passes through without an end or a corner near it.
        assert_eq!(at(&mut s, json!([[24, 4], [26, 6]])), ["track"]);
    }

    #[test]
    fn pages_cover_every_match_once() {
        let mut s = Session::in_memory();
        for i in 0..250 {
            let y = f64::from(i);
            s.execute("track.add", &json!({ "layer": "Top Copper", "from": [0, y], "to": [1, y] })).expect("track");
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut offset = Value::from(0);
        let mut pages = 0;
        while !offset.is_null() {
            let page = query(&mut s, json!({ "kinds": ["track"], "offset": offset, "limit": 100 }));
            assert_eq!(page["total"], 250);
            seen.extend(page["items"].as_array().into_iter().flatten().filter_map(|i| i["id"].as_str().map(str::to_owned)));
            offset = page["next"].clone();
            pages += 1;
        }
        assert_eq!((seen.len(), pages), (250, 3));
        // Past the end: nothing, and no next page.
        let past = query(&mut s, json!({ "offset": u64::MAX }));
        assert_eq!((&past["items"], &past["next"], &past["total"]), (&json!([]), &Value::Null, &json!(250)));
        // A query on an empty board, and the default limit.
        assert_eq!(query(&mut Session::in_memory(), json!({})), json!({ "total": 0, "offset": 0, "items": [], "next": null }));
        assert_eq!(query(&mut s, json!({}))["items"].as_array().map(Vec::len), Some(100));
    }

    #[test]
    fn hostile_parameters_are_refused_without_harm() {
        let (mut s, track, _) = sample();
        let refused = |s: &mut Session, id: &str, params: Value| match s.execute(id, &params) {
            Err(EngineError::BadParams(m)) => m,
            other => panic!("{id} {params}: {other:?}"),
        };
        for params in [
            json!({ "limit": 0 }),
            json!({ "limit": 1_001 }),
            json!({ "limit": -1 }),
            json!({ "limit": 1e300 }),
            json!({ "offset": -1 }),
            json!({ "kinds": [] }),
            json!({ "kinds": ["bogus"] }),
            json!({ "kinds": "track" }),
            json!({ "net": "NOPE" }),
            json!({ "layer": "Inner 7" }),
            json!({ "reference": 7 }),
            json!({ "area": [[1, 2]] }),
            json!({ "area": [[1, 2], [3]] }),
            json!({ "area": [[1e30, 0], [0, 0]] }),
            json!({ "area": [[1, 2], [3, "4"]] }),
        ] {
            refused(&mut s, "board.query", params);
        }
        assert_eq!(refused(&mut s, "board.query", json!({ "net": "NOPE" })), "no net named \"NOPE\"");
        for id in [json!("zz"), json!(""), json!(7), json!("00000000-0000-0000-0000-000000000000"), json!("x".repeat(100_000))] {
            refused(&mut s, "item.info", json!({ "id": id }));
        }
        let gone = ItemId::from_u128(999).to_string();
        assert_eq!(refused(&mut s, "item.info", json!({ "id": gone })), format!("no item or net with id {gone}"));
        // A list of kinds as long as a message can be is read, and still answers once per item.
        let many = vec!["track"; 1_000_000];
        assert_eq!(query(&mut s, json!({ "kinds": many }))["total"], 2);
        // An empty name is a name like any other: it finds nothing.
        assert_eq!(query(&mut s, json!({ "reference": "" }))["total"], 0);
        assert!(s.execute("item.info", &json!({ "id": track })).is_ok());
    }

    #[test]
    fn a_large_board_stays_within_the_reply_limit() {
        // Texts as long as a board allows, of characters JSON writes six bytes for, and a
        // footprint with as many pads as one may have.
        let mut b = Board::new("Large");
        let words: String = std::iter::repeat_n('\u{1}', circuitcraft_model::MAX_NAME_CHARS).collect();
        for i in 0..1_500_u128 {
            b.texts
                .insert(Text {
                    id: ItemId::from_u128(1_000 + i),
                    layer: layer_ids::TOP_SILK,
                    text: words.clone(),
                    position: Point::ORIGIN,
                    height: Nm(MM),
                    stroke: Nm(MM / 10),
                    rotation: Rotation(0),
                    mirrored: false,
                    justify: Justify::Left,
                })
                .expect("text");
        }
        let pads = (0..circuitcraft_model::MAX_PADS).map(|i| Pad::smd(&words, p(0.0, i as f64 * 0.001), rect(0.1, 0.1))).collect();
        b.footprints.insert(footprint(1, "BIG", Point::ORIGIN, 0, Side::Top, pads)).expect("footprint");
        let mut s = holding(b);
        let limit = MAX_REPLY_BYTES + 4_096;

        let mut offset = Value::from(0);
        let mut texts = 0;
        while !offset.is_null() {
            let page = query(&mut s, json!({ "kinds": ["text"], "limit": MAX_LIMIT, "offset": offset }));
            assert!(json_len(&page) <= limit, "a page of {} bytes", json_len(&page));
            let n = page["items"].as_array().map_or(0, Vec::len);
            assert!(n > 0 && n < 1_000, "{n} texts on a page");
            texts += n;
            offset = page["next"].clone();
        }
        assert_eq!(texts, 1_500);

        let pads = query(&mut s, json!({ "kinds": ["pad"], "limit": MAX_LIMIT }));
        assert!(json_len(&pads) <= limit && pads["total"] == circuitcraft_model::MAX_PADS && !pads["next"].is_null());

        let info = s.execute("item.info", &json!({ "id": ItemId::from_u128(1).to_string() })).expect("info");
        assert!(json_len(&info) <= limit, "item.info of {} bytes", json_len(&info));
        assert_eq!((&info["truncated"], &info["counts"]["pads"]), (&json!(true), &json!(circuitcraft_model::MAX_PADS)));
        assert!(info["pads"].as_array().is_some_and(|p| !p.is_empty() && p.len() < circuitcraft_model::MAX_PADS));
    }
}
