//! Splice strand: worker failure documentation.
//!
//! Strand 8 in the waterfall (runs before Knot). Scans heartbeat files for
//! workers with stale heartbeats whose tmux session is dead, and creates a
//! failure bead in the configured report workspace for each undocumented failure.
//!
//! Entry conditions:
//! - `strands.splice.enabled` is true (default: true)
//! - Heartbeat files exist in the heartbeat directory
//! - At least one worker has a stale heartbeat and dead tmux session
//!
//! State persistence:
//! - `splice_state.json` tracks which session IDs have already been documented
//! - Prevents duplicate failure beads for the same dead worker

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::bead_store::{BeadStore, BrCliBeadStore};
use crate::config::SpliceConfig;
use crate::telemetry::Telemetry;
use crate::types::StrandResult;

// ──────────────────────────────────────────────────────────────────────────────
// HeartbeatRecord
// ──────────────────────────────────────────────────────────────────────────────

/// Heartbeat record deserialized from a worker's heartbeat file.
#[derive(Debug, Deserialize)]
struct HeartbeatRecord {
    worker_id: String,
    pid: u32,
    #[serde(default)]
    state: String,
    #[serde(default)]
    current_bead: Option<String>,
    workspace: String,
    last_heartbeat: DateTime<Utc>,
    session: String,
    #[serde(default)]
    beads_processed: u64,
}

// ──────────────────────────────────────────────────────────────────────────────
// SpliceState
// ──────────────────────────────────────────────────────────────────────────────

/// Persisted state for the Splice strand.
#[derive(Debug, Default, Serialize, Deserialize)]
struct SpliceState {
    /// Session IDs that have already had a failure bead created.
    documented_sessions: HashSet<String>,
}

impl SpliceState {
    /// Load state from the state directory, returning None if file doesn't exist.
    fn load(state_dir: &Path) -> Result<Option<Self>> {
        let path = state_dir.join("splice_state.json");
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read splice state: {}", path.display()))?;
        let state: SpliceState =
            serde_json::from_str(&content).with_context(|| "failed to parse splice state")?;
        Ok(Some(state))
    }

    /// Save state to the state directory.
    fn save(&self, state_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("failed to create state dir: {}", state_dir.display()))?;
        let path = state_dir.join("splice_state.json");
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content)
            .with_context(|| format!("failed to write splice state: {}", path.display()))?;
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// SpliceStrand
// ──────────────────────────────────────────────────────────────────────────────

/// The Splice strand — worker failure documentation.
///
/// Scans heartbeat files for dead workers and creates failure beads.
pub struct SpliceStrand {
    config: SpliceConfig,
    heartbeat_dir: PathBuf,
    state_dir: PathBuf,
    #[allow(dead_code)]
    telemetry: Telemetry,
}

impl SpliceStrand {
    /// Create a new SpliceStrand.
    ///
    /// - `config`: splice strand configuration
    /// - `heartbeat_dir`: directory containing worker heartbeat JSON files
    /// - `state_dir`: directory for persisting splice state
    /// - `telemetry`: telemetry emitter
    pub fn new(
        config: SpliceConfig,
        heartbeat_dir: PathBuf,
        state_dir: PathBuf,
        telemetry: Telemetry,
    ) -> Self {
        SpliceStrand {
            config,
            heartbeat_dir,
            state_dir,
            telemetry,
        }
    }

    /// Scan for failed workers (stale heartbeat + dead tmux session).
    ///
    /// Returns a list of heartbeat records for workers that are considered dead.
    fn scan_failed_workers(&self) -> Result<Vec<HeartbeatRecord>> {
        let mut failed = Vec::new();

        // Read all *.json files from heartbeat directory.
        let entries = match std::fs::read_dir(&self.heartbeat_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!("splice: heartbeat directory does not exist");
                return Ok(Vec::new());
            }
            Err(e) => {
                return Err(e).context("failed to read heartbeat directory");
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            // Parse the heartbeat file.
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "splice: failed to read heartbeat file"
                    );
                    continue;
                }
            };

            let record: HeartbeatRecord = match serde_json::from_str(&content) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "splice: failed to parse heartbeat file"
                    );
                    continue;
                }
            };

            // Check if heartbeat is stale.
            let elapsed = Utc::now() - record.last_heartbeat;
            let stale_threshold_secs = self.config.stale_threshold_secs as i64;
            if elapsed.num_seconds() < stale_threshold_secs {
                continue;
            }

            // Check if tmux session is still alive.
            let alive = std::process::Command::new("tmux")
                .args(["has-session", "-t", &record.session])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);

            if !alive {
                // Session is dead — this is a failed worker.
                failed.push(record);
            }
        }

        Ok(failed)
    }

    /// Document a worker failure by creating a bead in the report workspace.
    ///
    /// If `report_workspace` is None or the workspace is invalid, returns Ok(())
    /// without creating a bead.
    async fn document_failure(&self, record: &HeartbeatRecord) -> Result<()> {
        let report_workspace = match &self.config.report_workspace {
            Some(ws) => ws,
            None => {
                tracing::debug!(
                    worker_id = %record.worker_id,
                    session = %record.session,
                    "splice: no report workspace configured, skipping bead creation"
                );
                return Ok(());
            }
        };

        // Verify the workspace exists and has a .beads/ subdirectory.
        let beads_dir = report_workspace.join(".beads");
        if !report_workspace.exists() || !beads_dir.exists() {
            tracing::warn!(
                workspace = %report_workspace.display(),
                worker_id = %record.worker_id,
                "splice: report workspace does not exist or has no .beads/ directory"
            );
            return Ok(());
        }

        // Instantiate bead store for the report workspace.
        let store = BrCliBeadStore::discover(report_workspace.clone())
            .context("failed to instantiate bead store for report workspace")?;

        // Build bead title.
        let title = format!("Worker failure: {} ({})", record.worker_id, record.session);

        // Calculate elapsed time since last heartbeat.
        let elapsed = Utc::now() - record.last_heartbeat;
        let elapsed_secs = elapsed.num_seconds();
        let elapsed_mins = elapsed_secs / 60;
        let elapsed_hours = elapsed_mins / 60;
        let elapsed_str = if elapsed_hours > 0 {
            format!("{}h {}m", elapsed_hours, elapsed_mins % 60)
        } else if elapsed_mins > 0 {
            format!("{}m", elapsed_mins)
        } else {
            format!("{}s", elapsed_secs)
        };

        // Build bead body.
        let current_bead_str = record.current_bead.as_deref().unwrap_or("(none)");
        let body = format!(
            "## Worker Failure\n\n\
             **Worker:** {}\n\
             **Session:** {}\n\
             **Workspace:** {}\n\
             **Last heartbeat:** {} ({} ago)\n\
             **State at failure:** {}\n\
             **Beads processed:** {}\n\
             **Current bead:** {}\n\
             **PID:** {}\n",
            record.worker_id,
            record.session,
            record.workspace,
            record.last_heartbeat.format("%Y-%m-%d %H:%M:%S UTC"),
            elapsed_str,
            record.state,
            record.beads_processed,
            current_bead_str,
            record.pid
        );

        // Create the bead.
        let bead_id = store
            .create_bead(&title, &body, &["worker-failure", "human"])
            .await
            .context("failed to create worker failure bead")?;

        tracing::info!(
            worker_id = %record.worker_id,
            session = %record.session,
            bead_id = %bead_id,
            "splice: documented worker failure"
        );

        Ok(())
    }
}

#[async_trait::async_trait]
impl super::Strand for SpliceStrand {
    fn name(&self) -> &str {
        "splice"
    }

    async fn evaluate(&self, _store: &dyn BeadStore) -> StrandResult {
        if !self.config.enabled {
            return StrandResult::NoWork;
        }

        let failed = match self.scan_failed_workers() {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(error = %e, "splice: failed to scan heartbeats");
                return StrandResult::NoWork;
            }
        };

        if failed.is_empty() {
            return StrandResult::NoWork;
        }

        let mut state = SpliceState::load(&self.state_dir)
            .ok()
            .flatten()
            .unwrap_or_default();
        let mut documented = 0usize;

        for record in &failed {
            if state.documented_sessions.contains(&record.session) {
                continue;
            }
            match self.document_failure(record).await {
                Ok(()) => {
                    state.documented_sessions.insert(record.session.clone());
                    documented += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        worker_id = %record.worker_id,
                        error = %e,
                        "splice: failed to document worker failure"
                    );
                }
            }
        }

        if documented > 0 {
            let _ = state.save(&self.state_dir);
            tracing::info!(documented, "splice: documented worker failures");
            StrandResult::WorkCreated
        } else {
            StrandResult::NoWork
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strand::Strand as _;

    /// Stub BeadStore for tests.
    struct NoOpStore;

    #[async_trait::async_trait]
    impl BeadStore for NoOpStore {
        async fn list_all(&self) -> Result<Vec<crate::types::Bead>> {
            Ok(vec![])
        }
        async fn ready(
            &self,
            _filters: &crate::bead_store::Filters,
        ) -> Result<Vec<crate::types::Bead>> {
            Ok(vec![])
        }
        async fn show(&self, _id: &crate::types::BeadId) -> Result<crate::types::Bead> {
            anyhow::bail!("not found")
        }
        async fn claim(
            &self,
            _id: &crate::types::BeadId,
            _actor: &str,
        ) -> Result<crate::types::ClaimResult> {
            anyhow::bail!("not implemented")
        }
        async fn release(&self, _id: &crate::types::BeadId) -> Result<()> {
            Ok(())
        }
        async fn flush(&self) -> Result<()> {
            Ok(())
        }
        async fn reopen(&self, _id: &crate::types::BeadId) -> Result<()> {
            Ok(())
        }
        async fn labels(&self, _id: &crate::types::BeadId) -> Result<Vec<String>> {
            Ok(vec![])
        }
        async fn add_label(&self, _id: &crate::types::BeadId, _label: &str) -> Result<()> {
            Ok(())
        }
        async fn remove_label(&self, _id: &crate::types::BeadId, _label: &str) -> Result<()> {
            Ok(())
        }
        async fn create_bead(
            &self,
            _title: &str,
            _body: &str,
            _labels: &[&str],
        ) -> Result<crate::types::BeadId> {
            Ok(crate::types::BeadId::from("new-bead".to_string()))
        }
        async fn doctor_repair(&self) -> Result<crate::bead_store::RepairReport> {
            Ok(crate::bead_store::RepairReport::default())
        }
        async fn doctor_check(&self) -> Result<crate::bead_store::RepairReport> {
            Ok(crate::bead_store::RepairReport::default())
        }
        async fn full_rebuild(&self) -> Result<()> {
            Ok(())
        }
        async fn add_dependency(
            &self,
            _blocker_id: &crate::types::BeadId,
            _blocked_id: &crate::types::BeadId,
        ) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn splice_strand_name() {
        let config = SpliceConfig::default();
        let tel = Telemetry::new("test".to_string());
        let strand = SpliceStrand::new(
            config,
            PathBuf::from("/tmp/heartbeats"),
            PathBuf::from("/tmp/state"),
            tel,
        );
        assert_eq!(strand.name(), "splice");
    }

    #[tokio::test]
    async fn splice_disabled_returns_no_work() {
        let config = SpliceConfig {
            enabled: false,
            ..Default::default()
        };
        let tel = Telemetry::new("test".to_string());
        let strand = SpliceStrand::new(
            config,
            PathBuf::from("/tmp/heartbeats"),
            PathBuf::from("/tmp/state"),
            tel,
        );
        let result = strand.evaluate(&NoOpStore).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    #[test]
    fn splice_state_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = SpliceState::default();
        state
            .documented_sessions
            .insert("session-abc123".to_string());
        state
            .documented_sessions
            .insert("session-xyz789".to_string());

        state.save(dir.path()).unwrap();

        let loaded = SpliceState::load(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.documented_sessions.len(), 2);
        assert!(loaded.documented_sessions.contains("session-abc123"));
        assert!(loaded.documented_sessions.contains("session-xyz789"));
    }

    #[tokio::test]
    async fn splice_no_heartbeats_returns_no_work() {
        let config = SpliceConfig::default();
        let tel = Telemetry::new("test".to_string());
        let temp_dir = tempfile::tempdir().unwrap();
        let heartbeat_dir = temp_dir.path().join("heartbeats");
        std::fs::create_dir_all(&heartbeat_dir).unwrap();

        let strand = SpliceStrand::new(config, heartbeat_dir, temp_dir.path().join("state"), tel);

        let result = strand.evaluate(&NoOpStore).await;
        assert!(matches!(result, StrandResult::NoWork));
    }
}
