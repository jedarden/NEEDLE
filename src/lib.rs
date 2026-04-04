//! NEEDLE — Navigates Every Enqueued Deliverable, Logs Effort.
//!
//! Library crate exposing the public API for integration tests and embedding.

pub mod agent_event;
pub mod bead_store;
pub mod canary;
pub mod claim;
pub mod cli;
pub mod config;
pub mod cost;
pub mod dispatch;
pub mod health;
pub mod learning;
pub mod mitosis;
pub mod outcome;
pub mod peer;
pub mod prompt;
pub mod rate_limit;
pub mod registry;
pub mod sanitize;
pub mod strand;
pub mod telemetry;
pub mod trace;
pub mod types;
pub mod upgrade;
pub mod validation;
pub mod worker;
