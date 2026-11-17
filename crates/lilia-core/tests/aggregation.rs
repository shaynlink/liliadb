use lilia_core::doc;
use lilia_core::aggregation::{
    AnomalyAccumulator, CountAccumulator, HyperLogLog, MaxAccumulator, MinAccumulator,
    SumAccumulator, TDigest, WelfordAccumulator,
};
use serde_json::json;

#[test]
fn test_welford_statistical_accuracy() {
    let mut w = WelfordAccumulator::new();
    // Dataset: 2, 4, 4, 4, 5, 5, 7, 9
    // N = 8, Sum = 40, Mean = 5.0
    // Variance (sample) = 32 / 7 ≈ 4.57142857
    // Variance (population) = 32 / 8 = 4.0
    // StdDev (population) = 2.0
    let values = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
    for &x in &values {
        w.update(x);
    }

    assert_eq!(w.count(), 8);
    assert!((w.mean() - 5.0).abs() < 1e-9);
    assert!((w.population_variance() - 4.0).abs() < 1e-9);
    assert!((w.sample_variance() - (32.0 / 7.0)).abs() < 1e-9);
    assert!((w.std_dev() - 2.0).abs() < 1e-9);
}

#[test]
fn test_tdigest_quantiles_and_iqr() {
    let mut td = TDigest::new(100.0);
    for i in 1..=1000 {
        td.add(i as f64);
    }

    // Median should be close to 500.5
    let p50 = td.quantile(0.5);
    assert!((p50 - 500.0).abs() < 10.0, "p50 was {p50}");

    // p95 should be close to 950
    let p95 = td.quantile(0.95);
    assert!((p95 - 950.0).abs() < 15.0, "p95 was {p95}");

    // p99 should be close to 990
    let p99 = td.quantile(0.99);
    assert!((p99 - 990.0).abs() < 15.0, "p99 was {p99}");

    // IQR = Q3 (750) - Q1 (250) ≈ 500
    let iqr = td.iqr();
    assert!((iqr - 500.0).abs() < 20.0, "iqr was {iqr}");
}

#[test]
fn test_hyperloglog_distinct_count() {
    let mut hll = HyperLogLog::new();
    // Add 10,000 items with only 500 distinct values
    for i in 0..10_000 {
        let val = format!("user_{}", i % 500);
        hll.add(&val);
    }

    let estimated = hll.count();
    // Expected ~500 within ~5% error
    let diff = (estimated as f64 - 500.0).abs();
    assert!(diff < 35.0, "estimated {estimated} vs expected 500");
}

#[test]
fn test_anomaly_accumulator() {
    let mut anomaly = AnomalyAccumulator::new(3.0);
    // Feed 100 normal values ~ 50.0 with small noise
    for i in 0..100 {
        let val = 50.0 + (if i % 2 == 0 { 1.0 } else { -1.0 });
        anomaly.update(val);
    }

    // Should have 0 anomalies so far
    assert_eq!(anomaly.anomaly_count(), 0);

    // Feed extreme values: 1000.0 and -500.0
    anomaly.update(1000.0);
    anomaly.update(-500.0);

    assert_eq!(anomaly.anomaly_count(), 2);
}

#[test]
fn test_count_sum_min_max() {
    let mut count = CountAccumulator::new();
    let mut sum = SumAccumulator::new();
    let mut min = MinAccumulator::new();
    let mut max = MaxAccumulator::new();

    for &x in &[10.0, 5.0, 30.0, 15.0] {
        count.update();
        sum.update(x);
        min.update(x);
        max.update(x);
    }

    assert_eq!(count.value(), 4);
    assert_eq!(sum.value(), 60.0);
    assert_eq!(min.value(), Some(5.0));
    assert_eq!(max.value(), Some(30.0));
}

#[test]
fn test_filter_matching() {
    let filter = doc! {
        "status": "completed",
        "year": 2026,
        "amount": { "$gt": 50 }
    };

    let doc_match = json!({
        "status": "completed",
        "year": 2026,
        "amount": 75.5
    });

    let doc_mismatch_status = json!({
        "status": "pending",
        "year": 2026,
        "amount": 75.5
    });

    let doc_mismatch_amount = json!({
        "status": "completed",
        "year": 2026,
        "amount": 25.0
    });

    assert!(filter.matches_json(&doc_match));
    assert!(!filter.matches_json(&doc_mismatch_status));
    assert!(!filter.matches_json(&doc_mismatch_amount));
}
