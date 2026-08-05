//! NEEDLE — Navigates Every Enqueued Deliverable, Logs Effort.
//!
//! Library crate exposing the public API for integration tests and embedding.

pub mod agent_event;
pub mod bead_store;
pub mod canary;
pub mod cargo_test;
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
pub mod integration_t;
pub mod learning;
pub mod mitosis;
pub mod outcome;
pub mod peer;
pub mod process_guard;
pub mod prompt;
pub mod rate_limit;
pub mod registry;
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
pub mod validation;
pub mod worker;
