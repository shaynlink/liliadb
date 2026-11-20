use ark_bn254::Fr;
use ark_ff::{AdditiveGroup, Field};
use ark_r1cs_std::alloc::AllocVar;
use ark_r1cs_std::boolean::Boolean;
use ark_r1cs_std::eq::EqGadget;
use ark_r1cs_std::fields::fp::FpVar;
use ark_r1cs_std::fields::FieldVar;
use ark_r1cs_std::GR1CSVar;
use ark_relations::gr1cs::SynthesisError;

/// Enforces that `val >= threshold` over unsigned `BITS`-bit integers in the scalar field.
///
/// Computes $\Delta = val - threshold$.
/// If $val \ge threshold$, then $\Delta \in [0, 2^{BITS}-1]$.
/// If $val < threshold$, then in $\mathbb{F}_r$, $\Delta = r - (threshold - val) > 2^{BITS}$.
/// Therefore, constraining that $\Delta$ can be decomposed into `BITS` boolean bits
/// strictly guarantees $val \ge threshold$ in $\mathbb{Z}$.
pub fn enforce_gte<const BITS: usize>(
    val_var: &FpVar<Fr>,
    threshold_var: &FpVar<Fr>,
    val_native: Option<u64>,
    threshold_native: Option<u64>,
) -> Result<(), SynthesisError> {
    assert!(
        BITS <= 64,
        "BITS must be <= 64 for unsigned integer comparisons"
    );
    let cs = val_var.cs();

    let diff_var = val_var - threshold_var;

    // Witness bit values
    let delta_native = match (val_native, threshold_native) {
        (Some(v), Some(t)) if v >= t => Some(v - t),
        _ => None,
    };

    let mut bits = Vec::with_capacity(BITS);
    for i in 0..BITS {
        let bit_val = delta_native.map(|d| ((d >> i) & 1) == 1);
        let b = Boolean::new_witness(cs.clone(), || {
            bit_val.ok_or(SynthesisError::AssignmentMissing)
        })?;
        bits.push(b);
    }

    // Reconstruct delta from bits: sum = b_0 * 2^0 + b_1 * 2^1 + ...
    let mut reconstructed = FpVar::zero();
    let mut coeff = Fr::ONE;
    for b in &bits {
        let b_fp = FpVar::from(b.clone());
        reconstructed += b_fp * coeff;
        coeff = coeff.double();
    }

    reconstructed.enforce_equal(&diff_var)?;
    Ok(())
}

/// Enforces that `min_val <= val <= max_val`.
pub fn enforce_between<const BITS: usize>(
    val_var: &FpVar<Fr>,
    min_var: &FpVar<Fr>,
    max_var: &FpVar<Fr>,
    val_native: Option<u64>,
    min_native: Option<u64>,
    max_native: Option<u64>,
) -> Result<(), SynthesisError> {
    enforce_gte::<BITS>(val_var, min_var, val_native, min_native)?;
    enforce_gte::<BITS>(max_var, val_var, max_native, val_native)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_relations::gr1cs::ConstraintSystem;

    #[test]
    fn test_range_proof_satisfaction() {
        let cs = ConstraintSystem::<Fr>::new_ref();

        // 2450 >= 1000
        let val = 2450u64;
        let thresh = 1000u64;

        let val_var = FpVar::new_witness(cs.clone(), || Ok(Fr::from(val))).unwrap();
        let thresh_var = FpVar::new_input(cs.clone(), || Ok(Fr::from(thresh))).unwrap();

        enforce_gte::<64>(&val_var, &thresh_var, Some(val), Some(thresh)).unwrap();
        assert!(cs.is_satisfied().unwrap());

        // 29 >= 18
        let age = 29u64;
        let min_age = 18u64;
        let age_var = FpVar::new_witness(cs.clone(), || Ok(Fr::from(age))).unwrap();
        let min_age_var = FpVar::new_input(cs.clone(), || Ok(Fr::from(min_age))).unwrap();

        enforce_gte::<32>(&age_var, &min_age_var, Some(age), Some(min_age)).unwrap();
        assert!(cs.is_satisfied().unwrap());
    }

    #[test]
    fn test_range_proof_failure_when_invalid() {
        let cs = ConstraintSystem::<Fr>::new_ref();

        // 999 < 1000 (invalid witness)
        let val = 999u64;
        let thresh = 1000u64;

        let val_var = FpVar::new_witness(cs.clone(), || Ok(Fr::from(val))).unwrap();
        let thresh_var = FpVar::new_input(cs.clone(), || Ok(Fr::from(thresh))).unwrap();

        // Native delta would underflow/be none
        let res = enforce_gte::<64>(&val_var, &thresh_var, Some(val), Some(thresh));
        // Assignment missing or not satisfied
        assert!(res.is_err() || !cs.is_satisfied().unwrap());
    }
}
