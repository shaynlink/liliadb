//! Unit tests for Paillier key generation, encryption, decryption, and homomorphic operations.

use crate::fle::paillier::cipher::{decrypt_f64, decrypt_i64, encrypt_f64, encrypt_i64};
use crate::fle::paillier::homomorphic::{add, mul_scalar, rerandomize, sub};
use crate::fle::paillier::keys::PaillierKeypair;

#[test]
fn test_paillier_encrypt_decrypt_integers() {
    // Fast 512-bit key for unit testing
    let keypair = PaillierKeypair::generate(512).unwrap();
    let pk = &keypair.public_key;
    let sk = &keypair.private_key;

    // Positive numbers
    let c1 = encrypt_i64(pk, 42);
    let m1 = decrypt_i64(sk, pk, &c1).unwrap();
    assert_eq!(m1, 42);

    let c2 = encrypt_i64(pk, 1_000_000);
    let m2 = decrypt_i64(sk, pk, &c2).unwrap();
    assert_eq!(m2, 1_000_000);

    // Zero
    let c_zero = encrypt_i64(pk, 0);
    let m_zero = decrypt_i64(sk, pk, &c_zero).unwrap();
    assert_eq!(m_zero, 0);

    // Negative numbers
    let c_neg = encrypt_i64(pk, -550);
    let m_neg = decrypt_i64(sk, pk, &c_neg).unwrap();
    assert_eq!(m_neg, -550);
}

#[test]
fn test_paillier_randomized_confidentiality() {
    let keypair = PaillierKeypair::generate(512).unwrap();
    let pk = &keypair.public_key;

    let c1 = encrypt_i64(pk, 50_000);
    let c2 = encrypt_i64(pk, 50_000);

    assert_ne!(
        c1.raw(),
        c2.raw(),
        "identical plaintexts must produce different ciphertexts (IND-CPA)"
    );
}

#[test]
fn test_paillier_homomorphic_addition() {
    let keypair = PaillierKeypair::generate(512).unwrap();
    let pk = &keypair.public_key;
    let sk = &keypair.private_key;

    let a = 75_000;
    let b = 25_500;
    let c1 = encrypt_i64(pk, a);
    let c2 = encrypt_i64(pk, b);

    let c_sum = add(pk, &c1, &c2).unwrap();
    let decrypted_sum = decrypt_i64(sk, pk, &c_sum).unwrap();
    assert_eq!(decrypted_sum, a + b);

    // Addition with negative number
    let neg = -15_000;
    let c_neg = encrypt_i64(pk, neg);
    let c_sum2 = add(pk, &c_sum, &c_neg).unwrap();
    let decrypted_sum2 = decrypt_i64(sk, pk, &c_sum2).unwrap();
    assert_eq!(decrypted_sum2, a + b + neg);
}

#[test]
fn test_paillier_homomorphic_subtraction() {
    let keypair = PaillierKeypair::generate(512).unwrap();
    let pk = &keypair.public_key;
    let sk = &keypair.private_key;

    let a = 100_000;
    let b = 40_000;
    let c1 = encrypt_i64(pk, a);
    let c2 = encrypt_i64(pk, b);

    let c_diff = sub(pk, &c1, &c2).unwrap();
    let decrypted_diff = decrypt_i64(sk, pk, &c_diff).unwrap();
    assert_eq!(decrypted_diff, a - b);
}

#[test]
fn test_paillier_homomorphic_scalar_multiplication() {
    let keypair = PaillierKeypair::generate(512).unwrap();
    let pk = &keypair.public_key;
    let sk = &keypair.private_key;

    let a = 12_500;
    let c1 = encrypt_i64(pk, a);

    let c_mul = mul_scalar(pk, &c1, 3).unwrap();
    let decrypted_mul = decrypt_i64(sk, pk, &c_mul).unwrap();
    assert_eq!(decrypted_mul, a * 3);

    // Multiply by 0
    let c_zero = mul_scalar(pk, &c1, 0).unwrap();
    let dec_zero = decrypt_i64(sk, pk, &c_zero).unwrap();
    assert_eq!(dec_zero, 0);

    // Multiply by negative scalar
    let c_neg_mul = mul_scalar(pk, &c1, -2).unwrap();
    let dec_neg = decrypt_i64(sk, pk, &c_neg_mul).unwrap();
    assert_eq!(dec_neg, a * -2);
}

#[test]
fn test_paillier_fixed_point_floats() {
    let keypair = PaillierKeypair::generate(512).unwrap();
    let pk = &keypair.public_key;
    let sk = &keypair.private_key;

    let scale = 2; // cents
    let price1 = 19.99;
    let price2 = 5.50;

    let c1 = encrypt_f64(pk, price1, scale);
    let c2 = encrypt_f64(pk, price2, scale);

    let dec1 = decrypt_f64(sk, pk, &c1).unwrap();
    assert!((dec1 - price1).abs() < 1e-4);

    let c_sum = add(pk, &c1, &c2).unwrap();
    let dec_sum = decrypt_f64(sk, pk, &c_sum).unwrap();
    assert!((dec_sum - (price1 + price2)).abs() < 1e-4);
}

#[test]
fn test_paillier_rerandomize() {
    let keypair = PaillierKeypair::generate(512).unwrap();
    let pk = &keypair.public_key;
    let sk = &keypair.private_key;

    let val = 88_888;
    let c1 = encrypt_i64(pk, val);
    let c2 = rerandomize(pk, &c1);

    assert_ne!(
        c1.raw(),
        c2.raw(),
        "re-randomized ciphertext must differ in bytes"
    );
    assert_eq!(decrypt_i64(sk, pk, &c2).unwrap(), val);
}

#[test]
fn test_paillier_serialization_roundtrip() {
    let keypair = PaillierKeypair::generate(512).unwrap();
    let pk = &keypair.public_key;

    let pk_b64 = pk.to_base64();
    let restored_pk = crate::fle::paillier::keys::PaillierPublicKey::from_base64(&pk_b64).unwrap();
    assert_eq!(pk.n(), restored_pk.n());

    let c = encrypt_f64(pk, 123.45, 2);
    let c_b64 = c.to_base64();
    let restored_c = crate::fle::paillier::cipher::PaillierCiphertext::from_base64(&c_b64).unwrap();
    assert_eq!(c.raw(), restored_c.raw());
    assert_eq!(c.scale(), restored_c.scale());
}
