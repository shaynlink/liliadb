use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Execution and security context representing the authenticated caller.
///
/// Holds identity claims (e.g. `tenant_id`, `user_id`, `org_id`) and assigned roles
/// used to enforce Row-Level Security (RLS) policies and dynamic data masking.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityContext {
    pub claims: HashMap<String, String>,
    pub roles: HashSet<String>,
}

impl SecurityContext {
    /// Creates an empty security context.
    #[must_use]
    pub fn new() -> Self {
        Self {
            claims: HashMap::new(),
            roles: HashSet::new(),
        }
    }

    /// Convenience constructor initializing context with a single claim (e.g. `tenant_id`).
    #[must_use]
    pub fn from_pair(key: impl Into<String>, value: impl Into<String>) -> Self {
        let mut ctx = Self::new();
        ctx.claims.insert(key.into(), value.into());
        ctx
    }

    /// Builder method to attach a claim.
    #[must_use]
    pub fn with_claim(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.claims.insert(key.into(), value.into());
        self
    }

    /// Builder method to attach a role.
    #[must_use]
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        let r = role.into();
        self.claims
            .entry("role".to_string())
            .or_insert_with(|| r.clone());
        self.roles.insert(r);
        self
    }

    /// Retrieve a claim value by key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        if let Some(val) = self.claims.get(key) {
            return Some(val.as_str());
        }
        if key == "role" {
            return self.roles.iter().next().map(String::as_str);
        }
        None
    }

    /// Check if the context holds a given role.
    #[must_use]
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.contains(role)
    }
}
