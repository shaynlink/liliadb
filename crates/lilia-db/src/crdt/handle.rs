//! The CRDT handles of a session or transaction.

use std::collections::BTreeSet;
use std::ops::Bound;

use bytes::Bytes;
use lilia_core::crdt::{LwwJsonDocument, OrSet, PNCounter};
use serde_json::{json, Value};

use super::store::{
    decode_counter, decode_entry, decode_field, end_of, entry_parts, object_prefix,
};
use super::{normalize_path, CrdtWrite};
use crate::batch::{Outcome, Write};
use crate::context::{guard, Context};
use crate::error::{corruption, Result};
use crate::keys::{self, validate_name};
use crate::read::Source;
use crate::record::now_ms;

/// CRDT space `name` of a session or transaction: PN-counters, OR-sets and LWW JSON documents
/// by id, each id naming one object of each type.
///
/// An object is readable when the read policies of the space admit the document
/// `{"space", "id", "type"}`, its `type` being `counter`, `set` or `doc`; one that is not reads
/// as empty. Times are in milliseconds.
#[derive(Debug)]
pub struct Crdt<'c, C: Context> {
    context: &'c C,
    name: String,
}

impl<'c, C: Context> Crdt<'c, C> {
    pub(crate) fn new(context: &'c C, name: &str) -> Self {
        Self {
            context,
            name: name.to_owned(),
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// PN-counter `id`.
    #[must_use]
    pub fn counter(&self, id: &str) -> CrdtCounter<'c, C> {
        CrdtCounter(self.object(id, "counter"))
    }

    /// Add-wins OR-set `id` of strings.
    #[must_use]
    pub fn set(&self, id: &str) -> CrdtSet<'c, C> {
        CrdtSet(self.object(id, "set"))
    }

    /// LWW JSON document `id`.
    #[must_use]
    pub fn doc(&self, id: &str) -> CrdtDoc<'c, C> {
        CrdtDoc(self.object(id, "doc"))
    }

    fn object(&self, id: &str, kind: &'static str) -> Object<'c, C> {
        Object {
            context: self.context,
            space: self.name.clone(),
            id: id.to_owned(),
            kind,
        }
    }
}

/// One object of a CRDT space.
#[derive(Debug)]
struct Object<'c, C: Context> {
    context: &'c C,
    space: String,
    id: String,
    kind: &'static str,
}

impl<C: Context> Object<'_, C> {
    fn submit(&self, write: CrdtWrite) -> Result<Outcome> {
        let mut outcomes = self.context.submit(vec![Write::Crdt(write)])?;
        outcomes
            .pop()
            .ok_or_else(|| corruption("a CRDT write produced no outcome"))
    }

    /// The records of keyspace `tag` of this object whose keys continue with `within`, as
    /// (key after the object's prefix, value); none when the caller may not read the object.
    fn records(&self, tag: u8, within: &[u8]) -> Result<Vec<(Vec<u8>, Bytes)>> {
        validate_name("space", &self.space)?;
        validate_name("CRDT id", &self.id)?;
        let readable = guard(self.context, &self.space).related_hit(
            None,
            || json!({ "space": self.space, "id": self.id, "type": self.kind }),
        );
        if !readable {
            return Ok(Vec::new());
        }
        let prefix = object_prefix(tag, &self.space, &self.id);
        let mut start = prefix.clone();
        start.extend_from_slice(within);
        let end = end_of(&start);
        let reader = self.context.reader(true);
        let mut found = Vec::new();
        for item in reader.range(Bound::Included(&start), Bound::Excluded(&end))? {
            let item = item?;
            reader.observe(&item);
            found.push((keys::suffix(&item.key, &prefix).to_vec(), item.value));
        }
        Ok(found)
    }
}

/// A PN-counter of a [`Crdt`] space: increments and decrements per actor.
#[derive(Debug)]
pub struct CrdtCounter<'c, C: Context>(Object<'c, C>);

impl<C: Context> CrdtCounter<'_, C> {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.0.id
    }

    /// The counter's value: all increments minus all decrements, clamped to `i64`.
    ///
    /// # Errors
    ///
    /// Invalid names, records that do not decode, engine errors.
    pub fn value(&self) -> Result<i64> {
        Ok(self.state()?.value())
    }

    /// The totals of every actor.
    ///
    /// # Errors
    ///
    /// As [`CrdtCounter::value`].
    pub fn state(&self) -> Result<PNCounter> {
        let mut counter = PNCounter::new();
        for (actor, raw) in self.0.records(keys::CRDT_COUNTER, &[])? {
            let actor =
                String::from_utf8(actor).map_err(|_| corruption("unreadable CRDT counter key"))?;
            let (p, n) = decode_counter(&raw)?;
            if p > 0 {
                counter.p.insert(actor.clone(), p);
            }
            if n > 0 {
                counter.n.insert(actor, n);
            }
        }
        Ok(counter)
    }

    /// Adds `amount` to the increments of `actor`.
    ///
    /// # Errors
    ///
    /// Invalid names; as [`crate::Session::commit`].
    pub fn increment(&self, actor: &str, amount: u64) -> Result<Outcome> {
        self.count(actor, amount, 0)
    }

    /// Adds `amount` to the decrements of `actor`.
    ///
    /// # Errors
    ///
    /// As [`CrdtCounter::increment`].
    pub fn decrement(&self, actor: &str, amount: u64) -> Result<Outcome> {
        self.count(actor, 0, amount)
    }

    fn count(&self, actor: &str, increment: u64, decrement: u64) -> Result<Outcome> {
        self.0.submit(CrdtWrite::Count {
            space: self.0.space.clone(),
            id: self.0.id.clone(),
            actor: actor.to_owned(),
            increment,
            decrement,
        })
    }

    /// Joins the state of a remote replica: each actor's totals become the larger of the two.
    ///
    /// # Errors
    ///
    /// As [`CrdtCounter::increment`].
    pub fn merge(&self, remote: &PNCounter) -> Result<Outcome> {
        self.0.submit(CrdtWrite::CounterMerge {
            space: self.0.space.clone(),
            id: self.0.id.clone(),
            state: remote.clone(),
        })
    }
}

/// An add-wins OR-set of strings of a [`Crdt`] space. Each add tags the element anew, a removal
/// tombstones the tags it sees, so a concurrent add survives it.
#[derive(Debug)]
pub struct CrdtSet<'c, C: Context>(Object<'c, C>);

impl<C: Context> CrdtSet<'_, C> {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.0.id
    }

    /// The elements present: those with a tag that is not tombstoned.
    ///
    /// # Errors
    ///
    /// Invalid names, records that do not decode, engine errors.
    pub fn elements(&self) -> Result<BTreeSet<String>> {
        Ok(self.state()?.read())
    }

    /// Whether `element` is present.
    ///
    /// # Errors
    ///
    /// As [`CrdtSet::elements`].
    pub fn contains(&self, element: &str) -> Result<bool> {
        let mut within = Vec::new();
        keys::push_escaped(&mut within, element.as_bytes());
        for (_, raw) in self.0.records(keys::CRDT_SET, &within)? {
            if decode_entry(&raw)?.is_none() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Every tag of every element, and the tombstones.
    ///
    /// # Errors
    ///
    /// As [`CrdtSet::elements`].
    pub fn state(&self) -> Result<OrSet<String>> {
        let mut set = OrSet::new();
        for (rest, raw) in self.0.records(keys::CRDT_SET, &[])? {
            let (element, tag) =
                entry_parts(&rest).ok_or_else(|| corruption("unreadable CRDT set entry key"))?;
            if let Some(at) = decode_entry(&raw)? {
                set.tombstones.insert(tag.clone(), at);
            }
            set.add_with_tag(element, tag);
        }
        Ok(set)
    }

    /// Adds `element` on behalf of `actor`, under a new tag.
    ///
    /// # Errors
    ///
    /// Invalid names; as [`crate::Session::commit`].
    pub fn add(&self, actor: &str, element: &str) -> Result<Outcome> {
        validate_name("actor", actor)?;
        self.0.submit(CrdtWrite::set_add(
            &self.0.space,
            &self.0.id,
            actor,
            element,
        ))
    }

    /// Removes `element` now; `true` if it was present.
    ///
    /// # Errors
    ///
    /// As [`CrdtSet::add`].
    pub fn remove(&self, element: &str) -> Result<bool> {
        self.remove_at(element, now_ms())
    }

    /// Removes `element`, its tags tombstoned at `timestamp_ms`; `true` if it was present.
    ///
    /// # Errors
    ///
    /// As [`CrdtSet::add`].
    pub fn remove_at(&self, element: &str, timestamp_ms: i64) -> Result<bool> {
        Ok(self
            .0
            .submit(CrdtWrite::SetRemove {
                space: self.0.space.clone(),
                id: self.0.id.clone(),
                element: element.to_owned(),
                timestamp_ms,
            })?
            .deleted)
    }

    /// Joins the state of a remote replica: the union of the tags, a tag tombstoned on either
    /// side tombstoned.
    ///
    /// # Errors
    ///
    /// As [`CrdtSet::add`].
    pub fn merge(&self, remote: &OrSet<String>) -> Result<Outcome> {
        self.0.submit(CrdtWrite::SetMerge {
            space: self.0.space.clone(),
            id: self.0.id.clone(),
            state: remote.clone(),
        })
    }

    /// Forgets the tags tombstoned before `cutoff_ms`; `true` if there were some. Only safe
    /// once every replica has seen those removals: a replica that has not would bring the tags
    /// back on a merge.
    ///
    /// # Errors
    ///
    /// As [`CrdtSet::add`].
    pub fn prune(&self, cutoff_ms: i64) -> Result<bool> {
        Ok(self
            .0
            .submit(CrdtWrite::SetPrune {
                space: self.0.space.clone(),
                id: self.0.id.clone(),
                cutoff_ms,
            })?
            .deleted)
    }
}

/// A JSON document of a [`Crdt`] space whose fields (by JSON pointer) are last-writer-wins
/// registers, so concurrent edits of different fields all survive a merge.
///
/// A local write is ordered after the fields it replaces: its clock is its time, or the next
/// tick of the latest clock it replaces when that is later. A local write therefore always
/// takes effect, and wins on a merge against the values it replaced.
#[derive(Debug)]
pub struct CrdtDoc<'c, C: Context>(Object<'c, C>);

impl<C: Context> CrdtDoc<'_, C> {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.0.id
    }

    /// The document, its live fields assembled; `None` when it has no field, live or deleted.
    ///
    /// # Errors
    ///
    /// Invalid names, records that do not decode, engine errors.
    pub fn get(&self) -> Result<Option<Value>> {
        Ok(self.state()?.map(|doc| doc.to_value()))
    }

    /// The value at JSON pointer `path` of the document.
    ///
    /// # Errors
    ///
    /// An invalid path; as [`CrdtDoc::get`].
    pub fn field(&self, path: &str) -> Result<Option<Value>> {
        let path = normalize_path(path)?;
        let document = self.get()?;
        Ok(if path == "/" {
            document
        } else {
            document.and_then(|value| value.pointer(&path).cloned())
        })
    }

    /// Every field with its clock, deleted ones included; `None` when there is none.
    ///
    /// # Errors
    ///
    /// As [`CrdtDoc::get`].
    pub fn state(&self) -> Result<Option<LwwJsonDocument>> {
        let records = self.0.records(keys::CRDT_FIELD, &[])?;
        if records.is_empty() {
            return Ok(None);
        }
        let mut doc = LwwJsonDocument::new();
        for (path, raw) in records {
            let path =
                String::from_utf8(path).map_err(|_| corruption("unreadable CRDT field key"))?;
            doc.fields.insert(path, decode_field(&raw)?);
        }
        Ok(Some(doc))
    }

    /// Sets the field at `path` to `value`, now, on behalf of `actor`.
    ///
    /// # Errors
    ///
    /// Invalid names or path; as [`crate::Session::commit`].
    pub fn set_field(&self, actor: &str, path: &str, value: &Value) -> Result<Outcome> {
        self.set_field_at(actor, path, value, now_ms())
    }

    /// Sets the field at `path` to `value` at `timestamp_ms`, on behalf of `actor`.
    ///
    /// # Errors
    ///
    /// As [`CrdtDoc::set_field`].
    pub fn set_field_at(
        &self,
        actor: &str,
        path: &str,
        value: &Value,
        timestamp_ms: i64,
    ) -> Result<Outcome> {
        self.0.submit(CrdtWrite::DocSet {
            space: self.0.space.clone(),
            id: self.0.id.clone(),
            actor: actor.to_owned(),
            path: path.to_owned(),
            value: value.clone(),
            timestamp_ms,
        })
    }

    /// Deletes the field at `path` and every field below it, now; `true` if one was live.
    ///
    /// # Errors
    ///
    /// As [`CrdtDoc::set_field`].
    pub fn delete_field(&self, actor: &str, path: &str) -> Result<bool> {
        self.delete_field_at(actor, path, now_ms())
    }

    /// Deletes the field at `path` and every field below it at `timestamp_ms`; `true` if one
    /// was live.
    ///
    /// # Errors
    ///
    /// As [`CrdtDoc::set_field`].
    pub fn delete_field_at(&self, actor: &str, path: &str, timestamp_ms: i64) -> Result<bool> {
        Ok(self
            .0
            .submit(CrdtWrite::DocDelete {
                space: self.0.space.clone(),
                id: self.0.id.clone(),
                actor: actor.to_owned(),
                path: path.to_owned(),
                timestamp_ms,
            })?
            .deleted)
    }

    /// Joins the state of a remote replica: each field takes the value of the later clock.
    ///
    /// # Errors
    ///
    /// A field path that is not normalized; as [`CrdtDoc::set_field`].
    pub fn merge(&self, remote: &LwwJsonDocument) -> Result<Outcome> {
        self.0.submit(CrdtWrite::DocMerge {
            space: self.0.space.clone(),
            id: self.0.id.clone(),
            state: remote.clone(),
        })
    }
}
