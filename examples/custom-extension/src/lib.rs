#![allow(unsafe_code)]

use std::sync::atomic::{AtomicU8, Ordering};
use lilia_plugin_api::{Capability, CodecDescriptorV1, PluginDescriptorV1};

static PLUGIN_NAME: &[u8] = b"lilia-custom-codec\0";
static CODEC_NAME: &[u8] = b"xor-rotate-codec\0";

/// Global dynamic key for the XOR cipher.
/// Default key is 0x42 (deterministic development key).
static CIPHER_KEY: AtomicU8 = AtomicU8::new(0x42);

/// Initialize the codec with a key buffer.
///
/// # Safety
/// If `key` is non-null and `key_len > 0`, it must be valid for reads.
#[no_mangle]
pub unsafe extern "C" fn codec_init(key: *const u8, key_len: usize) -> i32 {
    if !key.is_null() && key_len > 0 {
        let first_byte = unsafe { *key };
        CIPHER_KEY.store(first_byte, Ordering::SeqCst);
        0
    } else {
        // Fallback to default
        CIPHER_KEY.store(0x42, Ordering::SeqCst);
        0
    }
}

/// Encode plaintext into ciphertext using XOR and 1-bit left circular rotation.
///
/// # Safety
/// `input` must be readable for `input_len` bytes.
/// `out_len` must be a valid pointer to usize.
/// `out` must have capacity of at least `out_cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn codec_encode(
    input: *const u8,
    input_len: usize,
    out: *mut u8,
    out_cap: usize,
    out_len: *mut usize,
) -> i32 {
    if (input.is_null() && input_len > 0) || out_len.is_null() {
        return -1;
    }

    // Output length equals input length
    unsafe {
        *out_len = input_len;
    }

    if out.is_null() || out_cap < input_len {
        return 1; // Needs more capacity
    }

    let key = CIPHER_KEY.load(Ordering::Relaxed);
    let in_slice = if input_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(input, input_len) }
    };
    let out_slice = unsafe { std::slice::from_raw_parts_mut(out, input_len) };

    for (i, &b) in in_slice.iter().enumerate() {
        // Transformation: XOR with key, then rotate left by 1 bit
        out_slice[i] = (b ^ key).rotate_left(1);
    }

    0
}

/// Decode ciphertext back into plaintext by rotating right by 1 bit then XOR with key.
///
/// # Safety
/// `input` must be readable for `input_len` bytes.
/// `out_len` must be a valid pointer to usize.
/// `out` must have capacity of at least `out_cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn codec_decode(
    input: *const u8,
    input_len: usize,
    out: *mut u8,
    out_cap: usize,
    out_len: *mut usize,
) -> i32 {
    if (input.is_null() && input_len > 0) || out_len.is_null() {
        return -1;
    }

    unsafe {
        *out_len = input_len;
    }

    if out.is_null() || out_cap < input_len {
        return 1; // Needs more capacity
    }

    let key = CIPHER_KEY.load(Ordering::Relaxed);
    let in_slice = if input_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(input, input_len) }
    };
    let out_slice = unsafe { std::slice::from_raw_parts_mut(out, input_len) };

    for (i, &b) in in_slice.iter().enumerate() {
        // Invert: rotate right by 1 bit, then XOR with key
        out_slice[i] = b.rotate_right(1) ^ key;
    }

    0
}

// -------------------------------------------------------------------------
// WebAssembly guest memory allocator exports
// -------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn lilia_alloc(size: i32) -> i32 {
    if size <= 0 {
        return 0;
    }
    let layout = std::alloc::Layout::from_size_align(size as usize, 8).unwrap();
    let ptr = unsafe { std::alloc::alloc(layout) };
    ptr as i32
}

#[no_mangle]
pub extern "C" fn lilia_dealloc(ptr: i32, size: i32) {
    if ptr <= 0 || size <= 0 {
        return;
    }
    let layout = std::alloc::Layout::from_size_align(size as usize, 8).unwrap();
    unsafe { std::alloc::dealloc(ptr as *mut u8, layout) };
}

// -------------------------------------------------------------------------
// Native C-ABI descriptor export
// -------------------------------------------------------------------------

static CODEC_DESCRIPTOR: CodecDescriptorV1 = CodecDescriptorV1 {
    name: CODEC_NAME.as_ptr().cast(),
    init: Some(codec_init),
    encode: Some(codec_encode),
    decode: Some(codec_decode),
};

#[no_mangle]
pub extern "C" fn lilia_plugin_v1() -> PluginDescriptorV1 {
    PluginDescriptorV1::with_codec(
        PLUGIN_NAME.as_ptr().cast(),
        Capability::Codec.bit(),
        &raw const CODEC_DESCRIPTOR,
    )
}

// -------------------------------------------------------------------------
// Unit Tests
// -------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_descriptor_declaration() {
        let desc = lilia_plugin_v1();
        assert_eq!(desc.capabilities, Capability::Codec.bit());
        assert!(!desc.codec.is_null());
        let codec = unsafe { &*desc.codec };
        assert!(codec.encode.is_some());
        assert!(codec.decode.is_some());
    }

    #[test]
    fn test_encode_decode_roundtrip() {
        let key = 0xA5u8;
        unsafe { codec_init(&key, 1) };

        let original = b"Hello LiliaDB Custom Extension Plugin!";
        let mut encoded = vec![0u8; 128];
        let mut enc_len = 0usize;

        let res = unsafe {
            codec_encode(
                original.as_ptr(),
                original.len(),
                encoded.as_mut_ptr(),
                encoded.len(),
                &raw mut enc_len,
            )
        };
        assert_eq!(res, 0);
        assert_eq!(enc_len, original.len());
        encoded.truncate(enc_len);

        // Ciphertext should differ from original
        assert_ne!(&encoded, original);

        // Decode
        let mut decoded = vec![0u8; 128];
        let mut dec_len = 0usize;
        let res_dec = unsafe {
            codec_decode(
                encoded.as_ptr(),
                encoded.len(),
                decoded.as_mut_ptr(),
                decoded.len(),
                &raw mut dec_len,
            )
        };
        assert_eq!(res_dec, 0);
        assert_eq!(dec_len, original.len());
        decoded.truncate(dec_len);

        assert_eq!(&decoded, original);
    }

    #[test]
    fn test_sqlite_integration() {
        use lilia_core::{BatchOperation, ValueCodec};
        use lilia_storage_sqlite::{Database, DatabaseOptions};
        use serde_json::json;
        use std::sync::Arc;

        #[derive(Debug)]
        struct CustomCodecWrapper;
        impl ValueCodec for CustomCodecWrapper {
            fn name(&self) -> &str {
                "xor-rotate-codec"
            }
            fn encode<'a>(&self, plaintext: &'a [u8]) -> lilia_core::Result<std::borrow::Cow<'a, [u8]>> {
                let mut out = vec![0u8; plaintext.len()];
                let mut out_len = 0;
                let res = unsafe {
                    codec_encode(
                        plaintext.as_ptr(),
                        plaintext.len(),
                        out.as_mut_ptr(),
                        out.len(),
                        &raw mut out_len,
                    )
                };
                assert_eq!(res, 0);
                Ok(std::borrow::Cow::Owned(out))
            }
            fn decode(&self, ciphertext: &[u8]) -> lilia_core::Result<Vec<u8>> {
                let mut out = vec![0u8; ciphertext.len()];
                let mut out_len = 0;
                let res = unsafe {
                    codec_decode(
                        ciphertext.as_ptr(),
                        ciphertext.len(),
                        out.as_mut_ptr(),
                        out.len(),
                        &raw mut out_len,
                    )
                };
                assert_eq!(res, 0);
                Ok(out)
            }
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("custom_plugin.lilia");

        let codec = Arc::new(CustomCodecWrapper);
        let options = DatabaseOptions::durable(&db_path).with_codec(codec);
        let db = Database::open(options).unwrap();

        db.batch(&[
            BatchOperation::KvSet {
                namespace: "config".into(),
                key: b"secret_token".to_vec(),
                value: b"super_secret_cleartext".to_vec(),
                if_version: None,
                expires_at_ms: None,
            },
            BatchOperation::JsonPut {
                space: "devices".into(),
                id: "dev-01".into(),
                value: json!({"ip": "192.168.1.100", "auth": "topsecret"}),
                if_version: None,
            },
        ])
        .unwrap();

        // Read transparently through database
        let kv_entry = db.kv_get("config", b"secret_token").unwrap().unwrap();
        assert_eq!(kv_entry.value, b"super_secret_cleartext");

        let json_entry = db.json_get("devices", "dev-01").unwrap().unwrap();
        assert_eq!(json_entry.value["auth"], "topsecret");

        drop(db);

        // Verify disk contains encrypted bytes (not cleartext)
        let raw_conn = rusqlite::Connection::open(&db_path).unwrap();
        let raw_kv: Vec<u8> = raw_conn
            .query_row(
                "SELECT value FROM _lilia_kv WHERE namespace = 'config' AND key = ?1",
                [b"secret_token"],
                |row| row.get(0),
            )
            .unwrap();
        assert_ne!(raw_kv, b"super_secret_cleartext");
    }
}
