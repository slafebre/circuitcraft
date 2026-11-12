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
}
