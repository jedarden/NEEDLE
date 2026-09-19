//! Focused behavioral contracts for the operator-owned impact contract
//! (N-T07, `needle-9c5ee565`).
//!
//! The property under test is a separation of powers: the operator owns every
//! weight, the producer owns only its own evidence and uncertainty, and no
//! producer input can reach a weight. The rest is what happens when a profile
//! is missing or stale.

use chrono::{TimeZone, Utc};
use needle::learning::improvement::{
    Band, Confidence, EffortEstimate, EvidenceKind, EvidenceRef, ImpactContract, ProducerEvidence,
    WorkspaceImpactProfile, IMPACT_CONTRACT_SCHEMA_VERSION,
};

fn at(day: u32) -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(2026, 9, day, 12, 0, 0).unwrap()
}

/// A date after every profile expiry used here (profiles expire on day 30).
fn after_expiry() -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(2026, 10, 1, 12, 0, 0).unwrap()
}

/// A date far beyond any expiry, for the never-expires case.
fn far_future() -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap()
}

fn producer() -> ProducerEvidence {
    ProducerEvidence::new(
        Confidence::Medium,
        EffortEstimate::Small,
        vec![EvidenceRef::new(EvidenceKind::Attempt, "attempt-a", at(12))],
        "18 beads failed identically three times running",
    )
}

/// An operator profile marking a workspace as important.
fn important_profile() -> WorkspaceImpactProfile {
    WorkspaceImpactProfile {
        schema_version: IMPACT_CONTRACT_SCHEMA_VERSION,
        workspace: "NEEDLE".to_string(),
        objective: "the work factory itself".to_string(),
        affected: vec!["every other workspace".to_string()],
        severity: Band::MAX,
        time_sensitivity: Band::MAX,
        strategic_fit: Band::MAX,
        public_visibility: false,
        review_expiry: Some(at(30)),
    }
}

// ──────────────────────────────────────────────────────────────────────────
// The contract represents everything the acceptance criteria name
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn the_contract_represents_objective_affected_severity_urgency_fit_and_visibility() {
    let profile = important_profile();
    let contract = ImpactContract::assemble("NEEDLE", Some(&profile), producer(), at(14));

    assert_eq!(contract.schema_version, IMPACT_CONTRACT_SCHEMA_VERSION);
    assert_eq!(contract.profile.objective, "the work factory itself");
    assert_eq!(contract.profile.affected, vec!["every other workspace"]);
    assert_eq!(contract.profile.severity, Band::MAX);
    assert_eq!(contract.profile.time_sensitivity, Band::MAX);
    assert_eq!(contract.profile.strategic_fit, Band::MAX);
    assert!(!contract.profile.public_visibility);
    assert_eq!(contract.profile.review_expiry, Some(at(30)));

    // Producer-owned half.
    assert_eq!(contract.producer.confidence, Confidence::Medium);
    assert_eq!(contract.producer.effort, EffortEstimate::Small);
    assert_eq!(contract.producer.evidence.len(), 1);
    assert!(!contract.producer.summary.is_empty());
}

#[test]
fn application_value_is_built_only_from_operator_owned_bands() {
    let profile = important_profile();
    let top = ImpactContract::assemble("NEEDLE", Some(&profile), producer(), at(14));
    assert!(
        (top.application_value() - 1.0).abs() < f64::EPSILON,
        "all-max bands are full value, got {}",
        top.application_value()
    );

    let mut unimportant = important_profile();
    unimportant.severity = Band::MIN;
    unimportant.time_sensitivity = Band::MIN;
    unimportant.strategic_fit = Band::MIN;
    let bottom = ImpactContract::assemble("NEEDLE", Some(&unimportant), producer(), at(14));
    assert!(
        bottom.application_value().abs() < f64::EPSILON,
        "all-min bands are zero value"
    );
}

#[test]
fn public_visibility_is_not_a_component_of_application_value() {
    let mut private = important_profile();
    private.public_visibility = false;
    let mut public = important_profile();
    public.public_visibility = true;

    let private_value =
        ImpactContract::assemble("NEEDLE", Some(&private), producer(), at(14)).application_value();
    let public_value =
        ImpactContract::assemble("NEEDLE", Some(&public), producer(), at(14)).application_value();

    assert_eq!(
        private_value, public_value,
        "visibility is a tie-break, never a substitute for application value"
    );
}

// ──────────────────────────────────────────────────────────────────────────
// A producer cannot raise an operator-owned weight
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn producer_evidence_carries_no_weight_field_and_cannot_change_a_band() {
    let profile = important_profile();

    // The strongest thing a producer can claim about itself.
    let boastful = ProducerEvidence::new(
        Confidence::High,
        EffortEstimate::Small,
        vec![EvidenceRef::new(EvidenceKind::Attempt, "a", at(12))],
        "this is extremely important and urgent and strategic",
    );
    let modest = ProducerEvidence::new(
        Confidence::Low,
        EffortEstimate::Large,
        vec![EvidenceRef::new(EvidenceKind::Attempt, "a", at(12))],
        "minor",
    );

    let loud = ImpactContract::assemble("NEEDLE", Some(&profile), boastful, at(14));
    let quiet = ImpactContract::assemble("NEEDLE", Some(&profile), modest, at(14));

    assert_eq!(
        loud.profile, quiet.profile,
        "whatever a producer says about itself, the operator's profile is unchanged"
    );
    assert_eq!(
        loud.application_value(),
        quiet.application_value(),
        "application value is operator-owned and identical for both"
    );
}

#[test]
fn bands_are_bounded_so_an_out_of_range_weight_cannot_be_expressed() {
    assert_eq!(Band::new(200).get(), 4, "a band clamps into 0..=4");
    assert_eq!(Band::new(4), Band::MAX);
    assert_eq!(Band::new(0), Band::MIN);
    assert_eq!(Band::default(), Band::NEUTRAL, "the default is mid-band");
    assert!((Band::MAX.fraction() - 1.0).abs() < f64::EPSILON);
    assert!(Band::MIN.fraction().abs() < f64::EPSILON);
}

// ──────────────────────────────────────────────────────────────────────────
// Unprofiled work is defaulted, not starved; expiry degrades to neutral
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn unprofiled_work_receives_a_neutral_bounded_default_rather_than_starvation() {
    let contract = ImpactContract::assemble("brand-new-repo", None, producer(), at(14));

    assert!(
        contract.profile_defaulted,
        "the default is reported as such"
    );
    assert!(!contract.profile_expired);
    assert_eq!(contract.profile.severity, Band::NEUTRAL);
    assert_eq!(contract.profile.time_sensitivity, Band::NEUTRAL);
    assert_eq!(contract.profile.strategic_fit, Band::NEUTRAL);

    let value = contract.application_value();
    assert!(
        value > 0.0,
        "unprofiled work must not score zero, or it is never worked: got {value}"
    );

    let mut important = important_profile();
    important.workspace = "brand-new-repo".to_string();
    let profiled = ImpactContract::assemble("brand-new-repo", Some(&important), producer(), at(14));
    assert!(
        value < profiled.application_value(),
        "unprofiled ranks below deliberately-important work"
    );

    let mut unimportant = important_profile();
    unimportant.severity = Band::MIN;
    unimportant.time_sensitivity = Band::MIN;
    unimportant.strategic_fit = Band::MIN;
    let deprioritized =
        ImpactContract::assemble("brand-new-repo", Some(&unimportant), producer(), at(14));
    assert!(
        value > deprioritized.application_value(),
        "unprofiled ranks above deliberately-unimportant work"
    );
}

#[test]
fn an_expired_profile_degrades_to_neutral_rather_than_keeping_its_bands() {
    let profile = important_profile(); // expires at day 30
    let contract = ImpactContract::assemble("NEEDLE", Some(&profile), producer(), after_expiry());

    assert!(contract.profile_expired, "the lapse is reported");
    assert!(!contract.profile_defaulted, "a profile did exist");
    assert_eq!(
        contract.profile.severity,
        Band::NEUTRAL,
        "a lapsed high band stops promoting work"
    );
    assert_eq!(
        contract.profile.objective, "the work factory itself",
        "the objective survives expiry: it is what an operator re-profiles from"
    );
}

#[test]
fn an_expired_low_profile_also_degrades_up_to_neutral() {
    // Expiry moves towards neutral in both directions — a lapsed judgement
    // must not keep suppressing a workspace either.
    let mut suppressed = important_profile();
    suppressed.severity = Band::MIN;
    suppressed.time_sensitivity = Band::MIN;
    suppressed.strategic_fit = Band::MIN;

    let live = ImpactContract::assemble("NEEDLE", Some(&suppressed), producer(), at(14));
    let lapsed = ImpactContract::assemble("NEEDLE", Some(&suppressed), producer(), after_expiry());

    assert!(
        lapsed.application_value() > live.application_value(),
        "a lapsed low band stops suppressing work"
    );
    assert_eq!(lapsed.profile.severity, Band::NEUTRAL);
}

#[test]
fn a_profile_without_an_expiry_never_lapses() {
    let mut permanent = important_profile();
    permanent.review_expiry = None;

    let contract = ImpactContract::assemble("NEEDLE", Some(&permanent), producer(), far_future());
    assert!(!contract.profile_expired);
    assert_eq!(contract.profile.severity, Band::MAX);
}

// ──────────────────────────────────────────────────────────────────────────
// Versioning and round-trip
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn a_profile_round_trips_and_defaults_its_schema_version() {
    let json = r#"{
        "workspace": "NEEDLE",
        "objective": "the work factory",
        "severity": 4,
        "time_sensitivity": 3,
        "strategic_fit": 4,
        "public_visibility": true
    }"#;
    let profile: WorkspaceImpactProfile = serde_json::from_str(json).expect("parses");

    assert_eq!(
        profile.schema_version, IMPACT_CONTRACT_SCHEMA_VERSION,
        "an operator writing a profile need not restate the schema version"
    );
    assert_eq!(profile.severity, Band::MAX);
    assert_eq!(profile.time_sensitivity, Band::new(3));
    assert!(profile.public_visibility);
    assert_eq!(profile.review_expiry, None);

    let encoded = serde_json::to_string(&profile).expect("serializes");
    let decoded: WorkspaceImpactProfile = serde_json::from_str(&encoded).expect("round-trips");
    assert_eq!(decoded, profile);
}

#[test]
fn producer_summary_text_is_bounded() {
    let evidence = ProducerEvidence::new(
        Confidence::Low,
        EffortEstimate::Small,
        vec![EvidenceRef::new(EvidenceKind::Attempt, "a", at(12))],
        "y".repeat(10_000),
    );
    assert!(
        evidence.summary.len() <= 300,
        "producer prose is bounded, was {}",
        evidence.summary.len()
    );
}

#[test]
fn confidence_and_effort_are_ordered_and_bounded() {
    assert!(Confidence::Low < Confidence::Medium);
    assert!(Confidence::Medium < Confidence::High);
    assert!(Confidence::High.fraction() <= 1.0);
    assert!(Confidence::Low.fraction() > 0.0);

    assert!(EffortEstimate::Small < EffortEstimate::Large);
    assert!(
        EffortEstimate::Small.divisor() >= 1.0,
        "no effort estimate may multiply value above the operator's ceiling"
    );
    assert!(EffortEstimate::Large.divisor() > EffortEstimate::Small.divisor());
}
