#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::doc_markdown,
    clippy::cast_possible_truncation,
    clippy::uninlined_format_args,
    clippy::must_use_candidate,
    clippy::module_name_repetitions
)]

pub mod circuits;
pub mod document;
pub mod error;
pub mod keys;
pub mod poseidon;
pub mod predicate;
pub mod prover;
pub mod verifier;

pub use circuits::SoldeAgePredicateCircuit;
pub use document::DocumentWitness;
pub use error::{Result, ZkError};
pub use keys::{
    default_solde_age_keys, deserialize_pk, deserialize_vk, generate_circuit_keys, hex_to_vk,
    serialize_pk, serialize_vk, vk_to_hex,
};

pub use ark_bn254;
pub use ark_groth16;

pub type ProvingKey = ark_groth16::ProvingKey<ark_bn254::Bn254>;
pub type VerifyingKey = ark_groth16::VerifyingKey<ark_bn254::Bn254>;
pub use poseidon::{
    bytes_to_fr, fr_to_hex, hex_to_fr, poseidon_hash_2, poseidon_hash_4, str_to_fr, u64_to_fr,
};
pub use predicate::{
    ZkClaim, ZkProofBundle, ZkStatement, ZkStatementBuilder, ZkVerificationReport,
};
pub use prover::ZkProver;
pub use verifier::ZkVerifier;
