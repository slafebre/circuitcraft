//! What a board is made of: nets, footprints with pads, tracks, vias, zones, keepouts, holes, text
//! and drawings, and the settings they share.
//!
//! Coordinates are board coordinates in [`Nm`], y up, except pads and footprint drawings and
//! texts, which are relative to their footprint's origin before its rotation and side are applied.

use crate::store::Keyed;
use crate::{LayerId, ModelError};
use circuitcraft_geom::{Arc, Contour, Polygon};
use circuitcraft_units::{Nm, Point};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

/// Reads a field that may be `null` but must be written: a file that leaves it out is refused,
/// not read as if it said `null`, so a field that was renamed or lost is noticed.
pub(crate) fn required<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    Option::deserialize(d)
}

/// 128 fresh random bits as a version 4 UUID; an error, never a panic, if the system has none.
fn random_uuid() -> Result<Uuid, ModelError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| ModelError::NoRandomness(e.to_string()))?;
    Ok(uuid::Builder::from_random_bytes(bytes).into_uuid())
}

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        ///
        /// Ids are random version 4 UUIDs, so items added on two git branches never share one, and
        /// files write them in the usual form (`3f2c9a10-6b7e-4d21-9a0c-58e1f4b2d7a3`).
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// A new random id.
            pub fn random() -> Result<$name, ModelError> {
                random_uuid().map($name)
            }

            /// The id with these 128 bits, for tests and for importers that derive ids from
            /// their source file.
            pub const fn from_u128(bits: u128) -> $name {
                $name(Uuid::from_u128(bits))
            }

            /// The id's 128 bits.
            pub const fn as_u128(self) -> u128 {
                self.0.as_u128()
            }

            /// Whether this is the all-zero id, which no item may have.
            pub const fn is_nil(self) -> bool {
                self.0.is_nil()
            }
        }

        impl fmt::Display for $name {
            /// The usual lower-case, hyphenated UUID form.
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                self.0.as_hyphenated().fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = ModelError;
            fn from_str(s: &str) -> Result<$name, ModelError> {
                Uuid::try_parse(s).map($name).map_err(|_| ModelError::BadId(s.chars().take(64).collect()))
            }
        }
    };
}

id_type!(
    /// The identity of a board item, stable for its life and unique within the board.
    ItemId
);

id_type!(
    /// The identity of a net, unique within the board.
    NetId
);

/// A rotation in thousandths of a degree, counter-clockwise, kept in `0..360_000`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Rotation(pub i32);

impl Rotation {
    /// A rotation from thousandths of a degree, normalised into one turn.
    pub fn from_millidegrees(md: i64) -> Rotation {
        // rem_euclid of a positive modulus is in range for i32.
        Rotation(i32::try_from(md.rem_euclid(360_000)).unwrap_or(0))
    }

    /// The rotation in degrees.
    pub fn degrees(self) -> f64 {
        f64::from(self.0) / 1_000.0
    }

    /// `p` turned about the origin by this rotation, to the nearest nanometre. Quarter turns are
    /// exact. `None` when the result would leave the range of a coordinate.
    pub fn turn(self, p: Point) -> Option<Point> {
        let (x, y) = (p.x.0, p.y.0);
        let (x, y) = match self.0.rem_euclid(360_000) {
            0 => (x, y),
            90_000 => (y.checked_neg()?, x),
            180_000 => (x.checked_neg()?, y.checked_neg()?),
            270_000 => (y, x.checked_neg()?),
            _ => {
                let (sin, cos) = self.degrees().to_radians().sin_cos();
                let (fx, fy) = (x as f64, y as f64);
                let (tx, ty) = ((fx * cos - fy * sin).round(), (fx * sin + fy * cos).round());
                // Beyond 2^62 a float no longer converts to a coordinate safely.
                if !(tx.abs() < 4.0e18 && ty.abs() < 4.0e18) {
                    return None;
                }
                (tx as i64, ty as i64)
            }
        };
        Some(Point::new(Nm(x), Nm(y)))
    }

    /// The rotation that undoes this one.
    pub fn reversed(self) -> Rotation {
        Rotation::from_millidegrees(-i64::from(self.0))
    }
}

/// A side of the board: the one a footprint is mounted on, or the one a layer belongs to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// The top side.
    #[default]
    Top,
    /// The bottom side. A footprint there is mirrored left to right ([`Footprint::to_board`]).
    Bottom,
}

impl Side {
    /// The other side.
    pub fn opposite(self) -> Side {
        match self {
            Side::Top => Side::Bottom,
            Side::Bottom => Side::Top,
        }
    }
}

/// A connection: everything on the same net must end up connected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Net {
    /// Identity.
    pub id: NetId,
    /// Name, unique within the board (`GND`, `VCC`, `Net-(R1-Pad2)`).
    pub name: String,
}

/// The copper shape of a pad, placed on the pad's copper centre ([`Pad::copper_offset`]). A file
/// writes it under its kind: `{"rect": {"width": …, "height": …}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum PadShape {
    /// A circle.
    Circle {
        /// Diameter.
        diameter: Nm,
    },
    /// A rectangle.
    Rect {
        /// Width (x before rotation).
        width: Nm,
        /// Height.
        height: Nm,
    },
    /// A rectangle with rounded corners.
    RoundRect {
        /// Width.
        width: Nm,
        /// Height.
        height: Nm,
        /// Corner radius.
        radius: Nm,
    },
    /// A stadium: a rectangle with fully rounded short ends.
    Oval {
        /// Width.
        width: Nm,
        /// Height.
        height: Nm,
    },
    /// Any outline, relative to the copper centre.
    Custom {
        /// The outline.
        outline: Contour,
    },
}

/// The opening over a pad or via in the solder mask, or the aperture for a pad in the paste
/// stencil. A file writes it as `"rules"`, `"closed"` or `{"open": {"by": …}}`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Opening {
    /// The board's design settings decide whether there is an opening, and how large
    /// ([`DesignSettings::pad_mask`], [`DesignSettings::pad_paste`], [`DesignSettings::via_mask`]).
    #[default]
    Rules,
    /// An opening in the shape of the copper, grown on every side.
    Open {
        /// How much larger than the copper the opening is; smaller when negative, and an opening
        /// shrunk to nothing is no opening. `None` takes the amount from the design settings.
        #[serde(deserialize_with = "required")]
        by: Option<Nm>,
    },
    /// No opening: the solder mask covers the copper (a tented via), or the stencil has no
    /// aperture.
    Closed,
}

/// What makes a hole a slot: a round hole stretched along one axis, with round ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Slot {
    /// Overall length, end to end; longer than the hole's diameter.
    pub length: Nm,
    /// Direction of the long axis, relative to the pad the hole is in (zero is along the pad's x
    /// axis), or to the board for a hole on its own. Below half a turn, since a slot looks the
    /// same turned around.
    pub rotation: Rotation,
}

/// A hole through the board: round, or a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hole {
    /// The diameter of a round hole, or the width of a slot.
    pub diameter: Nm,
    /// Makes the hole a slot; `None` for a round hole.
    #[serde(deserialize_with = "required")]
    pub slot: Option<Slot>,
    /// Whether the hole is plated (connects copper layers).
    pub plated: bool,
}

impl Hole {
    /// A round hole.
    pub fn round(diameter: Nm, plated: bool) -> Hole {
        Hole { diameter, slot: None, plated }
    }

    /// The ends of the line the hole is drilled along, relative to its centre, for a hole turned
    /// by `turn` (its pad's rotation, or none for a hole on its own): a slot's ends, turned by
    /// `turn` and the slot's own rotation, or the centre twice for a round hole. A drill of the
    /// hole's diameter along that line cuts the hole. `None` when an end would leave the range of
    /// a coordinate.
    pub fn ends(&self, turn: Rotation) -> Option<(Point, Point)> {
        let Some(slot) = self.slot else { return Some((Point::ORIGIN, Point::ORIGIN)) };
        let reach = slot.length.0.saturating_sub(self.diameter.0).max(0) / 2;
        let turn = Rotation::from_millidegrees(i64::from(turn.0) + i64::from(slot.rotation.0));
        let end = |x: i64| turn.turn(Point::new(Nm(x), Nm(0)));
        Some((end(-reach)?, end(reach)?))
    }
}

/// Where a point of a text's lettering, laid out around its anchor at the origin, lies on what
/// the text is placed on (the board, or a footprint before [`Footprint::to_board`]): mirrored left
/// to right about the anchor if the text is mirrored, turned by its rotation and moved to its
/// anchor. `None` when it would leave the range of a coordinate.
pub fn text_point(p: Point, mirrored: bool, rotation: Rotation, anchor: Point) -> Option<Point> {
    let p = if mirrored { Point::new(Nm(p.x.0.checked_neg()?), p.y) } else { p };
    rotation.turn(p)?.shifted(anchor)
}

/// What a pad has on one outer side of the board.
///
/// The mask and paste openings belong to the copper. An opening without copper under it (a paste
/// aperture that is one of several over a large pad, say) is a footprint drawing on the mask or
/// paste layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PadFace {
    /// Copper shape.
    pub shape: PadShape,
    /// Solder mask opening over the copper.
    pub mask: Opening,
    /// Paste stencil aperture over the copper.
    pub paste: Opening,
}

/// One pad of a footprint, in footprint coordinates.
///
/// A pad has copper or a hole, or both. A surface-mount pad has only a `front`; a through-hole
/// pad has copper on every layer around a plated hole; a mounting hole can be a hole alone. On a
/// side where a hole has no copper, the design rules decide the solder mask opening around it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pad {
    /// The pad number or name, matching a symbol pin (`1`, `A3`, `EP`). Empty for a pad that
    /// belongs to no pin, such as a mounting hole.
    pub number: String,
    /// Where the pad is, relative to the footprint origin: the centre of its hole, or of its
    /// copper when it has no hole.
    pub position: Point,
    /// Rotation relative to the footprint.
    pub rotation: Rotation,
    /// The centre of the copper shapes relative to `position`, before the pad's rotation. Zero
    /// unless the copper sits off centre around the hole.
    pub copper_offset: Point,
    /// Copper, mask and paste on the outer layer of the footprint's own side.
    #[serde(deserialize_with = "required")]
    pub front: Option<PadFace>,
    /// Copper on the inner copper layers; they all share one shape.
    #[serde(deserialize_with = "required")]
    pub inner: Option<PadShape>,
    /// Copper, mask and paste on the outer layer of the opposite side.
    #[serde(deserialize_with = "required")]
    pub back: Option<PadFace>,
    /// The hole through the board.
    #[serde(deserialize_with = "required")]
    pub hole: Option<Hole>,
    /// The net it connects to.
    #[serde(deserialize_with = "required")]
    pub net: Option<NetId>,
}

impl Pad {
    /// A surface-mount pad without a net: copper on the footprint's own side only, with mask and
    /// paste openings from the design rules.
    pub fn smd(number: &str, position: Point, shape: PadShape) -> Pad {
        let front = PadFace { shape, mask: Opening::Rules, paste: Opening::Rules };
        Pad { front: Some(front), ..Pad::bare(number, position) }
    }

    /// A through-hole pad without a net: the same copper shape on every layer, centred on `hole`,
    /// with mask openings from the design rules and no paste.
    pub fn through_hole(number: &str, position: Point, shape: PadShape, hole: Hole) -> Pad {
        let face = PadFace { shape: shape.clone(), mask: Opening::Rules, paste: Opening::Closed };
        Pad { front: Some(face.clone()), inner: Some(shape), back: Some(face), hole: Some(hole), ..Pad::bare(number, position) }
    }

    /// A hole alone, such as a mounting hole: no copper, no number and no net.
    pub fn bare_hole(position: Point, hole: Hole) -> Pad {
        Pad { hole: Some(hole), ..Pad::bare("", position) }
    }

    /// A pad with nothing on it yet, for the constructors to fill in.
    fn bare(number: &str, position: Point) -> Pad {
        Pad {
            number: number.to_owned(),
            position,
            rotation: Rotation(0),
            copper_offset: Point::ORIGIN,
            front: None,
            inner: None,
            back: None,
            hole: None,
            net: None,
        }
    }
}

/// A line, arc or circle drawn on a layer (silkscreen, outline, documentation). A file writes
/// it under its kind: `{"line": {"from": …, "to": …}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum GraphicShape {
    /// A straight line.
    Line {
        /// Start.
        from: Point,
        /// End.
        to: Point,
    },
    /// An arc.
    Arc {
        /// The arc.
        arc: Arc,
    },
    /// A circle outline.
    Circle {
        /// Centre.
        center: Point,
        /// Radius.
        radius: Nm,
    },
    /// A closed outline, optionally filled.
    Polygon {
        /// The outline.
        outline: Contour,
        /// Whether the inside is filled.
        filled: bool,
    },
}

/// A drawing on a non-copper or copper layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Graphic {
    /// Identity.
    pub id: ItemId,
    /// The layer it is drawn on.
    pub layer: LayerId,
    /// Pen width (zero for a filled polygon without outline).
    pub width: Nm,
    /// What is drawn.
    pub shape: GraphicShape,
}

/// Horizontal anchoring of text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Justify {
    /// The position is the left end.
    Left,
    /// The position is the centre.
    #[default]
    Center,
    /// The position is the right end.
    Right,
}

/// Text on a layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Text {
    /// Identity.
    pub id: ItemId,
    /// The layer it is on.
    pub layer: LayerId,
    /// The text itself.
    pub text: String,
    /// Anchor point.
    pub position: Point,
    /// Character height.
    pub height: Nm,
    /// Stroke width of the drafting font.
    pub stroke: Nm,
    /// Rotation, applied after the text is mirrored.
    pub rotation: Rotation,
    /// Mirrored left to right about its anchor, before it is turned: how text on the bottom of
    /// the board reads correctly from there. The same order as a footprint on the bottom
    /// ([`Footprint::to_board`]).
    pub mirrored: bool,
    /// Horizontal anchoring.
    pub justify: Justify,
}

/// What a footprint's text says. A file writes it as `"reference"`, `"value"` or
/// `{"fixed": "PIN 1"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextContent {
    /// The footprint's reference designator ([`Footprint::reference`]).
    Reference,
    /// The footprint's value ([`Footprint::value`]).
    Value,
    /// Words of its own (`PIN 1`, a polarity mark), never blank.
    Fixed(String),
}

/// Text that belongs to a footprint, in footprint coordinates: its designator on the silkscreen,
/// its value on the fabrication drawing, or a fixed label. A footprint can show its reference or
/// value in several places, and each stays in step with the footprint because none stores a copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FootprintText {
    /// Identity.
    pub id: ItemId,
    /// What it says.
    pub content: TextContent,
    /// Whether it is drawn. A hidden text keeps its place, so showing it again puts it back.
    pub visible: bool,
    /// The layer it is on, named as for a footprint on the top side; a bottom-side footprint
    /// flips it ([`crate::LayerStack::flipped`]).
    pub layer: LayerId,
    /// Anchor point, relative to the footprint origin.
    pub position: Point,
    /// Character height.
    pub height: Nm,
    /// Stroke width of the drafting font.
    pub stroke: Nm,
    /// Rotation relative to the footprint.
    pub rotation: Rotation,
    /// Mirrored against the footprint, about its anchor and before it is turned, as [`Text`]
    /// is. A bottom-side footprint already mirrors everything it holds, so its text reads
    /// correctly from the bottom with this off.
    pub mirrored: bool,
    /// Horizontal anchoring.
    pub justify: Justify,
}

/// Where a footprint came from: a footprint in a library. Nothing follows the link yet; it is
/// kept so the footprint can later be compared with its library and updated from it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryLink {
    /// The library's name. Empty when the source names the footprint alone, never blank.
    pub library: String,
    /// The footprint's name in that library (`R_0603`).
    pub footprint: String,
}

/// How a component is attached to the board. Whether there is something to buy, place or fit is
/// for the flags of [`Assembly`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mounting {
    /// Soldered onto pads on the surface.
    SurfaceMount,
    /// Leads soldered into holes.
    ThroughHole,
    /// Neither: pressed in, screwed on or wired by hand, or a footprint that is part of the board
    /// itself (a fiducial, a logo, a mounting hole).
    Other,
}

/// What assembly needs to know about a footprint. The three flags are independent of `mounting`
/// and of each other: a fiducial is no part to buy, yet the placement machine needs its position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assembly {
    /// How the component is put on the board. It is stored, not worked out from the pads: a
    /// surface-mount connector with through-hole pegs is still placed as a surface-mount part.
    pub mounting: Mounting,
    /// Whether the bill of materials lists it.
    pub in_bom: bool,
    /// Whether the pick-and-place file lists it.
    pub in_placement: bool,
    /// Whether the component is fitted when the board is assembled; a footprint that is not
    /// fitted keeps its pads and drawings and stays empty. `true` for a footprint without a
    /// component, which leaves nothing off the board.
    pub fitted: bool,
}

/// A placed component's pads, drawings and text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Footprint {
    /// Identity, shared with the schematic symbol once schematics exist.
    pub id: ItemId,
    /// Reference designator (`R1`, `U3`). `texts` say where it is shown.
    pub reference: String,
    /// Value (`10k`, `ATmega328P`). `texts` say where it is shown.
    pub value: String,
    /// The library footprint it was placed from; `None` for one drawn on the board.
    #[serde(deserialize_with = "required")]
    pub link: Option<LibraryLink>,
    /// What assembly needs to know about it.
    pub assembly: Assembly,
    /// Origin on the board.
    pub position: Point,
    /// Rotation about the origin.
    pub rotation: Rotation,
    /// Mounting side.
    pub side: Side,
    /// Pads, in footprint coordinates (at most [`MAX_PADS`]).
    #[serde(deserialize_with = "pads")]
    pub pads: Vec<Pad>,
    /// Silkscreen, courtyard and fabrication drawings, in footprint coordinates. Their layers are
    /// named as for a footprint on the top side; a bottom-side footprint flips them
    /// ([`crate::LayerStack::flipped`]).
    /// At most [`MAX_FOOTPRINT_GRAPHICS`].
    #[serde(deserialize_with = "footprint_graphics")]
    pub graphics: Vec<Graphic>,
    /// Designators, values and labels, in footprint coordinates (at most
    /// [`MAX_FOOTPRINT_TEXTS`]). A footprint without any shows no designator.
    #[serde(deserialize_with = "footprint_texts")]
    pub texts: Vec<FootprintText>,
}

impl Footprint {
    /// Where a point of the footprint is on the board. A footprint on the bottom side is
    /// mirrored left to right first (x becomes -x), then every footprint is turned by its
    /// rotation and moved to its position. `None` when the point would leave the range of a
    /// coordinate.
    pub fn to_board(&self, p: Point) -> Option<Point> {
        self.rotation.turn(self.mirrored(p)?)?.shifted(self.position)
    }

    /// Where a point given relative to `pad`'s position is on the board, without the pad's own
    /// rotation. Holes are placed this way; [`Hole::ends`] turns a slot.
    pub fn pad_to_board(&self, pad: &Pad, p: Point) -> Option<Point> {
        self.to_board(p.shifted(pad.position)?)
    }

    /// Where a point of `pad`'s copper, given relative to the copper's centre, is on the board:
    /// moved by the pad's copper offset, turned with the pad, then placed as
    /// [`Footprint::pad_to_board`] places it.
    pub fn pad_copper_to_board(&self, pad: &Pad, p: Point) -> Option<Point> {
        self.pad_to_board(pad, pad.rotation.turn(p.shifted(pad.copper_offset)?)?)
    }

    /// The point of the footprint that is at `p` on the board: the reverse of
    /// [`Footprint::to_board`], to the nearest nanometre.
    pub fn to_footprint(&self, p: Point) -> Option<Point> {
        let p = Point::new(Nm(p.x.0.checked_sub(self.position.x.0)?), Nm(p.y.0.checked_sub(self.position.y.0)?));
        self.mirrored(self.rotation.reversed().turn(p)?)
    }

    /// `p` mirrored left to right when the footprint is on the bottom side.
    fn mirrored(&self, p: Point) -> Option<Point> {
        match self.side {
            Side::Top => Some(p),
            Side::Bottom => Some(Point::new(Nm(p.x.0.checked_neg()?), p.y)),
        }
    }

    /// What `text` says on this footprint: the reference, the value or the text's own words.
    pub fn wording<'a>(&'a self, text: &'a FootprintText) -> &'a str {
        match &text.content {
            TextContent::Reference => &self.reference,
            TextContent::Value => &self.value,
            TextContent::Fixed(words) => words,
        }
    }
}

/// The most pads one footprint may have (the largest packages have a few thousand).
pub const MAX_PADS: usize = 100_000;

/// The most drawings one footprint may have.
pub const MAX_FOOTPRINT_GRAPHICS: usize = 100_000;

/// The most texts one footprint may have.
pub const MAX_FOOTPRINT_TEXTS: usize = 10_000;

fn footprint_texts<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<FootprintText>, D::Error> {
    crate::store::bounded_vec(d, MAX_FOOTPRINT_TEXTS)
}

fn pads<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Pad>, D::Error> {
    crate::store::bounded_vec(d, MAX_PADS)
}

fn footprint_graphics<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Graphic>, D::Error> {
    crate::store::bounded_vec(d, MAX_FOOTPRINT_GRAPHICS)
}

/// The centre line of a track. A file writes it under its kind: `{"line": {"from": …, "to": …}}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum TrackPath {
    /// A straight segment.
    Line {
        /// Start.
        from: Point,
        /// End.
        to: Point,
    },
    /// An arc.
    Arc {
        /// The arc.
        arc: Arc,
    },
}

impl TrackPath {
    /// The two ends.
    pub fn ends(&self) -> (Point, Point) {
        match *self {
            TrackPath::Line { from, to } => (from, to),
            TrackPath::Arc { arc } => (arc.start, arc.end),
        }
    }
}

/// A copper track segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Track {
    /// Identity.
    pub id: ItemId,
    /// Copper layer.
    pub layer: LayerId,
    /// Copper width.
    pub width: Nm,
    /// Centre line.
    pub path: TrackPath,
    /// The net it carries.
    #[serde(deserialize_with = "required")]
    pub net: Option<NetId>,
}

/// A plated hole connecting copper layers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Via {
    /// Identity.
    pub id: ItemId,
    /// Centre.
    pub position: Point,
    /// Copper ring diameter.
    pub diameter: Nm,
    /// Drill diameter.
    pub drill: Nm,
    /// Topmost copper layer it connects.
    pub from: LayerId,
    /// Bottommost copper layer it connects.
    pub to: LayerId,
    /// Solder mask over the via on the top side of the board ([`Opening::Closed`] tents it). It
    /// matters only for a via that reaches that side.
    pub top_mask: Opening,
    /// Solder mask over the via on the bottom side of the board.
    pub bottom_mask: Opening,
    /// The net it carries.
    #[serde(deserialize_with = "required")]
    pub net: Option<NetId>,
}

/// How a zone's copper is laid down inside its outline. A file writes it as `"solid"` or
/// `{"hatched": {"width": …, "gap": …}}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ZoneFill {
    /// Copper everywhere.
    Solid,
    /// A grid of copper lines, running both ways.
    Hatched {
        /// The width of each line.
        width: Nm,
        /// The gap between neighbouring lines.
        gap: Nm,
    },
}

/// How a zone joins the pads on its own net. A file writes it as `"solid"`, `"none"` or
/// `{"relief": {"gap": …, "spoke": …}}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum PadConnection {
    /// Copper all the way around the pad: the best connection, and the hardest to solder by hand.
    Solid,
    /// A thermal relief: a gap around the pad, bridged by spokes, so the pad heats up when it is
    /// soldered.
    Relief {
        /// The gap between the pad and the zone's copper.
        gap: Nm,
        /// The width of each spoke.
        spoke: Nm,
    },
    /// No connection: the zone keeps clear of these pads as if they were on another net.
    None,
}

/// A copper area (pour) on one layer, filled around everything it must keep clear of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Zone {
    /// Identity.
    pub id: ItemId,
    /// Copper layer.
    pub layer: LayerId,
    /// The net it connects to (usually a ground or supply).
    #[serde(deserialize_with = "required")]
    pub net: Option<NetId>,
    /// The area to fill.
    pub outline: Contour,
    /// Areas inside the outline that this zone leaves empty (at most [`MAX_ZONE_HOLES`]). An area
    /// that every zone on a layer must stay out of is a [`Keepout`].
    #[serde(deserialize_with = "zone_holes")]
    pub holes: Vec<Contour>,
    /// Clearance to copper on other nets.
    pub clearance: Nm,
    /// The narrowest copper the fill may leave; narrower necks and slivers are removed.
    pub min_width: Nm,
    /// Higher priority zones fill first where zones overlap.
    pub priority: u32,
    /// Solid copper or a hatch.
    pub fill: ZoneFill,
    /// How it joins the pads on its net.
    pub pads: PadConnection,
    /// Whether copper that ends up connected to nothing on the zone's net is kept.
    pub keep_islands: bool,
    /// The copper as it was last poured, clearances and thermal reliefs included: at most
    /// [`MAX_ZONE_POLYGONS`] areas and [`circuitcraft_geom::MAX_POINTS`] points in all, every
    /// area and every hole in one with at least three corners. Empty for a zone that has not
    /// been poured. Nothing pours zones yet, so this is what the file the
    /// board came from held, and it is not kept in step with the rest of the board.
    #[serde(deserialize_with = "zone_polygons")]
    pub filled: Vec<Polygon>,
}

/// The most holes one zone may have.
pub const MAX_ZONE_HOLES: usize = 10_000;

/// The most separate areas of copper one poured zone may have.
pub const MAX_ZONE_POLYGONS: usize = 100_000;

fn zone_holes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Contour>, D::Error> {
    crate::store::bounded_vec(d, MAX_ZONE_HOLES)
}

/// Reads poured copper, refusing a ring with fewer than three corners as soon as it has been
/// read. Without that, a file of empty rings would take many times its own size in memory
/// before [`crate::Board::validate`] saw it; with it, a ring costs about what its text does.
fn zone_polygons<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Polygon>, D::Error> {
    struct Ring(Vec<Point>);
    impl<'de> Deserialize<'de> for Ring {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Ring, D::Error> {
            let points = Vec::<Point>::deserialize(d)?;
            if points.len() < 3 { Err(serde::de::Error::custom(FEW_CORNERS)) } else { Ok(Ring(points)) }
        }
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Area {
        outer: Ring,
        holes: Vec<Ring>,
    }
    let areas: Vec<Area> = crate::store::bounded_vec(d, MAX_ZONE_POLYGONS)?;
    Ok(areas.into_iter().map(|a| Polygon { outer: a.outer.0, holes: a.holes.into_iter().map(|r| r.0).collect() }).collect())
}

/// Why a ring of poured copper is refused.
pub(crate) const FEW_CORNERS: &str = "an area or a hole in one needs at least three corners";

/// An area on one copper layer that tracks, vias or zones must stay out of.
///
/// This is the keepout that says what it keeps out, and on which layer, inner layers included.
/// A drawing on a keepout layer ([`crate::LayerKind::Keepout`]) keeps all copper out, on an
/// outer layer or on every layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keepout {
    /// Identity.
    pub id: ItemId,
    /// The copper layer it applies to.
    pub layer: LayerId,
    /// The area.
    pub outline: Contour,
    /// Whether tracks must stay out.
    pub tracks: bool,
    /// Whether vias must stay out, including vias that only pass through this layer.
    pub vias: bool,
    /// Whether the copper of zones must stay out.
    pub zones: bool,
}

/// A hole through the board that belongs to no footprint, such as a mounting hole drilled
/// straight into the board. It has no copper and no net.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardHole {
    /// Identity.
    pub id: ItemId,
    /// The centre of the hole.
    pub position: Point,
    /// The hole.
    pub hole: Hole,
}

/// Values the whole board shares: what items fall back on when they don't say otherwise, what
/// new items start with, and where outputs measure from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignSettings {
    /// How much larger than the copper a solder mask opening is, on every side; smaller when
    /// negative.
    pub mask_expansion: Nm,
    /// How much larger than the copper a paste aperture is, on every side; usually zero or
    /// negative.
    pub paste_expansion: Nm,
    /// Whether solder mask covers a via that leaves it to the settings ([`Opening::Rules`]).
    pub tent_vias: bool,
    /// The clearance between copper on different nets.
    pub clearance: Nm,
    /// The width of a new track.
    pub track_width: Nm,
    /// The copper diameter of a new via.
    pub via_diameter: Nm,
    /// The drill diameter of a new via; smaller than `via_diameter`.
    pub via_drill: Nm,
    /// The point that drill and placement files measure from.
    pub origin: Point,
}

impl Default for DesignSettings {
    /// Values most board houses can make: a 0.05 mm mask expansion, paste the size of the pad,
    /// tented vias, 0.2 mm clearance, 0.25 mm tracks and 0.6 mm vias with a 0.3 mm drill.
    fn default() -> Self {
        DesignSettings {
            mask_expansion: Nm(50_000),
            paste_expansion: Nm(0),
            tent_vias: true,
            clearance: Nm(200_000),
            track_width: Nm(250_000),
            via_diameter: Nm(600_000),
            via_drill: Nm(300_000),
            origin: Point::ORIGIN,
        }
    }
}

impl DesignSettings {
    /// How much larger than its copper the solder mask opening of a pad is, or `None` when the
    /// mask covers the pad.
    pub fn pad_mask(&self, opening: Opening) -> Option<Nm> {
        opening.grown(true, self.mask_expansion)
    }

    /// How much larger than its copper the paste aperture of a pad is, or `None` when the stencil
    /// has none.
    pub fn pad_paste(&self, opening: Opening) -> Option<Nm> {
        opening.grown(true, self.paste_expansion)
    }

    /// How much larger than its copper the solder mask opening of a via is, or `None` when the
    /// mask covers the via.
    pub fn via_mask(&self, opening: Opening) -> Option<Nm> {
        opening.grown(!self.tent_vias, self.mask_expansion)
    }
}

impl Opening {
    /// How much the opening grows its copper by, or `None` for no opening: the settings open
    /// what leaves it to them when `open`, and `amount` is their expansion.
    fn grown(self, open: bool, amount: Nm) -> Option<Nm> {
        match self {
            Opening::Rules => open.then_some(amount),
            Opening::Open { by } => Some(by.unwrap_or(amount)),
            Opening::Closed => None,
        }
    }
}

macro_rules! keyed {
    ($($t:ty => $id:ty),* $(,)?) => {
        $(impl Keyed for $t {
            type Id = $id;
            fn id(&self) -> $id {
                self.id
            }
        })*
    };
}

keyed!(
    Net => NetId,
    Footprint => ItemId,
    Track => ItemId,
    Via => ItemId,
    Zone => ItemId,
    Keepout => ItemId,
    BoardHole => ItemId,
    Text => ItemId,
    Graphic => ItemId,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_ids_are_distinct_v4_uuids() {
        let a = ItemId::random().expect("random");
        let b = ItemId::random().expect("random");
        assert_ne!(a, b);
        assert!(!a.is_nil());
        let text = a.to_string();
        assert_eq!(text.len(), 36);
        assert_eq!(text.chars().nth(14), Some('4'), "version 4");
        assert_eq!(text.parse::<ItemId>(), Ok(a));
    }

    #[test]
    fn ids_are_written_as_hyphenated_strings() {
        let id = ItemId::from_u128(0x3f2c9a10_6b7e_4d21_9a0c_58e1f4b2d7a3);
        let json = serde_json::to_string(&id).expect("json");
        assert_eq!(json, r#""3f2c9a10-6b7e-4d21-9a0c-58e1f4b2d7a3""#);
        assert_eq!(serde_json::from_str::<ItemId>(&json).ok(), Some(id));
        for bad in [r#""""#, r#""not-a-uuid""#, "42", r#""3f2c9a10-6b7e-4d21-9a0c""#] {
            assert!(serde_json::from_str::<ItemId>(bad).is_err(), "{bad}");
        }
        assert!(matches!("zz".parse::<NetId>(), Err(ModelError::BadId(_))));
    }

    #[test]
    fn a_footprint_places_its_points_on_the_board_and_back() {
        let p = |x: i64, y: i64| Point::new(Nm(x), Nm(y));
        let footprint = |rotation: i32, side: Side| Footprint {
            id: ItemId::from_u128(1),
            reference: "U1".to_owned(),
            value: String::new(),
            link: None,
            assembly: Assembly { mounting: Mounting::SurfaceMount, in_bom: true, in_placement: true, fitted: true },
            position: p(1_000, 2_000),
            rotation: Rotation(rotation),
            side,
            pads: Vec::new(),
            graphics: Vec::new(),
            texts: Vec::new(),
        };
        // A pad 100 to the right of the origin and 10 up.
        let pad = p(100, 10);
        assert_eq!(footprint(0, Side::Top).to_board(pad), Some(p(1_100, 2_010)));
        assert_eq!(footprint(90_000, Side::Top).to_board(pad), Some(p(990, 2_100)), "a quarter turn counter-clockwise");
        assert_eq!(footprint(180_000, Side::Top).to_board(pad), Some(p(900, 1_990)));
        assert_eq!(footprint(270_000, Side::Top).to_board(pad), Some(p(1_010, 1_900)));
        // On the bottom the pad is mirrored left to right, then turned.
        assert_eq!(footprint(0, Side::Bottom).to_board(pad), Some(p(900, 2_010)));
        assert_eq!(footprint(90_000, Side::Bottom).to_board(pad), Some(p(990, 1_900)));
        // 45 degrees: 100 along the diagonal, to the nearest nanometre.
        assert_eq!(footprint(45_000, Side::Top).to_board(p(100, 0)), Some(p(1_071, 2_071)));
        for rotation in [0, 30_000, 45_000, 90_000, 123_456, 180_000, 270_000, 359_999] {
            for side in [Side::Top, Side::Bottom] {
                let f = footprint(rotation, side);
                let there = f.to_board(p(123_456, -654_321)).expect("in range");
                let back = f.to_footprint(there).expect("in range");
                assert!((back.x.0 - 123_456).abs() <= 1 && (back.y.0 + 654_321).abs() <= 1, "{rotation} {side:?}: {back:?}");
            }
        }
        // Points that would leave the range of a coordinate are refused, not wrapped.
        assert_eq!(footprint(0, Side::Top).to_board(p(i64::MAX, 0)), None);
        assert_eq!(footprint(90_000, Side::Top).to_board(p(0, i64::MIN)), None);
        assert_eq!(footprint(45_000, Side::Top).to_board(p(i64::MAX, i64::MAX)), None);
        assert_eq!(footprint(0, Side::Bottom).to_board(p(i64::MIN, 0)), None);
        assert_eq!(footprint(0, Side::Top).to_footprint(p(i64::MIN, 0)), None);
        // A turn alone says so too, rather than hand back a coordinate cut off at the largest.
        assert_eq!(Rotation(45_000).turn(p(i64::MAX, i64::MAX)), None);
        assert_eq!(Rotation(30_000).turn(p(i64::MIN, 0)), None);
        // Just inside what a coordinate holds, it still turns: to the diagonal, give or take the
        // rounding of numbers this large.
        let turned = Rotation(45_000).turn(p(4_000_000_000_000_000_000, 0));
        assert!(turned.is_some_and(|q| (q.x.0 - q.y.0).abs() < 4_096 && (q.x.0 - 2_828_427_124_746_190_097).abs() < 4_096), "{turned:?}");
    }

    #[test]
    fn a_pad_its_hole_and_a_text_are_placed_on_the_board() {
        let p = |x: i64, y: i64| Point::new(Nm(x), Nm(y));
        let mut f = Footprint {
            id: ItemId::from_u128(1),
            reference: "J1".to_owned(),
            value: String::new(),
            link: None,
            assembly: Assembly { mounting: Mounting::ThroughHole, in_bom: true, in_placement: true, fitted: true },
            position: p(1_000, 2_000),
            rotation: Rotation(90_000),
            side: Side::Bottom,
            pads: Vec::new(),
            graphics: Vec::new(),
            texts: Vec::new(),
        };
        let slot = Hole { diameter: Nm(100), slot: Some(Slot { length: Nm(300), rotation: Rotation(0) }), plated: true };
        let mut pad = Pad::through_hole("1", p(100, 0), PadShape::Circle { diameter: Nm(400) }, slot);
        pad.rotation = Rotation(90_000);
        pad.copper_offset = p(50, 0);
        // The pad's place: mirrored to (-100, 0), turned a quarter to (0, -100), moved.
        assert_eq!(f.pad_to_board(&pad, Point::ORIGIN), Some(p(1_000, 1_900)));
        // Its copper centre: the offset (50, 0) turned with the pad to (0, 50), then placed as the
        // pad is: (100, 50), mirrored (-100, 50), turned (-50, -100), moved.
        assert_eq!(f.pad_copper_to_board(&pad, Point::ORIGIN), Some(p(950, 1_900)));
        // The slot runs 100 either way from the centre, along the pad turned a quarter.
        assert_eq!(slot.ends(pad.rotation), Some((p(0, -100), p(0, 100))));
        assert_eq!(slot.ends(Rotation(0)), Some((p(-100, 0), p(100, 0))));
        // A slot's own rotation adds to the pad's, past a whole turn too.
        let turned = Hole { slot: Some(Slot { length: Nm(300), rotation: Rotation(90_000) }), ..slot };
        assert_eq!(turned.ends(Rotation(0)), Some((p(0, -100), p(0, 100))));
        assert_eq!(turned.ends(Rotation(270_000)), Some((p(-100, 0), p(100, 0))));
        assert_eq!(Hole::round(Nm(100), false).ends(Rotation(45_000)), Some((Point::ORIGIN, Point::ORIGIN)));
        // A slot no longer than its drill is a round hole.
        let short = Hole { slot: Some(Slot { length: Nm(50), rotation: Rotation(0) }), ..slot };
        assert_eq!(short.ends(Rotation(0)), Some((Point::ORIGIN, Point::ORIGIN)));
        // A text's lettering: mirrored about its anchor, turned, moved.
        assert_eq!(text_point(p(10, 1), true, Rotation(90_000), p(5, 5)), Some(p(4, -5)));
        assert_eq!(text_point(p(10, 1), false, Rotation(0), p(5, 5)), Some(p(15, 6)));
        // Out of range is refused, not wrapped.
        assert_eq!(text_point(p(i64::MIN, 0), true, Rotation(0), Point::ORIGIN), None);
        assert_eq!(text_point(p(1, 0), false, Rotation(0), p(i64::MAX, 0)), None);
        let far = Pad { copper_offset: p(i64::MAX, 0), ..pad.clone() };
        assert_eq!(f.pad_copper_to_board(&far, p(1, 0)), None);
        (f.position, f.rotation, f.side) = (p(i64::MAX, 0), Rotation(0), Side::Top);
        assert_eq!(f.pad_to_board(&pad, Point::ORIGIN), None);
    }

    #[test]
    fn rotation_normalises() {
        assert_eq!(Rotation::from_millidegrees(-90_000), Rotation(270_000));
        assert_eq!(Rotation::from_millidegrees(720_000), Rotation(0));
        assert!((0..360_000).contains(&Rotation::from_millidegrees(i64::MIN).0));
        assert!((Rotation(45_500).degrees() - 45.5).abs() < 1e-12);
    }
}
