//! 3-way merge and conflict resolution for concurrent mutations.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::timestamp::Hlc;

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
                b_val, o_val, t_val, ours_hlc, theirs_hlc, &sub_path, conflicts,
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
