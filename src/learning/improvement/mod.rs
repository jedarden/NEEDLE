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

pub mod envelope;

pub use envelope::{
    AcceptanceMeasure, AuthorityLevel, Direction, EvidenceClass, EvidenceKind, EvidenceRef,
    ImpactMeasure, ImprovementProposal, ProposalRejection, ProposalScope, Rollback,
    IMPROVEMENT_PROPOSAL_SCHEMA_VERSION, PROPOSAL_REF_NAMESPACE,
};
