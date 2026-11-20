use serde::{Deserialize, Serialize};

/// An individual claim or condition to prove in Zero-Knowledge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ZkClaim {
    /// Field value must be >= threshold.
    Gte { field: String, threshold: u64 },
    /// Field value must be <= threshold.
    Lte { field: String, threshold: u64 },
    /// Field value must be between min and max inclusive.
    Between { field: String, min: u64, max: u64 },
    /// Field value must equal an exact target.
    Eq { field: String, value: u64 },
}

impl ZkClaim {
    #[must_use]
    pub fn gte(field: impl Into<String>, threshold: u64) -> Self {
        Self::Gte {
            field: field.into(),
            threshold,
        }
    }

    #[must_use]
    pub fn lte(field: impl Into<String>, threshold: u64) -> Self {
        Self::Lte {
            field: field.into(),
            threshold,
        }
    }

    #[must_use]
    pub fn between(field: impl Into<String>, min: u64, max: u64) -> Self {
        Self::Between {
            field: field.into(),
            min,
            max,
        }
    }

    #[must_use]
    pub fn to_claim_string(&self) -> String {
        match self {
            Self::Gte { field, threshold } => format!("{field} >= {threshold}"),
            Self::Lte { field, threshold } => format!("{field} <= {threshold}"),
            Self::Between { field, min, max } => format!("{min} <= {field} <= {max}"),
            Self::Eq { field, value } => format!("{field} == {value}"),
        }
    }
}

/// A composite statement specifying one or more claims to prove on a document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ZkStatement {
    pub claims: Vec<ZkClaim>,
}

impl ZkStatement {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn builder() -> ZkStatementBuilder {
        ZkStatementBuilder::default()
    }

    #[must_use]
    pub fn claims_summary(&self) -> Vec<String> {
        self.claims.iter().map(ZkClaim::to_claim_string).collect()
    }
}

#[derive(Debug, Default)]
pub struct ZkStatementBuilder {
    claims: Vec<ZkClaim>,
}

impl ZkStatementBuilder {
    #[must_use]
    pub fn field_gte(mut self, field: impl Into<String>, threshold: u64) -> Self {
        self.claims.push(ZkClaim::gte(field, threshold));
        self
    }

    #[must_use]
    pub fn field_lte(mut self, field: impl Into<String>, threshold: u64) -> Self {
        self.claims.push(ZkClaim::lte(field, threshold));
        self
    }

    #[must_use]
    pub fn field_between(mut self, field: impl Into<String>, min: u64, max: u64) -> Self {
        self.claims.push(ZkClaim::between(field, min, max));
        self
    }

    #[must_use]
    pub fn build(self) -> ZkStatement {
        ZkStatement {
            claims: self.claims,
        }
    }
}

/// Self-contained Zero-Knowledge Proof Bundle.
///
/// Contains the 128-byte compressed Groth16 proof, public inputs,
/// cryptographic commitment, and metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZkProofBundle {
    /// Compressed 128-byte Groth16 proof on BN254 encoded in hexadecimal.
    pub proof_hex: String,
    /// Public inputs formatted in hexadecimal (commitment, thresholds).
    pub public_inputs: Vec<String>,
    /// Cryptographic commitment anchor C = Poseidon(id, val1, val2, salt).
    pub commitment_hex: String,
    /// Identifier of the circuit template used.
    pub circuit_tag: String,
    /// Human-readable list of verified claims.
    pub verified_claims: Vec<String>,
    /// Size of the raw binary proof in bytes (always 128 for Groth16 BN254).
    pub proof_size_bytes: usize,
}

impl ZkProofBundle {
    /// Raw binary proof bytes decoded from `proof_hex`.
    pub fn proof_bytes(&self) -> Result<Vec<u8>, hex::FromHexError> {
        let clean = self.proof_hex.trim_start_matches("0x");
        hex::decode(clean)
    }
}

/// Report produced by the third-party verifier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZkVerificationReport {
    /// Whether the zk-SNARK proof is cryptographically valid.
    pub valid: bool,
    /// Verification duration in milliseconds (sub-millisecond on standard CPU).
    pub verification_time_ms: f64,
    /// The public commitment anchor verified by the circuit.
    pub commitment: String,
    /// Claims certified by this proof.
    pub certified_claims: Vec<String>,
    /// Confirmation that no private data was disclosed to the verifier.
    pub zero_knowledge: bool,
}
