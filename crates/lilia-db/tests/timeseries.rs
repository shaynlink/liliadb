//! Time series: appends, replacement, ranges, statistics, resolutions, lifecycle policies,
//! batches, transactions, security, ledger, concurrency.

mod common;

use std::sync::Arc;

use common::{small_options, Rng};
use lilia_db::{
    Batch, Database, DownsampleAggregation, Error, LedgerVerifyMode, Policy, SecurityContext,
    TimeSeriesLifecyclePolicy, TimeSeriesPoint, TimeSeriesRetentionRule, TimeSeriesWrite, Write,
};
use lilia_engine::Durability;

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;

fn open(dir: &std::path::Path, ledger: bool) -> Database {
    Database::open(dir, small_options(Durability::Performance, ledger)).unwrap()
}

fn p(timestamp: i64, value: f64) -> TimeSeriesPoint {
    TimeSeriesPoint::new(timestamp, value)
}

fn value(timestamp: i64) -> f64 {
    f64::from(i32::try_from(timestamp / 1000 % 100).unwrap())
}

fn times(points: &[TimeSeriesPoint]) -> Vec<i64> {
    points.iter().map(|p| p.timestamp).collect()
}

#[test]
fn points_are_stored_replaced_deleted_and_summarized() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    let all: Vec<i64> = (0..5000).map(|i| i * 1000).collect();
    {
        let s = db.system();
        let hr = s.series("hr");
        let mut shuffled = all.clone();
        let mut rng = Rng(7);
        for i in (1..shuffled.len()).rev() {
            shuffled.swap(i, usize::try_from(rng.below(i as u64 + 1)).unwrap());
        }
        for part in shuffled.chunks(700) {
            let points: Vec<_> = part.iter().map(|&t| p(t, value(t))).collect();
            hr.append(&points).unwrap();
        }
        let got = hr.range(i64::MIN, i64::MAX).unwrap();
        let expected: Vec<_> = all.iter().map(|&t| p(t, value(t))).collect();
        assert_eq!(got, expected, "values are stored exactly");
        assert_eq!(
            times(&hr.range(10_000, 12_000).unwrap()),
            [10_000, 11_000, 12_000]
        );
        assert_eq!(hr.range(5, 999).unwrap(), []);
        assert_eq!(hr.range(12_000, 10_000).unwrap(), []);

        let (a, b) = (123_456, 4_321_000);
        let inside: Vec<f64> = all
            .iter()
            .filter(|&&t| t >= a && t <= b)
            .map(|&t| value(t))
            .collect();
        let stats = hr.stats(a, b).unwrap().unwrap();
        assert_eq!(stats.count, inside.len() as u64);
        assert_eq!(stats.sum, inside.iter().sum::<f64>());
        assert_eq!((stats.min, stats.max), (0.0, 99.0));
        assert!(hr.stats(-10, -1).unwrap().is_none());

        hr.append(&[p(10_000, -5.0), p(10_000, 99.5)]).unwrap();
        assert_eq!(
            hr.range(10_000, 10_000).unwrap(),
            [p(10_000, 99.5)],
            "the last value wins"
        );
        assert!(hr.delete_range(1_000_000, 2_999_999).unwrap());
        assert!(!hr.delete_range(1_000_000, 2_999_999).unwrap());
        assert_eq!(hr.range(i64::MIN, i64::MAX).unwrap().len(), 3000);
        assert_eq!(hr.stats(i64::MIN, i64::MAX).unwrap().unwrap().count, 3000);

        assert!(matches!(
            hr.append(&[p(0, f64::NAN)]),
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            hr.append_at(-1, &[p(0, 1.0)]),
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            hr.delete_range(2, 1),
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            s.series("").range(0, 1),
            Err(Error::InvalidArgument(_))
        ));

        let minutes: Vec<_> = (0..84).map(|m| p(m * MINUTE, 1.0)).collect();
        hr.append_at(MINUTE, &minutes).unwrap();
        assert_eq!(hr.resolutions().unwrap(), [0, MINUTE]);
        assert_eq!(
            hr.auto(0, 5_000_000, 100).unwrap().len(),
            84,
            "raw has 3000"
        );
        assert_eq!(hr.auto(0, 5_000_000, 10_000).unwrap().len(), 3000);
        assert_eq!(hr.auto(0, 5_000_000, 0).unwrap().len(), 3000);
        assert_eq!(
            hr.auto(0, 5_000_000, 10).unwrap().len(),
            84,
            "else the coarsest"
        );
        assert_eq!(hr.auto(-10, -1, 10).unwrap(), []);
        assert_eq!(s.series("none").resolutions().unwrap(), Vec::<i64>::new());
    }
    db.close().unwrap();

    let db = open(dir.path(), false);
    let s = db.system();
    let got = s.series("hr").range(i64::MIN, i64::MAX).unwrap();
    assert_eq!(got.len(), 3000);
    assert_eq!(got[10], p(10_000, 99.5));
}

#[test]
fn extreme_timestamps_and_large_appends_keep_their_order() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    let s = db.system();
    let series = s.series("edges");
    series
        .append(&[p(i64::MAX, 1.0), p(i64::MIN, 2.0), p(0, 3.0)])
        .unwrap();
    let points: Vec<_> = (1..=20_000).map(|i| p(i, 0.5)).collect();
    series.append(&points).unwrap();
    let got = series.range(i64::MIN, i64::MAX).unwrap();
    assert_eq!(got.len(), 20_003);
    assert_eq!(got.first(), Some(&p(i64::MIN, 2.0)));
    assert_eq!(got.last(), Some(&p(i64::MAX, 1.0)));
    assert!(got.windows(2).all(|w| w[0].timestamp < w[1].timestamp));
    assert_eq!(
        series.range(i64::MAX, i64::MAX).unwrap(),
        [p(i64::MAX, 1.0)]
    );
    assert!(series.delete_range(i64::MIN, i64::MAX).unwrap());
    assert_eq!(series.range(i64::MIN, i64::MAX).unwrap(), []);
}

fn three_tier_policy() -> TimeSeriesLifecyclePolicy {
    TimeSeriesLifecyclePolicy::new(
        "patient:*",
        vec![
            TimeSeriesRetentionRule::new(1_000, Some(DAY), DownsampleAggregation::Avg),
            TimeSeriesRetentionRule::new(MINUTE, Some(30 * DAY), DownsampleAggregation::Avg),
            TimeSeriesRetentionRule::new(HOUR, None, DownsampleAggregation::Max),
        ],
    )
}

#[test]
fn lifecycle_policies_downsample_and_expire_tiers() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), true);
    db.set_series_policy(&three_tier_policy()).unwrap();
    db.set_series_policy(&TimeSeriesLifecyclePolicy::new(
        "*",
        vec![TimeSeriesRetentionRule::new(
            0,
            Some(1),
            DownsampleAggregation::Avg,
        )],
    ))
    .unwrap();
    assert_eq!(
        db.series_policy("patient:*").unwrap(),
        Some(three_tier_policy())
    );
    assert_eq!(db.series_policies().unwrap().len(), 2);
    assert!(matches!(
        db.set_series_policy(&TimeSeriesLifecyclePolicy::new("x", vec![])),
        Err(Error::InvalidArgument(_))
    ));

    // Two hours of 1 Hz points, from an hour boundary.
    let base = 472_222 * HOUR;
    let s = db.system();
    let hr = s.series("patient:hr");
    let points: Vec<_> = (0..7200i64)
        .map(|i| {
            p(
                base + i * 1000,
                60.0 + f64::from(i32::try_from(i % 20).unwrap()),
            )
        })
        .collect();
    hr.append(&points).unwrap();

    // An hour in, only the first hour's buckets are closed; nothing is old enough to expire.
    let early = db.apply_series_retention_at(base + HOUR + 1).unwrap();
    assert_eq!(
        early.downsampled_points_created, 61,
        "60 minutes and 1 hour"
    );
    assert_eq!(early.chunks_pruned, 0);
    assert_eq!(hr.range(base, base + 2 * HOUR).unwrap().len(), 7200);

    // A day and three hours later every bucket is closed and the raw points have expired.
    let later = db.apply_series_retention_at(base + 27 * HOUR).unwrap();
    assert_eq!(later.series_processed, 1, "the other series is empty");
    assert_eq!(later.downsampled_points_created, 61);
    assert!(later.chunks_pruned > 0 && later.bytes_reclaimed_approx > 0);
    assert_eq!(hr.range(i64::MIN, i64::MAX).unwrap(), []);
    let minutes = hr.range_at(MINUTE, i64::MIN, i64::MAX).unwrap();
    assert_eq!(
        times(&minutes),
        (0..120).map(|m| base + m * MINUTE).collect::<Vec<_>>()
    );
    assert!(minutes.iter().all(|m| (m.value - 69.5).abs() < 1e-9));
    let hours = hr.range_at(HOUR, i64::MIN, i64::MAX).unwrap();
    assert_eq!(hours, [p(base, 69.5), p(base + HOUR, 69.5)]);
    assert_eq!(hr.resolutions().unwrap(), [MINUTE, HOUR]);

    let again = db.apply_series_retention_at(base + 27 * HOUR).unwrap();
    assert_eq!(
        (again.downsampled_points_created, again.chunks_pruned),
        (0, 0)
    );

    // A month on, the minutes expire too; hours are kept for good.
    db.apply_series_retention_at(base + 32 * DAY).unwrap();
    assert_eq!(hr.resolutions().unwrap(), [HOUR]);
    assert_eq!(hr.range_at(HOUR, i64::MIN, i64::MAX).unwrap().len(), 2);

    // The catch-all policy governs other series: its raw points live 1 ms.
    s.series("other").append(&[p(0, 1.0), p(10, 2.0)]).unwrap();
    db.apply_series_retention_at(10).unwrap();
    assert_eq!(
        s.series("other").range(i64::MIN, i64::MAX).unwrap(),
        [p(10, 2.0)]
    );
    assert!(db.delete_series_policy("*").unwrap());
    assert!(!db.delete_series_policy("*").unwrap());
    db.apply_series_retention_at(i64::MAX).unwrap();
    assert_eq!(
        s.series("other").range(i64::MIN, i64::MAX).unwrap().len(),
        1
    );
    assert!(db.verify_ledger(LedgerVerifyMode::Strict).unwrap().is_valid);
}

#[test]
fn batches_and_transactions_see_their_own_points() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    let s = db.system();

    let mut batch = Batch::new();
    batch.push(Write::TimeSeries(TimeSeriesWrite::Append {
        series: "t".into(),
        resolution_ms: 0,
        points: (0..3000).map(|i| p(i, 1.0)).collect(),
    }));
    batch.push(Write::TimeSeries(TimeSeriesWrite::DeleteRange {
        series: "t".into(),
        resolution_ms: 0,
        start: 1000,
        end: 1999,
    }));
    batch.kv_put("meta", "t", "ready");
    let outcomes = s.commit(&batch).unwrap();
    assert!(outcomes[0].version.is_some() && outcomes[1].deleted);
    assert_eq!(s.series("t").range(i64::MIN, i64::MAX).unwrap().len(), 2000);

    let tx = s.transaction();
    let t = tx.series("t");
    t.append(&[p(5000, 5.0)]).unwrap();
    t.delete_range(0, 999).unwrap();
    assert_eq!(times(&t.range(4000, 6000).unwrap()), [5000]);
    assert_eq!(t.stats(i64::MIN, i64::MAX).unwrap().unwrap().count, 1001);
    assert_eq!(
        s.series("t").range(4000, 6000).unwrap(),
        [],
        "not committed yet"
    );
    tx.commit().unwrap();
    assert_eq!(
        s.series("t")
            .stats(i64::MIN, i64::MAX)
            .unwrap()
            .unwrap()
            .count,
        1001
    );

    // A transaction that read points conflicts with a writer that changed them.
    let tx = s.transaction();
    assert_eq!(tx.series("t").range(5000, 5000).unwrap().len(), 1);
    s.series("t").append(&[p(5001, 1.0)]).unwrap();
    tx.series("t").append(&[p(6000, 1.0)]).unwrap();
    assert!(matches!(tx.commit(), Err(Error::Conflict { .. })));

    // Transactions that only append to one series both commit.
    let a = s.transaction();
    let b = s.transaction();
    a.series("t").append(&[p(7000, 1.0)]).unwrap();
    b.series("t").append(&[p(7001, 1.0)]).unwrap();
    a.commit().unwrap();
    b.commit().unwrap();
    assert_eq!(
        times(&s.series("t").range(7000, 7001).unwrap()),
        [7000, 7001]
    );
}

#[test]
fn reads_pass_the_callers_policies_and_chunks_are_ledger_records() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), true);
    let s = db.system();
    s.series("vitals").append(&[p(1, 1.0), p(2, 2.0)]).unwrap();
    db.create_policy(
        "clinicians",
        Policy::new("vitals", "current_context('role') == 'clinician'"),
    )
    .unwrap();
    let clinician = db.session(SecurityContext::from_pair("role", "clinician"));
    let visitor = db.session(SecurityContext::from_pair("role", "visitor"));
    assert_eq!(clinician.series("vitals").range(0, 10).unwrap().len(), 2);
    assert_eq!(visitor.series("vitals").range(0, 10).unwrap(), []);
    assert!(visitor.series("vitals").stats(0, 10).unwrap().is_none());
    assert_eq!(
        visitor.series("vitals").resolutions().unwrap(),
        Vec::<i64>::new()
    );

    let proof = db.prove_series("vitals", 0, 2).unwrap();
    assert_eq!(proof.delta.id, format!("0:{}", i64::MAX));
    assert!(matches!(
        db.prove_series("vitals", 0, 3),
        Err(Error::NotFound(_))
    ));
    s.series("vitals").delete_range(1, 1).unwrap();
    assert!(db.prove_series("vitals", 0, 2).unwrap().commit.commit_seq > proof.commit.commit_seq);
    let report = db.verify_ledger(LedgerVerifyMode::Strict).unwrap();
    assert!(report.is_valid, "{:?}", report.error);
}

#[test]
fn concurrent_writers_leave_every_point() {
    for ledger in [false, true] {
        concurrent_writers(ledger);
    }
}

fn concurrent_writers(ledger: bool) {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(open(dir.path(), ledger));
    let threads: Vec<_> = (0..8u64)
        .map(|w| {
            let db = Arc::clone(&db);
            std::thread::spawn(move || {
                // Writer `w` owns the timestamps `8 * slot + w`, written in a shuffled order so
                // writers keep landing in each other's chunks.
                let mut slots: Vec<u64> = (0..1000).collect();
                let mut rng = Rng(w + 1);
                for i in (1..slots.len()).rev() {
                    slots.swap(i, usize::try_from(rng.below(i as u64 + 1)).unwrap());
                }
                let s = db.system();
                let series = s.series("shared");
                for part in slots.chunks(25) {
                    let points: Vec<_> = part
                        .iter()
                        .map(|&slot| p(i64::try_from(8 * slot + w).unwrap(), 1.0))
                        .collect();
                    series.append(&points).unwrap();
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let s = db.system();
    let got = s.series("shared").range(i64::MIN, i64::MAX).unwrap();
    assert_eq!(times(&got), (0..8000).collect::<Vec<_>>());
    assert_eq!(
        s.series("shared").stats(0, 7999).unwrap().unwrap().sum,
        8000.0
    );
    if ledger {
        let report = db.verify_ledger(LedgerVerifyMode::Strict).unwrap();
        assert!(report.is_valid, "{:?}", report.error);
    }
}
