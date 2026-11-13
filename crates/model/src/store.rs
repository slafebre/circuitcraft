//! A copy-on-write collection of board items, keyed by id.
//!
//! Each item sits behind an [`Arc`], so editing one item copies only that item
//! ([`Arc::make_mut`]), and an edit's record of what it changed ([`Change`]) holds pointers, not
//! copies. Items are kept in id order, which also gives saved files a stable order.
//!
//! Collections are bounded. Decoding refuses a list longer than its limit as it is read: the
//! element past the limit is refused before any of it is read, so it allocates nothing.
//! [`Items::insert`] refuses to grow a collection past [`MAX_ITEMS`].

use crate::ModelError;
use serde::de::{DeserializeSeed, Error as _, SeqAccess, Visitor};
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

/// At the limit: fails if the list goes on. The next element is refused before any of it is read
/// (even skipping it would walk its contents, which in a hostile file may be nested without end).
fn refuse_more<'de, A: SeqAccess<'de>>(seq: &mut A, max: usize) -> Result<(), A::Error> {
    struct Refuse(usize);
    impl<'de> DeserializeSeed<'de> for Refuse {
        type Value = ();
        fn deserialize<D: Deserializer<'de>>(self, _: D) -> Result<(), D::Error> {
            Err(D::Error::custom(format!("more than {} elements", self.0)))
        }
    }
    seq.next_element_seed(Refuse(max)).map(|_| ())
}

/// Something stored in an [`Items`] collection: it knows its own id.
pub trait Keyed {
    /// The id type.
    type Id: Ord + Copy + std::fmt::Debug;
    /// This item's id.
    fn id(&self) -> Self::Id;
}

/// What one edit did to one item: its value before and after (`None` when it didn't exist), so the
/// edit can be undone and redone, and readers such as the renderer know what to redraw.
#[derive(Debug, Clone)]
pub struct Change<T: Keyed> {
    /// The item's id.
    pub id: T::Id,
    /// The item before the edit; `None` when the edit added it.
    pub before: Option<Arc<T>>,
    /// The item after the edit; `None` when the edit removed it.
    pub after: Option<Arc<T>>,
}

/// Items of one kind, keyed by id, shared copy-on-write.
///
/// While an edit is being recorded, the collection keeps each touched item's value from before
/// its first change (a pointer, not a copy), so the edit becomes a list of [`Change`]s that costs
/// what it touched, not the size of the board.
#[derive(Debug)]
pub struct Items<T: Keyed> {
    map: BTreeMap<T::Id, Arc<T>>,
    /// Values from before the edit being recorded, by id; `None` when nothing is being recorded.
    journal: Option<BTreeMap<T::Id, Option<Arc<T>>>>,
}

impl<T: Keyed> Clone for Items<T> {
    /// A copy of the items; a recording in progress is not copied.
    fn clone(&self) -> Self {
        Items { map: self.map.clone(), journal: None }
    }
}

impl<T: Keyed> Default for Items<T> {
    fn default() -> Self {
        Items { map: BTreeMap::new(), journal: None }
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
        let old = self.map.insert(id, Arc::new(item));
        self.record(id, &old);
        Ok(old)
    }

    /// Removes and returns the item with this id.
    pub fn remove(&mut self, id: T::Id) -> Option<Arc<T>> {
        let old = self.map.remove(&id);
        if old.is_some() {
            self.record(id, &old);
        }
        old
    }

    /// The first key whose item no longer reports that id (its `id` was changed through
    /// [`Items::get_mut`]); `None` when every item is filed under its own id.
    pub fn mismatched_key(&self) -> Option<T::Id> {
        self.map.iter().find(|(k, v)| v.id() != **k).map(|(k, _)| *k)
    }

    /// Keeps `before` as the value of `id` before the edit being recorded, unless an earlier change
    /// in the same edit already kept one.
    fn record(&mut self, id: T::Id, before: &Option<Arc<T>>) {
        if let Some(journal) = &mut self.journal {
            journal.entry(id).or_insert_with(|| before.clone());
        }
    }

    /// Starts recording an edit (dropping any recording left unfinished).
    pub(crate) fn begin(&mut self) {
        self.journal = Some(BTreeMap::new());
    }

    /// Puts every item touched since [`Items::begin`] back as it was, and stops recording.
    pub(crate) fn rollback(&mut self) {
        for (id, before) in self.journal.take().unwrap_or_default() {
            self.put(id, before);
        }
    }

    /// Sets or clears one entry, without recording.
    fn put(&mut self, id: T::Id, value: Option<Arc<T>>) {
        match value {
            Some(v) => self.map.insert(id, v),
            None => self.map.remove(&id),
        };
    }

    /// Puts each change's value from before (`forward == false`) or after (`forward == true`) the
    /// edit in place, without recording.
    pub(crate) fn apply(&mut self, changes: &[Change<T>], forward: bool) {
        for c in changes {
            self.put(c.id, if forward { c.after.clone() } else { c.before.clone() });
        }
    }
}

impl<T: Keyed + PartialEq> Items<T> {
    /// Stops recording and returns what changed since [`Items::begin`], in id order. An item that
    /// was touched but ends up equal to how it started is not a change, and its original is put
    /// back, so the copy made for editing it doesn't stay around.
    pub(crate) fn finish(&mut self) -> Vec<Change<T>> {
        let mut changes = Vec::new();
        for (id, before) in self.journal.take().unwrap_or_default() {
            let after = self.map.get(&id).cloned();
            match (before, after) {
                (None, None) => {}
                (Some(b), Some(a)) if b == a => {
                    self.map.insert(id, b);
                }
                (before, after) => changes.push(Change { id, before, after }),
            }
        }
        changes
    }
}

impl<T: Keyed + Clone> Items<T> {
    /// The item with this id, for editing; copies it first if a snapshot or the edit being
    /// recorded still shares it. Don't change its id: the item stays filed under the old one, and
    /// `Board::validate` refuses that.
    pub fn get_mut(&mut self, id: T::Id) -> Option<&mut T> {
        if self.journal.is_some() {
            let current = self.map.get(&id).cloned();
            if current.is_some() {
                self.record(id, &current);
            }
        }
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

    fn things(ids: &[(u32, i32)]) -> Items<Thing> {
        let mut a = Items::default();
        for &(id, v) in ids {
            a.insert(Thing { id, v }).expect("room");
        }
        a
    }

    #[test]
    fn a_recording_lists_each_touched_item_once_with_its_first_value() {
        let mut a = things(&[(1, 10), (2, 20), (3, 30)]);
        a.begin();
        if let Some(t) = a.get_mut(1) {
            t.v = 11;
        }
        if let Some(t) = a.get_mut(1) {
            t.v = 12; // second change: the value before the edit is still 10
        }
        a.remove(2);
        a.insert(Thing { id: 4, v: 40 }).expect("room");
        let _ = a.get_mut(3); // touched, unchanged: not a change
        a.insert(Thing { id: 5, v: 50 }).expect("room");
        a.remove(5); // added and removed again: not a change
        let changes = a.finish();
        let summary: Vec<_> = changes.iter().map(|c| (c.id, c.before.as_ref().map(|t| t.v), c.after.as_ref().map(|t| t.v))).collect();
        assert_eq!(summary, [(1, Some(10), Some(12)), (2, Some(20), None), (4, None, Some(40))]);
        let after = a.clone();
        a.apply(&changes, false);
        assert_eq!(a, things(&[(1, 10), (2, 20), (3, 30)]), "undone");
        a.apply(&changes, true);
        assert_eq!(a, after, "redone");
    }

    #[test]
    fn overwriting_and_re_adding_an_id_keep_the_first_value() {
        let mut a = things(&[(1, 10), (2, 20)]);
        a.begin();
        a.insert(Thing { id: 1, v: 11 }).expect("room"); // over an existing id
        a.remove(2);
        a.insert(Thing { id: 2, v: 22 }).expect("room"); // removed, then added again
        a.remove(9); // not there: nothing to record
        let changes = a.finish();
        let summary: Vec<_> = changes.iter().map(|c| (c.id, c.before.as_ref().map(|t| t.v), c.after.as_ref().map(|t| t.v))).collect();
        assert_eq!(summary, [(1, Some(10), Some(11)), (2, Some(20), Some(22))]);
    }

    #[test]
    fn rollback_restores_everything_touched() {
        let mut a = things(&[(1, 10), (2, 20)]);
        a.begin();
        if let Some(t) = a.get_mut(1) {
            t.v = 99;
        }
        a.remove(2);
        a.insert(Thing { id: 3, v: 30 }).expect("room");
        a.rollback();
        assert_eq!(a, things(&[(1, 10), (2, 20)]));
        // No longer recording: later changes are not journalled.
        a.insert(Thing { id: 4, v: 40 }).expect("room");
        assert!(a.finish().is_empty());
    }

    #[test]
    fn a_copy_does_not_carry_a_recording() {
        let mut a = things(&[(1, 10)]);
        a.begin();
        a.remove(1);
        let mut copy = a.clone();
        assert!(copy.finish().is_empty());
        assert_eq!(a.finish().len(), 1);
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
        // The extra element is deeply nested and unfinished: it is refused for being one too many,
        // before any of it is read.
        let err = serde_json::from_str::<Items<Thing>>(&format!("[{items},{}", "[".repeat(100_000))).err().map(|e| e.to_string());
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
        let json = format!("[1,2,3,{}", "[".repeat(100_000));
        let mut de = serde_json::Deserializer::from_str(&json);
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
