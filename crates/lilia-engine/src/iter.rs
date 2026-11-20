//! Iterators over internal keys, and the merge of several of them.

use std::cmp::Ordering;

use bytes::Bytes;

use crate::error::Result;
use crate::key;

/// Ordered iteration over internal keys (see [`crate::key`]).
pub trait InternalIterator: Send {
    fn valid(&self) -> bool;
    fn seek_to_first(&mut self) -> Result<()>;
    /// Positions at the first entry whose key is at or after `target`.
    fn seek(&mut self, target: &[u8]) -> Result<()>;
    fn next(&mut self) -> Result<()>;
    /// Current key; only meaningful while [`InternalIterator::valid`].
    fn key(&self) -> &[u8];
    fn value(&self) -> &[u8];

    /// The value as owned bytes; iterators over shared buffers return a slice of them instead
    /// of a copy.
    fn value_bytes(&self) -> Bytes {
        Bytes::copy_from_slice(self.value())
    }
}

/// Merge of iterators, in internal-key order. When two children hold the same internal key (it
/// cannot happen for distinct sequence numbers) the earlier child wins.
pub struct MergingIterator {
    children: Vec<Box<dyn InternalIterator>>,
    current: Option<usize>,
}

impl MergingIterator {
    pub fn new(children: Vec<Box<dyn InternalIterator>>) -> Self {
        Self { children, current: None }
    }

    fn pick(&mut self) {
        self.current = None;
        for (i, child) in self.children.iter().enumerate() {
            if !child.valid() {
                continue;
            }
            let better = match self.current {
                None => true,
                Some(c) => key::compare(child.key(), self.children[c].key()) == Ordering::Less,
            };
            if better {
                self.current = Some(i);
            }
        }
    }
}

impl InternalIterator for MergingIterator {
    fn valid(&self) -> bool {
        self.current.is_some()
    }

    fn seek_to_first(&mut self) -> Result<()> {
        for child in &mut self.children {
            child.seek_to_first()?;
        }
        self.pick();
        Ok(())
    }

    fn seek(&mut self, target: &[u8]) -> Result<()> {
        for child in &mut self.children {
            child.seek(target)?;
        }
        self.pick();
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        if let Some(c) = self.current {
            self.children[c].next()?;
        }
        self.pick();
        Ok(())
    }

    fn key(&self) -> &[u8] {
        self.current.map_or(&[], |c| self.children[c].key())
    }

    fn value(&self) -> &[u8] {
        self.current.map_or(&[], |c| self.children[c].value())
    }

    fn value_bytes(&self) -> Bytes {
        self.current.map_or_else(Bytes::new, |c| self.children[c].value_bytes())
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;

    /// An iterator over a sorted vector, for tests.
    pub struct VecIter {
        entries: Vec<(Vec<u8>, Vec<u8>)>,
        at: usize,
    }

    impl VecIter {
        pub fn new(mut entries: Vec<(Vec<u8>, Vec<u8>)>) -> Self {
            entries.sort_by(|a, b| key::compare(&a.0, &b.0));
            let at = entries.len();
            Self { entries, at }
        }
    }

    impl InternalIterator for VecIter {
        fn valid(&self) -> bool {
            self.at < self.entries.len()
        }
        fn seek_to_first(&mut self) -> Result<()> {
            self.at = 0;
            Ok(())
        }
        fn seek(&mut self, target: &[u8]) -> Result<()> {
            self.at = self.entries.partition_point(|e| key::compare(&e.0, target) == Ordering::Less);
            Ok(())
        }
        fn next(&mut self) -> Result<()> {
            self.at += 1;
            Ok(())
        }
        fn key(&self) -> &[u8] {
            &self.entries[self.at].0
        }
        fn value(&self) -> &[u8] {
            &self.entries[self.at].1
        }
    }

    #[test]
    fn merge_yields_global_internal_key_order() {
        use crate::key::{encode, Kind};
        let a = VecIter::new(vec![(encode(b"a", 3, Kind::Put), b"a3".to_vec()), (encode(b"c", 1, Kind::Put), b"c1".to_vec())]);
        let b = VecIter::new(vec![(encode(b"a", 5, Kind::Put), b"a5".to_vec()), (encode(b"b", 2, Kind::Delete), vec![])]);
        let mut merged = MergingIterator::new(vec![Box::new(a), Box::new(b)]);
        merged.seek_to_first().unwrap();
        let mut values = Vec::new();
        while merged.valid() {
            values.push(String::from_utf8(merged.value().to_vec()).unwrap());
            merged.next().unwrap();
        }
        assert_eq!(values, ["a5", "a3", "", "c1"]);
        merged.seek(&key::lookup(b"b", 9)).unwrap();
        assert_eq!(key::user(merged.key()), b"b");
    }
}
