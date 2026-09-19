//! Deterministic pure transformations over factory records.

use crate::factory_types::{
    ConfirmedState, Digest, Evaluation, EvaluationStatus, EvidenceGap, FactoryInput, FactoryOutput,
    Intent, LearningProposal, Outcome, Receipt, SourceId, CURRENT_SCHEMA_VERSION,
};

/// Validation or correlation failure at the kernel boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelError {
    /// One or more records violated a schema or correlation invariant.
    InvalidInput(&'static str),
}

impl core::fmt::Display for KernelError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidInput(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for KernelError {}

/// Evaluate one immutable factory input.
///
/// The function has no clock, randomness, I/O, global state, or mutable
/// source. The returned intents and proposals are descriptions for controllers
/// and are not executed by this crate.
pub fn evaluate(input: &FactoryInput) -> Result<FactoryOutput, KernelError> {
    input.validate().map_err(KernelError::InvalidInput)?;

    let (evaluation, intents, proposals) = match input.resolution.as_ref() {
        None => (
            Evaluation {
                attempt_id: input.attempt.attempt_id.clone(),
                outcome: None,
                status: EvaluationStatus::Incomplete,
                score_basis_points: 0,
            },
            vec![Intent::RequestEvidence {
                reason: EvidenceGap::MissingResolution,
            }],
            Vec::new(),
        ),
        Some(resolution) if resolution.confirmed_state == ConfirmedState::Unknown => (
            Evaluation {
                attempt_id: input.attempt.attempt_id.clone(),
                outcome: Some(resolution.outcome),
                status: EvaluationStatus::Incomplete,
                score_basis_points: 0,
            },
            vec![Intent::RequestEvidence {
                reason: EvidenceGap::UnconfirmedState,
            }],
            Vec::new(),
        ),
        Some(resolution) if resolution.outcome == Outcome::VerifiedSuccess => (
            Evaluation {
                attempt_id: input.attempt.attempt_id.clone(),
                outcome: Some(resolution.outcome),
                status: EvaluationStatus::Verified,
                score_basis_points: 10_000,
            },
            Vec::new(),
            Vec::new(),
        ),
        Some(resolution) => {
            let proposal_id = SourceId::new(format!(
                "proposal:{}:{}",
                input.attempt.attempt_id,
                resolution.outcome_name()
            ))
            .map_err(|_| KernelError::InvalidInput("proposal identity could not be formed"))?;
            (
                Evaluation {
                    attempt_id: input.attempt.attempt_id.clone(),
                    outcome: Some(resolution.outcome),
                    status: EvaluationStatus::Failed,
                    score_basis_points: 0,
                },
                vec![Intent::SubmitForReview {
                    proposal_id: proposal_id.clone(),
                }],
                vec![LearningProposal {
                    proposal_id,
                    attempt_id: input.attempt.attempt_id.clone(),
                    outcome: resolution.outcome,
                    evidence: resolution.evidence.clone(),
                    review_required: true,
                }],
            )
        }
    };

    let input_digest = Digest::new(format!("fnv1a:{:016x}", input_fingerprint(input)))
        .map_err(|_| KernelError::InvalidInput("evaluation digest could not be formed"))?;
    let receipt_id = SourceId::new(format!(
        "receipt:{}:{}",
        input.attempt.attempt_id,
        evaluation.status_name()
    ))
    .map_err(|_| KernelError::InvalidInput("receipt identity could not be formed"))?;
    let output = FactoryOutput {
        schema_version: CURRENT_SCHEMA_VERSION,
        attempt_id: input.attempt.attempt_id.clone(),
        evaluation,
        intents,
        proposals,
        receipt: Receipt {
            receipt_id,
            attempt_id: input.attempt.attempt_id.clone(),
            input_digest,
            effect_confirmed: false,
        },
    };
    output.validate().map_err(KernelError::InvalidInput)?;
    Ok(output)
}

trait OutcomeName {
    fn outcome_name(&self) -> &'static str;
}

fn input_fingerprint(input: &FactoryInput) -> u64 {
    let mut hash = 14_695_981_039_346_656_037_u64;
    let duration = input.evidence.process.duration_ms.to_string();
    let fields = [
        input.attempt.attempt_id.as_str(),
        input.attempt.bead_id.as_str(),
        input.attempt.started_at.as_str(),
        input.context.policy.source_id.as_str(),
        input.context.policy.digest.as_str(),
        input.context.policy.version.as_str(),
        duration.as_str(),
    ];
    for field in fields {
        for byte in field.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(1_099_511_628_211);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    hash
}

impl OutcomeName for crate::factory_types::Resolution {
    fn outcome_name(&self) -> &'static str {
        match self.outcome {
            Outcome::VerifiedSuccess => "verified_success",
            Outcome::WorkFailure => "work_failure",
            Outcome::InfrastructureFailure => "infrastructure_failure",
            Outcome::Indeterminate => "indeterminate",
            Outcome::Cancelled => "cancelled",
        }
    }
}

trait EvaluationName {
    fn status_name(&self) -> &'static str;
}

impl EvaluationName for Evaluation {
    fn status_name(&self) -> &'static str {
        match self.status {
            EvaluationStatus::Verified => "verified",
            EvaluationStatus::Failed => "failed",
            EvaluationStatus::Incomplete => "incomplete",
            EvaluationStatus::Conflicting => "conflicting",
        }
    }
}
