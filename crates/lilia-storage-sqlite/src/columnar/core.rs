//! Shared state of the columnar store and helpers to reach the row store.

use std::sync::{Arc, Weak};

use lilia_analytics::arrow::array::{Array, ArrayRef, AsArray};
use lilia_analytics::arrow::datatypes::Int64Type;
use lilia_analytics::arrow::error::ArrowError;
use lilia_analytics::{AnalyticsError, ChunkCache, Scalar};
use lilia_core::{DictionaryRegistry, ErrorCode, LiliaError, Result, ValueCodec};
use lilia_sqlite_native::NativeConnection;

use super::AnalyticsOptions;
use crate::catalog::CollectionCatalog;
use crate::engine::Resources;
use crate::lifecycle::Lifecycle;
use crate::ring_buffer::WriteRingBuffer;

/// Everything the columnar store needs, shared by queries, maintenance and the worker.
///
/// The database handles are weak, like every background component: the store can never
/// keep a closed database alive.
#[derive(Debug)]
pub(crate) struct ColumnarCore {
    resources: Weak<Resources>,
    lifecycle: Weak<Lifecycle>,
    ring: Weak<WriteRingBuffer>,
    pub(crate) catalog: Arc<CollectionCatalog>,
    pub(crate) codec: Arc<dyn ValueCodec>,
    pub(crate) dict_registry: Arc<DictionaryRegistry>,
    pub(crate) cache: ChunkCache,
    pub(crate) options: AnalyticsOptions,
}

fn closed() -> LiliaError {
    LiliaError::new(ErrorCode::Closed, "database is closed", false)
}

impl ColumnarCore {
    pub(crate) fn new(database: &crate::Database, options: AnalyticsOptions) -> Self {
        Self {
            resources: Arc::downgrade(&database.resources),
            lifecycle: Arc::downgrade(&database.lifecycle),
            ring: Arc::downgrade(&database.ring_core),
            catalog: Arc::clone(&database.catalog),
            codec: Arc::clone(&database.codec),
            dict_registry: Arc::clone(&database.dict_registry),
            cache: ChunkCache::new(options.cache_bytes),
            options,
        }
    }

    /// Runs `work` on a pooled reader connection.
    pub(crate) fn with_reader<T>(
        &self,
        work: impl FnOnce(&NativeConnection) -> Result<T>,
    ) -> Result<T> {
        let resources = self.resources.upgrade().ok_or_else(closed)?;
        let lifecycle = self.lifecycle.upgrade().ok_or_else(closed)?;
        let connection = resources.read_lock(&lifecycle)?;
        work(&connection)
    }

    /// Runs `work` on the exclusive writer connection.
    ///
    /// Never call this while holding a reader: another thread may hold the writer and
    /// be waiting for that very reader.
    pub(crate) fn with_writer<T>(
        &self,
        work: impl FnOnce(&mut NativeConnection) -> Result<T>,
    ) -> Result<T> {
        let resources = self.resources.upgrade().ok_or_else(closed)?;
        let lifecycle = self.lifecycle.upgrade().ok_or_else(closed)?;
        let mut connection = resources.lock_writer(&lifecycle, None)?;
        work(&mut connection)
    }

    /// Whether JSON documents may live in attached shard files, out of reach of the
    /// change-tracking triggers installed on the main database.
    pub(crate) fn sharding_enabled(&self) -> Result<bool> {
        let resources = self.resources.upgrade().ok_or_else(closed)?;
        Ok(resources.shards().is_some())
    }

    /// Persists writes still buffered in memory so the row store reflects them.
    pub(crate) fn flush_ring(&self) -> Result<()> {
        if let Some(ring) = self.ring.upgrade() {
            ring.flush()?;
        }
        Ok(())
    }
}

/// Holds a read transaction open so several statements observe one snapshot.
pub(crate) struct ReadSnapshot<'c> {
    connection: &'c NativeConnection,
}

impl<'c> ReadSnapshot<'c> {
    pub(crate) fn begin(connection: &'c NativeConnection) -> Result<Self> {
        connection.exec("BEGIN;")?;
        Ok(Self { connection })
    }
}

impl Drop for ReadSnapshot<'_> {
    fn drop(&mut self) {
        // Nothing was written: ending the transaction only releases the snapshot.
        let _ = self.connection.exec("COMMIT;");
    }
}

pub(crate) fn arrow_error(error: ArrowError) -> LiliaError {
    AnalyticsError::from(error).into()
}

/// Key tying a columnar row to its row-store origin: a document id or a `rowid`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RowKey {
    Int(i64),
    Text(String),
}

impl RowKey {
    pub(crate) fn from_scalar(scalar: &Scalar) -> Option<Self> {
        match scalar {
            Scalar::Int(value) => Some(Self::Int(*value)),
            Scalar::Text(value) => Some(Self::Text(value.clone())),
            Scalar::Float(_) | Scalar::Bool(_) => None,
        }
    }
}

/// Index of the first element of the sorted key column that is `>= key`.
pub(crate) fn lower_bound(keys: &ArrayRef, key: &RowKey) -> usize {
    match key {
        RowKey::Int(wanted) => keys.as_primitive_opt::<Int64Type>().map_or(0, |values| {
            values.values().partition_point(|value| value < wanted)
        }),
        RowKey::Text(wanted) => keys.as_string_opt::<i32>().map_or(0, |values| {
            let (mut low, mut high) = (0, values.len());
            while low < high {
                let middle = low + (high - low) / 2;
                if values.value(middle) < wanted.as_str() {
                    low = middle + 1;
                } else {
                    high = middle;
                }
            }
            low
        }),
    }
}

/// Position of `key` in the sorted key column, if present.
pub(crate) fn find_key(keys: &ArrayRef, key: &RowKey) -> Option<usize> {
    let index = lower_bound(keys, key);
    if index >= keys.len() {
        return None;
    }
    let found = match key {
        RowKey::Int(wanted) => keys
            .as_primitive_opt::<Int64Type>()
            .is_some_and(|values| values.value(index) == *wanted),
        RowKey::Text(wanted) => keys
            .as_string_opt::<i32>()
            .is_some_and(|values| values.value(index) == wanted),
    };
    found.then_some(index)
}

#[cfg(test)]
mod tests {
    use lilia_analytics::arrow::array::{Int64Array, StringArray};

    use super::*;

    #[test]
    fn searches_sorted_integer_and_text_keys() {
        let ints: ArrayRef = Arc::new(Int64Array::from(vec![2, 4, 9]));
        assert_eq!(lower_bound(&ints, &RowKey::Int(1)), 0);
        assert_eq!(lower_bound(&ints, &RowKey::Int(4)), 1);
        assert_eq!(lower_bound(&ints, &RowKey::Int(5)), 2);
        assert_eq!(lower_bound(&ints, &RowKey::Int(10)), 3);
        assert_eq!(find_key(&ints, &RowKey::Int(9)), Some(2));
        assert_eq!(find_key(&ints, &RowKey::Int(3)), None);
        assert_eq!(find_key(&ints, &RowKey::Int(10)), None);

        let texts: ArrayRef = Arc::new(StringArray::from(vec!["b", "d", "f"]));
        assert_eq!(lower_bound(&texts, &RowKey::Text("a".into())), 0);
        assert_eq!(lower_bound(&texts, &RowKey::Text("d".into())), 1);
        assert_eq!(lower_bound(&texts, &RowKey::Text("e".into())), 2);
        assert_eq!(find_key(&texts, &RowKey::Text("f".into())), Some(2));
        assert_eq!(find_key(&texts, &RowKey::Text("g".into())), None);
        // A key of the wrong kind never matches.
        assert_eq!(find_key(&texts, &RowKey::Int(1)), None);
    }
}
