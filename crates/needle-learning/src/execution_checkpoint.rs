//! Bounded, versioned observations from one claimed execution attempt.
//!
//! This module only validates records. The NEEDLE runtime owns sanitization,
//! live claim verification and durable storage.

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::factory_types::{
    AttemptId, BeadId, ContentHash, EvidenceRef, FencingEpoch, SchemaVersion, Timestamp,
};

/// Maximum encoded checkpoint size, including all evidence references.
pub const MAX_CHECKPOINT_BYTES: usize = 8 * 1024;
/// Maximum number of evidence references attached to one observation.
pub const MAX_CHECKPOINT_EVIDENCE: usize = 16;
/// Current schema version for execution-checkpoint records. Version 1
/// observations remain readable; reducer decisions use version 2 fields.
pub const CURRENT_EXECUTION_CHECKPOINT_VERSION: SchemaVersion = 2;

/// Attribution of recovery material captured before a verified artifact exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAttribution {
    /// The producer established that the material belongs to this attempt.
    Owned,
    /// The shared checkout could not establish exclusive ownership.
    Ambiguous,
}

/// Evidence classes must not be confused by later recovery consumers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum CheckpointEvidence {
    /// A reducer's proposed recovery action and the attempt-resolution record
    /// that justified it. This records a decision only; it does not attest an
    /// intervention or effect.
    RecoveryDecision { reference: EvidenceRef },
    /// Uncommitted material; even owned material is not a verified artifact.
    UncommittedRecovery {
        reference: EvidenceRef,
        attribution: RecoveryAttribution,
    },
    /// Evidence bound to a verified commit or immutable artifact digest.
    VerifiedArtifact {
        reference: EvidenceRef,
        artifact_digest: ContentHash,
    },
}

/// Owner fields retained with a recovery-decision checkpoint. The epoch is
/// stored in [`ExecutionCheckpoint::claim_epoch`] so the full identity remains
/// `(actor, revision, claim_epoch)` without duplicating the fencing token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointOwnership {
    pub actor: String,
    pub revision: Option<u64>,
}

/// Recovery action selected by the existing outcome reducer.
///
/// These values describe proposals. They do not claim the action was applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryDecision {
    Retry,
    Backoff,
    Decomposition,
    Quarantine,
    Handoff,
    /// The reducer selected no recovery action for this attempt.
    NoRecovery,
}

impl RecoveryDecision {
    /// Stable wire spelling for concise checkpoint summaries.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::Backoff => "backoff",
            Self::Decomposition => "decomposition",
            Self::Quarantine => "quarantine",
            Self::Handoff => "handoff",
            Self::NoRecovery => "no_recovery",
        }
    }
}

/// One immutable, concise observation at a meaningful execution boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionCheckpoint {
    pub schema_version: SchemaVersion,
    pub checkpoint_id: String,
    pub bead_id: BeadId,
    pub attempt_id: AttemptId,
    pub claim_epoch: FencingEpoch,
    /// Present on reducer-decision records; absent on earlier checkpoint
    /// kinds that predate ownership identity capture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<CheckpointOwnership>,
    pub timestamp: Timestamp,
    pub intended_result: String,
    pub observable_result: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    pub next_intervention: String,
    /// The outcome reducer's selected proposal, if this is a recovery
    /// decision checkpoint. `NoRecovery` is an explicit recorded result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_decision: Option<RecoveryDecision>,
    #[serde(default)]
    pub evidence_refs: Vec<CheckpointEvidence>,
}

/// A record was refused before it could become durable evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointValidationError {
    UnsupportedVersion(SchemaVersion),
    InvalidField(&'static str),
    Oversized(&'static str),
    Sensitive(&'static str),
}

impl fmt::Display for CheckpointValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported checkpoint schema version {version}")
            }
            Self::InvalidField(field) => write!(f, "invalid checkpoint field {field}"),
            Self::Oversized(field) => write!(f, "oversized checkpoint field {field}"),
            Self::Sensitive(field) => write!(f, "sensitive checkpoint field {field}"),
        }
    }
}

impl std::error::Error for CheckpointValidationError {}

/// Validate a record using a caller-supplied, pure sensitive-text predicate.
///
/// The runtime supplies its configured sanitizer as the predicate. This
/// leaves both record decisions and size limits deterministic and independent
/// of filesystem or backend access.
pub fn validate_checkpoint(
    value: &ExecutionCheckpoint,
    mut is_sensitive: impl FnMut(&str) -> bool,
) -> Result<(), CheckpointValidationError> {
    use CheckpointValidationError as Error;

    if !matches!(
        value.schema_version,
        1 | CURRENT_EXECUTION_CHECKPOINT_VERSION
    ) {
        return Err(Error::UnsupportedVersion(value.schema_version));
    }
    if value.claim_epoch.0 == 0 {
        return Err(Error::InvalidField("claim_epoch"));
    }
    if value.recovery_decision.is_some() && value.ownership.is_none() {
        return Err(Error::InvalidField("ownership"));
    }
    if value.recovery_decision.is_some()
        && value.schema_version != CURRENT_EXECUTION_CHECKPOINT_VERSION
    {
        return Err(Error::InvalidField("schema_version"));
    }
    let mut check = |name: &'static str, text: &str, limit: usize, id: bool| {
        if text.is_empty()
            || text.trim() != text
            || text.chars().any(char::is_control)
            || (id
                && !text
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
        {
            return Err(Error::InvalidField(name));
        }
        if text.len() > limit {
            return Err(Error::Oversized(name));
        }
        if is_sensitive(text) {
            return Err(Error::Sensitive(name));
        }
        Ok(())
    };
    check("checkpoint_id", &value.checkpoint_id, 128, true)?;
    check("bead_id", value.bead_id.as_str(), 128, true)?;
    check("attempt_id", value.attempt_id.as_str(), 128, true)?;
    check("timestamp", value.timestamp.as_str(), 64, false)?;
    if !is_rfc3339(value.timestamp.as_str()) {
        return Err(Error::InvalidField("timestamp"));
    }
    check("intended_result", &value.intended_result, 1024, false)?;
    check("observable_result", &value.observable_result, 1024, false)?;
    if let Some(rationale) = &value.rationale {
        check("rationale", rationale, 512, false)?;
    }
    check("next_intervention", &value.next_intervention, 512, false)?;
    if let Some(ownership) = &value.ownership {
        check("owner_actor", &ownership.actor, 128, true)?;
    }
    if value.evidence_refs.len() > MAX_CHECKPOINT_EVIDENCE {
        return Err(Error::Oversized("evidence_refs"));
    }
    if value.recovery_decision.is_some()
        && value
            .evidence_refs
            .iter()
            .filter(|evidence| matches!(evidence, CheckpointEvidence::RecoveryDecision { .. }))
            .count()
            != 1
    {
        return Err(Error::InvalidField("recovery_decision_evidence"));
    }
    for evidence in &value.evidence_refs {
        if matches!(evidence, CheckpointEvidence::RecoveryDecision { .. })
            != value.recovery_decision.is_some()
        {
            return Err(Error::InvalidField("recovery_decision_evidence"));
        }
        let reference = match evidence {
            CheckpointEvidence::RecoveryDecision { reference }
            | CheckpointEvidence::UncommittedRecovery { reference, .. }
            | CheckpointEvidence::VerifiedArtifact { reference, .. } => reference,
        };
        check("evidence_id", reference.evidence_id.as_str(), 128, true)?;
        check("evidence_digest", reference.digest.as_str(), 128, false)?;
        check("evidence_source", reference.source.as_str(), 512, false)?;
        if let CheckpointEvidence::VerifiedArtifact {
            artifact_digest, ..
        } = evidence
        {
            check("artifact_digest", artifact_digest.as_str(), 128, false)?;
        }
    }
    // serde_json cannot fail for these record types; retain a defensive error
    // path so the validation result remains explicit if fields evolve.
    let encoded = serde_json::to_vec(value).map_err(|_| Error::InvalidField("checkpoint"))?;
    if encoded.len() > MAX_CHECKPOINT_BYTES {
        return Err(Error::Oversized("checkpoint"));
    }
    Ok(())
}

fn is_rfc3339(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return false;
    }
    let number = |start: usize, end: usize| -> Option<u32> {
        let digits = bytes.get(start..end)?;
        digits.iter().all(u8::is_ascii_digit).then(|| {
            digits
                .iter()
                .fold(0, |value, byte| value * 10 + u32::from(byte - b'0'))
        })
    };
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        number(0, 4),
        number(5, 7),
        number(8, 10),
        number(11, 13),
        number(14, 16),
        number(17, 19),
    ) else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    if year == 0 || day == 0 || day > days || hour > 23 || minute > 59 || second > 60 {
        return false;
    }
    let mut offset = 19;
    if bytes.get(offset) == Some(&b'.') {
        offset += 1;
        let fraction_start = offset;
        while bytes.get(offset).is_some_and(u8::is_ascii_digit) {
            offset += 1;
        }
        if offset == fraction_start {
            return false;
        }
    }
    match bytes.get(offset..) {
        Some([b'Z']) => true,
        Some([b'+' | b'-', h1, h2, b':', m1, m2]) => {
            let digits = [*h1, *h2, *m1, *m2];
            digits.iter().all(u8::is_ascii_digit)
                && u32::from(*h1 - b'0') * 10 + u32::from(*h2 - b'0') <= 23
                && u32::from(*m1 - b'0') * 10 + u32::from(*m2 - b'0') <= 59
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factory_types::{EvidenceId, SourceId, CURRENT_SCHEMA_VERSION};

    fn record() -> ExecutionCheckpoint {
        ExecutionCheckpoint {
            schema_version: CURRENT_SCHEMA_VERSION,
            checkpoint_id: "plan-1".into(),
            bead_id: BeadId::new("needle-123").unwrap(),
            attempt_id: AttemptId::new("attempt-123").unwrap(),
            claim_epoch: FencingEpoch(7),
            timestamp: Timestamp::new("2026-10-07T12:00:00Z").unwrap(),
            ownership: None,
            intended_result: "The focused test passes".into(),
            observable_result: "Test exited zero".into(),
            rationale: Some("The behavior is ready for review".into()),
            next_intervention: "Run the broader gate".into(),
            recovery_decision: None,
            evidence_refs: vec![
                CheckpointEvidence::UncommittedRecovery {
                    reference: EvidenceRef {
                        evidence_id: EvidenceId::new("recovery-1").unwrap(),
                        digest: ContentHash::new("sha256:one").unwrap(),
                        source: SourceId::new("trace/recovery.diff").unwrap(),
                    },
                    attribution: RecoveryAttribution::Ambiguous,
                },
                CheckpointEvidence::VerifiedArtifact {
                    reference: EvidenceRef {
                        evidence_id: EvidenceId::new("artifact-1").unwrap(),
                        digest: ContentHash::new("sha256:two").unwrap(),
                        source: SourceId::new("commit/abc").unwrap(),
                    },
                    artifact_digest: ContentHash::new("sha256:abc").unwrap(),
                },
            ],
        }
    }

    fn validate_recovery_decision_owner_and_proposal_evidence() {
        let mut value = record();
        value.schema_version = CURRENT_EXECUTION_CHECKPOINT_VERSION;
        value.recovery_decision = Some(RecoveryDecision::Retry);
        assert_eq!(
            validate_checkpoint(&value, |_| false),
            Err(CheckpointValidationError::InvalidField("ownership"))
        );

        value.ownership = Some(CheckpointOwnership {
            actor: "worker-1".into(),
            revision: Some(12),
        });
        value.evidence_refs = vec![CheckpointEvidence::RecoveryDecision {
            reference: EvidenceRef {
                evidence_id: EvidenceId::new("recovery-attempt-123").unwrap(),
                digest: ContentHash::new("sha256:decision").unwrap(),
                source: SourceId::new("attempt-resolved-attempt-123").unwrap(),
            },
        }];
        validate_checkpoint(&value, |_| false).unwrap();
        let encoded = serde_json::to_vec(&value).unwrap();
        let decoded: ExecutionCheckpoint = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(decoded.recovery_decision, Some(RecoveryDecision::Retry));
    }

    #[test]
    fn accepted_record_round_trips_with_distinct_evidence_classes() {
        let value = record();
        validate_checkpoint(&value, |_| false).unwrap();
        let mut offset_timestamp = value.clone();
        offset_timestamp.timestamp = Timestamp::new("2026-10-07T12:00:00.123+00:00").unwrap();
        validate_checkpoint(&offset_timestamp, |_| false).unwrap();
        let encoded = serde_json::to_vec(&value).unwrap();
        let decoded: ExecutionCheckpoint = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, value);
        assert!(matches!(
            decoded.evidence_refs[0],
            CheckpointEvidence::UncommittedRecovery {
                attribution: RecoveryAttribution::Ambiguous,
                ..
            }
        ));
        assert!(matches!(
            decoded.evidence_refs[1],
            CheckpointEvidence::VerifiedArtifact { .. }
        ));
        validate_recovery_decision_owner_and_proposal_evidence();
    }

    #[test]
    fn malformed_oversized_and_sensitive_records_fail_explicitly() {
        let mut value = record();
        value.schema_version = 99;
        assert_eq!(
            validate_checkpoint(&value, |_| false),
            Err(CheckpointValidationError::UnsupportedVersion(99))
        );
        value = record();
        value.checkpoint_id = "../escape".into();
        assert_eq!(
            validate_checkpoint(&value, |_| false),
            Err(CheckpointValidationError::InvalidField("checkpoint_id"))
        );
        value = record();
        value.timestamp = Timestamp::new("not-a-timestamp").unwrap();
        assert_eq!(
            validate_checkpoint(&value, |_| false),
            Err(CheckpointValidationError::InvalidField("timestamp"))
        );
        value = record();
        value.rationale = Some("x".repeat(513));
        assert_eq!(
            validate_checkpoint(&value, |_| false),
            Err(CheckpointValidationError::Oversized("rationale"))
        );
        value = record();
        value.observable_result = "contains-secret".into();
        assert_eq!(
            validate_checkpoint(&value, |text| text.contains("secret")),
            Err(CheckpointValidationError::Sensitive("observable_result"))
        );
        value = record();
        value.evidence_refs = vec![value.evidence_refs[0].clone(); MAX_CHECKPOINT_EVIDENCE + 1];
        assert_eq!(
            validate_checkpoint(&value, |_| false),
            Err(CheckpointValidationError::Oversized("evidence_refs"))
        );
        let malformed = serde_json::json!({"schema_version":1,"checkpoint_id":"x"});
        assert!(serde_json::from_value::<ExecutionCheckpoint>(malformed).is_err());
    }
}
