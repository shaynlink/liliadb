use ark_bn254::Fr;
use ark_r1cs_std::alloc::AllocVar;
use ark_r1cs_std::eq::EqGadget;
use ark_r1cs_std::fields::fp::FpVar;
use ark_relations::gr1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};

use super::range::enforce_gte;
use crate::poseidon::poseidon_hash_4_gadget;

/// Composite Zero-Knowledge Predicate Circuit:
/// Certifies:
/// 1. $w_{\text{balance}} \ge x_{\text{min\_balance}}$ (64-bit range constraint)
/// 2. $w_{\text{age}} \ge x_{\text{min\_age}}$ (32-bit range constraint)
/// 3. $\text{Poseidon}(w_{\text{doc\_id}}, w_{\text{balance}}, w_{\text{age}}, w_{\text{salt}}) == x_{\text{commitment}}$
///
/// Public Inputs (ordered):
/// - `public_inputs[0]`: Commitment $C$
/// - `public_inputs[1]`: Min balance threshold
/// - `public_inputs[2]`: Min age threshold
#[derive(Clone, Debug, Default)]
pub struct SoldeAgePredicateCircuit {
    // Private witness
    pub doc_id: Option<Fr>,
    pub balance: Option<u64>,
    pub age: Option<u32>,
    pub salt: Option<Fr>,

    // Public inputs
    pub commitment: Option<Fr>,
    pub min_balance: Option<u64>,
    pub min_age: Option<u32>,
}

impl SoldeAgePredicateCircuit {
    #[must_use]
    pub fn new(
        doc_id: Fr,
        balance: u64,
        age: u32,
        salt: Fr,
        commitment: Fr,
        min_balance: u64,
        min_age: u32,
    ) -> Self {
        Self {
            doc_id: Some(doc_id),
            balance: Some(balance),
            age: Some(age),
            salt: Some(salt),
            commitment: Some(commitment),
            min_balance: Some(min_balance),
            min_age: Some(min_age),
        }
    }

    /// Empty circuit for generating setup parameters (Proving Key and Verifying Key).
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }
}

impl ConstraintSynthesizer<Fr> for SoldeAgePredicateCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        // 1. Allocate Public Inputs
        let commitment_var = FpVar::new_input(cs.clone(), || {
            self.commitment.ok_or(SynthesisError::AssignmentMissing)
        })?;

        let min_balance_var = FpVar::new_input(cs.clone(), || {
            self.min_balance
                .map(Fr::from)
                .ok_or(SynthesisError::AssignmentMissing)
        })?;

        let min_age_var = FpVar::new_input(cs.clone(), || {
            self.min_age
                .map(Fr::from)
                .ok_or(SynthesisError::AssignmentMissing)
        })?;

        // 2. Allocate Private Witness Variables
        let doc_id_var = FpVar::new_witness(cs.clone(), || {
            self.doc_id.ok_or(SynthesisError::AssignmentMissing)
        })?;

        let balance_var = FpVar::new_witness(cs.clone(), || {
            self.balance
                .map(Fr::from)
                .ok_or(SynthesisError::AssignmentMissing)
        })?;

        let age_var = FpVar::new_witness(cs.clone(), || {
            self.age
                .map(Fr::from)
                .ok_or(SynthesisError::AssignmentMissing)
        })?;

        let salt_var = FpVar::new_witness(cs.clone(), || {
            self.salt.ok_or(SynthesisError::AssignmentMissing)
        })?;

        // 3. Enforce Cryptographic Commitment Anchor:
        // Poseidon(doc_id, balance, age, salt) == commitment
        let computed_commitment =
            poseidon_hash_4_gadget(&doc_id_var, &balance_var, &age_var, &salt_var)?;
        computed_commitment.enforce_equal(&commitment_var)?;

        // 4. Enforce Condition 1: balance >= min_balance (64-bit integer inequality)
        enforce_gte::<64>(
            &balance_var,
            &min_balance_var,
            self.balance,
            self.min_balance,
        )?;

        // 5. Enforce Condition 2: age >= min_age (32-bit integer inequality)
        enforce_gte::<32>(
            &age_var,
            &min_age_var,
            self.age.map(u64::from),
            self.min_age.map(u64::from),
        )?;

        Ok(())
    }
}
