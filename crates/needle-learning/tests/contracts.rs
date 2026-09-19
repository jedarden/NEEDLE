use needle_learning::{
    evaluate, Attempt, AttemptId, BeadId, ConfirmedState, ContextManifest, Digest, EvidenceBundle,
    EvidenceId, EvidenceRef, FactoryInput, FactoryOutput, FencingEpoch, GateObservation,
    GateStatus, MemoryExposure, Outcome, PolicyIdentity, ProcessObservation, RedactedText,
    RedactionBoundary, RequestedAction, Resolution, ResolutionId, Revision, Sensitivity, SourceId,
    Timestamp, ToolIdentity, CURRENT_SCHEMA_VERSION,
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

// ─── Versioned-record invariants (ADR-024 / transition N-T02) ────────────────
//
// The tests below pin the record-level contract the kernel types must keep
// while lifecycle behavior migrates onto them: stable wire names, complete
// serialization round trips, single-attempt correlation, process exit as
// observation only, and legacy events decoding as non-authoritative.

fn sample_context(attempt_id: &AttemptId) -> ContextManifest {
    ContextManifest {
        schema_version: CURRENT_SCHEMA_VERSION,
        attempt_id: attempt_id.clone(),
        policy: PolicyIdentity {
            source_id: id(SourceId::new),
            digest: id(Digest::new),
            version: "policy-v1".to_owned(),
        },
        tools: vec![ToolIdentity {
            name: "claude-code".to_owned(),
            version: "glm-4.7".to_owned(),
            digest: None,
        }],
        memory: vec![MemoryExposure {
            source_id: id(SourceId::new),
            digest: id(Digest::new),
            sensitivity: Sensitivity::Internal,
            redacted: true,
        }],
        redactions: vec![RedactionBoundary {
            rule: "secrets".to_owned(),
            ruleset_digest: id(Digest::new),
        }],
    }
}

fn sample_evidence(attempt_id: &AttemptId) -> EvidenceBundle {
    EvidenceBundle {
        schema_version: CURRENT_SCHEMA_VERSION,
        attempt_id: attempt_id.clone(),
        process: ProcessObservation {
            exit_code: Some(0),
            duration_ms: 12,
            stdout_digest: Some(id(needle_learning::ContentHash::new)),
            stderr_digest: None,
            interrupted: false,
        },
        gates: vec![GateObservation {
            name: "fmt".to_owned(),
            status: GateStatus::Pass,
            evidence: None,
        }],
        references: vec![EvidenceRef {
            evidence_id: id(EvidenceId::new),
            digest: id(needle_learning::ContentHash::new),
            source: id(SourceId::new),
        }],
        redacted_summary: Some(RedactedText::from_redacted("safe summary")),
    }
}

fn sample_resolution(attempt_id: &AttemptId) -> Resolution {
    Resolution {
        schema_version: CURRENT_SCHEMA_VERSION,
        resolution_id: id(ResolutionId::new),
        attempt_id: attempt_id.clone(),
        outcome: Outcome::VerifiedSuccess,
        requested_action: RequestedAction::None,
        confirmed_state: ConfirmedState::Closed,
        evidence: vec![EvidenceRef {
            evidence_id: id(EvidenceId::new),
            digest: id(needle_learning::ContentHash::new),
            source: id(SourceId::new),
        }],
        resulting_revision: Some(Revision(8)),
    }
}

/// A complete input whose context, evidence, and resolution all carry
/// populated policy/tool/memory and gate/reference fields, so a round trip
/// exercises every ADR-024 field rather than the sparse default fixture.
fn rich_input() -> FactoryInput {
    let mut value = input(None);
    let attempt_id = value.attempt.attempt_id.clone();
    value.context = sample_context(&attempt_id);
    value.evidence = sample_evidence(&attempt_id);
    value.resolution = Some(sample_resolution(&attempt_id));
    value
}

/// Pin the wire name of every variant of a snake_case enum. The mapping is
/// an exhaustive match, so adding a variant fails to compile until a
/// deliberate, stable wire name is chosen (no catch-all arms on outcome
/// enums).
macro_rules! stable_wire_format {
    ($test_name:ident, $enum:ty, {$($variant:path => $wire:literal),+ $(,)?}) => {
        #[test]
        fn $test_name() {
            fn wire_name(value: $enum) -> &'static str {
                match value {
                    $($variant => $wire,)+
                }
            }
            let mut names: Vec<&'static str> = Vec::new();
            $(
                assert_eq!(
                    serde_json::to_string(&$variant).expect("variant serializes"),
                    format!("\"{}\"", wire_name($variant)),
                    concat!(stringify!($variant), " keeps its stable wire name"),
                );
                let decoded: $enum = serde_json::from_str(&format!("\"{}\"", wire_name($variant)))
                    .expect("stable wire name decodes");
                assert_eq!(decoded, $variant);
                names.push(wire_name($variant));
            )+
            let unique: std::collections::HashSet<&str> = names.iter().copied().collect();
            assert_eq!(unique.len(), names.len(), "wire names must stay distinct: {names:?}");
        }
    };
}

stable_wire_format!(outcome_wire_names_are_stable, Outcome, {
    Outcome::VerifiedSuccess => "verified_success",
    Outcome::WorkFailure => "work_failure",
    Outcome::InfrastructureFailure => "infrastructure_failure",
    Outcome::Indeterminate => "indeterminate",
    Outcome::Cancelled => "cancelled",
});

stable_wire_format!(requested_action_wire_names_are_stable, RequestedAction, {
    RequestedAction::None => "none",
    RequestedAction::Release => "release",
    RequestedAction::Quarantine => "quarantine",
    RequestedAction::Reevaluate => "reevaluate",
});

stable_wire_format!(confirmed_state_wire_names_are_stable, ConfirmedState, {
    ConfirmedState::Open => "open",
    ConfirmedState::InProgress => "in_progress",
    ConfirmedState::Closed => "closed",
    ConfirmedState::Unknown => "unknown",
});

stable_wire_format!(gate_status_wire_names_are_stable, GateStatus, {
    GateStatus::Pass => "pass",
    GateStatus::Fail => "fail",
    GateStatus::InfrastructureFailure => "infrastructure_failure",
    GateStatus::NotRun => "not_run",
});

stable_wire_format!(sensitivity_wire_names_are_stable, Sensitivity, {
    Sensitivity::Public => "public",
    Sensitivity::Internal => "internal",
    Sensitivity::Sensitive => "sensitive",
});

stable_wire_format!(evaluation_status_wire_names_are_stable, needle_learning::EvaluationStatus, {
    needle_learning::EvaluationStatus::Verified => "verified",
    needle_learning::EvaluationStatus::Failed => "failed",
    needle_learning::EvaluationStatus::Incomplete => "incomplete",
    needle_learning::EvaluationStatus::Conflicting => "conflicting",
});

stable_wire_format!(evidence_gap_wire_names_are_stable, needle_learning::EvidenceGap, {
    needle_learning::EvidenceGap::MissingResolution => "missing_resolution",
    needle_learning::EvidenceGap::MissingSupportingEvidence => "missing_supporting_evidence",
    needle_learning::EvidenceGap::UnconfirmedState => "unconfirmed_state",
});

stable_wire_format!(legacy_outcome_wire_names_are_stable, needle_learning::LegacyOutcome, {
    needle_learning::LegacyOutcome::Success => "success",
    needle_learning::LegacyOutcome::Failure => "failure",
    needle_learning::LegacyOutcome::Cancelled => "cancelled",
    needle_learning::LegacyOutcome::Unknown => "unknown",
});

#[test]
fn complete_canonical_records_round_trip_through_json() {
    let value = rich_input();
    let encoded = serde_json::to_string(&value).expect("serialize canonical input");
    let decoded: FactoryInput =
        serde_json::from_str(&encoded).expect("deserialize canonical input");
    assert_eq!(decoded, value);

    // Starting revision and fencing survive (ADR-024 attempt fields).
    assert_eq!(decoded.attempt.bead_revision, Revision(7));
    assert_eq!(decoded.attempt.fencing_epoch, FencingEpoch(3));
    // Policy, tool, and memory identities survive.
    assert_eq!(decoded.context.tools.len(), 1);
    assert_eq!(decoded.context.memory[0].sensitivity, Sensitivity::Internal);
    assert_eq!(decoded.context.redactions.len(), 1);
    // Process observation, gate verdicts, and evidence references survive.
    assert_eq!(decoded.evidence.process.exit_code, Some(0));
    assert_eq!(decoded.evidence.gates[0].status, GateStatus::Pass);
    // Semantic outcome, requested action, and confirmed resulting state.
    let resolution = decoded.resolution.as_ref().expect("resolution present");
    assert_eq!(resolution.outcome, Outcome::VerifiedSuccess);
    assert_eq!(resolution.requested_action, RequestedAction::None);
    assert_eq!(resolution.confirmed_state, ConfirmedState::Closed);
    assert_eq!(resolution.resulting_revision, Some(Revision(8)));
}

#[test]
fn kernel_output_round_trips_through_json() {
    let output = evaluate(&rich_input()).expect("complete input evaluates");
    assert_eq!(
        output.evaluation.status,
        needle_learning::EvaluationStatus::Verified
    );
    let encoded = serde_json::to_string(&output).expect("serialize kernel output");
    let decoded: FactoryOutput = serde_json::from_str(&encoded).expect("deserialize kernel output");
    assert_eq!(decoded, output);
    assert!(!decoded.receipt.effect_confirmed);
    assert!(
        decoded.proposals.is_empty(),
        "verified outcomes propose nothing"
    );
}

#[test]
fn retries_of_one_bead_are_distinct_immutable_attempts() {
    let build = |attempt_id: &str, started_at: &str| Attempt {
        schema_version: CURRENT_SCHEMA_VERSION,
        attempt_id: AttemptId::new(attempt_id.to_owned()).expect("attempt identifiers are valid"),
        bead_id: id(BeadId::new),
        bead_revision: Revision(7),
        fencing_epoch: FencingEpoch(3),
        started_at: Timestamp::new(started_at.to_owned()).expect("timestamps are valid"),
    };
    let first = build(
        "0190dce9-7c1a-7cce-98c4-dc0c0c073001",
        "2026-09-19T12:00:00Z",
    );
    let second = build(
        "0190dce9-7c1a-7cce-98c4-dc0c0c073002",
        "2026-09-19T12:05:00Z",
    );
    assert_eq!(first.bead_id, second.bead_id, "both tries are of one bead");
    assert_ne!(
        first.attempt_id, second.attempt_id,
        "each retry carries its own attempt ID"
    );
    assert_ne!(first, second, "retries are distinct records");

    let encoded = serde_json::to_string(&first).expect("serialize attempt");
    let decoded: Attempt = serde_json::from_str(&encoded).expect("deserialize attempt");
    assert_eq!(
        decoded, first,
        "attempt identity is immutable across a round trip"
    );
}

#[test]
fn identifier_validation_rejects_empty_and_framing_characters() {
    assert!(AttemptId::new("").is_err());
    assert!(AttemptId::new("\n".to_owned()).is_err());
    assert!(BeadId::new("\u{0}".to_owned()).is_err());
    let parsed: AttemptId = "0190dce9-7c1a-7cce-98c4-dc0c0c073001"
        .parse()
        .expect("wire-safe identifiers parse");
    assert_eq!(parsed.as_str(), "0190dce9-7c1a-7cce-98c4-dc0c0c073001");
}

#[test]
fn resolution_validation_enforces_version_correlation_and_evidence() {
    let attempt_id = id(AttemptId::new);
    let resolution = sample_resolution(&attempt_id);
    assert_eq!(resolution.validate_for(&attempt_id), Ok(()));

    let mut stale = resolution.clone();
    stale.schema_version = CURRENT_SCHEMA_VERSION + 1;
    assert_eq!(
        stale.validate_for(&attempt_id),
        Err("unsupported resolution schema version")
    );

    let foreign = AttemptId::new("foreign-attempt".to_owned()).expect("foreign ID is valid");
    assert_eq!(
        resolution.validate_for(&foreign),
        Err("resolution belongs to another attempt")
    );

    let mut unclosed = resolution.clone();
    unclosed.confirmed_state = ConfirmedState::InProgress;
    assert_eq!(
        unclosed.validate_for(&attempt_id),
        Err("verified success requires closed state and evidence")
    );

    let mut unverifiable = resolution.clone();
    unverifiable.evidence.clear();
    assert_eq!(
        unverifiable.validate_for(&attempt_id),
        Err("verified success requires closed state and evidence")
    );
}

#[test]
fn context_and_evidence_belong_to_exactly_one_attempt() {
    let attempt_id = id(AttemptId::new);
    let foreign = AttemptId::new("foreign-attempt".to_owned()).expect("foreign ID is valid");
    let context = sample_context(&attempt_id);
    let evidence = sample_evidence(&attempt_id);
    assert_eq!(context.validate_for(&attempt_id), Ok(()));
    assert_eq!(evidence.validate_for(&attempt_id), Ok(()));
    assert_eq!(
        context.validate_for(&foreign),
        Err("context manifest belongs to another attempt")
    );
    assert_eq!(
        evidence.validate_for(&foreign),
        Err("evidence belongs to another attempt")
    );
}

#[test]
fn canonical_records_reject_stale_versions_and_foreign_effects() {
    let mut stale_attempt = input(None).attempt;
    stale_attempt.schema_version = CURRENT_SCHEMA_VERSION + 1;
    assert_eq!(
        stale_attempt.validate(),
        Err("unsupported attempt schema version")
    );

    let mut stale_envelope = input(None);
    stale_envelope.schema_version = CURRENT_SCHEMA_VERSION + 1;
    assert_eq!(
        stale_envelope.validate(),
        Err("unsupported factory input schema version")
    );

    let mut confirmed_effect = evaluate(&input(None)).expect("valid input evaluates");
    confirmed_effect.receipt.effect_confirmed = true;
    assert_eq!(
        confirmed_effect.validate(),
        Err("kernel output cannot confirm an external effect")
    );
}

#[test]
fn exit_zero_with_a_failed_semantic_outcome_is_never_verified() {
    let mut value = input(None);
    let attempt_id = value.attempt.attempt_id.clone();
    let mut resolution = sample_resolution(&attempt_id);
    resolution.outcome = Outcome::WorkFailure;
    resolution.confirmed_state = ConfirmedState::Open;
    resolution.evidence.clear();
    value.resolution = Some(resolution);
    let output = evaluate(&value).expect("a failed resolution is valid input");
    assert_eq!(
        output.evaluation.status,
        needle_learning::EvaluationStatus::Failed
    );
    assert_eq!(output.evaluation.outcome, Some(Outcome::WorkFailure));
    assert_eq!(value.evidence.process.exit_code, Some(0));
}

#[test]
fn verified_success_with_unconfirmed_state_is_rejected_at_the_kernel_boundary() {
    let mut value = input(None);
    let attempt_id = value.attempt.attempt_id.clone();
    let mut resolution = sample_resolution(&attempt_id);
    resolution.confirmed_state = ConfirmedState::Unknown;
    value.resolution = Some(resolution);
    assert_eq!(
        evaluate(&value),
        Err(needle_learning::KernelError::InvalidInput(
            "verified success requires closed state and evidence"
        ))
    );
}

#[test]
fn legacy_fixture_decodes_as_a_non_authoritative_observation() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/legacy_attempt_event.json"
    );
    let raw = std::fs::read_to_string(path).expect("legacy fixture is present");
    let event: needle_learning::LegacyAttemptEvent =
        serde_json::from_str(&raw).expect("a historical attempt.resolved row decodes");
    assert_eq!(event.event_type, "attempt.resolved");
    assert_eq!(event.schema_version, Some(1));
    assert_eq!(
        event.bead_id.as_ref().map(BeadId::as_str),
        Some("needle-dcebc961")
    );
    // The producer's provisional attempt ID is kept as an observation; it is
    // never promoted into an authoritative Attempt.
    assert!(event.attempt_id.is_some());
    // A legacy "success" label is observational, and exit 0 stays a process
    // fact rather than a semantic success decision.
    assert_eq!(event.observed_outcome(), None);
    let observation = event.into_observation();
    assert!(
        !observation.authoritative,
        "legacy rows are never authoritative"
    );
    assert_eq!(observation.exit_code, Some(0));
    assert_eq!(observation.recorded_at, None, "absent fields stay absent");
}
