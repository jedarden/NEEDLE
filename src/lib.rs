//! NEEDLE — Navigates Every Enqueued Deliverable, Logs Effort.
//!
//! Library crate exposing the public API for integration tests and embedding.

pub mod agent_event;
pub mod bead_store;
pub mod build_metadata;
pub mod canary;
pub mod cargo_test;
pub mod checkpoint_utils;
pub mod claim;
pub mod claude_md_placement;
pub mod cli;
pub mod commit_hook;
pub mod config;
pub mod cost;
pub mod decision;
pub mod dispatch;
pub mod drift;
pub mod health;
pub mod hoop_hooks;
/// Load-simulation harness for regression tests.
///
/// Gated off by default. As committed in `ea85a89` ("chore(lab): preserve local
/// work before removing duplicate clone") this module does not compile as part
/// of the library, which broke `cargo test`, `cargo clippy` and therefore the
/// `rust-verify` CI gate for the whole repo. It was moved from `tests/` — where
/// it was inert, since nothing declared `mod integration_t` — into `src/`
/// without being adapted to build inside the crate.
///
/// Outstanding before this can be un-gated:
/// - `use needle::…` paths must become `crate::…` (the crate cannot refer to
///   itself by name from within `src/`)
/// - `tempfile` is a dev-dependency and is not available to the library; it
///   needs to become an optional dependency of this feature
/// - `Telemetry::get_events()` is called in `supervise_auto_scale_gate.rs` but
///   does not exist — this one needs whoever owns the telemetry API, it is not
///   a mechanical fix
///
/// Enable with `--features integration-t` once the above are resolved.
#[cfg(feature = "integration-t")]
pub mod integration_t;
pub mod learning;
pub mod log_writer;
pub mod mitosis;
pub mod outcome;
pub mod peer;
pub mod process_guard;
pub mod prompt;
pub mod rate_limit;
pub mod registry;
pub mod resolve;
pub mod routing;
pub mod sanitize;
pub mod skill;
pub mod span;
pub mod spawn_path;
pub mod stats;
pub mod strand;
pub mod supervisor;
pub mod telemetry;
pub mod test_output;
pub mod test_runner;
pub mod tmux_socket;
pub mod trace;
pub mod transcript;
pub mod tsnet;
pub mod types;
pub mod upgrade;
pub mod util;
pub mod validation;
pub mod worker;
pub mod workspace_equality;
