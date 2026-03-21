//! CLI layer — parses commands and manages worker sessions.
//!
//! Entry point for the `needle` binary. Routes subcommands to worker
//! lifecycle management.
//!
//! Depends on: `worker`, `config`.

use anyhow::Result;
use clap::{Parser, Subcommand};

/// NEEDLE — Navigates Every Enqueued Deliverable, Logs Effort.
#[derive(Debug, Parser)]
#[command(name = "needle", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start a worker (or fleet of workers).
    Run {
        /// Number of workers to start.
        #[arg(short = 'n', long, default_value = "1")]
        workers: u32,

        /// Worker name prefix.
        #[arg(long, default_value = "needle")]
        name: String,

        /// Workspace directory (default: current directory).
        #[arg(long)]
        workspace: Option<std::path::PathBuf>,
    },

    /// Stop all running workers.
    Stop {
        /// Worker name or prefix to stop.
        #[arg(long)]
        name: Option<String>,
    },

    /// Show worker status.
    Status,

    /// List beads in the queue.
    List {
        /// Filter by status.
        #[arg(long)]
        status: Option<String>,
    },
}

/// Entry point called from `main`.
pub fn run() -> Result<()> {
    // TODO(needle-thg): implement full CLI dispatch
    let _cli = Cli::parse();
    tracing::info!("NEEDLE CLI initialized");
    Ok(())
}
