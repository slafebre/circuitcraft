//! A copy-on-write collection of board items, keyed by id.
//!
//! Each item sits behind an [`Arc`], so cloning a whole board (an undo snapshot) copies pointers,
//! not items, and editing one item copies only that item ([`Arc::make_mut`]). Items are kept in id
//! order, which also gives saved files a stable order.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Something stored in an [`Items`] collection: it knows its own id.
pub trait Keyed {
    /// The id type.
    type Id: Ord + Copy + std::fmt::Debug;
    /// This item's id.
    fn id(&self) -> Self::Id;
}

/// Items of one kind, keyed by id, shared copy-on-write.
#[derive(Debug)]
pub struct Items<T: Keyed> {
    map: BTreeMap<T::Id, Arc<T>>,
}

impl<T: Keyed> Clone for Items<T> {
    fn clone(&self) -> Self {
        Items { map: self.map.clone() }
    }
}

impl<T: Keyed> Default for Items<T> {
    fn default() -> Self {
        Items { map: BTreeMap::new() }
    }
}

impl<T: Keyed + PartialEq> PartialEq for Items<T> {
    fn eq(&self, other: &Self) -> bool {
        self.map.len() == other.map.len() && self.map.iter().zip(other.map.iter()).all(|((a, x), (b, y))| a == b && x == y)
    }
}

impl<T: Keyed + Eq> Eq for Items<T> {}

impl<T: Keyed> Items<T> {
    /// How many items there are.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The item with this id.
    pub fn get(&self, id: T::Id) -> Option<&T> {
        self.map.get(&id).map(Arc::as_ref)
    }

    /// Whether an item has this id.
    pub fn contains(&self, id: T::Id) -> bool {
        self.map.contains_key(&id)
    }

    /// Items in id order.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.map.values().map(Arc::as_ref)
    }

    /// Adds an item; returns the one it replaced, if its id was taken.
    pub fn insert(&mut self, item: T) -> Option<Arc<T>> {
        self.map.insert(item.id(), Arc::new(item))
    }

    /// Removes and returns the item with this id.
    pub fn remove(&mut self, id: T::Id) -> Option<Arc<T>> {
        self.map.remove(&id)
    }
}

impl<T: Keyed + Clone> Items<T> {
    /// The item with this id, for editing; copies it first if a snapshot still shares it.
    pub fn get_mut(&mut self, id: T::Id) -> Option<&mut T> {
        self.map.get_mut(&id).map(Arc::make_mut)
    }
}

impl<T: Keyed + Serialize> Serialize for Items<T> {
    /// A list in id order: diffs stay small and saves are byte-identical.
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.map.values().map(Arc::as_ref))
    }
}

impl<'de, T: Keyed + Deserialize<'de>> Deserialize<'de> for Items<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let list = Vec::<T>::deserialize(d)?;
        let mut items = Items::default();
        for item in list {
            let id = item.id();
            if items.insert(item).is_some() {
                return Err(D::Error::custom(format!("duplicate id {id:?}")));
            }
        }
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct Thing {
        id: u32,
        v: i32,
    }

    impl Keyed for Thing {
        type Id = u32;
        fn id(&self) -> u32 {
            self.id
        }
    }

    #[test]
    fn copy_on_write_keeps_snapshots_intact() {
        let mut a: Items<Thing> = Items::default();
        a.insert(Thing { id: 2, v: 20 });
        a.insert(Thing { id: 1, v: 10 });
        let snapshot = a.clone();
        if let Some(t) = a.get_mut(1) {
            t.v = 11;
        }
        assert_eq!(a.get(1).map(|t| t.v), Some(11));
        assert_eq!(snapshot.get(1).map(|t| t.v), Some(10));
        // The untouched item is still shared, not copied.
        assert!(Arc::ptr_eq(&a.map[&2], &snapshot.map[&2]));
    }

    #[test]
    fn serializes_in_id_order_and_rejects_duplicates() {
        let mut a: Items<Thing> = Items::default();
        a.insert(Thing { id: 2, v: 20 });
        a.insert(Thing { id: 1, v: 10 });
        let json = serde_json::to_string(&a).expect("json");
        assert_eq!(json, r#"[{"id":1,"v":10},{"id":2,"v":20}]"#);
        let back: Items<Thing> = serde_json::from_str(&json).expect("parse");
        assert_eq!(back, a);
        assert!(serde_json::from_str::<Items<Thing>>(r#"[{"id":1,"v":1},{"id":1,"v":2}]"#).is_err());
    }
}
