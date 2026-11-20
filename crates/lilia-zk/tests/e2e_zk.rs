use std::collections::BTreeMap;

use lilia_zk::{u64_to_fr, ZkProver, ZkStatement, ZkVerifier};

#[test]
fn test_e2e_zk_solde_et_age_scenario() {
    // 1. Private User Data (secret witness)
    let doc_id = "usr_alice_42";
    let private_balance = 2450u64; // Solde réel : 2 450 €
    let private_age = 29u32; // Âge réel : 29 ans
    let private_salt = u64_to_fr(0xdead_beef_1234_5678);

    // 2. Condition to prove without revealing secret data:
    // "Solde >= 1 000 € et Âge >= 18 ans"
    let min_balance = 1000u64;
    let min_age = 18u32;

    println!("[1] Generating Zero-Knowledge Proof with micro-prover...");
    let start_proof = std::time::Instant::now();
    let proof_bundle = ZkProver::prove_solde_age(
        doc_id,
        private_balance,
        private_age,
        private_salt,
        min_balance,
        min_age,
        None, // Use default zero-config keys
    )
    .expect("Proof generation should succeed for valid witness");
    let proof_time = start_proof.elapsed();
    println!("    Prover elapsed time: {proof_time:?}");

    // 3. Verify that the proof is strictly 128 bytes (Groth16 BN254 compressed)!
    let proof_bytes = proof_bundle.proof_bytes().expect("Valid hex proof");
    assert_eq!(
        proof_bytes.len(),
        128,
        "Groth16 BN254 compressed proof must be strictly 128 bytes!"
    );
    assert_eq!(proof_bundle.proof_size_bytes, 128);
    println!("    Proof size: {} bytes (compact!)", proof_bytes.len());
    println!("    Proof hex prefix: {:.32}...", proof_bundle.proof_hex);
    println!("    Commitment: {}", proof_bundle.commitment_hex);

    // 4. Third-party verifier validates the proof in < 1 ms without seeing balance, age, or salt!
    println!("[2] Verifying proof with independent verifier...");
    let report = ZkVerifier::verify(&proof_bundle, None).expect("Verification execution succeeds");

    assert!(report.valid, "Mathematical proof must verify as valid");
    assert!(report.zero_knowledge, "Zero knowledge guarantee preserved");
    println!(
        "    Verification successful: valid={} in {:.3} ms (< 1 ms!)",
        report.valid, report.verification_time_ms
    );
    assert_eq!(
        report.certified_claims,
        vec!["balance >= 1000", "age >= 18"]
    );

    // 5. Soundness Test: Proving an impossible condition (e.g. balance >= 5000 when balance is 2450) must fail
    let invalid_prove = ZkProver::prove_solde_age(
        doc_id,
        private_balance,
        private_age,
        private_salt,
        5000, // min_balance = 5000 (false!)
        min_age,
        None,
    );
    assert!(
        invalid_prove.is_err(),
        "Prover must reject false condition (2450 < 5000)"
    );

    // 6. Soundness Test: Tampering with public commitment in proof bundle must fail verification
    let mut tampered_bundle = proof_bundle.clone();
    tampered_bundle.public_inputs[0] =
        "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef".to_string();
    let tampered_report = ZkVerifier::verify(&tampered_bundle, None).expect("Verifier runs");
    assert!(
        !tampered_report.valid,
        "Tampered commitment must NOT verify"
    );

    // 7. Soundness Test: Tampering with threshold in public inputs (e.g. claiming min_balance = 2000 was proved) must fail
    let mut tampered_thresh = proof_bundle.clone();
    tampered_thresh.public_inputs[1] = format!("0x{:016x}", 2000u64);
    let thresh_report = ZkVerifier::verify(&tampered_thresh, None).expect("Verifier runs");
    assert!(
        !thresh_report.valid,
        "Tampered public input must NOT verify"
    );
}

#[test]
fn test_declarative_statement_builder() {
    let doc_id = "patient_bob_77";
    let mut witness = BTreeMap::new();
    witness.insert("balance".to_string(), 1500u64);
    witness.insert("age".to_string(), 21u64);
    let salt = u64_to_fr(12345);

    let statement = ZkStatement::builder()
        .field_gte("balance", 1000)
        .field_gte("age", 18)
        .build();

    let bundle = ZkProver::prove_statement(doc_id, &statement, &witness, salt, None)
        .expect("Statement proof succeeds");

    let report = ZkVerifier::verify(&bundle, None).expect("Verification succeeds");
    assert!(report.valid);
    assert_eq!(bundle.proof_size_bytes, 128);
}
