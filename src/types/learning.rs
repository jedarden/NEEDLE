//! Transition adapters from NEEDLE's legacy attempt records to kernel records.
//!
//! These functions copy bounded, already-observable fields only. They do not
//! read stores, generate identities, or infer a semantic success outcome.

use needle_learning::{Attempt, AttemptId, BeadId, FencingEpoch, Revision, Timestamp};

use crate::attempt_history::AttemptRecord;

/// Adapt an existing attempt record after the controller supplies the
/// immutable claim-time facts that legacy records did not carry.
pub fn attempt_from_record(
    record: &AttemptRecord,
    bead_id: &str,
    bead_revision: u64,
    fencing_epoch: u64,
) -> Result<Attempt, needle_learning::InvalidId> {
    Ok(Attempt {
        schema_version: needle_learning::CURRENT_SCHEMA_VERSION,
        attempt_id: AttemptId::new(record.attempt_id.clone())?,
        bead_id: BeadId::new(bead_id.to_owned())?,
        bead_revision: Revision(bead_revision),
        fencing_epoch: FencingEpoch(fencing_epoch),
        started_at: Timestamp::new(record.recorded_at.clone())?,
    })
}
