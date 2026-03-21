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

/// Check if an error message indicates SQLite database corruption.
///
/// Returns `true` if the message contains any known corruption marker.
pub fn is_corruption_error(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    CORRUPTION_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
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

    /// Release a claimed bead back to open (e.g., after agent failure).
    async fn release(&self, id: &BeadId) -> Result<()>;

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

    /// Run a `br` subcommand in the workspace directory and return stdout.
    ///
    /// Returns `Err` if the process fails to spawn, exits non-zero (unless
    /// the caller handles specific codes), or stdout is not valid UTF-8.
    async fn run_br(&self, args: &[&str]) -> Result<String> {
        self.run_br_in(&self.workspace, args).await
    }

    async fn run_br_in(&self, dir: &Path, args: &[&str]) -> Result<String> {
        let output = tokio::process::Command::new(&self.br_path)
            .args(args)
            .current_dir(dir)
            .output()
            .await
            .with_context(|| format!("failed to spawn br with args: {args:?}"))?;

        let stdout = String::from_utf8(output.stdout).context("br stdout was not valid UTF-8")?;
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            bail!("br {args:?} exited with code {code}\nstderr: {stderr}\nstdout: {stdout}");
        }

        Ok(stdout)
    }

    /// Run br and return both exit code and stdout (for claim race detection).
    async fn run_br_with_status(&self, args: &[&str]) -> Result<(i32, String)> {
        let output = tokio::process::Command::new(&self.br_path)
            .args(args)
            .current_dir(&self.workspace)
            .output()
            .await
            .with_context(|| format!("failed to spawn br with args: {args:?}"))?;

        let code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8(output.stdout).context("br stdout was not valid UTF-8")?;

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
        let blocker = blocker_id.as_ref();
        let blocked = blocked_id.as_ref();
        self.run_br(&["dep", "add", blocker, "--blocks", blocked])
            .await
            .with_context(|| format!("br dep add {blocker} --blocks {blocked} failed"))?;
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
        self.run_br(&["sync", "--import"])
            .await
            .context("br sync --import failed during full rebuild")?;

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
}

impl BrCliBeadStore {
    /// Parse `br doctor` output into a `RepairReport`.
    fn parse_doctor_output(stdout: &str) -> RepairReport {
        let mut report = RepairReport::default();
        for line in stdout.lines() {
            if let Some(rest) = line.strip_prefix("WARN ") {
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
}
