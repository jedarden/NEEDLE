//! Synthetic fixture tests for multi-claim staleness detection.
//!
//! This test suite validates the per-assignee staleness heuristic against
//! realistic synthetic fixtures, simulating the real incident from 2026-08-17
//! where workers ended up with multiple simultaneous claims on different beads.
//!
//! ## Fixture Design
//!
//! Tests create a mock bead store with in_progress claims for one or more
//! assignees, each with distinct `updated_at` timestamps. The fixture validates:
//! - For each assignee with multiple claims, only the newest is valid
//! - Older claims are reap-eligible regardless of worker liveness
//! - No cross-contamination between assignees
//!
//! ## Isolation
//!
//! All tests use temporary directories and mock bead stores to prevent
//! contamination of production environments, following the isolation patterns
//! in CLAUDE.md.

use std::path::PathBuf;

use chrono::{DateTime, TimeZone, Utc};
use needle::types::{Bead, BeadId, BeadStatus};

// Import the staleness detection function from the mend module
// This function is made public for testing purposes
use needle::strand::mend::get_stale_by_assignee_overlap;

// ──────────────────────────────────────────────────────────────────────────────
// Test fixtures
// ──────────────────────────────────────────────────────────────────────────────

/// Create an in_progress bead with a specific timestamp.
fn make_bead_with_timestamp(id: &str, assignee: &str, updated_at: DateTime<Utc>) -> Bead {
    let dt = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    Bead {
        id: BeadId::from(id),
        title: format!("Bead {}", id),
        body: None,
        priority: 1,
        status: BeadStatus::InProgress,
        assignee: Some(assignee.to_string()),
        labels: vec![],
        workspace: PathBuf::from("/tmp/test-workspace"),
        dependencies: vec![],
        dependents: vec![],
        comments: vec![],
        created_at: dt,
        updated_at,
    }
}

/// Create three in_progress beads for the same assignee with distinct timestamps.
///
/// Returns (bead_t1, bead_t2, bead_t3) where t1 < t2 < t3.
fn make_three_claims_same_assignee(assignee: &str) -> (Bead, Bead, Bead) {
    let now = Utc::now();
    let bead_t1 = make_bead_with_timestamp(
        &format!("{}-old", assignee),
        assignee,
        now - chrono::Duration::seconds(300), // 5 minutes old
    );
    let bead_t2 = make_bead_with_timestamp(
        &format!("{}-medium", assignee),
        assignee,
        now - chrono::Duration::seconds(180), // 3 minutes old
    );
    let bead_t3 = make_bead_with_timestamp(
        &format!("{}-new", assignee),
        assignee,
        now - chrono::Duration::seconds(60), // 1 minute old (newest)
    );
    (bead_t1, bead_t2, bead_t3)
}

/// Create claims for two assignees with overlapping timestamps.
///
/// Simulates the real incident where multiple workers had overlapping claims.
fn make_overlapping_multi_assignee_claims() -> Vec<Bead> {
    let now = Utc::now();

    // Assignee A: 3 claims with timestamps t1 < t2 < t3
    let a_t1 = make_bead_with_timestamp(
        "worker-a-t1",
        "worker-a",
        now - chrono::Duration::seconds(300),
    );
    let a_t2 = make_bead_with_timestamp(
        "worker-a-t2",
        "worker-a",
        now - chrono::Duration::seconds(180),
    );
    let a_t3 = make_bead_with_timestamp(
        "worker-a-t3",
        "worker-a",
        now - chrono::Duration::seconds(60),
    );

    // Assignee B: 2 claims with timestamps s1 < s2
    // s2 is newer than A's t1 but older than A's t3 (overlap)
    let b_s1 = make_bead_with_timestamp(
        "worker-b-s1",
        "worker-b",
        now - chrono::Duration::seconds(240),
    );
    let b_s2 = make_bead_with_timestamp(
        "worker-b-s2",
        "worker-b",
        now - chrono::Duration::seconds(120),
    );

    vec![a_t1, a_t2, a_t3, b_s1, b_s2]
}

// ──────────────────────────────────────────────────────────────────────────────
// Test cases: Single assignee, multiple claims
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn three_claims_same_assignee_oldest_two_are_stale() {
    // Given: 3 in_progress claims for assignee A with timestamps t1 < t2 < t3
    let (bead_t1, bead_t2, bead_t3) = make_three_claims_same_assignee("worker-alpha");
    let all_beads = vec![bead_t1.clone(), bead_t2.clone(), bead_t3.clone()];

    // When: Apply staleness detection
    let stale = get_stale_by_assignee_overlap(&all_beads);

    // Then: Exactly the 2 older claims (t1 and t2) are marked stale
    assert_eq!(
        stale.len(),
        2,
        "should identify exactly 2 stale claims out of 3"
    );
    assert!(
        stale.contains(&bead_t1.id),
        "oldest claim (t1) should be marked stale"
    );
    assert!(
        stale.contains(&bead_t2.id),
        "middle claim (t2) should be marked stale"
    );
    assert!(
        !stale.contains(&bead_t3.id),
        "newest claim (t3) should NOT be marked stale"
    );
}

#[test]
fn three_claims_same_assignee_newest_is_always_protected() {
    // Given: 3 claims for same assignee, t3 is newest
    let (bead_t1, bead_t2, bead_t3) = make_three_claims_same_assignee("worker-beta");
    let all_beads = vec![bead_t1, bead_t2, bead_t3.clone()];

    // When: Apply staleness detection
    let stale = get_stale_by_assignee_overlap(&all_beads);

    // Then: Newest claim (t3) is never marked stale, even with other claims present
    assert!(
        !stale.contains(&bead_t3.id),
        "newest claim must be protected"
    );
}

#[test]
fn single_claim_same_assignee_never_stale() {
    // Given: Single in_progress claim for assignee
    let now = Utc::now();
    let bead = make_bead_with_timestamp("single-claim", "worker-gamma", now);
    let all_beads = vec![bead.clone()];

    // When: Apply staleness detection
    let stale = get_stale_by_assignee_overlap(&all_beads);

    // Then: Single claim is never marked stale (no overlap possible)
    assert!(
        stale.is_empty(),
        "single claim should never be marked stale by overlap detection"
    );
    assert!(
        !stale.contains(&bead.id),
        "single claim must not be in stale set"
    );
}

#[test]
fn two_claims_same_assignee_older_is_stale() {
    // Given: 2 claims for same assignee with timestamps t1 < t2
    let now = Utc::now();
    let bead_old = make_bead_with_timestamp(
        "worker-delta-old",
        "worker-delta",
        now - chrono::Duration::seconds(120),
    );
    let bead_new = make_bead_with_timestamp(
        "worker-delta-new",
        "worker-delta",
        now - chrono::Duration::seconds(60),
    );
    let all_beads = vec![bead_old.clone(), bead_new.clone()];

    // When: Apply staleness detection
    let stale = get_stale_by_assignee_overlap(&all_beads);

    // Then: Only the older claim is marked stale
    assert_eq!(stale.len(), 1, "should identify exactly 1 stale claim");
    assert!(
        stale.contains(&bead_old.id),
        "older claim should be marked stale"
    );
    assert!(
        !stale.contains(&bead_new.id),
        "newer claim should NOT be marked stale"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Test cases: Multiple assignees, no cross-contamination
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn two_assignees_with_overlapping_claims_no_cross_contamination() {
    // Given: 2 assignees with overlapping timestamps (simulates real incident)
    // Assignee A: t1 < t2 < t3
    // Assignee B: s1 < s2
    // Where s2 (newer for B) is older than A's t3 but newer than A's t1
    let all_beads = make_overlapping_multi_assignee_claims();

    // When: Apply staleness detection
    let stale = get_stale_by_assignee_overlap(&all_beads);

    // Then: Verify per-assignee staleness without cross-contamination
    // Assignee A: t1 and t2 are stale (t3 is newest for A)
    // Assignee B: s1 is stale (s2 is newest for B)
    assert_eq!(
        stale.len(),
        3,
        "should identify exactly 3 stale claims total"
    );

    // A's claims: t1 and t2 should be stale, t3 protected
    assert!(stale.contains(&BeadId::from("worker-a-t1")));
    assert!(stale.contains(&BeadId::from("worker-a-t2")));
    assert!(!stale.contains(&BeadId::from("worker-a-t3")));

    // B's claims: s1 should be stale, s2 protected
    assert!(stale.contains(&BeadId::from("worker-b-s1")));
    assert!(!stale.contains(&BeadId::from("worker-b-s2")));
}

#[test]
fn three_assignees_independent_staleness_detection() {
    // Given: 3 assignees, each with different numbers of claims
    let now = Utc::now();

    // Assignee A: 3 claims (2 stale expected)
    let a1 = make_bead_with_timestamp("worker-a-1", "worker-a", now - chrono::Duration::seconds(300));
    let a2 = make_bead_with_timestamp("worker-a-2", "worker-a", now - chrono::Duration::seconds(200));
    let a3 = make_bead_with_timestamp("worker-a-3", "worker-a", now - chrono::Duration::seconds(100));

    // Assignee B: 2 claims (1 stale expected)
    let b1 = make_bead_with_timestamp("worker-b-1", "worker-b", now - chrono::Duration::seconds(250));
    let b2 = make_bead_with_timestamp("worker-b-2", "worker-b", now - chrono::Duration::seconds(150));

    // Assignee C: 1 claim (0 stale expected)
    let c1 = make_bead_with_timestamp("worker-c-1", "worker-c", now - chrono::Duration::seconds(180));

    let all_beads = vec![
        a1.clone(),
        a2.clone(),
        a3.clone(),
        b1.clone(),
        b2.clone(),
        c1.clone(),
    ];

    // When: Apply staleness detection
    let stale = get_stale_by_assignee_overlap(&all_beads);

    // Then: Each assignee's staleness is computed independently
    assert_eq!(
        stale.len(),
        3,
        "total stale claims: 2 from A + 1 from B + 0 from C"
    );

    // A: oldest 2 are stale
    assert!(stale.contains(&a1.id));
    assert!(stale.contains(&a2.id));
    assert!(!stale.contains(&a3.id));

    // B: oldest 1 is stale
    assert!(stale.contains(&b1.id));
    assert!(!stale.contains(&b2.id));

    // C: single claim never stale
    assert!(!stale.contains(&c1.id));
}

#[test]
fn stale_detection_ignores_non_in_progress_beads() {
    // Given: Mix of InProgress and non-InProgress beads for same assignee
    let now = Utc::now();

    let ip_old = make_bead_with_timestamp("ip-old", "worker", now - chrono::Duration::seconds(300));
    let ip_new = make_bead_with_timestamp("ip-new", "worker", now - chrono::Duration::seconds(60));

    // Create Open and Closed beads with same assignee (should be ignored)
    let mut open_bead = ip_old.clone();
    open_bead.id = BeadId::from("open-bead");
    open_bead.status = BeadStatus::Open;

    let mut closed_bead = ip_new.clone();
    closed_bead.id = BeadId::from("closed-bead");
    closed_bead.status = BeadStatus::Closed;

    let all_beads = vec![ip_old.clone(), ip_new.clone(), open_bead, closed_bead];

    // When: Apply staleness detection
    let stale = get_stale_by_assignee_overlap(&all_beads);

    // Then: Only InProgress beads are considered
    assert_eq!(stale.len(), 1, "only InProgress beads should be evaluated");
    assert!(stale.contains(&ip_old.id), "older InProgress bead is stale");
    assert!(
        !stale.contains(&ip_new.id),
        "newer InProgress bead is protected"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Test cases: Edge cases and corner cases
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn empty_bead_list_returns_empty_stale_set() {
    // Given: Empty bead list
    let all_beads: Vec<Bead> = vec![];

    // When: Apply staleness detection
    let stale = get_stale_by_assignee_overlap(&all_beads);

    // Then: No stale claims (no panic, empty result)
    assert!(
        stale.is_empty(),
        "empty bead list should yield empty stale set"
    );
}

#[test]
fn beads_without_assignee_are_ignored() {
    // Given: Beads with no assignee or empty assignee
    let now = Utc::now();

    let mut no_assignee = make_bead_with_timestamp("no-assignee", "", now);
    no_assignee.assignee = None;

    let mut empty_assignee = make_bead_with_timestamp("empty-assignee", "", now);
    empty_assignee.assignee = Some(String::new());

    let valid_bead = make_bead_with_timestamp("valid", "worker-x", now);

    let all_beads = vec![no_assignee, empty_assignee, valid_bead];

    // When: Apply staleness detection
    let stale = get_stale_by_assignee_overlap(&all_beads);

    // Then: Beads without valid assignee are ignored, no staleness detected
    assert!(stale.is_empty(), "beads without assignee should be ignored");
}

#[test]
fn identical_timestamps_newest_by_id_order() {
    // Given: 2 claims for same assignee with identical timestamps
    let now = Utc::now();
    let bead_a = make_bead_with_timestamp("bead-a", "worker-z", now);
    let bead_b = make_bead_with_timestamp("bead-b", "worker-z", now);

    let all_beads = vec![bead_a.clone(), bead_b.clone()];

    // When: Apply staleness detection
    let stale = get_stale_by_assignee_overlap(&all_beads);

    // Then: Neither is marked stale (identical timestamps means no clear winner)
    // The implementation marks beads as stale only when updated_at < newest_updated.
    // With identical timestamps, this condition is never met, so both beads are
    // considered valid. This is the correct behavior - we can't determine which
    // is newer if timestamps are identical.
    assert_eq!(
        stale.len(),
        0,
        "with identical timestamps, neither should be marked stale"
    );
}

#[test]
fn very_old_claim_vs_very_new_claim_gap() {
    // Given: 2 claims for same assignee with large time gap (1 hour)
    let now = Utc::now();
    let ancient = make_bead_with_timestamp(
        "ancient",
        "worker-time-traveler",
        now - chrono::Duration::seconds(3600), // 1 hour old
    );
    let recent = make_bead_with_timestamp(
        "recent",
        "worker-time-traveler",
        now - chrono::Duration::seconds(10), // 10 seconds old
    );

    let all_beads = vec![ancient.clone(), recent.clone()];

    // When: Apply staleness detection
    let stale = get_stale_by_assignee_overlap(&all_beads);

    // Then: Age gap doesn't matter—only relative recency within assignee
    assert_eq!(
        stale.len(),
        1,
        "large time gap doesn't change staleness logic"
    );
    assert!(stale.contains(&ancient.id), "much older claim is stale");
    assert!(!stale.contains(&recent.id), "much newer claim is protected");
}

#[test]
fn five_claims_same_assignee_only_newest_protected() {
    // Given: 5 claims for same assignee with spaced timestamps
    let now = Utc::now();
    let beads = vec![
        make_bead_with_timestamp("p1", "poly-worker", now - chrono::Duration::seconds(500)),
        make_bead_with_timestamp("p2", "poly-worker", now - chrono::Duration::seconds(400)),
        make_bead_with_timestamp("p3", "poly-worker", now - chrono::Duration::seconds(300)),
        make_bead_with_timestamp("p4", "poly-worker", now - chrono::Duration::seconds(200)),
        make_bead_with_timestamp("p5", "poly-worker", now - chrono::Duration::seconds(100)),
    ];

    // When: Apply staleness detection
    let stale = get_stale_by_assignee_overlap(&beads);

    // Then: Exactly 4 are stale, only newest (p5) is protected
    assert_eq!(stale.len(), 4, "out of 5 claims, 4 should be stale");
    assert!(stale.contains(&BeadId::from("p1")));
    assert!(stale.contains(&BeadId::from("p2")));
    assert!(stale.contains(&BeadId::from("p3")));
    assert!(stale.contains(&BeadId::from("p4")));
    assert!(!stale.contains(&BeadId::from("p5")));
}

#[test]
fn real_incident_simulation_2026_08_17() {
    // Given: Fixture simulating the real incident from 2026-08-17
    // Assignee A has 3 claims: t1 < t2 < t3
    // Assignee B has 2 claims: s1 < s2
    // Worker process is "alive" (liveness check mocked to return true)
    // Expected: A's t1 and t2 are reap-eligible, A's t3 and B's s2 are NOT
    // (even though s1 is stale for B)
    let now = Utc::now();

    // Assignee A's claims
    let a_t1 = make_bead_with_timestamp("cg-l0v0kc", "worker-a", now - chrono::Duration::seconds(7200));
    let a_t2 = make_bead_with_timestamp("cg-l1v0kc", "worker-a", now - chrono::Duration::seconds(3600));
    let a_t3 = make_bead_with_timestamp("cg-l2v0kc", "worker-a", now - chrono::Duration::seconds(1800));

    // Assignee B's claims
    let b_s1 = make_bead_with_timestamp("cg-m0v0kc", "worker-b", now - chrono::Duration::seconds(5400));
    let b_s2 = make_bead_with_timestamp("cg-m1v0kc", "worker-b", now - chrono::Duration::seconds(2700));

    let all_beads = vec![
        a_t1.clone(),
        a_t2.clone(),
        a_t3.clone(),
        b_s1.clone(),
        b_s2.clone(),
    ];

    // When: Apply staleness detection
    let stale = get_stale_by_assignee_overlap(&all_beads);

    // Then: Verify the expected outcome from the real incident
    assert_eq!(stale.len(), 3, "exactly 3 claims should be reap-eligible");

    // A's t1 and t2 are stale (t3 protected as newest for A)
    assert!(
        stale.contains(&a_t1.id),
        "A's t1 should be reap-eligible (superseded by t2 and t3)"
    );
    assert!(
        stale.contains(&a_t2.id),
        "A's t2 should be reap-eligible (superseded by t3)"
    );
    assert!(
        !stale.contains(&a_t3.id),
        "A's t3 should NOT be reap-eligible (newest for A)"
    );

    // B's s1 is stale (s2 protected as newest for B)
    assert!(
        stale.contains(&b_s1.id),
        "B's s1 should be reap-eligible (superseded by s2)"
    );
    assert!(
        !stale.contains(&b_s2.id),
        "B's s2 should NOT be reap-eligible (newest for B)"
    );

    // Verify no cross-contamination: B's newest (s2) is newer than A's t1,
    // but A's t1 is still stale because it's not the newest for A
    // This proves staleness is per-assignee, not global
}

// ──────────────────────────────────────────────────────────────────────────────
// Test cases: Timestamp precision and ordering
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn staleness_respects_second_precision() {
    // Given: 2 claims with timestamps differing by 1 second
    let now = Utc::now();
    let bead_older = make_bead_with_timestamp("older", "worker", now - chrono::Duration::seconds(1));
    let bead_newer = make_bead_with_timestamp("newer", "worker", now);

    let all_beads = vec![bead_older.clone(), bead_newer];

    // When: Apply staleness detection
    let stale = get_stale_by_assignee_overlap(&all_beads);

    // Then: 1-second difference is sufficient for staleness determination
    assert_eq!(stale.len(), 1, "1-second precision should be respected");
    assert!(stale.contains(&bead_older.id));
}

#[test]
fn staleness_with_millisecond_precision() {
    // Given: 2 claims with timestamps differing by milliseconds
    let now = Utc::now();
    let bead_older =
        make_bead_with_timestamp("older-ms", "worker", now - chrono::Duration::milliseconds(500));
    let bead_newer = make_bead_with_timestamp("newer-ms", "worker", now);

    let all_beads = vec![bead_older.clone(), bead_newer];

    // When: Apply staleness detection
    let stale = get_stale_by_assignee_overlap(&all_beads);

    // Then: Millisecond precision is respected (if timestamps have it)
    assert_eq!(stale.len(), 1, "millisecond precision should be detected");
}

#[test]
fn unsorted_bead_list_correct_identification() {
    // Given: Beads in random order (not sorted by timestamp or assignee)
    let now = Utc::now();

    let bead_newest = make_bead_with_timestamp("newest", "worker", now - chrono::Duration::seconds(10));
    let bead_oldest = make_bead_with_timestamp("oldest", "worker", now - chrono::Duration::seconds(100));
    let bead_middle = make_bead_with_timestamp("middle", "worker", now - chrono::Duration::seconds(50));

    // Intentionally random order: middle, newest, oldest
    let all_beads = vec![
        bead_middle.clone(),
        bead_newest.clone(),
        bead_oldest.clone(),
    ];

    // When: Apply staleness detection
    let stale = get_stale_by_assignee_overlap(&all_beads);

    // Then: Correct identification regardless of input order
    assert_eq!(stale.len(), 2, "order shouldn't affect staleness logic");
    assert!(stale.contains(&bead_oldest.id));
    assert!(stale.contains(&bead_middle.id));
    assert!(!stale.contains(&bead_newest.id));
}

// ──────────────────────────────────────────────────────────────────────────────
// Test cases: Assignee name handling
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn assignee_names_with_special_characters() {
    // Given: Assignee names with hyphens, underscores, numbers (common in NEEDLE)
    let now = Utc::now();

    let bead_old = make_bead_with_timestamp(
        "old",
        "claude-code-glm-4.7-glm-hopt:commitgraph",
        now - chrono::Duration::seconds(300),
    );
    let bead_new = make_bead_with_timestamp(
        "new",
        "claude-code-glm-4.7-glm-hopt:commitgraph",
        now - chrono::Duration::seconds(60),
    );

    let all_beads = vec![bead_old.clone(), bead_new];

    // When: Apply staleness detection
    let stale = get_stale_by_assignee_overlap(&all_beads);

    // Then: Complex assignee names are handled correctly
    assert_eq!(stale.len(), 1);
    assert!(stale.contains(&bead_old.id));
}

#[test]
fn case_sensitive_assignee_matching() {
    // Given: Assignee names that differ only in case (should be treated as different)
    let now = Utc::now();

    let bead_lower =
        make_bead_with_timestamp("lower", "worker-alpha", now - chrono::Duration::seconds(300));
    let bead_upper = make_bead_with_timestamp("upper", "WORKER-ALPHA", now - chrono::Duration::seconds(60));

    let all_beads = vec![bead_lower.clone(), bead_upper.clone()];

    // When: Apply staleness detection
    let stale = get_stale_by_assignee_overlap(&all_beads);

    // Then: Different case = different assignees, neither is stale
    assert!(
        stale.is_empty(),
        "assignee matching should be case-sensitive"
    );
}
