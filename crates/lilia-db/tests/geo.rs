//! Geo: fences found by their points, GPS tracks, H3 proximity, distances, batches,
//! transactions, security, ledger, concurrency.

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::{small_options, Rng};
use lilia_core::geo::{circle_to_geopoly_json, haversine_distance_meters};
use lilia_core::h3::{k_ring, lat_lng_to_cell};
use lilia_db::{
    Database, Error, Fence, GpsPoint, LedgerVerifyMode, Policy, Polygon, SecurityContext,
};
use lilia_engine::Durability;

fn open(dir: &std::path::Path, ledger: bool) -> Database {
    Database::open(dir, small_options(Durability::Performance, ledger)).unwrap()
}

fn fence(name: &str, category: &str, polygon: Polygon) -> Fence {
    Fence {
        name: name.into(),
        category: category.into(),
        polygon,
        metadata: None,
    }
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Polygon {
    Polygon::new(&[(x0, y0), (x1, y0), (x1, y1), (x0, y1)]).unwrap()
}

fn ids(matches: &[lilia_db::GeofenceMatch]) -> Vec<&str> {
    matches.iter().map(|m| m.id.as_str()).collect()
}

/// A uniform value in `[low, high)`.
fn uniform(rng: &mut Rng, low: f64, high: f64) -> f64 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "values below 2^24, exact in f64"
    )]
    let unit = rng.below(1 << 24) as f64 / f64::from(1 << 24);
    low + unit * (high - low)
}

#[test]
fn fences_are_found_by_the_points_they_contain() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    {
        let s = db.system();
        let g = s.geo("zones");
        let park = Polygon::parse(&circle_to_geopoly_json(2.2945, 48.8584, 500.0, 32)).unwrap();
        g.put_fence("park", &fence("Champ de Mars", "running", park))
            .unwrap();
        g.put_fence(
            "city",
            &fence("Paris", "city", rect(2.22, 48.81, 2.47, 48.91)),
        )
        .unwrap();
        g.put_fence(
            "france",
            &fence("France", "country", rect(-5.0, 42.0, 8.0, 51.0)),
        )
        .unwrap();
        assert_eq!(
            ids(&g.fences_at(2.2945, 48.8584).unwrap()),
            ["city", "france", "park"]
        );
        assert_eq!(ids(&g.fences_at(2.40, 48.85).unwrap()), ["city", "france"]);
        assert_eq!(
            ids(&g.fences_at(2.47, 48.85).unwrap()),
            ["city", "france"],
            "boundary"
        );
        assert_eq!(ids(&g.fences_at(-74.0, 40.7).unwrap()), Vec::<&str>::new());
        assert_eq!(g.fence("park").unwrap().unwrap().category, "running");
        assert!(g.fence("none").unwrap().is_none());

        // Moving a fence moves its grid entries.
        g.put_fence(
            "city",
            &fence("Lyon", "city", rect(4.77, 45.70, 4.90, 45.80)),
        )
        .unwrap();
        assert_eq!(ids(&g.fences_at(2.40, 48.85).unwrap()), ["france"]);
        assert_eq!(ids(&g.fences_at(4.83, 45.76).unwrap()), ["city", "france"]);
        assert!(g.remove_fence("france").unwrap());
        assert!(!g.remove_fence("france").unwrap());
        assert_eq!(ids(&g.fences_at(4.83, 45.76).unwrap()), ["city"]);
        assert!(matches!(
            g.fences_at(200.0, 0.0),
            Err(Error::InvalidArgument(_))
        ));
        assert!(Polygon::parse("not json").is_err());
    }
    db.close().unwrap();
    let db = open(dir.path(), false);
    let s = db.system();
    assert_eq!(
        ids(&s.geo("zones").fences_at(2.2945, 48.8584).unwrap()),
        ["park"]
    );
}

#[test]
fn the_fence_grid_finds_what_a_full_scan_finds() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    let s = db.system();
    let g = s.geo("random");
    let mut rng = Rng(11);
    let mut fences = Vec::new();
    for i in 0..300 {
        // Sizes from meters to continents.
        let size = 10f64.powf(uniform(&mut rng, -4.0, 1.7));
        let (x, y) = (
            uniform(&mut rng, -170.0, 170.0 - size),
            uniform(&mut rng, -80.0, 80.0 - size),
        );
        let polygon = if i % 2 == 0 {
            rect(x, y, x + size, y + size)
        } else {
            Polygon::new(&[(x, y), (x + size, y), (x + size / 2.0, y + size)]).unwrap()
        };
        let id = format!("f{i:03}");
        g.put_fence(&id, &fence(&id, "zone", polygon.clone()))
            .unwrap();
        fences.push((id, polygon));
    }
    for _ in 0..2000 {
        // Points near a fence's corner, so that most queries hit something.
        let (_, target) = &fences[usize::try_from(rng.below(300)).unwrap()];
        let (x, y) = target.vertices()[0];
        let (lon, lat) = (
            x + uniform(&mut rng, -0.01, 0.05),
            y + uniform(&mut rng, -0.01, 0.05),
        );
        let expected: Vec<&str> = fences
            .iter()
            .filter(|(_, p)| p.contains(lon, lat))
            .map(|(id, _)| id.as_str())
            .collect();
        assert_eq!(
            ids(&g.fences_at(lon, lat).unwrap()),
            expected,
            "({lon}, {lat})"
        );
    }
}

#[test]
fn tracks_are_recorded_queried_measured_and_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    let s = db.system();
    let g = s.geo("runs");
    g.put_fence(
        "park",
        &fence("Park", "running", rect(2.30, 48.86, 2.31, 48.87)),
    )
    .unwrap();
    let points: Vec<GpsPoint> = (0..60)
        .map(|i| {
            let step = f64::from(i) * 0.0002;
            GpsPoint::with_details(
                1000 * i64::from(i),
                48.84 + step,
                2.28 + step,
                Some(35.0),
                Some(3.0),
                None,
            )
        })
        .collect();
    let at_end = g.record("morning", &points).unwrap();
    assert_eq!(
        ids(&at_end),
        Vec::<&str>::new(),
        "the newest point is past the park"
    );
    let inside = g
        .record("morning", &[GpsPoint::new(-1, 48.865, 2.305)])
        .unwrap();
    assert_eq!(ids(&inside), ["park"]);

    let track = g.track("morning", 0, i64::MAX).unwrap();
    assert_eq!(track.len(), 60);
    assert_eq!(track[1].altitude, Some(35.0));
    let default_cell = lat_lng_to_cell(track[1].latitude, track[1].longitude, 9).unwrap();
    assert_eq!(track[1].h3_index, Some(default_cell));
    assert_eq!(g.track("morning", 5000, 7000).unwrap().len(), 3);
    let expected: f64 = track
        .windows(2)
        .map(|w| {
            haversine_distance_meters(w[0].latitude, w[0].longitude, w[1].latitude, w[1].longitude)
        })
        .sum::<f64>()
        + haversine_distance_meters(48.865, 2.305, 48.84, 2.28);
    assert!((g.track_distance("morning").unwrap() - expected).abs() < 1e-6);

    // Replacing a point moves it to its new cell.
    g.record("morning", &[GpsPoint::new(0, 10.0, 10.0)])
        .unwrap();
    let paris = lat_lng_to_cell(48.84, 2.28, 9).unwrap();
    let far = lat_lng_to_cell(10.0, 10.0, 9).unwrap();
    assert_eq!(g.near(paris, 0, 0, 0).unwrap(), []);
    assert_eq!(g.near(far, 0, 0, 0).unwrap()[0].point.latitude, 10.0);

    assert!(g.delete_track("morning", 0, 29_000).unwrap());
    assert_eq!(g.track("morning", i64::MIN, i64::MAX).unwrap().len(), 31);
    assert_eq!(g.near(far, 0, i64::MIN, i64::MAX).unwrap(), []);
    assert!(matches!(
        g.record("morning", &[GpsPoint::new(0, 91.0, 0.0)]),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        g.delete_track("morning", 2, 1),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        g.near(12345, 1, 0, 1),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn proximity_matches_the_cells_of_every_point() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    let s = db.system();
    let g = s.geo("fleet");
    let mut rng = Rng(5);
    let mut all = Vec::new();
    for track in ["a", "b", "c"] {
        let points: Vec<GpsPoint> = (0..400)
            .map(|t| {
                GpsPoint::new(
                    t,
                    uniform(&mut rng, 48.80, 48.92),
                    uniform(&mut rng, 2.22, 2.47),
                )
            })
            .collect();
        g.record(track, &points).unwrap();
        all.extend(points.into_iter().map(|p| (track, p)));
    }
    for (resolution, k) in [
        (5u8, 0u32),
        (6, 0),
        (7, 0),
        (8, 0),
        (9, 1),
        (9, 3),
        (10, 0),
        (12, 2),
        (15, 0),
    ] {
        for _ in 0..20 {
            let (_, probe) = &all[usize::try_from(rng.below(1200)).unwrap()];
            let center = lat_lng_to_cell(probe.latitude, probe.longitude, resolution).unwrap();
            let around: BTreeSet<u64> = k_ring(center, k).unwrap().into_iter().collect();
            let mut expected: Vec<(i64, String)> = all
                .iter()
                .filter(|(_, p)| p.timestamp >= 100 && p.timestamp <= 300)
                .filter(|(_, p)| {
                    around.contains(&lat_lng_to_cell(p.latitude, p.longitude, resolution).unwrap())
                })
                .map(|(track, p)| (p.timestamp, (*track).to_owned()))
                .collect();
            expected.sort_unstable();
            let found: Vec<(i64, String)> = g
                .near(center, k, 100, 300)
                .unwrap()
                .into_iter()
                .map(|t| (t.point.timestamp, t.track))
                .collect();
            assert_eq!(found, expected, "resolution {resolution}, k {k}");
        }
    }
}

#[test]
fn transactions_see_their_own_fences_and_points() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    let s = db.system();
    let tx = s.transaction();
    let g = tx.geo("zones");
    g.put_fence("home", &fence("Home", "home", rect(0.0, 0.0, 1.0, 1.0)))
        .unwrap();
    assert_eq!(
        ids(&g.record("me", &[GpsPoint::new(1, 0.5, 0.5)]).unwrap()),
        ["home"]
    );
    assert_eq!(g.track("me", 0, 10).unwrap().len(), 1);
    let cell = lat_lng_to_cell(0.5, 0.5, 9).unwrap();
    assert_eq!(g.near(cell, 0, 0, 10).unwrap().len(), 1);
    assert!(
        s.geo("zones").fences_at(0.5, 0.5).unwrap().is_empty(),
        "not committed"
    );
    tx.commit().unwrap();
    assert_eq!(ids(&s.geo("zones").fences_at(0.5, 0.5).unwrap()), ["home"]);
    assert_eq!(s.geo("zones").near(cell, 0, 0, 10).unwrap()[0].track, "me");
}

#[test]
fn reads_pass_the_callers_policies_and_geo_records_are_in_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), true);
    let s = db.system();
    let g = s.geo("zones");
    g.put_fence("home", &fence("Home", "private", rect(0.0, 0.0, 1.0, 1.0)))
        .unwrap();
    g.put_fence("shop", &fence("Shop", "public", rect(0.0, 0.0, 2.0, 2.0)))
        .unwrap();
    g.record("ann", &[GpsPoint::new(1, 0.5, 0.5)]).unwrap();
    g.record("bob", &[GpsPoint::new(1, 0.5, 0.5)]).unwrap();
    db.create_policy(
        "visible",
        Policy::new(
            "zones",
            "$.category == 'public' || $.track == current_context('user')",
        ),
    )
    .unwrap();
    let bob = db.session(SecurityContext::from_pair("user", "bob"));
    assert_eq!(
        ids(&bob.geo("zones").fences_at(0.5, 0.5).unwrap()),
        ["shop"]
    );
    assert!(bob.geo("zones").fence("home").unwrap().is_none());
    assert_eq!(bob.geo("zones").track("ann", 0, 10).unwrap(), []);
    let cell = lat_lng_to_cell(0.5, 0.5, 9).unwrap();
    let near = bob.geo("zones").near(cell, 0, 0, 10).unwrap();
    assert_eq!(
        near.iter().map(|t| t.track.as_str()).collect::<Vec<_>>(),
        ["bob"]
    );

    let fence_proof = db.prove_fence("zones", "home").unwrap();
    assert_eq!(
        (
            fence_proof.delta.model.as_str(),
            fence_proof.delta.id.as_str()
        ),
        ("geo_fence", "home")
    );
    let point_proof = db.prove_gps_point("zones", "ann", 1).unwrap();
    assert_eq!(point_proof.delta.id, "ann@1");
    assert!(matches!(
        db.prove_gps_point("zones", "ann", 2),
        Err(Error::NotFound(_))
    ));
    s.geo("zones").delete_track("ann", 0, 10).unwrap();
    let report = db.verify_ledger(LedgerVerifyMode::Strict).unwrap();
    assert!(report.is_valid, "{:?}", report.error);
}

#[test]
fn concurrent_writers_keep_one_cell_entry_per_point() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(open(dir.path(), true));
    let threads: Vec<_> = (0..6u64)
        .map(|w| {
            let db = Arc::clone(&db);
            std::thread::spawn(move || {
                let s = db.system();
                let g = s.geo("fleet");
                let mut rng = Rng(w + 1);
                // Every writer records and re-records the same timestamps at new positions.
                for _ in 0..40 {
                    let points: Vec<GpsPoint> = (0..5)
                        .map(|_| {
                            GpsPoint::new(
                                i64::try_from(rng.below(50)).unwrap(),
                                uniform(&mut rng, 48.80, 48.92),
                                uniform(&mut rng, 2.22, 2.47),
                            )
                        })
                        .collect();
                    g.record("shared", &points).unwrap();
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let s = db.system();
    let g = s.geo("fleet");
    let track = g.track("shared", i64::MIN, i64::MAX).unwrap();
    let region = lat_lng_to_cell(48.86, 2.35, 2).unwrap();
    let near = g.near(region, 1, i64::MIN, i64::MAX).unwrap();
    assert_eq!(near.len(), track.len(), "one cell entry per point");
    for (found, point) in near.iter().zip(&track) {
        assert_eq!(&found.point, point);
        let cell = lat_lng_to_cell(point.latitude, point.longitude, 15).unwrap();
        assert_eq!(
            g.near(cell, 0, point.timestamp, point.timestamp)
                .unwrap()
                .len(),
            1
        );
    }
    let report = db.verify_ledger(LedgerVerifyMode::Strict).unwrap();
    assert!(report.is_valid, "{:?}", report.error);
}
