//! Layer colours and, from 0.1, display lists for the board view.
//!
//! This crate knows nothing about egui or wgpu: it turns the model into plain data that any front
//! end can draw. Colours here are CircuitCraft's own defaults.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use circuitcraft_model::{Layer, LayerKind};

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
