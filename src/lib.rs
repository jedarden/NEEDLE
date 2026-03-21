//! NEEDLE — Navigates Every Enqueued Deliverable, Logs Effort.
//!
//! Library crate exposing the public API for integration tests and embedding.
#![allow(dead_code)]

pub mod bead_store;
pub mod claim;
pub mod cli;
pub mod config;
pub mod dispatch;
pub mod health;
pub mod outcome;
pub mod peer;
pub mod prompt;
pub mod registry;
pub mod strand;
pub mod telemetry;
pub mod types;
pub mod worker;
