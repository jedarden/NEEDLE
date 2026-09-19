//! Focused behavioral contracts for executability admission
//! (N-T07, `needle-8c3520f7`).
//!
//! The headline case from the acceptance criteria: "improve coverage" is
//! refused with an explicit reason, and a concrete uncovered branch plus a
//! focused test command is admitted.

use chrono::{TimeZone, Utc};
use needle::learning::improvement::{
    assess_executability, AcceptanceMeasure, Confidence, Direction, EffortEstimate, EvidenceClass,
    EvidenceKind, EvidenceRef, ExecutionPlan, ImpactContract, ImpactMeasure, ImprovementProposal,
    NonExecutable, ProducerEvidence, ProposalScope, Rollback, WorkspaceImpactProfile,
};

fn at(day: u32) -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(2026, 9, day, 12, 0, 0).unwrap()
}

fn proposal_asking(intended_change: &str) -> ImprovementProposal {
    ImprovementProposal::new(
        EvidenceClass::RepeatedIdenticalFailures,
        vec![EvidenceRef::new(
            EvidenceKind::Bead,
            "needle-abc12345",
            at(12),
        )],
        ProposalScope::new(["NEEDLE".to_string()], []),
        intended_change,
        EvidenceClass::RepeatedIdenticalFailures.max_authority(),
        "the fingerprint stops recurring",
        AcceptanceMeasure {
            measure: ImpactMeasure::FingerprintRecurrence,
            direction: Direction::Decrease,
            min_delta: 0.1,
            horizon_days: 14,
        },
        Rollback {
            description: "revert the commit".to_string(),
            automatic: false,
        },
        at(14),
        1,
    )
    .expect("valid proposal")
}

fn contract() -> ImpactContract {
    let profile = WorkspaceImpactProfile::neutral("NEEDLE");
    ImpactContract::assemble(
        "NEEDLE",
        Some(&profile),
        ProducerEvidence::new(
            Confidence::Medium,
            EffortEstimate::Small,
            vec![EvidenceRef::new(
                EvidenceKind::Bead,
                "needle-abc12345",
                at(12),
            )],
            "three identical failures",
        ),
        at(14),
    )
}

fn good_plan() -> ExecutionPlan {
    ExecutionPlan {
        acceptance_command: "cargo test --test nt53_improvement_proposals".to_string(),
        overlap_scope: vec!["src/learning/improvement/generator.rs".to_string()],
        dependencies: Vec::new(),
        approved_intent: None,
    }
}

// ──────────────────────────────────────────────────────────────────────────
// The two cases the acceptance criteria name
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn improve_coverage_is_refused_with_an_explicit_reason() {
    let vague = proposal_asking("improve coverage");
    let plan = ExecutionPlan {
        acceptance_command: "cargo test".to_string(),
        overlap_scope: vec!["everything".to_string()],
        dependencies: Vec::new(),
        approved_intent: None,
    };

    let rejection = assess_executability(&vague, &contract(), &plan).expect_err("must be refused");

    assert_eq!(
        rejection,
        NonExecutable::NotConcrete {
            phrase: "improve".to_string()
        }
    );
    let reason = rejection.to_string();
    assert!(
        reason.contains("directional") && reason.contains("nothing concrete"),
        "the refusal must say why, got: {reason}"
    );
}

#[test]
fn a_concrete_uncovered_branch_with_a_focused_test_command_is_admissible() {
    let concrete = proposal_asking(
        "cover the untaken `Err` branch in `resolve_usage` at src/attempt_accounting.rs:210",
    );
    let plan = ExecutionPlan {
        acceptance_command: "cargo test --test nt47_attempt_spend_cap".to_string(),
        overlap_scope: vec!["src/attempt_accounting.rs".to_string()],
        dependencies: Vec::new(),
        approved_intent: None,
    };

    let admitted = assess_executability(&concrete, &contract(), &plan).expect("admissible");
    assert_eq!(admitted.proposal.signature, concrete.signature);
    assert_eq!(admitted.plan, plan);
}

#[test]
fn directional_wording_is_fine_when_the_scope_names_a_real_path() {
    // "reduce" is directional, but the change points at a file, so it is a
    // destination rather than a direction.
    let concrete = proposal_asking("improve the retry bound in src/worker/mod.rs to 3");
    let admitted = assess_executability(&concrete, &contract(), &good_plan());
    assert!(
        admitted.is_ok(),
        "a directional word with a concrete scope is admissible: {admitted:?}"
    );
}

// ──────────────────────────────────────────────────────────────────────────
// One machine-checkable acceptance command
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn a_missing_acceptance_command_is_refused() {
    let plan = ExecutionPlan {
        acceptance_command: "   ".to_string(),
        ..good_plan()
    };
    assert_eq!(
        assess_executability(&proposal_asking("fix src/x.rs"), &contract(), &plan),
        Err(NonExecutable::NoAcceptanceCommand)
    );
}

#[test]
fn a_chained_acceptance_command_is_refused_for_each_chainer() {
    for (command, chainer) in [
        ("cargo test --test a && cargo clippy", "&&"),
        ("cargo test --test a; cargo fmt", ";"),
        ("cargo test --test a | tee log", "|"),
        ("cargo test --test a\ncargo fmt", "\n"),
        ("cargo test --test $(echo a)", "$("),
    ] {
        let plan = ExecutionPlan {
            acceptance_command: command.to_string(),
            ..good_plan()
        };
        let rejection = assess_executability(&proposal_asking("fix src/x.rs"), &contract(), &plan)
            .expect_err("chained commands are refused");
        assert_eq!(
            rejection,
            NonExecutable::NotASingleCommand {
                chainer: chainer.to_string()
            },
            "{command:?} must be refused as a chain on {chainer:?}"
        );
    }
}

#[test]
fn an_unrunnable_acceptance_command_is_refused() {
    let plan = ExecutionPlan {
        acceptance_command: "echo done".to_string(),
        ..good_plan()
    };
    let rejection = assess_executability(&proposal_asking("fix src/x.rs"), &contract(), &plan)
        .expect_err("echo is not an acceptance check");
    assert_eq!(
        rejection,
        NonExecutable::NotMachineCheckable {
            command: "echo done".to_string()
        }
    );
    assert!(
        rejection.to_string().contains("cargo test"),
        "the refusal names what would be acceptable"
    );
}

#[test]
fn every_declared_runner_prefix_is_accepted() {
    for command in [
        "cargo test --test nt53_improvement_proposals",
        "cargo clippy --lib",
        "cargo check --all-targets",
        "cargo fmt --check",
        "bash tests/dod-modes/run.sh",
        "bash scripts/definition-of-done.sh --fast",
        "./scripts/definition-of-done.sh --fast",
        "scripts/check-test-policy.sh",
    ] {
        let plan = ExecutionPlan {
            acceptance_command: command.to_string(),
            ..good_plan()
        };
        assert!(
            assess_executability(&proposal_asking("fix src/x.rs"), &contract(), &plan).is_ok(),
            "{command} should be runnable"
        );
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Bounded scope, evidence, fingerprint
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn a_change_with_no_overlap_scope_is_refused() {
    for scope in [Vec::new(), vec!["  ".to_string()]] {
        let plan = ExecutionPlan {
            overlap_scope: scope,
            ..good_plan()
        };
        assert_eq!(
            assess_executability(&proposal_asking("fix src/x.rs"), &contract(), &plan),
            Err(NonExecutable::UnboundedScope),
            "a change that names no paths cannot be serialized against another"
        );
    }
}

#[test]
fn an_operator_approved_intent_substitutes_for_evidence_but_nothing_else_does() {
    // The envelope refuses an evidence-free proposal, so the approved-intent
    // path is reached by clearing evidence on an already-built record — the
    // shape an operator-authored proposal would arrive in.
    let mut intent_only = proposal_asking("fix src/x.rs");
    intent_only.evidence.clear();

    assert_eq!(
        assess_executability(&intent_only, &contract(), &good_plan()),
        Err(NonExecutable::NoEvidenceOrApprovedIntent),
        "no evidence and no approved intent is refused"
    );

    let approved = ExecutionPlan {
        approved_intent: Some("operator decision 2026-09-18".to_string()),
        ..good_plan()
    };
    assert!(
        assess_executability(&intent_only, &contract(), &approved).is_ok(),
        "an operator-approved intent is the declared exception"
    );

    let blank = ExecutionPlan {
        approved_intent: Some("   ".to_string()),
        ..good_plan()
    };
    assert_eq!(
        assess_executability(&intent_only, &contract(), &blank),
        Err(NonExecutable::NoEvidenceOrApprovedIntent),
        "an empty intent string is not an approval"
    );
}

#[test]
fn a_proposal_without_a_stable_fingerprint_is_refused() {
    let mut unsigned = proposal_asking("fix src/x.rs");
    unsigned.signature.clear();
    assert_eq!(
        assess_executability(&unsigned, &contract(), &good_plan()),
        Err(NonExecutable::NoStableFingerprint)
    );
}

#[test]
fn declared_dependencies_are_carried_through_admission() {
    let plan = ExecutionPlan {
        dependencies: vec!["needle-d6c5397a".to_string()],
        ..good_plan()
    };
    let admitted =
        assess_executability(&proposal_asking("fix src/x.rs"), &contract(), &plan).expect("ok");
    assert_eq!(admitted.plan.dependencies, vec!["needle-d6c5397a"]);
}
