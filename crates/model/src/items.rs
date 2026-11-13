//! What a board is made of: nets, footprints with pads, tracks, vias, zones, text and drawings.
//!
//! Coordinates are board coordinates in [`Nm`], y up, except pads and footprint drawings, which
//! are relative to their footprint's origin before its rotation and side are applied.

use crate::LayerId;
use crate::store::Keyed;
use circuitcraft_geom::{Arc, Contour};
use circuitcraft_units::{Nm, Point};
use serde::{Deserialize, Serialize};

/// The identity of a board item, stable for its life and unique within the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ItemId(pub u64);

/// The identity of a net.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NetId(pub u64);

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

/// Which side of the board a footprint is mounted on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// The top side.
    #[default]
    Top,
    /// The bottom side (mirrored).
    Bottom,
}

/// A connection: everything on the same net must end up connected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Net {
    /// Identity.
    pub id: NetId,
    /// Name, unique within the board (`GND`, `VCC`, `Net-(R1-Pad2)`).
    pub name: String,
}

/// The copper shape of a pad.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PadShape {
    /// A circle.
    Circle {
        /// Diameter.
        diameter: Nm,
    },
    /// A rectangle centred on the pad position.
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
    /// Any outline, relative to the pad position.
    Custom {
        /// The outline.
        outline: Contour,
    },
}

/// A drilled hole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Drill {
    /// Hole diameter.
    pub diameter: Nm,
    /// Whether the hole is plated (connects copper layers).
    pub plated: bool,
}

/// Which layers a pad is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PadKind {
    /// A surface-mount pad on the outer copper layer of its footprint's side.
    Smd,
    /// A pad on every copper layer around a drilled hole.
    ThroughHole {
        /// The hole.
        drill: Drill,
    },
}

/// One pad of a footprint, in footprint coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pad {
    /// The pad number or name, matching a symbol pin (`1`, `A3`, `EP`).
    pub number: String,
    /// Centre, relative to the footprint origin.
    pub position: Point,
    /// Rotation relative to the footprint.
    pub rotation: Rotation,
    /// Copper shape.
    pub shape: PadShape,
    /// SMD or through-hole.
    pub kind: PadKind,
    /// The net it connects to.
    pub net: Option<NetId>,
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

/// A placed component's copper pads and drawings.
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
    /// the top-side layers; a bottom-side footprint flips them. At most [`MAX_FOOTPRINT_GRAPHICS`].
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
    fn rotation_normalises() {
        assert_eq!(Rotation::from_millidegrees(-90_000), Rotation(270_000));
        assert_eq!(Rotation::from_millidegrees(720_000), Rotation(0));
        assert!((0..360_000).contains(&Rotation::from_millidegrees(i64::MIN).0));
        assert!((Rotation(45_500).degrees() - 45.5).abs() < 1e-12);
    }
}
