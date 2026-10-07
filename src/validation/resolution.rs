//! Aggregate gate execution into the verdict consumed by resolution.

use super::{GateReport, GateResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateVerdict {
    /// Gates have not run yet. The reducer may propose verification, not close.
    Pending,
    Accepted,
    Rejected,
    Unavailable,
}

pub fn verdict(verified: bool, report: Option<&GateReport>) -> GateVerdict {
    let Some(report) = report else {
        return if verified {
            GateVerdict::Accepted
        } else {
            GateVerdict::Unavailable
        };
    };
    if report.results.values().any(|result| {
        matches!(
            result,
            GateResult::Unsatisfiable(_) | GateResult::ExecutionError { .. }
        )
    }) {
        GateVerdict::Unavailable
    } else if !verified || !report.all_passed {
        GateVerdict::Rejected
    } else {
        GateVerdict::Accepted
    }
}
