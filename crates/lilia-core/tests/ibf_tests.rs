use lilia_core::ibf::{
    compute_checksum, get_hash_buckets, InvertibleBloomFilter, ReconciliationItem, StrataEstimator,
    DEFAULT_IBF_SEED,
};

#[test]
fn test_ibf_zero_difference_sets() {
    let mut ibf_a = InvertibleBloomFilter::new(50, 3, DEFAULT_IBF_SEED).unwrap();
    let mut ibf_b = InvertibleBloomFilter::new(50, 3, DEFAULT_IBF_SEED).unwrap();

    for i in 0..100 {
        let item = ReconciliationItem::new(i, 42).to_u128();
        ibf_a.insert(item);
        ibf_b.insert(item);
    }

    ibf_a.subtract(&ibf_b).unwrap();
    let diff = ibf_a.decode().unwrap();
    assert!(diff.is_empty());
    assert_eq!(diff.total_diff(), 0);
}

#[test]
fn test_strata_estimator_normal_path_with_sized_ibf_reconciliation() {
    let mut strata_a = StrataEstimator::with_default_params(DEFAULT_IBF_SEED);
    let mut strata_b = StrataEstimator::with_default_params(DEFAULT_IBF_SEED);

    // 5000 common documents
    for i in 0..5000 {
        let item = ReconciliationItem::new(i, 100).to_u128();
        strata_a.insert(item);
        strata_b.insert(item);
    }

    // Divergence: Node A has 60 unique docs, Node B has 40 unique docs (total d = 100)
    let mut a_only = Vec::new();
    for i in 0..60 {
        let item = ReconciliationItem::new(100_000 + i, 1).to_u128();
        a_only.push(item);
        strata_a.insert(item);
    }

    let mut b_only = Vec::new();
    for i in 0..40 {
        let item = ReconciliationItem::new(200_000 + i, 2).to_u128();
        b_only.push(item);
        strata_b.insert(item);
    }

    // Step 1: Strata estimation
    let estimate = strata_a.estimate_difference(&strata_b).unwrap();
    println!("Estimate result: {:?}", estimate);
    assert!(
        !estimate.fully_decoded,
        "100 differences should exceed stratum 0 capacity"
    );
    // The estimate should be within a factor of 2 of true difference d=100
    assert!(
        estimate.estimated_diff >= 40 && estimate.estimated_diff <= 250,
        "estimate was {}",
        estimate.estimated_diff
    );

    // Step 2: Sized IBF based on estimate
    let mut ibf_a = InvertibleBloomFilter::with_expected_diff(estimate.estimated_diff).unwrap();
    let mut ibf_b = InvertibleBloomFilter::with_expected_diff(estimate.estimated_diff).unwrap();

    // Populate sized IBFs
    for i in 0..5000 {
        let item = ReconciliationItem::new(i, 100).to_u128();
        ibf_a.insert(item);
        ibf_b.insert(item);
    }
    for &item in &a_only {
        ibf_a.insert(item);
    }
    for &item in &b_only {
        ibf_b.insert(item);
    }

    // Step 3: Subtract & Decode
    ibf_a.subtract(&ibf_b).unwrap();
    let diff = ibf_a.decode().expect("sized IBF must peel successfully");

    assert_eq!(diff.a_minus_b.len(), 60);
    assert_eq!(diff.b_minus_a.len(), 40);

    for &item in &a_only {
        assert!(diff.a_minus_b.contains(&item));
    }
    for &item in &b_only {
        assert!(diff.b_minus_a.contains(&item));
    }
}

#[test]
fn test_hash_buckets_deterministic_and_distinct() {
    let key = 0x1234_5678_9abc_def0_fedc_ba98_7654_3210_u128;
    let buckets1 = get_hash_buckets(key, 128, 4, DEFAULT_IBF_SEED);
    let buckets2 = get_hash_buckets(key, 128, 4, DEFAULT_IBF_SEED);

    assert_eq!(buckets1, buckets2);
    assert_eq!(buckets1.len(), 4);

    let mut dedup = buckets1.clone();
    dedup.sort_unstable();
    dedup.dedup();
    assert_eq!(dedup.len(), 4, "all 4 buckets must be distinct");
}

#[test]
fn test_checksum_collision_resistance() {
    let key1 = 1u128;
    let key2 = 2u128;
    assert_ne!(compute_checksum(key1), compute_checksum(key2));
}
