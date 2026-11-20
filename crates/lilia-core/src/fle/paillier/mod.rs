//! Additive Homomorphic Encryption using the Paillier cryptosystem.
//!
//! Provides semantic IND-CPA security for numeric values, allowing linear additions
//! and scalar multiplications directly on ciphertexts without decryption:
//! $$E(m_1) \times E(m_2) \equiv E(m_1 + m_2) \pmod{n^2}$$
//! $$E(m)^k \equiv E(k \cdot m) \pmod{n^2}$$

pub mod cipher;
pub mod homomorphic;
pub mod keys;
#[cfg(test)]
mod tests;

pub use cipher::{
    decrypt_f64, decrypt_i64, encrypt_f64, encrypt_i64, PaillierCiphertext, CIPHERTEXT_VERSION,
};
pub use homomorphic::{add, add_raw, mul_scalar, rerandomize, sub, zero, zero_raw};
pub use keys::{PaillierKeypair, PaillierPrivateKey, PaillierPublicKey};
