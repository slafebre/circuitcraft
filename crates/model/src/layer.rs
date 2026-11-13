//! The physical layers of a board.

use crate::ModelError;
use circuitcraft_units::Nm;
use serde::{Deserialize, Serialize};

/// A layer's identity, stable for the life of the board (names can change, ids don't).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LayerId(pub u16);

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
    /// Stable identity.
    pub id: LayerId,
    /// Display name, unique within the stack.
    pub name: String,
    /// What the layer is for.
    pub kind: LayerKind,
    /// Physical thickness, where it matters (copper and dielectric).
    pub thickness: Option<Nm>,
}

/// The most layers a stack may have (copper and everything else together).
pub const MAX_LAYERS: usize = 1_024;

/// The physical layers of a board, top to bottom, plus the outline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerStack {
    /// Layers, top to bottom.
    #[serde(deserialize_with = "layers")]
    pub layers: Vec<Layer>,
}

fn layers<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Layer>, D::Error> {
    crate::store::bounded_vec(d, MAX_LAYERS)
}

/// Ids of the layers in [`LayerStack::two_layer`], for code that needs a known layer.
pub mod ids {
    use super::LayerId;
    /// Top paste.
    pub const TOP_PASTE: LayerId = LayerId(1);
    /// Top silkscreen.
    pub const TOP_SILK: LayerId = LayerId(2);
    /// Top solder mask.
    pub const TOP_MASK: LayerId = LayerId(3);
    /// Top copper.
    pub const TOP_COPPER: LayerId = LayerId(4);
    /// The core between the copper layers.
    pub const CORE: LayerId = LayerId(5);
    /// Bottom copper.
    pub const BOTTOM_COPPER: LayerId = LayerId(6);
    /// Bottom solder mask.
    pub const BOTTOM_MASK: LayerId = LayerId(7);
    /// Bottom silkscreen.
    pub const BOTTOM_SILK: LayerId = LayerId(8);
    /// Bottom paste.
    pub const BOTTOM_PASTE: LayerId = LayerId(9);
    /// The board outline.
    pub const OUTLINE: LayerId = LayerId(10);
}

impl LayerStack {
    /// A common 1.6 mm two-layer stack: 35 µm copper (1 oz) on a 1.5 mm core.
    pub fn two_layer() -> LayerStack {
        use LayerKind::{Copper, Dielectric, Outline, Paste, Silkscreen, SolderMask};
        let l = |id: LayerId, name: &str, kind, um: Option<i64>| Layer { id, name: name.to_owned(), kind, thickness: um.map(|um| Nm(um * 1_000)) };
        LayerStack {
            layers: vec![
                l(ids::TOP_PASTE, "Top Paste", Paste, None),
                l(ids::TOP_SILK, "Top Silkscreen", Silkscreen, None),
                l(ids::TOP_MASK, "Top Solder Mask", SolderMask, Some(10)),
                l(ids::TOP_COPPER, "Top Copper", Copper, Some(35)),
                l(ids::CORE, "Core", Dielectric, Some(1_510)),
                l(ids::BOTTOM_COPPER, "Bottom Copper", Copper, Some(35)),
                l(ids::BOTTOM_MASK, "Bottom Solder Mask", SolderMask, Some(10)),
                l(ids::BOTTOM_SILK, "Bottom Silkscreen", Silkscreen, None),
                l(ids::BOTTOM_PASTE, "Bottom Paste", Paste, None),
                l(ids::OUTLINE, "Outline", Outline, None),
            ],
        }
    }

    /// The layer with this id.
    pub fn get(&self, id: LayerId) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }

    /// The layer with this name (exact match).
    pub fn by_name(&self, name: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.name == name)
    }

    /// Copper layers, top to bottom.
    pub fn copper(&self) -> impl Iterator<Item = &Layer> {
        self.layers.iter().filter(|l| l.kind == LayerKind::Copper)
    }

    /// The number of copper layers.
    pub fn copper_count(&self) -> usize {
        self.copper().count()
    }

    /// Whether `id` names a copper layer.
    pub fn is_copper(&self, id: LayerId) -> bool {
        self.get(id).is_some_and(|l| l.kind == LayerKind::Copper)
    }

    /// Checks that layer names are usable and that no two layers share an id or a name, so every
    /// lookup by id or name has exactly one answer.
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.layers.len() > MAX_LAYERS {
            return Err(ModelError::TooMany("layers"));
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut names = std::collections::BTreeSet::new();
        for l in &self.layers {
            crate::check_name(&l.name, "layer name")?;
            if !ids.insert(l.id) {
                return Err(ModelError::BadLayer("layer stack", "uniquely numbered"));
            }
            if !names.insert(l.name.as_str()) {
                return Err(ModelError::BadName(l.name.clone(), "duplicate layer name"));
            }
        }
        Ok(())
    }

    /// Total thickness of every layer that has one, saturating rather than overflowing.
    pub fn thickness(&self) -> Nm {
        Nm(self.layers.iter().filter_map(|l| l.thickness).fold(0_i64, |sum, t| sum.saturating_add(t.0)))
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
        assert!(s.is_copper(ids::TOP_COPPER) && s.is_copper(ids::BOTTOM_COPPER));
        assert!(!s.is_copper(ids::TOP_SILK) && !s.is_copper(LayerId(999)));
        assert_eq!(s.by_name("Bottom Copper").map(|l| l.id), Some(ids::BOTTOM_COPPER));
    }

    #[test]
    fn layer_names_and_ids_are_unique() {
        let s = LayerStack::two_layer();
        let mut names: Vec<_> = s.layers.iter().map(|l| l.name.as_str()).collect();
        let mut ids: Vec<_> = s.layers.iter().map(|l| l.id).collect();
        names.sort_unstable();
        names.dedup();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(names.len(), s.layers.len());
        assert_eq!(ids.len(), s.layers.len());
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_ambiguous_stacks() {
        let mut same_id = LayerStack::two_layer();
        if let Some(l) = same_id.layers.get_mut(1) {
            l.id = ids::TOP_PASTE;
        }
        assert_eq!(same_id.validate(), Err(ModelError::BadLayer("layer stack", "uniquely numbered")));
        let mut same_name = LayerStack::two_layer();
        if let Some(l) = same_name.layers.get_mut(1) {
            l.name = "Top Paste".to_owned();
        }
        assert!(matches!(same_name.validate(), Err(ModelError::BadName(..))));
        let mut unnamed = LayerStack::two_layer();
        if let Some(l) = unnamed.layers.get_mut(1) {
            l.name = " ".to_owned();
        }
        assert!(matches!(unnamed.validate(), Err(ModelError::BadName(..))));
    }

    #[test]
    fn decoding_refuses_too_many_layers() {
        let one = r#"{"id":1,"name":"L","kind":"copper","thickness":null}"#;
        let stack = |n: usize| format!(r#"{{"layers":[{}]}}"#, vec![one; n].join(","));
        assert!(serde_json::from_str::<LayerStack>(&stack(MAX_LAYERS)).is_ok());
        assert!(serde_json::from_str::<LayerStack>(&stack(MAX_LAYERS + 1)).is_err());
    }
}
