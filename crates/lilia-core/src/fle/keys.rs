//! Key material management and concurrent key registry for compartments.

use std::collections::HashMap;
use std::sync::RwLock;

use sha2::Sha256;

use crate::{ErrorCode, LiliaError, Result};

pub const KEY_EXPANSION_INFO: &[u8] = b"liliadb:aes-siv:key-expansion:v1";
pub const VERIFY_INFO: &[u8] = b"liliadb:compartment:verify:v1";

/// Secure key material supporting both AES-256-GCM (32 bytes) and AES-256-SIV (64 bytes).
#[derive(Clone)]
pub struct KeyMaterial {
    gcm_key: [u8; 32],
    siv_key: [u8; 64],
}

impl std::fmt::Debug for KeyMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyMaterial")
            .field("gcm_key", &"***REDACTED***")
            .field("siv_key", &"***REDACTED***")
            .finish()
    }
}

impl KeyMaterial {
    /// Constructs `KeyMaterial` from a 32-byte master key.
    ///
    /// The 64-byte SIV key is derived via PBKDF2-HMAC-SHA256 expansion (`salt = "liliadb:aes-siv:key-expansion:v1"`).
    #[must_use]
    pub fn from_32_bytes(key: [u8; 32]) -> Self {
        let mut siv_key = [0u8; 64];
        pbkdf2::pbkdf2_hmac::<Sha256>(&key, KEY_EXPANSION_INFO, 1, &mut siv_key);
        Self {
            gcm_key: key,
            siv_key,
        }
    }

    /// Constructs `KeyMaterial` from a 64-byte key.
    ///
    /// The first 32 bytes are used for AES-256-GCM, and the full 64 bytes for AES-256-SIV.
    #[must_use]
    pub fn from_64_bytes(key: [u8; 64]) -> Self {
        let mut gcm_key = [0u8; 32];
        gcm_key.copy_from_slice(&key[..32]);
        Self {
            gcm_key,
            siv_key: key,
        }
    }

    /// Constructs `KeyMaterial` from arbitrary bytes (if 32 or 64 bytes).
    ///
    /// # Errors
    /// Returns `LiliaError` if the slice length is neither 32 nor 64 bytes.
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        if bytes.len() == 32 {
            let mut k = [0u8; 32];
            k.copy_from_slice(bytes);
            Ok(Self::from_32_bytes(k))
        } else if bytes.len() == 64 {
            let mut k = [0u8; 64];
            k.copy_from_slice(bytes);
            Ok(Self::from_64_bytes(k))
        } else {
            Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!("key length must be 32 or 64 bytes, got {}", bytes.len()),
                false,
            ))
        }
    }

    /// Derives `KeyMaterial` from a passphrase using PBKDF2-HMAC-SHA256 (100,000 iterations).
    #[must_use]
    pub fn from_passphrase(passphrase: &str, salt: &[u8]) -> Self {
        let mut key32 = [0u8; 32];
        pbkdf2::pbkdf2_hmac::<Sha256>(passphrase.as_bytes(), salt, 100_000, &mut key32);
        Self::from_32_bytes(key32)
    }

    #[must_use]
    pub fn gcm_key(&self) -> &[u8; 32] {
        &self.gcm_key
    }

    #[must_use]
    pub fn siv_key(&self) -> &[u8; 64] {
        &self.siv_key
    }

    /// Computes a verification tag to validate if a passphrase/key is correct against a stored salt.
    #[must_use]
    pub fn compute_verify_tag(&self, salt: &[u8]) -> [u8; 32] {
        use sha2::Digest;
        let mut hasher = Sha256::new();
        hasher.update(self.gcm_key);
        hasher.update(salt);
        hasher.update(VERIFY_INFO);
        hasher.finalize().into()
    }
}

/// Metadata about a compartment's availability in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompartmentInfo {
    pub id: String,
    pub is_unlocked: bool,
}

/// Thread-safe registry holding unlocked keys for compartments / tenants.
#[derive(Debug, Default)]
pub struct KeyRegistry {
    keys: RwLock<HashMap<String, KeyMaterial>>,
}

impl KeyRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            keys: RwLock::new(HashMap::new()),
        }
    }

    /// Registers / unlocks a key for a given compartment `id`.
    pub fn register_key(&self, id: impl Into<String>, key: KeyMaterial) {
        let mut guard = self
            .keys
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.insert(id.into(), key);
    }

    /// Locks / removes a key from memory for compartment `id`.
    pub fn lock_key(&self, id: &str) -> bool {
        let mut guard = self
            .keys
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.remove(id).is_some()
    }

    /// Checks if the compartment `id` is currently unlocked in memory.
    #[must_use]
    pub fn is_unlocked(&self, id: &str) -> bool {
        let guard = self
            .keys
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.contains_key(id)
    }

    /// Retrieves the key material for compartment `id` if unlocked.
    #[must_use]
    pub fn get_key(&self, id: &str) -> Option<KeyMaterial> {
        let guard = self
            .keys
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.get(id).cloned()
    }

    /// Lists all currently known compartments in the registry.
    #[must_use]
    pub fn list(&self) -> Vec<CompartmentInfo> {
        let guard = self
            .keys
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .keys()
            .map(|id| CompartmentInfo {
                id: id.clone(),
                is_unlocked: true,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_material_derivation() {
        let salt = b"custom_salt_12345";
        let k1 = KeyMaterial::from_passphrase("super-secret-password", salt);
        let k2 = KeyMaterial::from_passphrase("super-secret-password", salt);

        assert_eq!(k1.gcm_key(), k2.gcm_key());
        assert_eq!(k1.siv_key(), k2.siv_key());

        let tag1 = k1.compute_verify_tag(salt);
        let tag2 = k2.compute_verify_tag(salt);
        assert_eq!(tag1, tag2);

        let k_diff = KeyMaterial::from_passphrase("wrong-password", salt);
        assert_ne!(k1.compute_verify_tag(salt), k_diff.compute_verify_tag(salt));
    }

    #[test]
    fn test_key_registry_lifecycle() {
        let registry = KeyRegistry::new();
        assert!(!registry.is_unlocked("vault:private"));

        let key = KeyMaterial::from_32_bytes([7u8; 32]);
        registry.register_key("vault:private", key);
        assert!(registry.is_unlocked("vault:private"));

        let retrieved = registry.get_key("vault:private");
        assert!(retrieved.is_some());

        assert!(registry.lock_key("vault:private"));
        assert!(!registry.is_unlocked("vault:private"));
        assert!(registry.get_key("vault:private").is_none());
    }
}
