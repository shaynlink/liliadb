//! Where the writes of the record-level models (geo, CRDTs) read: the planner of a commit, or a
//! transaction's view when its overlay is computed. One effect function serves both.

use std::ops::Bound;

use bytes::Bytes;

use crate::error::Result;
use crate::read::Source;
use crate::txn::View;
use crate::write::Planner;

/// What a write reads.
pub trait RecordSource {
    /// The record at `key`.
    ///
    /// # Errors
    ///
    /// Engine errors.
    fn read(&mut self, key: &Bytes) -> Result<Option<Bytes>>;

    /// Live records with keys in `[start, end)`.
    ///
    /// # Errors
    ///
    /// Engine errors.
    fn live(&mut self, start: &[u8], end: &[u8]) -> Result<Vec<(Bytes, Bytes)>>;
}

impl RecordSource for Planner<'_> {
    fn read(&mut self, key: &Bytes) -> Result<Option<Bytes>> {
        self.current(key)
    }

    /// Each record found is read again with an expectation: a write that rewrites or removes
    /// the records it sees fails if one of them changes concurrently. A record added to the
    /// range concurrently is as if added after the write.
    fn live(&mut self, start: &[u8], end: &[u8]) -> Result<Vec<(Bytes, Bytes)>> {
        let mut found = Vec::new();
        for (key, _) in self.live_range(start, end)? {
            if let Some(value) = self.current(&key)? {
                found.push((key, value));
            }
        }
        Ok(found)
    }
}

impl RecordSource for &View<'_> {
    fn read(&mut self, key: &Bytes) -> Result<Option<Bytes>> {
        Ok(self.peek(key)?.map(|item| item.value))
    }

    fn live(&mut self, start: &[u8], end: &[u8]) -> Result<Vec<(Bytes, Bytes)>> {
        self.range(Bound::Included(start), Bound::Excluded(end))?
            .map(|item| item.map(|item| (item.key, item.value)))
            .collect()
    }
}
