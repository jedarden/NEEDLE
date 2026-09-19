//! Focused behavioral contracts for admission decisions (N-T07,
//! `needle-43c0d818`).
//!
//! Every proposal gets a recorded decision with a reason. There is no silent
//! drop, because an operator has to be able to tell "seen and refused" from
//! "never generated".

mod admission_support;

use admission_support::*;
use needle::learning::improvement::{
    AdmissionDecision, AdmissionPolicy, AdmissionRoute, AuthorityLevel, EvidenceClass,
    RefusalReason,
};

#[test]
fn every_proposal_receives_a_decision_and_none_is_silently_dropped() {
    let proposals = vec![
        proposal(EvidenceClass::RedBaselineWorkspace, "NEEDLE"),
        proposal(EvidenceClass::RepeatedIdenticalFailures, "commitgraph"),
        proposal(EvidenceClass::WorkspaceAdapterRegret, "pdftract"),
    ];
    let records = run(&proposals, &live_policy(), World::default());

    assert_eq!(
        records.len(),
        proposals.len(),
        "one record per proposal, always"
    );
    for record in &records {
        assert!(
            proposals.iter().any(|p| p.signature == record.signature),
            "every record addresses a real proposal"
        );
    }
}

#[test]
fn an_l4_proposal_is_admitted_as_an_implementation_bead_in_its_owning_workspace() {
    let proposals = vec![proposal(EvidenceClass::RedBaselineWorkspace, "NEEDLE")];
    let records = run(&proposals, &live_policy(), World::default());

    assert_eq!(
        records[0].decision,
        AdmissionDecision::Admitted {
            route: AdmissionRoute::ImplementationBead {
                workspace: "NEEDLE".to_string()
            }
        },
        "L4 becomes a bead the fleet works through the normal factory path"
    );
}

#[test]
fn an_l1_proposal_is_admitted_to_its_owning_controller_rather_than_a_bead() {
    // Workspace adapter regret tops out at L1 — a routing change the evidence
    // routing controller applies itself.
    let proposals = vec![proposal(EvidenceClass::WorkspaceAdapterRegret, "pdftract")];
    let records = run(&proposals, &live_policy(), World::default());

    assert_eq!(
        records[0].decision,
        AdmissionDecision::Admitted {
            route: AdmissionRoute::Controller {
                authority: AuthorityLevel::L1
            }
        }
    );
}

#[test]
fn an_l5_proposal_is_refused_until_gate_d() {
    let proposals = vec![l5_proposal()];

    let records = run(&proposals, &live_policy(), World::default());
    assert_eq!(
        records[0].decision,
        AdmissionDecision::Refused {
            reason: RefusalReason::AuthorityNotPermitted {
                authority: AuthorityLevel::L5
            }
        },
        "L5 is refused before anything else is even considered"
    );

    let after_gate_d = AdmissionPolicy {
        gate_d_satisfied: true,
        ..live_policy()
    };
    let records = run(&proposals, &after_gate_d, World::default());
    assert!(
        records[0].decision.is_admitted(),
        "Gate D is what unlocks L5, and nothing else"
    );
}

#[test]
fn authority_is_checked_before_budget_so_a_full_budget_cannot_mask_an_l5() {
    let proposals = vec![l5_proposal()];
    let world = World {
        admitted_today: 99,
        ..World::default()
    };
    let records = run(&proposals, &live_policy(), world);

    assert_eq!(
        records[0].decision,
        AdmissionDecision::Refused {
            reason: RefusalReason::AuthorityNotPermitted {
                authority: AuthorityLevel::L5
            }
        },
        "the reason an L5 was refused must be its authority, not a coincidental budget"
    );
}

#[test]
fn an_owned_evidence_class_creates_nothing_and_names_its_owner() {
    let proposals = vec![proposal(EvidenceClass::RepeatedIdenticalFailures, "NEEDLE")];
    let world = World {
        owner: Some("needle-d6c5397a".to_string()),
        ..World::default()
    };
    let records = run(&proposals, &live_policy(), world);

    assert_eq!(
        records[0].decision,
        AdmissionDecision::Refused {
            reason: RefusalReason::AlreadyOwned {
                owner: "needle-d6c5397a".to_string()
            }
        },
        "the plan already owns unchanged retries; filing again is the ADR-015 failure"
    );
}

#[test]
fn a_non_executable_proposal_is_refused_with_the_executability_reason() {
    let proposals = vec![proposal(EvidenceClass::RedBaselineWorkspace, "NEEDLE")];
    let world = World {
        non_executable: true,
        ..World::default()
    };
    let records = run(&proposals, &live_policy(), world);

    match &records[0].decision {
        AdmissionDecision::Refused {
            reason: RefusalReason::NotExecutable { detail },
        } => assert!(
            detail.contains("overlap scope"),
            "the executability reason is carried through verbatim: {detail}"
        ),
        other => panic!("expected a NotExecutable refusal, got {other:?}"),
    }
}

#[test]
fn shadow_mode_decides_everything_and_admits_nothing() {
    let proposals = vec![
        proposal(EvidenceClass::RedBaselineWorkspace, "NEEDLE"),
        proposal(EvidenceClass::WorkspaceAdapterRegret, "pdftract"),
    ];
    let records = run(&proposals, &AdmissionPolicy::default(), World::default());

    assert_eq!(records.len(), 2);
    assert!(
        records.iter().all(|r| !r.decision.is_admitted()),
        "shadow admits nothing"
    );
    assert!(
        records.iter().all(|r| matches!(
            r.decision,
            AdmissionDecision::Refused {
                reason: RefusalReason::ShadowMode
            }
        )),
        "and says shadow was the reason, not something incidental"
    );
}

#[test]
fn shadow_mode_is_the_default_policy() {
    assert!(
        AdmissionPolicy::default().shadow,
        "the plan's activation order runs N-T53 in shadow first"
    );
    assert!(
        !AdmissionPolicy::default().gate_d_satisfied,
        "Gate D is not satisfied by default"
    );
}

#[test]
fn shadow_mode_still_reports_which_proposal_would_have_been_admitted() {
    // Shadow is checked last, after ownership and executability, so a shadow
    // run distinguishes "would have been admitted" from "would have been
    // refused anyway".
    let proposals = vec![
        proposal(EvidenceClass::RedBaselineWorkspace, "NEEDLE"),
        proposal(EvidenceClass::RepeatedIdenticalFailures, "owned-one"),
    ];
    let world = World {
        owner_for: Some(("owned-one".to_string(), "needle-7b9718bc".to_string())),
        ..World::default()
    };
    let records = run(&proposals, &AdmissionPolicy::default(), world);

    let owned = records
        .iter()
        .find(|r| r.signature == proposals[1].signature)
        .expect("record");
    assert!(
        matches!(
            owned.decision,
            AdmissionDecision::Refused {
                reason: RefusalReason::AlreadyOwned { .. }
            }
        ),
        "an owned proposal reads as owned even in shadow, not as shadow"
    );

    let fresh = records
        .iter()
        .find(|r| r.signature == proposals[0].signature)
        .expect("record");
    assert!(matches!(
        fresh.decision,
        AdmissionDecision::Refused {
            reason: RefusalReason::ShadowMode
        }
    ));
}

#[test]
fn every_refusal_reason_carries_a_stable_wire_tag() {
    let tags = [
        RefusalReason::AlreadyOwned {
            owner: "x".to_string(),
        }
        .tag(),
        RefusalReason::DuplicateInRun.tag(),
        RefusalReason::BudgetExhausted { per_day: 1 }.tag(),
        RefusalReason::Backpressure {
            open: 3,
            ceiling: 3,
        }
        .tag(),
        RefusalReason::AuthorityNotPermitted {
            authority: AuthorityLevel::L5,
        }
        .tag(),
        RefusalReason::NotExecutable {
            detail: "x".to_string(),
        }
        .tag(),
        RefusalReason::ShadowMode.tag(),
    ];
    assert_eq!(
        tags,
        [
            "already_owned",
            "duplicate_in_run",
            "budget_exhausted",
            "backpressure",
            "authority_not_permitted",
            "not_executable",
            "shadow_mode",
        ]
    );
}

#[test]
fn a_decision_records_the_rank_it_was_made_at() {
    let proposals = vec![
        proposal(EvidenceClass::RedBaselineWorkspace, "a"),
        proposal(EvidenceClass::RedBaselineWorkspace, "b"),
        proposal(EvidenceClass::RedBaselineWorkspace, "c"),
    ];
    let records = run(&proposals, &live_policy(), World::default());
    let ranks: Vec<usize> = records.iter().map(|r| r.rank).collect();
    assert_eq!(
        ranks,
        vec![0, 1, 2],
        "'refused: budget exhausted' is only interpretable beside a queue position"
    );
}
