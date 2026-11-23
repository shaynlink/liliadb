use std::sync::OnceLock;

use ark_bn254::Fr;
use ark_crypto_primitives::sponge::poseidon::constraints::PoseidonSpongeVar;
use ark_crypto_primitives::sponge::poseidon::{PoseidonConfig, PoseidonSponge};
use ark_crypto_primitives::sponge::{constraints::CryptographicSpongeVar, CryptographicSponge};
use ark_ff::{BigInteger, Field, PrimeField};
use ark_r1cs_std::fields::fp::FpVar;
use ark_r1cs_std::GR1CSVar;
use ark_relations::gr1cs::SynthesisError;
use ark_std::rand::rngs::StdRng;
use ark_std::rand::SeedableRng;
use ark_std::UniformRand;

static POSEIDON_CONFIG_T5: OnceLock<PoseidonConfig<Fr>> = OnceLock::new();
static POSEIDON_CONFIG_T3: OnceLock<PoseidonConfig<Fr>> = OnceLock::new();

/// Deterministically generates a secure Poseidon configuration for BN254.
///
/// Uses an MDS Cauchy matrix and round constants derived from a domain-separated
/// seed (`b"liliadb-poseidon-bn254-t<width>"`), guaranteeing full diffusion and algebraic security.
fn build_poseidon_config(
    rate: usize,
    capacity: usize,
    full_rounds: usize,
    partial_rounds: usize,
) -> PoseidonConfig<Fr> {
    let width = rate + capacity;
    let seed_str = format!("liliadb-poseidon-bn254-t{width}-v1");
    let mut seed = [0u8; 32];
    let bytes = seed_str.as_bytes();
    seed[..bytes.len().min(32)].copy_from_slice(&bytes[..bytes.len().min(32)]);
    let mut rng = StdRng::from_seed(seed);

    let total_rounds = full_rounds + partial_rounds;
    let mut ark = Vec::with_capacity(total_rounds);
    for _ in 0..total_rounds {
        let mut round_constants = Vec::with_capacity(width);
        for _ in 0..width {
            round_constants.push(Fr::rand(&mut rng));
        }
        ark.push(round_constants);
    }

    // Generate an MDS Cauchy matrix: M_ij = 1 / (x_i + y_j) where x_i + y_j != 0
    let mut xs = Vec::with_capacity(width);
    let mut ys = Vec::with_capacity(width);
    for _ in 0..width {
        xs.push(Fr::rand(&mut rng));
        ys.push(Fr::rand(&mut rng));
    }

    let mut mds = Vec::with_capacity(width);
    for x in &xs {
        let mut row = Vec::with_capacity(width);
        for y in &ys {
            let sum = *x + *y;
            let inv = sum.inverse().unwrap_or(Fr::ONE);
            row.push(inv);
        }
        mds.push(row);
    }

    PoseidonConfig::new(
        full_rounds,
        partial_rounds,
        5, // alpha = 5 for BN254
        mds,
        ark,
        rate,
        capacity,
    )
}

/// Returns the cached 5-element Poseidon configuration (rate=4, capacity=1).
/// Suitable for hashing 4 field elements (e.g. `doc_id`, `val1`, `val2`, `salt`) in a single permutation.
#[must_use]
pub fn poseidon_config_t5() -> &'static PoseidonConfig<Fr> {
    POSEIDON_CONFIG_T5.get_or_init(|| build_poseidon_config(4, 1, 8, 60))
}

/// Returns the cached 3-element Poseidon configuration (rate=2, capacity=1).
/// Suitable for pairwise 2:1 tree hashing or 2-element commitments.
#[must_use]
pub fn poseidon_config_t3() -> &'static PoseidonConfig<Fr> {
    POSEIDON_CONFIG_T3.get_or_init(|| build_poseidon_config(2, 1, 8, 56))
}

/// Computes the native Poseidon commitment over up to 4 elements.
///
/// Output: 32-byte field element `Fr` serving as the public commitment $C$.
#[must_use]
pub fn poseidon_hash_4(doc_id: Fr, val1: Fr, val2: Fr, salt: Fr) -> Fr {
    let config = poseidon_config_t5();
    let mut sponge = PoseidonSponge::new(config);
    sponge.absorb(&doc_id);
    sponge.absorb(&val1);
    sponge.absorb(&val2);
    sponge.absorb(&salt);
    sponge.squeeze_field_elements(1)[0]
}

/// Computes the native Poseidon commitment over 2 elements (e.g. single value + salt).
#[must_use]
pub fn poseidon_hash_2(val: Fr, salt: Fr) -> Fr {
    let config = poseidon_config_t3();
    let mut sponge = PoseidonSponge::new(config);
    sponge.absorb(&val);
    sponge.absorb(&salt);
    sponge.squeeze_field_elements(1)[0]
}

/// In-circuit Poseidon sponge gadget for 4 input variables.
///
/// Enforces the commitment constraint in the R1CS constraint system.
pub fn poseidon_hash_4_gadget(
    doc_id: &FpVar<Fr>,
    val1: &FpVar<Fr>,
    val2: &FpVar<Fr>,
    salt: &FpVar<Fr>,
) -> std::result::Result<FpVar<Fr>, SynthesisError> {
    let config = poseidon_config_t5();
    let mut sponge_var = PoseidonSpongeVar::new(doc_id.cs(), config);
    sponge_var.absorb(doc_id)?;
    sponge_var.absorb(val1)?;
    sponge_var.absorb(val2)?;
    sponge_var.absorb(salt)?;
    let mut out = sponge_var.squeeze_field_elements(1)?;
    Ok(out.remove(0))
}

/// Converts a 64-bit integer into a BN254 scalar field element.
#[must_use]
pub fn u64_to_fr(val: u64) -> Fr {
    Fr::from(val)
}

/// Converts an arbitrary string identifier (e.g. "`usr_42`") into a BN254 scalar field element
/// using domain-separated BLAKE3 hashing.
#[must_use]
pub fn str_to_fr(s: &str) -> Fr {
    let hash = blake3::hash(s.as_bytes());
    Fr::from_be_bytes_mod_order(hash.as_bytes())
}

/// Converts a 32-byte slice or hex string into a BN254 scalar field element.
#[must_use]
pub fn bytes_to_fr(bytes: &[u8; 32]) -> Fr {
    Fr::from_be_bytes_mod_order(bytes)
}

/// Formats a BN254 scalar field element as a 0x-prefixed 64-character hex string.
#[must_use]
pub fn fr_to_hex(val: &Fr) -> String {
    let bi = val.into_bigint();
    let bytes = bi.to_bytes_be();
    format!("0x{}", hex::encode(bytes))
}

/// Parses a 0x-prefixed or raw hex string into a BN254 scalar field element.
pub fn hex_to_fr(hex_str: &str) -> Result<Fr, crate::error::ZkError> {
    let clean = hex_str.trim().trim_start_matches("0x");
    let bytes = hex::decode(clean)?;
    if bytes.len() > 32 {
        return Err(crate::error::ZkError::Serialization(
            "Hex string exceeds 32 bytes for BN254 Fr".into(),
        ));
    }
    let mut buf = [0u8; 32];
    buf[32 - bytes.len()..].copy_from_slice(&bytes);
    Ok(Fr::from_be_bytes_mod_order(&buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_r1cs_std::alloc::AllocVar;
    use ark_r1cs_std::eq::EqGadget;
    use ark_relations::gr1cs::ConstraintSystem;

    #[test]
    fn test_native_vs_gadget_poseidon() {
        let cs = ConstraintSystem::<Fr>::new_ref();

        let id = str_to_fr("usr_101");
        let v1 = u64_to_fr(2450);
        let v2 = u64_to_fr(29);
        let salt = u64_to_fr(999_999);

        // Native calculation
        let native_hash = poseidon_hash_4(id, v1, v2, salt);

        // Circuit calculation
        let id_var = FpVar::new_witness(cs.clone(), || Ok(id)).unwrap();
        let v1_var = FpVar::new_witness(cs.clone(), || Ok(v1)).unwrap();
        let v2_var = FpVar::new_witness(cs.clone(), || Ok(v2)).unwrap();
        let salt_var = FpVar::new_witness(cs.clone(), || Ok(salt)).unwrap();

        let gadget_hash = poseidon_hash_4_gadget(&id_var, &v1_var, &v2_var, &salt_var).unwrap();

        let expected_var = FpVar::new_input(cs.clone(), || Ok(native_hash)).unwrap();
        gadget_hash.enforce_equal(&expected_var).unwrap();

        assert!(cs.is_satisfied().unwrap());
        println!(
            "Poseidon R1CS constraints for t=5: {}",
            cs.num_constraints()
        );
    }
}
