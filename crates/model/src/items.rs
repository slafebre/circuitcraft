//! What a board is made of: nets, footprints with pads, tracks, vias, zones, text and drawings.
//!
//! Coordinates are board coordinates in [`Nm`], y up, except pads and footprint drawings, which
//! are relative to their footprint's origin before its rotation and side are applied.

use crate::store::Keyed;
use crate::{LayerId, ModelError};
use circuitcraft_geom::{Arc, Contour};
use circuitcraft_units::{Nm, Point};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

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
}

/// A side of the board: the one a footprint is mounted on, or the one a layer belongs to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// The top side.
    #[default]
    Top,
    /// The bottom side (mirrored).
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
pub struct Net {
    /// Identity.
    pub id: NetId,
    /// Name, unique within the board (`GND`, `VCC`, `Net-(R1-Pad2)`).
    pub name: String,
}

/// The copper shape of a pad, placed on the pad's copper centre ([`Pad::copper_offset`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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
/// stencil.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Opening {
    /// The board's design rules decide whether there is an opening, and how large.
    #[default]
    Rules,
    /// An opening in the shape of the copper, grown on every side.
    Open {
        /// How much larger than the copper the opening is; smaller when negative, and an opening
        /// shrunk to nothing is no opening. `None` takes the amount from the design rules.
        by: Option<Nm>,
    },
    /// No opening: the solder mask covers the copper (a tented via), or the stencil has no
    /// aperture.
    Closed,
}

/// What makes a hole a slot: a round hole stretched along one axis, with round ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Slot {
    /// Overall length, end to end; longer than the hole's diameter.
    pub length: Nm,
    /// Direction of the long axis, relative to the pad (zero is along the pad's x axis). Below
    /// half a turn, since a slot looks the same turned around.
    pub rotation: Rotation,
}

/// A hole through the board: round, or a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Hole {
    /// The diameter of a round hole, or the width of a slot.
    pub diameter: Nm,
    /// Makes the hole a slot; `None` for a round hole.
    pub slot: Option<Slot>,
    /// Whether the hole is plated (connects copper layers).
    pub plated: bool,
}

impl Hole {
    /// A round hole.
    pub fn round(diameter: Nm, plated: bool) -> Hole {
        Hole { diameter, slot: None, plated }
    }
}

/// What a pad has on one outer side of the board.
///
/// The mask and paste openings belong to the copper. An opening without copper under it (a paste
/// aperture that is one of several over a large pad, say) is a footprint drawing on the mask or
/// paste layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    pub front: Option<PadFace>,
    /// Copper on the inner copper layers; they all share one shape.
    pub inner: Option<PadShape>,
    /// Copper, mask and paste on the outer layer of the opposite side.
    pub back: Option<PadFace>,
    /// The hole through the board.
    pub hole: Option<Hole>,
    /// The net it connects to.
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

/// A line, arc or circle drawn on a layer (silkscreen, outline, documentation).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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
    /// Rotation.
    pub rotation: Rotation,
    /// Mirrored (reads correctly from the bottom).
    pub mirrored: bool,
    /// Horizontal anchoring.
    pub justify: Justify,
}

/// A placed component's pads and drawings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Footprint {
    /// Identity, shared with the schematic symbol once schematics exist.
    pub id: ItemId,
    /// Reference designator (`R1`, `U3`).
    pub reference: String,
    /// Value (`10k`, `ATmega328P`).
    pub value: String,
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
    /// the top-side layers; a bottom-side footprint flips them ([`crate::LayerStack::flipped`]).
    /// At most [`MAX_FOOTPRINT_GRAPHICS`].
    #[serde(deserialize_with = "footprint_graphics")]
    pub graphics: Vec<Graphic>,
}

/// The most pads one footprint may have (the largest packages have a few thousand).
pub const MAX_PADS: usize = 100_000;

/// The most drawings one footprint may have.
pub const MAX_FOOTPRINT_GRAPHICS: usize = 100_000;

fn pads<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Pad>, D::Error> {
    crate::store::bounded_vec(d, MAX_PADS)
}

fn footprint_graphics<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Graphic>, D::Error> {
    crate::store::bounded_vec(d, MAX_FOOTPRINT_GRAPHICS)
}

/// The centre line of a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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
    pub net: Option<NetId>,
}

/// A plated hole connecting copper layers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    pub net: Option<NetId>,
}

/// A copper area (pour) on one layer, filled around everything it must keep clear of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Zone {
    /// Identity.
    pub id: ItemId,
    /// Copper layer.
    pub layer: LayerId,
    /// The net it connects to (usually a ground or supply).
    pub net: Option<NetId>,
    /// The area to fill.
    pub outline: Contour,
    /// Clearance to copper on other nets.
    pub clearance: Nm,
    /// Higher priority zones fill first where zones overlap.
    pub priority: u32,
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

keyed!(Net => NetId, Footprint => ItemId, Track => ItemId, Via => ItemId, Zone => ItemId, Text => ItemId, Graphic => ItemId);

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
    fn rotation_normalises() {
        assert_eq!(Rotation::from_millidegrees(-90_000), Rotation(270_000));
        assert_eq!(Rotation::from_millidegrees(720_000), Rotation(0));
        assert!((0..360_000).contains(&Rotation::from_millidegrees(i64::MIN).0));
        assert!((Rotation(45_500).degrees() - 45.5).abs() < 1e-12);
    }
}
