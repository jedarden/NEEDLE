//! Abstract bead store interface and `br` CLI implementation.
//!
//! NEEDLE interacts with beads exclusively through the `BeadStore` trait. The
//! default implementation shells out to `br --json`. JSON parsing failures are
//! explicit errors — never silently treated as empty results (v1 bug).
//!
//! The trait is `Send + Sync` because it is called from async worker tasks.
//!
//! Depends on: `types`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;

use crate::types::{Bead, BeadId, ClaimResult};

// ─── Corruption detection ────────────────────────────────────────────────────

/// Known error strings that indicate SQLite database corruption.
const CORRUPTION_MARKERS: &[&str] = &[
    "database disk image is malformed",
    "database is locked",
    "database or disk is full",
    "attempt to write a readonly database",
    "file is not a database",
];

/// Known error strings that indicate br sync conflicts.
const SYNC_CONFLICT_MARKERS: &[&str] = &["SYNC_CONFLICT", "JSONL is newer", "sync conflict"];

/// Check if an error message indicates SQLite database corruption.
///
/// Returns `true` if the message contains any known corruption marker.
pub fn is_corruption_error(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    CORRUPTION_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

/// Check if an error message indicates a br sync conflict.
///
/// Returns `true` if the message contains any known sync conflict marker.
pub fn is_sync_conflict(msg: &str) -> bool {
    SYNC_CONFLICT_MARKERS
        .iter()
        .any(|marker| msg.contains(marker))
}

/// Outcome of a database recovery attempt.
#[derive(Debug)]
pub enum RecoveryOutcome {
    /// `br doctor --repair` fixed the issue.
    Repaired(RepairReport),
    /// Full rebuild (rm db + br sync --import) succeeded.
    Rebuilt,
    /// Recovery failed — JSONL itself may be corrupt or missing.
    Failed(anyhow::Error),
}

/// Error returned when SYNC_CONFLICT recovery fails after retry.
///
/// This is a distinct error type so callers can detect when br sync
/// recovery was attempted but the retry still failed. The caller may
/// choose to emit a failure event and continue rather than blocking.
#[derive(Debug, thiserror::Error)]
#[error("SYNC_CONFLICT recovery failed: {reason}")]
pub struct SyncRecoveryError {
    pub reason: String,
}

// ─── Filters ─────────────────────────────────────────────────────────────────

/// Filters applied when listing ready beads.
#[derive(Debug, Default, Clone)]
pub struct Filters {
    /// Only return beads assigned to this actor. `None` = no filter.
    pub assignee: Option<String>,
    /// Exclude beads that have any of these labels.
    pub exclude_labels: Vec<String>,
}

// ─── RepairReport ─────────────────────────────────────────────────────────────

/// Summary from `br doctor --repair`.
#[derive(Debug, Default)]
pub struct RepairReport {
    pub warnings: Vec<String>,
    pub fixed: Vec<String>,
}

// ─── BeadStore trait ─────────────────────────────────────────────────────────

/// Abstract interface to the bead backend.
#[async_trait]
pub trait BeadStore: Send + Sync {
    /// List all beads with no incomplete blockers (ready to work on).
    ///
    /// Returns an empty `Vec` when the queue is empty — that is not an error.
    /// Returns `Err` if JSON parsing or br invocation fails.
    async fn ready(&self, filters: &Filters) -> Result<Vec<Bead>>;

    /// List ALL beads in the workspace (no readiness/filter checks).
    ///
    /// Used by Knot strand for three-state verification — a DIFFERENT code
    /// path from `ready()` to avoid v1's false-positive bug.
    async fn list_all(&self) -> Result<Vec<Bead>>;

    /// Fetch a single bead by ID.
    async fn show(&self, id: &BeadId) -> Result<Bead>;

    /// Attempt to atomically claim a bead (set status=in_progress, assignee=actor).
    ///
    /// Returns a `ClaimResult` describing the outcome:
    /// - `Claimed(bead)` — success, returns the full bead.
    /// - `RaceLost { claimed_by }` — another worker got there first.
    /// - `NotClaimable { reason }` — bead not in a claimable state.
    async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult>;

    /// Atomically find and claim the next available bead (server-selected).
    ///
    /// This is the preferred method for multi-worker scenarios as it eliminates
    /// race conditions: the server selects the bead and assigns it in a single
    /// BEGIN IMMEDIATE transaction. Two workers calling this simultaneously will
    /// always receive distinct beads.
    ///
    /// Returns a `ClaimResult` describing the outcome:
    /// - `Claimed(bead)` — success, returns the full bead.
    /// - `NotClaimable { reason }` — no beads available to claim.
    async fn claim_auto(&self, actor: &str) -> Result<ClaimResult>;

    /// Release a claimed bead back to open (e.g., after agent failure).
    async fn release(&self, id: &BeadId) -> Result<()>;

    /// Flush local bead changes to JSONL before release.
    ///
    /// Runs `br sync --flush-only` to ensure any local writes are persisted
    /// to JSONL before attempting to release a bead. This prevents SYNC_CONFLICT
    /// errors when the JSONL has newer remote changes.
    async fn flush(&self) -> Result<()>;

    /// Reopen a closed (Done) bead back to open status.
    ///
    /// Used by validation gates when verification fails after an agent has
    /// already closed the bead.
    async fn reopen(&self, id: &BeadId) -> Result<()>;

    /// List all labels on a bead.
    async fn labels(&self, id: &BeadId) -> Result<Vec<String>>;

    /// Add a label to a bead.
    async fn add_label(&self, id: &BeadId, label: &str) -> Result<()>;

    /// Remove a label from a bead.
    async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()>;

    /// Create a new bead and return its ID.
    async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId>;

    /// Add a dependency link: `blocker_id` blocks `blocked_id`.
    ///
    /// Uses `br dep add <blocker_id> --blocks <blocked_id>`.
    async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()>;

    /// Remove a dependency link: `blocker_id` blocks `blocked_id`.
    ///
    /// Uses `br dep remove <blocked_id> <blocker_id>`.
    async fn remove_dependency(&self, blocked_id: &BeadId, blocker_id: &BeadId) -> Result<()>;

    /// Run `br doctor --repair` and return the report.
    async fn doctor_repair(&self) -> Result<RepairReport>;

    /// Run `br doctor` (without `--repair`) to check database health.
    ///
    /// Returns warnings if any issues are detected, without attempting to fix them.
    async fn doctor_check(&self) -> Result<RepairReport>;

    /// Full database rebuild: remove SQLite DB and reimport from JSONL.
    ///
    /// 1. rm .beads/beads.db
    /// 2. br sync --import
    /// 3. Verify: br doctor
    ///
    /// Returns `Err` if rebuild or verification fails (JSONL itself may be corrupt).
    async fn full_rebuild(&self) -> Result<()>;
}

// ─── BrCliBeadStore ──────────────────────────────────────────────────────────

/// `br` CLI-backed bead store implementation.
///
/// All operations shell out to `br` with `--json` output and parse the result.
/// The workspace directory is set via `BEADS_PATH` / cwd when invoking br.
pub struct BrCliBeadStore {
    /// Path to the `br` binary.
    pub br_path: PathBuf,
    /// Workspace root (directory containing `.beads/`).
    pub workspace: PathBuf,
}

impl BrCliBeadStore {
    /// Construct a new store, validating that the `br` binary exists.
    pub fn new(br_path: PathBuf, workspace: PathBuf) -> Result<Self> {
        if !br_path.exists() {
            bail!("br binary not found at {}", br_path.display());
        }
        Ok(BrCliBeadStore { br_path, workspace })
    }

    /// Try to find `br` on PATH or the default install location.
    pub fn discover(workspace: PathBuf) -> Result<Self> {
        let br_path = which::which("br")
            .or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_default();
                let candidate = PathBuf::from(format!("{home}/.local/bin/br"));
                if candidate.exists() {
                    Ok(candidate)
                } else {
                    Err(anyhow!("br not found on PATH or at ~/.local/bin/br"))
                }
            })
            .context("br CLI not found; install beads_rust")?;
        Ok(BrCliBeadStore { br_path, workspace })
    }

    /// Default timeout for br subprocess calls (30 seconds).
    const DEFAULT_BR_TIMEOUT_SECS: u64 = 30;

    /// Run a `br` subcommand in the workspace directory and return stdout.
    ///
    /// Returns `Err` if the process fails to spawn, exits non-zero (unless
    /// the caller handles specific codes), or stdout is not valid UTF-8.
    async fn run_br(&self, args: &[&str]) -> Result<String> {
        self.run_br_in(&self.workspace, args, Self::DEFAULT_BR_TIMEOUT_SECS)
            .await
    }

    /// Run a `br` subcommand with a custom timeout.
    ///
    /// Use this for calls that may take longer (e.g., sync operations).
    #[allow(dead_code)]
    async fn run_br_with_timeout(&self, args: &[&str], timeout_secs: u64) -> Result<String> {
        self.run_br_in(&self.workspace, args, timeout_secs).await
    }

    async fn run_br_in(&self, dir: &Path, args: &[&str], timeout_secs: u64) -> Result<String> {
        let timeout_duration = std::time::Duration::from_secs(timeout_secs);

        // kill_on_drop ensures the process is killed if the wait_with_output
        // future is dropped (e.g., on timeout), preventing orphaned br processes.
        let mut cmd = tokio::process::Command::new(&self.br_path);
        cmd.args(args)
            .current_dir(dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn br subprocess: {args:?}"))?;

        // Wait for output with timeout. On timeout, kill_on_drop fires automatically.
        let output = match tokio::time::timeout(timeout_duration, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                return Err(e).context(format!("br subprocess failed: {args:?}"));
            }
            Err(_) => {
                tracing::error!(
                    args = ?args,
                    timeout_secs,
                    "br subprocess timed out, killing process"
                );
                bail!("br subprocess timed out after {timeout_secs}s: {args:?}");
            }
        };

        let stdout = String::from_utf8(output.stdout).context("br stdout was not valid UTF-8")?;
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);

            // FrankenSQLite crash recovery: if br was killed by a signal
            // (code() returns None) but stdout shows the operation completed
            // and stderr is empty, treat as success. This commonly happens
            // when br's SQLite layer crashes during post-commit cleanup while
            // the mutation was already persisted to the append-only JSONL file.
            if output.status.code().is_none() && stderr.is_empty() && !stdout.is_empty() {
                tracing::warn!(
                    args = ?args,
                    stdout = %stdout.trim(),
                    "br was killed by signal but stdout indicates success — \
                     treating as successful (FrankenSQLite crash recovery)"
                );
                return Ok(stdout);
            }

            // Auto-recover from SYNC_CONFLICT: run `br sync` then retry once.
            if is_sync_conflict(&stderr) {
                tracing::warn!(
                    args = ?args,
                    "br hit SYNC_CONFLICT, running br sync and retrying"
                );

                let sync_timeout = std::time::Duration::from_secs(60);
                let mut sync_cmd = tokio::process::Command::new(&self.br_path);
                sync_cmd.args(["sync"])
                    .current_dir(dir)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true);
                let sync_child = sync_cmd
                    .spawn()
                    .context("failed to spawn br sync during SYNC_CONFLICT recovery")?;

                let sync_output = match tokio::time::timeout(
                    sync_timeout,
                    sync_child.wait_with_output(),
                )
                .await
                {
                    Ok(Ok(output)) => output,
                    Ok(Err(e)) => {
                        return Err(e).context("br sync failed during SYNC_CONFLICT recovery");
                    }
                    Err(_) => {
                        tracing::error!("br sync timed out after 60s during SYNC_CONFLICT recovery, killing process");
                        bail!("br sync timed out after 60s during SYNC_CONFLICT recovery");
                    }
                };

                if !sync_output.status.success() {
                    let sync_stderr = String::from_utf8_lossy(&sync_output.stderr);
                    tracing::warn!(stderr = %sync_stderr, "br sync failed, retrying original command anyway");
                }

                // Retry the original command once with timeout.
                let mut retry_cmd = tokio::process::Command::new(&self.br_path);
                retry_cmd.args(args)
                    .current_dir(dir)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true);
                let retry_child = retry_cmd
                    .spawn()
                    .with_context(|| format!("failed to spawn br retry with args: {args:?}"))?;

                let retry =
                    match tokio::time::timeout(timeout_duration, retry_child.wait_with_output())
                        .await
                    {
                        Ok(Ok(output)) => output,
                        Ok(Err(e)) => {
                            return Err(e).context(format!("br retry failed: {args:?}"));
                        }
                        Err(_) => {
                            tracing::error!(
                                args = ?args,
                                timeout_secs,
                                "br retry timed out, killing process"
                            );
                            bail!("br retry subprocess timed out after {timeout_secs}s: {args:?}");
                        }
                    };

                let retry_stdout = String::from_utf8(retry.stdout)
                    .context("br retry stdout was not valid UTF-8")?;
                let retry_stderr = String::from_utf8_lossy(&retry.stderr).into_owned();

                if !retry.status.success() {
                    let retry_code = retry.status.code().unwrap_or(-1);
                    return Err(anyhow::Error::new(SyncRecoveryError {
                        reason: format!(
                            "exit code {retry_code} after br sync retry\n\
                             stderr: {retry_stderr}\nstdout: {retry_stdout}"
                        ),
                    }));
                }

                return Ok(retry_stdout);
            }

            bail!("br {args:?} exited with code {code}\nstderr: {stderr}\nstdout: {stdout}");
        }

        Ok(stdout)
    }

    /// Run br and return both exit code and stdout (for claim race detection).
    ///
    /// Auto-recovers from SYNC_CONFLICT (exit code 6): runs `br sync` then
    /// retries the original command once.
    async fn run_br_with_status(&self, args: &[&str]) -> Result<(i32, String)> {
        let timeout_duration = std::time::Duration::from_secs(Self::DEFAULT_BR_TIMEOUT_SECS);

        // kill_on_drop ensures the process is killed if the wait_with_output
        // future is dropped (e.g., on timeout), preventing orphaned br processes.
        let mut cmd = tokio::process::Command::new(&self.br_path);
        cmd.args(args)
            .current_dir(&self.workspace)
            .kill_on_drop(true);
        let child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn br subprocess: {args:?}"))?;

        let output = match tokio::time::timeout(timeout_duration, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                return Err(e).context(format!("br subprocess failed: {args:?}"));
            }
            Err(_) => {
                tracing::error!(
                    args = ?args,
                    timeout_secs = Self::DEFAULT_BR_TIMEOUT_SECS,
                    "br subprocess timed out, killing process"
                );
                bail!(
                    "br subprocess timed out after {timeout_secs}s: {args:?}",
                    timeout_secs = Self::DEFAULT_BR_TIMEOUT_SECS
                );
            }
        };

        let code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8(output.stdout).context("br stdout was not valid UTF-8")?;
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Auto-recover from SYNC_CONFLICT: run `br sync` then retry once.
        if code != 0 && is_sync_conflict(&stderr) {
            tracing::warn!(
                args = ?args,
                "br hit SYNC_CONFLICT (run_br_with_status), running br sync and retrying"
            );
            let sync_timeout = std::time::Duration::from_secs(60);
            let _ = tokio::time::timeout(
                sync_timeout,
                tokio::process::Command::new(&self.br_path)
                    .args(["sync"])
                    .current_dir(&self.workspace)
                    .output(),
            )
            .await;

            let retry = tokio::time::timeout(
                timeout_duration,
                tokio::process::Command::new(&self.br_path)
                    .args(args)
                    .current_dir(&self.workspace)
                    .output(),
            )
            .await
            .with_context(|| {
                format!(
                    "br retry subprocess timed out after {timeout_secs}s: {args:?}",
                    timeout_secs = Self::DEFAULT_BR_TIMEOUT_SECS
                )
            })?
            .with_context(|| format!("failed to spawn br retry with args: {args:?}"))?;

            let retry_code = retry.status.code().unwrap_or(-1);
            let retry_stdout =
                String::from_utf8(retry.stdout).context("br retry stdout was not valid UTF-8")?;
            return Ok((retry_code, retry_stdout));
        }

        Ok((code, stdout))
    }

    /// Parse a JSON array of beads from br output.
    fn parse_beads(json: &str, context: &str) -> Result<Vec<Bead>> {
        if json.trim().is_empty() {
            return Ok(vec![]);
        }
        serde_json::from_str::<Vec<Bead>>(json)
            .with_context(|| format!("JSON parse error from {context}:\n{json}"))
    }

    /// Parse a single bead from a JSON array (first element).
    fn parse_single_bead(json: &str, context: &str) -> Result<Bead> {
        let beads = Self::parse_beads(json, context)?;
        beads
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("{context} returned empty array"))
    }
}

#[async_trait]
impl BeadStore for BrCliBeadStore {
    async fn list_all(&self) -> Result<Vec<Bead>> {
        let stdout = self
            .run_br(&["list", "--json", "--limit", "0"])
            .await
            .context("br list --json failed")?;
        Self::parse_beads(&stdout, "br list --json")
    }

    async fn ready(&self, filters: &Filters) -> Result<Vec<Bead>> {
        let mut args = vec!["ready", "--json", "--limit", "0"];

        // Build filter args — stored so they live long enough for the slice.
        let assignee_arg;
        if let Some(ref assignee) = filters.assignee {
            args.push("--assignee");
            assignee_arg = assignee.clone();
            args.push(&assignee_arg);
        }

        let stdout = self.run_br(&args).await.context("br ready failed")?;
        let mut beads = Self::parse_beads(&stdout, "br ready --json")?;

        // Apply label exclusion filter (br CLI doesn't support this natively).
        if !filters.exclude_labels.is_empty() {
            beads.retain(|b| !b.labels.iter().any(|l| filters.exclude_labels.contains(l)));
        }

        Ok(beads)
    }

    async fn show(&self, id: &BeadId) -> Result<Bead> {
        let id_str = id.as_ref();
        let stdout = self
            .run_br(&["show", id_str, "--json"])
            .await
            .with_context(|| format!("br show {id_str} failed"))?;
        Self::parse_single_bead(&stdout, &format!("br show {id_str} --json"))
    }

    async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
        let id_str = id.as_ref();
        // Attempt claim by setting status=in_progress and assignee.
        let (code, _stdout) = self
            .run_br_with_status(&[
                "update",
                id_str,
                "--status",
                "in_progress",
                "--assignee",
                actor,
            ])
            .await
            .with_context(|| format!("br update {id_str} (claim) failed to spawn"))?;

        match code {
            0 => {
                // Verify we actually won by reading back the bead.
                let bead = self.show(id).await?;
                if bead.assignee.as_deref() == Some(actor) {
                    Ok(ClaimResult::Claimed(bead))
                } else {
                    let claimed_by = bead
                        .assignee
                        .clone()
                        .unwrap_or_else(|| "(unknown)".to_string());
                    Ok(ClaimResult::RaceLost { claimed_by })
                }
            }
            4 => {
                // br exit code 4 signals a conflict / optimistic lock failure.
                let bead = self.show(id).await.ok();
                let claimed_by = bead
                    .and_then(|b| b.assignee)
                    .unwrap_or_else(|| "(unknown)".to_string());
                Ok(ClaimResult::RaceLost { claimed_by })
            }
            _ => Ok(ClaimResult::NotClaimable {
                reason: format!("br update exited with code {code}"),
            }),
        }
    }

    async fn release(&self, id: &BeadId) -> Result<()> {
        let id_str = id.as_ref();
        self.run_br(&["update", id_str, "--status", "open", "--assignee", ""])
            .await
            .with_context(|| format!("br release {id_str} failed"))?;
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        self.run_br(&["sync", "--flush-only"])
            .await
            .context("br sync --flush-only failed")?;
        Ok(())
    }

    async fn reopen(&self, id: &BeadId) -> Result<()> {
        let id_str = id.as_ref();
        self.run_br(&["reopen", id_str])
            .await
            .with_context(|| format!("br reopen {id_str} failed"))?;
        Ok(())
    }

    async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
        // Read labels from br show --json since br doesn't have a label list subcommand.
        // Note: v1 omitted labels here; this bead requires explicit label fetching.
        let bead = self.show(id).await?;
        Ok(bead.labels)
    }

    async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
        let id_str = id.as_ref();
        self.run_br(&["label", "add", id_str, label])
            .await
            .with_context(|| format!("br label add {id_str} {label} failed"))?;
        Ok(())
    }

    async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
        let id_str = id.as_ref();
        self.run_br(&["label", "remove", id_str, label])
            .await
            .with_context(|| format!("br label remove {id_str} {label} failed"))?;
        Ok(())
    }

    async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId> {
        let mut args: Vec<String> = vec![
            "create".into(),
            "--title".into(),
            title.into(),
            "--body".into(),
            body.into(),
            "--json".into(),
            "--silent".into(),
        ];
        if !labels.is_empty() {
            args.push("--labels".into());
            args.push(labels.join(","));
        }
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let stdout = self.run_br(&arg_refs).await.context("br create failed")?;
        let id_str = stdout.trim();
        if id_str.is_empty() {
            bail!("br create --silent returned empty ID");
        }
        Ok(BeadId::from(id_str))
    }

    async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
        // blocker_id blocks blocked_id (child blocks parent)
        // br dep add <ISSUE> <DEPENDS_ON> --type blocks
        // ISSUE depends on DEPENDS_ON, so blocked_id depends on blocker_id
        let blocker = blocker_id.as_ref();
        let blocked = blocked_id.as_ref();
        self.run_br(&["dep", "add", blocked, blocker, "--type", "blocks"])
            .await
            .with_context(|| format!("br dep add {blocked} {blocker} --type blocks failed"))?;
        Ok(())
    }

    async fn remove_dependency(&self, blocked_id: &BeadId, blocker_id: &BeadId) -> Result<()> {
        // Remove the dependency: blocked_id depends on blocker_id
        // br dep remove <ISSUE> <DEPENDENCY>
        let blocked = blocked_id.as_ref();
        let blocker = blocker_id.as_ref();
        self.run_br(&["dep", "remove", blocked, blocker])
            .await
            .with_context(|| format!("br dep remove {blocked} {blocker} failed"))?;
        Ok(())
    }

    async fn doctor_repair(&self) -> Result<RepairReport> {
        let stdout = self
            .run_br(&["doctor", "--repair"])
            .await
            .context("br doctor --repair failed")?;
        Ok(Self::parse_doctor_output(&stdout))
    }

    async fn doctor_check(&self) -> Result<RepairReport> {
        let stdout = self.run_br(&["doctor"]).await.context("br doctor failed")?;
        Ok(Self::parse_doctor_output(&stdout))
    }

    async fn full_rebuild(&self) -> Result<()> {
        let db_path = self.workspace.join(".beads/beads.db");

        // Step 1: Remove the corrupt SQLite database.
        if db_path.exists() {
            tokio::fs::remove_file(&db_path)
                .await
                .with_context(|| format!("failed to remove {}", db_path.display()))?;
            tracing::info!(path = %db_path.display(), "removed corrupt database file");
        }

        // Also remove WAL and SHM files if present.
        for suffix in &["-wal", "-shm"] {
            let wal_path = self.workspace.join(format!(".beads/beads.db{suffix}"));
            if wal_path.exists() {
                let _ = tokio::fs::remove_file(&wal_path).await;
            }
        }

        // Step 2: Reimport from JSONL.
        self.run_br(&["sync", "--import-only"])
            .await
            .context("br sync --import-only failed during full rebuild")?;

        // Step 3: Verify with br doctor.
        let verify = self
            .run_br(&["doctor"])
            .await
            .context("br doctor verification failed after rebuild")?;
        let report = Self::parse_doctor_output(&verify);

        if !report.warnings.is_empty() {
            bail!(
                "database still has issues after rebuild: {:?}",
                report.warnings
            );
        }

        tracing::info!("database fully rebuilt from JSONL — verified clean");
        Ok(())
    }

    async fn claim_auto(&self, actor: &str) -> Result<ClaimResult> {
        // BrCliBeadStore doesn't support atomic claim_auto.
        // Fall back to the old behavior: get ready list, try to claim first.
        let filters = Filters::default();
        let candidates = self.ready(&filters).await?;
        if let Some(bead) = candidates.first() {
            self.claim(&bead.id, actor).await
        } else {
            Ok(ClaimResult::NotClaimable {
                reason: "no beads available".to_string(),
            })
        }
    }
}

impl BrCliBeadStore {
    /// Parse `br doctor` output into a `RepairReport`.
    fn parse_doctor_output(stdout: &str) -> RepairReport {
        let mut report = RepairReport::default();
        for line in stdout.lines() {
            if let Some(rest) = line.strip_prefix("WARN ") {
                // Filter out non-actionable warnings that cannot be repaired
                // (e.g., sqlite3 binary not installed on the system, or
                // leftover recovery backup files from a prior repair/rebuild).
                if rest.contains("sqlite3 not available") || rest.contains("recovery_artifacts") {
                    continue;
                }
                report.warnings.push(rest.to_string());
            } else if let Some(rest) = line.strip_prefix("FIXED ") {
                report.fixed.push(rest.to_string());
            }
        }
        report
    }

    /// Attempt database recovery: try repair first, then full rebuild.
    ///
    /// Returns the outcome of the recovery attempt. This is the primary
    /// entry point for auto-recovery from SQLite corruption.
    pub async fn recover_db(&self) -> RecoveryOutcome {
        // Step 1: Try br doctor --repair.
        tracing::warn!("attempting database recovery via br doctor --repair");
        match self.doctor_repair().await {
            Ok(report) => {
                tracing::info!(
                    warnings = report.warnings.len(),
                    fixed = report.fixed.len(),
                    "br doctor --repair completed"
                );
                return RecoveryOutcome::Repaired(report);
            }
            Err(e) => {
                tracing::warn!(error = %e, "br doctor --repair failed, attempting full rebuild");
            }
        }

        // Step 2: Full rebuild — rm db + br sync --import + verify.
        match self.full_rebuild().await {
            Ok(()) => RecoveryOutcome::Rebuilt,
            Err(e) => {
                tracing::error!(error = %e, "full database rebuild failed — JSONL may be corrupt");
                RecoveryOutcome::Failed(e)
            }
        }
    }
}

// ─── BfCliBeadStore ─────────────────────────────────────────────────────────────

/// `bf` CLI-backed bead store implementation.
///
/// Uses `bf claim` for atomic server-selected bead claiming. This eliminates
/// the race condition in `BrCliBeadStore.claim()` where two workers could both
/// see the same bead in `ready()` and race to claim it.
///
/// The key difference: `bf claim` atomically selects AND claims a bead in a
/// single BEGIN IMMEDIATE transaction, guaranteeing that concurrent workers
/// receive distinct beads.
pub struct BfCliBeadStore {
    /// Path to the `bf` binary.
    pub bf_path: PathBuf,
    /// Workspace root (directory containing `.beads/`).
    pub workspace: PathBuf,
    /// Model name for telemetry (e.g., "claude-opus-4-7").
    pub model: Option<String>,
    /// Harness name for telemetry (e.g., "needle").
    pub harness: Option<String>,
    /// Harness version for telemetry.
    pub harness_version: Option<String>,
}

impl BfCliBeadStore {
    /// Construct a new store, validating that the `bf` binary exists.
    pub fn new(
        bf_path: PathBuf,
        workspace: PathBuf,
        model: Option<String>,
        harness: Option<String>,
        harness_version: Option<String>,
    ) -> Result<Self> {
        if !bf_path.exists() {
            bail!("bf binary not found at {}", bf_path.display());
        }
        Ok(BfCliBeadStore {
            bf_path,
            workspace,
            model,
            harness,
            harness_version,
        })
    }

    /// Try to find `bf` on PATH or the default install location.
    pub fn discover(
        workspace: PathBuf,
        model: Option<String>,
        harness: Option<String>,
        harness_version: Option<String>,
    ) -> Result<Self> {
        let bf_path = which::which("bf")
            .or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_default();
                let candidate = PathBuf::from(format!("{home}/.local/bin/bf"));
                if candidate.exists() {
                    Ok(candidate)
                } else {
                    Err(anyhow!("bf not found on PATH or at ~/.local/bin/bf"))
                }
            })
            .context("bf CLI not found; install bead-forge")?;
        Ok(BfCliBeadStore {
            bf_path,
            workspace,
            model,
            harness,
            harness_version,
        })
    }

    /// Default timeout for bf subprocess calls (30 seconds).
    const DEFAULT_BF_TIMEOUT_SECS: u64 = 30;

    /// Run a `bf` subcommand in the workspace directory and return stdout.
    async fn run_bf(&self, args: &[&str]) -> Result<String> {
        self.run_bf_in(&self.workspace, args, Self::DEFAULT_BF_TIMEOUT_SECS)
            .await
    }

    async fn run_bf_in(&self, dir: &Path, args: &[&str], timeout_secs: u64) -> Result<String> {
        let timeout_duration = std::time::Duration::from_secs(timeout_secs);

        let mut cmd = tokio::process::Command::new(&self.bf_path);
        cmd.args(args)
            .current_dir(dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn bf subprocess: {args:?}"))?;

        let output = match tokio::time::timeout(timeout_duration, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                return Err(e).context(format!("bf subprocess failed: {args:?}"));
            }
            Err(_) => {
                tracing::error!(
                    args = ?args,
                    timeout_secs,
                    "bf subprocess timed out, killing process"
                );
                bail!("bf subprocess timed out after {timeout_secs}s: {args:?}");
            }
        };

        let stdout = String::from_utf8(output.stdout).context("bf stdout was not valid UTF-8")?;
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            bail!("bf {args:?} exited with code {code}\nstderr: {stderr}\nstdout: {stdout}");
        }

        Ok(stdout)
    }

    /// Parse a JSON array of beads from bf output.
    fn parse_beads(json: &str, context: &str) -> Result<Vec<Bead>> {
        if json.trim().is_empty() {
            return Ok(vec![]);
        }
        serde_json::from_str::<Vec<Bead>>(json)
            .with_context(|| format!("JSON parse error from {context}:\n{json}"))
    }

    /// Parse a single bead from a JSON array (first element).
    fn parse_single_bead(json: &str, context: &str) -> Result<Bead> {
        let beads = Self::parse_beads(json, context)?;
        beads
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("{context} returned empty array"))
    }
}

#[async_trait]
impl BeadStore for BfCliBeadStore {
    async fn list_all(&self) -> Result<Vec<Bead>> {
        let stdout = self
            .run_bf(&["list", "--json", "--limit", "0"])
            .await
            .context("bf list --json failed")?;
        Self::parse_beads(&stdout, "bf list --json")
    }

    async fn ready(&self, filters: &Filters) -> Result<Vec<Bead>> {
        let mut args = vec!["list", "--json", "--status", "open", "--limit", "0"];

        // Build filter args — stored so they live long enough for the slice.
        let assignee_arg;
        if let Some(ref assignee) = filters.assignee {
            args.push("--assignee");
            assignee_arg = assignee.clone();
            args.push(&assignee_arg);
        }

        let stdout = self.run_bf(&args).await.context("bf list failed")?;
        let mut beads = Self::parse_beads(&stdout, "bf list --json")?;

        // Apply label exclusion filter (bf CLI doesn't support this natively).
        if !filters.exclude_labels.is_empty() {
            beads.retain(|b| !b.labels.iter().any(|l| filters.exclude_labels.contains(l)));
        }

        Ok(beads)
    }

    async fn show(&self, id: &BeadId) -> Result<Bead> {
        let id_str = id.as_ref();
        let stdout = self
            .run_bf(&["show", id_str, "--json"])
            .await
            .with_context(|| format!("bf show {id_str} failed"))?;
        Self::parse_single_bead(&stdout, &format!("bf show {id_str} --json"))
    }

    async fn claim(&self, _id: &BeadId, actor: &str) -> Result<ClaimResult> {
        // BfCliBeadStore uses atomic claim_auto() for all claim operations.
        // This eliminates the race condition from the old br-style
        // "update + show verify" pattern — two workers racing to claim
        // the same bead will always receive distinct beads.
        self.claim_auto(actor).await
    }

    async fn claim_auto(&self, actor: &str) -> Result<ClaimResult> {
        // Build bf claim args
        let mut args = vec!["claim", "--assignee", actor, "--json"];

        // Optional telemetry args
        let model_arg;
        let harness_arg;
        let harness_version_arg;

        if let Some(ref model) = self.model {
            args.push("--model");
            model_arg = model.as_str();
            args.push(model_arg);
        }
        if let Some(ref harness) = self.harness {
            args.push("--harness");
            harness_arg = harness.as_str();
            args.push(harness_arg);
        }
        if let Some(ref harness_version) = self.harness_version {
            args.push("--harness-version");
            harness_version_arg = harness_version.as_str();
            args.push(harness_version_arg);
        }

        let stdout = self
            .run_bf(&args)
            .await
            .context("bf claim failed")?;

        // Parse JSON output: {"bead_id": "...", "reclaimed": 0, "assignee": "..."}
        let json: serde_json::Value = serde_json::from_str(&stdout)
            .with_context(|| format!("bf claim returned invalid JSON: {stdout}"))?;

        if let Some(bead_id) = json.get("bead_id").and_then(|v| v.as_str()) {
            if bead_id.is_empty() || stdout.contains("No beads available") {
                return Ok(ClaimResult::NotClaimable {
                    reason: "no beads available".to_string(),
                });
            }
            // Fetch the full bead details
            let bead = self.show(&BeadId::from(bead_id)).await?;
            Ok(ClaimResult::Claimed(bead))
        } else {
            Ok(ClaimResult::NotClaimable {
                reason: "no beads available".to_string(),
            })
        }
    }

    async fn release(&self, id: &BeadId) -> Result<()> {
        let id_str = id.as_ref();
        self.run_bf(&["update", id_str, "--status", "open", "--assignee", ""])
            .await
            .with_context(|| format!("bf release {id_str} failed"))?;
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        self.run_bf(&["sync", "--flush-only"])
            .await
            .context("bf sync --flush-only failed")?;
        Ok(())
    }

    async fn reopen(&self, id: &BeadId) -> Result<()> {
        let id_str = id.as_ref();
        self.run_bf(&["reopen", id_str])
            .await
            .with_context(|| format!("bf reopen {id_str} failed"))?;
        Ok(())
    }

    async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
        let bead = self.show(id).await?;
        Ok(bead.labels)
    }

    async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
        let id_str = id.as_ref();
        self.run_bf(&["label", "add", id_str, label])
            .await
            .with_context(|| format!("bf label add {id_str} {label} failed"))?;
        Ok(())
    }

    async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
        let id_str = id.as_ref();
        self.run_bf(&["label", "remove", id_str, label])
            .await
            .with_context(|| format!("bf label remove {id_str} {label} failed"))?;
        Ok(())
    }

    async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId> {
        let mut args: Vec<String> = vec![
            "create".into(),
            "--title".into(),
            title.into(),
            "--body".into(),
            body.into(),
            "--json".into(),
        ];
        if !labels.is_empty() {
            args.push("--labels".into());
            args.push(labels.join(","));
        }
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let stdout = self.run_bf(&arg_refs).await.context("bf create failed")?;
        let id_str = stdout.trim();
        if id_str.is_empty() {
            bail!("bf create returned empty ID");
        }
        Ok(BeadId::from(id_str))
    }

    async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
        let blocker = blocker_id.as_ref();
        let blocked = blocked_id.as_ref();
        self.run_bf(&["dep", "add", blocked, blocker, "--type", "blocks"])
            .await
            .with_context(|| format!("bf dep add {blocked} {blocker} --type blocks failed"))?;
        Ok(())
    }

    async fn remove_dependency(&self, blocked_id: &BeadId, blocker_id: &BeadId) -> Result<()> {
        let blocked = blocked_id.as_ref();
        let blocker = blocker_id.as_ref();
        self.run_bf(&["dep", "remove", blocked, blocker])
            .await
            .with_context(|| format!("bf dep remove {blocked} {blocker} failed"))?;
        Ok(())
    }

    async fn doctor_repair(&self) -> Result<RepairReport> {
        let stdout = self
            .run_bf(&["doctor", "--repair"])
            .await
            .context("bf doctor --repair failed")?;
        Ok(BrCliBeadStore::parse_doctor_output(&stdout))
    }

    async fn doctor_check(&self) -> Result<RepairReport> {
        let stdout = self.run_bf(&["doctor"]).await.context("bf doctor failed")?;
        Ok(BrCliBeadStore::parse_doctor_output(&stdout))
    }

    async fn full_rebuild(&self) -> Result<()> {
        let db_path = self.workspace.join(".beads/beads.db");

        if db_path.exists() {
            tokio::fs::remove_file(&db_path)
                .await
                .with_context(|| format!("failed to remove {}", db_path.display()))?;
            tracing::info!(path = %db_path.display(), "removed corrupt database file");
        }

        for suffix in &["-wal", "-shm"] {
            let wal_path = self.workspace.join(format!(".beads/beads.db{suffix}"));
            if wal_path.exists() {
                let _ = tokio::fs::remove_file(&wal_path).await;
            }
        }

        self.run_bf(&["sync", "--import-only"])
            .await
            .context("bf sync --import-only failed during full rebuild")?;

        let verify = self
            .run_bf(&["doctor"])
            .await
            .context("bf doctor verification failed after rebuild")?;
        let report = BrCliBeadStore::parse_doctor_output(&verify);

        if !report.warnings.is_empty() {
            bail!(
                "database still has issues after rebuild: {:?}",
                report.warnings
            );
        }

        tracing::info!("database fully rebuilt from JSONL — verified clean");
        Ok(())
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_default_is_empty() {
        let f = Filters::default();
        assert!(f.assignee.is_none());
        assert!(f.exclude_labels.is_empty());
    }

    #[test]
    fn parse_beads_empty_json_array() {
        let beads = BrCliBeadStore::parse_beads("[]", "test").unwrap();
        assert!(beads.is_empty());
    }

    #[test]
    fn parse_beads_empty_string_returns_empty() {
        let beads = BrCliBeadStore::parse_beads("", "test").unwrap();
        assert!(beads.is_empty());
    }

    #[test]
    fn parse_beads_malformed_json_is_error() {
        let result = BrCliBeadStore::parse_beads("{ not json", "test");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("JSON parse error"));
    }

    #[test]
    fn parse_single_bead_empty_array_is_error() {
        let result = BrCliBeadStore::parse_single_bead("[]", "test");
        assert!(result.is_err());
    }

    #[test]
    fn repair_report_parses_warn_and_fixed_lines() {
        let report = BrCliBeadStore::parse_doctor_output(
            "WARN some-warning\nFIXED repaired-item\nOK normal-line\n",
        );
        assert_eq!(report.warnings, vec!["some-warning"]);
        assert_eq!(report.fixed, vec!["repaired-item"]);
    }

    // ── Corruption detection tests ──────────────────────────────────────────

    #[test]
    fn detects_malformed_db_error() {
        assert!(is_corruption_error(
            "Error: database disk image is malformed"
        ));
    }

    #[test]
    fn detects_locked_db_error() {
        assert!(is_corruption_error("database is locked"));
    }

    #[test]
    fn detects_not_a_database_error() {
        assert!(is_corruption_error("file is not a database"));
    }

    #[test]
    fn detects_case_insensitive() {
        assert!(is_corruption_error(
            "ERROR: Database Disk Image Is Malformed"
        ));
    }

    #[test]
    fn non_corruption_error_returns_false() {
        assert!(!is_corruption_error("bead not found"));
        assert!(!is_corruption_error("connection refused"));
        assert!(!is_corruption_error(""));
    }

    #[test]
    fn corruption_in_longer_message() {
        let msg = "br [\"list\"] exited with code 1\nstderr: Error: database disk image is malformed\nstdout: ";
        assert!(is_corruption_error(msg));
    }

    // ── parse_doctor_output tests ───────────────────────────────────────────

    #[test]
    fn parse_doctor_output_empty() {
        let report = BrCliBeadStore::parse_doctor_output("");
        assert!(report.warnings.is_empty());
        assert!(report.fixed.is_empty());
    }

    #[test]
    fn parse_doctor_output_multiple_entries() {
        let report = BrCliBeadStore::parse_doctor_output(
            "WARN index missing\nWARN stale ref\nFIXED rebuilt index\nOK\n",
        );
        assert_eq!(report.warnings.len(), 2);
        assert_eq!(report.fixed.len(), 1);
    }

    #[test]
    fn parse_doctor_output_filters_sqlite3_not_available() {
        let report = BrCliBeadStore::parse_doctor_output(
            "WARN sqlite3 not available for integrity check\nWARN real issue\nFIXED something\n",
        );
        assert_eq!(
            report.warnings,
            vec!["real issue"],
            "sqlite3 not available should be filtered out"
        );
        assert_eq!(report.fixed, vec!["something"]);
    }

    #[test]
    fn parse_doctor_output_filters_recovery_artifacts() {
        let report = BrCliBeadStore::parse_doctor_output(
            "WARN db.recovery_artifacts: Preserved recovery artifacts remain for this database family (1 item(s))\nWARN real issue\n",
        );
        assert_eq!(
            report.warnings,
            vec!["real issue"],
            "recovery_artifacts should be filtered out"
        );
    }

    // ── Sync conflict detection tests ─────────────────────────────────────

    #[test]
    fn sync_conflict_detects_sync_conflict_marker() {
        assert!(is_sync_conflict("Error: SYNC_CONFLICT detected"));
    }

    #[test]
    fn sync_conflict_detects_jsonl_is_newer() {
        assert!(is_sync_conflict("JSONL is newer than database"));
    }

    #[test]
    fn sync_conflict_detects_lowercase_marker() {
        assert!(is_sync_conflict("sync conflict on update"));
    }

    #[test]
    fn sync_conflict_in_longer_stderr() {
        let msg = "br [\"update\"] exited with code 6\nstderr: SYNC_CONFLICT\nstdout: ";
        assert!(is_sync_conflict(msg));
    }

    #[test]
    fn sync_conflict_returns_false_for_non_conflict() {
        assert!(!is_sync_conflict("bead not found"));
        assert!(!is_sync_conflict("database disk image is malformed"));
        assert!(!is_sync_conflict(""));
    }

    #[test]
    fn sync_conflict_is_case_sensitive() {
        // SYNC_CONFLICT is an exact marker, case matters
        assert!(!is_sync_conflict("sync_conflict"));
        assert!(is_sync_conflict("SYNC_CONFLICT"));
    }

    // ── parse_beads edge case tests ───────────────────────────────────────

    #[test]
    fn parse_beads_whitespace_only_returns_empty() {
        let beads = BrCliBeadStore::parse_beads("   \n\t  ", "test").unwrap();
        assert!(beads.is_empty());
    }
}
