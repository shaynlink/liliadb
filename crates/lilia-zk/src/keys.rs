use std::sync::OnceLock;

use ark_bn254::{Bn254, Fr};
use ark_crypto_primitives::snark::SNARK;
use ark_groth16::{Groth16, ProvingKey, VerifyingKey};
use ark_relations::gr1cs::ConstraintSynthesizer;
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use ark_std::rand::rngs::StdRng;
use ark_std::rand::SeedableRng;

use crate::circuits::composite::SoldeAgePredicateCircuit;
use crate::error::{Result, ZkError};

static DEFAULT_SOLDE_AGE_KEYS: OnceLock<(ProvingKey<Bn254>, VerifyingKey<Bn254>)> = OnceLock::new();

/// Generates a new Groth16 ProvingKey and VerifyingKey pair for a given circuit.
pub fn generate_circuit_keys<C: ConstraintSynthesizer<Fr>>(
    circuit: C,
) -> Result<(ProvingKey<Bn254>, VerifyingKey<Bn254>)> {
    let mut rng = StdRng::from_seed([42u8; 32]);
    Groth16::<Bn254>::circuit_specific_setup(circuit, &mut rng)
        .map_err(|e| ZkError::Proving(format!("Key generation failed: {e:?}")))
}

/// Returns the cached, deterministically initialized default keys for the
/// `solde_age` (balance >= min_balance && age >= min_age with Poseidon commitment) circuit.
///
/// This provides instant zero-configuration proving and verification without requiring
/// a manual setup ceremony step.
pub fn default_solde_age_keys() -> &'static (ProvingKey<Bn254>, VerifyingKey<Bn254>) {
    DEFAULT_SOLDE_AGE_KEYS.get_or_init(|| {
        let circuit = SoldeAgePredicateCircuit::empty();
        generate_circuit_keys(circuit).expect("Failed to initialize default solde_age keys")
    })
}

/// Serializes a ProvingKey to compressed binary format.
pub fn serialize_pk(pk: &ProvingKey<Bn254>) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pk.serialize_compressed(&mut bytes)
        .map_err(|e| ZkError::Serialization(e.to_string()))?;
    Ok(bytes)
}

/// Deserializes a ProvingKey from compressed binary format.
pub fn deserialize_pk(bytes: &[u8]) -> Result<ProvingKey<Bn254>> {
    ProvingKey::<Bn254>::deserialize_compressed(bytes)
        .map_err(|e| ZkError::Serialization(e.to_string()))
}

/// Serializes a VerifyingKey to compressed binary format (~384 bytes).
pub fn serialize_vk(vk: &VerifyingKey<Bn254>) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    vk.serialize_compressed(&mut bytes)
        .map_err(|e| ZkError::Serialization(e.to_string()))?;
    Ok(bytes)
}

/// Deserializes a VerifyingKey from compressed binary format.
pub fn deserialize_vk(bytes: &[u8]) -> Result<VerifyingKey<Bn254>> {
    VerifyingKey::<Bn254>::deserialize_compressed(bytes)
        .map_err(|e| ZkError::Serialization(e.to_string()))
}

/// Serializes a VerifyingKey to a hex string.
pub fn vk_to_hex(vk: &VerifyingKey<Bn254>) -> Result<String> {
    let bytes = serialize_vk(vk)?;
    Ok(format!("0x{}", hex::encode(bytes)))
}

/// Deserializes a VerifyingKey from a hex string.
pub fn hex_to_vk(hex_str: &str) -> Result<VerifyingKey<Bn254>> {
    let clean = hex_str.trim().trim_start_matches("0x");
    let bytes = hex::decode(clean)?;
    deserialize_vk(&bytes)
}
