//! The measured autonomous improvement loop (ADR-029, plan section 4.10).
//!
//! NEEDLE proposes its own improvements from the attempt ledger, admits them
//! through an explicit policy, and keeps them only when a receipt shows the
//! acceptance measure actually moved. The operator reads receipts; nobody
//! curates proposals.
//!
//! Everything in this module is pure. It reads no file, store, clock or
//! network: a caller supplies collected evidence and a timestamp, and gets
//! records and decisions back. The effects — writing state, filing beads,
//! applying a controller change — belong to the controllers that own them,
//! which is what keeps the whole loop testable against fixtures without
//! touching the live estate.
//!
//! | Stage | Plan | Bead | Here |
//! | --- | --- | --- | --- |
//! | Envelope | 4.10 | `needle-2b0a309b` | [`envelope`] |
//! | Impact contract | 4.10 | `needle-9c5ee565` | [`impact`] |
//! | Executability | 4.10 | `needle-8c3520f7` | [`executable`] |
//! | Ranking | 4.10 | `needle-29f396d3` | [`scoring`] |
//! | Admission | 4.10 | `needle-43c0d818`, `needle-f754b4cb` | [`admission`] |
//! | Generate | 4.10 step 2 | `needle-908c1f25` (N-T53) | [`generator`] |
//! | Measure | 4.10 step 5 | `needle-c4e6424a` (N-T55) | [`receipts`] |

pub mod admission;
pub mod envelope;
pub mod executable;
pub mod generator;
pub mod impact;
pub mod receipts;
pub mod scoring;

pub use admission::{
    admit, AdmissionDecision, AdmissionPolicy, AdmissionRecord, AdmissionRoute, AdmissionWorld,
    RefusalReason, DEFAULT_ADMISSION_PER_DAY, DEFAULT_MAX_OPEN_ADMITTED,
};
pub use envelope::{
    AcceptanceMeasure, AuthorityLevel, Direction, EvidenceClass, EvidenceKind, EvidenceRef,
    ImpactMeasure, ImprovementProposal, ProposalRejection, ProposalScope, Rollback,
    IMPROVEMENT_PROPOSAL_SCHEMA_VERSION, PROPOSAL_REF_NAMESPACE,
};
pub use executable::{
    assess as assess_executability, ExecutableProposal, ExecutionPlan, NonExecutable,
};
pub use generator::{
    by_signature, generate, GeneratedProposals, GeneratorThresholds, RefusedProposal,
    GENERATOR_VERSION,
};
pub use impact::{
    Band, Confidence, EffortEstimate, ImpactContract, ProducerEvidence, WorkspaceImpactProfile,
    IMPACT_CONTRACT_SCHEMA_VERSION,
};
pub use receipts::{
    append as append_receipt, decide as decide_receipt, measure as measure_cohort,
    read_all as read_receipts, receipts_path, revert_proposal, Cohort, CohortMeasures,
    Contamination, ImpactReceipt, ReceiptDecision, ReceiptThresholds,
    IMPACT_RECEIPT_SCHEMA_VERSION, RECEIPT_REF_NAMESPACE,
};
pub use scoring::{
    rank, score, ScoreComponents, ScoredProposal, ScoringPolicy, SCORING_POLICY_VERSION,
};
