//! The vector handle of a session or transaction.

use std::collections::HashSet;
use std::ops::Bound;

use lilia_core::{VectorHit, VectorMetric};
use serde_json::json;

use super::vector::{distance, Vector};
use super::{fits, vector_key, VectorWrite};
use crate::batch::{Outcome, Write};
use crate::context::{guard, Context};
use crate::error::{corruption, Result};
use crate::guard::Guard;
use crate::keys::{self, validate_name};
use crate::read::Source;
use crate::record::decode_doc;

/// Vector collection `name` of a session or transaction.
///
/// Hits are read through the session or transaction, so they show its view of the vectors,
/// and a transaction's own vectors are compared exactly. The HNSW index itself follows the
/// committed state: a search in a transaction may miss a vector that was deleted or moved after
/// the transaction started.
#[derive(Debug)]
pub struct Vectors<'c, C: Context> {
    context: &'c C,
    name: String,
}

impl<'c, C: Context> Vectors<'c, C> {
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

    fn submit(&self, write: VectorWrite) -> Result<Outcome> {
        let mut outcomes = self.context.submit(vec![Write::Vector(write)])?;
        outcomes
            .pop()
            .ok_or_else(|| corruption("a vector write produced no outcome"))
    }

    /// Stores `vector` as `id`, replacing the previous one.
    ///
    /// # Errors
    ///
    /// Invalid names, a vector that does not fit the collection's index; as
    /// [`crate::Session::commit`].
    pub fn set(&self, id: &str, vector: &Vector) -> Result<Outcome> {
        self.submit(VectorWrite::Set {
            collection: self.name.clone(),
            id: id.to_owned(),
            vector: vector.clone(),
        })
    }

    /// Deletes vector `id`; `true` if it existed.
    ///
    /// # Errors
    ///
    /// As [`Vectors::set`].
    pub fn delete(&self, id: &str) -> Result<bool> {
        Ok(self
            .submit(VectorWrite::Delete {
                collection: self.name.clone(),
                id: id.to_owned(),
            })?
            .deleted)
    }

    /// Vector `id`, if the caller may read it.
    ///
    /// # Errors
    ///
    /// Invalid names, engine errors.
    pub fn get(&self, id: &str) -> Result<Option<Vector>> {
        validate_name("collection", &self.name)?;
        validate_name("vector id", id)?;
        let reader = self.context.reader(false);
        let Some(item) = reader.get(&vector_key(&self.name, id))? else {
            return Ok(None);
        };
        let allowed = self.allowed(&reader, &guard(self.context, &self.name), id)?;
        allowed.then(|| Vector::decode(&item.value)).transpose()
    }

    /// The `limit` vectors nearest to `query` under `metric`, nearest first. Uses the
    /// collection's HNSW index when it has one for `metric` that `query` fits, otherwise
    /// compares every vector of a comparable type.
    ///
    /// # Errors
    ///
    /// Invalid name, engine errors.
    pub fn search(
        &self,
        query: &Vector,
        metric: VectorMetric,
        limit: usize,
    ) -> Result<Vec<VectorHit>> {
        self.nearest(query, metric, None, limit)
    }

    /// As [`Vectors::search`], among the vectors of `ids` only.
    ///
    /// # Errors
    ///
    /// As [`Vectors::search`].
    pub fn search_within(
        &self,
        query: &Vector,
        metric: VectorMetric,
        ids: &[&str],
        limit: usize,
    ) -> Result<Vec<VectorHit>> {
        self.nearest(query, metric, Some(&ids.iter().copied().collect()), limit)
    }

    fn nearest(
        &self,
        query: &Vector,
        metric: VectorMetric,
        within: Option<&HashSet<&str>>,
        limit: usize,
    ) -> Result<Vec<VectorHit>> {
        validate_name("collection", &self.name)?;
        if limit == 0 || within.is_some_and(HashSet::is_empty) {
            return Ok(Vec::new());
        }
        let reader = self.context.reader(true);
        let guard = guard(self.context, &self.name);
        let database = self.context.database();
        let config = database
            .schema(&self.name)
            .and_then(|s| s.vector.clone())
            .filter(|c| c.metric == metric && fits(c, query).is_ok());
        let Some(config) = config else {
            return self.exact(&reader, &guard, query, metric, within, limit);
        };
        let loaded = database
            .vector_indexes()
            .get(database.engine(), &self.name, &config)?;
        let prepared = query
            .convert(config.element_type)
            .ok_or_else(|| corruption("a fitting query does not convert"))?;
        let base = keys::collection(keys::VECTOR, &self.name);
        let end = keys::prefix_end(&base).unwrap_or_default();
        let pending: Vec<String> = reader
            .pending(&base, &end)
            .into_iter()
            .filter_map(|item| {
                std::str::from_utf8(keys::suffix(&item.key, &base))
                    .ok()
                    .map(str::to_owned)
            })
            .collect();
        let quantized = config.element_type != query.element_type();
        let mut k = (limit * if quantized { 4 } else { 2 }).max(16);
        loop {
            let (candidates, total) = {
                let loaded = loaded.read();
                let filter = |id: &str| within.is_none_or(|w| w.contains(id));
                let ef = config.ef_search.max(k);
                let hits =
                    loaded
                        .index
                        .search(&self.name, prepared.data(), k, Some(ef), Some(&filter));
                (hits, loaded.index.len())
            };
            let mut ids: Vec<String> = candidates.into_iter().map(|hit| hit.id).collect();
            ids.extend(
                pending
                    .iter()
                    .filter(|id| within.is_none_or(|w| w.contains(id.as_str())))
                    .cloned(),
            );
            ids.sort_unstable();
            ids.dedup();
            let mut scored = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(item) = reader.get(&vector_key(&self.name, &id))? {
                    if let Some(d) = distance(query, &Vector::decode(&item.value)?, metric) {
                        scored.push((d, id));
                    }
                }
            }
            let hits = self.ranked(&reader, &guard, scored, limit)?;
            if hits.len() >= limit || k >= total {
                return Ok(hits);
            }
            k = k.saturating_mul(4);
        }
    }

    /// Exact search: every vector of the collection (or of `within`) the query can be compared
    /// with.
    fn exact(
        &self,
        reader: &impl Source,
        guard: &Guard<'_>,
        query: &Vector,
        metric: VectorMetric,
        within: Option<&HashSet<&str>>,
        limit: usize,
    ) -> Result<Vec<VectorHit>> {
        let base = keys::collection(keys::VECTOR, &self.name);
        let mut scored = Vec::new();
        if let Some(within) = within {
            for id in within {
                if let Some(item) = reader.get(&vector_key(&self.name, id))? {
                    if let Some(d) = distance(query, &Vector::decode(&item.value)?, metric) {
                        scored.push((d, (*id).to_owned()));
                    }
                }
            }
        } else {
            let end = keys::prefix_end(&base).unwrap_or_default();
            for item in reader.range(Bound::Included(&base), Bound::Excluded(&end))? {
                let item = item?;
                reader.observe(&item);
                if let Some(d) = distance(query, &Vector::decode(&item.value)?, metric) {
                    let id = std::str::from_utf8(keys::suffix(&item.key, &base))
                        .map_err(|_| corruption("vector id is not UTF-8"))?;
                    scored.push((d, id.to_owned()));
                }
            }
        }
        self.ranked(reader, guard, scored, limit)
    }

    /// The `limit` nearest of `scored` the caller may read.
    fn ranked(
        &self,
        reader: &impl Source,
        guard: &Guard<'_>,
        mut scored: Vec<(f64, String)>,
        limit: usize,
    ) -> Result<Vec<VectorHit>> {
        scored.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let mut hits = Vec::with_capacity(limit.min(scored.len()));
        for (d, id) in scored {
            if hits.len() >= limit {
                break;
            }
            if self.allowed(reader, guard, &id)? {
                hits.push(VectorHit {
                    collection: self.name.clone(),
                    id,
                    distance: d,
                });
            }
        }
        Ok(hits)
    }

    /// Whether the caller may see vector `id`: through the read policies of the JSON document
    /// of the same id when there is one, otherwise of a document made of the collection and id.
    fn allowed(&self, reader: &impl Source, guard: &Guard<'_>, id: &str) -> Result<bool> {
        if !guard.checks_reads() {
            return Ok(true);
        }
        let document = reader
            .get(&keys::doc(&self.name, id))?
            .map(|item| decode_doc(&item.value))
            .transpose()?;
        Ok(guard.related_hit(
            document.as_ref(),
            || json!({ "collection": self.name, "id": id }),
        ))
    }
}
