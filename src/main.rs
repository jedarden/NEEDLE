//! NEEDLE — Navigates Every Enqueued Deliverable, Logs Effort.
//!
//! Module stubs are scaffolded here; implementations are built in subsequent beads.
#![allow(dead_code)]

use anyhow::Result;
use tracing_subscriber::EnvFilter;

mod bead_store;
mod claim;
mod cli;
mod config;
mod dispatch;
mod health;
mod outcome;
mod prompt;
mod strand;
mod telemetry;
mod types;
mod worker;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    cli::run()
}
