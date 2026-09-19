//! Focused behavioral contracts for admission budgets, deduplication and
//! backpressure (N-T07, `needle-f754b4cb`).

mod admission_support;

use admission_support::*;
use needle::learning::improvement::{
    AdmissionDecision, AdmissionPolicy, EvidenceClass, RefusalReason, DEFAULT_ADMISSION_PER_DAY,
    DEFAULT_MAX_OPEN_ADMITTED,
};

#[test]
fn the_default_budget_is_one_admitted_proposal_per_day() {
    assert_eq!(
        DEFAULT_ADMISSION_PER_DAY, 1,
        "the plan's activation order starts at one admitted L4 proposal per day"
    );
    assert_eq!(
        AdmissionPolicy::default().per_day,
        DEFAULT_ADMISSION_PER_DAY
    );
    assert_eq!(
        AdmissionPolicy::default().max_open_admitted,
        DEFAULT_MAX_OPEN_ADMITTED
    );
}

#[test]
fn the_budget_admits_the_highest_ranked_and_refuses_the_rest_with_a_reason() {
    let proposals = vec![
        proposal(EvidenceClass::RedBaselineWorkspace, "a"),
        proposal(EvidenceClass::RedBaselineWorkspace, "b"),
        proposal(EvidenceClass::RedBaselineWorkspace, "c"),
    ];
    let records = run(&proposals, &live_policy(), World::default());

    let admitted: Vec<_> = records
        .iter()
        .filter(|r| r.decision.is_admitted())
        .collect();
    assert_eq!(admitted.len(), 1, "one per day");
    assert_eq!(
        admitted[0].rank, 0,
        "the highest-ranked one is the one taken"
    );

    for record in records.iter().filter(|r| !r.decision.is_admitted()) {
        assert_eq!(
            record.decision,
            AdmissionDecision::Refused {
                reason: RefusalReason::BudgetExhausted { per_day: 1 }
            },
            "a proposal refused for budget says so"
        );
    }
}

#[test]
fn a_budget_already_spent_today_admits_nothing_further() {
    let proposals = vec![proposal(EvidenceClass::RedBaselineWorkspace, "a")];
    let world = World {
        admitted_today: 1,
        ..World::default()
    };
    let records = run(&proposals, &live_policy(), world);

    assert_eq!(
        records[0].decision,
        AdmissionDecision::Refused {
            reason: RefusalReason::BudgetExhausted { per_day: 1 }
        },
        "today's admissions count against today's budget"
    );
}

#[test]
fn a_raised_budget_admits_more() {
    let proposals = vec![
        proposal(EvidenceClass::RedBaselineWorkspace, "a"),
        proposal(EvidenceClass::RedBaselineWorkspace, "b"),
        proposal(EvidenceClass::RedBaselineWorkspace, "c"),
    ];
    let policy = AdmissionPolicy {
        per_day: 2,
        ..live_policy()
    };
    let records = run(&proposals, &policy, World::default());

    assert_eq!(
        records.iter().filter(|r| r.decision.is_admitted()).count(),
        2
    );
}

// ──────────────────────────────────────────────────────────────────────────
// Deduplication
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn a_new_evidence_class_creates_exactly_one_bead_across_two_concurrent_submissions() {
    // The same proposal submitted twice in one run: the second is a duplicate
    // of the first, not a second thing to admit.
    let single = proposal(EvidenceClass::RedBaselineWorkspace, "NEEDLE");
    let proposals = vec![single.clone(), single.clone()];

    let policy = AdmissionPolicy {
        per_day: 10,
        ..live_policy()
    };
    let records = run(&proposals, &policy, World::default());

    assert_eq!(
        records.iter().filter(|r| r.decision.is_admitted()).count(),
        1,
        "two concurrent submissions of one evidence signature create one bead"
    );
    assert!(
        records.iter().any(|r| matches!(
            r.decision,
            AdmissionDecision::Refused {
                reason: RefusalReason::DuplicateInRun
            }
        )),
        "and the duplicate is recorded as such"
    );
}

#[test]
fn deduplication_against_the_estate_beats_the_budget_check() {
    // An owned proposal must not consume a budget slot: it was never going to
    // become work, and spending the day's budget on it would starve a
    // proposal that would have.
    let proposals = vec![
        proposal(EvidenceClass::RepeatedIdenticalFailures, "owned"),
        proposal(EvidenceClass::RedBaselineWorkspace, "fresh"),
    ];
    let world = World {
        owner_for: Some(("owned".to_string(), "needle-d6c5397a".to_string())),
        ..World::default()
    };
    let records = run(&proposals, &live_policy(), world);

    let owned = records
        .iter()
        .find(|r| r.signature == proposals[0].signature)
        .expect("record");
    assert!(matches!(
        owned.decision,
        AdmissionDecision::Refused {
            reason: RefusalReason::AlreadyOwned { .. }
        }
    ));

    let fresh = records
        .iter()
        .find(|r| r.signature == proposals[1].signature)
        .expect("record");
    assert!(
        fresh.decision.is_admitted(),
        "the owned proposal did not consume the day's only slot"
    );
}

#[test]
fn a_non_executable_proposal_does_not_consume_the_budget_either() {
    let proposals = vec![proposal(EvidenceClass::RedBaselineWorkspace, "a")];
    let world = World {
        non_executable: true,
        ..World::default()
    };
    let records = run(&proposals, &live_policy(), world);

    assert!(matches!(
        records[0].decision,
        AdmissionDecision::Refused {
            reason: RefusalReason::NotExecutable { .. }
        }
    ));
}

// ──────────────────────────────────────────────────────────────────────────
// Backpressure
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn backpressure_refuses_while_admitted_work_is_still_open() {
    let proposals = vec![proposal(EvidenceClass::RedBaselineWorkspace, "a")];
    let world = World {
        open_admitted: DEFAULT_MAX_OPEN_ADMITTED,
        ..World::default()
    };
    let records = run(&proposals, &live_policy(), world);

    assert_eq!(
        records[0].decision,
        AdmissionDecision::Refused {
            reason: RefusalReason::Backpressure {
                open: DEFAULT_MAX_OPEN_ADMITTED,
                ceiling: DEFAULT_MAX_OPEN_ADMITTED,
            }
        },
        "more proposals into a backed-up factory make the pile deeper, not the factory faster"
    );
}

#[test]
fn backpressure_lifts_as_admitted_work_finishes() {
    let proposals = vec![proposal(EvidenceClass::RedBaselineWorkspace, "a")];
    let world = World {
        open_admitted: DEFAULT_MAX_OPEN_ADMITTED - 1,
        ..World::default()
    };
    let records = run(&proposals, &live_policy(), world);
    assert!(records[0].decision.is_admitted());
}

#[test]
fn backpressure_is_checked_before_the_budget() {
    // Both would refuse; the reported reason must be the one that is actually
    // blocking, or an operator raising the budget would see no change.
    let proposals = vec![proposal(EvidenceClass::RedBaselineWorkspace, "a")];
    let world = World {
        open_admitted: DEFAULT_MAX_OPEN_ADMITTED,
        admitted_today: 5,
        ..World::default()
    };
    let records = run(&proposals, &live_policy(), world);

    assert!(
        matches!(
            records[0].decision,
            AdmissionDecision::Refused {
                reason: RefusalReason::Backpressure { .. }
            }
        ),
        "backpressure is the binding constraint and is what gets reported"
    );
}

#[test]
fn admission_is_deterministic_over_the_same_inputs() {
    let proposals = vec![
        proposal(EvidenceClass::RedBaselineWorkspace, "a"),
        proposal(EvidenceClass::RedBaselineWorkspace, "b"),
        proposal(EvidenceClass::RedBaselineWorkspace, "c"),
    ];
    let first = run(&proposals, &live_policy(), World::default());
    let second = run(&proposals, &live_policy(), World::default());
    assert_eq!(first, second);
}
