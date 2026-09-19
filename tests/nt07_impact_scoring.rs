//! Focused behavioral contracts for deterministic expected-application-value
//! ranking (N-T07, `needle-29f396d3`).
//!
//! The ranking must be explainable, replayable, and immune to a producer
//! talking up its own proposal.

use chrono::{TimeZone, Utc};
use needle::learning::improvement::{
    rank, score, AcceptanceMeasure, Band, Confidence, Direction, EffortEstimate, EvidenceClass,
    EvidenceKind, EvidenceRef, ImpactContract, ImpactMeasure, ImprovementProposal,
    ProducerEvidence, ProposalScope, Rollback, ScoringPolicy, WorkspaceImpactProfile,
    SCORING_POLICY_VERSION,
};

fn at(day: u32) -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(2026, 9, day, 12, 0, 0).unwrap()
}

fn proposal_named(workspace: &str, evidence_id: &str, observed: u32) -> ImprovementProposal {
    ImprovementProposal::new(
        EvidenceClass::RedBaselineWorkspace,
        vec![EvidenceRef::new(
            EvidenceKind::Attempt,
            evidence_id,
            at(observed),
        )],
        ProposalScope::new([workspace.to_string()], []),
        "raise the workspace default gate to build+test in `.needle.yaml`",
        EvidenceClass::RedBaselineWorkspace.max_authority(),
        "verified-closure yield per attempt rises",
        AcceptanceMeasure {
            measure: ImpactMeasure::VerifiedYieldPerAttempt,
            direction: Direction::Increase,
            min_delta: 0.05,
            horizon_days: 7,
        },
        Rollback {
            description: "restore the previous gate".to_string(),
            automatic: true,
        },
        at(14),
        1,
    )
    .expect("valid proposal")
}

fn profile(workspace: &str, band: Band, public: bool) -> WorkspaceImpactProfile {
    let mut profile = WorkspaceImpactProfile::neutral(workspace);
    profile.severity = band;
    profile.time_sensitivity = band;
    profile.strategic_fit = band;
    profile.public_visibility = public;
    profile
}

fn producer(confidence: Confidence, effort: EffortEstimate, observed: u32) -> ProducerEvidence {
    ProducerEvidence::new(
        confidence,
        effort,
        vec![EvidenceRef::new(EvidenceKind::Attempt, "a", at(observed))],
        "summary",
    )
}

// ──────────────────────────────────────────────────────────────────────────
// Explainable and replayable
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn a_score_exposes_its_components_and_policy_version() {
    let proposal = proposal_named("NEEDLE", "a", 13);
    let profile = profile("NEEDLE", Band::MAX, false);
    let contract = ImpactContract::assemble(
        "NEEDLE",
        Some(&profile),
        producer(Confidence::High, EffortEstimate::Small, 13),
        at(14),
    );

    let scored = score(&proposal, &contract, &ScoringPolicy::default(), at(14));

    assert_eq!(scored.signature, proposal.signature);
    assert_eq!(scored.policy_version, SCORING_POLICY_VERSION);
    assert!((scored.components.application_value - 1.0).abs() < f64::EPSILON);
    assert!((scored.components.confidence - 1.0).abs() < f64::EPSILON);
    assert!((scored.components.effort_divisor - 1.0).abs() < f64::EPSILON);
    assert!(!scored.components.evidence_stale);
    assert!(!scored.components.profile_expired);
    assert!(!scored.components.profile_defaulted);
    assert!(
        (scored.score - 1.0).abs() < f64::EPSILON,
        "max value, full confidence, small effort scores 1.0, got {}",
        scored.score
    );
}

#[test]
fn scoring_is_deterministic_across_runs() {
    let proposal = proposal_named("NEEDLE", "a", 13);
    let profile = profile("NEEDLE", Band::new(3), false);
    let contract = ImpactContract::assemble(
        "NEEDLE",
        Some(&profile),
        producer(Confidence::Medium, EffortEstimate::Medium, 13),
        at(14),
    );

    let first = score(&proposal, &contract, &ScoringPolicy::default(), at(14));
    let second = score(&proposal, &contract, &ScoringPolicy::default(), at(14));
    assert_eq!(
        first, second,
        "the same inputs must replay to the same score"
    );
}

// ──────────────────────────────────────────────────────────────────────────
// A producer cannot self-promote
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn no_producer_input_can_raise_a_score_above_the_operators_own_value() {
    let proposal = proposal_named("NEEDLE", "a", 13);
    let profile = profile("NEEDLE", Band::new(2), false);
    let ceiling = {
        let contract = ImpactContract::assemble(
            "NEEDLE",
            Some(&profile),
            producer(Confidence::High, EffortEstimate::Small, 13),
            at(14),
        );
        contract.application_value()
    };

    for confidence in [Confidence::Low, Confidence::Medium, Confidence::High] {
        for effort in [
            EffortEstimate::Small,
            EffortEstimate::Medium,
            EffortEstimate::Large,
        ] {
            let contract = ImpactContract::assemble(
                "NEEDLE",
                Some(&profile),
                producer(confidence, effort, 13),
                at(14),
            );
            let scored = score(&proposal, &contract, &ScoringPolicy::default(), at(14));
            assert!(
                scored.score <= ceiling + f64::EPSILON,
                "{confidence:?}/{effort:?} scored {} above the operator ceiling {ceiling}",
                scored.score
            );
        }
    }
}

#[test]
fn higher_confidence_and_lower_effort_move_the_score_in_the_expected_direction() {
    let proposal = proposal_named("NEEDLE", "a", 13);
    let profile = profile("NEEDLE", Band::MAX, false);
    let policy = ScoringPolicy::default();

    let sure_and_small = score(
        &proposal,
        &ImpactContract::assemble(
            "NEEDLE",
            Some(&profile),
            producer(Confidence::High, EffortEstimate::Small, 13),
            at(14),
        ),
        &policy,
        at(14),
    );
    let unsure_and_large = score(
        &proposal,
        &ImpactContract::assemble(
            "NEEDLE",
            Some(&profile),
            producer(Confidence::Low, EffortEstimate::Large, 13),
            at(14),
        ),
        &policy,
        at(14),
    );

    assert!(sure_and_small.score > unsure_and_large.score);
}

// ──────────────────────────────────────────────────────────────────────────
// Stale and defaulted inputs
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn evidence_older_than_the_window_caps_confidence_and_cannot_raise_rank() {
    let policy = ScoringPolicy::default(); // 30 days
    let profile = profile("NEEDLE", Band::MAX, false);

    // Observed on day 1, scored on day 100 of the same scale: stale.
    let stale_proposal = proposal_named("NEEDLE", "a", 1);
    let stale_contract = ImpactContract::assemble(
        "NEEDLE",
        Some(&profile),
        producer(Confidence::High, EffortEstimate::Small, 1),
        at(14),
    );
    let stale = score(
        &stale_proposal,
        &stale_contract,
        &policy,
        Utc.with_ymd_and_hms(2026, 12, 1, 12, 0, 0).unwrap(),
    );

    let fresh_proposal = proposal_named("NEEDLE", "a", 13);
    let fresh_contract = ImpactContract::assemble(
        "NEEDLE",
        Some(&profile),
        producer(Confidence::High, EffortEstimate::Small, 13),
        at(14),
    );
    let fresh = score(&fresh_proposal, &fresh_contract, &policy, at(14));

    assert!(stale.components.evidence_stale, "stale evidence is flagged");
    assert!(
        (stale.components.confidence - Confidence::Low.fraction()).abs() < f64::EPSILON,
        "stale evidence caps confidence at low"
    );
    assert!(
        stale.score < fresh.score,
        "stale evidence cannot rank at or above fresh evidence"
    );
}

#[test]
fn staleness_caps_confidence_but_never_raises_it() {
    let policy = ScoringPolicy::default();
    let profile = profile("NEEDLE", Band::MAX, false);
    let proposal = proposal_named("NEEDLE", "a", 1);
    let contract = ImpactContract::assemble(
        "NEEDLE",
        Some(&profile),
        producer(Confidence::Low, EffortEstimate::Small, 1),
        at(14),
    );

    let scored = score(
        &proposal,
        &contract,
        &policy,
        Utc.with_ymd_and_hms(2026, 12, 1, 12, 0, 0).unwrap(),
    );
    assert!(
        (scored.components.confidence - Confidence::Low.fraction()).abs() < f64::EPSILON,
        "a low-confidence proposal with stale evidence stays low, it is not raised to the cap"
    );
}

#[test]
fn an_expired_profile_is_reported_in_the_score_components() {
    let mut expiring = profile("NEEDLE", Band::MAX, false);
    expiring.review_expiry = Some(at(20));
    let proposal = proposal_named("NEEDLE", "a", 21);
    let contract = ImpactContract::assemble(
        "NEEDLE",
        Some(&expiring),
        producer(Confidence::High, EffortEstimate::Small, 21),
        at(21),
    );

    let scored = score(&proposal, &contract, &ScoringPolicy::default(), at(21));
    assert!(scored.components.profile_expired);
    assert!(
        scored.score < 1.0,
        "a lapsed max profile no longer scores as a max profile"
    );
}

#[test]
fn an_unprofiled_workspace_scores_between_important_and_unimportant_ones() {
    let policy = ScoringPolicy::default();
    let proposal = proposal_named("new-repo", "a", 13);
    let evidence = producer(Confidence::Medium, EffortEstimate::Small, 13);

    let unprofiled = score(
        &proposal,
        &ImpactContract::assemble("new-repo", None, evidence.clone(), at(14)),
        &policy,
        at(14),
    );
    let important = score(
        &proposal,
        &ImpactContract::assemble(
            "new-repo",
            Some(&profile("new-repo", Band::MAX, false)),
            evidence.clone(),
            at(14),
        ),
        &policy,
        at(14),
    );
    let unimportant = score(
        &proposal,
        &ImpactContract::assemble(
            "new-repo",
            Some(&profile("new-repo", Band::MIN, false)),
            evidence,
            at(14),
        ),
        &policy,
        at(14),
    );

    assert!(unprofiled.components.profile_defaulted);
    assert!(unimportant.score < unprofiled.score);
    assert!(unprofiled.score < important.score);
}

// ──────────────────────────────────────────────────────────────────────────
// Ordering: visibility is a tie-break only
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn public_visibility_breaks_a_tie_but_never_outranks_higher_value() {
    let policy = ScoringPolicy::default();
    let evidence = producer(Confidence::Medium, EffortEstimate::Small, 13);

    let private_high = score(
        &proposal_named("high", "a", 13),
        &ImpactContract::assemble(
            "high",
            Some(&profile("high", Band::MAX, false)),
            evidence.clone(),
            at(14),
        ),
        &policy,
        at(14),
    );
    let public_low = score(
        &proposal_named("low", "b", 13),
        &ImpactContract::assemble(
            "low",
            Some(&profile("low", Band::MIN, true)),
            evidence.clone(),
            at(14),
        ),
        &policy,
        at(14),
    );

    let ranked = rank(vec![public_low.clone(), private_high.clone()]);
    assert_eq!(
        ranked[0].signature, private_high.signature,
        "a public low-value proposal must not outrank a private high-value one"
    );

    // Equal value: visibility decides.
    let public_equal = score(
        &proposal_named("equal-public", "c", 13),
        &ImpactContract::assemble(
            "equal-public",
            Some(&profile("equal-public", Band::new(3), true)),
            evidence.clone(),
            at(14),
        ),
        &policy,
        at(14),
    );
    let private_equal = score(
        &proposal_named("equal-private", "d", 13),
        &ImpactContract::assemble(
            "equal-private",
            Some(&profile("equal-private", Band::new(3), false)),
            evidence,
            at(14),
        ),
        &policy,
        at(14),
    );
    assert!(
        (public_equal.score - private_equal.score).abs() < f64::EPSILON,
        "the two are equal on value"
    );

    let ranked = rank(vec![private_equal.clone(), public_equal.clone()]);
    assert_eq!(
        ranked[0].signature, public_equal.signature,
        "at equal value the public one comes first"
    );
}

#[test]
fn ranking_is_a_total_order_even_when_everything_ties() {
    let policy = ScoringPolicy::default();
    let evidence = producer(Confidence::Medium, EffortEstimate::Small, 13);
    let mut scored = Vec::new();
    for name in ["w1", "w2", "w3"] {
        scored.push(score(
            &proposal_named(name, name, 13),
            &ImpactContract::assemble(
                name,
                Some(&profile(name, Band::NEUTRAL, false)),
                evidence.clone(),
                at(14),
            ),
            &policy,
            at(14),
        ));
    }

    let forwards = rank(scored.clone());
    scored.reverse();
    let backwards = rank(scored);

    let forwards: Vec<&str> = forwards.iter().map(|s| s.signature.as_str()).collect();
    let backwards: Vec<&str> = backwards.iter().map(|s| s.signature.as_str()).collect();
    assert_eq!(
        forwards, backwards,
        "equal proposals sort by signature, so input order cannot change the queue"
    );
}
