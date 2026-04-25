//! Splice strand: worker failure documentation.
//!
//! Strand 8 in the waterfall (runs before Knot). Scans heartbeat files for
//! workers with stale heartbeats whose tmux session is dead, and creates a
//! failure bead in the configured report workspace for each undocumented failure.
//!
//! Also detects live-but-looping workers: workers with fresh heartbeats that are
//! stuck in tight event loops (claim churn, state ping-pong, log runaway).
//!
//! Entry conditions:
//! - `strands.splice.enabled` is true (default: true)
//! - Heartbeat files exist in the heartbeat directory
//! - At least one worker has a stale heartbeat and dead tmux session OR
//!   a live worker exhibits loop patterns in its JSONL tail
//!
//! State persistence:
//! - `splice_state.json` tracks which session IDs have already been documented
//! - Prevents duplicate failure beads for the same dead/looping worker

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::bead_store::{BeadStore, BrCliBeadStore};
use crate::config::SpliceConfig;
use crate::telemetry::Telemetry;
use crate::types::StrandResult;

// ──────────────────────────────────────────────────────────────────────────────
// TelemetryEventLike (minimal subset for JSONL scanning)
// ──────────────────────────────────────────────────────────────────────────────

/// Minimal telemetry event representation for JSONL tail scanning.
#[derive(Debug, Deserialize)]
struct TelemetryEventLike {
    timestamp: DateTime<Utc>,
    event_type: String,
    #[serde(default)]
    bead_id: Option<String>,
    #[serde(default)]
    data: serde_json::Value,
}

// ──────────────────────────────────────────────────────────────────────────────
// Loop detection result structures
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct ClaimChurnInfo {
    bead_id: String,
    count: u32,
    sample: Vec<String>,
}

#[derive(Debug)]
struct StatePingPongInfo {
    states: Vec<String>,
    cycle_count: u32,
    sample: Vec<String>,
}

#[derive(Debug)]
struct LogRunawayInfo {
    bytes_growth: u64,
    window_secs: u64,
    sample: Vec<String>,
}

// ──────────────────────────────────────────────────────────────────────────────
// HeartbeatRecord
// ──────────────────────────────────────────────────────────────────────────────

/// Heartbeat record deserialized from a worker's heartbeat file.
#[derive(Debug, Deserialize, Clone)]
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
// Loop detection types
// ──────────────────────────────────────────────────────────────────────────────

/// Type of live-loop pattern detected.
#[derive(Debug, Clone)]
enum LoopType {
    /// Repeated claim.race_lost events for the same bead.
    ClaimChurn { bead_id: String, count: u32 },
    /// Excessive JSONL growth without forward progress.
    LogRunaway { bytes_growth: u64, window_secs: u64 },
    /// State ping-pong between a small set of states.
    StatePingPong {
        states: Vec<String>,
        cycle_count: u32,
    },
}

/// Evidence collected for a loop detection.
#[derive(Debug, Clone)]
struct LoopEvidence {
    /// Number of events scanned from JSONL tail.
    events_scanned: usize,
    /// Sample of recent events showing the pattern.
    sample_events: Vec<String>,
    /// When the loop was detected.
    detected_at: DateTime<Utc>,
}

/// A worker exhibiting a live-loop pattern.
#[derive(Debug, Clone)]
struct LiveLoopWorker {
    /// The worker's heartbeat record.
    heartbeat: HeartbeatRecord,
    /// The type of loop detected.
    loop_type: LoopType,
    /// Evidence supporting the detection.
    evidence: LoopEvidence,
}

// ──────────────────────────────────────────────────────────────────────────────
// SpliceState
// ──────────────────────────────────────────────────────────────────────────────

/// Persisted state for the Splice strand.
#[derive(Debug, Default, Serialize, Deserialize)]
struct SpliceState {
    /// Session IDs that have already had a failure bead created (dead workers).
    documented_sessions: HashSet<String>,
    /// (worker_id, session_id, pattern_name) tuples already documented (live loops).
    documented_loops: HashSet<String>,
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

    /// Derive the JSONL log path for a worker's session.
    ///
    /// Format: `<log_dir>/<worker_id>-<session_id>.jsonl`
    fn worker_log_path(&self, worker_id: &str, session_id: &str) -> Option<PathBuf> {
        // Try to get log_dir from the heartbeat record's workspace.
        // Default to ~/.needle/logs if not configured.
        let log_dir =
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".needle").join("logs"))?;

        Some(log_dir.join(format!("{}-{}.jsonl", worker_id, session_id)))
    }

    /// Read the last N events from a JSONL file.
    ///
    /// Returns lines in reverse order (newest first), limited to scan_events.
    fn read_jsonl_tail(&self, path: &Path, scan_events: usize) -> Result<Vec<String>> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read jsonl: {}", path.display()))?;

        let lines: Vec<&str> = content.lines().collect();
        let tail_len = scan_events.min(lines.len());
        let mut tail = Vec::with_capacity(tail_len);

        for line in lines.iter().rev().take(tail_len) {
            tail.push(line.to_string());
        }

        Ok(tail)
    }

    /// Scan live workers for loop patterns.
    ///
    /// Returns workers with fresh heartbeats that exhibit stuck-loop behavior.
    fn scan_live_loops(&self) -> Result<Vec<LiveLoopWorker>> {
        let mut looping = Vec::new();

        if !self.config.detect_live_loops {
            return Ok(looping);
        }

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
                Err(_) => continue,
            };

            let record: HeartbeatRecord = match serde_json::from_str(&content) {
                Ok(r) => r,
                Err(_) => continue,
            };

            // Skip stale heartbeats — those are handled by scan_failed_workers.
            let elapsed = Utc::now() - record.last_heartbeat;
            let stale_threshold_secs = self.config.stale_threshold_secs as i64;
            if elapsed.num_seconds() >= stale_threshold_secs {
                continue;
            }

            // Check for loop patterns in the JSONL tail.
            let log_path = match self.worker_log_path(&record.worker_id, &record.session) {
                Some(p) => p,
                None => continue,
            };

            if !log_path.exists() {
                continue;
            }

            if let Some(loop_worker) = self.check_worker_for_loops(&record, &log_path)? {
                looping.push(loop_worker);
            }
        }

        Ok(looping)
    }

    /// Check a single worker's JSONL for loop patterns.
    ///
    /// Returns Some(LiveLoopWorker) if a pattern is detected, None otherwise.
    fn check_worker_for_loops(
        &self,
        record: &HeartbeatRecord,
        log_path: &Path,
    ) -> Result<Option<LiveLoopWorker>> {
        let tail_events = self.read_jsonl_tail(log_path, self.config.live_loop_scan_events)?;

        // Parse events into structured form for analysis.
        let mut events: Vec<TelemetryEventLike> = Vec::new();
        for line in &tail_events {
            if let Ok(ev) = serde_json::from_str::<TelemetryEventLike>(line) {
                events.push(ev);
            }
        }

        let detected_at = Utc::now();

        // Pattern 1: Claim churn — repeated race_lost for same bead.
        if let Some(churn) = self.detect_claim_churn(&events) {
            return Ok(Some(LiveLoopWorker {
                heartbeat: record.clone(),
                loop_type: LoopType::ClaimChurn {
                    bead_id: churn.bead_id,
                    count: churn.count,
                },
                evidence: LoopEvidence {
                    events_scanned: tail_events.len(),
                    sample_events: churn.sample,
                    detected_at,
                },
            }));
        }

        // Pattern 2: State ping-pong — short cycle without forward progress.
        if let Some(pingpong) = self.detect_state_ping_pong(&events) {
            return Ok(Some(LiveLoopWorker {
                heartbeat: record.clone(),
                loop_type: LoopType::StatePingPong {
                    states: pingpong.states,
                    cycle_count: pingpong.cycle_count,
                },
                evidence: LoopEvidence {
                    events_scanned: tail_events.len(),
                    sample_events: pingpong.sample,
                    detected_at,
                },
            }));
        }

        // Pattern 3: Log runaway — excessive growth without completion.
        // This requires checking file metadata, not just event content.
        if let Some(runaway) = self.detect_log_runaway(record, log_path, &events)? {
            return Ok(Some(LiveLoopWorker {
                heartbeat: record.clone(),
                loop_type: LoopType::LogRunaway {
                    bytes_growth: runaway.bytes_growth,
                    window_secs: runaway.window_secs,
                },
                evidence: LoopEvidence {
                    events_scanned: tail_events.len(),
                    sample_events: runaway.sample,
                    detected_at,
                },
            }));
        }

        Ok(None)
    }

    /// Detect claim churn: repeated race_lost events for the same bead.
    fn detect_claim_churn(&self, events: &[TelemetryEventLike]) -> Option<ClaimChurnInfo> {
        let mut race_lost_counts: HashMap<String, u32> = HashMap::new();
        let mut sample: Vec<String> = Vec::new();

        for ev in events.iter().rev() {
            if ev.event_type == "bead.claim.race_lost" {
                if let Some(bead_id) = &ev.bead_id {
                    *race_lost_counts.entry(bead_id.clone()).or_insert(0) += 1;
                    if sample.len() < 10 {
                        sample.push(format!("{} race_lost {}", ev.timestamp, bead_id));
                    }
                }
            }
        }

        for (bead_id, count) in &race_lost_counts {
            if *count >= self.config.claim_churn_threshold {
                return Some(ClaimChurnInfo {
                    bead_id: bead_id.clone(),
                    count: *count,
                    sample,
                });
            }
        }

        None
    }

    /// Detect state ping-pong: short state cycles without forward progress.
    fn detect_state_ping_pong(&self, events: &[TelemetryEventLike]) -> Option<StatePingPongInfo> {
        // Look for state_transition events and extract state sequences.
        let mut state_transitions: Vec<(DateTime<Utc>, String, String)> = Vec::new();

        for ev in events.iter().rev() {
            if ev.event_type == "worker.state_transition" {
                if let (Some(from), Some(to)) = (&ev.data.get("from"), &ev.data.get("to")) {
                    if let (Some(from_str), Some(to_str)) = (from.as_str(), to.as_str()) {
                        state_transitions.push((
                            ev.timestamp,
                            from_str.to_string(),
                            to_str.to_string(),
                        ));
                    }
                }
            }
        }

        if state_transitions.len() < 8 {
            return None; // Not enough transitions to detect a cycle.
        }

        // Check for cycles of length <= 4 states.
        let window_size = 8.min(state_transitions.len());
        let window: Vec<_> = state_transitions
            .iter()
            .take(window_size)
            .map(|(_, _, to)| to.clone())
            .collect();

        // Count unique states in the window.
        let unique_states: HashSet<_> = window.iter().collect();

        if unique_states.len() <= 4 && window.len() >= 6 {
            // Check if we're cycling without forward-progress events.
            let has_forward_progress = events
                .iter()
                .any(|ev| matches!(ev.event_type.as_str(), "agent.completed" | "bead.completed"));

            if !has_forward_progress {
                let sample = state_transitions
                    .iter()
                    .take(10)
                    .map(|(ts, from, to)| format!("{} {} -> {}", ts.format("%H:%M:%S"), from, to))
                    .collect();

                return Some(StatePingPongInfo {
                    states: unique_states.iter().map(|s| (*s).clone()).collect(),
                    cycle_count: (window.len() / unique_states.len()) as u32,
                    sample,
                });
            }
        }

        None
    }

    /// Detect log runaway: excessive file growth without completion events.
    fn detect_log_runaway(
        &self,
        _record: &HeartbeatRecord,
        log_path: &Path,
        events: &[TelemetryEventLike],
    ) -> Result<Option<LogRunawayInfo>> {
        let metadata = std::fs::metadata(log_path)?;
        let file_size = metadata.len();

        // Check for agent.completed events in the scanned window.
        let has_completed = events
            .iter()
            .any(|ev| ev.event_type == "agent.completed" || ev.event_type == "bead.completed");

        if !has_completed && file_size > self.config.log_runaway_bytes {
            // Calculate time window from oldest to newest event.
            if let (Some(oldest), Some(newest)) = (events.first(), events.last()) {
                let window_secs = (newest.timestamp - oldest.timestamp).num_seconds().max(1) as u64;

                if window_secs <= self.config.live_loop_window_secs {
                    let sample = events
                        .iter()
                        .rev()
                        .take(10)
                        .map(|ev| format!("{} {}", ev.timestamp, ev.event_type))
                        .collect();

                    return Ok(Some(LogRunawayInfo {
                        bytes_growth: file_size,
                        window_secs,
                        sample,
                    }));
                }
            }

            // If we can't calculate window but file is huge and no completion, still flag.
            if file_size > self.config.log_runaway_bytes * 2 {
                let sample = events
                    .iter()
                    .rev()
                    .take(10)
                    .map(|ev| format!("{} {}", ev.timestamp, ev.event_type))
                    .collect();

                return Ok(Some(LogRunawayInfo {
                    bytes_growth: file_size,
                    window_secs: self.config.live_loop_window_secs,
                    sample,
                }));
            }
        }

        Ok(None)
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

    /// Document a live-loop worker by creating a bead in the report workspace.
    ///
    /// If `report_workspace` is None or the workspace is invalid, returns Ok(())
    /// without creating a bead.
    async fn document_live_loop(&self, worker: &LiveLoopWorker) -> Result<()> {
        let report_workspace = match &self.config.report_workspace {
            Some(ws) => ws,
            None => {
                tracing::debug!(
                    worker_id = %worker.heartbeat.worker_id,
                    session = %worker.heartbeat.session,
                    "splice: no report workspace configured, skipping loop bead creation"
                );
                return Ok(());
            }
        };

        // Verify the workspace exists and has a .beads/ subdirectory.
        let beads_dir = report_workspace.join(".beads");
        if !report_workspace.exists() || !beads_dir.exists() {
            tracing::warn!(
                workspace = %report_workspace.display(),
                worker_id = %worker.heartbeat.worker_id,
                "splice: report workspace does not exist or has no .beads/ directory"
            );
            return Ok(());
        }

        // Instantiate bead store for the report workspace.
        let store = BrCliBeadStore::discover(report_workspace.clone())
            .context("failed to instantiate bead store for report workspace")?;

        // Build bead title and body based on loop type.
        let (title, pattern_name, details) = match &worker.loop_type {
            LoopType::ClaimChurn { bead_id, count } => {
                let title = format!(
                    "Live loop: {} claim churn on bead {}",
                    worker.heartbeat.worker_id, bead_id
                );
                let details = format!(
                    "**Pattern:** Claim churn\n\
                     **Bead ID:** {}\n\
                     **Race-lost events (in last {}):** {}\n",
                    bead_id, worker.evidence.events_scanned, count
                );
                (title, "claim_churn".to_string(), details)
            }
            LoopType::StatePingPong {
                states,
                cycle_count,
            } => {
                let title = format!("Live loop: {} state ping-pong", worker.heartbeat.worker_id);
                let state_list = states.join(" → ");
                let details = format!(
                    "**Pattern:** State ping-pong\n\
                     **States:** {}\n\
                     **Cycles detected:** {}\n",
                    state_list, cycle_count
                );
                (title, "state_ping_pong".to_string(), details)
            }
            LoopType::LogRunaway {
                bytes_growth,
                window_secs,
            } => {
                let title = format!(
                    "Live loop: {} log runaway ({} MB)",
                    worker.heartbeat.worker_id,
                    bytes_growth / 1024 / 1024
                );
                let details = format!(
                    "**Pattern:** Log runaway\n\
                     **JSONL growth:** {} MB\n\
                     **Time window:** {}s\n\
                     **No completion events detected**\n",
                    bytes_growth / 1024 / 1024,
                    window_secs
                );
                (title, "log_runaway".to_string(), details)
            }
        };

        // Build sample events section.
        let sample_section = if worker.evidence.sample_events.is_empty() {
            String::new()
        } else {
            let samples: String = worker
                .evidence
                .sample_events
                .iter()
                .take(20)
                .map(|s| format!("  - {}\n", s))
                .collect();
            format!(
                "\n## Sample Events (last {} scanned)\n\n```\n{}\n```\n",
                worker.evidence.events_scanned, samples
            )
        };

        let body = format!(
            "## Live Worker Loop Detected\n\n\
             **Worker:** {}\n\
             **Session:** {}\n\
             **Workspace:** {}\n\
             **Last heartbeat:** {}\n\
             **State:** {}\n\
             **Beads processed:** {}\n\
             **Detected at:** {}\n\n\
             ## Pattern Details\n\n\
             {}{}\n\
             ## Context\n\n\
             Worker is alive (fresh heartbeat) but making no forward progress.\n\
             Consider investigating the selector or agent dispatch logic.",
            worker.heartbeat.worker_id,
            worker.heartbeat.session,
            worker.heartbeat.workspace,
            worker
                .heartbeat
                .last_heartbeat
                .format("%Y-%m-%d %H:%M:%S UTC"),
            worker.heartbeat.state,
            worker.heartbeat.beads_processed,
            worker.evidence.detected_at.format("%Y-%m-%d %H:%M:%S UTC"),
            details,
            sample_section
        );

        // Create the bead.
        let bead_id = store
            .create_bead(&title, &body, &["worker-loop", "human"])
            .await
            .context("failed to create worker loop bead")?;

        tracing::info!(
            worker_id = %worker.heartbeat.worker_id,
            session = %worker.heartbeat.session,
            pattern = %pattern_name,
            bead_id = %bead_id,
            "splice: documented live loop"
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

        let mut state = SpliceState::load(&self.state_dir)
            .ok()
            .flatten()
            .unwrap_or_default();
        let mut documented = 0usize;

        // Scan for failed workers (stale heartbeat + dead tmux session).
        let failed = match self.scan_failed_workers() {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(error = %e, "splice: failed to scan heartbeats");
                return StrandResult::NoWork;
            }
        };

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

        // Scan for live-but-looping workers.
        let looping = match self.scan_live_loops() {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "splice: failed to scan for live loops");
                Vec::new()
            }
        };

        for worker in &looping {
            // Build de-duplication key: (worker_id, session_id, pattern_name)
            let pattern_name = match &worker.loop_type {
                LoopType::ClaimChurn { .. } => "claim_churn",
                LoopType::StatePingPong { .. } => "state_ping_pong",
                LoopType::LogRunaway { .. } => "log_runaway",
            };
            let key = format!(
                "{}-{}-{}",
                worker.heartbeat.worker_id, worker.heartbeat.session, pattern_name
            );

            if state.documented_loops.contains(&key) {
                continue;
            }
            match self.document_live_loop(worker).await {
                Ok(()) => {
                    state.documented_loops.insert(key);
                    documented += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        worker_id = %worker.heartbeat.worker_id,
                        error = %e,
                        "splice: failed to document live loop"
                    );
                }
            }
        }

        if documented > 0 {
            let _ = state.save(&self.state_dir);
            tracing::info!(documented, "splice: documented worker failures and loops");
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
