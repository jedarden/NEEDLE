//! Focused behavioral contracts for impact receipts (N-T55,
//! `needle-c4e6424a`; ADR-029 step 5).
//!
//! An admitted change is kept only if the acceptance measure it chose for
//! itself actually moved, over its own cohort, against a baseline taken
//! before exposure.

use chrono::{DateTime, TimeZone, Utc};
use needle::evidence_routing::LedgerRow;
use needle::learning::improvement::{
    append_receipt, decide_receipt, measure_cohort, read_receipts, revert_proposal,
    AcceptanceMeasure, Cohort, CohortMeasures, Contamination, Direction, EvidenceClass,
    EvidenceKind, EvidenceRef, ImpactMeasure, ImprovementProposal, ProposalScope, ReceiptDecision,
    ReceiptThresholds, Rollback, IMPACT_RECEIPT_SCHEMA_VERSION, RECEIPT_REF_NAMESPACE,
};
use serde_json::json;

fn at(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, day, 12, 0, 0).unwrap()
}

/// A date past the end of the fixture month, for a third decision window.
fn later() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 5, 12, 0, 0).unwrap()
}

fn proposal() -> ImprovementProposal {
    ImprovementProposal::new(
        EvidenceClass::RedBaselineWorkspace,
        vec![EvidenceRef::new(EvidenceKind::Workspace, "NEEDLE", at(12))],
        ProposalScope::new(["NEEDLE".to_string()], []),
        "raise the default gate in `.needle.yaml` to build+test",
        EvidenceClass::RedBaselineWorkspace.max_authority(),
        "verified-closure yield per attempt rises",
        AcceptanceMeasure {
            measure: ImpactMeasure::VerifiedYieldPerAttempt,
            direction: Direction::Increase,
            min_delta: 0.05,
            horizon_days: 7,
        },
        Rollback {
            description: "restore the previous default gate".to_string(),
            automatic: true,
        },
        at(14),
        1,
    )
    .expect("valid")
}

fn cohort() -> Cohort {
    Cohort::from_scope(&proposal().scope, vec!["needle-aaaa1111".to_string()])
}

/// A window of `total` costed attempts of which `verified` verified.
fn window(total: u64, verified: u64) -> CohortMeasures {
    CohortMeasures {
        attempts: total,
        verified,
        cost_usd: total as f64 * 2.0,
        yield_per_attempt: verified as f64 / total as f64,
        yield_per_dollar: verified as f64 / (total as f64 * 2.0),
        ..CohortMeasures::default()
    }
}

fn thresholds() -> ReceiptThresholds {
    ReceiptThresholds::default()
}

// ──────────────────────────────────────────────────────────────────────────
// The three outcomes the acceptance criteria name
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn an_improved_cohort_promotes() {
    let receipt = decide_receipt(
        &proposal(),
        cohort(),
        window(20, 4),  // 20%
        window(20, 12), // 60%
        Vec::new(),
        &thresholds(),
        at(21),
    );

    assert_eq!(receipt.decision, ReceiptDecision::Promote);
    assert!(
        (receipt.delta - 0.4).abs() < 1e-9,
        "the delta is signed in the measure's own direction: {}",
        receipt.delta
    );
    assert_eq!(receipt.schema_version, IMPACT_RECEIPT_SCHEMA_VERSION);
    assert_eq!(
        receipt.unique_ref(),
        format!("{RECEIPT_REF_NAMESPACE}:{}", receipt.signature)
    );
}

#[test]
fn an_unchanged_cohort_withdraws_and_emits_exactly_one_revert_proposal() {
    let original = proposal();
    let receipt = decide_receipt(
        &original,
        cohort(),
        window(20, 8),
        window(20, 8), // identical: the change did nothing
        Vec::new(),
        &thresholds(),
        at(21),
    );

    match &receipt.decision {
        ReceiptDecision::Withdraw { detail } => assert!(
            detail.contains("whatever its author reports"),
            "the withdrawal says why: {detail}"
        ),
        other => panic!("expected a withdrawal, got {other:?}"),
    }

    let revert = revert_proposal(&receipt, &original, at(21)).expect("one revert proposal");
    assert_eq!(
        revert.authority, original.authority,
        "a withdrawal is delivered at the original authority level, not a privileged one"
    );
    assert!(
        revert.intended_change.contains(&original.signature),
        "the revert names what it is reverting: {}",
        revert.intended_change
    );
    assert!(
        revert.rollback.description.contains(&original.signature),
        "and how to put it back"
    );
}

#[test]
fn a_promoted_receipt_emits_no_revert_proposal() {
    let original = proposal();
    let receipt = decide_receipt(
        &original,
        cohort(),
        window(20, 4),
        window(20, 12),
        Vec::new(),
        &thresholds(),
        at(21),
    );
    assert!(revert_proposal(&receipt, &original, at(21)).is_none());
}

#[test]
fn a_contaminated_cohort_holds_rather_than_deciding() {
    let original = proposal();
    // The measures would otherwise promote — contamination must override that,
    // not merely break a tie.
    let receipt = decide_receipt(
        &original,
        cohort(),
        window(20, 4),
        window(20, 18),
        vec![Contamination::OperatorCommit {
            commit: "abc1234".to_string(),
        }],
        &thresholds(),
        at(21),
    );

    match &receipt.decision {
        ReceiptDecision::Hold { detail } => assert!(
            detail.contains("not attributable"),
            "the hold says why: {detail}"
        ),
        other => panic!("expected a hold, got {other:?}"),
    }
    assert!(revert_proposal(&receipt, &original, at(21)).is_none());
}

#[test]
fn a_concurrent_proposal_on_the_same_cohort_also_holds() {
    let receipt = decide_receipt(
        &proposal(),
        cohort(),
        window(20, 12),
        window(20, 2), // would otherwise withdraw
        vec![Contamination::ConcurrentProposal {
            signature: "abcdef0123456789".to_string(),
        }],
        &thresholds(),
        at(21),
    );
    assert!(
        matches!(receipt.decision, ReceiptDecision::Hold { .. }),
        "an unattributable delta is not withdrawn either"
    );
}

#[test]
fn a_cohort_too_small_to_decide_holds() {
    let receipt = decide_receipt(
        &proposal(),
        cohort(),
        window(3, 0),
        window(3, 3), // a perfect rate over three attempts is not evidence
        Vec::new(),
        &thresholds(),
        at(21),
    );
    match &receipt.decision {
        ReceiptDecision::Hold { detail } => {
            assert!(detail.contains("below the 10"), "detail: {detail}")
        }
        other => panic!("expected a hold, got {other:?}"),
    }
}

#[test]
fn a_decrease_measure_is_judged_in_its_own_direction() {
    let mut recurrence = proposal();
    recurrence.acceptance = AcceptanceMeasure {
        measure: ImpactMeasure::FingerprintRecurrence,
        direction: Direction::Decrease,
        min_delta: 0.2,
        horizon_days: 14,
    };

    let before = CohortMeasures {
        attempts: 20,
        fingerprint_recurrence: 0.6,
        ..CohortMeasures::default()
    };
    let after = CohortMeasures {
        attempts: 20,
        fingerprint_recurrence: 0.1,
        ..CohortMeasures::default()
    };

    let receipt = decide_receipt(
        &recurrence,
        cohort(),
        before,
        after,
        Vec::new(),
        &thresholds(),
        at(21),
    );
    assert_eq!(receipt.decision, ReceiptDecision::Promote);
    assert!(
        receipt.delta > 0.0,
        "a positive delta always means 'moved the way the proposal wanted': {}",
        receipt.delta
    );
}

// ──────────────────────────────────────────────────────────────────────────
// Measures exclude uncosted, decomposed and fixture rows
// ──────────────────────────────────────────────────────────────────────────

fn row(workspace: &str, outcome: &str, worker: &str, costed: bool, cost: f64) -> LedgerRow {
    LedgerRow {
        timestamp: Some(at(13)),
        data: json!({
            "attempt_id": format!("a-{outcome}-{worker}-{cost}"),
            "bead_id": "needle-aaaa1111",
            "workspace": format!("/home/coding/{workspace}"),
            "worker": worker,
            "adapter": "claude-code-glm-5.3",
            "outcome": outcome,
            "costed": costed,
            "estimated_cost_usd": cost,
            "terminal_reason": "gate:cargo-test",
        }),
    }
}

#[test]
fn measures_are_computed_only_from_costed_non_decomposed_non_fixture_rows() {
    let rows = vec![
        // Counted: four costed, live rows, two verified.
        row("NEEDLE", "verified_success", "glm-roam-18", true, 2.0),
        row("NEEDLE", "verified_success", "glm-roam-19", true, 2.0),
        row("NEEDLE", "work_failure", "glm-roam-20", true, 2.0),
        row("NEEDLE", "work_failure", "glm-roam-21", true, 2.0),
        // Excluded: uncosted (cost unknown, never zero).
        row("NEEDLE", "work_failure", "glm-roam-22", false, 0.0),
        // Excluded: decomposed (ADR-030).
        row("NEEDLE", "decomposed", "glm-roam-23", true, 2.0),
        // Excluded: fixture worker.
        row("NEEDLE", "work_failure", "echo-test-test-worker", true, 9.0),
        // Excluded: a different workspace.
        row("commitgraph", "work_failure", "glm-roam-24", true, 9.0),
    ];

    let measures = measure_cohort(&rows, &cohort(), Some("gate:cargo-test"), &[]);

    assert_eq!(
        measures.attempts, 4,
        "uncosted, decomposed, fixture and out-of-cohort rows are all excluded"
    );
    assert_eq!(measures.verified, 2);
    assert!((measures.yield_per_attempt - 0.5).abs() < 1e-9);
    assert!(
        (measures.cost_usd - 8.0).abs() < 1e-9,
        "only the four counted rows contribute cost, got {}",
        measures.cost_usd
    );
    assert!((measures.yield_per_dollar - 0.25).abs() < 1e-9);
}

#[test]
fn fingerprint_recurrence_counts_only_the_target_reason() {
    let mut rows = vec![row("NEEDLE", "work_failure", "w1", true, 1.0)];
    let mut other = row("NEEDLE", "work_failure", "w2", true, 1.0);
    other.data["terminal_reason"] = json!("exit_code:2");
    rows.push(other);

    let measures = measure_cohort(&rows, &cohort(), Some("gate:cargo-test"), &[]);
    assert!((measures.fingerprint_recurrence - 0.5).abs() < 1e-9);

    let untargeted = measure_cohort(&rows, &cohort(), None, &[]);
    assert!(
        untargeted.fingerprint_recurrence.abs() < 1e-9,
        "with no target fingerprint there is nothing to recur"
    );
}

#[test]
fn false_close_rate_is_reopened_closures_over_closures() {
    let rows = vec![
        row("NEEDLE", "verified_success", "w1", true, 1.0),
        row("NEEDLE", "verified_success", "w2", true, 1.0),
    ];
    // Both rows carry bead needle-aaaa1111, which was reopened.
    let measures = measure_cohort(&rows, &cohort(), None, &["needle-aaaa1111".to_string()]);
    assert!((measures.false_close_rate - 1.0).abs() < 1e-9);

    let clean = measure_cohort(&rows, &cohort(), None, &[]);
    assert!(clean.false_close_rate.abs() < 1e-9);
}

#[test]
fn an_empty_cohort_measures_zero_rather_than_dividing_by_zero() {
    let measures = measure_cohort(&[], &cohort(), None, &[]);
    assert_eq!(measures, CohortMeasures::default());
    assert_eq!(measures.attempts, 0);
}

#[test]
fn a_cohort_naming_nothing_matches_nothing_rather_than_the_whole_fleet() {
    let rows = vec![row("NEEDLE", "verified_success", "w1", true, 1.0)];
    let empty = Cohort::default();
    let measures = measure_cohort(&rows, &empty, None, &[]);
    assert_eq!(
        measures.attempts, 0,
        "an unscoped cohort must not silently measure everything"
    );
}

// ──────────────────────────────────────────────────────────────────────────
// Append-only persistence
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn receipts_survive_a_restart_unchanged_and_are_never_rewritten() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("improvements/receipts.jsonl");

    let first = decide_receipt(
        &proposal(),
        cohort(),
        window(20, 4),
        window(20, 12),
        Vec::new(),
        &thresholds(),
        at(21),
    );
    let second = decide_receipt(
        &proposal(),
        cohort(),
        window(20, 12),
        window(20, 12),
        Vec::new(),
        &thresholds(),
        at(28),
    );

    append_receipt(&path, &first).expect("append");
    append_receipt(&path, &second).expect("append");

    // "Restart": read back from disk only.
    let read = read_receipts(&path).expect("read");
    assert_eq!(read.len(), 2, "both receipts are there");
    assert_eq!(
        read[0], first,
        "the first is byte-identical after a restart"
    );
    assert_eq!(read[1], second);
    assert_eq!(
        read[0].decision,
        ReceiptDecision::Promote,
        "the earlier decision is not restated by the later one"
    );

    // A third append about the same proposal adds a record; it does not edit
    // either existing one.
    let third = decide_receipt(
        &proposal(),
        cohort(),
        window(20, 12),
        window(20, 2),
        Vec::new(),
        &thresholds(),
        later(),
    );
    append_receipt(&path, &third).expect("append");
    let read = read_receipts(&path).expect("read");
    assert_eq!(read.len(), 3);
    assert_eq!(
        read[0], first,
        "history is not rewritten by a later decision"
    );
}

#[test]
fn reading_a_missing_receipts_file_is_empty_rather_than_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let receipts = read_receipts(&dir.path().join("nothing/here.jsonl")).expect("ok");
    assert!(receipts.is_empty());
}

#[test]
fn one_malformed_line_does_not_make_the_other_receipts_unreadable() {
    use std::io::Write;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("receipts.jsonl");

    let good = decide_receipt(
        &proposal(),
        cohort(),
        window(20, 4),
        window(20, 12),
        Vec::new(),
        &thresholds(),
        at(21),
    );
    append_receipt(&path, &good).expect("append");
    {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open");
        writeln!(file, "{{ this is not a receipt").expect("write");
    }
    append_receipt(&path, &good).expect("append");

    let read = read_receipts(&path).expect("read");
    assert_eq!(read.len(), 2, "the readable receipts are still readable");
}
