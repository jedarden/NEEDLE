//! Environment variables NEEDLE exports to a dispatched agent when file
//! contention coverage is `enabled`, and that `needle contention` reads as
//! fallbacks for its identity flags. The names are part of the hook contract
//! (`docs/file-contention-hook-contract.md`); renaming one is a breaking change.

/// Coverage state for this dispatch: `enabled`, `supported_disabled` or
/// `unsupported`. Hooks act only on `enabled`.
pub const COVERAGE: &str = "NEEDLE_FILE_CONTENTION";
/// Hook contract version the agent is expected to honour.
pub const CONTRACT: &str = "NEEDLE_FILE_CONTENTION_CONTRACT";
/// Repository root whose `.needle/locks/` the agent records intent in.
pub const REPO: &str = "NEEDLE_FILE_CONTENTION_REPO";
/// Lease duration in seconds.
pub const LEASE_SECS: &str = "NEEDLE_FILE_CONTENTION_LEASE_SECS";
/// Bead being worked.
pub const BEAD_ID: &str = "NEEDLE_BEAD_ID";
/// Attempt identity that owns markers written during this dispatch.
pub const ATTEMPT_ID: &str = "NEEDLE_ATTEMPT_ID";
/// Interactive session identity (no attempt).
pub const SESSION_ID: &str = "NEEDLE_SESSION_ID";
/// Worker identity.
pub const WORKER_ID: &str = "NEEDLE_WORKER_ID";
/// Holder pid recorded for liveness (the agent process).
pub const HOLDER_PID: &str = "NEEDLE_FILE_CONTENTION_PID";
