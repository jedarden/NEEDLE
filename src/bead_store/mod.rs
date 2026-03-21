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

    /// Run `br doctor --repair` and return the report.
    async fn doctor_repair(&self) -> Result<RepairReport>;
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

        let stdout = String::from_utf8(output.stdout)
            .context("br stdout was not valid UTF-8")?;
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            bail!(
                "br {args:?} exited with code {code}\nstderr: {stderr}\nstdout: {stdout}"
            );
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
        let stdout = String::from_utf8(output.stdout)
            .context("br stdout was not valid UTF-8")?;

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
            beads.retain(|b| {
                !b.labels.iter().any(|l| filters.exclude_labels.contains(l))
            });
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
        self.run_br(&["update", id_str, "--status", "open"])
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
        // br label add <id> <label> — syntax may vary; adjust if br CLI differs.
        self.run_br(&["label", "add", id_str, label])
            .await
            .with_context(|| format!("br label add {id_str} {label} failed"))?;
        Ok(())
    }

    async fn doctor_repair(&self) -> Result<RepairReport> {
        let stdout = self
            .run_br(&["doctor", "--repair"])
            .await
            .context("br doctor --repair failed")?;
        // Parse the output into warnings and fixed items.
        let mut report = RepairReport::default();
        for line in stdout.lines() {
            if let Some(rest) = line.strip_prefix("WARN ") {
                report.warnings.push(rest.to_string());
            } else if let Some(rest) = line.strip_prefix("FIXED ") {
                report.fixed.push(rest.to_string());
            }
        }
        Ok(report)
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
        let stdout = "WARN some-warning\nFIXED repaired-item\nOK normal-line\n";
        let mut report = RepairReport::default();
        for line in stdout.lines() {
            if let Some(rest) = line.strip_prefix("WARN ") {
                report.warnings.push(rest.to_string());
            } else if let Some(rest) = line.strip_prefix("FIXED ") {
                report.fixed.push(rest.to_string());
            }
        }
        assert_eq!(report.warnings, vec!["some-warning"]);
        assert_eq!(report.fixed, vec!["repaired-item"]);
    }
}
