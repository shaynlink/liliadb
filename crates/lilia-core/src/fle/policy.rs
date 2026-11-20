//! Field-Level Encryption policies and declarative rules.

use serde::{Deserialize, Serialize};

/// Cipher algorithm used for field encryption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldCipherAlgorithm {
    /// AES-256-GCM (Probabilistic / Randomized AEAD).
    /// Best for maximum confidentiality (e.g. medical data, credit cards).
    #[serde(alias = "gcm")]
    RandomizedGcm,
    /// AES-256-SIV (Deterministic AEAD, RFC 5297).
    /// Enables direct equality searches (`WHERE field = :val`) and index lookups.
    #[serde(alias = "siv")]
    DeterministicSiv,
    /// Paillier Additive Homomorphic Encryption.
    /// Enables linear operations (SUM, scalar multiplication) on encrypted data in memory without decryption.
    #[serde(alias = "paillier", alias = "homomorphic")]
    HomomorphicPaillier,
}

impl FieldCipherAlgorithm {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RandomizedGcm => "gcm",
            Self::DeterministicSiv => "siv",
            Self::HomomorphicPaillier => "paillier",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "gcm" | "randomized" | "randomized_gcm" => Some(Self::RandomizedGcm),
            "siv" | "deterministic" | "deterministic_siv" => Some(Self::DeterministicSiv),
            "paillier" | "homomorphic" | "homomorphic_paillier" => Some(Self::HomomorphicPaillier),
            _ => None,
        }
    }
}

/// A declarative encryption rule for a specific field within a collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldEncryptionRule {
    /// Field name or JSON path (e.g. "email", "$.`medical_record`", "/ssn").
    pub path: String,
    /// Algorithm (Randomized GCM vs Deterministic SIV).
    pub algorithm: FieldCipherAlgorithm,
    /// Compartment / Key ID (e.g. "vault:team", "vault:private").
    pub key_id: String,
    /// Whether to bind the field path as AEAD Associated Data (anti-swapping protection).
    #[serde(default = "default_true")]
    pub bind_path_as_ad: bool,
}

const fn default_true() -> bool {
    true
}

impl FieldEncryptionRule {
    #[must_use]
    pub fn new(
        path: impl Into<String>,
        algorithm: FieldCipherAlgorithm,
        key_id: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            algorithm,
            key_id: key_id.into(),
            bind_path_as_ad: true,
        }
    }

    /// Checks if a candidate path matches this rule (supports bare name, `$.path`, or `/path`).
    #[must_use]
    pub fn matches_path(&self, candidate: &str) -> bool {
        let norm_rule = self.path.trim_start_matches("$.").trim_start_matches('/');
        let norm_cand = candidate.trim_start_matches("$.").trim_start_matches('/');
        norm_rule == norm_cand
    }
}

/// Collection-level Field Encryption Policy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlePolicy {
    pub rules: Vec<FieldEncryptionRule>,
}

impl FlePolicy {
    #[must_use]
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    #[must_use]
    pub fn with_rule(mut self, rule: FieldEncryptionRule) -> Self {
        self.rules.push(rule);
        self
    }

    #[must_use]
    pub fn field(
        mut self,
        path: impl Into<String>,
        algo: FieldCipherAlgorithm,
        key_id: impl Into<String>,
    ) -> Self {
        self.rules
            .push(FieldEncryptionRule::new(path, algo, key_id));
        self
    }

    #[must_use]
    pub fn find_rule(&self, path: &str) -> Option<&FieldEncryptionRule> {
        self.rules.iter().find(|r| r.matches_path(path))
    }

    #[must_use]
    pub fn is_field_encrypted(&self, path: &str) -> bool {
        self.find_rule(path).is_some()
    }

    #[must_use]
    pub fn deterministic_rule_for(&self, path: &str) -> Option<&FieldEncryptionRule> {
        self.rules
            .iter()
            .find(|r| r.matches_path(path) && r.algorithm == FieldCipherAlgorithm::DeterministicSiv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_policy_matching() {
        let policy = FlePolicy::new()
            .field(
                "email",
                FieldCipherAlgorithm::DeterministicSiv,
                "vault:team",
            )
            .field(
                "$.medical_notes",
                FieldCipherAlgorithm::RandomizedGcm,
                "vault:private",
            );

        assert!(policy.is_field_encrypted("email"));
        assert!(policy.is_field_encrypted("$.email"));
        assert!(policy.is_field_encrypted("/email"));

        assert!(policy.is_field_encrypted("medical_notes"));
        assert!(policy.is_field_encrypted("$.medical_notes"));

        assert!(!policy.is_field_encrypted("created_at"));

        let siv_rule = policy.deterministic_rule_for("email");
        assert!(siv_rule.is_some());
        assert_eq!(siv_rule.unwrap().key_id, "vault:team");

        assert!(policy.deterministic_rule_for("medical_notes").is_none());
    }
}
