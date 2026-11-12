//! The design database.
//!
//! Today a [`Board`] is a name and a [`LayerStack`]. Pads, tracks, vias, pours, footprints and nets,
//! and the copy-on-write store that makes undo cheap, arrive in 0.1 (see ROADMAP.md).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use circuitcraft_units::Nm;
use serde::{Deserialize, Serialize};

/// What a layer is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerKind {
    /// Copper (signal or plane).
    Copper,
    /// Insulating material between copper layers.
    Dielectric,
    /// Solder mask.
    SolderMask,
    /// Solder paste (stencil apertures).
    Paste,
    /// Silkscreen (legend).
    Silkscreen,
    /// The board outline and cutouts.
    Outline,
}

/// One layer of the board, listed top to bottom in a [`LayerStack`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layer {
    /// Display name, unique within the stack.
    pub name: String,
    /// What the layer is for.
    pub kind: LayerKind,
    /// Physical thickness, where it matters (copper and dielectric).
    pub thickness: Option<Nm>,
}

impl Layer {
    fn new(name: &str, kind: LayerKind, thickness_um: Option<i64>) -> Layer {
        Layer { name: name.to_owned(), kind, thickness: thickness_um.map(|um| Nm(um * 1_000)) }
    }
}

/// The physical layers of a board, top to bottom, plus the outline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerStack {
    /// Layers, top to bottom.
    pub layers: Vec<Layer>,
}

impl LayerStack {
    /// A common 1.6 mm two-layer stack: 35 µm copper (1 oz) on a 1.5 mm core.
    pub fn two_layer() -> LayerStack {
        use LayerKind::{Copper, Dielectric, Outline, Paste, Silkscreen, SolderMask};
        LayerStack {
            layers: vec![
                Layer::new("Top Paste", Paste, None),
                Layer::new("Top Silkscreen", Silkscreen, None),
                Layer::new("Top Solder Mask", SolderMask, Some(10)),
                Layer::new("Top Copper", Copper, Some(35)),
                Layer::new("Core", Dielectric, Some(1_510)),
                Layer::new("Bottom Copper", Copper, Some(35)),
                Layer::new("Bottom Solder Mask", SolderMask, Some(10)),
                Layer::new("Bottom Silkscreen", Silkscreen, None),
                Layer::new("Bottom Paste", Paste, None),
                Layer::new("Outline", Outline, None),
            ],
        }
    }

    /// The number of copper layers.
    pub fn copper_count(&self) -> usize {
        self.layers.iter().filter(|l| l.kind == LayerKind::Copper).count()
    }

    /// Total thickness of every layer that has one, saturating rather than overflowing.
    pub fn thickness(&self) -> Nm {
        Nm(self.layers.iter().filter_map(|l| l.thickness).fold(0_i64, |sum, t| sum.saturating_add(t.0)))
    }
}

/// A printed circuit board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Board {
    /// The board's name, shown in title bars and outputs.
    pub name: String,
    /// Its layers.
    pub stack: LayerStack,
}

impl Board {
    /// A new, empty two-layer board.
    pub fn new(name: &str) -> Board {
        Board { name: name.to_owned(), stack: LayerStack::two_layer() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_layer_stack_is_one_point_six_mm() {
        let s = LayerStack::two_layer();
        assert_eq!(s.copper_count(), 2);
        assert_eq!(s.thickness(), Nm(1_600_000));
    }

    #[test]
    fn layer_names_are_unique() {
        let s = LayerStack::two_layer();
        let mut names: Vec<_> = s.layers.iter().map(|l| l.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), s.layers.len());
    }
}
