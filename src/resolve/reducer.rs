//! Pure, fail-closed reduction of one finished dispatch into a lifecycle proposal.
//!
//! A process exit is an observation, never proof that a bead was delivered.
//! The caller obtains authoritative state and evidence before calling this
//! function; no store, clock, or filesystem operation occurs here.

use crate::resolve::evidence::EvidenceBundle;
use crate::resolve::ResolveDecision;
use crate::types::{Bead, BeadStatus, ClaimStatus, Outcome};
use crate::validation::resolution::GateVerdict;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionProposal {
    VerifyThenComplete,
    Complete,
    Release,
    Quarantine,
    Block,
    Split,
    AdmissionFailure,
    OwnershipLost,
}

/// Policy fixed for this attempt before reduction. A verdict cannot relax it.
#[derive(Debug, Clone, Copy, Default)]
pub struct ResolutionPolicy {
    pub failure_ceiling_reached: bool,
    pub allow_unshipped_completion: bool,
}

/// All inputs are snapshots from the same dispatch. `work_accepted` is the
/// shipped-work verifier's result, rather than an inference from an exit code.
pub struct ResolutionFacts<'a> {
    pub bead: &'a Bead,
    pub claim: &'a ClaimStatus,
    pub actor: &'a str,
    pub expected_revision: Option<u64>,
    pub expected_epoch: Option<u64>,
    pub evidence: Option<&'a EvidenceBundle>,
    pub gates: GateVerdict,
    pub work_accepted: bool,
    pub decision: Option<&'a ResolveDecision>,
    pub exit_code: i32,
    pub interrupted: bool,
    pub policy: ResolutionPolicy,
}

pub fn reduce(facts: &ResolutionFacts<'_>) -> ResolutionProposal {
    if facts.claim.status != BeadStatus::InProgress
        || facts.claim.assignee.as_deref() != Some(facts.actor)
        || facts.bead.status != BeadStatus::InProgress
        || facts.bead.assignee.as_deref() != Some(facts.actor)
        || facts
            .expected_revision
            .is_some_and(|revision| facts.claim.revision != Some(revision))
        || facts
            .expected_epoch
            .is_some_and(|epoch| facts.claim.claim_epoch != Some(epoch))
    {
        return ResolutionProposal::OwnershipLost;
    }

    // Use the process classifier rather than treating only negative exits as
    // crashes. Timeout (124), missing-agent (127), and signal-style positive
    // exits are infrastructure observations, not evidence of delivered work.
    if !matches!(
        Outcome::classify(facts.exit_code, facts.interrupted),
        Outcome::Success | Outcome::Failure
    ) {
        return ResolutionProposal::Release;
    }

    let Some(bundle) = facts.evidence else {
        return ResolutionProposal::AdmissionFailure;
    };
    if bundle.bead_id != facts.bead.id.as_ref()
        || bundle.dispatch.exit_code != facts.exit_code
        || bundle.dispatch.was_interrupted != facts.interrupted
        || bundle.dispatch.exit_status
            != if facts.exit_code == 0 {
                "success"
            } else {
                "failure"
            }
        || bundle.dispatch.exit_reason != format!("exit_code:{}", facts.exit_code)
    {
        return ResolutionProposal::AdmissionFailure;
    }
    let Some(decision) = facts.decision else {
        return ResolutionProposal::AdmissionFailure;
    };
    if decision.validate().is_err() {
        return ResolutionProposal::AdmissionFailure;
    }
    match decision {
        ResolveDecision::Complete { .. } => {
            if facts.gates == GateVerdict::Pending {
                ResolutionProposal::VerifyThenComplete
            } else if facts.gates == GateVerdict::Unavailable {
                ResolutionProposal::AdmissionFailure
            } else if facts.gates == GateVerdict::Rejected
                || (!facts.work_accepted && !facts.policy.allow_unshipped_completion)
            {
                if facts.policy.failure_ceiling_reached {
                    ResolutionProposal::Quarantine
                } else {
                    ResolutionProposal::Release
                }
            } else {
                ResolutionProposal::Complete
            }
        }
        ResolveDecision::Retry { .. } => {
            if facts.policy.failure_ceiling_reached {
                ResolutionProposal::Quarantine
            } else {
                ResolutionProposal::Release
            }
        }
        ResolveDecision::Blocked { .. } => ResolutionProposal::Block,
        ResolveDecision::Split { parent_bead_id, .. } => {
            if parent_bead_id == facts.bead.id.as_ref() {
                ResolutionProposal::Split
            } else {
                ResolutionProposal::AdmissionFailure
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod test_contracts {
    use super::*;
    use crate::resolve::evidence::{DispatchEvidence, GitEvidence, GitState};
    use crate::types::BeadId;

    fn bead() -> Bead {
        Bead {
            id: BeadId::from("test-resolution"),
            title: "Resolve delivered work".to_string(),
            body: Some("Acceptance: verified work".to_string()),
            priority: 1,
            status: BeadStatus::InProgress,
            assignee: Some("worker-a".to_string()),
            labels: Vec::new(),
            workspace: std::path::PathBuf::from("/fixture"),
            dependencies: Vec::new(),
            dependents: Vec::new(),
            comments: Vec::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn bundle() -> EvidenceBundle {
        EvidenceBundle {
            bead_id: "test-resolution".to_string(),
            acceptance_criteria: "verified work".to_string(),
            dispatch: DispatchEvidence {
                exit_code: 0,
                exit_status: "success".to_string(),
                was_interrupted: false,
                exit_reason: "exit_code:0".to_string(),
                stdout_tail: String::new(),
                stderr_tail: String::new(),
            },
            git: GitEvidence {
                pre_dispatch: None,
                post_dispatch: GitState {
                    head_sha: Some("abc".to_string()),
                    clean: true,
                    dirty_paths: Vec::new(),
                    dirty_paths_omitted: 0,
                },
            },
            commits: Vec::new(),
            commits_omitted: 0,
            diff_summary: None,
            validation: Vec::new(),
            failure_history: Vec::new(),
            history_total: 0,
            trace_tail: None,
        }
    }

    pub(crate) fn complete_requires_authoritative_work_and_gate_acceptance() {
        let bead = bead();
        let evidence = bundle();
        let claim = ClaimStatus {
            status: BeadStatus::InProgress,
            assignee: Some("worker-a".to_string()),
            revision: Some(7),
            claim_epoch: Some(3),
        };
        let decision = ResolveDecision::Complete {
            evidence: "commit and tests".to_string(),
            commit_message: "deliver".to_string(),
        };
        let facts = ResolutionFacts {
            bead: &bead,
            claim: &claim,
            actor: "worker-a",
            expected_revision: Some(7),
            expected_epoch: Some(3),
            evidence: Some(&evidence),
            gates: GateVerdict::Accepted,
            work_accepted: true,
            decision: Some(&decision),
            exit_code: 0,
            interrupted: false,
            policy: ResolutionPolicy::default(),
        };
        assert_eq!(reduce(&facts), ResolutionProposal::Complete);
        assert_eq!(
            reduce(&ResolutionFacts {
                gates: GateVerdict::Pending,
                ..facts
            }),
            ResolutionProposal::VerifyThenComplete
        );
        assert_eq!(
            reduce(&ResolutionFacts {
                work_accepted: false,
                ..facts
            }),
            ResolutionProposal::Release
        );
        assert_eq!(
            reduce(&ResolutionFacts {
                work_accepted: false,
                policy: ResolutionPolicy {
                    failure_ceiling_reached: true,
                    ..ResolutionPolicy::default()
                },
                ..facts
            }),
            ResolutionProposal::Quarantine
        );
        assert_eq!(
            reduce(&ResolutionFacts {
                gates: GateVerdict::Rejected,
                ..facts
            }),
            ResolutionProposal::Release
        );
        assert_eq!(
            reduce(&ResolutionFacts {
                gates: GateVerdict::Unavailable,
                ..facts
            }),
            ResolutionProposal::AdmissionFailure
        );
        assert_eq!(
            reduce(&ResolutionFacts {
                evidence: None,
                ..facts
            }),
            ResolutionProposal::AdmissionFailure
        );
        assert_eq!(
            reduce(&ResolutionFacts {
                decision: None,
                ..facts
            }),
            ResolutionProposal::AdmissionFailure
        );
    }

    pub(crate) fn stale_fence_crash_and_split_mismatch_fail_quiet() {
        let bead = bead();
        let evidence = bundle();
        let claim = ClaimStatus {
            status: BeadStatus::InProgress,
            assignee: Some("worker-a".to_string()),
            revision: Some(7),
            claim_epoch: Some(3),
        };
        let decision = ResolveDecision::Complete {
            evidence: "verified".to_string(),
            commit_message: "deliver".to_string(),
        };
        let facts = ResolutionFacts {
            bead: &bead,
            claim: &claim,
            actor: "worker-a",
            expected_revision: Some(7),
            expected_epoch: Some(3),
            evidence: Some(&evidence),
            gates: GateVerdict::Accepted,
            work_accepted: true,
            decision: Some(&decision),
            exit_code: 0,
            interrupted: false,
            policy: ResolutionPolicy::default(),
        };
        assert_eq!(
            reduce(&ResolutionFacts {
                expected_revision: Some(6),
                ..facts
            }),
            ResolutionProposal::OwnershipLost
        );
        assert_eq!(
            reduce(&ResolutionFacts {
                expected_epoch: Some(2),
                ..facts
            }),
            ResolutionProposal::OwnershipLost
        );
        assert_eq!(
            reduce(&ResolutionFacts {
                actor: "worker-b",
                ..facts
            }),
            ResolutionProposal::OwnershipLost
        );
        assert_eq!(
            reduce(&ResolutionFacts {
                exit_code: -9,
                ..facts
            }),
            ResolutionProposal::Release
        );
        for exit_code in [124, 127, 137, 143] {
            assert_eq!(
                reduce(&ResolutionFacts { exit_code, ..facts }),
                ResolutionProposal::Release,
                "process exit {exit_code} must not close a bead"
            );
        }
        assert_eq!(
            reduce(&ResolutionFacts {
                interrupted: true,
                ..facts
            }),
            ResolutionProposal::Release
        );
        let mut invalid_evidence = evidence.clone();
        invalid_evidence.dispatch.exit_status = "failure".to_string();
        assert_eq!(
            reduce(&ResolutionFacts {
                evidence: Some(&invalid_evidence),
                ..facts
            }),
            ResolutionProposal::AdmissionFailure
        );
        invalid_evidence.dispatch.exit_status = "success".to_string();
        invalid_evidence.dispatch.exit_reason = "exit_code:1".to_string();
        assert_eq!(
            reduce(&ResolutionFacts {
                evidence: Some(&invalid_evidence),
                ..facts
            }),
            ResolutionProposal::AdmissionFailure
        );
        let split = ResolveDecision::Split {
            evidence: "independent work".to_string(),
            parent_bead_id: "other".to_string(),
            child_titles: vec!["child".to_string()],
        };
        assert_eq!(
            reduce(&ResolutionFacts {
                decision: Some(&split),
                ..facts
            }),
            ResolutionProposal::AdmissionFailure
        );
    }
}
