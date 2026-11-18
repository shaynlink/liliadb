use serde::{Deserialize, Serialize};

/// Target operation for a Row-Level Security (RLS) policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PolicyAction {
    /// Applies to all operations (read and write).
    #[default]
    All,
    /// Applies to SELECT / read operations.
    Select,
    /// Applies to INSERT operations.
    Insert,
    /// Applies to UPDATE operations.
    Update,
    /// Applies to DELETE operations.
    Delete,
}

impl PolicyAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Select => "select",
            Self::Insert => "insert",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "all" | "*" => Some(Self::All),
            "select" | "read" => Some(Self::Select),
            "insert" => Some(Self::Insert),
            "update" => Some(Self::Update),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }

    /// Whether this policy applies to read operations.
    #[must_use]
    pub const fn applies_to_read(self) -> bool {
        matches!(self, Self::All | Self::Select)
    }

    /// Whether this policy applies to write operations (insert/update/delete).
    #[must_use]
    pub const fn applies_to_write(self) -> bool {
        matches!(self, Self::All | Self::Insert | Self::Update | Self::Delete)
    }
}

/// A Row-Level Security (RLS) policy definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    /// Target table, collection, or space name.
    pub table: String,
    /// Boolean condition expression (e.g. `json_extract(data, '$.tenant_id') == current_context('tenant_id')`).
    pub condition: String,
    /// Applicable database operation (defaults to `PolicyAction::All`).
    #[serde(default)]
    pub action: PolicyAction,
}

impl Policy {
    /// Create a new policy with default `All` action.
    #[must_use]
    pub fn new(table: impl Into<String>, condition: impl Into<String>) -> Self {
        Self {
            table: table.into(),
            condition: condition.into(),
            action: PolicyAction::All,
        }
    }

    /// Configure explicit action.
    #[must_use]
    pub fn with_action(mut self, action: PolicyAction) -> Self {
        self.action = action;
        self
    }
}

/// A persisted or named security policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedPolicy {
    pub name: String,
    pub policy: Policy,
    pub created_at_ms: u64,
}
