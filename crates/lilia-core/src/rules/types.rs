use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Complex Event Processing (CEP) and reactive in-transaction rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    /// Declarative predicate expression evaluated in-transaction (e.g. `"events.heart_rate > 140 AND events.motion == 'still'"`).
    pub when: String,
    /// Optional sliding window temporal specification (CEP).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<SlidingWindow>,
    /// Reactive action to execute atomically within the same transaction commit.
    pub then: Action,
    /// Explicit target collection / space or namespace (optional; inferred from `when` if not provided).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

impl Rule {
    /// Create a new simple reactive rule without a temporal window.
    #[must_use]
    pub fn new(when: impl Into<String>, then: Action) -> Self {
        Self {
            when: when.into(),
            window: None,
            then,
            target: None,
        }
    }

    /// Fluent builder starting with the `when` predicate.
    #[must_use]
    pub fn when(when: impl Into<String>) -> RuleBuilder {
        RuleBuilder::new(when)
    }

    /// Attach an explicit target collection to this rule.
    #[must_use]
    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    /// Attach a sliding window to this rule.
    #[must_use]
    pub fn with_window(mut self, window: SlidingWindow) -> Self {
        self.window = Some(window);
        self
    }
}

/// Temporal sliding window configuration for Complex Event Processing (CEP).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlidingWindow {
    /// Window duration in milliseconds.
    pub duration_ms: u64,
    /// Partition key expression (e.g. `"$key"`, `"user_id"`, `"events.device_id"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_by: Option<String>,
    /// Aggregate condition for the window to trigger.
    pub condition: WindowCondition,
    /// Trigger mode: edge-triggered (`OnThresholdCrossed`, default) or level-triggered (`Always`).
    #[serde(default)]
    pub trigger_mode: TriggerMode,
}

impl SlidingWindow {
    /// Create a new sliding window with a given duration and condition.
    #[must_use]
    pub fn new(duration: Duration, condition: WindowCondition) -> Self {
        Self {
            duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
            partition_by: None,
            condition,
            trigger_mode: TriggerMode::OnThresholdCrossed,
        }
    }

    /// Partition sliding window events by a specific key or field expression (e.g. `"$key"`, `"user_id"`).
    #[must_use]
    pub fn partition_by(mut self, partition_by: impl Into<String>) -> Self {
        self.partition_by = Some(partition_by.into());
        self
    }

    /// Set the trigger mode for this sliding window.
    #[must_use]
    pub const fn trigger_mode(mut self, mode: TriggerMode) -> Self {
        self.trigger_mode = mode;
        self
    }
}

/// Triggering mode for sliding window aggregate condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TriggerMode {
    /// Edge-triggered: fires only when crossing the threshold (e.g. going from 5 to 6).
    #[default]
    OnThresholdCrossed,
    /// Level-triggered: fires on every incoming event that satisfies the condition in the window.
    Always,
}

/// Aggregate conditions supported by temporal sliding windows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowCondition {
    /// Event count strictly greater than threshold (`COUNT(*) > N`).
    CountGreaterThan(u64),
    /// Event count greater than or equal to threshold (`COUNT(*) >= N`).
    CountGreaterOrEqual(u64),
    /// Sum of numeric field strictly greater than threshold (`SUM(field) > N`).
    SumGreaterThan { field: String, threshold: f64 },
    /// Average of numeric field strictly greater than threshold (`AVG(field) > N`).
    AvgGreaterThan { field: String, threshold: f64 },
    /// Maximum of numeric field strictly greater than threshold (`MAX(field) > N`).
    MaxGreaterThan { field: String, threshold: f64 },
    /// Minimum of numeric field strictly less than threshold (`MIN(field) < N`).
    MinLessThan { field: String, threshold: f64 },
}

/// Reactive action executed in-transaction when a rule fires.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Insert a JSON document into the specified collection.
    Insert { collection: String, document: Value },
    /// Update/replace a JSON document in the specified collection.
    Update {
        collection: String,
        id: String,
        document: Value,
    },
    /// Delete a JSON document from the specified collection.
    Delete { collection: String, id: String },
    /// Set a key-value pair in the specified namespace.
    KvSet {
        namespace: String,
        key: String,
        #[serde(with = "serde_bytes")]
        value: Vec<u8>,
    },
    /// Delete a key from the specified namespace.
    KvDelete { namespace: String, key: String },
    /// Execute an arbitrary parameterized SQL statement.
    ExecuteSql { sql: String, params: Vec<Value> },
    /// Broadcast a notification on `LiliaDB`'s asynchronous event channel.
    EmitEvent { topic: String, payload: Value },
}

impl Action {
    /// Create an `Insert` action.
    #[must_use]
    pub fn insert(collection: impl Into<String>, document: impl Into<Value>) -> Self {
        Self::Insert {
            collection: collection.into(),
            document: document.into(),
        }
    }

    /// Create an `Update` action.
    #[must_use]
    pub fn update(
        collection: impl Into<String>,
        id: impl Into<String>,
        document: impl Into<Value>,
    ) -> Self {
        Self::Update {
            collection: collection.into(),
            id: id.into(),
            document: document.into(),
        }
    }

    /// Create a `Delete` action.
    #[must_use]
    pub fn delete(collection: impl Into<String>, id: impl Into<String>) -> Self {
        Self::Delete {
            collection: collection.into(),
            id: id.into(),
        }
    }

    /// Create a `KvSet` action.
    #[must_use]
    pub fn kv_set(
        namespace: impl Into<String>,
        key: impl Into<String>,
        value: impl Into<Vec<u8>>,
    ) -> Self {
        Self::KvSet {
            namespace: namespace.into(),
            key: key.into(),
            value: value.into(),
        }
    }

    /// Create a `KvDelete` action.
    #[must_use]
    pub fn kv_delete(namespace: impl Into<String>, key: impl Into<String>) -> Self {
        Self::KvDelete {
            namespace: namespace.into(),
            key: key.into(),
        }
    }
}

/// Fluent builder for declarative reactive rules.
#[derive(Debug, Clone)]
pub struct RuleBuilder {
    when: String,
    window: Option<SlidingWindow>,
    target: Option<String>,
    then: Option<Action>,
}

impl RuleBuilder {
    #[must_use]
    pub fn new(when: impl Into<String>) -> Self {
        Self {
            when: when.into(),
            window: None,
            target: None,
            then: None,
        }
    }

    #[must_use]
    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    #[must_use]
    pub fn sliding_window(mut self, duration: Duration, partition_by: impl Into<String>) -> Self {
        let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        self.window = Some(SlidingWindow {
            duration_ms,
            partition_by: Some(partition_by.into()),
            condition: WindowCondition::CountGreaterThan(0),
            trigger_mode: TriggerMode::OnThresholdCrossed,
        });
        self
    }

    #[must_use]
    pub fn having_count_gt(mut self, count: u64) -> Self {
        if let Some(ref mut win) = self.window {
            win.condition = WindowCondition::CountGreaterThan(count);
        } else {
            self.window = Some(SlidingWindow {
                duration_ms: 60_000,
                partition_by: Some("$key".to_string()),
                condition: WindowCondition::CountGreaterThan(count),
                trigger_mode: TriggerMode::OnThresholdCrossed,
            });
        }
        self
    }

    #[must_use]
    pub fn having_count_gte(mut self, count: u64) -> Self {
        if let Some(ref mut win) = self.window {
            win.condition = WindowCondition::CountGreaterOrEqual(count);
        } else {
            self.window = Some(SlidingWindow {
                duration_ms: 60_000,
                partition_by: Some("$key".to_string()),
                condition: WindowCondition::CountGreaterOrEqual(count),
                trigger_mode: TriggerMode::OnThresholdCrossed,
            });
        }
        self
    }

    #[must_use]
    pub fn having_sum_gt(mut self, field: impl Into<String>, threshold: f64) -> Self {
        if let Some(ref mut win) = self.window {
            win.condition = WindowCondition::SumGreaterThan {
                field: field.into(),
                threshold,
            };
        }
        self
    }

    #[must_use]
    pub fn trigger_mode(mut self, mode: TriggerMode) -> Self {
        if let Some(ref mut win) = self.window {
            win.trigger_mode = mode;
        }
        self
    }

    #[must_use]
    pub fn then(mut self, action: Action) -> Self {
        self.then = Some(action);
        self
    }

    #[must_use]
    pub fn then_insert(self, collection: impl Into<String>, document: impl Into<Value>) -> Self {
        self.then(Action::insert(collection, document))
    }

    #[must_use]
    pub fn then_kv_set(
        self,
        namespace: impl Into<String>,
        key: impl Into<String>,
        value: impl Into<Vec<u8>>,
    ) -> Self {
        self.then(Action::kv_set(namespace, key, value))
    }

    /// Finalize and build the [`Rule`].
    ///
    /// # Panics
    /// Panics if `then` action was not supplied.
    #[must_use]
    pub fn build(self) -> Rule {
        Rule {
            when: self.when,
            window: self.window,
            then: self
                .then
                .expect("rule must have an action configured via .then(...)"),
            target: self.target,
        }
    }
}

impl From<RuleBuilder> for Rule {
    fn from(builder: RuleBuilder) -> Self {
        builder.build()
    }
}
