//! The physical layers of a board.

use crate::{ModelError, Side};
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
    /// Courtyards: the room each component needs, its body and the space around it.
    Courtyard,
    /// Fabrication drawings: component outlines and designators for the assembly drawing.
    Fabrication,
    /// Keepouts: areas that copper must stay out of, on the outer copper layer of the layer's
    /// side, or on every copper layer when the layer is on neither side. The room a component
    /// needs is its courtyard, not a keepout.
    Keepout,
    /// Dimensions, notes and other drawings that are not manufactured.
    Mechanical,
}

/// One layer of the board, listed in a [`LayerStack`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    /// Stable identity.
    pub id: LayerId,
    /// Display name, unique within the stack.
    pub name: String,
    /// What the layer is for.
    pub kind: LayerKind,
    /// Physical thickness, where it matters (copper and dielectric).
    #[serde(deserialize_with = "crate::items::required")]
    pub thickness: Option<Nm>,
    /// The side of the board the layer belongs to, if it belongs to one. Solder mask, paste,
    /// silkscreen, courtyard and fabrication layers always have a side; keepout and mechanical
    /// layers may have one; dielectric and outline layers never do. Always `None` for copper,
    /// whose side follows from its place in the stack: ask [`LayerStack::side`].
    #[serde(deserialize_with = "crate::items::required")]
    pub side: Option<Side>,
    /// The layer of the same kind on the other side of the board, which is paired with this one
    /// in turn. A layer on a side always has one, and a layer on neither side never does. Always
    /// `None` for copper, which is mirrored by its place in the stack: ask
    /// [`LayerStack::flipped`].
    #[serde(deserialize_with = "crate::items::required")]
    pub pair: Option<LayerId>,
}

/// The most layers a stack may have (copper and everything else together).
pub const MAX_LAYERS: usize = 1_024;

/// The layers of a board: the physical build-up, and the layers that only hold drawings (outline,
/// courtyard, fabrication, keepout, mechanical).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayerStack {
    /// Layers. Copper and dielectric layers are in order from top to bottom. The order of the
    /// other layers has no physical meaning; it is the order in which layer lists show them.
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
    /// Top courtyard.
    pub const TOP_COURTYARD: LayerId = LayerId(11);
    /// Bottom courtyard.
    pub const BOTTOM_COURTYARD: LayerId = LayerId(12);
    /// Top fabrication drawing.
    pub const TOP_FABRICATION: LayerId = LayerId(13);
    /// Bottom fabrication drawing.
    pub const BOTTOM_FABRICATION: LayerId = LayerId(14);
    /// Keepouts on every copper layer.
    pub const KEEPOUT: LayerId = LayerId(15);
    /// Dimensions and notes.
    pub const MECHANICAL: LayerId = LayerId(16);
}

impl LayerStack {
    /// A common 1.6 mm two-layer stack: 35 µm copper (1 oz) on a 1.5 mm core, with the drawing
    /// layers a board needs. Each top layer is paired with its bottom counterpart.
    pub fn two_layer() -> LayerStack {
        use LayerKind::{Copper, Courtyard, Dielectric, Fabrication, Keepout, Mechanical, Outline, Paste, Silkscreen, SolderMask};
        let um = |um: i64| Some(Nm(um * 1_000));
        let single = |id: LayerId, name: &str, kind, thickness| Layer { id, name: name.to_owned(), kind, thickness, side: None, pair: None };
        // A top layer and its bottom counterpart, named alike.
        let pair = |top: LayerId, bottom: LayerId, name: &str, kind, thickness| {
            let layer =
                |id, side, word: &str, pair| Layer { id, name: format!("{word} {name}"), kind, thickness, side: Some(side), pair: Some(pair) };
            [layer(top, Side::Top, "Top", bottom), layer(bottom, Side::Bottom, "Bottom", top)]
        };
        let [top_paste, bottom_paste] = pair(ids::TOP_PASTE, ids::BOTTOM_PASTE, "Paste", Paste, None);
        let [top_silk, bottom_silk] = pair(ids::TOP_SILK, ids::BOTTOM_SILK, "Silkscreen", Silkscreen, None);
        let [top_mask, bottom_mask] = pair(ids::TOP_MASK, ids::BOTTOM_MASK, "Solder Mask", SolderMask, um(10));
        let [top_courtyard, bottom_courtyard] = pair(ids::TOP_COURTYARD, ids::BOTTOM_COURTYARD, "Courtyard", Courtyard, None);
        let [top_fabrication, bottom_fabrication] = pair(ids::TOP_FABRICATION, ids::BOTTOM_FABRICATION, "Fabrication", Fabrication, None);
        LayerStack {
            layers: vec![
                top_paste,
                top_silk,
                top_mask,
                single(ids::TOP_COPPER, "Top Copper", Copper, um(35)),
                single(ids::CORE, "Core", Dielectric, um(1_510)),
                single(ids::BOTTOM_COPPER, "Bottom Copper", Copper, um(35)),
                bottom_mask,
                bottom_silk,
                bottom_paste,
                single(ids::OUTLINE, "Outline", Outline, None),
                top_courtyard,
                bottom_courtyard,
                top_fabrication,
                bottom_fabrication,
                single(ids::KEEPOUT, "Keepout", Keepout, None),
                single(ids::MECHANICAL, "Mechanical", Mechanical, None),
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
    pub fn copper(&self) -> impl DoubleEndedIterator<Item = &Layer> {
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

    /// The side of the board layer `id` is on, if it is on one: the first copper layer is on the
    /// top and the last on the bottom (a lone copper layer is the top), and any other layer is on
    /// the side it says. `None` too when there is no such layer.
    pub fn side(&self, id: LayerId) -> Option<Side> {
        let layer = self.get(id)?;
        if layer.kind != LayerKind::Copper {
            layer.side
        } else if self.copper().next().is_some_and(|l| l.id == id) {
            Some(Side::Top)
        } else if self.copper().last().is_some_and(|l| l.id == id) {
            Some(Side::Bottom)
        } else {
            None
        }
    }

    /// The layer a drawing on `id` moves to when its footprint goes to the other side of the
    /// board, or `None` when there is no layer `id`. A copper layer goes to the one as far from
    /// the bottom as it is from the top; a layer on a side goes to its pair; a layer on neither
    /// side (the outline, say) stays where it is.
    pub fn flipped(&self, id: LayerId) -> Option<LayerId> {
        let layer = self.get(id)?;
        if layer.kind != LayerKind::Copper {
            return Some(layer.pair.unwrap_or(id));
        }
        let from_top = self.copper().position(|l| l.id == id)?;
        self.copper().rev().nth(from_top).map(|l| l.id)
    }

    /// Checks that layer names are usable and that no two layers share an id or a name, so every
    /// lookup by id or name has exactly one answer; then that each layer is on a side its kind
    /// allows, that a layer on a side is paired with its counterpart on the other, and that a
    /// side has one solder mask and one paste layer at most.
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
        // Sides first: a layer on the wrong side would otherwise be reported as a bad pair.
        let side = self.layers.iter().find_map(|l| side_problem(l).map(|why| (l, why)));
        let pair = || self.layers.iter().find_map(|l| self.pair_problem(l).map(|why| (l, why)));
        match side.or_else(pair).or_else(|| self.second_coating()) {
            Some((l, why)) => Err(ModelError::BadStack(l.name.clone(), why)),
            None => Ok(()),
        }
    }

    /// What is wrong with the pair of `l`, if anything.
    fn pair_problem(&self, l: &Layer) -> Option<&'static str> {
        match (l.side, l.pair) {
            (None, None) => None,
            // Flipping twice must give back the same layer.
            (Some(side), Some(pair)) => {
                let mirrored = self.get(pair).is_some_and(|o| o.kind == l.kind && o.side == Some(side.opposite()) && o.pair == Some(l.id));
                (!mirrored).then_some("its pair must be the layer of the same kind on the other side that is paired with it")
            }
            _ => Some("a layer on a side has a pair, and a layer on neither side has none"),
        }
    }

    /// A second solder mask or paste layer on one side, if there is one. Pads and vias say what
    /// "the" solder mask and paste of a side do, so a side has at most one of each.
    fn second_coating(&self) -> Option<(&Layer, &'static str)> {
        let mut seen = std::collections::HashSet::new();
        let coatings = self.layers.iter().filter(|l| matches!(l.kind, LayerKind::SolderMask | LayerKind::Paste));
        let mut second = coatings.filter(|l| !seen.insert((l.kind, l.side)));
        second.next().map(|l| (l, "a side has at most one solder mask and one paste layer"))
    }

    /// Total thickness of every layer that has one, saturating rather than overflowing.
    pub fn thickness(&self) -> Nm {
        Nm(self.layers.iter().filter_map(|l| l.thickness).fold(0_i64, |sum, t| sum.saturating_add(t.0)))
    }
}

/// What is wrong with the side `l` says it is on, if anything.
fn side_problem(l: &Layer) -> Option<&'static str> {
    use LayerKind::{Copper, Courtyard, Dielectric, Fabrication, Keepout, Mechanical, Outline, Paste, Silkscreen, SolderMask};
    match l.kind {
        // Stored, they could contradict the order of the layers, and every change to the copper
        // layers would have to rewrite them.
        Copper if l.side.is_some() || l.pair.is_some() => {
            Some("a copper layer's side and pair follow from its place in the stack; it stores neither")
        }
        Dielectric | Outline if l.side.is_some() => Some("this kind of layer is on neither side"),
        SolderMask | Paste | Silkscreen | Courtyard | Fabrication if l.side.is_none() => Some("this kind of layer needs a side"),
        Copper | Dielectric | Outline | SolderMask | Paste | Silkscreen | Courtyard | Fabrication | Keepout | Mechanical => None,
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

    const COPPER_STORES_NEITHER: &str = "a copper layer's side and pair follow from its place in the stack; it stores neither";
    const NO_SIDE: &str = "this kind of layer is on neither side";
    const NEEDS_SIDE: &str = "this kind of layer needs a side";
    const PAIR: &str = "its pair must be the layer of the same kind on the other side that is paired with it";
    const SIDE_AND_PAIR: &str = "a layer on a side has a pair, and a layer on neither side has none";
    const ONE_COATING: &str = "a side has at most one solder mask and one paste layer";

    fn bad(name: &str, why: &'static str) -> Result<(), ModelError> {
        Err(ModelError::BadStack(name.to_owned(), why))
    }

    fn layer(id: u16, name: &str, kind: LayerKind, side: Option<Side>, pair: Option<u16>) -> Layer {
        Layer { id: LayerId(id), name: name.to_owned(), kind, thickness: None, side, pair: pair.map(LayerId) }
    }

    /// `s` with one layer changed.
    fn changed(mut s: LayerStack, id: LayerId, change: impl FnOnce(&mut Layer)) -> LayerStack {
        if let Some(l) = s.layers.iter_mut().find(|l| l.id == id) {
            change(l);
        }
        s
    }

    /// The two-layer stack with one layer changed.
    fn with(id: LayerId, change: impl FnOnce(&mut Layer)) -> LayerStack {
        changed(LayerStack::two_layer(), id, change)
    }

    /// The two-layer stack with `inner` more copper layers between the outer two.
    fn with_inner_copper(inner: u16) -> LayerStack {
        let mut s = LayerStack::two_layer();
        let core = s.layers.iter().position(|l| l.id == ids::CORE).unwrap_or(0);
        s.layers.splice(core..core, (1..=inner).map(|n| layer(100 + n, &format!("Inner {n}"), LayerKind::Copper, None, None)));
        s
    }

    #[test]
    fn top_and_bottom_layers_are_paired_for_flipping() {
        let s = LayerStack::two_layer();
        assert_eq!(s.validate(), Ok(()));
        let pairs = [
            (ids::TOP_PASTE, ids::BOTTOM_PASTE),
            (ids::TOP_SILK, ids::BOTTOM_SILK),
            (ids::TOP_MASK, ids::BOTTOM_MASK),
            (ids::TOP_COPPER, ids::BOTTOM_COPPER),
            (ids::TOP_COURTYARD, ids::BOTTOM_COURTYARD),
            (ids::TOP_FABRICATION, ids::BOTTOM_FABRICATION),
        ];
        for (top, bottom) in pairs {
            assert_eq!((s.flipped(top), s.flipped(bottom)), (Some(bottom), Some(top)));
            assert_eq!((s.side(top), s.side(bottom)), (Some(Side::Top), Some(Side::Bottom)));
            assert_eq!(s.get(top).map(|l| l.kind), s.get(bottom).map(|l| l.kind));
        }
        // A layer on neither side stays where it is; a layer that doesn't exist goes nowhere.
        for alone in [ids::CORE, ids::OUTLINE, ids::KEEPOUT, ids::MECHANICAL] {
            assert_eq!((s.flipped(alone), s.side(alone)), (Some(alone), None));
        }
        assert_eq!((s.flipped(LayerId(999)), s.side(LayerId(999))), (None, None));
        let kind = |id| s.get(id).map(|l| l.kind);
        assert_eq!(kind(ids::TOP_COURTYARD), Some(LayerKind::Courtyard));
        assert_eq!(kind(ids::BOTTOM_FABRICATION), Some(LayerKind::Fabrication));
        assert_eq!(kind(ids::KEEPOUT), Some(LayerKind::Keepout));
        assert_eq!(kind(ids::MECHANICAL), Some(LayerKind::Mechanical));
    }

    #[test]
    fn copper_is_mirrored_by_its_place_in_the_stack() {
        // Six layers: each goes to the one as far from the other side.
        let six = with_inner_copper(4);
        assert_eq!(six.validate(), Ok(()));
        let copper: Vec<LayerId> = six.copper().map(|l| l.id).collect();
        assert_eq!(copper, [ids::TOP_COPPER, LayerId(101), LayerId(102), LayerId(103), LayerId(104), ids::BOTTOM_COPPER]);
        for (a, b) in [(ids::TOP_COPPER, ids::BOTTOM_COPPER), (LayerId(101), LayerId(104)), (LayerId(102), LayerId(103))] {
            assert_eq!((six.flipped(a), six.flipped(b)), (Some(b), Some(a)));
        }
        let sides: Vec<_> = copper.iter().map(|&id| six.side(id)).collect();
        assert_eq!(sides, [Some(Side::Top), None, None, None, None, Some(Side::Bottom)]);

        // An odd number: the middle layer stays where it is.
        let three = with_inner_copper(1);
        assert_eq!((three.flipped(LayerId(101)), three.side(LayerId(101))), (Some(LayerId(101)), None));
        assert_eq!(three.flipped(ids::TOP_COPPER), Some(ids::BOTTOM_COPPER));

        // One copper layer: it is the top, and its own mirror image.
        let mut one = LayerStack::two_layer();
        one.layers.retain(|l| l.id != ids::BOTTOM_COPPER);
        assert_eq!(one.validate(), Ok(()));
        assert_eq!((one.flipped(ids::TOP_COPPER), one.side(ids::TOP_COPPER)), (Some(ids::TOP_COPPER), Some(Side::Top)));

        // The order decides, so swapping the outer layers swaps their sides.
        let mut swapped = LayerStack::two_layer();
        let place = |s: &LayerStack, id| s.layers.iter().position(|l| l.id == id).unwrap_or(0);
        let (top, bottom) = (place(&swapped, ids::TOP_COPPER), place(&swapped, ids::BOTTOM_COPPER));
        swapped.layers.swap(top, bottom);
        assert_eq!(swapped.validate(), Ok(()));
        assert_eq!((swapped.side(ids::TOP_COPPER), swapped.side(ids::BOTTOM_COPPER)), (Some(Side::Bottom), Some(Side::Top)));

        // A copper layer stores neither a side nor a pair.
        assert_eq!(with(ids::TOP_COPPER, |l| l.side = Some(Side::Top)).validate(), bad("Top Copper", COPPER_STORES_NEITHER));
        assert_eq!(with(ids::BOTTOM_COPPER, |l| l.pair = Some(ids::TOP_COPPER)).validate(), bad("Bottom Copper", COPPER_STORES_NEITHER));
    }

    #[test]
    fn each_kind_of_layer_is_on_a_side_it_can_be() {
        assert_eq!(with(ids::CORE, |l| l.side = Some(Side::Top)).validate(), bad("Core", NO_SIDE));
        assert_eq!(with(ids::OUTLINE, |l| l.side = Some(Side::Bottom)).validate(), bad("Outline", NO_SIDE));
        for sided in [ids::TOP_MASK, ids::TOP_PASTE, ids::BOTTOM_SILK, ids::TOP_COURTYARD, ids::BOTTOM_FABRICATION] {
            let s = with(sided, |l| l.side = None);
            let name = s.get(sided).map(|l| l.name.clone()).unwrap_or_default();
            assert_eq!(s.validate(), bad(&name, NEEDS_SIDE));
        }
    }

    #[test]
    fn a_layer_on_a_side_is_paired_with_its_counterpart() {
        // Not paired, paired with itself, with a layer that doesn't exist, or with one that is
        // paired with another.
        assert_eq!(with(ids::TOP_SILK, |l| l.pair = None).validate(), bad("Top Silkscreen", SIDE_AND_PAIR));
        assert_eq!(with(ids::TOP_SILK, |l| l.pair = Some(ids::TOP_SILK)).validate(), bad("Top Silkscreen", PAIR));
        assert_eq!(with(ids::TOP_SILK, |l| l.pair = Some(LayerId(999))).validate(), bad("Top Silkscreen", PAIR));
        assert_eq!(with(ids::TOP_SILK, |l| l.pair = Some(ids::BOTTOM_MASK)).validate(), bad("Top Silkscreen", PAIR));
        // A third layer pointing into a pair: flipping it twice would not bring it back.
        let mut third = LayerStack::two_layer();
        third.layers.push(layer(30, "Third Silkscreen", LayerKind::Silkscreen, Some(Side::Top), Some(ids::BOTTOM_SILK.0)));
        assert_eq!(third.validate(), bad("Third Silkscreen", PAIR));
        // Every pair removed: nothing could flip.
        let mut unpaired = LayerStack::two_layer();
        unpaired.layers.iter_mut().for_each(|l| l.pair = None);
        assert_eq!(unpaired.validate(), bad("Top Paste", SIDE_AND_PAIR));
        // Both on one side.
        assert_eq!(with(ids::BOTTOM_COURTYARD, |l| l.side = Some(Side::Top)).validate(), bad("Top Courtyard", PAIR));
        // Paired with each other, but of two kinds.
        let kinds = changed(with(ids::TOP_SILK, |l| l.pair = Some(ids::BOTTOM_MASK)), ids::BOTTOM_MASK, |l| l.pair = Some(ids::TOP_SILK));
        assert_eq!(kinds.validate(), bad("Top Silkscreen", PAIR));

        // Keepout and mechanical layers: on neither side and alone, or a pair on opposite sides.
        let extra = |kind: LayerKind, a: Option<Side>, b: Option<Side>, paired: bool| {
            let mut s = LayerStack::two_layer();
            s.layers.push(layer(30, "Extra 1", kind, a, paired.then_some(31)));
            s.layers.push(layer(31, "Extra 2", kind, b, paired.then_some(30)));
            s.validate()
        };
        for kind in [LayerKind::Keepout, LayerKind::Mechanical] {
            assert_eq!(extra(kind, Some(Side::Top), Some(Side::Bottom), true), Ok(()));
            assert_eq!(extra(kind, Some(Side::Bottom), Some(Side::Top), true), Ok(()));
            assert_eq!(extra(kind, None, None, false), Ok(()));
            assert_eq!(extra(kind, None, None, true), bad("Extra 1", SIDE_AND_PAIR));
            assert_eq!(extra(kind, Some(Side::Top), Some(Side::Bottom), false), bad("Extra 1", SIDE_AND_PAIR));
            assert_eq!(extra(kind, Some(Side::Top), None, true), bad("Extra 1", PAIR));
            assert_eq!(extra(kind, Some(Side::Bottom), Some(Side::Bottom), true), bad("Extra 1", PAIR));
        }
    }

    #[test]
    fn a_side_has_one_solder_mask_and_one_paste_layer_at_most() {
        let second = |kind: LayerKind| {
            let mut s = LayerStack::two_layer();
            s.layers.push(layer(30, "Second Top", kind, Some(Side::Top), Some(31)));
            s.layers.push(layer(31, "Second Bottom", kind, Some(Side::Bottom), Some(30)));
            s.validate()
        };
        assert_eq!(second(LayerKind::SolderMask), bad("Second Top", ONE_COATING));
        assert_eq!(second(LayerKind::Paste), bad("Second Top", ONE_COATING));
        // Several silkscreen or courtyard layers on a side are fine, and so is a board with no
        // solder mask at all.
        assert_eq!(second(LayerKind::Silkscreen), Ok(()));
        assert_eq!(second(LayerKind::Courtyard), Ok(()));
        let mut bare = LayerStack::two_layer();
        bare.layers.retain(|l| l.kind != LayerKind::SolderMask);
        assert_eq!(bare.validate(), Ok(()));
    }

    #[test]
    fn decoding_refuses_too_many_layers() {
        let one = r#"{"id":1,"name":"L","kind":"copper","thickness":null,"side":null,"pair":null}"#;
        let stack = |n: usize| format!(r#"{{"layers":[{}]}}"#, vec![one; n].join(","));
        assert!(serde_json::from_str::<LayerStack>(&stack(MAX_LAYERS)).is_ok());
        assert!(serde_json::from_str::<LayerStack>(&stack(MAX_LAYERS + 1)).is_err());
    }

    #[test]
    fn a_layer_is_written_in_full() {
        let read = |layer: &str| serde_json::from_str::<Layer>(layer).map_err(|e| e.to_string());
        let whole = r#"{"id":1,"name":"L","kind":"copper","thickness":null,"side":null,"pair":null}"#;
        assert!(read(whole).is_ok());
        // A field left out is not read as `null`, and a field this build doesn't know is refused.
        for field in ["thickness", "side", "pair"] {
            let without = whole.replace(&format!(r#","{field}":null"#), "");
            assert!(read(&without).is_err_and(|e| e.contains(&format!("missing field `{field}`"))), "{field}");
        }
        let extra = whole.replace(r#""side""#, r#""colour":"red","side""#);
        assert!(read(&extra).is_err_and(|e| e.contains("unknown field `colour`")));
    }
}
