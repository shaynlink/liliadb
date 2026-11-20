use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Weight/differential associated with a record in differential dataflow (+1 for insert, -1 for delete).
pub type DiffWeight = isize;

/// A record paired with its differential multiplicity (+Δ or -Δ).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diff<T> {
    pub item: T,
    pub weight: DiffWeight,
}

impl<T> Diff<T> {
    #[must_use]
    pub const fn new(item: T, weight: DiffWeight) -> Self {
        Self { item, weight }
    }

    #[must_use]
    pub const fn insert(item: T) -> Self {
        Self { item, weight: 1 }
    }

    #[must_use]
    pub const fn delete(item: T) -> Self {
        Self { item, weight: -1 }
    }

    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> Diff<U> {
        Diff {
            item: f(self.item),
            weight: self.weight,
        }
    }
}

/// Differential running sum accumulator. Updates in O(1) time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiffSum {
    sum: f64,
}

impl DiffSum {
    #[must_use]
    pub const fn new() -> Self {
        Self { sum: 0.0 }
    }

    pub fn update(&mut self, val: f64, weight: DiffWeight) {
        if !val.is_nan() {
            #[allow(clippy::cast_precision_loss)]
            let delta = val * (weight as f64);
            self.sum += delta;
        }
    }

    #[must_use]
    pub const fn value(&self) -> f64 {
        self.sum
    }
}

/// Differential running count accumulator. Updates in O(1) time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffCount {
    count: i64,
}

impl DiffCount {
    #[must_use]
    pub const fn new() -> Self {
        Self { count: 0 }
    }

    pub fn update(&mut self, weight: DiffWeight) {
        self.count += weight as i64;
    }

    #[must_use]
    pub const fn value(&self) -> i64 {
        self.count
    }
}

/// Differential running average accumulator. Updates in O(1) time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiffAvg {
    sum: DiffSum,
    count: DiffCount,
}

impl DiffAvg {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            sum: DiffSum::new(),
            count: DiffCount::new(),
        }
    }

    pub fn update(&mut self, val: f64, weight: DiffWeight) {
        self.sum.update(val, weight);
        self.count.update(weight);
    }

    #[must_use]
    pub fn value(&self) -> Option<f64> {
        let cnt = self.count.value();
        if cnt <= 0 {
            None
        } else {
            #[allow(clippy::cast_precision_loss)]
            Some(self.sum.value() / (cnt as f64))
        }
    }
}

/// Helper for ordering floating point numbers in differential min/max multisets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrderedFloat(pub f64);

impl Eq for OrderedFloat {}

impl PartialOrd for OrderedFloat {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedFloat {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// Differential running minimum accumulator.
/// Tracks value multiplicity to handle element deletions without rescanning.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffMin {
    frequencies: BTreeMap<OrderedFloat, usize>,
}

impl DiffMin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            frequencies: BTreeMap::new(),
        }
    }

    pub fn update(&mut self, val: f64, weight: DiffWeight) {
        if val.is_nan() {
            return;
        }
        let key = OrderedFloat(val);
        if weight > 0 {
            *self.frequencies.entry(key).or_insert(0) += weight.unsigned_abs();
        } else if weight < 0 {
            if let Some(count) = self.frequencies.get_mut(&key) {
                let to_sub = weight.unsigned_abs();
                if *count <= to_sub {
                    self.frequencies.remove(&key);
                } else {
                    *count -= to_sub;
                }
            }
        }
    }

    #[must_use]
    pub fn value(&self) -> Option<f64> {
        self.frequencies.first_key_value().map(|(k, _)| k.0)
    }
}

/// Differential running maximum accumulator.
/// Tracks value multiplicity to handle element deletions without rescanning.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffMax {
    frequencies: BTreeMap<OrderedFloat, usize>,
}

impl DiffMax {
    #[must_use]
    pub fn new() -> Self {
        Self {
            frequencies: BTreeMap::new(),
        }
    }

    pub fn update(&mut self, val: f64, weight: DiffWeight) {
        if val.is_nan() {
            return;
        }
        let key = OrderedFloat(val);
        if weight > 0 {
            *self.frequencies.entry(key).or_insert(0) += weight.unsigned_abs();
        } else if weight < 0 {
            if let Some(count) = self.frequencies.get_mut(&key) {
                let to_sub = weight.unsigned_abs();
                if *count <= to_sub {
                    self.frequencies.remove(&key);
                } else {
                    *count -= to_sub;
                }
            }
        }
    }

    #[must_use]
    pub fn value(&self) -> Option<f64> {
        self.frequencies.last_key_value().map(|(k, _)| k.0)
    }
}

/// Specification of aggregation types supported in reactive dataflow pipelines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataflowAggKind {
    Sum(String),
    Count,
    Avg(String),
    Min(String),
    Max(String),
}

/// Event describing a differential update to a materialized view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewDiff {
    pub view_name: String,
    pub key: String,
    pub old_value: Option<f64>,
    pub new_value: Option<f64>,
    pub weight: DiffWeight,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_sum_constant_time() {
        let mut sum = DiffSum::new();
        sum.update(100.0, 1);
        sum.update(50.0, 1);
        assert!((sum.value() - 150.0).abs() < 1e-6);

        // Deletion
        sum.update(100.0, -1);
        assert!((sum.value() - 50.0).abs() < 1e-6);
    }

    #[test]
    fn test_diff_avg_constant_time() {
        let mut avg = DiffAvg::new();
        avg.update(10.0, 1);
        avg.update(20.0, 1);
        assert_eq!(avg.value(), Some(15.0));

        avg.update(20.0, -1);
        assert_eq!(avg.value(), Some(10.0));

        avg.update(10.0, -1);
        assert_eq!(avg.value(), None);
    }

    #[test]
    fn test_diff_min_max_multiset() {
        let mut min = DiffMin::new();
        let mut max = DiffMax::new();

        min.update(10.0, 1);
        min.update(5.0, 1);
        max.update(10.0, 1);
        max.update(5.0, 1);

        assert_eq!(min.value(), Some(5.0));
        assert_eq!(max.value(), Some(10.0));

        // Delete 5.0
        min.update(5.0, -1);
        max.update(10.0, -1);

        assert_eq!(min.value(), Some(10.0));
        assert_eq!(max.value(), Some(5.0));
    }
}
