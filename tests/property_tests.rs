//! Property-based tests for NEEDLE's core invariants.
//!
//! These tests verify the design invariants documented in the plan using
//! randomized inputs via `proptest`. Each test corresponds to a property
//! listed in the plan's "Property Tests" section.

use chrono::{DateTime, TimeZone, Utc};
use needle::types::{Bead, BeadId, BeadStatus, Outcome};
use proptest::prelude::*;
use std::collections::HashSet;
use std::path::PathBuf;

// ─── Strategies ──────────────────────────────────────────────────────────────

/// Generate a random BeadId.
fn arb_bead_id() -> impl Strategy<Value = BeadId> {
    "[a-z]{1,4}-[a-z0-9]{3,6}".prop_map(BeadId::from)
}

/// Generate a random priority (1-5).
fn arb_priority() -> impl Strategy<Value = u8> {
    1u8..=5
}

/// Generate a random UTC datetime within a reasonable range.
fn arb_datetime() -> impl Strategy<Value = DateTime<Utc>> {
    // Range from 2024-01-01 to 2027-01-01 (in seconds since epoch)
    (1704067200i64..1798761600i64).prop_map(|secs| Utc.timestamp_opt(secs, 0).unwrap())
}

/// Generate a random Bead for property testing.
fn arb_bead() -> impl Strategy<Value = Bead> {
    (
        arb_bead_id(),
        arb_priority(),
        arb_datetime(),
        arb_datetime(),
    )
        .prop_map(|(id, priority, created_at, updated_at)| Bead {
            id,
            title: "test bead".to_string(),
            body: None,
            priority,
            status: BeadStatus::Open,
            assignee: None,
            labels: vec![],
            workspace: PathBuf::from("/tmp/test-workspace"),
            dependencies: vec![],
            created_at,
            updated_at,
        })
}

/// Generate a vector of random beads (1-20 beads).
fn arb_bead_vec() -> impl Strategy<Value = Vec<Bead>> {
    prop::collection::vec(arb_bead(), 1..20)
}

// ─── Property 1: Deterministic Ordering ──────────────────────────────────────
//
// "For any queue state, all workers compute the same candidate ordering."
//
// The sort key is (priority ASC, created_at ASC, id ASC). Given the same
// input, the output must always be identical regardless of initial order.

/// Sort beads using the same logic as PluckStrand::sort_candidates.
fn deterministic_sort(beads: &mut [Bead]) {
    beads.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.created_at.cmp(&b.created_at))
            .then_with(|| a.id.as_ref().cmp(b.id.as_ref()))
    });
}

proptest! {
    /// Two independent sorts of the same input produce identical output.
    #[test]
    fn deterministic_ordering_same_input(beads in arb_bead_vec()) {
        let mut sort_a = beads.clone();
        let mut sort_b = beads;

        deterministic_sort(&mut sort_a);
        deterministic_sort(&mut sort_b);

        let ids_a: Vec<&str> = sort_a.iter().map(|b| b.id.as_ref()).collect();
        let ids_b: Vec<&str> = sort_b.iter().map(|b| b.id.as_ref()).collect();

        prop_assert_eq!(ids_a, ids_b, "two sorts of the same beads must produce identical ordering");
    }

    /// Sorting a pre-shuffled copy produces the same order as the original sort.
    #[test]
    fn deterministic_ordering_shuffled_input(beads in arb_bead_vec()) {
        let mut canonical = beads.clone();
        deterministic_sort(&mut canonical);

        // Reverse the input (a simple, deterministic "shuffle")
        let mut reversed = beads;
        reversed.reverse();
        deterministic_sort(&mut reversed);

        let ids_canonical: Vec<&str> = canonical.iter().map(|b| b.id.as_ref()).collect();
        let ids_reversed: Vec<&str> = reversed.iter().map(|b| b.id.as_ref()).collect();

        prop_assert_eq!(ids_canonical, ids_reversed,
            "sorting must be deterministic regardless of initial order");
    }

    /// Sorted output respects the sort key invariant: for consecutive beads,
    /// priority is non-decreasing; within the same priority, created_at is
    /// non-decreasing; within same priority+created_at, id is non-decreasing.
    #[test]
    fn deterministic_ordering_invariant(beads in arb_bead_vec()) {
        let mut sorted = beads;
        deterministic_sort(&mut sorted);

        for window in sorted.windows(2) {
            let a = &window[0];
            let b = &window[1];

            // Priority must be non-decreasing
            prop_assert!(a.priority <= b.priority,
                "priority must be non-decreasing: {} > {}", a.priority, b.priority);

            // Within same priority, created_at must be non-decreasing
            if a.priority == b.priority {
                prop_assert!(a.created_at <= b.created_at,
                    "within same priority, created_at must be non-decreasing");

                // Within same priority and created_at, id must be non-decreasing
                if a.created_at == b.created_at {
                    prop_assert!(a.id.as_ref() <= b.id.as_ref(),
                        "within same priority and created_at, id must be non-decreasing");
                }
            }
        }
    }
}

// ─── Property 2: Exhaustive Outcomes ─────────────────────────────────────────
//
// "The outcome enum covers all possible exit codes (no `_` wildcard)."
//
// Every i32 exit code must produce a defined Outcome variant. The classify
// function must never panic, and the result must be one of the documented
// variants.

proptest! {
    /// classify() never panics for any i32 exit code.
    #[test]
    fn exhaustive_outcomes_no_panic(exit_code: i32) {
        let _ = Outcome::classify(exit_code, false);
        let _ = Outcome::classify(exit_code, true);
    }

    /// classify() returns a valid Outcome for every exit code.
    #[test]
    fn exhaustive_outcomes_valid_variant(exit_code: i32) {
        let outcome = Outcome::classify(exit_code, false);

        // Verify the outcome is a known variant by exhaustively matching.
        match outcome {
            Outcome::Success => prop_assert_eq!(exit_code, 0),
            Outcome::Failure => {
                prop_assert!(
                    exit_code == 1
                        || (2..=123).contains(&exit_code)
                        || (125..=128).contains(&exit_code),
                    "Failure should only come from exit codes 1, 2-123, 125-128; got {}",
                    exit_code
                );
            }
            Outcome::Timeout => prop_assert_eq!(exit_code, 124),
            Outcome::AgentNotFound => prop_assert_eq!(exit_code, 127),
            Outcome::Interrupted => {
                // Should not happen when was_interrupted=false
                prop_assert!(false, "Interrupted should not appear without interrupt flag");
            }
            Outcome::Crash(code) => {
                prop_assert_eq!(code, exit_code);
                prop_assert!(
                    !(0..=128).contains(&exit_code),
                    "Crash should only come from exit codes >128 or <0; got {}",
                    exit_code
                );
            }
        }
    }

    /// was_interrupted=true always produces Interrupted, regardless of exit code.
    #[test]
    fn exhaustive_outcomes_interrupted_overrides(exit_code: i32) {
        let outcome = Outcome::classify(exit_code, true);
        prop_assert_eq!(outcome, Outcome::Interrupted,
            "was_interrupted=true must always produce Interrupted for exit_code={}",
            exit_code);
    }

    /// classify is idempotent: calling it twice with the same input produces the
    /// same result.
    #[test]
    fn exhaustive_outcomes_idempotent(exit_code: i32, was_interrupted: bool) {
        let a = Outcome::classify(exit_code, was_interrupted);
        let b = Outcome::classify(exit_code, was_interrupted);
        prop_assert_eq!(a, b, "classify must be deterministic");
    }
}

// ─── Property 3: Claim Exclusivity ───────────────────────────────────────────
//
// "Given N concurrent claim attempts on 1 bead, exactly 1 succeeds."
//
// We can't easily test concurrent claiming without a real bead store, but we
// CAN verify the structural property: the claim exclusion set correctly
// prevents re-claiming, and the ClaimOutcome enum covers all paths.

proptest! {
    /// Exclusion set correctly filters out beads. After adding a bead ID to
    /// the exclusion set, the candidate list must not contain that bead.
    #[test]
    fn claim_exclusion_filters_correctly(
        beads in arb_bead_vec(),
        exclude_idx in 0usize..20,
    ) {
        if beads.is_empty() {
            return Ok(());
        }
        let exclude_idx = exclude_idx % beads.len();
        let excluded_id = beads[exclude_idx].id.clone();

        let mut exclusions = HashSet::new();
        exclusions.insert(excluded_id.clone());

        let eligible: Vec<&Bead> = beads
            .iter()
            .filter(|b| !exclusions.contains(&b.id))
            .collect();

        for bead in &eligible {
            prop_assert_ne!(bead.id.as_ref(), excluded_id.as_ref(),
                "excluded bead must not appear in eligible list");
        }
    }

    /// Exclusion set grows correctly: adding N distinct IDs results in N
    /// exclusions. No duplicates inflate the set.
    #[test]
    fn claim_exclusion_set_no_inflation(ids in prop::collection::vec(arb_bead_id(), 1..20)) {
        let mut exclusions = HashSet::new();
        for id in &ids {
            exclusions.insert(id.clone());
        }

        // Unique IDs
        let unique_ids: HashSet<_> = ids.iter().collect();
        prop_assert_eq!(exclusions.len(), unique_ids.len(),
            "exclusion set size must equal unique ID count");
    }
}

// ─── Property 4: Heartbeat Staleness ─────────────────────────────────────────
//
// "A healthy worker's heartbeat is always within TTL."
//
// We test the staleness check function: a heartbeat is stale iff its age
// exceeds the TTL. The check must be monotonic — if a heartbeat is stale at
// time T, it is stale at all times > T.

proptest! {
    /// A heartbeat emitted "now" is never stale for any positive TTL.
    #[test]
    fn heartbeat_fresh_is_never_stale(ttl_secs in 1u64..3600) {
        let heartbeat = needle::health::HeartbeatData {
            worker_id: "test-worker".to_string(),
            pid: 12345,
            state: needle::types::WorkerState::Selecting,
            current_bead: None,
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: Utc::now(),
            started_at: Utc::now(),
            beads_processed: 0,
            session: "test".to_string(),
        };

        let ttl = std::time::Duration::from_secs(ttl_secs);
        let is_stale = needle::health::HealthMonitor::is_stale(&heartbeat, ttl);

        prop_assert!(!is_stale,
            "a heartbeat emitted now must not be stale with TTL={}s", ttl_secs);
    }

    /// A heartbeat far in the past is always stale for any reasonable TTL.
    #[test]
    fn heartbeat_old_is_always_stale(ttl_secs in 1u64..3600) {
        let old_time = Utc::now() - chrono::Duration::hours(24);
        let heartbeat = needle::health::HeartbeatData {
            worker_id: "test-worker".to_string(),
            pid: 12345,
            state: needle::types::WorkerState::Selecting,
            current_bead: None,
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: old_time,
            started_at: old_time,
            beads_processed: 0,
            session: "test".to_string(),
        };

        let ttl = std::time::Duration::from_secs(ttl_secs);
        let is_stale = needle::health::HealthMonitor::is_stale(&heartbeat, ttl);

        prop_assert!(is_stale,
            "a 24h-old heartbeat must be stale with TTL={}s", ttl_secs);
    }
}
