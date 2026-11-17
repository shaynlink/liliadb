use std::collections::{HashMap, HashSet, VecDeque};
use serde::{Deserialize, Serialize};

/// Target entity in LiliaDB that can be tracked and prefetched.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AccessTarget {
    Json {
        space: String,
        id: String,
    },
    Kv {
        namespace: String,
        #[serde(with = "serde_bytes")]
        key: Vec<u8>,
    },
    GraphNode {
        graph: String,
        id: String,
    },
    Bitemporal {
        space: String,
        id: String,
    },
}

impl AccessTarget {
    #[must_use]
    pub fn json(space: impl Into<String>, id: impl Into<String>) -> Self {
        Self::Json {
            space: space.into(),
            id: id.into(),
        }
    }

    #[must_use]
    pub fn kv(namespace: impl Into<String>, key: impl Into<Vec<u8>>) -> Self {
        Self::Kv {
            namespace: namespace.into(),
            key: key.into(),
        }
    }

    #[must_use]
    pub fn graph_node(graph: impl Into<String>, id: impl Into<String>) -> Self {
        Self::GraphNode {
            graph: graph.into(),
            id: id.into(),
        }
    }

    #[must_use]
    pub fn bitemporal(space: impl Into<String>, id: impl Into<String>) -> Self {
        Self::Bitemporal {
            space: space.into(),
            id: id.into(),
        }
    }

    /// Approximate size in memory bytes.
    #[must_use]
    pub fn estimated_size(&self) -> usize {
        match self {
            Self::Json { space, id } => space.len() + id.len() + 16,
            Self::Kv { namespace, key } => namespace.len() + key.len() + 16,
            Self::GraphNode { graph, id } => graph.len() + id.len() + 16,
            Self::Bitemporal { space, id } => space.len() + id.len() + 16,
        }
    }
}

/// Configuration parameters for the Markov Chain probability micro-model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarkovConfig {
    /// Maximum number of distinct states kept in RAM.
    pub max_states: usize,
    /// Maximum number of outgoing transition edges per state.
    pub max_transitions_per_state: usize,
    /// Minimum transition probability required to trigger a speculative prefetch (e.g. 0.50).
    pub confidence_threshold: f64,
    /// Minimum observation count for a transition before it qualifies for prediction.
    pub min_observations: u16,
    /// Total transition weight at which edge counters are decayed by 2 (half-life aging).
    pub decay_threshold: u16,
    /// Maximum elapsed time between two operations to consider them part of the same sequence.
    pub session_window_ms: u64,
}

impl Default for MarkovConfig {
    fn default() -> Self {
        Self {
            max_states: 512,
            max_transitions_per_state: 8,
            confidence_threshold: 0.50,
            min_observations: 2,
            decay_threshold: 500,
            session_window_ms: 2_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StateNode {
    transitions: Vec<(AccessTarget, u16)>,
    total_weight: u16,
    last_accessed_at_ms: u64,
}

impl StateNode {
    fn new(now_ms: u64) -> Self {
        Self {
            transitions: Vec::new(),
            total_weight: 0,
            last_accessed_at_ms: now_ms,
        }
    }

    fn record_transition(&mut self, target: AccessTarget, max_transitions: usize, decay_threshold: u16) {
        if let Some((_, count)) = self.transitions.iter_mut().find(|(t, _)| *t == target) {
            *count = count.saturating_add(1);
        } else {
            self.transitions.push((target, 1));
        }

        self.total_weight = self.total_weight.saturating_add(1);

        // Sort descending by count
        self.transitions.sort_by(|a, b| b.1.cmp(&a.1));

        // Truncate to max transitions allowed
        if self.transitions.len() > max_transitions {
            self.transitions.truncate(max_transitions);
            // Recompute total weight of retained transitions
            self.total_weight = self.transitions.iter().map(|(_, c)| *c).sum();
        }

        // Half-life decay when total weight reaches decay_threshold
        if self.total_weight >= decay_threshold {
            let mut new_total = 0_u16;
            for (_, count) in &mut self.transitions {
                *count = (*count / 2).max(1);
                new_total = new_total.saturating_add(*count);
            }
            self.total_weight = new_total;
        }
    }
}

mod map_as_pairs {
    use super::*;
    use serde::ser::SerializeSeq;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S>(
        map: &HashMap<AccessTarget, StateNode>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(map.len()))?;
        for pair in map {
            seq.serialize_element(&pair)?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> std::result::Result<HashMap<AccessTarget, StateNode>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let vec = Vec::<(AccessTarget, StateNode)>::deserialize(deserializer)?;
        Ok(vec.into_iter().collect())
    }
}

/// Real-time statistics about the Markov model footprint and performance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkovStats {
    pub state_count: usize,
    pub transition_count: usize,
    pub estimated_memory_bytes: usize,
    pub total_recorded_transitions: u64,
    pub predictions_generated: u64,
}

/// Compact bounded Markov Chain micro-model for predictive prefetching.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkovModel {
    config: MarkovConfig,
    #[serde(with = "map_as_pairs")]
    states: HashMap<AccessTarget, StateNode>,
    lru_order: VecDeque<AccessTarget>,
    total_recorded_transitions: u64,
    predictions_generated: u64,
}

impl MarkovModel {
    #[must_use]
    pub fn new(config: MarkovConfig) -> Self {
        Self {
            config,
            states: HashMap::new(),
            lru_order: VecDeque::new(),
            total_recorded_transitions: 0,
            predictions_generated: 0,
        }
    }

    #[must_use]
    pub fn config(&self) -> &MarkovConfig {
        &self.config
    }

    /// Record a transition from `from` to `to`.
    pub fn record_transition(&mut self, from: AccessTarget, to: AccessTarget, now_ms: u64) {
        // Prevent trivial self-transitions
        if from == to {
            return;
        }

        self.total_recorded_transitions = self.total_recorded_transitions.saturating_add(1);

        // Ensure state exists or evict if capacity exceeded
        if !self.states.contains_key(&from) && self.states.len() >= self.config.max_states {
            self.evict_oldest();
        }

        let max_transitions = self.config.max_transitions_per_state;
        let decay_threshold = self.config.decay_threshold;

        let node = self
            .states
            .entry(from.clone())
            .or_insert_with(|| StateNode::new(now_ms));
        node.last_accessed_at_ms = now_ms;
        node.record_transition(to, max_transitions, decay_threshold);

        // Update LRU tracking
        self.touch_lru(&from);
    }

    /// Predict next targets from `current` with transition probability >= confidence_threshold.
    ///
    /// Supports multi-hop chained predictions up to `max_depth`.
    #[must_use]
    pub fn predict(&self, current: &AccessTarget, max_depth: usize) -> Vec<(AccessTarget, f64)> {
        let mut results = Vec::new();
        let mut visited = HashSet::new();
        visited.insert(current.clone());

        let mut queue = VecDeque::new();
        queue.push_back((current.clone(), 1.0_f64, 0_usize));

        let min_confidence = self.config.confidence_threshold;
        let min_observations = self.config.min_observations;

        while let Some((node_target, current_prob, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }

            if let Some(node) = self.states.get(&node_target) {
                if node.total_weight == 0 {
                    continue;
                }

                let total = f64::from(node.total_weight);
                for (next_target, count) in &node.transitions {
                    if *count < min_observations {
                        continue;
                    }

                    let step_prob = f64::from(*count) / total;
                    let chained_prob = current_prob * step_prob;

                    if chained_prob >= min_confidence && visited.insert(next_target.clone()) {
                        results.push((next_target.clone(), chained_prob));
                        queue.push_back((next_target.clone(), chained_prob, depth + 1));
                    }
                }
            }
        }

        // Sort descending by chained probability
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results
    }

    /// Mutably records an access prediction increment.
    pub fn mark_predictions_generated(&mut self, count: usize) {
        self.predictions_generated = self
            .predictions_generated
            .saturating_add(count as u64);
    }

    /// Returns telemetry and memory statistics for the Markov model.
    #[must_use]
    pub fn stats(&self) -> MarkovStats {
        let mut transitions = 0;
        let mut bytes = std::mem::size_of::<Self>();

        for (target, node) in &self.states {
            bytes += target.estimated_size() + std::mem::size_of::<StateNode>();
            transitions += node.transitions.len();
            for (dest, _) in &node.transitions {
                bytes += dest.estimated_size() + std::mem::size_of::<(AccessTarget, u16)>();
            }
        }

        MarkovStats {
            state_count: self.states.len(),
            transition_count: transitions,
            estimated_memory_bytes: bytes,
            total_recorded_transitions: self.total_recorded_transitions,
            predictions_generated: self.predictions_generated,
        }
    }

    /// Clears all recorded Markov states and resets statistics.
    pub fn clear(&mut self) {
        self.states.clear();
        self.lru_order.clear();
        self.total_recorded_transitions = 0;
        self.predictions_generated = 0;
    }

    fn touch_lru(&mut self, target: &AccessTarget) {
        if let Some(pos) = self.lru_order.iter().position(|t| t == target) {
            self.lru_order.remove(pos);
        }
        self.lru_order.push_back(target.clone());
    }

    fn evict_oldest(&mut self) {
        while let Some(oldest) = self.lru_order.pop_front() {
            if self.states.remove(&oldest).is_some() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_markov_transition_probability() {
        let mut model = MarkovModel::new(MarkovConfig {
            confidence_threshold: 0.50,
            min_observations: 2,
            ..Default::default()
        });

        let doc_conv = AccessTarget::json("conversations", "conv_12");
        let doc_msg1 = AccessTarget::json("messages", "msg_1");
        let doc_msg2 = AccessTarget::json("messages", "msg_2");

        // conv_12 -> msg_1 (3 times)
        // conv_12 -> msg_2 (1 time)
        model.record_transition(doc_conv.clone(), doc_msg1.clone(), 1000);
        model.record_transition(doc_conv.clone(), doc_msg1.clone(), 1050);
        model.record_transition(doc_conv.clone(), doc_msg1.clone(), 1100);
        model.record_transition(doc_conv.clone(), doc_msg2.clone(), 1150);

        // msg_1 probability = 3 / 4 = 0.75 >= 0.50 and count 3 >= 2 -> should be predicted
        // msg_2 count = 1 < min_observations (2) -> should NOT be predicted
        let predictions = model.predict(&doc_conv, 1);
        assert_eq!(predictions.len(), 1);
        assert_eq!(predictions[0].0, doc_msg1);
        assert!((predictions[0].1 - 0.75).abs() < 1e-6);
    }

    #[test]
    fn test_markov_chained_predictions() {
        let mut model = MarkovModel::new(MarkovConfig {
            confidence_threshold: 0.40,
            min_observations: 2,
            ..Default::default()
        });

        let a = AccessTarget::json("screen", "home");
        let b = AccessTarget::json("conversations", "conv_12");
        let c = AccessTarget::json("messages", "msg_1");

        // a -> b with 100% prob
        for _ in 0..5 {
            model.record_transition(a.clone(), b.clone(), 1000);
        }
        // b -> c with 80% prob
        for _ in 0..4 {
            model.record_transition(b.clone(), c.clone(), 1000);
        }
        let other = AccessTarget::json("other", "x");
        model.record_transition(b.clone(), other, 1000);

        // Depth 1: only b is predicted
        let pred_d1 = model.predict(&a, 1);
        assert_eq!(pred_d1.len(), 1);
        assert_eq!(pred_d1[0].0, b);

        // Depth 2: b and c should both be predicted (c prob = 1.0 * 0.8 = 0.8 >= 0.4)
        let pred_d2 = model.predict(&a, 2);
        assert_eq!(pred_d2.len(), 2);
        assert_eq!(pred_d2[0].0, b);
        assert_eq!(pred_d2[1].0, c);
        assert!((pred_d2[1].1 - 0.8).abs() < 1e-6);
    }

    #[test]
    fn test_markov_capacity_eviction() {
        let mut model = MarkovModel::new(MarkovConfig {
            max_states: 3,
            ..Default::default()
        });

        let s1 = AccessTarget::json("space", "1");
        let s2 = AccessTarget::json("space", "2");
        let s3 = AccessTarget::json("space", "3");
        let s4 = AccessTarget::json("space", "4");
        let target = AccessTarget::json("space", "target");

        model.record_transition(s1.clone(), target.clone(), 100);
        model.record_transition(s2.clone(), target.clone(), 200);
        model.record_transition(s3.clone(), target.clone(), 300);

        assert_eq!(model.states.len(), 3);
        assert!(model.states.contains_key(&s1));

        // Adding s4 should evict s1 (oldest in LRU)
        model.record_transition(s4.clone(), target, 400);
        assert_eq!(model.states.len(), 3);
        assert!(!model.states.contains_key(&s1));
        assert!(model.states.contains_key(&s4));
    }

    #[test]
    fn test_markov_half_life_decay() {
        let mut model = MarkovModel::new(MarkovConfig {
            decay_threshold: 10,
            max_transitions_per_state: 4,
            ..Default::default()
        });

        let a = AccessTarget::json("space", "a");
        let b = AccessTarget::json("space", "b");

        for _ in 0..10 {
            model.record_transition(a.clone(), b.clone(), 100);
        }

        let node = model.states.get(&a).unwrap();
        // Counter was halved when reaching 10 -> 5
        assert_eq!(node.total_weight, 5);
        assert_eq!(node.transitions[0].1, 5);
    }

    #[test]
    fn test_all_target_types_supported() {
        let mut model = MarkovModel::new(MarkovConfig::default());

        let json_t = AccessTarget::json("docs", "doc_1");
        let kv_t = AccessTarget::kv("settings", b"theme");
        let graph_t = AccessTarget::graph_node("social", "user_42");
        let bitemp_t = AccessTarget::bitemporal("deals", "deal_99");

        model.record_transition(json_t.clone(), kv_t.clone(), 100);
        model.record_transition(kv_t.clone(), graph_t.clone(), 200);
        model.record_transition(graph_t.clone(), bitemp_t.clone(), 300);

        assert_eq!(model.states.len(), 3);
        let stats = model.stats();
        assert_eq!(stats.state_count, 3);
        assert_eq!(stats.transition_count, 3);
        assert!(stats.estimated_memory_bytes > 0);
    }

    #[test]
    fn test_markov_serialization_roundtrip() {
        let mut model = MarkovModel::new(MarkovConfig::default());
        let a = AccessTarget::json("conv", "1");
        let b = AccessTarget::json("msg", "1");
        model.record_transition(a.clone(), b.clone(), 1000);

        let serialized = serde_json::to_vec(&model).expect("serialize markov model");
        let deserialized: MarkovModel =
            serde_json::from_slice(&serialized).expect("deserialize markov model");

        assert_eq!(deserialized.stats().state_count, 1);
        let preds = deserialized.predict(&a, 1);
        assert_eq!(preds.len(), 0); // min_observations is 2 by default
    }
}
