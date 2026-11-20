use ark_bn254::{Bn254, Fr};
use ark_crypto_primitives::snark::SNARK;
use ark_groth16::{Groth16, ProvingKey};
use ark_serialize::CanonicalSerialize;
use ark_std::rand::rngs::StdRng;
use ark_std::rand::SeedableRng;

use crate::circuits::composite::SoldeAgePredicateCircuit;
use crate::error::{Result, ZkError};
use crate::keys::default_solde_age_keys;
use crate::poseidon::{fr_to_hex, poseidon_hash_4, str_to_fr, u64_to_fr};
use crate::predicate::{ZkProofBundle, ZkStatement};

/// Zero-Knowledge Micro-Prover for LiliaDB.
///
/// Runs on embedded devices, mobile phones, or desktop servers to generate
/// succinct mathematical proofs (128 bytes) certifying conditions on private data.
#[derive(Debug, Default)]
pub struct ZkProver;

impl ZkProver {
    /// Proves that `balance >= min_balance` and `age >= min_age` for a document identified by `doc_id`.
    ///
    /// Computes the cryptographic Poseidon commitment $C = \text{Poseidon}(id, balance, age, salt)$
    /// and generates a compressed 128-byte Groth16 proof.
    pub fn prove_solde_age(
        doc_id: &str,
        balance: u64,
        age: u32,
        salt: Fr,
        min_balance: u64,
        min_age: u32,
        pk_override: Option<&ProvingKey<Bn254>>,
    ) -> Result<ZkProofBundle> {
        // Enforce basic pre-condition checks before entering circuit
        if balance < min_balance {
            return Err(ZkError::InvalidStatement(format!(
                "Private balance {balance} is strictly less than required threshold {min_balance}"
            )));
        }
        if age < min_age {
            return Err(ZkError::InvalidStatement(format!(
                "Private age {age} is strictly less than required threshold {min_age}"
            )));
        }

        let doc_id_fr = str_to_fr(doc_id);
        let balance_fr = u64_to_fr(balance);
        let age_fr = u64_to_fr(u64::from(age));

        // Compute native commitment anchor C
        let commitment = poseidon_hash_4(doc_id_fr, balance_fr, age_fr, salt);

        // Instantiate circuit with full private witness
        let circuit = SoldeAgePredicateCircuit::new(
            doc_id_fr,
            balance,
            age,
            salt,
            commitment,
            min_balance,
            min_age,
        );

        let default_keys = default_solde_age_keys();
        let pk = pk_override.unwrap_or(&default_keys.0);

        let mut rng = StdRng::from_seed([77u8; 32]);
        let proof = Groth16::<Bn254>::prove(pk, circuit, &mut rng)
            .map_err(|e| ZkError::Proving(format!("Proof generation failed: {e:?}")))?;

        // Serialize proof in compressed format (exactly 128 bytes for Groth16 BN254)
        let mut proof_bytes = Vec::new();
        proof
            .serialize_compressed(&mut proof_bytes)
            .map_err(|e| ZkError::Serialization(e.to_string()))?;

        let proof_size = proof_bytes.len();
        let proof_hex = format!("0x{}", hex::encode(proof_bytes));

        let public_inputs = vec![
            fr_to_hex(&commitment),
            format!("0x{:016x}", min_balance),
            format!("0x{:08x}", min_age),
        ];

        let verified_claims = vec![
            format!("balance >= {min_balance}"),
            format!("age >= {min_age}"),
        ];

        Ok(ZkProofBundle {
            proof_hex,
            public_inputs,
            commitment_hex: fr_to_hex(&commitment),
            circuit_tag: "solde_age_v1".to_string(),
            verified_claims,
            proof_size_bytes: proof_size,
        })
    }

    /// High-level prover supporting declarative `ZkStatement`.
    pub fn prove_statement(
        doc_id: &str,
        statement: &ZkStatement,
        witness_values: &std::collections::BTreeMap<String, u64>,
        salt: Fr,
        pk_override: Option<&ProvingKey<Bn254>>,
    ) -> Result<ZkProofBundle> {
        let mut min_balance = None;
        let mut min_age = None;

        for claim in &statement.claims {
            match claim {
                crate::predicate::ZkClaim::Gte { field, threshold } if field == "balance" => {
                    min_balance = Some(*threshold);
                }
                crate::predicate::ZkClaim::Gte { field, threshold } if field == "age" => {
                    min_age = Some(*threshold as u32);
                }
                _ => {
                    return Err(ZkError::InvalidStatement(format!(
                        "Unsupported claim in solde_age circuit: {:?}",
                        claim
                    )));
                }
            }
        }

        let balance = witness_values
            .get("balance")
            .copied()
            .ok_or_else(|| ZkError::MissingWitness("balance".into()))?;

        let age_u64 = witness_values
            .get("age")
            .copied()
            .ok_or_else(|| ZkError::MissingWitness("age".into()))?;
        let age = u32::try_from(age_u64).map_err(|_| {
            ZkError::InvalidStatement("age must fit in 32-bit unsigned integer".into())
        })?;

        let min_b = min_balance.unwrap_or(0);
        let min_a = min_age.unwrap_or(0);

        Self::prove_solde_age(doc_id, balance, age, salt, min_b, min_a, pk_override)
    }
}
