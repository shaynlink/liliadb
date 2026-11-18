#![cfg(test)]

use serde_json::json;

use super::causality::{detect_causality, CausalRelation};
use super::clock::HybridClock;
use super::merge::three_way_merge;
use super::timestamp::{Hlc, HlcError};

#[test]
fn test_hlc_string_formatting_and_parsing() {
    let hlc1 = Hlc::new(100, 1, "device-a");
    assert_eq!(hlc1.to_string(), "100.1@device-a");
    assert_eq!(hlc1.display_compact(), "100.1");

    let parsed: Hlc = "100.1@device-a".parse().unwrap();
    assert_eq!(parsed, hlc1);

    let parsed_compact: Hlc = "100.2".parse().unwrap();
    assert_eq!(parsed_compact.millis, 100);
    assert_eq!(parsed_compact.counter, 2);
    assert_eq!(parsed_compact.node_id, "");

    let json_str = serde_json::to_string(&hlc1).unwrap();
    assert_eq!(json_str, "\"100.1@device-a\"");
    let deserialized: Hlc = serde_json::from_str(&json_str).unwrap();
    assert_eq!(deserialized, hlc1);
}

#[test]
fn test_hlc_total_ordering_and_tie_breaking() {
    let a = Hlc::new(100, 1, "node-a");
    let b = Hlc::new(100, 2, "node-a");
    let c = Hlc::new(200, 0, "node-a");
    let d = Hlc::new(100, 1, "node-b"); // Tie break on node_id

    assert!(b > a);
    assert!(c > b);
    assert!(d > a); // "node-b" > "node-a"
}

#[test]
fn test_hlc_clock_skew_and_lagging_physical_clock() {
    // Appareil A creates an event at t=500
    let clock_a = HybridClock::new("device-a");
    let event_a = clock_a.tick_at(500);
    assert_eq!(event_a.millis, 500);
    assert_eq!(event_a.counter, 0);

    // Appareil B receives event A, but Appareil B's physical clock is 10 minutes BEHIND (t=100)
    let clock_b = HybridClock::new("device-b");
    let event_b = clock_b.update_at(&event_a, 100).unwrap();

    // Mathematical guarantee: HLC(B) > HLC(A) holds strictly!
    assert!(
        event_b > event_a,
        "HLC(B) must be strictly greater than HLC(A)"
    );
    assert_eq!(event_b.millis, 500);
    assert_eq!(event_b.counter, 1);
    assert_eq!(event_b.node_id, "device-b");

    // Next local event on Appareil B (still lagging at t=100)
    let next_b = clock_b.tick_at(100);
    assert!(next_b > event_b);
    assert_eq!(next_b.millis, 500);
    assert_eq!(next_b.counter, 2);
}

#[test]
fn test_hlc_physical_clock_backward_jump() {
    let clock = HybridClock::new("device-1");

    let t1 = clock.tick_at(1000);
    assert_eq!(t1.millis, 1000);
    assert_eq!(t1.counter, 0);

    // Clock jumps backward by 500ms
    let t2 = clock.tick_at(500);
    assert_eq!(t2.millis, 1000);
    assert_eq!(t2.counter, 1);
    assert!(t2 > t1);
}

#[test]
fn test_hlc_max_drift_rejection() {
    let clock = HybridClock::with_max_drift("device-1", 5_000); // 5 sec max drift
    let future_hlc = Hlc::new(20_000, 0, "rogue-device");

    let res = clock.update_at(&future_hlc, 10_000);
    assert!(matches!(res, Err(HlcError::ClockDriftExceeded { .. })));
}

#[test]
fn test_deterministic_concurrency_detection() {
    let base_hlc = Hlc::new(50, 0, "initial");

    // Appareil A modifies offline based on base_hlc
    let hlc_a = Hlc::new(100, 1, "device-a");
    let parent_a = Some(base_hlc.clone());

    // Appareil B modifies offline based on base_hlc
    let hlc_b = Hlc::new(100, 2, "device-b");
    let parent_b = Some(base_hlc.clone());

    // Concurrency detection!
    let relation = detect_causality(&hlc_a, parent_a.as_ref(), &hlc_b, parent_b.as_ref());
    assert_eq!(relation, CausalRelation::Concurrent);

    // Sequential case: C modified after observing A
    let hlc_c = Hlc::new(150, 0, "device-c");
    let parent_c = Some(hlc_a.clone());
    let seq_relation = detect_causality(&hlc_a, parent_a.as_ref(), &hlc_c, parent_c.as_ref());
    assert_eq!(seq_relation, CausalRelation::Ancestor);
}

#[test]
fn test_three_way_merge_disjoint_fields() {
    let base = json!({
        "user": {
            "name": "Alex",
            "email": "alex@example.com",
            "role": "member"
        }
    });

    // Appareil A changes name to "Alice"
    let ours = json!({
        "user": {
            "name": "Alice",
            "email": "alex@example.com",
            "role": "member"
        }
    });
    let hlc_a = Hlc::new(100, 1, "device-a");

    // Appareil B changes role to "admin" and adds avatar
    let theirs = json!({
        "user": {
            "name": "Alex",
            "email": "alex@example.com",
            "role": "admin",
            "avatar": "pic.png"
        }
    });
    let hlc_b = Hlc::new(100, 2, "device-b");

    let result = three_way_merge(Some(&base), &ours, &theirs, &hlc_a, &hlc_b);
    assert!(!result.has_conflicts);
    assert_eq!(result.field_conflicts.len(), 0);

    // Both disjoint changes merged cleanly!
    assert_eq!(result.merged_value["user"]["name"], "Alice");
    assert_eq!(result.merged_value["user"]["email"], "alex@example.com");
    assert_eq!(result.merged_value["user"]["role"], "admin");
    assert_eq!(result.merged_value["user"]["avatar"], "pic.png");
}

#[test]
fn test_three_way_merge_field_collision_resolution() {
    let base = json!({
        "user": {
            "name": "Alex"
        }
    });

    // Appareil A modifies user.name to "Alice" (HLC 100.1@node-a)
    let ours = json!({
        "user": {
            "name": "Alice"
        }
    });
    let hlc_a = Hlc::new(100, 1, "node-a");

    // Appareil B modifies user.name to "Alicia" (HLC 100.2@node-b)
    let theirs = json!({
        "user": {
            "name": "Alicia"
        }
    });
    let hlc_b = Hlc::new(100, 2, "node-b");

    let result = three_way_merge(Some(&base), &ours, &theirs, &hlc_a, &hlc_b);
    assert!(result.has_conflicts);
    assert_eq!(result.field_conflicts.len(), 1);

    let conflict = &result.field_conflicts[0];
    assert_eq!(conflict.path, "/user/name");
    assert_eq!(conflict.ours_value, json!("Alice"));
    assert_eq!(conflict.theirs_value, json!("Alicia"));
    // Since HLC(B) 100.2 > HLC(A) 100.1, Alicia wins the tie-break deterministically
    assert_eq!(conflict.resolved_value, json!("Alicia"));
    assert_eq!(conflict.winner, "node-b");
    assert_eq!(result.merged_value["user"]["name"], "Alicia");
}
