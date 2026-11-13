//! A copy-on-write collection of board items, keyed by id.
//!
//! Each item sits behind an [`Arc`], so cloning a whole board (an undo snapshot) copies pointers,
//! not items, and editing one item copies only that item ([`Arc::make_mut`]). Items are kept in id
//! order, which also gives saved files a stable order.
//!
//! Collections are bounded. Decoding refuses a list longer than its limit as it is read: the
//! element past the limit is skipped without being decoded, so it allocates nothing.
//! [`Items::insert`] refuses to grow a collection past [`MAX_ITEMS`].

use crate::ModelError;
use serde::de::{Error as _, IgnoredAny, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

/// The most items one collection may hold (tracks, vias, nets, ...). Far above any real board;
/// it exists so that reading a file cannot allocate without limit.
pub const MAX_ITEMS: usize = 1_000_000;

/// Decodes a list of at most `max` elements, refusing a longer one as soon as it is seen.
pub(crate) fn bounded_vec<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D, max: usize) -> Result<Vec<T>, D::Error> {
    struct Bounded<T>(usize, PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for Bounded<T> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            write!(f, "a list of at most {} elements", self.0)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<T>, A::Error> {
            // The size hint comes from the input, so it only ever caps the first allocation.
            let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(self.0).min(4_096));
            while out.len() < self.0 {
                match seq.next_element()? {
                    Some(v) => out.push(v),
                    None => return Ok(out),
                }
            }
            refuse_more(&mut seq, self.0)?;
            Ok(out)
        }
    }
    d.deserialize_seq(Bounded(max, PhantomData))
}

/// At the limit: fails if the list goes on. The next element is skipped, never decoded.
fn refuse_more<'de, A: SeqAccess<'de>>(seq: &mut A, max: usize) -> Result<(), A::Error> {
    match seq.next_element::<IgnoredAny>()? {
        Some(_) => Err(A::Error::custom(format!("more than {max} elements"))),
        None => Ok(()),
    }
}

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

    /// Adds an item; returns the one it replaced, if its id was taken. Refuses a new id when the
    /// collection already holds [`MAX_ITEMS`] (replacing an item is always allowed).
    pub fn insert(&mut self, item: T) -> Result<Option<Arc<T>>, ModelError> {
        let id = item.id();
        if self.map.len() >= MAX_ITEMS && !self.map.contains_key(&id) {
            return Err(ModelError::TooMany("items"));
        }
        Ok(self.map.insert(id, Arc::new(item)))
    }

    /// Removes and returns the item with this id.
    pub fn remove(&mut self, id: T::Id) -> Option<Arc<T>> {
        self.map.remove(&id)
    }

    /// The first key whose item no longer reports that id (its `id` was changed through
    /// [`Items::get_mut`]); `None` when every item is filed under its own id.
    pub fn mismatched_key(&self) -> Option<T::Id> {
        self.map.iter().find(|(k, v)| v.id() != **k).map(|(k, _)| *k)
    }
}

impl<T: Keyed + Clone> Items<T> {
    /// The item with this id, for editing; copies it first if a snapshot still shares it. Don't
    /// change its id: the item stays filed under the old one, and `Board::validate` refuses that.
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
    /// A list of at most [`MAX_ITEMS`] items with distinct ids, filed as they are read.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct ItemsVisitor<T>(PhantomData<T>);
        impl<'de, T: Keyed + Deserialize<'de>> Visitor<'de> for ItemsVisitor<T> {
            type Value = Items<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "a list of at most {MAX_ITEMS} items")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Items<T>, A::Error> {
                let mut items = Items::default();
                while items.len() < MAX_ITEMS {
                    let Some(item) = seq.next_element::<T>()? else {
                        return Ok(items);
                    };
                    let id = item.id();
                    if items.map.insert(id, Arc::new(item)).is_some() {
                        return Err(A::Error::custom(format!("duplicate id {id:?}")));
                    }
                }
                refuse_more(&mut seq, MAX_ITEMS)?;
                Ok(items)
            }
        }
        d.deserialize_seq(ItemsVisitor(PhantomData))
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
        a.insert(Thing { id: 2, v: 20 }).expect("room");
        a.insert(Thing { id: 1, v: 10 }).expect("room");
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
        a.insert(Thing { id: 2, v: 20 }).expect("room");
        a.insert(Thing { id: 1, v: 10 }).expect("room");
        let json = serde_json::to_string(&a).expect("json");
        assert_eq!(json, r#"[{"id":1,"v":10},{"id":2,"v":20}]"#);
        let back: Items<Thing> = serde_json::from_str(&json).expect("parse");
        assert_eq!(back, a);
        assert!(serde_json::from_str::<Items<Thing>>(r#"[{"id":1,"v":1},{"id":1,"v":2}]"#).is_err());
    }

    #[test]
    fn decoding_stops_at_the_item_limit() {
        let items = (0..MAX_ITEMS).map(|i| format!(r#"{{"id":{i},"v":0}}"#)).collect::<Vec<_>>().join(",");
        let at_limit: Items<Thing> = serde_json::from_str(&format!("[{items}]")).expect("at the limit");
        assert_eq!(at_limit.len(), MAX_ITEMS);
        // The extra element isn't even a Thing: it is refused for being one too many, unread.
        let err = serde_json::from_str::<Items<Thing>>(&format!(r#"[{items},"not a thing"]"#)).err().map(|e| e.to_string());
        assert!(err.is_some_and(|e| e.contains("more than")), "refused by count, not by decoding the extra element");
    }

    #[test]
    fn insert_stops_at_the_item_limit() {
        let mut a: Items<Thing> = Items::default();
        for id in 0..MAX_ITEMS as u32 {
            a.insert(Thing { id, v: 0 }).expect("room");
        }
        assert_eq!(a.insert(Thing { id: u32::MAX, v: 0 }), Err(ModelError::TooMany("items")));
        assert_eq!(a.len(), MAX_ITEMS);
        // Replacing an item doesn't grow the collection, so it is still allowed.
        assert!(a.insert(Thing { id: 0, v: 1 }).is_ok_and(|old| old.is_some()));
    }

    #[test]
    fn bounded_vec_refuses_long_lists() {
        let mut de = serde_json::Deserializer::from_str("[1,2,3]");
        assert_eq!(bounded_vec::<_, i32>(&mut de, 3).ok(), Some(vec![1, 2, 3]));
        let mut de = serde_json::Deserializer::from_str(r#"[1,2,3,"four"]"#);
        let err = bounded_vec::<_, i32>(&mut de, 3).err().map(|e| e.to_string());
        assert!(err.is_some_and(|e| e.contains("more than 3")), "refused by count, not by decoding the extra element");
    }

    #[test]
    fn a_changed_id_is_reported() {
        let mut a: Items<Thing> = Items::default();
        a.insert(Thing { id: 1, v: 10 }).expect("room");
        assert_eq!(a.mismatched_key(), None);
        if let Some(t) = a.get_mut(1) {
            t.id = 7;
        }
        assert_eq!(a.mismatched_key(), Some(1));
    }
}
