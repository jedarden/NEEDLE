//! `bead` (bead-rs) CLI-backed bead store implementation.
//!
//! bead-rs is a separate, clean-room reimplementation from bead-forge (`bf`)
//! — see `~/bead-rs`. Its CLI surface was deliberately built to be
//! NEEDLE-compatible (`bead show --json` emits the same one-element-array
//! shape bf does; `bead list --json` emits the same NDJSON shape; `bead
//! claim --json` emits the same `{"bead_id": ..., "assignee": ...}` shape),
//! but several commands differ from bf/br in ways that need real code, not
//! just a binary-name swap:
//!
//! - `sync flush-only` is a subcommand, not a `--flush-only` flag.
//! - `dep add <BLOCKED> <BLOCKER>` is positional with no `--blocks` flag,
//!   and the argument order is blocked-first (opposite of bf's
//!   blocker-first + `--blocks` convention).
//! - `release`/`update --clear-assignee` are native commands; there is no
//!   `batch` subcommand at all, so the bf-specific batch workaround (needed
//!   because bf 0.4.1 dropped `--assignee` from `update`) does not apply.
//! - `claim` has no `--model`/`--harness`/`--harness-version` scoring
//!   inputs (bf's velocity/critical-path scorer); they are simply omitted.
//! - `list --ready` is used instead of `list --status open` for `ready()`
//!   — bead-rs's own docs are explicit that "open" and "ready frontier"
//!   are different sets (open-but-blocked issues are `open` but not
//!   ready), so filtering on status alone would silently include blocked
//!   beads.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use crate::types::{Bead, BeadId, ClaimResult};

use super::{
    has_valid_bead_store, is_lock_error, spawn_with_etxtbsy_retry_child, Filters, RepairReport,
};

/// `bead` (bead-rs) CLI-backed bead store implementation.
pub struct BeadCliBeadStore {
    /// Path to the `bead` binary.
    pub bead_path: PathBuf,
    /// Workspace root (directory containing `.beads/`).
    pub workspace: PathBuf,
}

impl BeadCliBeadStore {
    /// Construct a new store, validating that the `bead` binary exists.
    pub fn new(bead_path: PathBuf, workspace: PathBuf) -> Result<Self> {
        if !bead_path.exists() {
            bail!("bead binary not found at {}", bead_path.display());
        }
        Ok(BeadCliBeadStore {
            bead_path,
            workspace,
        })
    }

    /// Try to find `bead` on PATH or the default install locations, matching
    /// `config::resolve_bead_cli`'s documented precedence for the `bead`
    /// backend: PATH -> ~/.local/bin/bead -> /usr/local/cargo/bin/bead.
    pub fn discover(workspace: PathBuf) -> Result<Self> {
        let bead_path = which::which("bead")
            .or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_default();
                let candidate = PathBuf::from(format!("{home}/.local/bin/bead"));
                if candidate.exists() {
                    Ok(candidate)
                } else {
                    Err(anyhow!("bead not found on PATH or at ~/.local/bin/bead"))
                }
            })
            .or_else(|_| {
                let candidate = PathBuf::from("/usr/local/cargo/bin/bead");
                if candidate.exists() {
                    Ok(candidate)
                } else {
                    Err(anyhow!("bead not found at /usr/local/cargo/bin/bead"))
                }
            })
            .context("bead CLI not found; install bead-rs")?;
        Ok(BeadCliBeadStore {
            bead_path,
            workspace,
        })
    }

    /// Default timeout for bead subprocess calls (30 seconds).
    const DEFAULT_BEAD_TIMEOUT_SECS: u64 = 30;

    /// Run a `bead` subcommand in the workspace directory and return stdout.
    async fn run_bead(&self, args: &[&str]) -> Result<String> {
        self.run_bead_in(&self.workspace, args, Self::DEFAULT_BEAD_TIMEOUT_SECS)
            .await
    }

    async fn run_bead_in(&self, dir: &Path, args: &[&str], timeout_secs: u64) -> Result<String> {
        const MAX_RETRIES: u32 = 5;
        const BASE_DELAY_MS: u64 = 50;

        let mut attempt = 0;

        loop {
            attempt += 1;
            let timeout_duration = std::time::Duration::from_secs(timeout_secs);

            let bead_path = self.bead_path.clone();
            let dir = dir.to_path_buf();
            let args = args.to_vec();
            let child = spawn_with_etxtbsy_retry_child(
                || async {
                    let mut cmd = tokio::process::Command::new(&bead_path);
                    cmd.args(&args)
                        .current_dir(&dir)
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .kill_on_drop(true);
                    cmd.spawn()
                },
                5,
                20,
            )
            .await
            .with_context(|| format!("failed to spawn bead subprocess: {args:?}"))?;

            let output =
                match tokio::time::timeout(timeout_duration, child.wait_with_output()).await {
                    Ok(Ok(output)) => output,
                    Ok(Err(e)) => {
                        tracing::error!(
                            args = ?args,
                            attempt,
                            error = %e,
                            "bead subprocess spawn failed, not retrying"
                        );
                        break;
                    }
                    Err(_) => {
                        tracing::error!(
                            args = ?args,
                            timeout_secs,
                            attempt,
                            "bead subprocess timed out, not retrying"
                        );
                        break;
                    }
                };

            let stdout =
                String::from_utf8(output.stdout).context("bead stdout was not valid UTF-8")?;
            let stderr = String::from_utf8_lossy(&output.stderr);

            if output.status.success() {
                return Ok(stdout);
            }

            let code = output.status.code().unwrap_or(-1);
            let stderr_trimmed = stderr.trim().to_string();

            // bead-rs's own optimistic-concurrency conflicts (exit code 4,
            // "if-revision mismatch") are not produced by any call this
            // store makes (none of them pass --if-revision), so the only
            // retryable failures here are SQLite lock errors, same as bf.
            let is_lock_error = is_lock_error(&stderr_trimmed);

            if !is_lock_error || attempt >= MAX_RETRIES {
                tracing::error!(
                    args = ?args,
                    exit_code = code,
                    attempt,
                    max_retries = MAX_RETRIES,
                    is_lock_error,
                    bead_stderr = %stderr_trimmed,
                    stdout_preview = %stdout.chars().take(200).collect::<String>(),
                    "bead subprocess failed - stderr captured"
                );

                let base_error = anyhow::anyhow!("bead {args:?} exited with code {code}");
                let error_with_stderr = if stderr_trimmed.is_empty() {
                    base_error
                } else {
                    base_error.context(format!("bead stderr: {}", stderr_trimmed))
                };
                return Err(error_with_stderr);
            }

            tracing::warn!(
                args = ?args,
                attempt,
                max_retries = MAX_RETRIES,
                exit_code = code,
                bead_stderr = %stderr_trimmed,
                "bead subprocess failed with lock error, retrying with exponential backoff"
            );

            let delay_ms = BASE_DELAY_MS * (1 << (attempt - 1));
            let delay = std::time::Duration::from_millis(delay_ms);

            tokio::time::sleep(delay).await;
        }

        Err(anyhow::anyhow!(
            "bead subprocess failed after {} attempts",
            attempt
        ))
    }

    /// Parse a JSON array or NDJSON stream of beads from bead output.
    /// Identical shape/fallback logic to `BfCliBeadStore::parse_beads` —
    /// `bead show --json` emits a one-element array, `bead list --json`
    /// emits one compact object per line.
    fn parse_beads(json: &str, context: &str) -> Result<Vec<Bead>> {
        let trimmed = json.trim();
        if trimmed.is_empty() {
            return Ok(vec![]);
        }
        if trimmed.starts_with('[') {
            return serde_json::from_str::<Vec<Bead>>(trimmed)
                .with_context(|| format!("JSON parse error from {context}:\n{json}"));
        }
        let mut beads = Vec::new();
        for line in trimmed.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<Bead>(line) {
                Ok(bead) => beads.push(bead),
                Err(e) => {
                    tracing::error!(
                        context = %context,
                        error = %e,
                        line = %line,
                        "NDJSON parse error on one record — skipping this bead, \
                         keeping the rest of the list intact"
                    );
                }
            }
        }
        Ok(beads)
    }

    fn parse_single_bead(json: &str, context: &str) -> Result<Bead> {
        let beads = Self::parse_beads(json, context)?;
        beads
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("{context} returned empty array"))
    }
}

#[async_trait::async_trait]
impl super::BeadStore for BeadCliBeadStore {
    async fn list_all(&self) -> Result<Vec<Bead>> {
        let stdout = self
            .run_bead(&["list", "--json", "--limit", "999999"])
            .await?;
        Self::parse_beads(&stdout, "bead list --json")
    }

    async fn ready(&self, filters: &Filters) -> Result<Vec<Bead>> {
        // `--ready` (not `--status open`) — bead-rs's own docs distinguish
        // "open" (base status) from "ready frontier" (open, unassigned,
        // not manually blocked, no unfinished blockers); status alone
        // would include blocked-but-open beads.
        let mut args = vec!["list", "--json", "--ready", "--limit", "999999"];

        let assignee_arg;
        if let Some(ref assignee) = filters.assignee {
            args.push("--assignee");
            assignee_arg = assignee.clone();
            args.push(&assignee_arg);
        }

        let stdout = self.run_bead(&args).await?;
        let mut beads = Self::parse_beads(&stdout, "bead list --json")?;

        if !filters.exclude_labels.is_empty() {
            beads.retain(|b| !b.labels.iter().any(|l| filters.exclude_labels.contains(l)));
        }
        if !filters.exclude_ids.is_empty() {
            beads.retain(|b| !filters.exclude_ids.contains(&b.id));
        }

        Ok(beads)
    }

    async fn show(&self, id: &BeadId) -> Result<Bead> {
        let id_str = id.as_ref();
        let stdout = self
            .run_bead(&["show", id_str, "--json"])
            .await
            .with_context(|| format!("bead show {id_str} failed"))?;
        Self::parse_single_bead(&stdout, &format!("bead show {id_str} --json"))
    }

    async fn claim(&self, _id: &BeadId, actor: &str) -> Result<ClaimResult> {
        self.claim_auto(actor).await
    }

    async fn claim_auto(&self, actor: &str) -> Result<ClaimResult> {
        // bead-rs has no --model/--harness/--harness-version scoring
        // inputs; claim selection is policy-based (--policy, default
        // fifo-v1) rather than velocity-scored.
        let stdout = self
            .run_bead(&["claim", "--assignee", actor, "--json"])
            .await?;

        // {"bead_id": "<id>"|null, "assignee": "...", "lease": null}
        let json: serde_json::Value = serde_json::from_str(&stdout)
            .with_context(|| format!("bead claim returned invalid JSON: {stdout}"))?;

        if let Some(bead_id) = json.get("bead_id").and_then(|v| v.as_str()) {
            if bead_id.is_empty() {
                return Ok(ClaimResult::NotClaimable {
                    reason: "no beads available".to_string(),
                });
            }
            let bead = self.show(&BeadId::from(bead_id)).await?;
            Ok(ClaimResult::Claimed(bead))
        } else {
            Ok(ClaimResult::NotClaimable {
                reason: "no beads available".to_string(),
            })
        }
    }

    async fn release(&self, id: &BeadId) -> Result<()> {
        // Native `release` command — no batch workaround needed here (that
        // exists in bf_cli.rs only because bf 0.4.1 dropped `--assignee`
        // from `update`; bead-rs never had it there to begin with).
        let id_str = id.as_ref();
        self.run_bead(&["release", id_str])
            .await
            .with_context(|| format!("bead release {id_str} failed"))?;
        Ok(())
    }

    async fn block(&self, id: &BeadId) -> Result<()> {
        let id_str = id.as_ref();
        self.run_bead(&["update", id_str, "--status", "blocked"])
            .await
            .with_context(|| format!("bead block {id_str} failed"))?;
        Ok(())
    }

    async fn clear_assignee(&self, id: &BeadId) -> Result<()> {
        let id_str = id.as_ref();
        self.run_bead(&["update", id_str, "--clear-assignee"])
            .await
            .with_context(|| format!("bead clear_assignee {id_str} failed"))?;
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        // `sync flush-only` is a subcommand, not a `--flush-only` flag.
        self.run_bead(&["sync", "flush-only"])
            .await
            .context("bead sync flush-only failed")?;
        Ok(())
    }

    async fn reopen(&self, id: &BeadId) -> Result<()> {
        let id_str = id.as_ref();
        self.run_bead(&["reopen", id_str])
            .await
            .with_context(|| format!("bead reopen {id_str} failed"))?;
        Ok(())
    }

    async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
        let bead = self.show(id).await?;
        Ok(bead.labels)
    }

    async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
        let id_str = id.as_ref();
        self.run_bead(&["label", "add", id_str, "--label", label])
            .await
            .with_context(|| format!("bead label add {id_str} {label} failed"))?;
        Ok(())
    }

    async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
        let id_str = id.as_ref();
        self.run_bead(&["label", "remove", id_str, "--label", label])
            .await
            .with_context(|| format!("bead label remove {id_str} {label} failed"))?;
        Ok(())
    }

    async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId> {
        let mut args: Vec<String> = vec![
            "create".into(),
            "--title".into(),
            title.into(),
            "--description".into(),
            body.into(),
        ];
        for label in labels {
            args.push("--label".into());
            args.push((*label).into());
        }
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let stdout = self.run_bead(&arg_refs).await?;
        let id_str = stdout.trim();
        if id_str.is_empty() {
            bail!("bead create returned empty ID");
        }
        Ok(BeadId::from(id_str))
    }

    async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
        // bead-rs is `dep add <BLOCKED> <BLOCKER>` — blocked-first,
        // positional, no --blocks flag (opposite order from bf's
        // `dep add <blocker> --blocks <blocked>`).
        let blocker = blocker_id.as_ref();
        let blocked = blocked_id.as_ref();
        self.run_bead(&["dep", "add", blocked, blocker])
            .await
            .with_context(|| format!("bead dep add {blocked} {blocker} failed"))?;
        Ok(())
    }

    async fn remove_dependency(&self, blocked_id: &BeadId, blocker_id: &BeadId) -> Result<()> {
        // Already blocked-first in the trait signature, matching bead-rs's
        // `dep remove <BLOCKED> <BLOCKER>` directly.
        let blocked = blocked_id.as_ref();
        let blocker = blocker_id.as_ref();
        self.run_bead(&["dep", "remove", blocked, blocker])
            .await
            .with_context(|| format!("bead dep remove {blocked} {blocker} failed"))?;
        Ok(())
    }

    async fn doctor_repair(&self) -> Result<RepairReport> {
        let stdout = self
            .run_bead(&["doctor", "--repair"])
            .await
            .context("bead doctor --repair failed")?;
        Ok(super::br_cli::BrCliBeadStore::parse_doctor_output(&stdout))
    }

    async fn doctor_check(&self) -> Result<RepairReport> {
        let stdout = self
            .run_bead(&["doctor"])
            .await
            .context("bead doctor failed")?;
        Ok(super::br_cli::BrCliBeadStore::parse_doctor_output(&stdout))
    }

    async fn full_rebuild(&self) -> Result<()> {
        // bead-rs's recovery path is `bead init` (recreate schema) then
        // `sync import-only --restore-into-empty` (replay the checkpoint
        // into that empty schema) — unlike bf, whose `sync --import-only`
        // recreates the schema implicitly. This path is not yet exercised
        // by any live NEEDLE deployment; validate against a real corrupt
        // store before relying on it operationally.
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

        self.run_bead(&["init"])
            .await
            .context("bead init failed during full rebuild")?;

        let checkpoint_dir = self.workspace.join(".beads/checkpoint");
        let checkpoint_str = checkpoint_dir
            .to_str()
            .ok_or_else(|| anyhow!("checkpoint path is not valid UTF-8"))?;
        self.run_bead(&[
            "sync",
            "import-only",
            "--input",
            checkpoint_str,
            "--restore-into-empty",
            "--actor",
            "needle",
        ])
        .await
        .context("bead sync import-only --restore-into-empty failed during full rebuild")?;

        let verify = self
            .run_bead(&["doctor"])
            .await
            .context("bead doctor verification failed after rebuild")?;
        let report = super::br_cli::BrCliBeadStore::parse_doctor_output(&verify);

        if !report.warnings.is_empty() {
            bail!(
                "database still has issues after rebuild: {:?}",
                report.warnings
            );
        }

        tracing::info!("database fully rebuilt from checkpoint — verified clean");
        Ok(())
    }

    fn has_valid_store(&self) -> bool {
        has_valid_bead_store(&self.workspace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::BeadStore;

    fn minimal_bead_json(id: &str, status: &str) -> String {
        format!(
            r#"{{"id":"{id}","title":"Test bead","description":"desc","priority":2,"status":"{status}","assignee":null,"dependencies":[],"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}}"#
        )
    }

    #[test]
    fn bead_parse_beads_ndjson() {
        let good_one = minimal_bead_json("demo-1", "open");
        let good_two = minimal_bead_json("demo-2", "closed");
        let json = format!("{good_one}\n{good_two}");
        let beads = BeadCliBeadStore::parse_beads(&json, "test").unwrap();
        assert_eq!(beads.len(), 2);
    }

    #[test]
    fn bead_parse_beads_one_element_array() {
        let json = format!("[{}]", minimal_bead_json("demo-1", "open"));
        let bead = BeadCliBeadStore::parse_single_bead(&json, "test").unwrap();
        assert_eq!(bead.id.as_ref(), "demo-1");
    }

    #[tokio::test]
    async fn bead_cli_ready_uses_ready_flag_not_status_open() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let workspace = tmp_dir.path();
        std::fs::create_dir_all(workspace.join(".beads")).unwrap();

        let args_file = tmp_dir.path().join("bead-ready-args.txt");
        let fake_bead = tmp_dir.path().join("fake-bead-ready");
        std::fs::write(
            &fake_bead,
            format!(
                r#"#!/bin/sh
echo "$@" > {}
echo '[]'
"#,
                args_file.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            &fake_bead,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let store = BeadCliBeadStore::new(fake_bead.clone(), workspace.to_path_buf()).unwrap();
        let filters = Filters::default();
        let _ = store.ready(&filters).await;

        let args = std::fs::read_to_string(&args_file).unwrap();
        assert!(args.contains("--ready"), "ready() must pass --ready");
        assert!(
            !args.contains("--status"),
            "ready() must not pass --status open (bead-rs has --ready instead)"
        );
    }

    #[tokio::test]
    async fn bead_cli_add_dependency_swaps_to_blocked_first() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let workspace = tmp_dir.path();
        std::fs::create_dir_all(workspace.join(".beads")).unwrap();

        let args_file = tmp_dir.path().join("bead-dep-args.txt");
        let fake_bead = tmp_dir.path().join("fake-bead-dep");
        std::fs::write(
            &fake_bead,
            format!(
                r#"#!/bin/sh
echo "$@" > {}
"#,
                args_file.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            &fake_bead,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let store = BeadCliBeadStore::new(fake_bead.clone(), workspace.to_path_buf()).unwrap();
        store
            .add_dependency(&BeadId::from("blocker-1"), &BeadId::from("blocked-1"))
            .await
            .unwrap();

        let args = std::fs::read_to_string(&args_file).unwrap();
        // blocked-1 must appear before blocker-1 (bead-rs is BLOCKED-first).
        let blocked_pos = args.find("blocked-1").unwrap();
        let blocker_pos = args.find("blocker-1").unwrap();
        assert!(
            blocked_pos < blocker_pos,
            "add_dependency must emit blocked id before blocker id for bead-rs: {args}"
        );
        assert!(
            !args.contains("--blocks"),
            "bead-rs dep add takes no --blocks flag: {args}"
        );
    }
}
