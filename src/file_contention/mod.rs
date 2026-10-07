//! Checkout-local file-contention markers (plan Phase 20).
//!
//! An opt-in, advisory way for agents sharing ONE checkout to advertise that a
//! file may be under active modification. Markers live in
//! `<repo>/.needle/locks/<relative-path>.lock`, never leave that checkout, and
//! never replace bead-rs claims, fencing, resource keys, dependencies, or
//! attempt authority. See docs/plan/plan.md, Phase 20.

pub mod assessment;
pub mod env;
pub mod git_safety;
pub mod store;

pub use assessment::{
    assess, reclaim_or_acquire, Assessment, BeadState, FileState, HolderStatus, Liveness,
    LocalHolderStatus,
};
pub use store::{
    AcquireOutcome, Baseline, Conflict, MarkerRead, MarkerRecord, MarkerStore, Participant,
    PathIntent, WriteIntent, LOCKS_DIR,
};
