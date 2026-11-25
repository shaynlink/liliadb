//! What the model handles ([`crate::Graph`] and the others) read from and write to: a
//! [`crate::Session`] (the latest state, writes committed at once) or a [`crate::Transaction`]
//! (its snapshot under its own writes, writes buffered until commit). Each handle is written
//! once over this trait.

use std::ops::Bound;

use bytes::Bytes;

use crate::batch::{Outcome, Write};
use crate::db::Database;
use crate::error::Result;
use crate::guard::{Access, Guard};
use crate::read::{Item, Items, Latest, Source};
use crate::txn::View;

/// A session or a transaction: what the model handles read from and write to, the latest state
/// or a transaction's snapshot under its own writes.
pub trait Context: sealed::Inner {}

pub(crate) mod sealed {
    use super::{Access, Database, Outcome, Reader, Result, Write};

    /// The operations a model handle needs. Not nameable outside the crate, so only the crate's
    /// own contexts implement it and only the crate calls it.
    pub trait Inner {
        fn database(&self) -> &Database;
        fn access(&self) -> &Access;
        /// A reader; `pinned` asks for one fixed state across several reads.
        fn reader(&self, pinned: bool) -> Reader<'_>;
        /// Applies `writes`: committed at once by a session, buffered by a transaction (whose
        /// outcomes then carry no version).
        fn submit(&self, writes: Vec<Write>) -> Result<Vec<Outcome>>;
    }
}

/// The source of a context's reads.
#[derive(Debug)]
pub enum Reader<'a> {
    Latest(Latest<'a>),
    View(View<'a>),
}

impl Source for Reader<'_> {
    fn get(&self, key: &Bytes) -> Result<Option<Item>> {
        match self {
            Self::Latest(source) => source.get(key),
            Self::View(source) => source.get(key),
        }
    }

    fn range(&self, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<Items<'_>> {
        match self {
            Self::Latest(source) => source.range(start, end),
            Self::View(source) => source.range(start, end),
        }
    }

    fn observe(&self, item: &Item) {
        match self {
            Self::Latest(source) => source.observe(item),
            Self::View(source) => source.observe(item),
        }
    }

    fn pending(&self, start: &[u8], end: &[u8]) -> Vec<Item> {
        match self {
            Self::Latest(source) => source.pending(start, end),
            Self::View(source) => source.pending(start, end),
        }
    }

    fn is_pending(&self, key: &[u8]) -> bool {
        match self {
            Self::Latest(source) => source.is_pending(key),
            Self::View(source) => source.is_pending(key),
        }
    }
}

/// The guard of collection `name` for a context's caller.
pub(crate) fn guard<'c>(context: &'c impl sealed::Inner, name: &str) -> Guard<'c> {
    Guard::new(context.access(), context.database().schema(name))
}
