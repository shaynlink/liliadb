#![allow(
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]

use std::sync::Arc;

use lilia_core::{
    CollectionKind, CollectionSpec, Durability, ErrorCode, EvictionPolicy, LiliaError, Result,
    SqlQueryResult, StorageTier, ValueCodec,
};
use lilia_sqlite_native::{ffi, NativeConnection, NativeStatement};

#[allow(clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
pub(crate) fn register_sql_functions(
    conn: &NativeConnection,
    codec: &Arc<dyn ValueCodec>,
    dict_registry: &Arc<lilia_core::DictionaryRegistry>,
    key_registry: &Arc<lilia_core::KeyRegistry>,
    wasm_registry: &Arc<crate::wasm_udf::WasmUdfRegistry>,
) -> Result<()> {
    conn.register_scalar_function("current_context", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let key = args[0].as_text();
        if let Some(sec_ctx) = crate::security::get_active_context() {
            if let Some(val) = sec_ctx.get(key) {
                ctx.set_result_text(val);
                return Ok(());
            }
        }
        ctx.set_result_null();
        Ok(())
    })?;

    conn.register_scalar_function("has_role", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_int64(0);
            return Ok(());
        }
        let role = args[0].as_text();
        if let Some(sec_ctx) = crate::security::get_active_context() {
            ctx.set_result_int64(i64::from(sec_ctx.has_role(role)));
        } else {
            ctx.set_result_int64(0);
        }
        Ok(())
    })?;

    let codec_kv = Arc::clone(codec);
    conn.register_scalar_function("lilia_decode_kv", 1, move |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let bytes = args[0].as_blob();
        match codec_kv.decode(bytes) {
            Ok(dec) => {
                ctx.set_result_blob(&dec);
                Ok(())
            }
            Err(e) => Err(LiliaError::new(ErrorCode::Storage, e.to_string(), false)),
        }
    })?;

    let codec_json = Arc::clone(codec);
    let dict_reg_decode = Arc::clone(dict_registry);
    let key_reg_decode = Arc::clone(key_registry);
    conn.register_scalar_function("lilia_decode_json", 1, move |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }

        if args[0].is_blob() {
            let blob = args[0].as_blob();
            if blob.starts_with(b"LBJ\x01") {
                if let Ok(root) = lilia_core::json::BinaryDoc::read_root_from_slice(blob) {
                    let mut val = root.to_json_value();
                    let _ = lilia_core::decrypt_json_fields(
                        &mut val,
                        &key_reg_decode,
                        lilia_core::DecryptionMode::Tolerant,
                    );
                    let json_str =
                        serde_json::to_string(&val).unwrap_or_else(|_| "null".to_string());
                    ctx.set_result_text(&json_str);
                    return Ok(());
                }
            } else if blob.starts_with(b"LBZ\x01") && blob.len() >= 8 {
                let dict_id = u32::from_le_bytes(blob[4..8].try_into().unwrap());
                if let Some(dict) = dict_reg_decode.get_by_id(dict_id) {
                    if let Ok(decompressed) = lilia_core::compression::dict::decompress_with_dict(
                        &blob[8..],
                        &dict.raw_bytes,
                    ) {
                        if let Ok(root) =
                            lilia_core::json::BinaryDoc::read_root_from_slice(&decompressed)
                        {
                            let mut val = root.to_json_value();
                            let _ = lilia_core::decrypt_json_fields(
                                &mut val,
                                &key_reg_decode,
                                lilia_core::DecryptionMode::Tolerant,
                            );
                            let json_str =
                                serde_json::to_string(&val).unwrap_or_else(|_| "null".to_string());
                            ctx.set_result_text(&json_str);
                            return Ok(());
                        }
                    }
                }
            }
            // Decrypt with codec if not identity
            if let Ok(plain) = codec_json.decode(blob) {
                if plain.starts_with(b"LBJ\x01") {
                    if let Ok(root) = lilia_core::json::BinaryDoc::read_root_from_slice(&plain) {
                        let mut val = root.to_json_value();
                        let _ = lilia_core::decrypt_json_fields(
                            &mut val,
                            &key_reg_decode,
                            lilia_core::DecryptionMode::Tolerant,
                        );
                        let json_str =
                            serde_json::to_string(&val).unwrap_or_else(|_| "null".to_string());
                        ctx.set_result_text(&json_str);
                        return Ok(());
                    }
                }
            }
            ctx.set_result_null();
            return Ok(());
        }

        let text = args[0].as_text();
        let mut resolved_str = text.to_string();

        if text.starts_with("{\"$codec\":") {
            if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(text) {
                if let Some(b64) = json_val.get("$data").and_then(serde_json::Value::as_str) {
                    use base64::Engine;
                    if let Ok(ciphertext) = base64::engine::general_purpose::STANDARD.decode(b64) {
                        if let Ok(plaintext) = codec_json.decode(&ciphertext) {
                            if let Ok(plain_str) = String::from_utf8(plaintext) {
                                resolved_str = plain_str;
                            }
                        }
                    }
                }
            }
        }

        if resolved_str.contains(lilia_core::ENC_PREFIX) {
            if let Ok(mut json_val) = serde_json::from_str::<serde_json::Value>(&resolved_str) {
                let _ = lilia_core::decrypt_json_fields(
                    &mut json_val,
                    &key_reg_decode,
                    lilia_core::DecryptionMode::Tolerant,
                );
                if let Ok(dec_str) = serde_json::to_string(&json_val) {
                    ctx.set_result_text(&dec_str);
                    return Ok(());
                }
            }
        }

        ctx.set_result_text(&resolved_str);
        Ok(())
    })?;

    let key_reg_siv_enc = Arc::clone(key_registry);
    conn.register_scalar_function("lilia_encrypt_siv", -1, move |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let plaintext = args[0].as_text();
        let key_id = args[1].as_text();
        let path = if args.len() >= 3 && !args[2].is_null() {
            Some(
                args[2]
                    .as_text()
                    .trim_start_matches("$.")
                    .trim_start_matches('/'),
            )
        } else {
            None
        };
        let Some(key) = key_reg_siv_enc.get_key(key_id) else {
            return Err(LiliaError::new(
                ErrorCode::Unauthorized,
                format!("compartment key '{key_id}' not found or locked"),
                false,
            ));
        };
        let plain_bytes = serde_json::to_vec(plaintext)
            .map_err(|e| LiliaError::new(ErrorCode::InvalidInput, e.to_string(), false))?;
        let ad = path.map(str::as_bytes);
        let ct = lilia_core::encrypt_siv(&plain_bytes, key.siv_key(), ad)?;
        let env_str = lilia_core::EncryptedEnvelope::new(
            lilia_core::FieldCipherAlgorithm::DeterministicSiv,
            key_id,
            ct,
        )
        .encode();
        ctx.set_result_text(&env_str);
        Ok(())
    })?;

    let key_reg_siv_dec = Arc::clone(key_registry);
    conn.register_scalar_function("lilia_decrypt_siv", -1, move |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let ct_str = args[0].as_text();
        let key_id = args[1].as_text();
        let path = if args.len() >= 3 && !args[2].is_null() {
            Some(
                args[2]
                    .as_text()
                    .trim_start_matches("$.")
                    .trim_start_matches('/'),
            )
        } else {
            None
        };
        let Some(env) = lilia_core::EncryptedEnvelope::parse(ct_str) else {
            ctx.set_result_text(ct_str);
            return Ok(());
        };
        let Some(key) = key_reg_siv_dec.get_key(key_id) else {
            return Err(LiliaError::new(
                ErrorCode::Unauthorized,
                format!("compartment key '{key_id}' not found or locked"),
                false,
            ));
        };
        let ad = path.map(str::as_bytes);
        let plain_bytes = lilia_core::decrypt_siv(&env.ciphertext, key.siv_key(), ad)?;
        if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&plain_bytes) {
            if let Some(s) = val.as_str() {
                ctx.set_result_text(s);
            } else {
                ctx.set_result_text(&val.to_string());
            }
        } else if let Ok(s) = String::from_utf8(plain_bytes) {
            ctx.set_result_text(&s);
        } else {
            ctx.set_result_null();
        }
        Ok(())
    })?;

    let key_reg_gcm_enc = Arc::clone(key_registry);
    conn.register_scalar_function("lilia_encrypt_gcm", -1, move |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let plaintext = args[0].as_text();
        let key_id = args[1].as_text();
        let path = if args.len() >= 3 && !args[2].is_null() {
            Some(
                args[2]
                    .as_text()
                    .trim_start_matches("$.")
                    .trim_start_matches('/'),
            )
        } else {
            None
        };
        let Some(key) = key_reg_gcm_enc.get_key(key_id) else {
            return Err(LiliaError::new(
                ErrorCode::Unauthorized,
                format!("compartment key '{key_id}' not found or locked"),
                false,
            ));
        };
        let plain_bytes = serde_json::to_vec(plaintext)
            .map_err(|e| LiliaError::new(ErrorCode::InvalidInput, e.to_string(), false))?;
        let ad = path.map(str::as_bytes);
        let ct = lilia_core::encrypt_gcm(&plain_bytes, key.gcm_key(), ad)?;
        let env_str = lilia_core::EncryptedEnvelope::new(
            lilia_core::FieldCipherAlgorithm::RandomizedGcm,
            key_id,
            ct,
        )
        .encode();
        ctx.set_result_text(&env_str);
        Ok(())
    })?;

    let key_reg_gcm_dec = Arc::clone(key_registry);
    conn.register_scalar_function("lilia_decrypt_gcm", -1, move |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let ct_str = args[0].as_text();
        let key_id = args[1].as_text();
        let path = if args.len() >= 3 && !args[2].is_null() {
            Some(
                args[2]
                    .as_text()
                    .trim_start_matches("$.")
                    .trim_start_matches('/'),
            )
        } else {
            None
        };
        let Some(env) = lilia_core::EncryptedEnvelope::parse(ct_str) else {
            ctx.set_result_text(ct_str);
            return Ok(());
        };
        let Some(key) = key_reg_gcm_dec.get_key(key_id) else {
            return Err(LiliaError::new(
                ErrorCode::Unauthorized,
                format!("compartment key '{key_id}' not found or locked"),
                false,
            ));
        };
        let ad = path.map(str::as_bytes);
        let plain_bytes = lilia_core::decrypt_gcm(&env.ciphertext, key.gcm_key(), ad)?;
        if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&plain_bytes) {
            if let Some(s) = val.as_str() {
                ctx.set_result_text(s);
            } else {
                ctx.set_result_text(&val.to_string());
            }
        } else if let Ok(s) = String::from_utf8(plain_bytes) {
            ctx.set_result_text(&s);
        } else {
            ctx.set_result_null();
        }
        Ok(())
    })?;

    let key_reg_fle_dec = Arc::clone(key_registry);
    conn.register_scalar_function("lilia_fle_decrypt", 1, move |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let text = args[0].as_text();
        if let Some(env) = lilia_core::EncryptedEnvelope::parse(text) {
            if env.algorithm == lilia_core::FieldCipherAlgorithm::HomomorphicPaillier {
                if let (Some(sk), Some(pk)) = (
                    key_reg_fle_dec.get_paillier_private(&env.key_id),
                    key_reg_fle_dec.get_paillier_public(&env.key_id),
                ) {
                    if let Ok(ct) = lilia_core::PaillierCiphertext::from_bytes(&env.ciphertext) {
                        if ct.scale() == 0 {
                            if let Ok(i) = lilia_core::decrypt_paillier_i64(&sk, &pk, &ct) {
                                ctx.set_result_int64(i);
                                return Ok(());
                            }
                        } else if let Ok(f) = lilia_core::decrypt_paillier_f64(&sk, &pk, &ct) {
                            ctx.set_result_double(f);
                            return Ok(());
                        }
                    }
                }
                ctx.set_result_text(text);
                return Ok(());
            }

            if let Some(key) = key_reg_fle_dec.get_key(&env.key_id) {
                let plain = match env.algorithm {
                    lilia_core::FieldCipherAlgorithm::RandomizedGcm => {
                        lilia_core::decrypt_gcm(&env.ciphertext, key.gcm_key(), None)
                    }
                    lilia_core::FieldCipherAlgorithm::DeterministicSiv => {
                        lilia_core::decrypt_siv(&env.ciphertext, key.siv_key(), None)
                    }
                    lilia_core::FieldCipherAlgorithm::HomomorphicPaillier => unreachable!(),
                };
                if let Ok(plain_bytes) = plain {
                    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&plain_bytes) {
                        if let Some(s) = val.as_str() {
                            ctx.set_result_text(s);
                        } else {
                            ctx.set_result_text(&val.to_string());
                        }
                    } else if let Ok(s) = String::from_utf8(plain_bytes) {
                        ctx.set_result_text(&s);
                    } else {
                        ctx.set_result_text(text);
                    }
                    return Ok(());
                }
            }
            ctx.set_result_text(text);
        } else if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(text) {
            let _ = lilia_core::decrypt_json_fields(
                &mut val,
                &key_reg_fle_dec,
                lilia_core::DecryptionMode::Tolerant,
            );
            let s = serde_json::to_string(&val).unwrap_or_else(|_| text.to_string());
            ctx.set_result_text(&s);
        } else {
            ctx.set_result_text(text);
        }
        Ok(())
    })?;

    let key_reg_p_enc = Arc::clone(key_registry);
    conn.register_scalar_function("lilia_paillier_encrypt", -1, move |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let comp_id = args[1].as_text();
        let Some(pk) = key_reg_p_enc.get_paillier_public(comp_id) else {
            return Err(LiliaError::new(
                ErrorCode::Unauthorized,
                format!("compartment '{comp_id}' Paillier public key not found"),
                false,
            ));
        };
        let scale = if args.len() >= 3 && !args[2].is_null() {
            args[2].as_int64() as u8
        } else {
            0
        };

        let ct = if scale > 0 {
            let val = args[0].as_double();
            lilia_core::encrypt_paillier_f64(&pk, val, scale)
        } else {
            let val = args[0].as_int64();
            lilia_core::encrypt_paillier_i64(&pk, val)
        };
        let env_str = lilia_core::EncryptedEnvelope::new(
            lilia_core::FieldCipherAlgorithm::HomomorphicPaillier,
            comp_id,
            ct.to_bytes(),
        )
        .encode();
        ctx.set_result_text(&env_str);
        Ok(())
    })?;

    let key_reg_p_dec = Arc::clone(key_registry);
    conn.register_scalar_function("lilia_paillier_decrypt", -1, move |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let text = args[0].as_text();
        let comp_id = if args.len() >= 2 && !args[1].is_null() {
            Some(args[1].as_text())
        } else {
            None
        };

        let Some(env) = lilia_core::EncryptedEnvelope::parse(text) else {
            ctx.set_result_text(text);
            return Ok(());
        };
        let target_comp = comp_id.unwrap_or(&env.key_id);

        let Some(sk) = key_reg_p_dec.get_paillier_private(target_comp) else {
            return Err(LiliaError::new(
                ErrorCode::Unauthorized,
                format!("compartment '{target_comp}' Paillier private key is locked"),
                false,
            ));
        };
        let Some(pk) = key_reg_p_dec.get_paillier_public(target_comp) else {
            return Err(LiliaError::new(
                ErrorCode::Unauthorized,
                format!("compartment '{target_comp}' Paillier public key not found"),
                false,
            ));
        };

        let ct = lilia_core::PaillierCiphertext::from_bytes(&env.ciphertext)?;
        if ct.scale() == 0 {
            let val = lilia_core::decrypt_paillier_i64(&sk, &pk, &ct)?;
            ctx.set_result_int64(val);
        } else {
            let val = lilia_core::decrypt_paillier_f64(&sk, &pk, &ct)?;
            ctx.set_result_double(val);
        }
        Ok(())
    })?;

    let key_reg_p_add = Arc::clone(key_registry);
    conn.register_scalar_function("lilia_paillier_add", 2, move |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let t1 = args[0].as_text();
        let t2 = args[1].as_text();
        let (Some(env1), Some(env2)) = (
            lilia_core::EncryptedEnvelope::parse(t1),
            lilia_core::EncryptedEnvelope::parse(t2),
        ) else {
            ctx.set_result_null();
            return Ok(());
        };
        let Some(pk) = key_reg_p_add.get_paillier_public(&env1.key_id) else {
            return Err(LiliaError::new(
                ErrorCode::Unauthorized,
                format!(
                    "compartment '{}' Paillier public key not found",
                    env1.key_id
                ),
                false,
            ));
        };
        let ct1 = lilia_core::PaillierCiphertext::from_bytes(&env1.ciphertext)?;
        let ct2 = lilia_core::PaillierCiphertext::from_bytes(&env2.ciphertext)?;
        let res_ct = lilia_core::paillier_ops::add(&pk, &ct1, &ct2)?;
        let env_str = lilia_core::EncryptedEnvelope::new(
            lilia_core::FieldCipherAlgorithm::HomomorphicPaillier,
            &env1.key_id,
            res_ct.to_bytes(),
        )
        .encode();
        ctx.set_result_text(&env_str);
        Ok(())
    })?;

    let key_reg_p_sub = Arc::clone(key_registry);
    conn.register_scalar_function("lilia_paillier_sub", 2, move |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let t1 = args[0].as_text();
        let t2 = args[1].as_text();
        let (Some(env1), Some(env2)) = (
            lilia_core::EncryptedEnvelope::parse(t1),
            lilia_core::EncryptedEnvelope::parse(t2),
        ) else {
            ctx.set_result_null();
            return Ok(());
        };
        let Some(pk) = key_reg_p_sub.get_paillier_public(&env1.key_id) else {
            return Err(LiliaError::new(
                ErrorCode::Unauthorized,
                format!(
                    "compartment '{}' Paillier public key not found",
                    env1.key_id
                ),
                false,
            ));
        };
        let ct1 = lilia_core::PaillierCiphertext::from_bytes(&env1.ciphertext)?;
        let ct2 = lilia_core::PaillierCiphertext::from_bytes(&env2.ciphertext)?;
        let res_ct = lilia_core::paillier_ops::sub(&pk, &ct1, &ct2)?;
        let env_str = lilia_core::EncryptedEnvelope::new(
            lilia_core::FieldCipherAlgorithm::HomomorphicPaillier,
            &env1.key_id,
            res_ct.to_bytes(),
        )
        .encode();
        ctx.set_result_text(&env_str);
        Ok(())
    })?;

    let key_reg_p_mul = Arc::clone(key_registry);
    conn.register_scalar_function("lilia_paillier_mul", 2, move |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let t = args[0].as_text();
        let scalar = args[1].as_int64();
        let Some(env) = lilia_core::EncryptedEnvelope::parse(t) else {
            ctx.set_result_null();
            return Ok(());
        };
        let Some(pk) = key_reg_p_mul.get_paillier_public(&env.key_id) else {
            return Err(LiliaError::new(
                ErrorCode::Unauthorized,
                format!("compartment '{}' Paillier public key not found", env.key_id),
                false,
            ));
        };
        let ct = lilia_core::PaillierCiphertext::from_bytes(&env.ciphertext)?;
        let res_ct = lilia_core::paillier_ops::mul_scalar(&pk, &ct, scalar)?;
        let env_str = lilia_core::EncryptedEnvelope::new(
            lilia_core::FieldCipherAlgorithm::HomomorphicPaillier,
            &env.key_id,
            res_ct.to_bytes(),
        )
        .encode();
        ctx.set_result_text(&env_str);
        Ok(())
    })?;

    let key_reg_p_sum = Arc::clone(key_registry);
    conn.register_aggregate_function(
        "lilia_paillier_sum",
        1,
        move |ctx, args| {
            if args.is_empty() || args[0].is_null() {
                return Ok(());
            }
            let text = args[0].as_text();
            if !text.starts_with("ENC:paillier:") {
                return Ok(());
            }
            let Some(env) = lilia_core::EncryptedEnvelope::parse(text) else {
                return Ok(());
            };
            let Some(acc) = ctx
                .get_aggregate_context::<Option<(lilia_core::HomomorphicSumAccumulator, String)>>()
            else {
                return Ok(());
            };
            if acc.is_none() {
                if let Some(pk) = key_reg_p_sum.get_paillier_public(&env.key_id) {
                    *acc = Some((
                        lilia_core::HomomorphicSumAccumulator::new(pk),
                        env.key_id.clone(),
                    ));
                }
            }
            if let Some((ref mut h_acc, _)) = acc {
                let _ = h_acc.update_envelope(&env);
            }
            Ok(())
        },
        move |ctx| {
            let Some(state) = ctx
                .take_aggregate_context::<Option<(lilia_core::HomomorphicSumAccumulator, String)>>(
                )
            else {
                ctx.set_result_null();
                return Ok(());
            };
            if let Some((h_acc, key_id)) = state {
                let env_str = h_acc.to_envelope(&key_id);
                ctx.set_result_text(&env_str);
            } else {
                ctx.set_result_null();
            }
            Ok(())
        },
    )?;

    let dict_reg_extract = Arc::clone(dict_registry);
    conn.register_scalar_function("lilia_extract", 2, move |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let path = args[1].as_text();
        if args[0].is_blob() {
            let blob = args[0].as_blob();
            if blob.starts_with(b"LBJ\x01") {
                if let Ok(root) = lilia_core::json::BinaryDoc::read_root_from_slice(blob) {
                    if let Some(target) = root.extract(path) {
                        match target {
                            lilia_core::json::BinaryValueRef::Null => ctx.set_result_null(),
                            lilia_core::json::BinaryValueRef::Bool(b) => {
                                ctx.set_result_int64(i64::from(b))
                            }
                            lilia_core::json::BinaryValueRef::Int64(i) => ctx.set_result_int64(i),
                            lilia_core::json::BinaryValueRef::Float64(f) => {
                                ctx.set_result_double(f)
                            }
                            lilia_core::json::BinaryValueRef::String(s) => ctx.set_result_text(s),
                            _ => {
                                let val = target.to_json_value();
                                let s = serde_json::to_string(&val)
                                    .unwrap_or_else(|_| "null".to_string());
                                ctx.set_result_text(&s);
                            }
                        }
                        return Ok(());
                    }
                }
            } else if blob.starts_with(b"LBZ\x01") && blob.len() >= 8 {
                let dict_id = u32::from_le_bytes(blob[4..8].try_into().unwrap());
                if let Some(dict) = dict_reg_extract.get_by_id(dict_id) {
                    if let Ok(decompressed) = lilia_core::compression::dict::decompress_with_dict(
                        &blob[8..],
                        &dict.raw_bytes,
                    ) {
                        if let Ok(root) =
                            lilia_core::json::BinaryDoc::read_root_from_slice(&decompressed)
                        {
                            if let Some(target) = root.extract(path) {
                                match target {
                                    lilia_core::json::BinaryValueRef::Null => ctx.set_result_null(),
                                    lilia_core::json::BinaryValueRef::Bool(b) => {
                                        ctx.set_result_int64(i64::from(b))
                                    }
                                    lilia_core::json::BinaryValueRef::Int64(i) => {
                                        ctx.set_result_int64(i)
                                    }
                                    lilia_core::json::BinaryValueRef::Float64(f) => {
                                        ctx.set_result_double(f)
                                    }
                                    lilia_core::json::BinaryValueRef::String(s) => {
                                        ctx.set_result_text(s)
                                    }
                                    _ => {
                                        let val = target.to_json_value();
                                        let s = serde_json::to_string(&val)
                                            .unwrap_or_else(|_| "null".to_string());
                                        ctx.set_result_text(&s);
                                    }
                                }
                                return Ok(());
                            }
                        }
                    }
                }
            }
        } else if args[0].is_text() {
            let text = args[0].as_text();
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(text) {
                let ptr = if let Some(stripped) = path.strip_prefix("$.") {
                    format!("/{}", stripped.replace('.', "/"))
                } else if path.contains('.') {
                    format!("/{}", path.replace('.', "/"))
                } else if path.starts_with('/') {
                    path.to_string()
                } else {
                    format!("/{path}")
                };
                if let Some(target) = val.pointer(&ptr) {
                    match target {
                        serde_json::Value::Null => ctx.set_result_null(),
                        serde_json::Value::Bool(b) => ctx.set_result_int64(i64::from(*b)),
                        serde_json::Value::Number(n) => {
                            if let Some(i) = n.as_i64() {
                                ctx.set_result_int64(i);
                            } else if let Some(f) = n.as_f64() {
                                ctx.set_result_double(f);
                            } else {
                                ctx.set_result_text(&n.to_string());
                            }
                        }
                        serde_json::Value::String(s) => ctx.set_result_text(s),
                        _ => ctx.set_result_text(&target.to_string()),
                    }
                    return Ok(());
                }
            }
        }
        ctx.set_result_null();
        Ok(())
    })?;

    let codec_vec = Arc::clone(codec);
    conn.register_scalar_function("lilia_decode_vector", 1, move |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let bytes = args[0].as_blob();
        match codec_vec.decode(bytes) {
            Ok(dec) => {
                ctx.set_result_blob(&dec);
                Ok(())
            }
            Err(e) => Err(LiliaError::new(ErrorCode::Storage, e.to_string(), false)),
        }
    })?;

    conn.register_scalar_function("lilia_vec_distance_cosine", 2, |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let b1 = args[0].as_blob();
        let b2 = args[1].as_blob();
        let v1 = bytes_to_f32_vec(b1);
        let v2 = bytes_to_f32_vec(b2);
        let dist = lilia_core::quantization::cosine_distance_f32(&v1, &v2);
        ctx.set_result_double(dist);
        Ok(())
    })?;

    conn.register_scalar_function("lilia_vec_distance_l2", 2, |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let b1 = args[0].as_blob();
        let b2 = args[1].as_blob();
        let v1 = bytes_to_f32_vec(b1);
        let v2 = bytes_to_f32_vec(b2);
        let dist = lilia_core::quantization::l2_distance_f32(&v1, &v2);
        ctx.set_result_double(dist);
        Ok(())
    })?;

    conn.register_scalar_function("lilia_vec_distance_hamming", 2, |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let b1 = args[0].as_blob();
        let b2 = args[1].as_blob();
        let dist = lilia_core::quantization::hamming_distance_bits(b1, b2);
        ctx.set_result_int64(i64::from(dist));
        Ok(())
    })?;

    conn.register_scalar_function("lilia_vec_to_json", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let b = args[0].as_blob();
        let v = bytes_to_f32_vec(b);
        let s = serde_json::to_string(&v).unwrap_or_else(|_| "[]".to_string());
        ctx.set_result_text(&s);
        Ok(())
    })?;

    conn.register_scalar_function("lilia_vec_from_json", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let text = args[0].as_text();
        if let Ok(floats) = serde_json::from_str::<Vec<f32>>(text) {
            let mut bytes = Vec::with_capacity(floats.len() * 4);
            for f in floats {
                bytes.extend_from_slice(&f.to_le_bytes());
            }
            ctx.set_result_blob(&bytes);
        } else {
            ctx.set_result_null();
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_vec_quantize_sq8", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let b = args[0].as_blob();
        let f = bytes_to_f32_vec(b);
        let (q, _) = lilia_core::quantization::quantize_f32_to_i8(&f);
        let bytes: Vec<u8> = q.into_iter().map(i8::cast_unsigned).collect();
        ctx.set_result_blob(&bytes);
        Ok(())
    })?;

    conn.register_scalar_function("lilia_vec_quantize_bit", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let b = args[0].as_blob();
        let f = bytes_to_f32_vec(b);
        let bits = lilia_core::quantization::quantize_f32_to_bits(&f);
        ctx.set_result_blob(&bits);
        Ok(())
    })?;

    conn.register_scalar_function("lilia_vec_dimension", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let b = args[0].as_blob();
        let dim = (b.len() / 4) as i64;
        ctx.set_result_int64(dim);
        Ok(())
    })?;

    conn.register_scalar_function("lilia_geo_distance", 4, |ctx, args| {
        if args.len() < 4
            || args[0].is_null()
            || args[1].is_null()
            || args[2].is_null()
            || args[3].is_null()
        {
            ctx.set_result_null();
            return Ok(());
        }
        let lat1 = args[0].as_double();
        let lon1 = args[1].as_double();
        let lat2 = args[2].as_double();
        let lon2 = args[3].as_double();
        let dist = lilia_core::haversine_distance_meters(lat1, lon1, lat2, lon2);
        ctx.set_result_double(dist);
        Ok(())
    })?;

    conn.register_scalar_function("lilia_geo_circle", 4, |ctx, args| {
        if args.len() < 4
            || args[0].is_null()
            || args[1].is_null()
            || args[2].is_null()
            || args[3].is_null()
        {
            ctx.set_result_null();
            return Ok(());
        }
        let lon = args[0].as_double();
        let lat = args[1].as_double();
        let radius = args[2].as_double();
        let vertices = args[3].as_int64();
        let n_vert = if vertices <= 0 {
            16
        } else {
            usize::try_from(vertices).unwrap_or(16)
        };
        let poly_json = lilia_core::circle_to_geopoly_json(lon, lat, radius, n_vert);
        ctx.set_result_text(&poly_json);
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_lat_lng_to_cell", 3, |ctx, args| {
        if args.len() < 3 || args[0].is_null() || args[1].is_null() || args[2].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let lat = args[0].as_double();
        let lng = args[1].as_double();
        let res = args[2].as_int64();
        let res_u8 = match u8::try_from(res) {
            Ok(r) if r <= 15 => r,
            _ => {
                ctx.set_result_null();
                return Ok(());
            }
        };
        match lilia_core::h3::lat_lng_to_cell(lat, lng, res_u8) {
            Ok(cell) => ctx.set_result_int64(cell as i64),
            Err(_) => ctx.set_result_null(),
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_cell_to_lat", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let cell = args[0].as_int64() as u64;
        match lilia_core::h3::cell_to_lat_lng(cell) {
            Ok((lat, _)) => ctx.set_result_double(lat),
            Err(_) => ctx.set_result_null(),
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_cell_to_lng", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let cell = args[0].as_int64() as u64;
        match lilia_core::h3::cell_to_lat_lng(cell) {
            Ok((_, lng)) => ctx.set_result_double(lng),
            Err(_) => ctx.set_result_null(),
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_to_string", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let cell = args[0].as_int64() as u64;
        ctx.set_result_text(&lilia_core::h3::cell_to_string(cell));
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_from_string", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let s = args[0].as_text();
        match lilia_core::h3::string_to_cell(s) {
            Ok(cell) => ctx.set_result_int64(cell as i64),
            Err(_) => ctx.set_result_null(),
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_resolution", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let cell = args[0].as_int64() as u64;
        match lilia_core::h3::resolution(cell) {
            Ok(res) => ctx.set_result_int64(i64::from(res)),
            Err(_) => ctx.set_result_null(),
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_parent", 2, |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let cell = args[0].as_int64() as u64;
        let parent_res = args[1].as_int64();
        let p_res_u8 = match u8::try_from(parent_res) {
            Ok(r) if r <= 15 => r,
            _ => {
                ctx.set_result_null();
                return Ok(());
            }
        };
        match lilia_core::h3::cell_to_parent(cell, p_res_u8) {
            Ok(parent) => ctx.set_result_int64(parent as i64),
            Err(_) => ctx.set_result_null(),
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_distance", 2, |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let c1 = args[0].as_int64() as u64;
        let c2 = args[1].as_int64() as u64;
        match lilia_core::h3::grid_distance(c1, c2) {
            Ok(dist) => ctx.set_result_int64(dist),
            Err(_) => ctx.set_result_null(),
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_is_valid", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let cell = args[0].as_int64() as u64;
        ctx.set_result_int64(if lilia_core::h3::is_valid(cell) { 1 } else { 0 });
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_child_range_min", 2, |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let cell = args[0].as_int64() as u64;
        let c_res = match u8::try_from(args[1].as_int64()) {
            Ok(r) if r <= 15 => r,
            _ => {
                ctx.set_result_null();
                return Ok(());
            }
        };
        match lilia_core::h3::child_range(cell, c_res) {
            Ok((min_val, _)) => ctx.set_result_int64(min_val as i64),
            Err(_) => ctx.set_result_null(),
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_child_range_max", 2, |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let cell = args[0].as_int64() as u64;
        let c_res = match u8::try_from(args[1].as_int64()) {
            Ok(r) if r <= 15 => r,
            _ => {
                ctx.set_result_null();
                return Ok(());
            }
        };
        match lilia_core::h3::child_range(cell, c_res) {
            Ok((_, max_val)) => ctx.set_result_int64(max_val as i64),
            Err(_) => ctx.set_result_null(),
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_k_ring", 2, |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let cell = args[0].as_int64() as u64;
        let k = match u32::try_from(args[1].as_int64()) {
            Ok(v) => v,
            _ => {
                ctx.set_result_null();
                return Ok(());
            }
        };
        match lilia_core::h3::k_ring(cell, k) {
            Ok(neighbors) => {
                let json = serde_json::to_string(&neighbors).unwrap_or_else(|_| "[]".to_string());
                ctx.set_result_text(&json);
            }
            Err(_) => ctx.set_result_null(),
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_h3_to_boundary", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let cell = args[0].as_int64() as u64;
        match lilia_core::h3::cell_to_boundary(cell) {
            Ok(coords) => {
                let json = lilia_core::coords_to_geopoly_json(&coords);
                ctx.set_result_text(&json);
            }
            Err(_) => ctx.set_result_null(),
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_ts_decompress", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let blob = args[0].as_blob();
        if let Ok(points) = lilia_core::decompress_timeseries(blob) {
            let json = serde_json::to_string(&points).unwrap_or_else(|_| "[]".to_string());
            ctx.set_result_text(&json);
            Ok(())
        } else {
            ctx.set_result_null();
            Ok(())
        }
    })?;

    conn.register_scalar_function("lilia_ts_downsample", 3, |ctx, args| {
        if args.len() < 3 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let text = args[0].as_text();
        let bucket_ms = args[1].as_int64();
        let agg_str = args[2].as_text().to_lowercase();
        let agg = match agg_str.as_str() {
            "min" => lilia_core::DownsampleAggregation::Min,
            "max" => lilia_core::DownsampleAggregation::Max,
            "sum" => lilia_core::DownsampleAggregation::Sum,
            "first" => lilia_core::DownsampleAggregation::First,
            "last" => lilia_core::DownsampleAggregation::Last,
            "count" => lilia_core::DownsampleAggregation::Count,
            _ => lilia_core::DownsampleAggregation::Avg,
        };
        if let Ok(points) = serde_json::from_str::<Vec<lilia_core::TimeSeriesPoint>>(text) {
            let downsampled = lilia_core::downsample_points(&points, bucket_ms, agg);
            let json = serde_json::to_string(&downsampled).unwrap_or_else(|_| "[]".to_string());
            ctx.set_result_text(&json);
        } else {
            ctx.set_result_null();
        }
        Ok(())
    })?;

    conn.register_scalar_function("lilia_iso_date", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let ms = args[0].as_int64();
        let iso = lilia_core::format_ms_to_iso(ms);
        ctx.set_result_text(&iso);
        Ok(())
    })?;

    conn.register_scalar_function("lilia_to_timestamp", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let txt = args[0].as_text();
        match lilia_core::parse_iso_to_ms(txt) {
            Ok(ms) => {
                ctx.set_result_int64(ms);
                Ok(())
            }
            Err(e) => Err(LiliaError::new(
                ErrorCode::InvalidInput,
                e.to_string(),
                false,
            )),
        }
    })?;

    conn.register_scalar_function("lilia_crdt_json_extract", 2, |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let blob = args[0].as_blob();
        let path = args[1].as_text();
        if let Ok(raw) = lilia_core::compression::decompress_bytes(blob) {
            if let Ok(doc) = serde_json::from_slice::<lilia_core::crdt::LwwJsonDocument>(&raw) {
                if let Some(val) = doc.get_field(path) {
                    let json_str =
                        serde_json::to_string(val).unwrap_or_else(|_| "null".to_string());
                    ctx.set_result_text(&json_str);
                    return Ok(());
                }
            }
        }
        ctx.set_result_null();
        Ok(())
    })?;

    conn.register_scalar_function("lilia_compressed_match", 2, |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_int64(0);
            return Ok(());
        }
        let blob = args[0].as_blob();
        let target_word = args[1].as_text();

        if let Some(doc_mask) = lilia_core::compression::dict::extract_symbol_signature(blob) {
            let target_mask = lilia_core::compression::dict::hash_token_to_mask(target_word);
            let matches = (doc_mask & target_mask) == target_mask;
            ctx.set_result_int64(i64::from(matches));
            return Ok(());
        }

        if blob.starts_with(lilia_core::MAGIC) {
            let tokens = lilia_core::compression::dict::extract_tokens_from_bytes(blob);
            let matches = tokens.iter().any(|t| t.as_str() == target_word);
            ctx.set_result_int64(i64::from(matches));
            return Ok(());
        }

        // If uncompressed or legacy format without symbol mask, return 1 to avoid false negatives
        ctx.set_result_int64(1);
        Ok(())
    })?;

    conn.register_scalar_function("lilia_symbol_mask", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let blob = args[0].as_blob();
        if let Some(mask) = lilia_core::compression::dict::extract_symbol_signature(blob) {
            ctx.set_result_int64(mask as i64);
        } else {
            ctx.set_result_null();
        }
        Ok(())
    })?;

    // Wasm In-Query User-Defined Functions (wasm_call, wasm_register, wasm_unregister, wasm_list)
    let wasm_reg_call = Arc::clone(wasm_registry);
    conn.register_scalar_function("wasm_call", -1, move |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_error("wasm_call requires function name as first argument");
            return Ok(());
        }
        let func_name = args[0].as_text();

        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some(call_ctx) = ctx.get_auxdata::<crate::wasm_udf::WasmUdfCallContext>(0) {
                let user_args = if args.len() > 1 { &args[1..] } else { &[] };
                return call_ctx.invoke(ctx, user_args);
            }

            let Some(registered) = wasm_reg_call.get(func_name) else {
                ctx.set_result_error(&format!("wasm udf '{func_name}' is not registered"));
                return Ok(());
            };

            let engine = wasm_reg_call.engine();
            match crate::wasm_udf::WasmUdfCallContext::new(engine, &registered) {
                Ok(mut new_ctx) => {
                    let user_args = if args.len() > 1 { &args[1..] } else { &[] };
                    let res = new_ctx.invoke(ctx, user_args);
                    ctx.set_auxdata(0, Box::new(new_ctx));
                    res
                }
                Err(e) => {
                    ctx.set_result_error(&format!(
                        "failed to instantiate wasm udf '{func_name}': {e}"
                    ));
                    Ok(())
                }
            }
        }

        #[cfg(target_arch = "wasm32")]
        {
            ctx.set_result_error("wasm udfs are not supported on wasm32");
            Ok(())
        }
    })?;

    let wasm_reg_register = Arc::clone(wasm_registry);
    conn.register_scalar_function("wasm_register", -1, move |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_error("wasm_register requires at least 2 arguments: (name, bytecode_or_wat, [is_persistent, memory_limit, fuel_limit])");
            return Ok(());
        }

        let name = args[0].as_text();
        let bytecode = if args[1].is_blob() {
            args[1].as_blob()
        } else {
            args[1].as_text().as_bytes()
        };

        let mut config = crate::wasm_udf::WasmUdfConfig::default();
        if args.len() >= 4 && !args[3].is_null() {
            let mem = args[3].as_int64();
            if mem > 0 {
                config.memory_limit_bytes = mem as usize;
            }
        }
        if args.len() >= 5 && !args[4].is_null() {
            let fuel = args[4].as_int64();
            if fuel > 0 {
                config.fuel_limit = fuel as u64;
            }
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        match wasm_reg_register.register(name, bytecode, config, now) {
            Ok(_) => {
                ctx.set_result_int64(1);
            }
            Err(e) => {
                ctx.set_result_error(&e.to_string());
            }
        }
        Ok(())
    })?;

    let wasm_reg_unregister = Arc::clone(wasm_registry);
    conn.register_scalar_function("wasm_unregister", 1, move |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_int64(0);
            return Ok(());
        }
        let name = args[0].as_text();
        let removed = wasm_reg_unregister.unregister(name).is_some();
        ctx.set_result_int64(if removed { 1 } else { 0 });
        Ok(())
    })?;

    let wasm_reg_list = Arc::clone(wasm_registry);
    conn.register_scalar_function("wasm_list", 0, move |ctx, _args| {
        let list = wasm_reg_list.list();
        let json_str = serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_string());
        ctx.set_result_text(&json_str);
        Ok(())
    })?;

    for udf in wasm_registry.list() {
        register_direct_wasm_function(conn, wasm_registry, &udf.name)?;
    }

    #[cfg(feature = "zk")]
    {
        // zk_verify: validates a zk-SNARK proof bundle in SQL (< 1 ms)
        conn.register_scalar_function("zk_verify", -1, |ctx, args| {
            if args.is_empty() || args[0].is_null() {
                ctx.set_result_null();
                return Ok(());
            }
            let bundle: lilia_zk::ZkProofBundle = if args.len() == 1 {
                let json_text = args[0].as_text();
                match serde_json::from_str(json_text) {
                    Ok(b) => b,
                    Err(e) => {
                        ctx.set_result_error(&format!("Invalid ZkProofBundle JSON: {e}"));
                        return Ok(());
                    }
                }
            } else if args.len() >= 2 {
                let proof_hex = args[0].as_text().to_string();
                let pub_inputs_json = args[1].as_text();
                let inputs: Vec<String> = match serde_json::from_str(pub_inputs_json) {
                    Ok(i) => i,
                    Err(e) => {
                        ctx.set_result_error(&format!("Invalid public inputs JSON: {e}"));
                        return Ok(());
                    }
                };
                let commitment = inputs.first().cloned().unwrap_or_default();
                lilia_zk::ZkProofBundle {
                    proof_hex,
                    public_inputs: inputs,
                    commitment_hex: commitment,
                    circuit_tag: "solde_age_v1".to_string(),
                    verified_claims: vec![],
                    proof_size_bytes: 128,
                }
            } else {
                ctx.set_result_error(
                    "zk_verify requires either a bundle JSON or (proof_hex, public_inputs_json)",
                );
                return Ok(());
            };

            let vk = if args.len() >= 3 && !args[2].is_null() {
                match lilia_zk::hex_to_vk(args[2].as_text()) {
                    Ok(vk) => Some(vk),
                    Err(e) => {
                        ctx.set_result_error(&format!("Invalid VerifyingKey hex: {e}"));
                        return Ok(());
                    }
                }
            } else {
                None
            };

            match lilia_zk::ZkVerifier::verify(&bundle, vk.as_ref()) {
                Ok(report) => {
                    ctx.set_result_int64(if report.valid { 1 } else { 0 });
                }
                Err(e) => {
                    ctx.set_result_error(&format!("zk_verify error: {e}"));
                }
            }
            Ok(())
        })?;

        // zk_commit: computes Poseidon commitment anchor over (doc_id, balance, age, salt)
        conn.register_scalar_function("zk_commit", -1, |ctx, args| {
            if args.len() < 3 {
                ctx.set_result_error("zk_commit requires (doc_id, balance, age [, salt])");
                return Ok(());
            }
            let doc_id = args[0].as_text();
            let balance = args[1].as_int64() as u64;
            let age = args[2].as_int64() as u64;

            let doc_id_fr = lilia_zk::str_to_fr(doc_id);
            let balance_fr = lilia_zk::u64_to_fr(balance);
            let age_fr = lilia_zk::u64_to_fr(age);

            let salt_fr = if args.len() >= 4 && !args[3].is_null() {
                let s = args[3].as_text();
                lilia_zk::hex_to_fr(s).unwrap_or_else(|_| lilia_zk::str_to_fr(s))
            } else {
                lilia_zk::str_to_fr(doc_id)
            };

            let commitment = lilia_zk::poseidon_hash_4(doc_id_fr, balance_fr, age_fr, salt_fr);
            ctx.set_result_text(&lilia_zk::fr_to_hex(&commitment));
            Ok(())
        })?;
    }

    // Phonetic scalar functions: soundex, double_metaphone, phonetic_match
    conn.register_scalar_function("soundex", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let s = args[0].as_text();
        ctx.set_result_text(&lilia_core::soundex(s));
        Ok(())
    })?;

    conn.register_scalar_function("lilia_soundex", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let s = args[0].as_text();
        ctx.set_result_text(&lilia_core::soundex(s));
        Ok(())
    })?;

    conn.register_scalar_function("double_metaphone", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let s = args[0].as_text();
        let res = lilia_core::double_metaphone(s);
        let json = if let Some(ref alt) = res.alternate {
            serde_json::to_string(&[&res.primary, alt]).unwrap_or_default()
        } else {
            serde_json::to_string(&[&res.primary]).unwrap_or_default()
        };
        ctx.set_result_text(&json);
        Ok(())
    })?;

    conn.register_scalar_function("double_metaphone_primary", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let s = args[0].as_text();
        let res = lilia_core::double_metaphone(s);
        ctx.set_result_text(&res.primary);
        Ok(())
    })?;

    conn.register_scalar_function("double_metaphone_alt", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let s = args[0].as_text();
        let res = lilia_core::double_metaphone(s);
        if let Some(ref alt) = res.alternate {
            ctx.set_result_text(alt);
        } else {
            ctx.set_result_null();
        }
        Ok(())
    })?;

    conn.register_scalar_function("phonetic_match", 2, |ctx, args| {
        if args.len() < 2 || args[0].is_null() || args[1].is_null() {
            ctx.set_result_int64(0);
            return Ok(());
        }
        let s1 = args[0].as_text();
        let s2 = args[1].as_text();
        let matches = lilia_core::is_phonetic_match(s1, s2);
        ctx.set_result_int64(i64::from(matches));
        Ok(())
    })?;

    // CJK Segmentation function: cjk_segment
    conn.register_scalar_function("cjk_segment", 1, |ctx, args| {
        if args.is_empty() || args[0].is_null() {
            ctx.set_result_null();
            return Ok(());
        }
        let s = args[0].as_text();
        let segmented = lilia_core::prepare_cjk_for_indexing(s);
        ctx.set_result_text(&segmented);
        Ok(())
    })?;

    Ok(())
}

pub(crate) fn register_direct_wasm_function(
    conn: &NativeConnection,
    wasm_registry: &Arc<crate::wasm_udf::WasmUdfRegistry>,
    name: &str,
) -> Result<()> {
    let wasm_reg = Arc::clone(wasm_registry);
    let name_str = name.to_string();
    conn.register_scalar_function(name, -1, move |ctx, args| {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some(call_ctx) = ctx.get_auxdata::<crate::wasm_udf::WasmUdfCallContext>(0) {
                return call_ctx.invoke(ctx, args);
            }

            let Some(registered) = wasm_reg.get(&name_str) else {
                ctx.set_result_error(&format!("wasm udf '{name_str}' is not registered"));
                return Ok(());
            };

            let engine = wasm_reg.engine();
            match crate::wasm_udf::WasmUdfCallContext::new(engine, &registered) {
                Ok(mut new_ctx) => {
                    let res = new_ctx.invoke(ctx, args);
                    if !args.is_empty() {
                        ctx.set_auxdata(0, Box::new(new_ctx));
                    }
                    res
                }
                Err(e) => {
                    ctx.set_result_error(&format!(
                        "failed to instantiate wasm udf '{name_str}': {e}"
                    ));
                    Ok(())
                }
            }
        }

        #[cfg(target_arch = "wasm32")]
        {
            ctx.set_result_error("wasm udfs are not supported on wasm32");
            Ok(())
        }
    })
}

fn bytes_to_f32_vec(bytes: &[u8]) -> Vec<f32> {
    if !bytes.len().is_multiple_of(4) {
        return Vec::new();
    }
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

pub(crate) fn read_column_as_json(stmt: &NativeStatement, col: i32) -> serde_json::Value {
    match stmt.column_type(col) {
        ffi::SQLITE_INTEGER => serde_json::Value::Number(stmt.column_int64(col).into()),
        ffi::SQLITE_FLOAT => serde_json::Number::from_f64(stmt.column_double(col))
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        ffi::SQLITE_TEXT => serde_json::Value::String(stmt.column_text(col).to_string()),
        ffi::SQLITE_BLOB => serde_json::Value::String(hex::encode(stmt.column_blob(col))),
        _ => serde_json::Value::Null,
    }
}

pub(crate) fn bind_json_params(
    stmt: &mut NativeStatement,
    params: &[serde_json::Value],
    string_cache: &mut Vec<String>,
) -> Result<()> {
    bind_json_params_offset(stmt, params, 1, string_cache)
}

fn bind_json_params_offset(
    stmt: &mut NativeStatement,
    params: &[serde_json::Value],
    offset: i32,
    string_cache: &mut Vec<String>,
) -> Result<()> {
    for (idx, val) in params.iter().enumerate() {
        let col = offset + idx as i32;
        match val {
            serde_json::Value::Null => stmt.bind_null(col)?,
            serde_json::Value::Bool(b) => stmt.bind_int64(col, i64::from(*b))?,
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    stmt.bind_int64(col, i)?;
                } else if let Some(u) = n.as_u64() {
                    stmt.bind_int64(col, u as i64)?;
                } else if let Some(f) = n.as_f64() {
                    stmt.bind_double(col, f)?;
                } else {
                    let s = n.to_string();
                    string_cache.push(s);
                    stmt.bind_text_static(col, string_cache.last().unwrap())?;
                }
            }
            serde_json::Value::String(s) => {
                stmt.bind_text_static(col, s)?;
            }
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                let s = val.to_string();
                string_cache.push(s);
                stmt.bind_text_static(col, string_cache.last().unwrap())?;
            }
        }
    }
    Ok(())
}

fn extract_string_literal(sql: &str, field: &str) -> Option<String> {
    let lower_sql = sql.to_ascii_lowercase();
    let pattern_eq = format!("{field}=");
    let pattern_space_eq = format!("{field} =");

    let pos = lower_sql
        .find(&pattern_eq)
        .or_else(|| lower_sql.find(&pattern_space_eq))?;
    let slice = &sql[pos..];
    let eq_idx = slice.find('=')?;
    let after_eq = slice[eq_idx + 1..].trim_start();
    if let Some(stripped) = after_eq.strip_prefix('\'') {
        let closing = stripped.find('\'')?;
        Some(stripped[..closing].to_string())
    } else if let Some(stripped) = after_eq.strip_prefix('"') {
        let closing = stripped.find('"')?;
        Some(stripped[..closing].to_string())
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ValidTimeClause {
    Point(i64),
    Between(i64, i64),
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TransactionTimeClause {
    Point(i64),
    All,
}

struct BitemporalClauseMatch {
    start: usize,
    end: usize,
    table_name: String,
    tt: TransactionTimeClause,
    vt: ValidTimeClause,
}

fn extract_temporal_value(s: &str) -> Option<(String, usize)> {
    let trimmed = s.trim_start();
    let leading_ws = s.len() - trimmed.len();
    if let Some(stripped) = trimmed.strip_prefix('\'') {
        let end_idx = stripped.find('\'')?;
        let val = stripped[..end_idx].to_string();
        Some((val, leading_ws + end_idx + 2))
    } else if let Some(stripped) = trimmed.strip_prefix('"') {
        let end_idx = stripped.find('"')?;
        let val = stripped[..end_idx].to_string();
        Some((val, leading_ws + end_idx + 2))
    } else {
        let mut len = 0;
        for c in trimmed.chars() {
            if c.is_alphanumeric() || c == '_' || c == '-' || c == '+' || c == ':' || c == '.' {
                len += c.len_utf8();
            } else {
                break;
            }
        }
        if len > 0 {
            Some((trimmed[..len].to_string(), leading_ws + len))
        } else {
            None
        }
    }
}

fn find_bitemporal_clause(sql: &str) -> Option<BitemporalClauseMatch> {
    let lower = sql.to_ascii_lowercase();
    let from_keyword = "from ";
    let from_idx = lower.find(from_keyword)?;
    let after_from_idx = from_idx + from_keyword.len();
    let after_from = &sql[after_from_idx..];

    let trimmed = after_from.trim_start();
    let leading_ws = after_from.len() - trimmed.len();
    let table_start_in_sql = after_from_idx + leading_ws;

    let mut table_len = 0;
    for c in trimmed.chars() {
        if c.is_alphanumeric() || c == '_' || c == '$' {
            table_len += c.len_utf8();
        } else {
            break;
        }
    }
    if table_len == 0 {
        return None;
    }
    let table_name = trimmed[..table_len].to_string();
    let after_table_idx = table_start_in_sql + table_len;
    let remainder = &sql[after_table_idx..];

    let mut current_offset = 0;
    let mut tt_clause = None;
    let mut vt_clause = None;

    for _ in 0..2 {
        let rest = &remainder[current_offset..];
        let rest_lower = rest.to_ascii_lowercase();
        let trimmed_rest = rest.trim_start();
        let ws_len = rest.len() - trimmed_rest.len();
        let trimmed_lower = &rest_lower[ws_len..];

        let tt_prefix = if trimmed_lower.starts_with("as of transaction_time ") {
            Some("as of transaction_time ")
        } else if trimmed_lower.starts_with("as of transaction ") {
            Some("as of transaction ")
        } else if trimmed_lower.starts_with("as of system_time ") {
            Some("as of system_time ")
        } else if trimmed_lower.starts_with("as of system time ") {
            Some("as of system time ")
        } else if trimmed_lower.starts_with("as of transaction")
            && (trimmed_lower.len() == 17
                || trimmed_lower[17..].starts_with('\'')
                || trimmed_lower[17..].starts_with('"'))
        {
            Some("as of transaction")
        } else {
            None
        };

        if let Some(prefix) = tt_prefix {
            if tt_clause.is_some() {
                break;
            }
            let after_prefix = &trimmed_rest[prefix.len()..];
            if let Some((val_str, eaten)) = extract_temporal_value(after_prefix) {
                let spec = if val_str.eq_ignore_ascii_case("all") {
                    TransactionTimeClause::All
                } else {
                    let ms = lilia_core::parse_iso_to_ms(&val_str).ok()?;
                    TransactionTimeClause::Point(ms)
                };
                tt_clause = Some(spec);
                current_offset += ws_len + prefix.len() + eaten;
                continue;
            }
        }

        let vt_prefix = if trimmed_lower.starts_with("for valid_time ") {
            Some("for valid_time ")
        } else if trimmed_lower.starts_with("for valid time ") {
            Some("for valid time ")
        } else if trimmed_lower.starts_with("for business_time ") {
            Some("for business_time ")
        } else if trimmed_lower.starts_with("for application_time ") {
            Some("for application_time ")
        } else {
            None
        };

        if let Some(prefix) = vt_prefix {
            if vt_clause.is_some() {
                break;
            }
            let after_prefix = trimmed_rest[prefix.len()..].trim_start();
            let p_ws = (trimmed_rest.len() - prefix.len()) - after_prefix.len();
            let lower_after = after_prefix.to_ascii_lowercase();

            if lower_after.starts_with("between ") {
                let after_between = &after_prefix[8..];
                let (val1_str, eaten1) = extract_temporal_value(after_between)?;
                let rest_between = after_between[eaten1..].trim_start();
                let b_ws = (after_between.len() - eaten1) - rest_between.len();
                let lower_rest_between = rest_between.to_ascii_lowercase();
                if lower_rest_between.starts_with("and ") {
                    let after_and = &rest_between[4..];
                    let (val2_str, eaten2) = extract_temporal_value(after_and)?;
                    let ms1 = lilia_core::parse_iso_to_ms(&val1_str).ok()?;
                    let mut ms2 = lilia_core::parse_iso_to_ms(&val2_str).ok()?;
                    if !val2_str.contains('T')
                        && !val2_str.contains(' ')
                        && ms2 < i64::MAX - 86_400_000
                    {
                        ms2 += 86_400_000;
                    }
                    vt_clause = Some(ValidTimeClause::Between(ms1, ms2));
                    current_offset += ws_len + prefix.len() + p_ws + 8 + eaten1 + b_ws + 4 + eaten2;
                    continue;
                }
            } else if lower_after.starts_with("as of ") {
                let after_as_of = &after_prefix[6..];
                let (val_str, eaten) = extract_temporal_value(after_as_of)?;
                let spec = if val_str.eq_ignore_ascii_case("all") {
                    ValidTimeClause::All
                } else {
                    let ms = lilia_core::parse_iso_to_ms(&val_str).ok()?;
                    ValidTimeClause::Point(ms)
                };
                vt_clause = Some(spec);
                current_offset += ws_len + prefix.len() + p_ws + 6 + eaten;
                continue;
            } else if let Some((val_str, eaten)) = extract_temporal_value(after_prefix) {
                let spec = if val_str.eq_ignore_ascii_case("all") {
                    ValidTimeClause::All
                } else {
                    let ms = lilia_core::parse_iso_to_ms(&val_str).ok()?;
                    ValidTimeClause::Point(ms)
                };
                vt_clause = Some(spec);
                current_offset += ws_len + prefix.len() + p_ws + eaten;
                continue;
            }
        }

        break;
    }

    if tt_clause.is_none() && vt_clause.is_none() {
        return None;
    }

    let end_in_sql = after_table_idx + current_offset;
    let now = crate::storage::now_ms();
    let final_tt = tt_clause.unwrap_or(TransactionTimeClause::Point(now));
    let final_vt = vt_clause.unwrap_or(match final_tt {
        TransactionTimeClause::Point(t) => ValidTimeClause::Point(t),
        TransactionTimeClause::All => ValidTimeClause::All,
    });

    Some(BitemporalClauseMatch {
        start: table_start_in_sql,
        end: end_in_sql,
        table_name,
        tt: final_tt,
        vt: final_vt,
    })
}

fn build_bitemporal_cte(spec: &BitemporalClauseMatch) -> String {
    let mut where_parts = Vec::new();
    let is_global = spec.table_name == "lilia_bitemporal";
    if !is_global {
        let esc_table = spec.table_name.replace('\'', "''");
        where_parts.push(format!("space = '{esc_table}'"));
    }

    match spec.tt {
        TransactionTimeClause::Point(tt) => {
            where_parts.push(format!("tt_from <= {tt} AND {tt} < tt_to"));
        }
        TransactionTimeClause::All => {}
    }

    match spec.vt {
        ValidTimeClause::Point(vt) => {
            where_parts.push(format!("vt_from <= {vt} AND {vt} < vt_to"));
        }
        ValidTimeClause::Between(v1, v2) => {
            where_parts.push(format!("vt_from < {v2} AND vt_to > {v1}"));
        }
        ValidTimeClause::All => {}
    }

    let where_clause = if where_parts.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_parts.join(" AND "))
    };

    let cols = if is_global {
        "(space, id, value, version, vt_from, vt_to, tt_from, tt_to)"
    } else {
        "(id, value, version, vt_from, vt_to, tt_from, tt_to)"
    };

    let select_cols = if is_global {
        "space, id, lilia_decode_json(value) AS value, version, vt_from, vt_to, tt_from, tt_to"
    } else {
        "id, lilia_decode_json(value) AS value, version, vt_from, vt_to, tt_from, tt_to"
    };

    format!(
        "{}{cols} AS (SELECT {select_cols} FROM _lilia_bitemporal {where_clause})",
        spec.table_name
    )
}

fn find_as_of_clause(sql: &str) -> Option<(usize, usize, String, String, u64)> {
    let lower = sql.to_ascii_lowercase();
    let as_of_keyword = " as of ";
    let idx = lower.find(as_of_keyword)?;

    let before_as_of = sql[..idx].trim_end();
    let from_idx = before_as_of.to_ascii_lowercase().rfind("from ")?;
    let table_name_part = before_as_of[from_idx + 5..].trim();
    let table_name = table_name_part.split_whitespace().last()?.to_string();

    let after_as_of = &sql[idx + as_of_keyword.len()..];
    let mut parts = after_as_of.split_whitespace();
    let mode_str = parts.next()?;
    let mode_upper = mode_str.to_ascii_uppercase();
    if mode_upper != "VERSION" && mode_upper != "TIMESTAMP" {
        return None;
    }

    let val_str = parts.next()?;
    let clean_val_str: String = val_str.chars().take_while(char::is_ascii_digit).collect();
    let val: u64 = clean_val_str.parse().ok()?;

    let clause_sub = format!("{table_name}{as_of_keyword}{mode_str} {clean_val_str}");
    let clause_sub_lower = clause_sub.to_ascii_lowercase();
    let start_in_orig = lower.find(&clause_sub_lower)?;
    let end_in_orig = start_in_orig + clause_sub.len();

    Some((start_in_orig, end_in_orig, table_name, mode_upper, val))
}

impl crate::Database {
    fn resolve_time_travel_docs(
        &self,
        table_name: &str,
        mode: &str,
        target_val: u64,
        sql_context: &str,
    ) -> Result<Vec<(String, String, String, u64)>> {
        let is_global_view = table_name == "lilia_json";
        let target_space = if is_global_view {
            extract_string_literal(sql_context, "space")
        } else {
            Some(table_name.to_string())
        };

        let target_id = extract_string_literal(sql_context, "id");
        let mut entries = Vec::new();

        if let Some(space) = target_space {
            if let Some(id) = target_id {
                let entry_opt = if mode == "VERSION" {
                    self.json_get_at_version(&space, &id, target_val)?
                } else {
                    #[allow(clippy::cast_possible_wrap)]
                    self.json_get_at_timestamp(&space, &id, target_val as i64)?
                };
                if let Some(entry) = entry_opt {
                    entries.push((
                        space,
                        id,
                        serde_json::to_string(&entry.value()).unwrap_or_else(|_| "{}".to_string()),
                        entry.version,
                    ));
                }
            } else {
                let conn = self.read_lock()?;
                let mut stmt = conn.prepare(
                    "SELECT DISTINCT id FROM _lilia_json_revisions WHERE space = ?1 AND version <= ?2 LIMIT 10000;",
                )?;
                stmt.bind_text_static(1, &space)?;
                stmt.bind_int64(2, i64::try_from(target_val).unwrap_or(i64::MAX))?;
                let mut ids = Vec::new();
                while stmt.step()? {
                    ids.push(stmt.column_text(0).to_string());
                }
                drop(stmt);

                for id in ids {
                    let entry_opt = if mode == "VERSION" {
                        self.json_get_at_version(&space, &id, target_val)?
                    } else {
                        #[allow(clippy::cast_possible_wrap)]
                        self.json_get_at_timestamp(&space, &id, target_val as i64)?
                    };
                    if let Some(entry) = entry_opt {
                        entries.push((
                            space.clone(),
                            id,
                            serde_json::to_string(&entry.value())
                                .unwrap_or_else(|_| "{}".to_string()),
                            entry.version,
                        ));
                    }
                }
            }
        }

        Ok(entries)
    }

    fn preprocess_time_travel_sql(&self, sql: &str) -> Result<String> {
        // 1. Check for bitemporal clauses (AS OF TRANSACTION ... FOR VALID_TIME ...)
        if let Some(bitemp_match) = find_bitemporal_clause(sql) {
            let cte_body = build_bitemporal_cte(&bitemp_match);
            let replaced_sql = format!(
                "{}{}{}",
                &sql[..bitemp_match.start],
                bitemp_match.table_name,
                &sql[bitemp_match.end..]
            );
            let trimmed = replaced_sql.trim();
            let final_sql = if trimmed.to_ascii_lowercase().starts_with("with ") {
                format!("WITH {}, {}", cte_body, &trimmed[5..])
            } else {
                format!("WITH {} {}", cte_body, trimmed)
            };
            return Ok(final_sql);
        }

        // 2. Fall back to standard JSON revisions time travel (AS OF VERSION / AS OF TIMESTAMP)
        let Some((start, end, table_name, mode, target_val)) = find_as_of_clause(sql) else {
            return Ok(sql.to_string());
        };

        let docs = self.resolve_time_travel_docs(&table_name, &mode, target_val, sql)?;
        let is_global = table_name == "lilia_json";

        let cte_body = if docs.is_empty() {
            if is_global {
                format!("{table_name}(space, id, value, version, tier) AS (SELECT '' AS space, '' AS id, '{{}}' AS value, 0 AS version, 'disk' AS tier WHERE 0)")
            } else {
                format!("{table_name}(id, value, version) AS (SELECT '' AS id, '{{}}' AS value, 0 AS version WHERE 0)")
            }
        } else {
            let mut select_clauses = Vec::new();
            for (space, id, json_str, ver) in docs {
                let esc_space = space.replace('\'', "''");
                let esc_id = id.replace('\'', "''");
                let esc_json = json_str.replace('\'', "''");
                if is_global {
                    select_clauses.push(format!(
                        "SELECT '{esc_space}', '{esc_id}', '{esc_json}', {ver}, 'disk'"
                    ));
                } else {
                    select_clauses.push(format!("SELECT '{esc_id}', '{esc_json}', {ver}"));
                }
            }
            let cols = if is_global {
                "(space, id, value, version, tier)"
            } else {
                "(id, value, version)"
            };
            format!(
                "{table_name}{cols} AS ({})",
                select_clauses.join(" UNION ALL ")
            )
        };

        let replaced_sql = format!("{}{table_name}{}", &sql[..start], &sql[end..]);
        let trimmed_replaced = replaced_sql.trim();

        let final_sql = if trimmed_replaced.to_ascii_lowercase().starts_with("with ") {
            format!("WITH {}, {}", cte_body, &trimmed_replaced[5..])
        } else {
            format!("WITH {cte_body} {trimmed_replaced}")
        };

        Ok(final_sql)
    }

    pub(crate) fn preprocess_rls_sql(&self, sql: &str) -> Result<String> {
        let all_policies = self.security_registry.list_policies();
        if all_policies.is_empty() {
            return Ok(sql.to_string());
        }

        let lower = sql.to_ascii_lowercase();
        // Skip DDL statements
        if lower.starts_with("create ") || lower.starts_with("drop ") || lower.starts_with("alter ")
        {
            return Ok(sql.to_string());
        }

        let mut ctes = Vec::new();
        let mut lilia_json_conditions = Vec::new();

        for np in &all_policies {
            let table = &np.policy.table;
            let cond = &np.policy.condition;
            if table == "lilia_json" {
                lilia_json_conditions.push(cond.clone());
            } else {
                let table_lower = table.to_ascii_lowercase();
                let from_table = format!("from {table_lower}");
                let join_table = format!("join {table_lower}");
                let from_table_quote = format!("from \"{table_lower}\"");
                let join_table_quote = format!("join \"{table_lower}\"");
                if lower.contains(&from_table)
                    || lower.contains(&join_table)
                    || lower.contains(&from_table_quote)
                    || lower.contains(&join_table_quote)
                {
                    ctes.push(format!(
                        "{table} AS (SELECT * FROM main.{table} WHERE {cond})"
                    ));
                }

                lilia_json_conditions.push(format!("(space != '{table}' OR ({cond}))"));
            }
        }

        let references_lilia_json =
            lower.contains("from lilia_json") || lower.contains("join lilia_json");
        if references_lilia_json && !lilia_json_conditions.is_empty() {
            let combined = lilia_json_conditions.join(" AND ");
            ctes.push(format!(
                "lilia_json AS (SELECT * FROM temp.lilia_json WHERE {combined})"
            ));
        }

        if ctes.is_empty() {
            return Ok(sql.to_string());
        }

        let ctes_str = ctes.join(", ");
        let trimmed = sql.trim();
        let final_sql = if trimmed.to_ascii_lowercase().starts_with("with ") {
            format!("WITH {ctes_str}, {}", &trimmed[5..])
        } else {
            format!("WITH {ctes_str} {trimmed}")
        };

        Ok(final_sql)
    }

    fn apply_sql_masking(&self, columns: &[String], rows: &mut [Vec<serde_json::Value>]) {
        let all_masking_rules = self.security_registry.list_masking_rules();
        if all_masking_rules.is_empty() {
            return;
        }
        let sec_ctx = crate::security::get_active_context();
        let space_idx = columns.iter().position(|c| c == "space");
        let value_idx = columns.iter().position(|c| c == "value" || c == "data");

        for row in rows.iter_mut() {
            // Case 1: row contains (space, value/data)
            if let (Some(s_idx), Some(v_idx)) = (space_idx, value_idx) {
                if let serde_json::Value::String(ref space_str) = row[s_idx] {
                    let rules = self.security_registry.masking_rules_for_table(space_str);
                    if !rules.is_empty() {
                        if let serde_json::Value::String(ref json_str) = row[v_idx] {
                            if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(json_str)
                            {
                                lilia_core::security::mask_document(
                                    &mut val,
                                    &rules,
                                    sec_ctx.as_ref(),
                                    |exempt, c| {
                                        lilia_core::security::eval_exemption_condition(exempt, c)
                                    },
                                );
                                row[v_idx] = serde_json::Value::String(val.to_string());
                            }
                        }
                    }
                }
            }

            // Case 2: column name itself matches a masked field path (e.g. "credit_card", "$.credit_card")
            for (col_idx, col_name) in columns.iter().enumerate() {
                for rule in &all_masking_rules {
                    if rule.matches_path(col_name) {
                        let is_exempt = rule.exempt_condition.as_deref().is_some_and(|ex| {
                            lilia_core::security::eval_exemption_condition(ex, sec_ctx.as_ref())
                        });
                        if !is_exempt {
                            row[col_idx] =
                                lilia_core::security::mask_value(&row[col_idx], &rule.mask);
                        }
                    }
                }
            }
        }
    }

    /// Execute a SQL query or mutation.
    /// Read-only queries (SELECT, EXPLAIN) are executed on the concurrent reader pool.
    /// Mutating queries (INSERT, UPDATE, DELETE) are executed via the exclusive writer queue.
    #[allow(clippy::too_many_lines)]
    pub fn sql_query(&self, sql: &str, params: &[serde_json::Value]) -> Result<SqlQueryResult> {
        let effective_sql = self.preprocess_time_travel_sql(sql)?;
        let rls_sql = self.preprocess_rls_sql(&effective_sql)?;
        let trimmed = rls_sql.trim();
        if trimmed.is_empty() {
            return Ok(SqlQueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                rows_affected: 0,
            });
        }

        if let Some(res) = self.try_execute_extended_ddl(trimmed)? {
            return Ok(res);
        }

        if crate::energy::is_explain_diagnose_query(trimmed) {
            let diag = self.explain_diagnose(trimmed, params)?;
            return Ok(SqlQueryResult {
                columns: vec![
                    "query".to_string(),
                    "execution_time_us".to_string(),
                    "estimated_energy_uj".to_string(),
                    "diagnosis".to_string(),
                    "recommendation".to_string(),
                ],
                rows: vec![vec![
                    serde_json::Value::String(diag.query),
                    serde_json::Value::Number(serde_json::Number::from(diag.execution_time_us)),
                    serde_json::json!(diag.estimated_energy_uj),
                    serde_json::Value::String(diag.diagnosis.as_str().to_string()),
                    diag.recommendation
                        .map_or(serde_json::Value::Null, serde_json::Value::String),
                ]],
                rows_affected: 0,
            });
        }

        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("create materialized view ") {
            let after = trimmed["create materialized view ".len()..].trim();
            let after_if_not_exists = if after.to_ascii_lowercase().starts_with("if not exists ") {
                after["if not exists ".len()..].trim()
            } else {
                after
            };
            if let Some(as_idx) = after_if_not_exists.to_ascii_lowercase().find(" as ") {
                let name = after_if_not_exists[..as_idx]
                    .trim()
                    .trim_matches('"')
                    .trim_matches('`');
                let query = after_if_not_exists[as_idx + 4..].trim();
                self.create_materialized_view(name, query)?;
                return Ok(SqlQueryResult {
                    columns: Vec::new(),
                    rows: Vec::new(),
                    rows_affected: 0,
                });
            }
        } else if lower.starts_with("drop materialized view ") {
            let after = trimmed["drop materialized view ".len()..].trim();
            let after_if_exists = if after.to_ascii_lowercase().starts_with("if exists ") {
                after["if exists ".len()..].trim()
            } else {
                after
            };
            let name = after_if_exists
                .trim_end_matches(';')
                .trim()
                .trim_matches('"')
                .trim_matches('`');
            self.drop_materialized_view(name)?;
            return Ok(SqlQueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                rows_affected: 0,
            });
        }

        let conn = self.read_lock()?;
        #[cfg(not(target_arch = "wasm32"))]
        let effective_query = crate::polyglot::preprocess_polyglot_sql(&conn, trimmed)?;
        #[cfg(target_arch = "wasm32")]
        let effective_query = trimmed.to_string();

        let stmt = conn.prepare(&effective_query)?;
        let is_readonly = stmt.readonly();
        drop(stmt);

        if is_readonly {
            let mut stmt = conn.prepare(&effective_query)?;
            let col_count = stmt.column_count();
            let mut columns = Vec::with_capacity(col_count);
            for i in 0..col_count {
                columns.push(stmt.column_name(i as i32).to_string());
            }

            let mut string_cache = Vec::new();
            bind_json_params(&mut stmt, params, &mut string_cache)?;

            let started = std::time::Instant::now();
            let mut rows = Vec::new();
            while stmt.step()? {
                let mut row_vals = Vec::with_capacity(col_count);
                for i in 0..col_count {
                    row_vals.push(read_column_as_json(&stmt, i as i32));
                }
                rows.push(row_vals);
            }
            self.autotune.record_sql(
                trimmed,
                started.elapsed(),
                stmt.status(lilia_sqlite_native::StmtStatus::FullscanStep, false),
            );

            self.apply_sql_masking(&columns, &mut rows);

            Ok(SqlQueryResult {
                columns,
                rows,
                rows_affected: 0,
            })
        } else {
            drop(conn);
            let mut writer = self.lock_writer()?;
            let session = if self.has_live_subscribers()
                || self.undo_log_capacity > 0
                || self.dataflow_engine.has_active_pipelines()
            {
                if let Ok(mut sess) = lilia_sqlite_native::NativeSession::create(&writer, "main") {
                    sess.set_table_filter(crate::engine::undo_records_table);
                    let _ = sess.attach_all();
                    Some(sess)
                } else {
                    None
                }
            } else {
                None
            };

            let tx = writer.transaction()?;
            #[cfg(not(target_arch = "wasm32"))]
            let effective_write_query =
                crate::polyglot::preprocess_polyglot_sql(tx.conn(), trimmed)?;
            #[cfg(target_arch = "wasm32")]
            let effective_write_query = trimmed.to_string();

            let mut protected: Vec<&str> = Vec::new();
            if !self.integrity.is_empty() {
                protected.extend(crate::integrity::CONTRACT_TABLES);
            }
            if self.ledger.enabled() {
                protected.extend(crate::ledger::LEDGER_TABLES);
            }
            let mut stmt = if protected.is_empty() {
                tx.prepare(&effective_write_query)?
            } else {
                tx.prepare_denying_writes(&effective_write_query, &protected)?
            };
            let col_count = stmt.column_count();
            let mut columns = Vec::with_capacity(col_count);
            for i in 0..col_count {
                columns.push(stmt.column_name(i as i32).to_string());
            }

            let mut string_cache = Vec::new();
            bind_json_params(&mut stmt, params, &mut string_cache)?;

            let started = std::time::Instant::now();
            let mut rows = Vec::new();
            let rows_affected = if col_count > 0 {
                while stmt.step()? {
                    let mut row_vals = Vec::with_capacity(col_count);
                    for i in 0..col_count {
                        row_vals.push(read_column_as_json(&stmt, i as i32));
                    }
                    rows.push(row_vals);
                }
                0
            } else {
                stmt.step()?;
                u64::try_from(tx.conn().changes()).unwrap_or(0)
            };
            self.autotune.record_sql(
                trimmed,
                started.elapsed(),
                stmt.status(lilia_sqlite_native::StmtStatus::FullscanStep, false),
            );
            drop(stmt);
            self.ledger
                .record_commit(tx.conn(), Vec::new(), crate::storage::now_ms())?;
            tx.commit()?;

            if let Some(sess) = session {
                if !sess.is_empty() {
                    if let Ok(changeset) = sess.changeset() {
                        if !changeset.is_empty() {
                            self.live_query_manager
                                .process_changeset(&changeset, &writer);
                            self.dataflow_engine.process_changeset(&changeset, &writer);
                        }
                    }
                }
            }

            self.apply_sql_masking(&columns, &mut rows);

            Ok(SqlQueryResult {
                columns,
                rows,
                rows_affected,
            })
        }
    }

    /// Fluent SQL interface for queries, parameter binding, and execution.
    #[must_use]
    pub fn sql(&self) -> SqlStore<'_> {
        SqlStore { db: self }
    }

    /// Execute a SQL query or mutation with a carray bound to parameter ?1.
    pub fn sql_query_with_carray<C: CarrayBindable>(
        &self,
        sql: &str,
        carray: &C,
    ) -> Result<SqlQueryResult> {
        self.sql_query_with_carray_and_params(sql, carray, &[])
    }

    /// Execute a SQL query or mutation with a carray bound to parameter ?1 and additional params to ?2, ?3, ...
    #[allow(clippy::too_many_lines)]
    pub fn sql_query_with_carray_and_params<C: CarrayBindable>(
        &self,
        sql: &str,
        carray: &C,
        params: &[serde_json::Value],
    ) -> Result<SqlQueryResult> {
        let effective_sql = self.preprocess_time_travel_sql(sql)?;
        let trimmed = effective_sql.trim();
        if trimmed.is_empty() {
            return Ok(SqlQueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                rows_affected: 0,
            });
        }

        let is_readonly = {
            let conn = self.read_lock()?;
            let stmt = conn.prepare(trimmed)?;
            stmt.readonly()
        };

        if is_readonly {
            let conn = self.read_lock()?;
            let mut stmt = conn.prepare(trimmed)?;
            let col_count = stmt.column_count();
            let mut columns = Vec::with_capacity(col_count);
            for i in 0..col_count {
                columns.push(stmt.column_name(i as i32).to_string());
            }

            carray.bind_to(&mut stmt, 1)?;

            let mut string_cache = Vec::new();
            bind_json_params_offset(&mut stmt, params, 2, &mut string_cache)?;

            let mut rows = Vec::new();
            while stmt.step()? {
                let mut row_vals = Vec::with_capacity(col_count);
                for i in 0..col_count {
                    row_vals.push(read_column_as_json(&stmt, i as i32));
                }
                rows.push(row_vals);
            }

            Ok(SqlQueryResult {
                columns,
                rows,
                rows_affected: 0,
            })
        } else {
            let mut writer = self.lock_writer()?;
            let tx = writer.transaction()?;
            let mut stmt = tx.prepare(trimmed)?;
            let col_count = stmt.column_count();
            let mut columns = Vec::with_capacity(col_count);
            for i in 0..col_count {
                columns.push(stmt.column_name(i as i32).to_string());
            }

            carray.bind_to(&mut stmt, 1)?;

            let mut string_cache = Vec::new();
            bind_json_params_offset(&mut stmt, params, 2, &mut string_cache)?;

            let mut rows = Vec::new();
            let rows_affected = if col_count > 0 {
                while stmt.step()? {
                    let mut row_vals = Vec::with_capacity(col_count);
                    for i in 0..col_count {
                        row_vals.push(read_column_as_json(&stmt, i as i32));
                    }
                    rows.push(row_vals);
                }
                0
            } else {
                stmt.step()?;
                u64::try_from(tx.conn().changes()).unwrap_or(0)
            };
            drop(stmt);
            tx.commit()?;

            Ok(SqlQueryResult {
                columns,
                rows,
                rows_affected,
            })
        }
    }

    fn try_execute_extended_ddl(&self, trimmed: &str) -> Result<Option<SqlQueryResult>> {
        let Some((w1, r1)) = take_ddl_token(trimmed) else {
            return Ok(None);
        };
        if !w1.eq_ignore_ascii_case("CREATE") {
            return Ok(None);
        }
        let Some((w2, r2)) = take_ddl_token(r1) else {
            return Ok(None);
        };
        if w2.eq_ignore_ascii_case("EPHEMERAL") {
            if let Some((w3, _)) = take_ddl_token(r2) {
                if w3.eq_ignore_ascii_case("TABLE") {
                    self.create_memory_table(trimmed)?;
                    return Ok(Some(SqlQueryResult {
                        columns: Vec::new(),
                        rows: Vec::new(),
                        rows_affected: 0,
                    }));
                }
            }
            return Ok(None);
        }
        if !w2.eq_ignore_ascii_case("COLLECTION") {
            return Ok(None);
        }

        let mut rest = r2;
        let mut if_not_exists = false;
        let (mut name_tok, next_rest) = take_ddl_token(rest).ok_or_else(|| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                "CREATE COLLECTION requires a collection name",
                false,
            )
        })?;
        rest = next_rest;
        if name_tok.eq_ignore_ascii_case("IF") {
            let (kw_not, r_not) = take_ddl_token(rest).ok_or_else(|| {
                LiliaError::new(
                    ErrorCode::InvalidInput,
                    "incomplete IF NOT EXISTS clause",
                    false,
                )
            })?;
            let (kw_exists, r_ex) = take_ddl_token(r_not).ok_or_else(|| {
                LiliaError::new(
                    ErrorCode::InvalidInput,
                    "incomplete IF NOT EXISTS clause",
                    false,
                )
            })?;
            if !kw_not.eq_ignore_ascii_case("NOT") || !kw_exists.eq_ignore_ascii_case("EXISTS") {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "expected IF NOT EXISTS in CREATE COLLECTION",
                    false,
                ));
            }
            if_not_exists = true;
            let (actual_name, after_name) = take_ddl_token(r_ex).ok_or_else(|| {
                LiliaError::new(
                    ErrorCode::InvalidInput,
                    "CREATE COLLECTION requires a collection name",
                    false,
                )
            })?;
            name_tok = actual_name;
            rest = after_name;
        }

        crate::storage::validate_name("collection", &name_tok)?;
        let opts_str = extract_collection_options_body(rest)?;
        let spec = parse_collection_spec_ddl(&name_tok, opts_str)?;

        if if_not_exists
            && self
                .catalog
                .list_specs()
                .iter()
                .any(|s| s.kind == spec.kind && s.name == spec.name)
        {
            return Ok(Some(SqlQueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                rows_affected: 0,
            }));
        }

        self.set_collection_spec(spec)?;
        Ok(Some(SqlQueryResult {
            columns: Vec::new(),
            rows: Vec::new(),
            rows_affected: 0,
        }))
    }
}

fn take_ddl_token(input: &str) -> Option<(String, &str)> {
    let mut s = input.trim_start();
    while s.starts_with(',') || s.starts_with('=') || s.starts_with(';') {
        s = s[1..].trim_start();
    }
    if s.is_empty() {
        return None;
    }
    let first = s.as_bytes()[0];
    if first == b'\'' || first == b'"' || first == b'`' {
        let quote = first as char;
        let rest = &s[1..];
        for (idx, ch) in rest.char_indices() {
            if ch == quote {
                return Some((rest[..idx].to_owned(), &rest[idx + ch.len_utf8()..]));
            }
        }
        return None;
    }
    let mut depth = 0usize;
    let mut end = s.len();
    for (idx, ch) in s.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                if depth == 0 {
                    end = idx;
                    break;
                }
                depth -= 1;
            }
            ',' | '=' | ';' if depth == 0 => {
                end = idx;
                break;
            }
            c if c.is_ascii_whitespace() && depth == 0 => {
                end = idx;
                break;
            }
            _ => {}
        }
    }
    if end == 0 {
        let ch = s.chars().next()?;
        let len = ch.len_utf8();
        return Some((ch.to_string(), &s[len..]));
    }
    Some((s[..end].to_owned(), &s[end..]))
}

fn extract_collection_options_body(rest: &str) -> Result<&str> {
    let trimmed = rest.trim();
    let no_semi = trimmed.strip_suffix(';').unwrap_or(trimmed).trim();
    if let Some(after_open) = no_semi.strip_prefix('(') {
        let inner = after_open.strip_suffix(')').ok_or_else(|| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                "unclosed '(' in CREATE COLLECTION options",
                false,
            )
        })?;
        return Ok(inner.trim());
    }
    Ok(no_semi)
}

fn parse_collection_spec_ddl(name: &str, body: &str) -> Result<CollectionSpec> {
    let mut kind: Option<CollectionKind> = None;
    let mut tier: Option<StorageTier> = None;
    let mut ttl_ms: Option<u64> = None;
    let mut eviction: Option<EvictionPolicy> = None;
    let mut durability: Option<Durability> = None;

    let mut cur = body;
    while let Some((key, after_key)) = take_ddl_token(cur) {
        let (mut val, after_val) = take_ddl_token(after_key).ok_or_else(|| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("missing value for CREATE COLLECTION option '{key}'"),
                false,
            )
        })?;
        cur = after_val;
        if let Some(stripped) = cur.trim_start().strip_prefix('(') {
            if let Some(close_idx) = stripped.find(')') {
                val.push('(');
                val.push_str(stripped[..close_idx].trim());
                val.push(')');
                cur = &stripped[close_idx + 1..];
            }
        }
        match key.to_ascii_lowercase().as_str() {
            "kind" => {
                kind = Some(CollectionKind::parse(&val).ok_or_else(|| {
                    LiliaError::new(
                        ErrorCode::InvalidInput,
                        format!("unknown collection KIND '{val}'"),
                        false,
                    )
                })?);
            }
            "tier" => {
                tier = Some(StorageTier::parse(&val).ok_or_else(|| {
                    LiliaError::new(
                        ErrorCode::InvalidInput,
                        format!("unknown collection TIER '{val}'"),
                        false,
                    )
                })?);
            }
            "ttl" | "ttl_ms" => {
                ttl_ms = parse_ddl_ttl_ms(&val)?;
            }
            "eviction" => {
                eviction = Some(parse_ddl_eviction(&val)?);
            }
            "durability" => {
                durability = parse_ddl_durability(&val)?;
            }
            other => {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    format!("unknown CREATE COLLECTION option '{other}'"),
                    false,
                ));
            }
        }
    }

    let kind = kind.ok_or_else(|| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            "CREATE COLLECTION requires a KIND option",
            false,
        )
    })?;
    let mut spec = CollectionSpec::new(kind, name, tier.unwrap_or(StorageTier::Disk));
    if let Some(ttl) = ttl_ms {
        spec = spec.with_ttl_ms(ttl);
    }
    if let Some(ev) = eviction {
        spec = spec.with_eviction(ev);
    }
    if let Some(dur) = durability {
        spec = spec.with_durability(dur);
    }
    Ok(spec)
}

fn parse_ddl_ttl_ms(raw: &str) -> Result<Option<u64>> {
    let lower = raw.trim().to_ascii_lowercase();
    if lower == "none" || lower == "null" {
        return Ok(None);
    }
    if let Some(num) = lower.strip_suffix("ms") {
        let val = num.trim().parse::<u64>().map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("invalid TTL '{raw}': {e}"),
                false,
            )
        })?;
        return Ok(Some(val));
    }
    if let Some(num) = lower.strip_suffix('s') {
        let val = num.trim().parse::<u64>().map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("invalid TTL '{raw}': {e}"),
                false,
            )
        })?;
        return Ok(Some(val.saturating_mul(1_000)));
    }
    if let Some(num) = lower.strip_suffix('m') {
        let val = num.trim().parse::<u64>().map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("invalid TTL '{raw}': {e}"),
                false,
            )
        })?;
        return Ok(Some(val.saturating_mul(60_000)));
    }
    if let Some(num) = lower.strip_suffix('h') {
        let val = num.trim().parse::<u64>().map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("invalid TTL '{raw}': {e}"),
                false,
            )
        })?;
        return Ok(Some(val.saturating_mul(3_600_000)));
    }
    let val = lower.parse::<u64>().map_err(|e| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid TTL '{raw}': {e}"),
            false,
        )
    })?;
    Ok(Some(val))
}

fn parse_ddl_eviction(raw: &str) -> Result<EvictionPolicy> {
    let lower = raw.trim().to_ascii_lowercase();
    if lower == "none" {
        return Ok(EvictionPolicy::None);
    }
    if let Some(arg) = lower.strip_prefix("lru(").and_then(|s| s.strip_suffix(')')) {
        let max_entries = arg.trim().parse::<usize>().map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("invalid LRU cap '{raw}': {e}"),
                false,
            )
        })?;
        return Ok(EvictionPolicy::Lru { max_entries });
    }
    if let Some(arg) = lower
        .strip_prefix("drop_oldest(")
        .or_else(|| lower.strip_prefix("fifo("))
        .and_then(|s| s.strip_suffix(')'))
    {
        let max_entries = arg.trim().parse::<usize>().map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("invalid DropOldest cap '{raw}': {e}"),
                false,
            )
        })?;
        return Ok(EvictionPolicy::DropOldest { max_entries });
    }
    Err(LiliaError::new(
        ErrorCode::InvalidInput,
        format!("invalid EVICTION policy '{raw}'"),
        false,
    ))
}

fn parse_ddl_durability(raw: &str) -> Result<Option<Durability>> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "none" | "default" => Ok(None),
        "durable" => Ok(Some(Durability::Durable)),
        "balanced" => Ok(Some(Durability::Balanced)),
        "performance" => Ok(Some(Durability::Performance)),
        _ => Err(LiliaError::new(
            ErrorCode::InvalidInput,
            format!("unknown DURABILITY '{raw}'"),
            false,
        )),
    }
}

/// Trait for types that can be bound to a SQLite `carray(?1)` parameter.
pub trait CarrayBindable {
    /// Bind the array slice into the statement at the given 1-based parameter column index.
    fn bind_to(&self, stmt: &mut NativeStatement, col: i32) -> Result<()>;
}

impl CarrayBindable for [u64] {
    fn bind_to(&self, stmt: &mut NativeStatement, col: i32) -> Result<()> {
        stmt.bind_carray_uint64_static(col, self)
    }
}

impl CarrayBindable for [i64] {
    fn bind_to(&self, stmt: &mut NativeStatement, col: i32) -> Result<()> {
        stmt.bind_carray_int64_static(col, self)
    }
}

impl CarrayBindable for [i32] {
    fn bind_to(&self, stmt: &mut NativeStatement, col: i32) -> Result<()> {
        stmt.bind_carray_int32_static(col, self)
    }
}

impl CarrayBindable for [f64] {
    fn bind_to(&self, stmt: &mut NativeStatement, col: i32) -> Result<()> {
        stmt.bind_carray_double_static(col, self)
    }
}

impl CarrayBindable for [&str] {
    fn bind_to(&self, stmt: &mut NativeStatement, col: i32) -> Result<()> {
        stmt.bind_carray_text_transient(col, self)
    }
}

impl<T: CarrayBindable + ?Sized> CarrayBindable for &T {
    fn bind_to(&self, stmt: &mut NativeStatement, col: i32) -> Result<()> {
        (*self).bind_to(stmt, col)
    }
}

impl<T> CarrayBindable for Vec<T>
where
    [T]: CarrayBindable,
{
    fn bind_to(&self, stmt: &mut NativeStatement, col: i32) -> Result<()> {
        self.as_slice().bind_to(stmt, col)
    }
}

/// Fluent SQL interface for queries, parameter binding, and execution.
#[derive(Debug, Clone, Copy)]
pub struct SqlStore<'a> {
    db: &'a crate::Database,
}

impl<'a> SqlStore<'a> {
    /// Execute a SQL query without parameters.
    pub fn query(&self, sql: &str) -> Result<SqlQueryResult> {
        self.db.sql_query(sql, &[])
    }

    /// Execute a SQL query with parameters.
    pub fn query_with_params(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<SqlQueryResult> {
        self.db.sql_query(sql, params)
    }

    /// Execute a SQL query with a carray bound to parameter ?1.
    pub fn query_with_carray<C: CarrayBindable>(
        &self,
        sql: &str,
        carray: &C,
    ) -> Result<SqlQueryResult> {
        self.db.sql_query_with_carray(sql, carray)
    }

    /// Execute a SQL query with a carray bound to parameter ?1 and additional params to ?2, ?3, ...
    pub fn query_with_carray_and_params<C: CarrayBindable>(
        &self,
        sql: &str,
        carray: &C,
        params: &[serde_json::Value],
    ) -> Result<SqlQueryResult> {
        self.db
            .sql_query_with_carray_and_params(sql, carray, params)
    }

    /// Execute a mutating SQL statement and return the number of rows affected.
    pub fn execute(&self, sql: &str) -> Result<u64> {
        let res = self.db.sql_query(sql, &[])?;
        Ok(res.rows_affected)
    }
}
