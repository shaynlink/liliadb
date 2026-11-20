//! Paillier Public, Private, and Keypair types and key generation.

use std::sync::Arc;

use num_bigint::{BigInt, BigUint};
use num_integer::Integer;
use num_traits::{One, Zero};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use sha2::{Digest, Sha256};

use crate::{ErrorCode, LiliaError, Result};

/// Paillier Public Key containing modulus `n`, precalculated `n^2`, and bit length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaillierPublicKey {
    pub(crate) n: BigUint,
    pub(crate) n_squared: BigUint,
    pub(crate) bit_len: usize,
}

impl PaillierPublicKey {
    /// Constructs a `PaillierPublicKey` from modulus `n`.
    #[must_use]
    pub fn new(n: BigUint) -> Self {
        let n_squared = &n * &n;
        let bit_len = n.bits() as usize;
        Self {
            n,
            n_squared,
            bit_len,
        }
    }

    #[must_use]
    pub fn n(&self) -> &BigUint {
        &self.n
    }

    #[must_use]
    pub fn n_squared(&self) -> &BigUint {
        &self.n_squared
    }

    #[must_use]
    pub fn bit_len(&self) -> usize {
        self.bit_len
    }

    /// Serializes the public key to binary bytes (big-endian encoding of `n`).
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        self.n.to_bytes_be()
    }

    /// Deserializes a public key from binary bytes.
    ///
    /// # Errors
    /// Returns `InvalidInput` if the slice is empty or modulus is invalid.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "empty bytes for Paillier public key",
                false,
            ));
        }
        let n = BigUint::from_bytes_be(bytes);
        if n <= BigUint::one() {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "invalid Paillier modulus n",
                false,
            ));
        }
        Ok(Self::new(n))
    }

    /// Encodes the public key as standard base64 string.
    #[must_use]
    pub fn to_base64(&self) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(self.to_bytes())
    }

    /// Decodes a public key from a base64 string.
    ///
    /// # Errors
    /// Returns `InvalidInput` if base64 decoding fails.
    pub fn from_base64(s: &str) -> Result<Self> {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(s.trim())
            .map_err(|e| {
                LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("invalid base64 for Paillier public key: {e}"),
                    false,
                )
            })?;
        Self::from_bytes(&bytes)
    }
}

/// Paillier Private Key with CRT (Chinese Remainder Theorem) acceleration parameters.
#[derive(Clone)]
pub struct PaillierPrivateKey {
    pub(crate) p: BigUint,
    pub(crate) q: BigUint,
    pub(crate) lambda: BigUint,
    pub(crate) mu: BigUint,
    // CRT parameters:
    pub(crate) p_squared: BigUint,
    pub(crate) q_squared: BigUint,
    pub(crate) hp: BigUint,
    pub(crate) hq: BigUint,
    pub(crate) q_inv_p: BigUint,
}

impl std::fmt::Debug for PaillierPrivateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaillierPrivateKey")
            .field("p", &"***REDACTED***")
            .field("q", &"***REDACTED***")
            .finish()
    }
}

impl PaillierPrivateKey {
    #[must_use]
    pub fn lambda(&self) -> &BigUint {
        &self.lambda
    }

    #[must_use]
    pub fn mu(&self) -> &BigUint {
        &self.mu
    }

    /// Constructs a private key from primes `p` and `q` and public key `n`.
    ///
    /// # Errors
    /// Returns `InvalidInput` if primes are equal or gcd condition fails.
    pub fn from_primes(p: BigUint, q: BigUint, n: &BigUint) -> Result<Self> {
        if p == q || p <= BigUint::one() || q <= BigUint::one() {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "invalid primes p and q for Paillier",
                false,
            ));
        }

        let p_minus_1 = &p - 1u32;
        let q_minus_1 = &q - 1u32;
        let gcd = p_minus_1.gcd(&q_minus_1);
        let lambda = (&p_minus_1 * &q_minus_1) / gcd;

        // mu = lambda^(-1) mod n
        let mu = mod_inverse_biguint(&lambda, n).ok_or_else(|| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                "lambda is not invertible modulo n",
                false,
            )
        })?;

        let p_squared = &p * &p;
        let q_squared = &q * &q;

        // CRT parameters:
        // hp = (-q)^(-1) mod p
        let q_mod_p = &q % &p;
        let neg_q_mod_p = if q_mod_p.is_zero() {
            BigUint::zero()
        } else {
            &p - &q_mod_p
        };
        let hp = mod_inverse_biguint(&neg_q_mod_p, &p).ok_or_else(|| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                "hp calculation failed during CRT setup",
                false,
            )
        })?;

        // hq = (-p)^(-1) mod q
        let p_mod_q = &p % &q;
        let neg_p_mod_q = if p_mod_q.is_zero() {
            BigUint::zero()
        } else {
            &q - &p_mod_q
        };
        let hq = mod_inverse_biguint(&neg_p_mod_q, &q).ok_or_else(|| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                "hq calculation failed during CRT setup",
                false,
            )
        })?;

        // q_inv_p = q^(-1) mod p
        let q_inv_p = mod_inverse_biguint(&q, &p).ok_or_else(|| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                "q_inv_p calculation failed during CRT setup",
                false,
            )
        })?;

        Ok(Self {
            p,
            q,
            lambda,
            mu,
            p_squared,
            q_squared,
            hp,
            hq,
            q_inv_p,
        })
    }

    /// Serializes private key as [p_len (4 bytes BE), p bytes, q_len (4 bytes BE), q bytes].
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let p_bytes = self.p.to_bytes_be();
        let q_bytes = self.q.to_bytes_be();
        let mut out = Vec::with_capacity(8 + p_bytes.len() + q_bytes.len());
        out.extend_from_slice(&(p_bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(&p_bytes);
        out.extend_from_slice(&(q_bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(&q_bytes);
        out
    }

    /// Deserializes private key from bytes and reconstructs CRT parameters given modulus `n`.
    ///
    /// # Errors
    /// Returns `InvalidInput` if byte buffer is malformed.
    pub fn from_bytes(bytes: &[u8], n: &BigUint) -> Result<Self> {
        if bytes.len() < 8 {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "buffer too short for Paillier private key",
                false,
            ));
        }
        let p_len = u32::from_be_bytes(bytes[0..4].try_into().unwrap()) as usize;
        if bytes.len() < 4 + p_len + 4 {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "malformed private key buffer (p_len)",
                false,
            ));
        }
        let p = BigUint::from_bytes_be(&bytes[4..4 + p_len]);
        let offset = 4 + p_len;
        let q_len = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        if bytes.len() < offset + 4 + q_len {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "malformed private key buffer (q_len)",
                false,
            ));
        }
        let q = BigUint::from_bytes_be(&bytes[offset + 4..offset + 4 + q_len]);
        Self::from_primes(p, q, n)
    }
}

/// A complete Paillier Keypair combining the public and private keys.
#[derive(Clone, Debug)]
pub struct PaillierKeypair {
    pub public_key: Arc<PaillierPublicKey>,
    pub private_key: Arc<PaillierPrivateKey>,
}

impl PaillierKeypair {
    /// Creates a keypair from primes `p` and `q`.
    ///
    /// # Errors
    /// Returns `InvalidInput` if primes are invalid.
    pub fn from_primes(p: BigUint, q: BigUint) -> Result<Self> {
        let n = &p * &q;
        let pk = Arc::new(PaillierPublicKey::new(n.clone()));
        let sk = Arc::new(PaillierPrivateKey::from_primes(p, q, &n)?);
        Ok(Self {
            public_key: pk,
            private_key: sk,
        })
    }

    /// Generates a fresh Paillier keypair of the specified bit length (e.g. 1024 or 2048).
    ///
    /// # Errors
    /// Returns `InvalidInput` if bits is less than 512.
    pub fn generate(bits: usize) -> Result<Self> {
        if bits < 512 {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "Paillier key length must be at least 512 bits",
                false,
            ));
        }
        let prime_bits = bits / 2;
        let mut rng = rand::rng();

        loop {
            let p = generate_prime(&mut rng, prime_bits);
            let q = generate_prime(&mut rng, prime_bits);
            if p != q {
                if let Ok(kp) = Self::from_primes(p, q) {
                    return Ok(kp);
                }
            }
        }
    }

    /// Deterministically derives a Paillier keypair from a 32-byte seed / salt.
    ///
    /// Useful for reconstructing compartment keys from a master passphrase and compartment salt.
    ///
    /// # Errors
    /// Returns `InvalidInput` if derivation fails.
    pub fn from_seed(seed: &[u8], bits: usize) -> Result<Self> {
        let prime_bits = bits / 2;
        let mut hasher = Sha256::new();
        hasher.update(b"liliadb:paillier:seed:v1");
        hasher.update(seed);
        let hash: [u8; 32] = hasher.finalize().into();
        let mut rng = StdRng::from_seed(hash);

        loop {
            let p = generate_prime(&mut rng, prime_bits);
            let q = generate_prime(&mut rng, prime_bits);
            if p != q {
                if let Ok(kp) = Self::from_primes(p, q) {
                    return Ok(kp);
                }
            }
        }
    }
}

/// Calculates modular inverse of `a` modulo `m` using extended Euclidean algorithm.
pub(crate) fn mod_inverse_biguint(a: &BigUint, m: &BigUint) -> Option<BigUint> {
    if m.is_zero() {
        return None;
    }
    let a_int = BigInt::from(a.clone());
    let m_int = BigInt::from(m.clone());
    let gcd_res = a_int.extended_gcd(&m_int);
    if gcd_res.gcd.is_one() {
        let res = (gcd_res.x % &m_int + &m_int) % &m_int;
        res.to_biguint()
    } else {
        None
    }
}

/// Miller-Rabin primality test with specified number of rounds.
fn is_probable_prime<R: Rng + ?Sized>(n: &BigUint, rounds: usize, rng: &mut R) -> bool {
    if n <= &BigUint::from(3u32) {
        return n > &BigUint::one();
    }
    if n.is_even() {
        return false;
    }

    // Small prime trial division for speed
    const SMALL_PRIMES: [u32; 25] = [
        3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71, 73, 79, 83, 89,
        97, 101,
    ];
    for &p in &SMALL_PRIMES {
        let p_big = BigUint::from(p);
        if n == &p_big {
            return true;
        }
        if (n % &p_big).is_zero() {
            return false;
        }
    }

    // Write n - 1 = 2^s * d with d odd
    let n_minus_1 = n - 1u32;
    let mut d = n_minus_1.clone();
    let mut s = 0usize;
    while d.is_even() {
        d >>= 1;
        s += 1;
    }

    let two = BigUint::from(2u32);
    let n_minus_2 = n - &two;

    'witness_loop: for _ in 0..rounds {
        let a = random_biguint_range(rng, &two, &n_minus_2);
        let mut x = a.modpow(&d, n);
        if x.is_one() || x == n_minus_1 {
            continue 'witness_loop;
        }
        for _ in 0..s - 1 {
            x = x.modpow(&two, n);
            if x == n_minus_1 {
                continue 'witness_loop;
            }
        }
        return false;
    }
    true
}

/// Generates a random `BigUint` of specified bit length.
pub(crate) fn random_biguint_bits<R: Rng + ?Sized>(rng: &mut R, bits: usize) -> BigUint {
    if bits == 0 {
        return BigUint::zero();
    }
    let num_bytes = bits.div_ceil(8);
    let mut bytes = vec![0u8; num_bytes];
    rng.fill(&mut bytes[..]);
    let mut num = BigUint::from_bytes_be(&bytes);
    let excess = num_bytes * 8 - bits;
    if excess > 0 {
        num >>= excess;
    }
    num
}

/// Generates a random `BigUint` in the range `[low, high)`.
pub(crate) fn random_biguint_range<R: Rng + ?Sized>(
    rng: &mut R,
    low: &BigUint,
    high: &BigUint,
) -> BigUint {
    if high <= low {
        return low.clone();
    }
    let range = high - low;
    let bits = range.bits() as usize;
    loop {
        let val = random_biguint_bits(rng, bits);
        if val < range {
            return low + val;
        }
    }
}

/// Generates a random probable prime of exactly `bits` length.
fn generate_prime<R: Rng + ?Sized>(rng: &mut R, bits: usize) -> BigUint {
    let mut candidate = random_biguint_bits(rng, bits);
    let high_bit = BigUint::one() << (bits - 1);
    candidate |= high_bit;
    candidate |= BigUint::one();

    loop {
        if is_probable_prime(&candidate, 32, rng) {
            return candidate;
        }
        candidate += 2u32;
    }
}
