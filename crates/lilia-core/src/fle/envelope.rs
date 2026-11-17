//! Encoders, decoders, and JSON document transformers for FLE envelopes.

use base64::Engine;
use serde_json::Value;

use crate::fle::cipher::{decrypt_gcm, decrypt_siv, encrypt_gcm, encrypt_siv};
use crate::fle::keys::KeyRegistry;
use crate::fle::policy::{FieldCipherAlgorithm, FlePolicy};
use crate::{ErrorCode, LiliaError, Result};

pub const ENC_PREFIX: &str = "ENC:";

/// Mode of handling fields whose compartment keys are locked during document decryption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DecryptionMode {
    /// Leaves locked fields intact as their `ENC:...` sealed ciphertext strings.
    #[default]
    Tolerant,
    /// Returns an error if any encrypted field cannot be decrypted.
    Strict,
    /// Replaces locked fields with `Value::Null`.
    Redact,
}

/// Parsed Field-Level Encryption envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedEnvelope {
    pub algorithm: FieldCipherAlgorithm,
    pub key_id: String,
    pub ciphertext: Vec<u8>,
}

impl EncryptedEnvelope {
    #[must_use]
    pub fn new(
        algorithm: FieldCipherAlgorithm,
        key_id: impl Into<String>,
        ciphertext: Vec<u8>,
    ) -> Self {
        Self {
            algorithm,
            key_id: key_id.into(),
            ciphertext,
        }
    }

    /// Encodes this envelope to the standard scalar string format: `ENC:<algo>:<kid>:<base64>`.
    #[must_use]
    pub fn encode(&self) -> String {
        let b64 = base64::engine::general_purpose::STANDARD.encode(&self.ciphertext);
        format!("{}{}:{}:{}", ENC_PREFIX, self.algorithm.as_str(), self.key_id, b64)
    }

    /// Parses an envelope scalar string if it matches `ENC:<algo>:<kid>:<base64>`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        if !s.starts_with(ENC_PREFIX) {
            return None;
        }
        let rest = &s[ENC_PREFIX.len()..];
        let (algorithm, after_algo) = if let Some(stripped) = rest.strip_prefix("gcm:") {
            (FieldCipherAlgorithm::RandomizedGcm, stripped)
        } else if let Some(stripped) = rest.strip_prefix("siv:") {
            (FieldCipherAlgorithm::DeterministicSiv, stripped)
        } else {
            return None;
        };

        // Key ID can contain colons (e.g. "vault:team"), so split at the last colon for base64 payload
        let last_colon_idx = after_algo.rfind(':')?;
        let key_id = after_algo[..last_colon_idx].to_string();
        if key_id.is_empty() {
            return None;
        }
        let b64_str = &after_algo[last_colon_idx + 1..];
        let ciphertext = base64::engine::general_purpose::STANDARD
            .decode(b64_str)
            .ok()?;

        Some(Self {
            algorithm,
            key_id,
            ciphertext,
        })
    }
}

/// Helper to navigate and retrieve a mutable reference to a value by simple path or slash/dot path.
fn get_field_mut<'a>(val: &'a mut Value, path: &str) -> Option<&'a mut Value> {
    let norm = path.trim_start_matches("$.").trim_start_matches('/');
    if norm.contains('.') || norm.contains('/') {
        let segs: Vec<&str> = norm.split(['.', '/']).collect();
        let mut curr = val;
        for seg in segs {
            match curr {
                Value::Object(map) => {
                    curr = map.get_mut(seg)?;
                }
                _ => return None,
            }
        }
        Some(curr)
    } else {
        val.as_object_mut()?.get_mut(norm)
    }
}

/// Applies Field-Level Encryption to the specified fields of a JSON document.
///
/// Plaintext field values are encrypted according to `policy` and replaced with
/// `ENC:<algo>:<kid>:<base64>` scalar strings.
pub fn encrypt_json_fields(
    doc: &mut Value,
    policy: &FlePolicy,
    keys: &KeyRegistry,
) -> Result<()> {
    for rule in &policy.rules {
        let norm_path = rule.path.trim_start_matches("$.").trim_start_matches('/');
        if let Some(target) = get_field_mut(doc, norm_path) {
            // Skip if already an encrypted envelope
            if let Some(s) = target.as_str() {
                if s.starts_with(ENC_PREFIX) {
                    continue;
                }
            }

            let key_material = keys.get_key(&rule.key_id).ok_or_else(|| {
                LiliaError::new(
                    ErrorCode::Unauthorized,
                    format!(
                        "cannot encrypt field '{}': compartment key '{}' is locked or not found",
                        rule.path, rule.key_id
                    ),
                    false,
                )
            })?;

            let plain_bytes = serde_json::to_vec(target).map_err(|e| {
                LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("failed to serialize field '{}' to json: {e}", rule.path),
                    false,
                )
            })?;

            let ad = if rule.bind_path_as_ad {
                Some(norm_path.as_bytes())
            } else {
                None
            };

            let cipher_bytes = match rule.algorithm {
                FieldCipherAlgorithm::RandomizedGcm => {
                    encrypt_gcm(&plain_bytes, key_material.gcm_key(), ad)?
                }
                FieldCipherAlgorithm::DeterministicSiv => {
                    encrypt_siv(&plain_bytes, key_material.siv_key(), ad)?
                }
            };

            let envelope_str = EncryptedEnvelope::new(rule.algorithm, &rule.key_id, cipher_bytes).encode();
            *target = Value::String(envelope_str);
        }
    }
    Ok(())
}

/// Recursively decrypts all `ENC:...` sealed fields in a JSON document.
pub fn decrypt_json_fields(
    doc: &mut Value,
    keys: &KeyRegistry,
    mode: DecryptionMode,
) -> Result<()> {
    decrypt_value_recursive(doc, None, keys, mode)
}

fn decrypt_value_recursive(
    val: &mut Value,
    field_name: Option<&str>,
    keys: &KeyRegistry,
    mode: DecryptionMode,
) -> Result<()> {
    match val {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                decrypt_value_recursive(v, Some(k), keys, mode)?;
            }
            Ok(())
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                decrypt_value_recursive(item, field_name, keys, mode)?;
            }
            Ok(())
        }
        Value::String(s) => {
            if !s.starts_with(ENC_PREFIX) {
                return Ok(());
            }

            let Some(env) = EncryptedEnvelope::parse(s) else {
                return Ok(());
            };

            let Some(key_material) = keys.get_key(&env.key_id) else {
                match mode {
                    DecryptionMode::Tolerant => return Ok(()),
                    DecryptionMode::Strict => {
                        return Err(LiliaError::new(
                            ErrorCode::Unauthorized,
                            format!("compartment '{}' is locked", env.key_id),
                            false,
                        ));
                    }
                    DecryptionMode::Redact => {
                        *val = Value::Null;
                        return Ok(());
                    }
                }
            };

            let ad_cand = field_name.map(str::as_bytes);

            let decrypted_bytes = match env.algorithm {
                FieldCipherAlgorithm::RandomizedGcm => {
                    // Try with field name AD first, then without AD if failed
                    if let Some(ad) = ad_cand {
                        decrypt_gcm(&env.ciphertext, key_material.gcm_key(), Some(ad))
                            .or_else(|_| decrypt_gcm(&env.ciphertext, key_material.gcm_key(), None))?
                    } else {
                        decrypt_gcm(&env.ciphertext, key_material.gcm_key(), None)?
                    }
                }
                FieldCipherAlgorithm::DeterministicSiv => {
                    if let Some(ad) = ad_cand {
                        decrypt_siv(&env.ciphertext, key_material.siv_key(), Some(ad))
                            .or_else(|_| decrypt_siv(&env.ciphertext, key_material.siv_key(), None))?
                    } else {
                        decrypt_siv(&env.ciphertext, key_material.siv_key(), None)?
                    }
                }
            };

            let restored_val: Value = serde_json::from_slice(&decrypted_bytes).map_err(|e| {
                LiliaError::new(
                    ErrorCode::Corrupt,
                    format!("failed to deserialize decrypted field to json: {e}"),
                    false,
                )
            })?;

            *val = restored_val;
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fle::keys::KeyMaterial;
    use serde_json::json;

    #[test]
    fn test_envelope_parse_encode() {
        let env = EncryptedEnvelope::new(
            FieldCipherAlgorithm::DeterministicSiv,
            "vault:team",
            vec![1, 2, 3, 4, 5],
        );
        let s = env.encode();
        assert!(s.starts_with("ENC:siv:vault:team:"));
        let parsed = EncryptedEnvelope::parse(&s).unwrap();
        assert_eq!(parsed, env);
    }

    #[test]
    fn test_encrypt_decrypt_doc_flow() {
        let keys = KeyRegistry::new();
        keys.register_key("vault:team", KeyMaterial::from_32_bytes([1u8; 32]));
        keys.register_key("vault:private", KeyMaterial::from_32_bytes([2u8; 32]));

        let policy = FlePolicy::new()
            .field("email", FieldCipherAlgorithm::DeterministicSiv, "vault:team")
            .field("medical_record", FieldCipherAlgorithm::RandomizedGcm, "vault:private");

        let mut doc = json!({
            "id": "pat_123",
            "created_at": 1715000000,
            "status": "admitted",
            "email": "alice@example.com",
            "medical_record": {
                "diagnosis": "Asthma",
                "score": 42
            }
        });

        // 1. Encrypt fields
        encrypt_json_fields(&mut doc, &policy, &keys).unwrap();

        // Metadata in cleartext
        assert_eq!(doc["id"], "pat_123");
        assert_eq!(doc["created_at"], 1715000000);
        assert_eq!(doc["status"], "admitted");

        // Sensitive fields are now ENC strings
        let email_str = doc["email"].as_str().unwrap();
        assert!(email_str.starts_with("ENC:siv:vault:team:"));

        let med_str = doc["medical_record"].as_str().unwrap();
        assert!(med_str.starts_with("ENC:gcm:vault:private:"));

        // 2. Lock private vault, verify tolerant decryption
        keys.lock_key("vault:private");
        let mut partially_decrypted = doc.clone();
        decrypt_json_fields(&mut partially_decrypted, &keys, DecryptionMode::Tolerant).unwrap();

        // Email was decrypted because vault:team is unlocked!
        assert_eq!(partially_decrypted["email"], "alice@example.com");
        // Medical record remains sealed!
        assert!(partially_decrypted["medical_record"].as_str().unwrap().starts_with("ENC:gcm:vault:private:"));

        // 3. Unlock private vault, verify full decryption
        keys.register_key("vault:private", KeyMaterial::from_32_bytes([2u8; 32]));
        let mut fully_decrypted = doc.clone();
        decrypt_json_fields(&mut fully_decrypted, &keys, DecryptionMode::Tolerant).unwrap();

        assert_eq!(fully_decrypted["email"], "alice@example.com");
        assert_eq!(fully_decrypted["medical_record"]["diagnosis"], "Asthma");
        assert_eq!(fully_decrypted["medical_record"]["score"], 42);
    }
}
