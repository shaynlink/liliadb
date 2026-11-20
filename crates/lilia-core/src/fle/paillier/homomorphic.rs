//! Additive homomorphic operations on Paillier ciphertexts.

use num_bigint::BigUint;
use num_integer::Integer;
use num_traits::One;

use super::cipher::PaillierCiphertext;
use super::keys::{mod_inverse_biguint, random_biguint_range, PaillierPublicKey};
use crate::{ErrorCode, LiliaError, Result};

/// Multiplies two ciphertexts modulo $n^2$, corresponding to the addition of plaintexts:
/// $$E(m_1) \times E(m_2) \equiv E(m_1 + m_2) \pmod{n^2}$$
///
/// # Errors
/// Returns `InvalidInput` if the two ciphertexts have incompatible decimal scales.
pub fn add(
    pk: &PaillierPublicKey,
    c1: &PaillierCiphertext,
    c2: &PaillierCiphertext,
) -> Result<PaillierCiphertext> {
    if c1.scale != c2.scale {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            format!(
                "cannot add ciphertexts with different scales: {} vs {}",
                c1.scale, c2.scale
            ),
            false,
        ));
    }
    let sum_raw = add_raw(&pk.n_squared, &c1.c, &c2.c);
    Ok(PaillierCiphertext::new(sum_raw, c1.scale))
}

/// Raw modular multiplication of two ciphertext big integers modulo $n^2$.
#[inline]
#[must_use]
pub fn add_raw(n_squared: &BigUint, c1: &BigUint, c2: &BigUint) -> BigUint {
    (c1 * c2) % n_squared
}

/// Subtracts $c_2$ from $c_1$ homomorphically:
/// $$E(m_1) \times E(m_2)^{-1} \equiv E(m_1 - m_2) \pmod{n^2}$$
///
/// # Errors
/// Returns `InvalidInput` if scale mismatch or if $c_2$ is not invertible.
pub fn sub(
    pk: &PaillierPublicKey,
    c1: &PaillierCiphertext,
    c2: &PaillierCiphertext,
) -> Result<PaillierCiphertext> {
    if c1.scale != c2.scale {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            format!(
                "cannot subtract ciphertexts with different scales: {} vs {}",
                c1.scale, c2.scale
            ),
            false,
        ));
    }
    let c2_inv = mod_inverse_biguint(&c2.c, &pk.n_squared).ok_or_else(|| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            "c2 is not invertible modulo n^2",
            false,
        )
    })?;
    let diff_raw = (&c1.c * &c2_inv) % &pk.n_squared;
    Ok(PaillierCiphertext::new(diff_raw, c1.scale))
}

/// Multiplies a ciphertext by a cleartext scalar integer $k$:
/// $$E(m)^k \equiv E(k \cdot m) \pmod{n^2}$$
///
/// # Errors
/// Returns `InvalidInput` if negative scalar and ciphertext is not invertible.
pub fn mul_scalar(
    pk: &PaillierPublicKey,
    c: &PaillierCiphertext,
    scalar: i64,
) -> Result<PaillierCiphertext> {
    if scalar == 0 {
        return Ok(zero(pk, c.scale));
    }
    if scalar > 0 {
        let exp = BigUint::from(scalar as u64);
        let res = c.c.modpow(&exp, &pk.n_squared);
        Ok(PaillierCiphertext::new(res, c.scale))
    } else {
        let abs_scalar = scalar.unsigned_abs();
        let exp = BigUint::from(abs_scalar);
        let inv = mod_inverse_biguint(&c.c, &pk.n_squared).ok_or_else(|| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                "ciphertext is not invertible modulo n^2",
                false,
            )
        })?;
        let res = inv.modpow(&exp, &pk.n_squared);
        Ok(PaillierCiphertext::new(res, c.scale))
    }
}

/// Returns the additive identity element ($E(0) = 1 \pmod{n^2}$) for the given scale.
#[must_use]
pub fn zero(pk: &PaillierPublicKey, scale: u8) -> PaillierCiphertext {
    let _ = pk;
    PaillierCiphertext::new(BigUint::one(), scale)
}

/// Returns the raw additive identity element ($1$).
#[inline]
#[must_use]
pub fn zero_raw() -> BigUint {
    BigUint::one()
}

/// Re-randomizes a ciphertext without changing its decrypted plaintext:
/// $$c' = c \cdot r^n \pmod{n^2}$$
#[must_use]
pub fn rerandomize(pk: &PaillierPublicKey, c: &PaillierCiphertext) -> PaillierCiphertext {
    let mut rng = rand::rng();
    let one = BigUint::one();
    let n_minus_1 = &pk.n - &one;

    let r = loop {
        let candidate = random_biguint_range(&mut rng, &one, &n_minus_1);
        if candidate.gcd(&pk.n).is_one() {
            break candidate;
        }
    };

    let r_pow_n = r.modpow(&pk.n, &pk.n_squared);
    let fresh_c = (&c.c * &r_pow_n) % &pk.n_squared;
    PaillierCiphertext::new(fresh_c, c.scale)
}
