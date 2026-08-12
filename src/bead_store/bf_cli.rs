//! `bf` CLI-backed bead store implementation.
//!
//! Uses `bf claim` for atomic server-selected bead claiming. This eliminates
//! the race condition in `BrCliBeadStore.claim()` where two workers could both
//! see the same bead in `ready()` and race to claim it.
//!
//! The key difference: `bf claim` atomically selects AND claims a bead in a
//! single BEGIN IMMEDIATE transaction, guaranteeing that concurrent workers
//! receive distinct beads.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use crate::types::{Bead, BeadId, ClaimResult};

use super::{
    has_valid_bead_store, is_lock_error, spawn_with_etxtbsy_retry_child, Filters, RepairReport,
};

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
        const MAX_RETRIES: u32 = 5;
        const BASE_DELAY_MS: u64 = 50;

        let mut attempt = 0;

        loop {
            attempt += 1;
            let timeout_duration = std::time::Duration::from_secs(timeout_secs);

            let bf_path = self.bf_path.clone();
            let dir = dir.to_path_buf();
            let args = args.to_vec();
            let child = spawn_with_etxtbsy_retry_child(
                || async {
                    let mut cmd = tokio::process::Command::new(&bf_path);
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
            .with_context(|| format!("failed to spawn bf subprocess: {args:?}"))?;

            let output =
                match tokio::time::timeout(timeout_duration, child.wait_with_output()).await {
                    Ok(Ok(output)) => output,
                    Ok(Err(e)) => {
                        // For subprocess spawn errors, don't retry - these are not transient
                        tracing::error!(
                            args = ?args,
                            attempt,
                            error = %e,
                            "bf subprocess spawn failed, not retrying"
                        );
                        break;
                    }
                    Err(_) => {
                        // Timeouts are not transient lock errors - don't retry
                        tracing::error!(
                            args = ?args,
                            timeout_secs,
                            attempt,
                            "bf subprocess timed out, not retrying"
                        );
                        break;
                    }
                };

            let stdout =
                String::from_utf8(output.stdout).context("bf stdout was not valid UTF-8")?;
            let stderr = String::from_utf8_lossy(&output.stderr);

            if output.status.success() {
                return Ok(stdout);
            }

            let code = output.status.code().unwrap_or(-1);
            let stderr_trimmed = stderr.trim().to_string();

            // Check if this is a transient lock error that should be retried
            let is_lock_error = is_lock_error(&stderr_trimmed);

            if !is_lock_error || attempt >= MAX_RETRIES {
                // Either not a lock error, or we've exhausted retries
                tracing::error!(
                    args = ?args,
                    exit_code = code,
                    attempt,
                    max_retries = MAX_RETRIES,
                    is_lock_error,
                    bf_stderr = %stderr_trimmed,
                    stdout_preview = %stdout.chars().take(200).collect::<String>(),
                    "bf subprocess failed - stderr captured"
                );

                let base_error = anyhow::anyhow!("bf {args:?} exited with code {code}");
                let error_with_stderr = if stderr_trimmed.is_empty() {
                    base_error
                } else {
                    base_error.context(format!("bf stderr: {}", stderr_trimmed))
                };
                return Err(error_with_stderr);
            }

            // This is a lock error and we have retries remaining
            tracing::warn!(
                args = ?args,
                attempt,
                max_retries = MAX_RETRIES,
                exit_code = code,
                bf_stderr = %stderr_trimmed,
                "bf subprocess failed with lock error, retrying with exponential backoff"
            );

            // Calculate exponential backoff delay: BASE_DELAY_MS * 2^(attempt-1)
            let delay_ms = BASE_DELAY_MS * (1 << (attempt - 1));
            let delay = std::time::Duration::from_millis(delay_ms);

            tokio::time::sleep(delay).await;
        }

        // If we broke out of the loop, return an appropriate error
        Err(anyhow::anyhow!(
            "bf subprocess failed after {} attempts",
            attempt
        ))
    }

    /// Parse a JSON array of beads from bf output.
    /// Handles both JSON array format `[{...},{...}]` and NDJSON (one object per line).
    ///
    /// A single unparseable NDJSON line (e.g. a bead carrying a status value
    /// this build of NEEDLE doesn't yet recognize) is logged loudly and
    /// skipped rather than failing the entire list — one bad record used to
    /// take down `list_all()` for every workspace it appeared in, which broke
    /// Weave/Mend/Unravel/Knot (all of which need the full bead list) on
    /// every single cycle. This is NOT the same "silently treat as empty" v1
    /// bug the module doc warns about: that bug swallowed failures and
    /// returned nothing; this still surfaces every bad record via a loud
    /// warning, it just doesn't discard the records that DID parse.
    fn parse_beads(json: &str, context: &str) -> Result<Vec<Bead>> {
        let trimmed = json.trim();
        if trimmed.is_empty() {
            return Ok(vec![]);
        }
        // Try JSON array first (bf show returns [...])
        if trimmed.starts_with('[') {
            return serde_json::from_str::<Vec<Bead>>(trimmed)
                .with_context(|| format!("JSON parse error from {context}:\n{json}"));
        }
        // Fall back to NDJSON (bf list returns one object per line)
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

    /// Parse a single bead from a JSON array (first element).
    fn parse_single_bead(json: &str, context: &str) -> Result<Bead> {
        let beads = Self::parse_beads(json, context)?;
        beads
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("{context} returned empty array"))
    }
}

#[async_trait::async_trait]
impl super::BeadStore for BfCliBeadStore {
    async fn list_all(&self) -> Result<Vec<Bead>> {
        // Use a large explicit limit instead of --limit 0, which returns
        // an empty set on bead-forge 0.2.0 (bug). 999999 effectively means "no limit".
        let stdout = self
            .run_bf(&["list", "--json", "--limit", "999999"])
            .await?;
        Self::parse_beads(&stdout, "bf list --json")
    }

    async fn ready(&self, filters: &Filters) -> Result<Vec<Bead>> {
        // Use a large explicit limit instead of --limit 0, which returns
        // an empty set on bead-forge 0.2.0 (bug). 999999 effectively means "no limit".
        let mut args = vec!["list", "--json", "--status", "open", "--limit", "999999"];

        // Build filter args — stored so they live long enough for the slice.
        let assignee_arg;
        if let Some(ref assignee) = filters.assignee {
            args.push("--assignee");
            assignee_arg = assignee.clone();
            args.push(&assignee_arg);
        }

        let stdout = self.run_bf(&args).await?;
        let mut beads = Self::parse_beads(&stdout, "bf list --json")?;

        // Apply label exclusion filter (bf CLI doesn't support this natively).
        if !filters.exclude_labels.is_empty() {
            beads.retain(|b| !b.labels.iter().any(|l| filters.exclude_labels.contains(l)));
        }

        // Apply ID exclusion filter (in-memory filter).
        if !filters.exclude_ids.is_empty() {
            beads.retain(|b| !filters.exclude_ids.contains(&b.id));
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
        // Build bf claim args. Velocity-aware scoring metadata is passed
        // BEFORE --assignee/--json; missing values are simply omitted.
        let mut args = vec!["claim"];
        if let Some(ref model) = self.model {
            args.push("--model");
            args.push(model.as_str());
        }
        if let Some(ref harness) = self.harness {
            args.push("--harness");
            args.push(harness.as_str());
        }
        if let Some(ref harness_version) = self.harness_version {
            args.push("--harness-version");
            args.push(harness_version.as_str());
        }
        args.push("--assignee");
        args.push(actor);
        args.push("--json");

        let stdout = self.run_bf(&args).await?;

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
        // --assignee no longer exists on `bf update` in bf 0.4.1 (bf-1hmey);
        // `bf batch` (op=update) still accepts status+assignee together.
        let batch_json = serde_json::to_string(&serde_json::json!([{
            "op": "update",
            "id": id_str,
            "status": "open",
            "assignee": "",
        }]))
        .context("failed to serialize release batch payload")?;
        self.run_bf(&["batch", "--json", &batch_json])
            .await
            .with_context(|| format!("bf batch release {id_str} failed"))?;
        Ok(())
    }

    async fn block(&self, id: &BeadId) -> Result<()> {
        let id_str = id.as_ref();
        self.run_bf(&["update", id_str, "--status", "blocked"])
            .await
            .with_context(|| format!("bf block {id_str} failed"))?;
        Ok(())
    }

    async fn clear_assignee(&self, id: &BeadId) -> Result<()> {
        let id_str = id.as_ref();
        // --assignee no longer exists on `bf update` in bf 0.4.1 (bf-1hmey);
        // `bf batch` (op=update) still accepts assignee alone.
        let batch_json = serde_json::to_string(&serde_json::json!([{
            "op": "update",
            "id": id_str,
            "assignee": "",
        }]))
        .context("failed to serialize clear_assignee batch payload")?;
        self.run_bf(&["batch", "--json", &batch_json])
            .await
            .with_context(|| format!("bf batch clear_assignee {id_str} failed"))?;
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
        self.run_bf(&["label", "add", id_str, "--label", label])
            .await
            .with_context(|| format!("bf label add {id_str} {label} failed"))?;
        Ok(())
    }

    async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
        let id_str = id.as_ref();
        self.run_bf(&["label", "remove", id_str, "--label", label])
            .await
            .with_context(|| format!("bf label remove {id_str} {label} failed"))?;
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
        let stdout = self.run_bf(&arg_refs).await?;
        let id_str = stdout.trim();
        if id_str.is_empty() {
            bail!("bf create returned empty ID");
        }
        Ok(BeadId::from(id_str))
    }

    async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
        let blocker = blocker_id.as_ref();
        let blocked = blocked_id.as_ref();
        self.run_bf(&["dep", "add", blocker, "--blocks", blocked])
            .await
            .with_context(|| format!("bf dep add {blocker} --blocks {blocked} failed"))?;
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
        Ok(super::br_cli::BrCliBeadStore::parse_doctor_output(&stdout))
    }

    async fn doctor_check(&self) -> Result<RepairReport> {
        let stdout = self.run_bf(&["doctor"]).await.context("bf doctor failed")?;
        Ok(super::br_cli::BrCliBeadStore::parse_doctor_output(&stdout))
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
        let report = super::br_cli::BrCliBeadStore::parse_doctor_output(&verify);

        if !report.warnings.is_empty() {
            bail!(
                "database still has issues after rebuild: {:?}",
                report.warnings
            );
        }

        tracing::info!("database fully rebuilt from JSONL — verified clean");
        Ok(())
    }

    fn has_valid_store(&self) -> bool {
        has_valid_bead_store(&self.workspace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::BeadStore; // Import the trait for test methods that call trait methods

    fn minimal_bead_json(id: &str, status: &str) -> String {
        format!(
            r#"{{"id":"{id}","title":"Test bead","description":"desc","priority":2,"status":"{status}","assignee":null,"source_repo":"/home/coding/NEEDLE","dependencies":[],"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}}"#
        )
    }

    #[test]
    fn bf_parse_beads_accepts_completed_status() {
        // bf has been observed emitting "completed" for some done beads. A
        // single such record must not fail the whole list — see
        // needle-weave-completed-status.
        let json = minimal_bead_json("bf-1", "completed");
        let beads = BfCliBeadStore::parse_beads(&json, "test").unwrap();
        assert_eq!(beads.len(), 1);
        assert_eq!(beads[0].status, crate::types::BeadStatus::Done);
    }

    #[test]
    fn bf_parse_beads_skips_one_bad_line_keeps_the_rest() {
        // A genuinely unparseable record (unknown field type, corrupt line,
        // etc.) must not take down every other bead in the same `bf list
        // --json` call — that was the root cause of Weave/Mend/Unravel/Knot
        // silently erroring on every cycle for any workspace with one such
        // record. The bad line is skipped and loudly logged, not silently
        // dropped from view entirely (that would repeat the v1 "silent
        // empty" bug this module's doc comment warns about).
        let good_one = minimal_bead_json("bf-1", "open");
        let good_two = minimal_bead_json("bf-2", "closed");
        let bad = r#"{"id":"bf-bad","status":"open" this is not valid json"#;
        let json = format!("{good_one}\n{bad}\n{good_two}");
        let beads = BfCliBeadStore::parse_beads(&json, "test").unwrap();
        let ids: Vec<_> = beads.iter().map(|b| b.id.to_string()).collect();
        assert_eq!(ids, vec!["bf-1", "bf-2"]);
    }

    #[tokio::test]
    async fn bf_cli_bead_store_ready_passes_explicit_limit() {
        // Verify that BfCliBeadStore ready() passes an explicit limit of 999999
        let tmp_dir = tempfile::tempdir().unwrap();
        let workspace = tmp_dir.path();
        std::fs::create_dir_all(workspace.join(".beads")).unwrap();

        // Use test-specific args file in temp dir to avoid race conditions
        let args_file = tmp_dir.path().join("bf-ready-args.txt");

        // Create a fake bf that logs its arguments
        let fake_bf = tmp_dir.path().join("fake-bf-ready-limit");
        std::fs::write(
            &fake_bf,
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
            &fake_bf,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let store = BfCliBeadStore::new(fake_bf.clone(), workspace.to_path_buf(), None, None, None)
            .unwrap();
        let filters = Filters::default();

        let _ = store.ready(&filters).await;

        // Read back the arguments that were passed
        let args = std::fs::read_to_string(&args_file).unwrap();
        assert!(
            args.contains("--limit"),
            "bf ready() must pass --limit flag"
        );
        assert!(
            args.contains("999999"),
            "bf ready() must pass limit of 999999"
        );

        // Cleanup handled by tmp_dir drop
    }

    #[tokio::test]
    async fn bf_cli_bead_store_ready_filters_by_exclude_ids() {
        use std::collections::HashSet;

        // Test that ready() filters out beads whose IDs are in exclude_ids
        let tmp_dir = tempfile::tempdir().unwrap();
        let workspace = tmp_dir.path();
        std::fs::create_dir_all(workspace.join(".beads")).unwrap();

        // Create a fake bf that returns multiple beads
        let fake_bf = tmp_dir.path().join("fake-bf-ready-exclude");
        std::fs::write(
            &fake_bf,
            r#"#!/bin/sh
echo '[{"id":"bf-abc","title":"Test bead ABC","description":"desc","priority":2,"status":"open","assignee":null,"source_repo":"/home/coding/NEEDLE","dependencies":[],"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"},{"id":"bf-def","title":"Test bead DEF","description":"desc","priority":2,"status":"open","assignee":null,"source_repo":"/home/coding/NEEDLE","dependencies":[],"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}]'
"#,
        )
        .unwrap();
        std::fs::set_permissions(
            &fake_bf,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let store = BfCliBeadStore::new(fake_bf.clone(), workspace.to_path_buf(), None, None, None)
            .unwrap();

        // Test 1: No exclude_ids filtering - both beads returned
        let filters = Filters::default();
        let beads = store.ready(&filters).await.unwrap();
        assert_eq!(
            beads.len(),
            2,
            "should return both beads when no exclude_ids"
        );

        // Test 2: Exclude one bead by ID
        let mut exclude_ids = HashSet::new();
        exclude_ids.insert(BeadId::from("bf-abc".to_string()));

        let filters_with_exclude = Filters {
            assignee: None,
            exclude_labels: vec![],
            exclude_ids,
        };

        let filtered_beads = store.ready(&filters_with_exclude).await.unwrap();
        assert_eq!(
            filtered_beads.len(),
            1,
            "should return only one bead after exclude_ids filtering"
        );
        assert_eq!(
            filtered_beads[0].id.as_ref(),
            "bf-def",
            "remaining bead should be bf-def"
        );
        assert!(
            !filtered_beads.iter().any(|b| b.id.as_ref() == "bf-abc"),
            "bf-abc should be excluded"
        );
    }

    #[tokio::test]
    async fn bf_cli_bead_store_list_all_passes_explicit_limit() {
        // Verify that BfCliBeadStore list_all() passes an explicit limit of 999999
        let tmp_dir = tempfile::tempdir().unwrap();
        let workspace = tmp_dir.path();
        std::fs::create_dir_all(workspace.join(".beads")).unwrap();

        // Use test-specific args file in temp dir to avoid race conditions
        let args_file = tmp_dir.path().join("bf-list-args.txt");

        // Create a fake bf that logs its arguments
        let fake_bf = tmp_dir.path().join("fake-bf-list-limit");
        std::fs::write(
            &fake_bf,
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
            &fake_bf,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let store = BfCliBeadStore::new(fake_bf.clone(), workspace.to_path_buf(), None, None, None)
            .unwrap();

        let _ = store.list_all().await;

        // Read back the arguments that were passed
        let args = std::fs::read_to_string(&args_file).unwrap();
        assert!(
            args.contains("--limit"),
            "bf list_all() must pass --limit flag"
        );
        assert!(
            args.contains("999999"),
            "bf list_all() must pass limit of 999999"
        );
        assert!(
            !args.contains("--limit 0"),
            "bf list_all() must NOT pass limit of 0"
        );

        // Cleanup handled by tmp_dir drop
    }
}
