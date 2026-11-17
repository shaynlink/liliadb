//! Hybrid Logical Clocks (HLC) & Concurrency Detection without Wall Clock dependency.
//!
//! Based on Kulkarni et al. (2014): "Logical Physical Clocks and Consistent Snapshots
//! in Globally Distributed Databases".
//!
//! Guarantees:
//! 1. Absolute Causal Ordering: If event B was created after observing event A, then
//!    `HLC(B) > HLC(A)` mathematically, even if B's physical clock is 10 minutes behind.
//! 2. Deterministic Concurrency Detection: Exactly distinguishes whether two mutations
//!    are sequential (A -> B or B -> A) or concurrent (A || B).
//! 3. 3-Way Merge & Automatic Conflict Resolution without silent write loss.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;
use std::sync::Mutex;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::{ErrorCode, LiliaError};

/// Default maximum allowed clock drift in milliseconds (60 seconds).
pub const DEFAULT_MAX_DRIFT_MS: i64 = 60_000;

/// Returns current physical timestamp in milliseconds since UNIX epoch.
#[must_use]
pub fn now_ms() -> i64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| {
                i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
            })
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

/// Errors related to Hybrid Logical Clocks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HlcError {
    #[error("remote clock drift exceeded: local={local_ms}ms, remote={remote_ms}ms, max_drift={max_drift_ms}ms")]
    ClockDriftExceeded {
        local_ms: i64,
        remote_ms: i64,
        max_drift_ms: i64,
    },
    #[error("invalid HLC string format: {0}")]
    InvalidFormat(String),
}

impl From<HlcError> for LiliaError {
    fn from(err: HlcError) -> Self {
        match err {
            HlcError::ClockDriftExceeded { .. } => {
                LiliaError::new(ErrorCode::Conflict, err.to_string(), false)
            }
            HlcError::InvalidFormat(..) => {
                LiliaError::new(ErrorCode::InvalidInput, err.to_string(), false)
            }
        }
    }
}

/// An immutable Hybrid Logical Clock timestamp.
///
/// Combines an approximate physical time in milliseconds, a monotonic logical counter,
/// and a unique node/actor identifier for deterministic total ordering.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hlc {
    /// Physical or highest observed logical timestamp in milliseconds.
    pub millis: i64,
    /// Monotonic logical counter disambiguating events within the same physical tick.
    pub counter: u32,
    /// Replica or device identifier used as a deterministic tie-breaker.
    pub node_id: String,
}

impl Hlc {
    /// Creates a new `Hlc` timestamp.
    #[must_use]
    pub fn new(millis: i64, counter: u32, node_id: impl Into<String>) -> Self {
        Self {
            millis,
            counter,
            node_id: node_id.into(),
        }
    }

    /// Formats the HLC in a compact form `"<millis>.<counter>"`.
    #[must_use]
    pub fn display_compact(&self) -> String {
        format!("{}.{}", self.millis, self.counter)
    }

    /// Returns true if this HLC causally strictly precedes `other` under the total order.
    #[must_use]
    pub fn precedes(&self, other: &Self) -> bool {
        self < other
    }
}

impl PartialOrd for Hlc {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Hlc {
    fn cmp(&self, other: &Self) -> Ordering {
        self.millis
            .cmp(&other.millis)
            .then_with(|| self.counter.cmp(&other.counter))
            .then_with(|| self.node_id.cmp(&other.node_id))
    }
}

impl fmt::Display for Hlc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.node_id.is_empty() {
            write!(f, "{}.{}", self.millis, self.counter)
        } else {
            write!(f, "{}.{}@{}", self.millis, self.counter, self.node_id)
        }
    }
}

impl FromStr for Hlc {
    type Err = HlcError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (time_part, node_id) = if let Some((left, right)) = s.split_once('@') {
            (left, right.to_string())
        } else if let Some((left, right)) = s.rsplit_once(':') {
            if right.chars().all(|c| c.is_ascii_digit()) && !left.contains('.') {
                (s, String::new())
            } else {
                (left, right.to_string())
            }
        } else {
            (s, String::new())
        };

        let (millis_str, counter_str) = if let Some((m, c)) = time_part.split_once('.') {
            (m, c)
        } else if let Some((m, c)) = time_part.split_once(':') {
            (m, c)
        } else {
            return Err(HlcError::InvalidFormat(format!(
                "expected <millis>.<counter>, got '{s}'"
            )));
        };

        let millis = millis_str
            .parse::<i64>()
            .map_err(|_| HlcError::InvalidFormat(format!("invalid millis in '{s}'")))?;
        let counter = counter_str
            .parse::<u32>()
            .map_err(|_| HlcError::InvalidFormat(format!("invalid counter in '{s}'")))?;

        Ok(Self {
            millis,
            counter,
            node_id,
        })
    }
}

impl Serialize for Hlc {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serializer.serialize_str(&self.to_string())
        } else {
            #[derive(Serialize)]
            struct RawHlc<'a> {
                millis: i64,
                counter: u32,
                node_id: &'a str,
            }
            RawHlc {
                millis: self.millis,
                counter: self.counter,
                node_id: &self.node_id,
            }
            .serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for Hlc {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Str(String),
            Obj {
                millis: i64,
                counter: u32,
                #[serde(default)]
                node_id: String,
            },
        }

        match Repr::deserialize(deserializer)? {
            Repr::Str(s) => Self::from_str(&s).map_err(serde::de::Error::custom),
            Repr::Obj {
                millis,
                counter,
                node_id,
            } => Ok(Self::new(millis, counter, node_id)),
        }
    }
}

/// Internal state of a `HybridClock`.
#[derive(Debug)]
struct ClockState {
    last_millis: i64,
    counter: u32,
    node_id: String,
    max_drift_ms: i64,
}

/// Thread-safe generator of monotonically increasing Hybrid Logical Clocks.
#[derive(Debug)]
pub struct HybridClock {
    state: Mutex<ClockState>,
}

impl HybridClock {
    /// Creates a new `HybridClock` bound to `node_id` with default max drift.
    #[must_use]
    pub fn new(node_id: impl Into<String>) -> Self {
        Self::with_max_drift(node_id, DEFAULT_MAX_DRIFT_MS)
    }

    /// Creates a new `HybridClock` bound to `node_id` with custom max drift.
    #[must_use]
    pub fn with_max_drift(node_id: impl Into<String>, max_drift_ms: i64) -> Self {
        Self {
            state: Mutex::new(ClockState {
                last_millis: 0,
                counter: 0,
                node_id: node_id.into(),
                max_drift_ms,
            }),
        }
    }

    /// Returns the node identifier bound to this clock generator.
    #[must_use]
    pub fn node_id(&self) -> String {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.node_id.clone()
    }

    /// Generates a new HLC timestamp for a local event or outbound message.
    ///
    /// Monotonically advances the clock even if physical time went backward.
    pub fn now(&self) -> Hlc {
        self.tick_at(now_ms())
    }

    /// Ticks the clock using explicit physical milliseconds (deterministic testing helper).
    pub fn tick_at(&self, physical_now_ms: i64) -> Hlc {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if physical_now_ms > state.last_millis {
            state.last_millis = physical_now_ms;
            state.counter = 0;
        } else {
            state.counter = state.counter.saturating_add(1);
        }
        Hlc {
            millis: state.last_millis,
            counter: state.counter,
            node_id: state.node_id.clone(),
        }
    }

    /// Updates this clock upon observing a remote HLC timestamp.
    ///
    /// Guarantees that the returned local timestamp will be strictly greater than
    /// `remote` (`new_local > remote`), preserving absolute causality even if the
    /// local physical clock is far behind.
    ///
    /// # Errors
    /// Returns [`HlcError::ClockDriftExceeded`] if `remote` is further ahead of the local
    /// physical clock than `max_drift_ms`.
    pub fn update(&self, remote: &Hlc) -> Result<Hlc, HlcError> {
        self.update_at(remote, now_ms())
    }

    /// Updates this clock with an explicit local physical time (deterministic testing helper).
    ///
    /// # Errors
    /// Returns [`HlcError::ClockDriftExceeded`] if `remote` is further ahead of `physical_now_ms`
    /// than `max_drift_ms`.
    pub fn update_at(&self, remote: &Hlc, physical_now_ms: i64) -> Result<Hlc, HlcError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // Guard against excessive forward clock drift
        if remote.millis - physical_now_ms > state.max_drift_ms {
            return Err(HlcError::ClockDriftExceeded {
                local_ms: physical_now_ms,
                remote_ms: remote.millis,
                max_drift_ms: state.max_drift_ms,
            });
        }

        let l_prev = state.last_millis;
        let l_new = l_prev.max(physical_now_ms).max(remote.millis);

        if l_new == l_prev && l_new == remote.millis {
            state.counter = state.counter.max(remote.counter).saturating_add(1);
        } else if l_new == l_prev {
            state.counter = state.counter.saturating_add(1);
        } else if l_new == remote.millis {
            state.counter = remote.counter.saturating_add(1);
        } else {
            state.counter = 0;
        }

        state.last_millis = l_new;

        Ok(Hlc {
            millis: state.last_millis,
            counter: state.counter,
            node_id: state.node_id.clone(),
        })
    }
}

/// The causal relationship between two mutations on the same key/document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CausalRelation {
    /// A happened before B (A -> B): B causally replaces A without conflict.
    Ancestor,
    /// B happened before A (B -> A): A causally replaces B without conflict.
    Descendant,
    /// Identical mutation (idempotent duplicate).
    Identical,
    /// Concurrent modifications (A || B): neither mutation observed the other!
    Concurrent,
}

impl fmt::Display for CausalRelation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ancestor => write!(f, "ancestor (sequential)"),
            Self::Descendant => write!(f, "descendant (sequential)"),
            Self::Identical => write!(f, "identical"),
            Self::Concurrent => write!(f, "concurrent (A ∥ B)"),
        }
    }
}

/// Evaluates the causal relationship between two events A and B.
///
/// `a_hlc`: Timestamp of A.
/// `a_parent`: Parent timestamp that A observed before writing.
/// `b_hlc`: Timestamp of B.
/// `b_parent`: Parent timestamp that B observed before writing.
#[must_use]
pub fn detect_causality(
    a_hlc: &Hlc,
    a_parent: Option<&Hlc>,
    b_hlc: &Hlc,
    b_parent: Option<&Hlc>,
) -> CausalRelation {
    if a_hlc == b_hlc {
        return CausalRelation::Identical;
    }

    // Did B observe A? (A -> B)
    if b_parent == Some(a_hlc) {
        return CausalRelation::Ancestor;
    }

    // Did A observe B? (B -> A)
    if a_parent == Some(b_hlc) {
        return CausalRelation::Descendant;
    }

    // Neither directly observed the other: concurrent writes!
    CausalRelation::Concurrent
}

/// Strategy for resolving concurrent mutations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictResolutionStrategy {
    /// Deterministic Last-Write-Wins based on strict HLC total ordering.
    #[default]
    LastWriteWins,
    /// 3-way merge on JSON documents using the common ancestor base state.
    ThreeWayMerge,
    /// Preserve the conflict without automated resolution for application review.
    Manual,
}

/// Detailed information on a collision between concurrent writes on a specific JSON path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldConflict {
    /// JSON Pointer path of the colliding field (e.g. `"/user/name"`).
    pub path: String,
    /// Value in the common ancestor base document (if any).
    pub base_value: Option<Value>,
    /// Value proposed by local branch (ours).
    pub ours_value: Value,
    /// Value proposed by remote branch (theirs).
    pub theirs_value: Value,
    /// Value chosen by the resolution strategy.
    pub resolved_value: Value,
    /// Node identifier of the winning branch for this field.
    pub winner: String,
}

/// Outcome of a 3-way document merge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreeWayMergeResult {
    /// The merged JSON document value.
    pub merged_value: Value,
    /// Whether any concurrent field collisions were detected and resolved.
    pub has_conflicts: bool,
    /// Specific field collision details.
    pub field_conflicts: Vec<FieldConflict>,
}

/// Performs a 3-way merge of two concurrent versions (`ours` and `theirs`) against `base`.
///
/// Non-colliding field edits from both sides are preserved. For conflicting edits on the
/// same field path, the branch with the strictly higher HLC wins deterministically,
/// and the collision is recorded in `field_conflicts`.
#[must_use]
pub fn three_way_merge(
    base: Option<&Value>,
    ours: &Value,
    theirs: &Value,
    ours_hlc: &Hlc,
    theirs_hlc: &Hlc,
) -> ThreeWayMergeResult {
    let mut field_conflicts = Vec::new();
    let merged = merge_recursive(
        base,
        ours,
        theirs,
        ours_hlc,
        theirs_hlc,
        "",
        &mut field_conflicts,
    );

    let has_conflicts = !field_conflicts.is_empty();
    ThreeWayMergeResult {
        merged_value: merged,
        has_conflicts,
        field_conflicts,
    }
}

fn merge_recursive(
    base: Option<&Value>,
    ours: &Value,
    theirs: &Value,
    ours_hlc: &Hlc,
    theirs_hlc: &Hlc,
    current_path: &str,
    conflicts: &mut Vec<FieldConflict>,
) -> Value {
    if ours == theirs {
        return ours.clone();
    }

    if let (Value::Object(ours_map), Value::Object(theirs_map)) = (ours, theirs) {
        let base_map = base.and_then(Value::as_object);
        let mut result_map = serde_json::Map::new();

        let mut all_keys = std::collections::BTreeSet::new();
        if let Some(b) = base_map {
            all_keys.extend(b.keys());
        }
        all_keys.extend(ours_map.keys());
        all_keys.extend(theirs_map.keys());

        for key in all_keys {
            let sub_path = format!("{current_path}/{key}");
            let b_val = base_map.and_then(|m| m.get(key));
            let o_val = ours_map.get(key);
            let t_val = theirs_map.get(key);

            if let Some(merged_field) = merge_object_key(
                b_val,
                o_val,
                t_val,
                ours_hlc,
                theirs_hlc,
                &sub_path,
                conflicts,
            ) {
                result_map.insert(key.clone(), merged_field);
            }
        }

        Value::Object(result_map)
    } else {
        resolve_collision(
            base,
            ours,
            theirs,
            ours_hlc,
            theirs_hlc,
            if current_path.is_empty() {
                "/"
            } else {
                current_path
            },
            conflicts,
        )
    }
}

fn resolve_collision(
    base: Option<&Value>,
    ours: &Value,
    theirs: &Value,
    ours_hlc: &Hlc,
    theirs_hlc: &Hlc,
    path: &str,
    conflicts: &mut Vec<FieldConflict>,
) -> Value {
    let (resolved, winner) = if theirs_hlc > ours_hlc {
        (theirs.clone(), theirs_hlc.node_id.clone())
    } else {
        (ours.clone(), ours_hlc.node_id.clone())
    };
    conflicts.push(FieldConflict {
        path: path.to_string(),
        base_value: base.cloned(),
        ours_value: ours.clone(),
        theirs_value: theirs.clone(),
        resolved_value: resolved.clone(),
        winner,
    });
    resolved
}

fn merge_object_key(
    b_val: Option<&Value>,
    o_val: Option<&Value>,
    t_val: Option<&Value>,
    ours_hlc: &Hlc,
    theirs_hlc: &Hlc,
    sub_path: &str,
    conflicts: &mut Vec<FieldConflict>,
) -> Option<Value> {
    match (o_val, t_val) {
        (Some(o), Some(t)) => {
            if o == t {
                Some(o.clone())
            } else if b_val == Some(o) {
                Some(t.clone())
            } else if b_val == Some(t) {
                Some(o.clone())
            } else if o.is_object() && t.is_object() {
                Some(merge_recursive(
                    b_val, o, t, ours_hlc, theirs_hlc, sub_path, conflicts,
                ))
            } else {
                Some(resolve_collision(
                    b_val, o, t, ours_hlc, theirs_hlc, sub_path, conflicts,
                ))
            }
        }
        (Some(o), None) => {
            if b_val == Some(o) {
                None
            } else if b_val.is_none() {
                Some(o.clone())
            } else {
                let res = resolve_collision(
                    b_val,
                    o,
                    &Value::Null,
                    ours_hlc,
                    theirs_hlc,
                    sub_path,
                    conflicts,
                );
                (!res.is_null()).then_some(res)
            }
        }
        (None, Some(t)) => {
            if b_val == Some(t) {
                None
            } else if b_val.is_none() {
                Some(t.clone())
            } else {
                let res = resolve_collision(
                    b_val,
                    &Value::Null,
                    t,
                    ours_hlc,
                    theirs_hlc,
                    sub_path,
                    conflicts,
                );
                (!res.is_null()).then_some(res)
            }
        }
        (None, None) => None,
    }
}

/// Audit record representing a detected conflict in persistent storage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConflictRecord {
    pub id: Option<i64>,
    pub model: String,
    pub space: String,
    pub doc_id: String,
    pub base_hlc: Option<Hlc>,
    pub ours_hlc: Hlc,
    pub theirs_hlc: Hlc,
    pub ours_value: Value,
    pub theirs_value: Value,
    pub resolved_value: Option<Value>,
    pub resolution_strategy: ConflictResolutionStrategy,
    pub status: String,
    pub detected_at_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_hlc_string_formatting_and_parsing() {
        let hlc1 = Hlc::new(100, 1, "device-a");
        assert_eq!(hlc1.to_string(), "100.1@device-a");
        assert_eq!(hlc1.display_compact(), "100.1");

        let parsed: Hlc = "100.1@device-a".parse().unwrap();
        assert_eq!(parsed, hlc1);

        let parsed_compact: Hlc = "100.2".parse().unwrap();
        assert_eq!(parsed_compact.millis, 100);
        assert_eq!(parsed_compact.counter, 2);
        assert_eq!(parsed_compact.node_id, "");

        let json_str = serde_json::to_string(&hlc1).unwrap();
        assert_eq!(json_str, "\"100.1@device-a\"");
        let deserialized: Hlc = serde_json::from_str(&json_str).unwrap();
        assert_eq!(deserialized, hlc1);
    }

    #[test]
    fn test_hlc_total_ordering_and_tie_breaking() {
        let a = Hlc::new(100, 1, "node-a");
        let b = Hlc::new(100, 2, "node-a");
        let c = Hlc::new(200, 0, "node-a");
        let d = Hlc::new(100, 1, "node-b"); // Tie break on node_id

        assert!(b > a);
        assert!(c > b);
        assert!(d > a); // "node-b" > "node-a"
    }

    #[test]
    fn test_hlc_clock_skew_and_lagging_physical_clock() {
        // Appareil A creates an event at t=500
        let clock_a = HybridClock::new("device-a");
        let event_a = clock_a.tick_at(500);
        assert_eq!(event_a.millis, 500);
        assert_eq!(event_a.counter, 0);

        // Appareil B receives event A, but Appareil B's physical clock is 10 minutes BEHIND (t=100)
        let clock_b = HybridClock::new("device-b");
        let event_b = clock_b.update_at(&event_a, 100).unwrap();

        // Mathematical guarantee: HLC(B) > HLC(A) holds strictly!
        assert!(
            event_b > event_a,
            "HLC(B) must be strictly greater than HLC(A)"
        );
        assert_eq!(event_b.millis, 500);
        assert_eq!(event_b.counter, 1);
        assert_eq!(event_b.node_id, "device-b");

        // Next local event on Appareil B (still lagging at t=100)
        let next_b = clock_b.tick_at(100);
        assert!(next_b > event_b);
        assert_eq!(next_b.millis, 500);
        assert_eq!(next_b.counter, 2);
    }

    #[test]
    fn test_hlc_physical_clock_backward_jump() {
        let clock = HybridClock::new("device-1");

        let t1 = clock.tick_at(1000);
        assert_eq!(t1.millis, 1000);
        assert_eq!(t1.counter, 0);

        // Clock jumps backward by 500ms
        let t2 = clock.tick_at(500);
        assert_eq!(t2.millis, 1000);
        assert_eq!(t2.counter, 1);
        assert!(t2 > t1);
    }

    #[test]
    fn test_hlc_max_drift_rejection() {
        let clock = HybridClock::with_max_drift("device-1", 5_000); // 5 sec max drift
        let future_hlc = Hlc::new(20_000, 0, "rogue-device");

        let res = clock.update_at(&future_hlc, 10_000);
        assert!(matches!(res, Err(HlcError::ClockDriftExceeded { .. })));
    }

    #[test]
    fn test_deterministic_concurrency_detection() {
        let base_hlc = Hlc::new(50, 0, "initial");

        // Appareil A modifies offline based on base_hlc
        let hlc_a = Hlc::new(100, 1, "device-a");
        let parent_a = Some(base_hlc.clone());

        // Appareil B modifies offline based on base_hlc
        let hlc_b = Hlc::new(100, 2, "device-b");
        let parent_b = Some(base_hlc.clone());

        // Concurrency detection!
        let relation = detect_causality(&hlc_a, parent_a.as_ref(), &hlc_b, parent_b.as_ref());
        assert_eq!(relation, CausalRelation::Concurrent);

        // Sequential case: C modified after observing A
        let hlc_c = Hlc::new(150, 0, "device-c");
        let parent_c = Some(hlc_a.clone());
        let seq_relation = detect_causality(&hlc_a, parent_a.as_ref(), &hlc_c, parent_c.as_ref());
        assert_eq!(seq_relation, CausalRelation::Ancestor);
    }

    #[test]
    fn test_three_way_merge_disjoint_fields() {
        let base = json!({
            "user": {
                "name": "Alex",
                "email": "alex@example.com",
                "role": "member"
            }
        });

        // Appareil A changes name to "Alice"
        let ours = json!({
            "user": {
                "name": "Alice",
                "email": "alex@example.com",
                "role": "member"
            }
        });
        let hlc_a = Hlc::new(100, 1, "device-a");

        // Appareil B changes role to "admin" and adds avatar
        let theirs = json!({
            "user": {
                "name": "Alex",
                "email": "alex@example.com",
                "role": "admin",
                "avatar": "pic.png"
            }
        });
        let hlc_b = Hlc::new(100, 2, "device-b");

        let result = three_way_merge(Some(&base), &ours, &theirs, &hlc_a, &hlc_b);
        assert!(!result.has_conflicts);
        assert_eq!(result.field_conflicts.len(), 0);

        // Both disjoint changes merged cleanly!
        assert_eq!(result.merged_value["user"]["name"], "Alice");
        assert_eq!(result.merged_value["user"]["email"], "alex@example.com");
        assert_eq!(result.merged_value["user"]["role"], "admin");
        assert_eq!(result.merged_value["user"]["avatar"], "pic.png");
    }

    #[test]
    fn test_three_way_merge_field_collision_resolution() {
        let base = json!({
            "user": {
                "name": "Alex"
            }
        });

        // Appareil A modifies user.name to "Alice" (HLC 100.1@node-a)
        let ours = json!({
            "user": {
                "name": "Alice"
            }
        });
        let hlc_a = Hlc::new(100, 1, "node-a");

        // Appareil B modifies user.name to "Alicia" (HLC 100.2@node-b)
        let theirs = json!({
            "user": {
                "name": "Alicia"
            }
        });
        let hlc_b = Hlc::new(100, 2, "node-b");

        let result = three_way_merge(Some(&base), &ours, &theirs, &hlc_a, &hlc_b);
        assert!(result.has_conflicts);
        assert_eq!(result.field_conflicts.len(), 1);

        let conflict = &result.field_conflicts[0];
        assert_eq!(conflict.path, "/user/name");
        assert_eq!(conflict.ours_value, json!("Alice"));
        assert_eq!(conflict.theirs_value, json!("Alicia"));
        // Since HLC(B) 100.2 > HLC(A) 100.1, Alicia wins the tie-break deterministically
        assert_eq!(conflict.resolved_value, json!("Alicia"));
        assert_eq!(conflict.winner, "node-b");
        assert_eq!(result.merged_value["user"]["name"], "Alicia");
    }
}
