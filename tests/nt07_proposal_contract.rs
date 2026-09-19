//! Focused behavioral contracts for the N-T07 typed proposal envelope and its
//! immutable producer evidence contract (`needle-2b0a309b`, ADR-029).
//!
//! These are pure record contracts: no process, no filesystem, no clock. The
//! properties under test are the ones the rest of the improvement loop relies
//! on — a signature that is derived rather than declared, an acceptance
//! measure that is always computable, and an authority claim a producer cannot
//! inflate.

use chrono::{TimeZone, Utc};
use needle::learning::improvement::{
    AcceptanceMeasure, AuthorityLevel, Direction, EvidenceClass, EvidenceKind, EvidenceRef,
    ImpactMeasure, ImprovementProposal, ProposalRejection, ProposalScope, Rollback,
    IMPROVEMENT_PROPOSAL_SCHEMA_VERSION, PROPOSAL_REF_NAMESPACE,
};

fn at(day: u32) -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(2026, 9, day, 12, 0, 0).unwrap()
}

fn acceptance() -> AcceptanceMeasure {
    AcceptanceMeasure {
        measure: ImpactMeasure::VerifiedYieldPerAttempt,
        direction: Direction::Increase,
        min_delta: 0.05,
        horizon_days: 7,
    }
}

fn rollback() -> Rollback {
    Rollback {
        description: "revert the gate default for this workspace".to_string(),
        automatic: true,
    }
}

/// A proposal built from the class's own evidence, at the authority that class
/// allows.
fn proposal_with(
    class: EvidenceClass,
    evidence: Vec<EvidenceRef>,
    scope: ProposalScope,
) -> Result<ImprovementProposal, ProposalRejection> {
    ImprovementProposal::new(
        class,
        evidence,
        scope,
        "raise the workspace default gate to build+test",
        class.max_authority(),
        "verified-closure yield per attempt rises in the named workspace",
        acceptance(),
        rollback(),
        at(14),
        1,
    )
}

fn one_workspace() -> ProposalScope {
    ProposalScope::new(["NEEDLE".to_string()], [])
}

fn two_attempts() -> Vec<EvidenceRef> {
    vec![
        EvidenceRef::new(EvidenceKind::Attempt, "attempt-a", at(12)),
        EvidenceRef::new(EvidenceKind::Attempt, "attempt-b", at(13)),
    ]
}

// ──────────────────────────────────────────────────────────────────────────
// The signature is derived from evidence, never declared by the producer
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn a_valid_proposal_carries_the_current_schema_version_and_a_derived_signature() {
    let proposal = proposal_with(
        EvidenceClass::RedBaselineWorkspace,
        two_attempts(),
        one_workspace(),
    )
    .expect("a complete proposal is admissible");

    assert_eq!(
        proposal.schema_version, IMPROVEMENT_PROPOSAL_SCHEMA_VERSION,
        "a proposal records the schema version it was built under"
    );
    assert_eq!(
        proposal.signature.len(),
        16,
        "the signature is a fixed-width deduplication key"
    );
    assert!(
        proposal.signature.chars().all(|c| c.is_ascii_hexdigit()),
        "the signature is hex: {}",
        proposal.signature
    );
    assert_eq!(
        proposal.unique_ref(),
        format!("{PROPOSAL_REF_NAMESPACE}:{}", proposal.signature),
        "the bead-rs unique ref carries the signature in the proposal namespace"
    );
}

#[test]
fn the_same_evidence_in_a_different_order_derives_the_same_signature() {
    let forwards = proposal_with(
        EvidenceClass::RedBaselineWorkspace,
        two_attempts(),
        one_workspace(),
    )
    .expect("valid");

    let mut reversed = two_attempts();
    reversed.reverse();
    let backwards = proposal_with(
        EvidenceClass::RedBaselineWorkspace,
        reversed,
        one_workspace(),
    )
    .expect("valid");

    assert_eq!(
        forwards.signature, backwards.signature,
        "two generators that saw the same rows in a different order must not \
         file two proposals about them"
    );
}

#[test]
fn duplicate_evidence_references_collapse_before_the_signature_is_derived() {
    let mut duplicated = two_attempts();
    duplicated.extend(two_attempts());

    let proposal = proposal_with(
        EvidenceClass::RedBaselineWorkspace,
        duplicated,
        one_workspace(),
    )
    .expect("valid");

    assert_eq!(
        proposal.evidence.len(),
        2,
        "repeated references to one attempt are one piece of evidence"
    );
    assert_eq!(
        proposal.signature,
        proposal_with(
            EvidenceClass::RedBaselineWorkspace,
            two_attempts(),
            one_workspace()
        )
        .expect("valid")
        .signature,
        "collapsing duplicates must not change the identity"
    );
}

#[test]
fn prose_is_outside_the_signature_so_rewording_does_not_refile() {
    let original = proposal_with(
        EvidenceClass::RedBaselineWorkspace,
        two_attempts(),
        one_workspace(),
    )
    .expect("valid");

    let reworded = ImprovementProposal::new(
        EvidenceClass::RedBaselineWorkspace,
        two_attempts(),
        one_workspace(),
        "COMPLETELY different wording for the same change",
        AuthorityLevel::L4,
        "a different benefit sentence entirely",
        AcceptanceMeasure {
            measure: ImpactMeasure::VerifiedYieldPerDollar,
            direction: Direction::Increase,
            min_delta: 0.9,
            horizon_days: 30,
        },
        Rollback {
            description: "different rollback prose".to_string(),
            automatic: false,
        },
        at(20),
        7,
    )
    .expect("valid");

    assert_eq!(
        original.signature, reworded.signature,
        "identity is the evidence and the scope, so re-running the generator \
         after a wording change must not file a second bead"
    );
}

#[test]
fn a_different_scope_or_class_is_a_different_proposal() {
    let base = proposal_with(
        EvidenceClass::RedBaselineWorkspace,
        two_attempts(),
        one_workspace(),
    )
    .expect("valid");

    let other_workspace = proposal_with(
        EvidenceClass::RedBaselineWorkspace,
        two_attempts(),
        ProposalScope::new(["commitgraph".to_string()], []),
    )
    .expect("valid");
    assert_ne!(
        base.signature, other_workspace.signature,
        "the same evidence class about two workspaces is two proposals"
    );

    let other_class = proposal_with(
        EvidenceClass::RepeatedIdenticalFailures,
        two_attempts(),
        one_workspace(),
    )
    .expect("valid");
    assert_ne!(
        base.signature, other_class.signature,
        "two ledger queries over the same rows are two proposals"
    );
}

#[test]
fn scope_normalizes_so_ordering_and_duplication_cannot_split_an_identity() {
    let scope = ProposalScope::new(
        ["b".to_string(), "a".to_string(), "b".to_string()],
        ["z".to_string(), "z".to_string()],
    );
    assert_eq!(scope.workspaces, vec!["a".to_string(), "b".to_string()]);
    assert_eq!(scope.adapters, vec!["z".to_string()]);
}

// ──────────────────────────────────────────────────────────────────────────
// Construction refuses anything that could not be decided later
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn a_proposal_without_evidence_is_refused() {
    assert_eq!(
        proposal_with(
            EvidenceClass::RedBaselineWorkspace,
            Vec::new(),
            one_workspace()
        ),
        Err(ProposalRejection::NoEvidence),
        "an evidence-addressed loop cannot accept an unaddressed proposal"
    );
}

#[test]
fn a_proposal_with_an_empty_scope_is_refused() {
    assert_eq!(
        proposal_with(
            EvidenceClass::RedBaselineWorkspace,
            two_attempts(),
            ProposalScope::default()
        ),
        Err(ProposalRejection::EmptyScope),
        "a proposal with no cohort has nothing for a receipt to measure"
    );
}

#[test]
fn a_proposal_with_no_intended_change_is_refused() {
    let rejection = ImprovementProposal::new(
        EvidenceClass::RedBaselineWorkspace,
        two_attempts(),
        one_workspace(),
        "   ",
        AuthorityLevel::L4,
        "benefit",
        acceptance(),
        rollback(),
        at(14),
        1,
    );
    assert_eq!(rejection, Err(ProposalRejection::NoIntendedChange));
}

#[test]
fn an_acceptance_threshold_that_could_never_decide_is_refused() {
    for bad in [0.0, -0.1, f64::NAN, f64::INFINITY] {
        let rejection = ImprovementProposal::new(
            EvidenceClass::RedBaselineWorkspace,
            two_attempts(),
            one_workspace(),
            "change",
            AuthorityLevel::L4,
            "benefit",
            AcceptanceMeasure {
                measure: ImpactMeasure::VerifiedYieldPerAttempt,
                direction: Direction::Increase,
                min_delta: bad,
                horizon_days: 7,
            },
            rollback(),
            at(14),
            1,
        );
        assert!(
            matches!(rejection, Err(ProposalRejection::UnusableThreshold { .. })),
            "min_delta {bad} must be refused at construction, got {rejection:?}"
        );
    }
}

#[test]
fn a_zero_day_horizon_is_refused() {
    let rejection = ImprovementProposal::new(
        EvidenceClass::RedBaselineWorkspace,
        two_attempts(),
        one_workspace(),
        "change",
        AuthorityLevel::L4,
        "benefit",
        AcceptanceMeasure {
            measure: ImpactMeasure::VerifiedYieldPerAttempt,
            direction: Direction::Increase,
            min_delta: 0.05,
            horizon_days: 0,
        },
        rollback(),
        at(14),
        1,
    );
    assert_eq!(rejection, Err(ProposalRejection::ZeroHorizon));
}

#[test]
fn free_text_fields_are_bounded_at_construction() {
    let huge = "x".repeat(10_000);
    let proposal = ImprovementProposal::new(
        EvidenceClass::RedBaselineWorkspace,
        two_attempts(),
        one_workspace(),
        huge.clone(),
        AuthorityLevel::L4,
        huge,
        acceptance(),
        rollback(),
        at(14),
        1,
    )
    .expect("valid");

    assert!(
        proposal.intended_change.len() <= 600,
        "intended_change is bounded, was {}",
        proposal.intended_change.len()
    );
    assert!(
        proposal.expected_benefit.len() <= 600,
        "expected_benefit is bounded, was {}",
        proposal.expected_benefit.len()
    );
}

// ──────────────────────────────────────────────────────────────────────────
// A producer cannot inflate its own authority
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn a_proposal_claiming_more_authority_than_its_evidence_class_allows_is_refused() {
    // Canary evidence can move a canary's own knob (L2). It can never
    // authorize deploying code (L5), however many canaries ran.
    let rejection = ImprovementProposal::new(
        EvidenceClass::CanaryResult,
        vec![EvidenceRef::new(EvidenceKind::Canary, "exp-1", at(12))],
        one_workspace(),
        "deploy the new prompt fleet-wide",
        AuthorityLevel::L5,
        "benefit",
        acceptance(),
        rollback(),
        at(14),
        1,
    );
    assert_eq!(
        rejection,
        Err(ProposalRejection::AuthorityExceedsEvidence {
            claimed: AuthorityLevel::L5,
            allowed: AuthorityLevel::L2,
        }),
        "an experiment cannot promote itself (plan section 5.7)"
    );
}

#[test]
fn every_evidence_class_declares_an_authority_ceiling_at_or_below_l4() {
    for class in EvidenceClass::ALL {
        let ceiling = class.max_authority();
        assert!(
            ceiling <= AuthorityLevel::L4,
            "{class} may not justify {ceiling}: L5 is reachable only through \
             a separately authorized release (plan section 5.7)"
        );
        // The ceiling must itself be constructible, or the class is dead.
        let evidence = vec![EvidenceRef::new(EvidenceKind::Workspace, "NEEDLE", at(12))];
        assert!(
            proposal_with(class, evidence, one_workspace()).is_ok(),
            "{class} must be able to produce a proposal at its own ceiling"
        );
    }
}

#[test]
fn controller_applied_levels_are_exactly_l1_through_l3() {
    assert!(!AuthorityLevel::L0.applied_by_controller());
    assert!(AuthorityLevel::L1.applied_by_controller());
    assert!(AuthorityLevel::L2.applied_by_controller());
    assert!(AuthorityLevel::L3.applied_by_controller());
    assert!(
        !AuthorityLevel::L4.applied_by_controller(),
        "an L4 change becomes an implementation bead, not a controller flip"
    );
    assert!(!AuthorityLevel::L5.applied_by_controller());
}

// ──────────────────────────────────────────────────────────────────────────
// Wire vocabulary and round-trip
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn the_acceptance_measure_vocabulary_admits_no_uncomputable_measure() {
    // Every measure is a section 10 quantity computable from ledger rows.
    // This test exists so that adding a free-text or "other" variant fails
    // here rather than silently producing undecidable proposals.
    let wire: Vec<&str> = ImpactMeasure::ALL.iter().map(|m| m.as_str()).collect();
    assert_eq!(
        wire,
        vec![
            "verified_yield_per_attempt",
            "verified_yield_per_dollar",
            "fingerprint_recurrence",
            "false_close_rate",
            "infrastructure_share",
        ]
    );
}

#[test]
fn the_evidence_class_vocabulary_matches_the_plan() {
    let wire: Vec<&str> = EvidenceClass::ALL.iter().map(|c| c.as_str()).collect();
    assert_eq!(
        wire,
        vec![
            "repeated_identical_failures",
            "workspace_adapter_regret",
            "unverified_spend_concentration",
            "red_baseline_workspace",
            "recurring_fingerprint_with_known_fix",
            "canary_result",
        ],
        "the recognised classes are fixed by plan section 4.10 step 2"
    );
}

#[test]
fn a_proposal_round_trips_through_json_unchanged() {
    let proposal = proposal_with(
        EvidenceClass::UnverifiedSpendConcentration,
        vec![
            EvidenceRef::new(EvidenceKind::Adapter, "claude-code-glm-5.3", at(12)),
            EvidenceRef::new(EvidenceKind::Workspace, "NEEDLE", at(12)),
        ],
        ProposalScope::new(["NEEDLE".to_string()], ["claude-code-glm-5.3".to_string()]),
    )
    .expect("valid");

    let encoded = serde_json::to_string(&proposal).expect("serializes");
    let decoded: ImprovementProposal = serde_json::from_str(&encoded).expect("deserializes");

    assert_eq!(
        decoded, proposal,
        "a receipt read back after a restart must be the proposal that was written"
    );
    assert!(
        encoded.contains("\"unverified_spend_concentration\""),
        "the class is stored in its wire form: {encoded}"
    );
}

#[test]
fn direction_decides_against_the_declared_threshold() {
    let up = Direction::Increase;
    assert!(
        up.satisfied(0.40, 0.46, 0.05),
        "a 6pt rise clears a 5pt bar"
    );
    assert!(
        !up.satisfied(0.40, 0.44, 0.05),
        "a 4pt rise does not clear a 5pt bar"
    );

    let down = Direction::Decrease;
    assert!(
        down.satisfied(0.30, 0.20, 0.05),
        "recurrence falling 10pt clears a 5pt bar"
    );
    assert!(
        !down.satisfied(0.30, 0.42, 0.05),
        "a measure moving the wrong way never satisfies"
    );
}

#[test]
fn evidence_ids_are_readable_by_kind() {
    let proposal = proposal_with(
        EvidenceClass::WorkspaceAdapterRegret,
        vec![
            EvidenceRef::new(EvidenceKind::Adapter, "adapter-b", at(12)),
            EvidenceRef::new(EvidenceKind::Adapter, "adapter-a", at(12)),
            EvidenceRef::new(EvidenceKind::Workspace, "NEEDLE", at(12)),
        ],
        one_workspace(),
    )
    .expect("valid");

    assert_eq!(
        proposal.evidence_ids(EvidenceKind::Adapter),
        vec!["adapter-a", "adapter-b"],
        "evidence is readable by kind, in the stable signature order"
    );
    assert_eq!(
        proposal.evidence_ids(EvidenceKind::Workspace),
        vec!["NEEDLE"]
    );
    assert!(proposal.evidence_ids(EvidenceKind::Bead).is_empty());
}
