use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;

use lilia_core::aggregation::{
    Aggregate, AnomalyAccumulator, CountAccumulator, FilterExpr, HomomorphicSumAccumulator,
    HyperLogLog, MaxAccumulator, MinAccumulator, Sort, SortDirection, SumAccumulator, TDigest,
    WelfordAccumulator,
};
use lilia_core::fle::{decrypt_paillier_f64, decrypt_paillier_i64, EncryptedEnvelope, KeyRegistry};
use lilia_core::Result;
use serde_json::Value;

use super::cursor::StreamDocument;

/// Filter stage: evaluates filter expressions or closures lazily.
/// Caller-supplied predicate deciding whether a document passes a filter stage.
pub type DocumentPredicate = Box<dyn Fn(&StreamDocument) -> bool + Send + Sync>;

pub struct FilterStream<I> {
    upstream: I,
    filter_expr: Option<FilterExpr>,
    filter_fn: Option<DocumentPredicate>,
}

impl<I> std::fmt::Debug for FilterStream<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterStream")
            .field("filter_expr", &self.filter_expr)
            .field("has_filter_fn", &self.filter_fn.is_some())
            .finish_non_exhaustive()
    }
}

impl<I> FilterStream<I> {
    pub fn new(
        upstream: I,
        filter_expr: Option<FilterExpr>,
        filter_fn: Option<DocumentPredicate>,
    ) -> Self {
        Self {
            upstream,
            filter_expr,
            filter_fn,
        }
    }
}

impl<I: Iterator<Item = Result<StreamDocument>>> Iterator for FilterStream<I> {
    type Item = Result<StreamDocument>;

    fn next(&mut self) -> Option<Self::Item> {
        for res in self.upstream.by_ref() {
            match res {
                Ok(doc) => {
                    if let Some(ref expr) = self.filter_expr {
                        if !expr.matches_binary(&doc.doc) {
                            continue;
                        }
                    }
                    if let Some(ref pred) = self.filter_fn {
                        if !pred(&doc) {
                            continue;
                        }
                    }
                    return Some(Ok(doc));
                }
                Err(e) => return Some(Err(e)),
            }
        }
        None
    }
}

/// Unwind stage: flattens an array field lazily one element at a time in O(1) RAM.
pub struct UnwindStream<I> {
    upstream: I,
    field: String,
    current_doc: Option<StreamDocument>,
    current_items: Vec<Value>,
    current_idx: usize,
}

impl<I> std::fmt::Debug for UnwindStream<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnwindStream")
            .field("field", &self.field)
            .field("current_doc", &self.current_doc)
            .field("current_idx", &self.current_idx)
            .field("items_len", &self.current_items.len())
            .finish_non_exhaustive()
    }
}

impl<I> UnwindStream<I> {
    pub fn new(upstream: I, field: String) -> Self {
        Self {
            upstream,
            field,
            current_doc: None,
            current_items: Vec::new(),
            current_idx: 0,
        }
    }
}

impl<I: Iterator<Item = Result<StreamDocument>>> Iterator for UnwindStream<I> {
    type Item = Result<StreamDocument>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(ref doc) = self.current_doc {
                if self.current_idx < self.current_items.len() {
                    let item = self.current_items[self.current_idx].clone();
                    self.current_idx += 1;
                    return Some(Ok(StreamDocument::with_unwound(
                        doc.id.clone(),
                        doc.doc.clone(),
                        self.field.clone(),
                        item,
                    )));
                }
            }

            // Fetch next document from upstream
            match self.upstream.next() {
                Some(Ok(doc)) => {
                    let items = if let Some(arr) =
                        doc.doc.extract(&self.field).and_then(|v| v.as_array())
                    {
                        arr.iter().map(|item| item.to_json_value()).collect()
                    } else if let Some(val) = doc.extract_value(&self.field) {
                        if let Value::Array(arr) = val {
                            arr
                        } else {
                            vec![val]
                        }
                    } else {
                        Vec::new()
                    };

                    if items.is_empty() {
                        continue;
                    }
                    self.current_doc = Some(doc);
                    self.current_items = items;
                    self.current_idx = 0;
                }
                Some(Err(e)) => return Some(Err(e)),
                None => return None,
            }
        }
    }
}

/// Dynamic running accumulator matching an `Aggregate` definition.
#[derive(Debug, Clone)]
pub enum RunningAccumulator {
    Count(CountAccumulator),
    Sum {
        plain: SumAccumulator,
        homomorphic: Option<HomomorphicSumAccumulator>,
        key_id: Option<String>,
        keys: Option<Arc<KeyRegistry>>,
    },
    HomomorphicSum {
        acc: Option<HomomorphicSumAccumulator>,
        key_id: Option<String>,
        keys: Option<Arc<KeyRegistry>>,
    },
    Avg(WelfordAccumulator),
    Min(MinAccumulator),
    Max(MaxAccumulator),
    Variance(WelfordAccumulator),
    StdDev(WelfordAccumulator),
    Quantile(TDigest, f64),
    // Boxed: the sketch is 2 KiB, the other accumulators a few words.
    ApproxCountDistinct(Box<HyperLogLog>),
    AnomalyCount(AnomalyAccumulator),
}

impl RunningAccumulator {
    #[must_use]
    pub fn from_aggregate(agg: &Aggregate, keys: Option<Arc<KeyRegistry>>) -> Self {
        match agg {
            Aggregate::Count { .. } => Self::Count(CountAccumulator::new()),
            Aggregate::Sum { .. } => Self::Sum {
                plain: SumAccumulator::new(),
                homomorphic: None,
                key_id: None,
                keys,
            },
            Aggregate::HomomorphicSum { key_id, .. } => Self::HomomorphicSum {
                acc: None,
                key_id: key_id.clone(),
                keys,
            },
            Aggregate::Avg { .. } => Self::Avg(WelfordAccumulator::new()),
            Aggregate::Min { .. } => Self::Min(MinAccumulator::new()),
            Aggregate::Max { .. } => Self::Max(MaxAccumulator::new()),
            Aggregate::Variance { .. } => Self::Variance(WelfordAccumulator::new()),
            Aggregate::StdDev { .. } => Self::StdDev(WelfordAccumulator::new()),
            Aggregate::Quantile { quantile, .. } => Self::Quantile(TDigest::new(100.0), *quantile),
            Aggregate::ApproxCountDistinct { .. } => {
                Self::ApproxCountDistinct(Box::new(HyperLogLog::new()))
            }
            Aggregate::AnomalyCount { threshold, .. } => {
                Self::AnomalyCount(AnomalyAccumulator::new(*threshold))
            }
        }
    }

    pub fn update(&mut self, doc: &StreamDocument, field: Option<&str>) {
        match self {
            Self::Count(c) => c.update(),
            Self::Sum {
                plain,
                homomorphic,
                key_id,
                keys,
            } => {
                if let Some(f_name) = field {
                    if let Some(s) = doc.extract_str(f_name) {
                        if s.starts_with("ENC:paillier:") {
                            if let Some(env) = EncryptedEnvelope::parse(&s) {
                                if homomorphic.is_none() {
                                    if let Some(k_reg) = keys {
                                        if let Some(pk) = k_reg.get_paillier_public(&env.key_id) {
                                            *homomorphic = Some(HomomorphicSumAccumulator::new(pk));
                                            *key_id = Some(env.key_id.clone());
                                        }
                                    }
                                }
                                if let Some(h) = homomorphic {
                                    let _ = h.update_envelope(&env);
                                }
                                return;
                            }
                        }
                    }
                    if let Some(f) = doc.extract_f64(f_name) {
                        plain.update(f);
                    }
                }
            }
            Self::HomomorphicSum { acc, key_id, keys } => {
                if let Some(f_name) = field {
                    if let Some(s) = doc.extract_str(f_name) {
                        if let Some(env) = EncryptedEnvelope::parse(&s) {
                            if acc.is_none() {
                                if let Some(k_reg) = keys {
                                    if let Some(pk) = k_reg.get_paillier_public(&env.key_id) {
                                        *acc = Some(HomomorphicSumAccumulator::new(pk));
                                        *key_id = Some(env.key_id.clone());
                                    }
                                }
                            }
                            if let Some(h) = acc {
                                let _ = h.update_envelope(&env);
                            }
                        }
                    }
                }
            }
            Self::Avg(w) | Self::Variance(w) | Self::StdDev(w) => {
                if let Some(f) = field.and_then(|name| doc.extract_f64(name)) {
                    w.update(f);
                }
            }
            Self::Min(m) => {
                if let Some(f) = field.and_then(|name| doc.extract_f64(name)) {
                    m.update(f);
                }
            }
            Self::Max(m) => {
                if let Some(f) = field.and_then(|name| doc.extract_f64(name)) {
                    m.update(f);
                }
            }
            Self::Quantile(td, _) => {
                if let Some(f) = field.and_then(|name| doc.extract_f64(name)) {
                    td.add(f);
                }
            }
            Self::ApproxCountDistinct(hll) => {
                if let Some(v) = field.and_then(|name| doc.extract_value(name)) {
                    hll.add_json_value(&v);
                }
            }
            Self::AnomalyCount(a) => {
                if let Some(f) = field.and_then(|name| doc.extract_f64(name)) {
                    a.update(f);
                }
            }
        }
    }

    pub fn finalize(&mut self) -> Value {
        match self {
            Self::Count(c) => Value::Number(c.value().into()),
            Self::Sum {
                plain,
                homomorphic,
                key_id,
                keys,
            } => {
                if let Some(h) = homomorphic {
                    if let (Some(kid), Some(k_reg)) = (key_id.as_deref(), keys) {
                        if let (Some(sk), Some(pk)) = (
                            k_reg.get_paillier_private(kid),
                            k_reg.get_paillier_public(kid),
                        ) {
                            let ct = h.ciphertext();
                            if ct.scale() == 0 {
                                if let Ok(i) = decrypt_paillier_i64(&sk, &pk, &ct) {
                                    return Value::Number(i.into());
                                }
                            } else if let Ok(f) = decrypt_paillier_f64(&sk, &pk, &ct) {
                                return serde_json::Number::from_f64(f)
                                    .map_or(Value::Null, Value::Number);
                            }
                        }
                        return Value::String(h.to_envelope(kid));
                    }
                    Value::Null
                } else {
                    serde_json::Number::from_f64(plain.value()).map_or(Value::Null, Value::Number)
                }
            }
            Self::HomomorphicSum { acc, key_id, keys } => {
                if let Some(h) = acc {
                    if let (Some(kid), Some(k_reg)) = (key_id.as_deref(), keys) {
                        if let (Some(sk), Some(pk)) = (
                            k_reg.get_paillier_private(kid),
                            k_reg.get_paillier_public(kid),
                        ) {
                            let ct = h.ciphertext();
                            if ct.scale() == 0 {
                                if let Ok(i) = decrypt_paillier_i64(&sk, &pk, &ct) {
                                    return Value::Number(i.into());
                                }
                            } else if let Ok(f) = decrypt_paillier_f64(&sk, &pk, &ct) {
                                return serde_json::Number::from_f64(f)
                                    .map_or(Value::Null, Value::Number);
                            }
                        }
                        return Value::String(h.to_envelope(kid));
                    }
                    Value::Null
                } else {
                    Value::Null
                }
            }
            Self::Avg(w) => {
                serde_json::Number::from_f64(w.mean()).map_or(Value::Null, Value::Number)
            }
            Self::Min(m) => m
                .value()
                .and_then(serde_json::Number::from_f64)
                .map_or(Value::Null, Value::Number),
            Self::Max(m) => m
                .value()
                .and_then(serde_json::Number::from_f64)
                .map_or(Value::Null, Value::Number),
            Self::Variance(w) => {
                serde_json::Number::from_f64(w.sample_variance()).map_or(Value::Null, Value::Number)
            }
            Self::StdDev(w) => {
                serde_json::Number::from_f64(w.std_dev()).map_or(Value::Null, Value::Number)
            }
            Self::Quantile(td, q) => {
                serde_json::Number::from_f64(td.quantile(*q)).map_or(Value::Null, Value::Number)
            }
            Self::ApproxCountDistinct(hll) => Value::Number(hll.count().into()),
            Self::AnomalyCount(a) => Value::Number(a.anomaly_count().into()),
        }
    }
}

/// Executes single-pass streaming grouping over an upstream document stream.
///
/// # Errors
/// Propagates errors from the operations it performs.
pub fn execute_group_by<I: Iterator<Item = Result<StreamDocument>>>(
    upstream: I,
    group_key_field: Option<&str>,
    aggregates: &[Aggregate],
    keys: Option<Arc<KeyRegistry>>,
) -> Result<Vec<Value>> {
    let mut groups: HashMap<String, Vec<RunningAccumulator>> = HashMap::new();

    for res in upstream {
        let doc = res?;
        let key = if let Some(field) = group_key_field {
            doc.extract_str(field).unwrap_or_else(|| "null".to_string())
        } else {
            String::new()
        };

        let accs = groups.entry(key).or_insert_with(|| {
            aggregates
                .iter()
                .map(|agg| RunningAccumulator::from_aggregate(agg, keys.as_ref().map(Arc::clone)))
                .collect()
        });

        for (agg_def, acc) in aggregates.iter().zip(accs.iter_mut()) {
            acc.update(&doc, agg_def.target_field());
        }
    }

    let mut results = Vec::with_capacity(groups.len());
    for (group_key, mut accs) in groups {
        let mut map = serde_json::Map::new();
        if let Some(field) = group_key_field {
            map.insert(field.to_string(), Value::String(group_key));
        }

        for (agg_def, acc) in aggregates.iter().zip(accs.iter_mut()) {
            map.insert(agg_def.output_name().to_string(), acc.finalize());
        }

        results.push(Value::Object(map));
    }

    Ok(results)
}

#[derive(Debug, Clone, PartialEq)]
enum SortKey {
    Number(f64),
    Text(String),
    Null,
}

impl SortKey {
    fn from_value(val: Option<&Value>) -> Self {
        match val {
            Some(Value::Number(n)) => {
                if let Some(f) = n.as_f64() {
                    Self::Number(f)
                } else {
                    Self::Null
                }
            }
            Some(Value::String(s)) => Self::Text(s.clone()),
            _ => Self::Null,
        }
    }

    fn cmp_key(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Number(a), Self::Number(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
            (Self::Text(a), Self::Text(b)) => a.cmp(b),
            (Self::Null, Self::Null) => Ordering::Equal,
            (Self::Null, _) | (Self::Number(_), Self::Text(_)) => Ordering::Less,
            (_, Self::Null) | (Self::Text(_), Self::Number(_)) => Ordering::Greater,
        }
    }
}

#[derive(Debug)]
struct HeapItem {
    item: Value,
    key: SortKey,
    descending: bool,
}

impl PartialEq for HeapItem {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for HeapItem {}

impl PartialOrd for HeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapItem {
    fn cmp(&self, other: &Self) -> Ordering {
        // For Top-K:
        // When descending (we want largest), the heap acts as a MIN-heap,
        // so smallest key has highest priority (reversed order).
        if self.descending {
            other.key.cmp_key(&self.key)
        } else {
            self.key.cmp_key(&other.key)
        }
    }
}

/// Applies Top-K streaming sort using a bounded heap of size `limit`.
#[must_use]
pub fn apply_top_k(items: Vec<Value>, sort: &Sort, limit: usize) -> Vec<Value> {
    if limit == 0 || items.is_empty() {
        return Vec::new();
    }

    let descending = sort.direction == SortDirection::Desc;
    let mut heap = BinaryHeap::with_capacity(limit + 1);

    for item in items {
        let key = SortKey::from_value(item.get(&sort.field));
        let heap_item = HeapItem {
            item,
            key,
            descending,
        };

        if heap.len() < limit {
            heap.push(heap_item);
        } else if let Some(min_top) = heap.peek() {
            // If descending, we want elements with key > min_top.key
            let should_replace = if descending {
                heap_item.key.cmp_key(&min_top.key) == Ordering::Greater
            } else {
                heap_item.key.cmp_key(&min_top.key) == Ordering::Less
            };

            if should_replace {
                heap.pop();
                heap.push(heap_item);
            }
        }
    }

    let mut result: Vec<Value> = heap.into_iter().map(|h| h.item).collect();
    // Sort the final top-k elements
    result.sort_by(|a, b| {
        let k1 = SortKey::from_value(a.get(&sort.field));
        let k2 = SortKey::from_value(b.get(&sort.field));
        if descending {
            k2.cmp_key(&k1)
        } else {
            k1.cmp_key(&k2)
        }
    });

    result
}

/// Sorts a vector of JSON values fully when unbounded.
#[must_use]
pub fn apply_full_sort(mut items: Vec<Value>, sort: &Sort) -> Vec<Value> {
    let descending = sort.direction == SortDirection::Desc;
    items.sort_by(|a, b| {
        let k1 = SortKey::from_value(a.get(&sort.field));
        let k2 = SortKey::from_value(b.get(&sort.field));
        if descending {
            k2.cmp_key(&k1)
        } else {
            k1.cmp_key(&k2)
        }
    });
    items
}
