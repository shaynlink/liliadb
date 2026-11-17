#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::hash::{Hash, Hasher};

/// Running counter accumulator.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CountAccumulator {
    count: u64,
}

impl CountAccumulator {
    #[must_use]
    pub fn new() -> Self {
        Self { count: 0 }
    }

    pub fn update(&mut self) {
        self.count = self.count.saturating_add(1);
    }

    #[must_use]
    pub fn value(&self) -> u64 {
        self.count
    }
}

/// Running sum accumulator.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SumAccumulator {
    sum: f64,
}

impl SumAccumulator {
    #[must_use]
    pub fn new() -> Self {
        Self { sum: 0.0 }
    }

    pub fn update(&mut self, value: f64) {
        if !value.is_nan() {
            self.sum += value;
        }
    }

    #[must_use]
    pub fn value(&self) -> f64 {
        self.sum
    }
}

/// Running minimum accumulator.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MinAccumulator {
    min: Option<f64>,
}

impl MinAccumulator {
    #[must_use]
    pub fn new() -> Self {
        Self { min: None }
    }

    pub fn update(&mut self, value: f64) {
        if !value.is_nan() {
            self.min = Some(match self.min {
                Some(curr) => curr.min(value),
                None => value,
            });
        }
    }

    #[must_use]
    pub fn value(&self) -> Option<f64> {
        self.min
    }
}

/// Running maximum accumulator.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MaxAccumulator {
    max: Option<f64>,
}

impl MaxAccumulator {
    #[must_use]
    pub fn new() -> Self {
        Self { max: None }
    }

    pub fn update(&mut self, value: f64) {
        if !value.is_nan() {
            self.max = Some(match self.max {
                Some(curr) => curr.max(value),
                None => value,
            });
        }
    }

    #[must_use]
    pub fn value(&self) -> Option<f64> {
        self.max
    }
}

/// Single-pass streaming Welford accumulator for numerically stable mean, variance, and standard deviation.
///
/// Occupies strictly 24 bytes in memory.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WelfordAccumulator {
    count: u64,
    mean: f64,
    m2: f64,
}

impl WelfordAccumulator {
    #[must_use]
    pub fn new() -> Self {
        Self {
            count: 0,
            mean: 0.0,
            m2: 0.0,
        }
    }

    pub fn update(&mut self, value: f64) {
        if value.is_nan() {
            return;
        }
        self.count = self.count.saturating_add(1);
        let delta = value - self.mean;
        self.mean += delta / (self.count as f64);
        let delta2 = value - self.mean;
        self.m2 += delta * delta2;
    }

    #[must_use]
    pub fn count(&self) -> u64 {
        self.count
    }

    #[must_use]
    pub fn mean(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.mean
        }
    }

    #[must_use]
    pub fn sample_variance(&self) -> f64 {
        if self.count > 1 {
            self.m2 / ((self.count - 1) as f64)
        } else {
            0.0
        }
    }

    #[must_use]
    pub fn population_variance(&self) -> f64 {
        if self.count > 0 {
            self.m2 / (self.count as f64)
        } else {
            0.0
        }
    }

    #[must_use]
    pub fn std_dev(&self) -> f64 {
        self.population_variance().sqrt()
    }
}

/// Centroid for the T-Digest streaming quantile estimator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Centroid {
    pub mean: f64,
    pub weight: f64,
}

/// Online streaming T-Digest for approximate percentiles and quantiles with bounded O(1) memory.
#[derive(Debug, Clone, PartialEq)]
pub struct TDigest {
    compression: f64,
    centroids: Vec<Centroid>,
    buffer: Vec<f64>,
    total_weight: f64,
    buffer_capacity: usize,
}

impl Default for TDigest {
    fn default() -> Self {
        Self::new(100.0)
    }
}

impl TDigest {
    #[must_use]
    pub fn new(compression: f64) -> Self {
        let delta = compression.clamp(20.0, 500.0);
        let buffer_capacity = (delta * 2.0) as usize;
        Self {
            compression: delta,
            centroids: Vec::with_capacity(delta as usize + 1),
            buffer: Vec::with_capacity(buffer_capacity),
            total_weight: 0.0,
            buffer_capacity,
        }
    }

    pub fn add(&mut self, value: f64) {
        if value.is_nan() {
            return;
        }
        self.buffer.push(value);
        if self.buffer.len() >= self.buffer_capacity {
            self.compress();
        }
    }

    pub fn compress(&mut self) {
        if self.buffer.is_empty() {
            return;
        }
        self.buffer
            .sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let mut all_points = Vec::with_capacity(self.centroids.len() + self.buffer.len());
        for c in &self.centroids {
            all_points.push(*c);
        }
        for &x in &self.buffer {
            all_points.push(Centroid {
                mean: x,
                weight: 1.0,
            });
            self.total_weight += 1.0;
        }
        self.buffer.clear();

        all_points
            .sort_unstable_by(|a, b| a.mean.partial_cmp(&b.mean).unwrap_or(std::cmp::Ordering::Equal));

        let mut new_centroids = Vec::with_capacity(self.compression as usize + 1);
        if let Some(first) = all_points.first() {
            let mut current = *first;
            let mut weight_so_far = 0.0;
            let total = self.total_weight;

            for &p in &all_points[1..] {
                let q = (weight_so_far + current.weight / 2.0) / total;
                let k = 4.0 * total * q * (1.0 - q) / self.compression;
                let max_weight = k.max(1.0);

                if current.weight + p.weight <= max_weight {
                    let total_w = current.weight + p.weight;
                    current.mean = (current.mean * current.weight + p.mean * p.weight) / total_w;
                    current.weight = total_w;
                } else {
                    weight_so_far += current.weight;
                    new_centroids.push(current);
                    current = p;
                }
            }
            new_centroids.push(current);
        }

        self.centroids = new_centroids;
    }

    #[must_use]
    pub fn quantile(&mut self, q: f64) -> f64 {
        self.compress();
        if self.centroids.is_empty() {
            return 0.0;
        }
        if self.centroids.len() == 1 {
            return self.centroids[0].mean;
        }

        let q_clamped = q.clamp(0.0, 1.0);
        let target_weight = q_clamped * self.total_weight;

        if target_weight <= self.centroids[0].weight / 2.0 {
            return self.centroids[0].mean;
        }
        let last_idx = self.centroids.len() - 1;
        if target_weight >= self.total_weight - (self.centroids[last_idx].weight / 2.0) {
            return self.centroids[last_idx].mean;
        }

        let mut cum_weight = self.centroids[0].weight / 2.0;
        for i in 0..last_idx {
            let c1 = self.centroids[i];
            let c2 = self.centroids[i + 1];
            let step_weight = f64::midpoint(c1.weight, c2.weight);
            if cum_weight + step_weight >= target_weight {
                let fraction = (target_weight - cum_weight) / step_weight;
                return c1.mean + fraction * (c2.mean - c1.mean);
            }
            cum_weight += step_weight;
        }

        self.centroids[last_idx].mean
    }

    #[must_use]
    pub fn iqr(&mut self) -> f64 {
        let q75 = self.quantile(0.75);
        let q25 = self.quantile(0.25);
        (q75 - q25).max(0.0)
    }
}

/// `HyperLogLog` cardinality estimator with 2048 registers (~2 KB memory) and ~2.3% error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HyperLogLog {
    registers: [u8; 2048],
}

impl Default for HyperLogLog {
    fn default() -> Self {
        Self::new()
    }
}

impl HyperLogLog {
    #[must_use]
    pub fn new() -> Self {
        Self {
            registers: [0; 2048],
        }
    }

    pub fn add_hash(&mut self, hash: u64) {
        // Use top 11 bits for index (2^11 = 2048)
        let idx = (hash >> 53) as usize;
        // Remaining 53 bits for leading zeros
        let remaining = (hash << 11) | (1 << 10);
        let zeros = (remaining.leading_zeros() as u8) + 1;
        if zeros > self.registers[idx] {
            self.registers[idx] = zeros;
        }
    }

    pub fn add<T: Hash>(&mut self, value: &T) {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        value.hash(&mut hasher);
        let h = hasher.finish();
        // Splitmix64 mixing for high dispersion
        let mut x = h;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^= x >> 31;
        self.add_hash(x);
    }

    pub fn add_json_value(&mut self, val: &serde_json::Value) {
        match val {
            serde_json::Value::String(s) => self.add(s),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    self.add(&i);
                } else if let Some(f) = n.as_f64() {
                    self.add(&f.to_bits());
                }
            }
            serde_json::Value::Bool(b) => self.add(b),
            serde_json::Value::Null => self.add(&0u8),
            _ => self.add(&val.to_string()),
        }
    }

    #[must_use]
    pub fn count(&self) -> u64 {
        const M: f64 = 2048.0;
        const ALPHA: f64 = 0.7213 / (1.0 + 1.079 / M);

        let mut sum = 0.0;
        let mut zero_count = 0;

        for &r in &self.registers {
            sum += 2.0_f64.powi(-i32::from(r));
            if r == 0 {
                zero_count += 1;
            }
        }

        let raw_estimate = ALPHA * M * M / sum;

        // Small range correction (Linear Counting)
        if raw_estimate <= 2.5 * M && zero_count > 0 {
            let lc = M * (M / f64::from(zero_count)).ln();
            lc.round() as u64
        } else {
            raw_estimate.round() as u64
        }
    }

    pub fn merge(&mut self, other: &Self) {
        for (a, &b) in self.registers.iter_mut().zip(other.registers.iter()) {
            *a = (*a).max(b);
        }
    }
}

/// Streaming online anomaly detector using running Welford statistics.
#[derive(Debug, Clone, PartialEq)]
pub struct AnomalyAccumulator {
    welford: WelfordAccumulator,
    threshold_sigmas: f64,
    anomalies_count: u64,
    warmup_samples: u64,
}

impl AnomalyAccumulator {
    #[must_use]
    pub fn new(threshold_sigmas: f64) -> Self {
        Self {
            welford: WelfordAccumulator::new(),
            threshold_sigmas: threshold_sigmas.abs().max(1.0),
            anomalies_count: 0,
            warmup_samples: 10,
        }
    }

    pub fn update(&mut self, value: f64) {
        if value.is_nan() {
            return;
        }

        if self.welford.count() >= self.warmup_samples {
            let std_dev = self.welford.std_dev();
            if std_dev > 1e-9 {
                let z_score = (value - self.welford.mean()).abs() / std_dev;
                if z_score >= self.threshold_sigmas {
                    self.anomalies_count = self.anomalies_count.saturating_add(1);
                }
            }
        }

        self.welford.update(value);
    }

    #[must_use]
    pub fn anomaly_count(&self) -> u64 {
        self.anomalies_count
    }

    #[must_use]
    pub fn anomaly_rate(&self) -> f64 {
        if self.welford.count() == 0 {
            0.0
        } else {
            (self.anomalies_count as f64) / (self.welford.count() as f64)
        }
    }

    #[must_use]
    pub fn mean(&self) -> f64 {
        self.welford.mean()
    }

    #[must_use]
    pub fn std_dev(&self) -> f64 {
        self.welford.std_dev()
    }
}
