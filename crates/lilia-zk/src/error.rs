use thiserror::Error;

#[derive(Debug, Error)]
pub enum ZkError {
    #[error("Circuit synthesis error: {0}")]
    Synthesis(String),

    #[error("Groth16 proving error: {0}")]
    Proving(String),

    #[error("Groth16 verification error: {0}")]
    Verification(String),

    #[error("Serialization / Deserialization error: {0}")]
    Serialization(String),

    #[error("Invalid public inputs: expected {expected}, got {got}")]
    InvalidPublicInputs { expected: usize, got: usize },

    #[error("Invalid statement: {0}")]
    InvalidStatement(String),

    #[error("Missing private witness field: {0}")]
    MissingWitness(String),

    #[error("Cryptographic anchor commitment mismatch")]
    CommitmentMismatch,

    #[error("Hex decode error: {0}")]
    Hex(#[from] hex::FromHexError),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, ZkError>;
