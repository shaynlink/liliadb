//! Cryptographic primitives for Field-Level Encryption (FLE).
//!
//! Provides:
//! - AES-256-GCM (Randomized AEAD for high confidentiality)
//! - AES-256-SIV (Deterministic AEAD, RFC 5297, for equality-searchable fields)

use aes_gcm::aead::{Aead as GcmAead, KeyInit as GcmKeyInit, Payload as GcmPayload};
use aes_gcm::{Aes256Gcm, Nonce as GcmNonce};
use aes_siv::aead::{Aead as SivAead, KeyInit as SivKeyInit, Payload as SivPayload};
use aes_siv::Aes256SivAead;

use crate::{ErrorCode, LiliaError, Result};

pub const GCM_NONCE_LEN: usize = 12;
pub const GCM_TAG_LEN: usize = 16;
pub const GCM_MIN_PAYLOAD_LEN: usize = GCM_NONCE_LEN + GCM_TAG_LEN;

pub const SIV_TAG_LEN: usize = 16;
pub const SIV_MIN_PAYLOAD_LEN: usize = SIV_TAG_LEN;

/// Encrypts `plaintext` using AES-256-GCM (Randomized AEAD).
///
/// Encoded layout: `[12-byte random nonce][ciphertext with 16-byte GCM tag]`.
pub fn encrypt_gcm(plaintext: &[u8], key: &[u8; 32], ad: Option<&[u8]>) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(aes_gcm::Key::<Aes256Gcm>::from_slice(key));
    let nonce_bytes: [u8; GCM_NONCE_LEN] = rand::random();
    let nonce = GcmNonce::from_slice(&nonce_bytes);

    let payload = GcmPayload {
        msg: plaintext,
        aad: ad.unwrap_or(&[]),
    };

    let ciphertext = cipher.encrypt(nonce, payload).map_err(|e| {
        LiliaError::new(
            ErrorCode::Storage,
            format!("AES-256-GCM field encryption failed: {e}"),
            false,
        )
    })?;

    let mut out = Vec::with_capacity(GCM_NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypts an AES-256-GCM encoded payload.
pub fn decrypt_gcm(payload: &[u8], key: &[u8; 32], ad: Option<&[u8]>) -> Result<Vec<u8>> {
    if payload.len() < GCM_MIN_PAYLOAD_LEN {
        return Err(LiliaError::new(
            ErrorCode::Corrupt,
            "AES-256-GCM payload too short",
            false,
        ));
    }

    let cipher = Aes256Gcm::new(aes_gcm::Key::<Aes256Gcm>::from_slice(key));
    let nonce = GcmNonce::from_slice(&payload[..GCM_NONCE_LEN]);
    let ciphertext = &payload[GCM_NONCE_LEN..];

    let gcm_payload = GcmPayload {
        msg: ciphertext,
        aad: ad.unwrap_or(&[]),
    };

    cipher.decrypt(nonce, gcm_payload).map_err(|_| {
        LiliaError::new(
            ErrorCode::Corrupt,
            "AES-256-GCM field decryption / auth tag failed",
            false,
        )
    })
}

/// Encrypts `plaintext` using AES-256-SIV (Deterministic AEAD, RFC 5297).
///
/// For identical plaintext, key, and associated data (`ad`), identical ciphertext is produced.
pub fn encrypt_siv(plaintext: &[u8], key: &[u8; 64], ad: Option<&[u8]>) -> Result<Vec<u8>> {
    let cipher = Aes256SivAead::new(&(*key).into());
    // Deterministic AEAD uses empty nonce (RFC 5297 section 3)
    let nonce = aes_siv::Nonce::default();

    let payload = SivPayload {
        msg: plaintext,
        aad: ad.unwrap_or(&[]),
    };

    cipher.encrypt(&nonce, payload).map_err(|e| {
        LiliaError::new(
            ErrorCode::Storage,
            format!("AES-256-SIV deterministic encryption failed: {e}"),
            false,
        )
    })
}

/// Decrypts an AES-256-SIV payload.
pub fn decrypt_siv(payload: &[u8], key: &[u8; 64], ad: Option<&[u8]>) -> Result<Vec<u8>> {
    if payload.len() < SIV_MIN_PAYLOAD_LEN {
        return Err(LiliaError::new(
            ErrorCode::Corrupt,
            "AES-256-SIV payload too short",
            false,
        ));
    }

    let cipher = Aes256SivAead::new(&(*key).into());
    let nonce = aes_siv::Nonce::default();

    let siv_payload = SivPayload {
        msg: payload,
        aad: ad.unwrap_or(&[]),
    };

    cipher.decrypt(&nonce, siv_payload).map_err(|_| {
        LiliaError::new(
            ErrorCode::Corrupt,
            "AES-256-SIV field decryption / auth tag failed",
            false,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gcm_roundtrip_and_randomness() {
        let key: [u8; 32] = rand::random();
        let msg = b"patient-ssn-123-45-6789";
        let ad = b"users:ssn";

        let c1 = encrypt_gcm(msg, &key, Some(ad)).unwrap();
        let c2 = encrypt_gcm(msg, &key, Some(ad)).unwrap();

        // GCM is randomized: same plaintext produces different ciphertexts
        assert_ne!(c1, c2);

        let d1 = decrypt_gcm(&c1, &key, Some(ad)).unwrap();
        assert_eq!(d1, msg);

        // Wrong AD fails
        assert!(decrypt_gcm(&c1, &key, Some(b"other:field")).is_err());
    }

    #[test]
    fn test_siv_roundtrip_and_determinism() {
        let key: [u8; 64] = rand::random();
        let msg = b"alice@example.com";
        let ad = b"users:email";

        let c1 = encrypt_siv(msg, &key, Some(ad)).unwrap();
        let c2 = encrypt_siv(msg, &key, Some(ad)).unwrap();

        // SIV is deterministic: same plaintext produces identical ciphertexts
        assert_eq!(c1, c2);

        let d1 = decrypt_siv(&c1, &key, Some(ad)).unwrap();
        assert_eq!(d1, msg);

        // Wrong AD fails
        assert!(decrypt_siv(&c1, &key, Some(b"wrong:ad")).is_err());
    }
}
