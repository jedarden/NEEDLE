use needle_learning::{
    evaluate, Attempt, AttemptId, BeadId, ConfirmedState, ContextManifest, Digest, EvidenceBundle,
    EvidenceId, EvidenceRef, FactoryInput, FencingEpoch, Outcome, PolicyIdentity,
    ProcessObservation, RedactedText, Resolution, ResolutionId, Revision, SourceId, Timestamp,
    CURRENT_SCHEMA_VERSION,
};

fn id<T>(constructor: impl FnOnce(String) -> Result<T, needle_learning::InvalidId>) -> T {
    constructor("fixture-id".to_owned()).expect("fixture IDs are valid")
}

fn input(resolution: Option<Resolution>) -> FactoryInput {
    let attempt_id = id(AttemptId::new);
    let bead_id = id(BeadId::new);
    let source_id = id(SourceId::new);
    FactoryInput {
        schema_version: CURRENT_SCHEMA_VERSION,
        attempt: Attempt {
            schema_version: CURRENT_SCHEMA_VERSION,
            attempt_id: attempt_id.clone(),
            bead_id,
            bead_revision: Revision(7),
            fencing_epoch: FencingEpoch(3),
            started_at: id(Timestamp::new),
        },
        context: ContextManifest {
            schema_version: CURRENT_SCHEMA_VERSION,
            attempt_id: attempt_id.clone(),
            policy: PolicyIdentity {
                source_id,
                digest: id(Digest::new),
                version: "policy-v1".to_owned(),
            },
            tools: Vec::new(),
            memory: Vec::new(),
            redactions: Vec::new(),
        },
        evidence: EvidenceBundle {
            schema_version: CURRENT_SCHEMA_VERSION,
            attempt_id: attempt_id.clone(),
            process: ProcessObservation {
                exit_code: Some(0),
                duration_ms: 12,
                stdout_digest: Some(id(needle_learning::ContentHash::new)),
                stderr_digest: None,
                interrupted: false,
            },
            gates: Vec::new(),
            references: vec![EvidenceRef {
                evidence_id: id(EvidenceId::new),
                digest: id(needle_learning::ContentHash::new),
                source: id(SourceId::new),
            }],
            redacted_summary: Some(RedactedText::from_redacted("safe summary")),
        },
        resolution,
    }
}

#[test]
fn verified_success_requires_authoritative_closed_evidence() {
    let attempt_id = id(AttemptId::new);
    let resolution = Resolution {
        schema_version: CURRENT_SCHEMA_VERSION,
        resolution_id: id(ResolutionId::new),
        attempt_id: attempt_id.clone(),
        outcome: Outcome::VerifiedSuccess,
        requested_action: needle_learning::RequestedAction::None,
        confirmed_state: ConfirmedState::Closed,
        evidence: vec![EvidenceRef {
            evidence_id: id(EvidenceId::new),
            digest: id(needle_learning::ContentHash::new),
            source: id(SourceId::new),
        }],
        resulting_revision: Some(Revision(8)),
    };
    let output = evaluate(&input(Some(resolution))).expect("valid input evaluates");
    assert_eq!(
        output.evaluation.status,
        needle_learning::EvaluationStatus::Verified
    );
    assert_eq!(output.evaluation.score_basis_points, 10_000);
    assert!(!output.receipt.effect_confirmed);
}

#[test]
fn exit_zero_without_resolution_is_incomplete_not_success() {
    let output = evaluate(&input(None)).expect("incomplete input is still valid");
    assert_eq!(
        output.evaluation.status,
        needle_learning::EvaluationStatus::Incomplete
    );
    assert_eq!(output.evaluation.outcome, None);
    assert_eq!(output.intents.len(), 1);
}

#[test]
fn canonical_records_round_trip_through_json() {
    let value = input(None);
    let encoded = serde_json::to_string(&value).expect("serialize canonical input");
    let decoded: FactoryInput =
        serde_json::from_str(&encoded).expect("deserialize canonical input");
    assert_eq!(decoded, value);
}

#[test]
fn evaluation_is_deterministic_for_immutable_input() {
    let value = input(None);
    assert_eq!(evaluate(&value).unwrap(), evaluate(&value).unwrap());
}

#[test]
fn legacy_success_is_decoded_as_non_authoritative_observation() {
    let event: needle_learning::LegacyAttemptEvent = serde_json::from_value(serde_json::json!({
        "event_type": "attempt.resolved",
        "bead_id": "legacy-42",
        "outcome": "success",
        "exit_code": 0,
    }))
    .expect("legacy event decodes");
    assert_eq!(event.observed_outcome(), None);
    let observation = event.into_observation();
    assert!(!observation.authoritative);
    assert_eq!(observation.attempt_id, None);
    assert_eq!(
        observation.outcome,
        Some(needle_learning::LegacyOutcome::Success)
    );
}

#[test]
fn redacted_text_serialization_property_is_stable() {
    // A small deterministic property corpus keeps the kernel's MSRV check
    // independent of a test-only generator dependency.
    for text in [
        "",
        "safe summary",
        "a.b-c_123",
        "unicode café",
        "repeated repeated repeated",
    ] {
        let value = RedactedText::from_redacted(text);
        let encoded = serde_json::to_string(&value).unwrap();
        let decoded: RedactedText = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, value);
    }
}

#[test]
fn dependency_direction_is_explicitly_minimal() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("read kernel manifest");
    let dependencies = manifest
        .split_once("[dependencies]")
        .and_then(|(_, rest)| rest.split_once("[dev-dependencies]"))
        .map(|(normal, _)| normal)
        .expect("manifest has dependency sections");
    assert!(dependencies.contains("serde ="));
    for forbidden in [
        "worker",
        "strand",
        "adapter",
        "process-spawning",
        "bead-store",
        "git",
        "deployment",
        "tokio",
        "reqwest",
        "clap",
    ] {
        assert!(
            !dependencies.contains(forbidden),
            "forbidden dependency: {forbidden}"
        );
    }
}

#[derive(Clone)]
struct OneAttemptSource {
    attempt: Attempt,
}

impl needle_learning::ReadOnlySource for OneAttemptSource {
    type Key = AttemptId;
    type Value = Attempt;

    fn read(&self, key: &Self::Key) -> Option<Self::Value> {
        (key == &self.attempt.attempt_id).then(|| self.attempt.clone())
    }
}

#[test]
fn source_contract_is_read_only_and_usable_by_kernel_clients() {
    let attempt = input(None).attempt;
    let source = OneAttemptSource {
        attempt: attempt.clone(),
    };
    let found = needle_learning::ReadOnlySource::read(&source, &attempt.attempt_id);
    assert_eq!(found, Some(attempt));
}
