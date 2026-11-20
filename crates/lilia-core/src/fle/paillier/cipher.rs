//! Encryption and decryption routines for the Paillier cryptosystem.

use num_bigint::BigUint;
use num_integer::Integer;
use num_traits::{One, ToPrimitive};

use super::keys::{random_biguint_range, PaillierPrivateKey, PaillierPublicKey};
use crate::{ErrorCode, LiliaError, Result};

pub const CIPHERTEXT_VERSION: u8 = 0x01;

/// A Paillier ciphertext representing an encrypted scalar value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaillierCiphertext {
    pub(crate) c: BigUint,
    pub(crate) scale: u8,
}

impl PaillierCiphertext {
    #[must_use]
    pub fn new(c: BigUint, scale: u8) -> Self {
        Self { c, scale }
    }

    #[must_use]
    pub fn raw(&self) -> &BigUint {
        &self.c
    }

    #[must_use]
    pub fn scale(&self) -> u8 {
        self.scale
    }

    /// Serializes ciphertext to binary bytes: `[version (1B), scale (1B), big-endian c bytes]`.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let c_bytes = self.c.to_bytes_be();
        let mut out = Vec::with_capacity(2 + c_bytes.len());
        out.push(CIPHERTEXT_VERSION);
        out.push(self.scale);
        out.extend_from_slice(&c_bytes);
        out
    }

    /// Deserializes ciphertext from binary bytes.
    ///
    /// # Errors
    /// Returns `InvalidInput` if byte buffer is invalid.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 3 {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "ciphertext bytes too short",
                false,
            ));
        }
        let version = bytes[0];
        if version != CIPHERTEXT_VERSION {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!("unsupported Paillier ciphertext version: {version}"),
                false,
            ));
        }
        let scale = bytes[1];
        let c = BigUint::from_bytes_be(&bytes[2..]);
        Ok(Self { c, scale })
    }

    /// Encodes ciphertext to base64 string.
    #[must_use]
    pub fn to_base64(&self) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(self.to_bytes())
    }

    /// Decodes ciphertext from base64 string.
    ///
    /// # Errors
    /// Returns `InvalidInput` if base64 decoding or structure is invalid.
    pub fn from_base64(s: &str) -> Result<Self> {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(s.trim())
            .map_err(|e| {
                LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("invalid base64 for Paillier ciphertext: {e}"),
                    false,
                )
            })?;
        Self::from_bytes(&bytes)
    }
}

/// Encrypts a 64-bit signed integer using Paillier public key.
///
/// Uses the Damgård-Jurik optimization $g = n + 1$:
/// $$c = (1 + m \cdot n) \cdot r^n \pmod{n^2}$$
#[must_use]
pub fn encrypt_i64(pk: &PaillierPublicKey, val: i64) -> PaillierCiphertext {
    let m = if val >= 0 {
        BigUint::from(val as u64)
    } else {
        let abs_val = val.unsigned_abs();
        &pk.n - (BigUint::from(abs_val) % &pk.n)
    };

    encrypt_biguint_scaled(pk, &m, 0)
}

/// Encrypts a 64-bit float using fixed-point scaling factor $10^{\text{scale}}$.
#[must_use]
pub fn encrypt_f64(pk: &PaillierPublicKey, val: f64, scale: u8) -> PaillierCiphertext {
    let multiplier = 10f64.powi(i32::from(scale));
    let int_val = (val * multiplier).round() as i64;
    let m = if int_val >= 0 {
        BigUint::from(int_val as u64)
    } else {
        let abs_val = int_val.unsigned_abs();
        &pk.n - (BigUint::from(abs_val) % &pk.n)
    };

    encrypt_biguint_scaled(pk, &m, scale)
}

/// Encrypts a message $m \in \mathbb{Z}_n$ with scale metadata.
#[must_use]
pub fn encrypt_biguint_scaled(
    pk: &PaillierPublicKey,
    m: &BigUint,
    scale: u8,
) -> PaillierCiphertext {
    // 1 + m*n mod n^2
    let term1 = (BigUint::one() + (m * &pk.n)) % &pk.n_squared;

    // Pick random r in [1, n-1] with gcd(r, n) == 1
    let mut rng = rand::rng();
    let one = BigUint::one();
    let n_minus_1 = &pk.n - &one;

    let r = loop {
        let candidate = random_biguint_range(&mut rng, &one, &n_minus_1);
        if candidate.gcd(&pk.n).is_one() {
            break candidate;
        }
    };

    // r^n mod n^2
    let term2 = r.modpow(&pk.n, &pk.n_squared);

    // c = term1 * term2 mod n^2
    let c = (term1 * term2) % &pk.n_squared;

    PaillierCiphertext::new(c, scale)
}

/// Decrypts a Paillier ciphertext to a signed 64-bit integer using CRT acceleration.
///
/// # Errors
/// Returns `InvalidInput` if ciphertext is out of bounds or cannot fit into `i64`.
pub fn decrypt_i64(
    sk: &PaillierPrivateKey,
    pk: &PaillierPublicKey,
    ciphertext: &PaillierCiphertext,
) -> Result<i64> {
    let m = decrypt_raw(sk, pk, &ciphertext.c)?;

    // Map m from Zn back to signed integer
    let half_n = &pk.n >> 1;
    if m > half_n {
        let neg = &pk.n - m;
        let val_u64 = neg.to_u64().ok_or_else(|| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                "decrypted negative value overflows i64",
                false,
            )
        })?;
        Ok(-(val_u64 as i64))
    } else {
        let val_u64 = m.to_u64().ok_or_else(|| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                "decrypted positive value overflows i64",
                false,
            )
        })?;
        Ok(val_u64 as i64)
    }
}

/// Decrypts a Paillier ciphertext to a 64-bit float according to its encoded scale.
///
/// # Errors
/// Returns `InvalidInput` if decrypted number cannot fit into `i64`.
pub fn decrypt_f64(
    sk: &PaillierPrivateKey,
    pk: &PaillierPublicKey,
    ciphertext: &PaillierCiphertext,
) -> Result<f64> {
    let int_val = decrypt_i64(sk, pk, ciphertext)?;
    if ciphertext.scale == 0 {
        Ok(int_val as f64)
    } else {
        let divisor = 10f64.powi(i32::from(ciphertext.scale));
        Ok((int_val as f64) / divisor)
    }
}

/// Core CRT (Chinese Remainder Theorem) decryption routine.
fn decrypt_raw(sk: &PaillierPrivateKey, pk: &PaillierPublicKey, c: &BigUint) -> Result<BigUint> {
    if c >= &pk.n_squared {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "ciphertext is greater than or equal to n^2",
            false,
        ));
    }

    // cp = c mod p^2, cq = c mod q^2
    let cp = c % &sk.p_squared;
    let cq = c % &sk.q_squared;

    let p_minus_1 = &sk.p - 1u32;
    let q_minus_1 = &sk.q - 1u32;

    // ap = cp^(p-1) mod p^2
    let ap = cp.modpow(&p_minus_1, &sk.p_squared);
    // aq = cq^(q-1) mod q^2
    let aq = cq.modpow(&q_minus_1, &sk.q_squared);

    // lp = (ap - 1) / p
    let lp = (ap - 1u32) / &sk.p;
    // lq = (aq - 1) / q
    let lq = (aq - 1u32) / &sk.q;

    // mp = lp * hp mod p
    let mp = (lp * &sk.hp) % &sk.p;
    // mq = lq * hq mod q
    let mq = (lq * &sk.hq) % &sk.q;

    // Garner's CRT: m = mq + q * ((mp - mq) * q_inv_p mod p)
    let diff = if mp >= mq {
        &mp - &mq
    } else {
        &sk.p - ((&mq - &mp) % &sk.p)
    };

    let h = (diff * &sk.q_inv_p) % &sk.p;
    let m = mq + (&h * &sk.q);

    Ok(m % &pk.n)
}
