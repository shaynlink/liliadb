use std::time::Instant;

use ark_bn254::{Bn254, Fr};
use ark_crypto_primitives::snark::SNARK;
use ark_groth16::{Groth16, Proof, VerifyingKey};
use ark_serialize::CanonicalDeserialize;

use crate::error::{Result, ZkError};
use crate::keys::default_solde_age_keys;
use crate::poseidon::hex_to_fr;
use crate::predicate::{ZkProofBundle, ZkVerificationReport};

/// High-Performance Zero-Knowledge Verifier.
///
/// Executes mathematical verification of 128-byte Groth16 proofs in < 1 ms
/// without ever seeing or requiring the private database values or witness.
#[derive(Debug, Default)]
pub struct ZkVerifier;

impl ZkVerifier {
    /// Mathematically verifies a proof bundle against a `VerifyingKey`.
    ///
    /// Runs 3 pairing evaluations on the BN254 elliptic curve and validates that
    /// the prover indeed knew secret values satisfying the committed public conditions.
    pub fn verify(
        bundle: &ZkProofBundle,
        vk_override: Option<&VerifyingKey<Bn254>>,
    ) -> Result<ZkVerificationReport> {
        let start = Instant::now();

        let vk = match vk_override {
            Some(vk) => vk,
            None => &default_solde_age_keys()?.1,
        };

        // 1. Deserialize 128-byte compressed proof
        let proof_bytes = bundle
            .proof_bytes()
            .map_err(|e| ZkError::Serialization(format!("Invalid proof hex: {e}")))?;

        if proof_bytes.len() != 128 {
            return Err(ZkError::Serialization(format!(
                "Invalid Groth16 BN254 proof length: expected 128 bytes, got {}",
                proof_bytes.len()
            )));
        }

        let proof = Proof::<Bn254>::deserialize_compressed(&proof_bytes[..])
            .map_err(|e| ZkError::Serialization(format!("Corrupt proof bytes: {e}")))?;

        // 2. Parse public inputs: [commitment, min_balance, min_age]
        if bundle.public_inputs.len() != 3 {
            return Err(ZkError::InvalidPublicInputs {
                expected: 3,
                got: bundle.public_inputs.len(),
            });
        }

        let commitment_fr = hex_to_fr(&bundle.public_inputs[0])?;

        let min_balance_u64 =
            u64::from_str_radix(bundle.public_inputs[1].trim().trim_start_matches("0x"), 16)
                .map_err(|e| ZkError::Serialization(format!("Invalid min_balance hex: {e}")))?;
        let min_balance_fr = Fr::from(min_balance_u64);

        let min_age_u64 =
            u64::from_str_radix(bundle.public_inputs[2].trim().trim_start_matches("0x"), 16)
                .map_err(|e| ZkError::Serialization(format!("Invalid min_age hex: {e}")))?;
        let min_age_fr = Fr::from(min_age_u64);

        let public_inputs_vec = vec![commitment_fr, min_balance_fr, min_age_fr];

        // 3. Process verifying key for efficient pairing
        let pvk = Groth16::<Bn254>::process_vk(vk)
            .map_err(|e| ZkError::Verification(format!("Failed to process VK: {e:?}")))?;

        // 4. Verify zk-SNARK proof using pairing check: e(A, B) == e(alpha, beta) * e(x * gamma, delta) * e(C, delta)
        let is_valid = Groth16::<Bn254>::verify_proof(&pvk, &proof, &public_inputs_vec)
            .map_err(|e| ZkError::Verification(format!("Verification pairing failed: {e:?}")))?;

        let elapsed = start.elapsed();
        let verification_time_ms = elapsed.as_secs_f64() * 1000.0;

        Ok(ZkVerificationReport {
            valid: is_valid,
            verification_time_ms,
            commitment: bundle.commitment_hex.clone(),
            certified_claims: bundle.verified_claims.clone(),
            zero_knowledge: true,
        })
    }
}
