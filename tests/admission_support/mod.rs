//! Shared fixtures for the two admission harnesses (`needle-43c0d818`,
//! `needle-f754b4cb`).
//!
//! Not a test target of its own: `autotests = false` plus the explicit
//! `[[test]]` inventory means only declared roots are built, so a `mod.rs`
//! under `tests/` is a plain module both harnesses include.

#![allow(dead_code)]

use chrono::{DateTime, TimeZone, Utc};
use needle::learning::improvement::{
    admit, rank, score, AcceptanceMeasure, AdmissionPolicy, AdmissionRecord, AdmissionWorld,
    AuthorityLevel, Confidence, Direction, EffortEstimate, EvidenceClass, EvidenceKind,
    EvidenceRef, ImpactContract, ImpactMeasure, ImprovementProposal, NonExecutable,
    ProducerEvidence, ProposalScope, Rollback, ScoringPolicy, WorkspaceImpactProfile,
};

pub fn at(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, day, 12, 0, 0).unwrap()
}

pub fn now() -> DateTime<Utc> {
    at(14)
}

/// A well-formed proposal of `class`, scoped to `workspace`.
pub fn proposal(class: EvidenceClass, workspace: &str) -> ImprovementProposal {
    ImprovementProposal::new(
        class,
        vec![EvidenceRef::new(EvidenceKind::Workspace, workspace, at(13))],
        ProposalScope::new([workspace.to_string()], []),
        format!("change something concrete in {workspace}/src/lib.rs"),
        class.max_authority(),
        "verified-closure yield per attempt rises",
        AcceptanceMeasure {
            measure: ImpactMeasure::VerifiedYieldPerAttempt,
            direction: Direction::Increase,
            min_delta: 0.05,
            horizon_days: 7,
        },
        Rollback {
            description: "revert it".to_string(),
            automatic: true,
        },
        now(),
        1,
    )
    .expect("fixture proposal is valid")
}

/// A proposal claiming L5.
///
/// The envelope refuses an L5 claim at construction, so this is built valid
/// and then raised — which is exactly the shape admission has to defend
/// against: a record that reached it already claiming more than it should.
pub fn l5_proposal() -> ImprovementProposal {
    let mut proposal = proposal(EvidenceClass::RedBaselineWorkspace, "NEEDLE");
    proposal.authority = AuthorityLevel::L5;
    proposal
}

/// A policy that actually admits: shadow off, Gate D still closed.
pub fn live_policy() -> AdmissionPolicy {
    AdmissionPolicy {
        shadow: false,
        ..AdmissionPolicy::default()
    }
}

/// The state of the world admission reads, as a fixture.
#[derive(Default, Clone)]
pub struct World {
    /// An owner returned for every proposal.
    pub owner: Option<String>,
    /// An owner returned only for the proposal scoped to this workspace.
    pub owner_for: Option<(String, String)>,
    /// Whether every proposal is treated as non-executable.
    pub non_executable: bool,
    /// Proposals already admitted today.
    pub admitted_today: usize,
    /// Admitted proposals still open.
    pub open_admitted: usize,
}

/// Rank `proposals` neutrally and run admission over them.
pub fn run(
    proposals: &[ImprovementProposal],
    policy: &AdmissionPolicy,
    world: World,
) -> Vec<AdmissionRecord> {
    let scored: Vec<_> = proposals
        .iter()
        .map(|proposal| {
            let workspace = proposal
                .scope
                .workspaces
                .first()
                .cloned()
                .unwrap_or_default();
            let profile = WorkspaceImpactProfile::neutral(&workspace);
            let contract = ImpactContract::assemble(
                &workspace,
                Some(&profile),
                ProducerEvidence::new(
                    Confidence::Medium,
                    EffortEstimate::Small,
                    proposal.evidence.clone(),
                    "fixture",
                ),
                now(),
            );
            score(proposal, &contract, &ScoringPolicy::default(), now())
        })
        .collect();

    // Rank, but keep the caller's order for equal scores by leaving the
    // signature tie-break to do its job — the tests that care about order
    // assert on it explicitly.
    let ranked = rank(scored);

    let owner = world.owner.clone();
    let owner_for = world.owner_for.clone();
    let owner_of = move |proposal: &ImprovementProposal| -> Option<String> {
        if let Some(owner) = &owner {
            return Some(owner.clone());
        }
        if let Some((workspace, owner)) = &owner_for {
            if proposal.scope.workspaces.contains(workspace) {
                return Some(owner.clone());
            }
        }
        None
    };

    let non_executable_flag = world.non_executable;
    let non_executable = move |_: &ImprovementProposal| -> Option<NonExecutable> {
        non_executable_flag.then_some(NonExecutable::UnboundedScope)
    };

    let owning_workspace = |proposal: &ImprovementProposal| -> String {
        proposal
            .scope
            .workspaces
            .first()
            .cloned()
            .unwrap_or_else(|| "NEEDLE".to_string())
    };

    let index: std::collections::BTreeMap<String, ImprovementProposal> = proposals
        .iter()
        .map(|p| (p.signature.clone(), p.clone()))
        .collect();
    let lookup = |signature: &str| index.get(signature).cloned();

    let admission_world = AdmissionWorld {
        owner_of: &owner_of,
        non_executable: &non_executable,
        owning_workspace: &owning_workspace,
        admitted_today: world.admitted_today,
        open_admitted: world.open_admitted,
    };

    admit(&ranked, &lookup, policy, &admission_world)
}
