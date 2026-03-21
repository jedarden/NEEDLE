//! Core types shared across all NEEDLE modules.
//!
//! This is a leaf module — it depends on nothing else in the crate.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Unique identifier for a bead (e.g., "needle-gob").
pub type BeadId = String;

/// Unique identifier for a worker instance.
pub type WorkerId = String;

/// Priority level of a bead (lower number = higher priority).
pub type Priority = u8;

/// A bead as returned from the bead store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bead {
    pub id: BeadId,
    pub title: String,
    pub body: Option<String>,
    pub status: BeadStatus,
    pub priority: Priority,
    pub assignee: Option<String>,
    pub bead_type: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Status values for a bead lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeadStatus {
    Open,
    InProgress,
    Done,
    Blocked,
}

impl std::fmt::Display for BeadStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BeadStatus::Open => write!(f, "open"),
            BeadStatus::InProgress => write!(f, "in_progress"),
            BeadStatus::Done => write!(f, "done"),
            BeadStatus::Blocked => write!(f, "blocked"),
        }
    }
}

/// The result of an agent execution.
#[derive(Debug, Clone)]
pub struct AgentOutcome {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Worker state machine states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorkerState {
    Booting,
    Selecting,
    Claiming,
    Building,
    Dispatching,
    Executing,
    Handling,
    Logging,
    Retrying,
    Exhausted,
    Stopped,
    Errored,
}

impl std::fmt::Display for WorkerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| format!("{:?}", self));
        write!(f, "{}", s)
    }
}
