//! Focused behavioral contracts for `needle improvements` (N-T56,
//! `needle-65e66131`; ADR-029 step 6).
//!
//! Presentation only: both formats render deterministically from persisted
//! state, and nothing here writes.

use chrono::{DateTime, TimeZone, Utc};
use needle::cli::improvements::{
    append_decision, decisions_path, read_decisions, render_human, render_json, FleetTrend,
    GeneratedView, ImprovementsReport, ProposalRow, RefusedRow, DECISIONS_FILE,
    DEFAULT_WINDOW_DAYS,
};
use needle::learning::improvement::{
    decide_receipt, read_receipts, AcceptanceMeasure, AdmissionDecision, AdmissionRecord,
    AdmissionRoute, Cohort, CohortMeasures, Direction, EvidenceClass, EvidenceKind, EvidenceRef,
    ImpactMeasure, ImprovementProposal, ProposalScope, ReceiptThresholds, RefusalReason, Rollback,
};

fn at(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, day, 12, 0, 0).unwrap()
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

/// A report with one of everything, so both renderers are exercised fully.
fn report() -> ImprovementsReport {
    let subject = proposal();
    let receipt = decide_receipt(
        &subject,
        Cohort::from_scope(&subject.scope, vec!["needle-aaaa1111".to_string()]),
        window(20, 4),
        window(20, 12),
        Vec::new(),
        &ReceiptThresholds::default(),
        at(21),
    );

    ImprovementsReport {
        collected_at: at(21),
        window_days: DEFAULT_WINDOW_DAYS,
        generated: GeneratedView {
            proposals: vec![ProposalRow {
                signature: subject.signature.clone(),
                evidence_class: "red_baseline_workspace".to_string(),
                authority: "L4".to_string(),
                workspaces: vec!["NEEDLE".to_string()],
                intended_change: subject.intended_change.clone(),
                acceptance_measure: "verified_yield_per_attempt".to_string(),
                horizon_days: 7,
                state: "promoted".to_string(),
            }],
            refused: vec![RefusedRow {
                evidence_class: "canary_result".to_string(),
                subject: "exp-1".to_string(),
                reason: "acceptance horizon is zero days".to_string(),
            }],
            fixture_rows_excluded: 4,
        },
        decisions: vec![AdmissionRecord {
            signature: subject.signature.clone(),
            decision: AdmissionDecision::Admitted {
                route: AdmissionRoute::ImplementationBead {
                    workspace: "NEEDLE".to_string(),
                },
            },
            rank: 0,
        }],
        receipts: vec![receipt],
        trend: FleetTrend {
            attempts: 1467,
            verified: 677,
            yield_per_attempt: 677.0 / 1467.0,
            cost_usd: 3316.73,
            cost_per_verified: Some(3316.73 / 677.0),
        },
        inputs_unavailable: vec!["CI history for LOOM unreadable".to_string()],
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Both formats render deterministically
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn both_formats_render_deterministically_from_fixture_state() {
    let report = report();

    let human_once = render_human(&report);
    let human_twice = render_human(&report);
    assert_eq!(
        human_once, human_twice,
        "the operator view is deterministic"
    );

    let json_once = render_json(&report).expect("json");
    let json_twice = render_json(&report).expect("json");
    assert_eq!(json_once, json_twice, "the JSON report is deterministic");
}

#[test]
fn the_operator_view_leads_with_the_trend_and_names_every_section() {
    let rendered = render_human(&report());

    let trend_at = rendered.find("FLEET TREND").expect("trend section");
    let proposals_at = rendered
        .find("PROPOSALS FROM CURRENT EVIDENCE")
        .expect("proposals section");
    assert!(
        trend_at < proposals_at,
        "the trend comes first: it is what every receipt is ultimately about"
    );

    assert!(rendered.contains("1467 costed attempts"));
    assert!(rendered.contains("677 verified"));
    assert!(rendered.contains("46.1% yield"));
    assert!(
        rendered.contains("$4.90 per verified closure"),
        "cost per verified closure is rendered: {rendered}"
    );
    assert!(rendered.contains("RECEIPTS"));
    assert!(rendered.contains("PROMOTE"));
    assert!(rendered.contains("EVIDENCE THAT COULD NOT BECOME A PROPOSAL"));
    assert!(rendered.contains("4 fixture row(s) excluded"));
    assert!(rendered.contains("INPUTS UNAVAILABLE"));
    assert!(rendered.contains("CI history for LOOM unreadable"));
}

#[test]
fn a_cost_per_verified_closure_of_none_reads_as_unknown_not_zero() {
    let mut report = report();
    report.trend.verified = 0;
    report.trend.cost_per_verified = None;

    let rendered = render_human(&report);
    assert!(
        rendered.contains("cost per verified closure unknown"),
        "an unknown cost must not render as $0.00: {rendered}"
    );
    assert!(!rendered.contains("$0.00 per verified closure"));
}

#[test]
fn an_empty_report_renders_rather_than_failing() {
    let empty = ImprovementsReport {
        collected_at: at(21),
        window_days: DEFAULT_WINDOW_DAYS,
        generated: GeneratedView {
            proposals: Vec::new(),
            refused: Vec::new(),
            fixture_rows_excluded: 0,
        },
        decisions: Vec::new(),
        receipts: Vec::new(),
        trend: FleetTrend::default(),
        inputs_unavailable: Vec::new(),
    };

    let rendered = render_human(&empty);
    assert!(rendered.contains("no costed attempts in the window"));
    assert!(rendered.contains("none: no evidence class cleared its threshold"));
    assert!(rendered.contains("none yet"));
    render_json(&empty).expect("an empty report still encodes");
}

#[test]
fn the_json_report_round_trips() {
    let report = report();
    let encoded = render_json(&report).expect("json");
    let decoded: ImprovementsReport = serde_json::from_str(&encoded).expect("decodes");
    assert_eq!(
        render_json(&decoded).expect("json"),
        encoded,
        "the machine report is a stable contract"
    );
}

#[test]
fn a_large_proposal_set_is_summarized_rather_than_listed_in_full() {
    // At fleet scale this class runs to dozens; the live ledger produced 116.
    // A view that prints all of them is one nobody reads to the end.
    let mut report = report();
    let template = report.generated.proposals[0].clone();
    report.generated.proposals = (0..60)
        .map(|n| ProposalRow {
            signature: format!("{n:016x}"),
            ..template.clone()
        })
        .collect();

    let rendered = render_human(&report);
    assert!(rendered.contains("60 total:"), "the count is stated");
    assert!(
        rendered.contains("--json for all"),
        "and the complete set is pointed at"
    );
    let listed = rendered.matches("decided by").count();
    assert!(
        listed <= 15,
        "the operator view stays bounded, listed {listed}"
    );

    // The JSON report is not truncated.
    let encoded = render_json(&report).expect("json");
    let decoded: ImprovementsReport = serde_json::from_str(&encoded).expect("decodes");
    assert_eq!(decoded.generated.proposals.len(), 60);
}

// ──────────────────────────────────────────────────────────────────────────
// Persisted state only, and no writes
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn decisions_round_trip_through_the_journal_after_a_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(DECISIONS_FILE);

    let admitted = AdmissionRecord {
        signature: "aaaaaaaaaaaaaaaa".to_string(),
        decision: AdmissionDecision::Admitted {
            route: AdmissionRoute::ImplementationBead {
                workspace: "NEEDLE".to_string(),
            },
        },
        rank: 0,
    };
    let refused = AdmissionRecord {
        signature: "bbbbbbbbbbbbbbbb".to_string(),
        decision: AdmissionDecision::Refused {
            reason: RefusalReason::BudgetExhausted { per_day: 1 },
        },
        rank: 1,
    };

    append_decision(&path, &admitted).expect("append");
    append_decision(&path, &refused).expect("append");

    let read = read_decisions(&path).expect("read");
    assert_eq!(read, vec![admitted, refused], "state survives a restart");
}

#[test]
fn reading_journals_that_do_not_exist_creates_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let decisions = dir.path().join("improvements/decisions.jsonl");
    let receipts = dir.path().join("improvements/receipts.jsonl");

    assert!(read_decisions(&decisions).expect("ok").is_empty());
    assert!(read_receipts(&receipts).expect("ok").is_empty());

    assert!(
        !decisions.exists() && !receipts.exists(),
        "a read-only view must not create the files it reads"
    );
    assert!(
        !dir.path().join("improvements").exists(),
        "nor their parent directory"
    );
}

#[test]
fn one_malformed_decision_does_not_hide_the_others() {
    use std::io::Write;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("decisions.jsonl");

    let good = AdmissionRecord {
        signature: "cccccccccccccccc".to_string(),
        decision: AdmissionDecision::Refused {
            reason: RefusalReason::ShadowMode,
        },
        rank: 0,
    };
    append_decision(&path, &good).expect("append");
    {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open");
        writeln!(file, "not a decision").expect("write");
    }
    append_decision(&path, &good).expect("append");

    assert_eq!(read_decisions(&path).expect("read").len(), 2);
}

#[test]
fn the_journal_path_lives_under_the_state_directory() {
    let path = decisions_path();
    assert!(
        path.ends_with(DECISIONS_FILE),
        "decisions live at a stable path under the state root: {}",
        path.display()
    );
}
