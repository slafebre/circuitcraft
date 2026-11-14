//! Drawing a board without a window: display lists, the stroke font, and SVG and PNG output.
//!
//! This crate knows nothing about egui or wgpu. [`display_list`] turns a board into layers of
//! simple shapes that any front end can draw; [`svg`] and [`png`] draw them into files, for the
//! CLI, MCP and CI. Colours here are CircuitCraft's own defaults.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod display;
pub mod font;
mod mesh;
mod raster;
mod svg;

pub use display::{
    AREA_LINE_WIDTH, BACKGROUND, DisplayList, Group, GroupKind, HAIRLINE, HIGHLIGHT, OUTLINE_WIDTH, Options, Shape, TOLERANCE, display_list,
};
pub use mesh::{GroupMesh, meshes};
pub use raster::{MAX_PIXELS, MAX_SIDE, png};
pub use svg::svg;

use circuitcraft_model::{Layer, LayerKind};

/// A picture could not be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    /// The picture would be larger than [`MAX_SIDE`] pixels on a side or [`MAX_PIXELS`] in all.
    #[error("a picture {0} by {1} pixels is larger than this build makes")]
    TooLarge(u64, u64),
    /// A picture must be at least one pixel wide.
    #[error("a picture must be at least one pixel wide")]
    Empty,
    /// Encoding the picture failed.
    #[error("could not encode the picture: {0}")]
    Encode(String),
}

impl Rgba {
    /// The colour as `#rrggbb`, without its opacity.
    pub fn hex(self) -> String {
        let [r, g, b, _] = self.0;
        format!("#{r:02x}{g:02x}{b:02x}")
    }
}

/// An sRGB colour with straight alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgba(pub [u8; 4]);

/// The default colour for a layer. Copper layers alternate warm and cool by their position among
/// the copper layers (`copper_index`, 0 = top), so adjacent copper layers stay distinguishable.
pub fn layer_color(layer: &Layer, copper_index: usize) -> Rgba {
    match layer.kind {
        LayerKind::Copper => {
            const COPPER: [[u8; 4]; 4] = [[232, 72, 60, 255], [64, 132, 240, 255], [236, 176, 56, 255], [72, 196, 140, 255]];
            Rgba(COPPER.get(copper_index % COPPER.len()).copied().unwrap_or([200, 200, 200, 255]))
        }
        LayerKind::Dielectric => Rgba([96, 112, 72, 255]),
        LayerKind::SolderMask => Rgba([140, 60, 160, 160]),
        LayerKind::Paste => Rgba([150, 150, 160, 200]),
        LayerKind::Silkscreen => Rgba([236, 236, 220, 255]),
        LayerKind::Outline => Rgba([240, 220, 80, 255]),
        LayerKind::Courtyard => Rgba([224, 96, 200, 255]),
        LayerKind::Fabrication => Rgba([150, 170, 190, 255]),
        LayerKind::Keepout => Rgba([60, 200, 220, 255]),
        LayerKind::Mechanical => Rgba([170, 150, 230, 255]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use circuitcraft_model::LayerStack;

    #[test]
    fn adjacent_copper_layers_differ() {
        let stack = LayerStack::two_layer();
        let copper: Vec<_> = stack.layers.iter().filter(|l| l.kind == LayerKind::Copper).collect();
        assert_eq!(copper.len(), 2);
        assert_ne!(layer_color(copper[0], 0), layer_color(copper[1], 1));
    }

    #[test]
    fn every_kind_of_layer_has_its_own_colour() {
        let stack = LayerStack::two_layer();
        let distinct = |mut colours: Vec<Rgba>| {
            colours.sort_by_key(|c| c.0);
            colours.dedup();
            colours
        };
        // Layers of one kind share a colour, so the stack's other layers show nine.
        let mut colours = distinct(stack.layers.iter().filter(|l| l.kind != LayerKind::Copper).map(|l| layer_color(l, 0)).collect());
        assert_eq!(colours.len(), 9, "one colour for each kind besides copper");
        // Every copper colour, as a board with four or more copper layers shows them all.
        let copper = stack.copper().next().expect("a copper layer");
        colours.extend((0..4).map(|i| layer_color(copper, i)));
        assert_eq!(distinct(colours).len(), 13, "no copper colour is another kind's colour");
    }
}
