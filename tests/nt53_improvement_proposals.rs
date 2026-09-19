//! Focused behavioral contracts for the N-T53 proposal generator
//! (`needle-908c1f25`, ADR-029 step 2).
//!
//! The fixture is shaped like the 2026-09-12..14 ledger the plan cites: beads
//! failing identically, a workspace on a worse adapter than one it could use,
//! spend concentrated on attempts that never verified, and a workspace that
//! closed nothing at all.

use chrono::{DateTime, TimeZone, Utc};
use needle::evidence_routing::LedgerRow;
use needle::learning::improvement::{
    generate, EvidenceClass, EvidenceKind, GeneratorThresholds, GENERATOR_VERSION,
};
use serde_json::json;

fn at(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, day, 12, 0, 0).unwrap()
}

fn now() -> DateTime<Utc> {
    at(14)
}

/// One `attempt.resolved` row.
///
/// Eight parameters, deliberately positional: a fixture row is a literal
/// table of ledger fields, and a builder here would obscure what each case
/// actually varies.
#[allow(clippy::too_many_arguments)]
fn row(
    workspace: &str,
    adapter: &str,
    outcome: &str,
    bead: &str,
    attempt: &str,
    terminal_reason: &str,
    cost: Option<f64>,
    day: u32,
) -> LedgerRow {
    let mut data = json!({
        "schema_version": 2,
        "attempt_id": attempt,
        "provisional": true,
        "bead_id": bead,
        "workspace": format!("/home/coding/{workspace}"),
        "worker": "glm-roam-18",
        "adapter": adapter,
        "prompt_template": "pluck",
        "template_version": "pluck-default",
        "gate_results": [],
        "outcome": outcome,
        "requested_action": "Released",
        "commits": [],
        "duration_ms": 1000,
        "exit_code": 1,
        "costed": cost.is_some(),
    });
    if !terminal_reason.is_empty() {
        data["terminal_reason"] = json!(terminal_reason);
    }
    if let Some(cost) = cost {
        data["estimated_cost_usd"] = json!(cost);
    }
    LedgerRow {
        timestamp: Some(at(day)),
        data,
    }
}

/// Rows shaped like the ledger window the plan cites.
fn fixture() -> Vec<LedgerRow> {
    let mut rows = Vec::new();

    // ── unchanged-retry: one bead, three identical failures ──────────────
    for (n, day) in [(1, 12), (2, 13), (3, 14)] {
        rows.push(row(
            "NEEDLE",
            "claude-code-glm-5.3-flash",
            "work_failure",
            "needle-aaaa1111",
            &format!("attempt-retry-{n}"),
            "gate:cargo-test",
            Some(3.0),
            day,
        ));
    }

    // ── workspace-adapter regret: reddit-media-player ────────────────────
    // A good adapter at 100% over 20, a bad one at 0% over 20: a 100-point
    // gap, well past the 15-point threshold.
    for n in 0..20 {
        rows.push(row(
            "reddit-media-player",
            "codex-gpt-5.6-luna-xhigh",
            "verified_success",
            &format!("rmp-good-{n}"),
            &format!("rmp-good-attempt-{n}"),
            "",
            Some(2.0),
            13,
        ));
        rows.push(row(
            "reddit-media-player",
            "claude-code-glm-5.3",
            "work_failure",
            &format!("rmp-bad-{n}"),
            &format!("rmp-bad-attempt-{n}"),
            "exit_code:1",
            Some(2.0),
            13,
        ));
    }

    // ── workspace-adapter regret: pdftract ───────────────────────────────
    for n in 0..20 {
        rows.push(row(
            "pdftract",
            "codex-gpt-5.6-luna-xhigh",
            "verified_success",
            &format!("pdf-good-{n}"),
            &format!("pdf-good-attempt-{n}"),
            "",
            Some(1.0),
            13,
        ));
        rows.push(row(
            "pdftract",
            "claude-code-glm-5.3",
            "work_failure",
            &format!("pdf-bad-{n}"),
            &format!("pdf-bad-attempt-{n}"),
            "exit_code:2",
            Some(1.0),
            13,
        ));
    }

    // ── unverified spend + red baseline: commitgraph ─────────────────────
    // 12 judged attempts, none verified, $4 each: 100% of $48 unverified.
    for n in 0..12 {
        rows.push(row(
            "commitgraph",
            "claude-code-glm-5.3-flash",
            "indeterminate",
            &format!("cg-{n}"),
            &format!("cg-attempt-{n}"),
            "signal:9",
            Some(4.0),
            13,
        ));
    }

    rows
}

// ──────────────────────────────────────────────────────────────────────────
// The classes the acceptance criteria name
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn the_fixture_yields_the_unchanged_retry_workspace_regret_and_unverified_spend_proposals() {
    let generated = generate(&fixture(), &GeneratorThresholds::default(), now());

    let count = |class: EvidenceClass| {
        generated
            .proposals
            .iter()
            .filter(|p| p.evidence_class == class)
            .count()
    };

    assert_eq!(
        count(EvidenceClass::RepeatedIdenticalFailures),
        1,
        "one bead failed identically three times"
    );
    assert_eq!(
        count(EvidenceClass::WorkspaceAdapterRegret),
        2,
        "reddit-media-player and pdftract each show adapter regret"
    );
    // Two: commitgraph at 100% of $48, and reddit-media-player at 50% of $80
    // — half its spend goes to the adapter that verifies nothing, which is
    // the same waste the regret proposal describes from the other side.
    assert_eq!(
        count(EvidenceClass::UnverifiedSpendConcentration),
        2,
        "both workspaces spend past the 40% share and the $25 floor"
    );
    let mut spend_workspaces: Vec<String> = generated
        .proposals
        .iter()
        .filter(|p| p.evidence_class == EvidenceClass::UnverifiedSpendConcentration)
        .flat_map(|p| p.scope.workspaces.clone())
        .collect();
    spend_workspaces.sort();
    assert_eq!(spend_workspaces, vec!["commitgraph", "reddit-media-player"]);
    assert_eq!(
        count(EvidenceClass::RedBaselineWorkspace),
        1,
        "commitgraph closed nothing across twelve judged attempts"
    );
}

#[test]
fn the_workspace_regret_proposals_name_the_two_workspaces_the_plan_cites() {
    let generated = generate(&fixture(), &GeneratorThresholds::default(), now());

    let mut workspaces: Vec<String> = generated
        .proposals
        .iter()
        .filter(|p| p.evidence_class == EvidenceClass::WorkspaceAdapterRegret)
        .flat_map(|p| p.scope.workspaces.clone())
        .collect();
    workspaces.sort();

    assert_eq!(workspaces, vec!["pdftract", "reddit-media-player"]);
}

#[test]
fn a_regret_proposal_names_both_adapters_and_proposes_moving_to_the_better_one() {
    let generated = generate(&fixture(), &GeneratorThresholds::default(), now());
    let regret = generated
        .proposals
        .iter()
        .find(|p| {
            p.evidence_class == EvidenceClass::WorkspaceAdapterRegret
                && p.scope.workspaces == vec!["pdftract".to_string()]
        })
        .expect("pdftract regret proposal");

    let adapters = regret.evidence_ids(EvidenceKind::Adapter);
    assert!(adapters.contains(&"codex-gpt-5.6-luna-xhigh"));
    assert!(adapters.contains(&"claude-code-glm-5.3"));
    assert!(
        regret.intended_change.contains("codex-gpt-5.6-luna-xhigh"),
        "the change moves towards the better adapter: {}",
        regret.intended_change
    );
    assert!(
        regret.rollback.automatic,
        "a routing change is revertible by its own controller"
    );
}

#[test]
fn a_repeated_failure_proposal_names_the_bead_the_reason_and_every_attempt() {
    let generated = generate(&fixture(), &GeneratorThresholds::default(), now());
    let retry = generated
        .proposals
        .iter()
        .find(|p| p.evidence_class == EvidenceClass::RepeatedIdenticalFailures)
        .expect("retry proposal");

    assert_eq!(
        retry.evidence_ids(EvidenceKind::Bead),
        vec!["needle-aaaa1111"]
    );
    assert_eq!(
        retry.evidence_ids(EvidenceKind::Attempt).len(),
        3,
        "every failing attempt is cited"
    );
    assert!(retry.intended_change.contains("gate:cargo-test"));
    assert_eq!(retry.generator_version, GENERATOR_VERSION);
}

// ──────────────────────────────────────────────────────────────────────────
// Thresholds: evidence below the floor is not a proposal
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn two_identical_failures_are_below_the_threshold() {
    let rows = vec![
        row(
            "NEEDLE",
            "a",
            "work_failure",
            "b1",
            "a1",
            "gate:x",
            Some(1.0),
            12,
        ),
        row(
            "NEEDLE",
            "a",
            "work_failure",
            "b1",
            "a2",
            "gate:x",
            Some(1.0),
            13,
        ),
    ];
    let generated = generate(&rows, &GeneratorThresholds::default(), now());
    assert!(
        !generated
            .proposals
            .iter()
            .any(|p| p.evidence_class == EvidenceClass::RepeatedIdenticalFailures),
        "three is the threshold, two is not evidence"
    );
}

#[test]
fn three_failures_with_different_reasons_are_three_problems_not_one_class() {
    let rows = vec![
        row(
            "NEEDLE",
            "a",
            "work_failure",
            "b1",
            "a1",
            "gate:x",
            Some(1.0),
            12,
        ),
        row(
            "NEEDLE",
            "a",
            "work_failure",
            "b1",
            "a2",
            "gate:y",
            Some(1.0),
            13,
        ),
        row(
            "NEEDLE",
            "a",
            "work_failure",
            "b1",
            "a3",
            "exit_code:2",
            Some(1.0),
            14,
        ),
    ];
    let generated = generate(&rows, &GeneratorThresholds::default(), now());
    assert!(
        !generated
            .proposals
            .iter()
            .any(|p| p.evidence_class == EvidenceClass::RepeatedIdenticalFailures),
        "a bead failing three different ways is not an unchanged-retry loop"
    );
}

#[test]
fn repeated_decompositions_are_not_repeated_failures() {
    // ADR-030: a decomposed attempt split its bead instead of delivering it
    // and earns neither success nor failure credit. Against the live ledger
    // this miscounted six beads that had correctly been broken up, and told
    // the fleet to stop "failing" at them.
    let rows: Vec<LedgerRow> = (0..7)
        .map(|n| {
            row(
                "NEEDLE",
                "a",
                "decomposed",
                "bf-az0okb",
                &format!("a{n}"),
                "decomposed:split_template",
                Some(1.0),
                13,
            )
        })
        .collect();

    let generated = generate(&rows, &GeneratorThresholds::default(), now());
    assert!(
        !generated
            .proposals
            .iter()
            .any(|p| p.evidence_class == EvidenceClass::RepeatedIdenticalFailures),
        "seven decompositions are seven splits, not seven failures"
    );
}

#[test]
fn an_adapter_below_the_evidence_floor_does_not_produce_a_regret_proposal() {
    let mut rows = Vec::new();
    // Only five attempts each: below the floor of twenty.
    for n in 0..5 {
        rows.push(row(
            "small",
            "good",
            "verified_success",
            &format!("g{n}"),
            &format!("ga{n}"),
            "",
            Some(1.0),
            13,
        ));
        rows.push(row(
            "small",
            "bad",
            "work_failure",
            &format!("b{n}"),
            &format!("ba{n}"),
            "exit_code:1",
            Some(1.0),
            13,
        ));
    }
    let generated = generate(&rows, &GeneratorThresholds::default(), now());
    assert!(
        !generated
            .proposals
            .iter()
            .any(|p| p.evidence_class == EvidenceClass::WorkspaceAdapterRegret),
        "a rate computed from five attempts is not evidence"
    );
}

#[test]
fn unverified_spend_below_the_dollar_floor_is_not_worth_a_proposal() {
    let mut rows = Vec::new();
    // 100% unverified, but only $3 total.
    for n in 0..3 {
        rows.push(row(
            "tiny",
            "a",
            "work_failure",
            &format!("t{n}"),
            &format!("ta{n}"),
            "exit_code:1",
            Some(1.0),
            13,
        ));
    }
    let generated = generate(&rows, &GeneratorThresholds::default(), now());
    assert!(
        !generated
            .proposals
            .iter()
            .any(|p| p.evidence_class == EvidenceClass::UnverifiedSpendConcentration),
        "a concentrated share of three dollars is not worth acting on"
    );
}

#[test]
fn uncosted_rows_are_not_counted_as_free_spend() {
    let mut rows = Vec::new();
    // Thirty uncosted failures: cost unknown, never zero (ADR-030). There is
    // no costed evidence here, so no spend proposal can be made.
    for n in 0..30 {
        rows.push(row(
            "uncosted",
            "a",
            "work_failure",
            &format!("u{n}"),
            &format!("ua{n}"),
            "exit_code:1",
            None,
            13,
        ));
    }
    let generated = generate(&rows, &GeneratorThresholds::default(), now());
    assert!(
        !generated
            .proposals
            .iter()
            .any(|p| p.evidence_class == EvidenceClass::UnverifiedSpendConcentration),
        "an uncosted attempt's cost is unknown; it cannot prove a spend concentration"
    );
}

// ──────────────────────────────────────────────────────────────────────────
// Purity, determinism, fixture exclusion
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn fixture_rows_are_excluded_before_any_denominator_is_computed() {
    let mut rows = fixture();
    let before = generate(&rows, &GeneratorThresholds::default(), now());

    // Contaminating rows of both documented shapes: a test worker, and the
    // relative-root workspace.
    for n in 0..40 {
        let mut contaminant = row(
            "NEEDLE",
            "claude-code-glm-5.3-flash",
            "verified_success",
            &format!("fake-{n}"),
            &format!("fake-attempt-{n}"),
            "",
            Some(9.0),
            13,
        );
        contaminant.data["worker"] = json!("echo-test-test-worker");
        rows.push(contaminant);

        let mut relative = row(
            "NEEDLE",
            "claude-code-glm-5.3-flash",
            "verified_success",
            &format!("rel-{n}"),
            &format!("rel-attempt-{n}"),
            "",
            Some(9.0),
            13,
        );
        relative.data["workspace"] = json!(".");
        rows.push(relative);
    }

    let after = generate(&rows, &GeneratorThresholds::default(), now());
    assert_eq!(
        after.fixture_rows_excluded, 80,
        "both fixture shapes are counted as excluded"
    );
    assert_eq!(
        before.proposals, after.proposals,
        "fixture contamination must not change a single proposal"
    );
}

#[test]
fn generation_is_deterministic_and_ordered() {
    let rows = fixture();
    let first = generate(&rows, &GeneratorThresholds::default(), now());

    let mut shuffled = rows.clone();
    shuffled.reverse();
    let second = generate(&shuffled, &GeneratorThresholds::default(), now());

    assert_eq!(
        first.proposals, second.proposals,
        "row order must not change the output"
    );

    let classes: Vec<EvidenceClass> = first.proposals.iter().map(|p| p.evidence_class).collect();
    let mut sorted = classes.clone();
    sorted.sort();
    assert_eq!(classes, sorted, "proposals are emitted in class order");
}

#[test]
fn an_empty_ledger_produces_nothing_rather_than_failing() {
    let generated = generate(&[], &GeneratorThresholds::default(), now());
    assert!(generated.proposals.is_empty());
    assert!(generated.refused.is_empty());
    assert_eq!(generated.fixture_rows_excluded, 0);
}

#[test]
fn every_generated_proposal_carries_a_computable_acceptance_measure() {
    let generated = generate(&fixture(), &GeneratorThresholds::default(), now());
    assert!(!generated.proposals.is_empty());

    for proposal in &generated.proposals {
        assert!(
            proposal.acceptance.min_delta > 0.0 && proposal.acceptance.min_delta.is_finite(),
            "{} has an undecidable threshold",
            proposal.signature
        );
        assert!(
            proposal.acceptance.horizon_days > 0,
            "{} has no horizon",
            proposal.signature
        );
        assert!(
            proposal.authority <= proposal.evidence_class.max_authority(),
            "{} claims more than its class allows",
            proposal.signature
        );
    }
}
