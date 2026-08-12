//! `br` CLI-backed bead store implementation.
//!
//! All operations shell out to `br` with `--json` output and parse the result.
//! The workspace directory is set via `BEADS_PATH` / cwd when invoking br.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use crate::types::{Bead, BeadId, ClaimResult};

use super::{
    has_valid_bead_store,
    is_sync_conflict,
    spawn_with_etxtbsy_retry,
    spawn_with_etxtbsy_retry_child,
    BeadStore, // Import the trait so inherent impls can call trait methods
    Filters,
    NewChild,
    RecoveryOutcome,
    RepairReport,
    SyncRecoveryError,
};

/// `br` CLI-backed bead store implementation.
///
/// All operations shell out to `br` with `--json` output and parse the result.
/// The workspace directory is set via `BEADS_PATH` / cwd when invoking br.
pub struct BrCliBeadStore {
    /// Path to the `br` binary.
    pub br_path: PathBuf,
    /// Workspace root (directory containing `.beads/`).
    pub workspace: PathBuf,
    /// Model name for velocity-aware claim scoring (e.g., "claude-sonnet-4-6").
    ///
    /// Passed to `bf claim --model` so bead-forge can route beads to the
    /// model/harness combo that closes each issue_type fastest (plan §4B.6).
    /// `None` falls back to the population-wide average.
    pub model: Option<String>,
    /// Harness name for velocity-aware claim scoring (e.g., "needle").
    pub harness: Option<String>,
    /// Harness version for velocity-aware claim scoring.
    pub harness_version: Option<String>,
}

impl BrCliBeadStore {
    /// Construct a new store, validating that the `br` binary exists.
    pub fn new(
        br_path: PathBuf,
        workspace: PathBuf,
        model: Option<String>,
        harness: Option<String>,
        harness_version: Option<String>,
    ) -> Result<Self> {
        if !br_path.exists() {
            bail!("br binary not found at {}", br_path.display());
        }
        Ok(BrCliBeadStore {
            br_path,
            workspace,
            model,
            harness,
            harness_version,
        })
    }

    /// Try to find the bead CLI on PATH or the default install location.
    ///
    /// Resolves `bf` (bead-forge, canonical) first and only falls back to the
    /// deprecated `br` alias for hosts that still carry the shim. Preferring
    /// `br` here is what kept NEEDLE its last consumer, and on a host with no
    /// shim at all it failed outright rather than using the CLI that was
    /// actually installed.
    ///
    /// `model`/`harness`/`harness_version` are threaded into `bf claim` for
    /// velocity-aware scoring (plan §4B.6). Any may be `None` — `bf claim`
    /// treats missing metadata as a documented fallback to the
    /// population-wide average, so partial metadata is safe.
    pub fn discover(
        workspace: PathBuf,
        model: Option<String>,
        harness: Option<String>,
        harness_version: Option<String>,
    ) -> Result<Self> {
        let home = std::env::var("HOME").unwrap_or_default();
        let br_path = which::which("bf")
            .or_else(|_| {
                let candidate = PathBuf::from(format!("{home}/.local/bin/bf"));
                if candidate.exists() {
                    Ok(candidate)
                } else {
                    Err(anyhow!("bf not found on PATH or at ~/.local/bin/bf"))
                }
            })
            .or_else(|_| which::which("br"))
            .or_else(|_| {
                let candidate = PathBuf::from(format!("{home}/.local/bin/br"));
                if candidate.exists() {
                    Ok(candidate)
                } else {
                    Err(anyhow!("bf not found on PATH or at ~/.local/bin/bf"))
                }
            })
            .context("bead CLI not found; install bead-forge (bf)")?;
        Ok(BrCliBeadStore {
            br_path,
            workspace,
            model,
            harness,
            harness_version,
        })
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
        let br_path = self.br_path.clone();
        let dir_buf = dir.to_path_buf();
        let args_vec = args.to_vec();
        let child = spawn_with_etxtbsy_retry_child(
            || async {
                let mut cmd = tokio::process::Command::new(&br_path);
                cmd.args(&args_vec)
                    .current_dir(&dir_buf)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true);
                cmd.spawn()
            },
            5,
            20,
        )
        .await
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
                let br_path = self.br_path.clone();
                let dir_buf_clone = dir_buf.to_path_buf();
                let sync_child = spawn_with_etxtbsy_retry_child(
                    || async {
                        let mut sync_cmd = tokio::process::Command::new(&br_path);
                        sync_cmd
                            .args(["sync"])
                            .current_dir(&dir_buf_clone)
                            .stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped())
                            .kill_on_drop(true);
                        sync_cmd.spawn()
                    },
                    5,
                    20,
                )
                .await
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
                let br_path = self.br_path.clone();
                let dir_buf_clone = dir_buf.to_path_buf();
                let args_vec_clone = args_vec.clone();
                let retry_child = spawn_with_etxtbsy_retry_child(
                    || async {
                        let mut retry_cmd = tokio::process::Command::new(&br_path);
                        retry_cmd
                            .args(&args_vec_clone)
                            .current_dir(&dir_buf_clone)
                            .stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped())
                            .kill_on_drop(true);
                        retry_cmd.spawn()
                    },
                    5,
                    20,
                )
                .await
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
        let br_path = self.br_path.clone();
        let workspace = self.workspace.clone();
        let args_vec = args.to_vec();
        let child = spawn_with_etxtbsy_retry_child(
            || async {
                let mut cmd = tokio::process::Command::new(&br_path);
                cmd.args(&args_vec)
                    .current_dir(&workspace)
                    .kill_on_drop(true);
                cmd.spawn()
            },
            5,
            20,
        )
        .await
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
            let br_path = self.br_path.clone();
            let workspace = self.workspace.clone();
            let _ = tokio::time::timeout(
                sync_timeout,
                spawn_with_etxtbsy_retry(
                    || async {
                        tokio::process::Command::new(&br_path)
                            .args(["sync"])
                            .current_dir(&workspace)
                            .output()
                            .await
                    },
                    5,
                    20,
                ),
            )
            .await;

            let br_path = self.br_path.clone();
            let workspace = self.workspace.clone();
            let args_vec = args.to_vec();
            let retry = tokio::time::timeout(
                timeout_duration,
                spawn_with_etxtbsy_retry(
                    || async {
                        tokio::process::Command::new(&br_path)
                            .args(&args_vec)
                            .current_dir(&workspace)
                            .output()
                            .await
                    },
                    5,
                    20,
                ),
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

    /// Parse a JSON array or JSONL stream of beads from br output.
    fn parse_beads(json: &str, context: &str) -> Result<Vec<Bead>> {
        if json.trim().is_empty() {
            return Ok(vec![]);
        }
        // Try JSON array first, then fall back to JSONL (one object per line)
        if let Ok(beads) = serde_json::from_str::<Vec<Bead>>(json) {
            return Ok(beads);
        }
        json.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|line| {
                serde_json::from_str::<Bead>(line)
                    .with_context(|| format!("JSON parse error from {context}:\n{line}"))
            })
            .collect()
    }

    /// Parse a single bead from a JSON array (first element).
    fn parse_single_bead(json: &str, context: &str) -> Result<Bead> {
        let beads = Self::parse_beads(json, context)?;
        beads
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("{context} returned empty array"))
    }

    /// Run `bf claim` for atomic bead selection and claiming.
    ///
    /// This uses bead-forge's atomic claim which performs scoring
    /// (downstream_impact + critical_float + priority + created_at) and
    /// the UPDATE in a single BEGIN IMMEDIATE transaction.
    ///
    /// The worker's `--model`/`--harness`/`--harness-version` are folded into
    /// the claim so bead-forge can record a `worker_sessions`/`velocity_stats`
    /// row and compute a velocity_adjusted_score (plan §4B.6) — routing beads
    /// to the model/harness combo that closes each issue_type fastest. The
    /// flags are emitted before `--assignee`/`--json`; any that are `None` are
    /// omitted, and `bf claim` falls back to the population-wide average.
    /// Locate the `bf` binary on PATH, falling back to the default install
    /// location (`~/.local/bin/bf`).
    fn resolve_bf(&self) -> Result<PathBuf> {
        which::which("bf").or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            let candidate = PathBuf::from(format!("{home}/.local/bin/bf"));
            if candidate.exists() {
                Ok(candidate)
            } else {
                Err(anyhow!("bf not found on PATH or at ~/.local/bin/bf"))
            }
        })
    }

    /// Run `bf batch --json <ops>` and return stdout.
    ///
    /// The entire op array executes inside a single SQLite `BEGIN IMMEDIATE`
    /// transaction (bf `execute_batch`), so a crash or a failing op rolls the
    /// whole batch back. Used by [`super::BeadStore::split_bead`] for crash-safe mitosis.
    async fn run_bf_batch(&self, ops_json: &str) -> Result<String> {
        let timeout_duration = std::time::Duration::from_secs(30);
        let bf_path = self
            .resolve_bf()
            .map_err(|e| e.context("bf CLI not found; cannot run atomic batch"))?;

        let args = ["batch", "--json", ops_json];
        let bf_path_clone = bf_path.clone();
        let workspace = self.workspace.clone();
        let args = args.to_vec();
        let child = spawn_with_etxtbsy_retry_child(
            || async {
                let mut cmd = tokio::process::Command::new(&bf_path_clone);
                cmd.args(&args)
                    .current_dir(&workspace)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true);
                cmd.spawn()
            },
            5,
            20,
        )
        .await
        .context("failed to spawn bf batch subprocess")?;

        let output = match tokio::time::timeout(timeout_duration, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(e).context("bf batch subprocess failed"),
            Err(_) => {
                tracing::error!("bf batch subprocess timed out, killing process");
                bail!("bf batch subprocess timed out after 30s");
            }
        };

        let stdout =
            String::from_utf8(output.stdout).context("bf batch stdout was not valid UTF-8")?;

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("bf batch exited with code {code}\nstderr: {stderr}");
        }

        Ok(stdout)
    }

    async fn run_bf_claim(&self, actor: &str) -> Result<String> {
        let timeout_duration = std::time::Duration::from_secs(30);

        // Try to find bf on PATH or at the default install location
        let bf_path = match self.resolve_bf() {
            Ok(p) => p,
            Err(e) => {
                return Err(e.context("bf CLI not found; falling back to br-style claim"));
            }
        };

        // Build the claim args. Velocity-aware scoring metadata is passed
        // BEFORE --assignee/--json; missing values are simply omitted.
        let mut args: Vec<&str> = Vec::with_capacity(10);
        args.push("claim");
        if let Some(model) = &self.model {
            args.push("--model");
            args.push(model.as_str());
        }
        if let Some(harness) = &self.harness {
            args.push("--harness");
            args.push(harness.as_str());
        }
        if let Some(harness_version) = &self.harness_version {
            args.push("--harness-version");
            args.push(harness_version.as_str());
        }
        args.push("--assignee");
        args.push(actor);
        args.push("--json");

        let bf_path_clone = bf_path.clone();
        let workspace = self.workspace.clone();
        let args_clone = args.clone();
        let child = spawn_with_etxtbsy_retry_child(
            || async {
                let mut cmd = tokio::process::Command::new(&bf_path_clone);
                cmd.args(&args_clone)
                    .current_dir(&workspace)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true);
                cmd.spawn()
            },
            5,
            20,
        )
        .await
        .with_context(|| format!("failed to spawn bf subprocess: {:?}", args))?;

        let output = match tokio::time::timeout(timeout_duration, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                return Err(e).context(format!("bf subprocess failed: {:?}", args));
            }
            Err(_) => {
                tracing::error!(
                    args = ?args,
                    "bf subprocess timed out, killing process"
                );
                bail!("bf subprocess timed out after 30s: {:?}", args);
            }
        };

        let stdout = String::from_utf8(output.stdout).context("bf stdout was not valid UTF-8")?;
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            bail!(
                "bf {:?} exited with code {}\nstderr: {}",
                args,
                code,
                stderr
            );
        }

        Ok(stdout)
    }

    /// Build the `bf claim` subprocess arguments for testing.
    ///
    /// This is a test helper that returns the exact arguments that would be passed
    /// to the `bf claim` subprocess, including metadata flags when available.
    /// Used by tests to verify that --model/--harness/--harness-version flags are
    /// properly included when metadata is set.
    #[cfg(test)]
    pub fn build_claim_args(&self, actor: &str) -> Vec<String> {
        let mut args: Vec<String> = Vec::with_capacity(10);
        args.push("claim".to_string());
        if let Some(model) = &self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if let Some(harness) = &self.harness {
            args.push("--harness".to_string());
            args.push(harness.clone());
        }
        if let Some(harness_version) = &self.harness_version {
            args.push("--harness-version".to_string());
            args.push(harness_version.clone());
        }
        args.push("--assignee".to_string());
        args.push(actor.to_string());
        args.push("--json".to_string());
        args
    }
}

/// Build the `bf batch` op array for an atomic split: create every child, then
/// link each freshly-created child as a blocker of `parent_id`.
///
/// Creates come first so the `dep_add_blocker` ops can reference the new
/// children by positional placeholder (`@0`, `@1`, …), which bf resolves to the
/// created IDs in creation order. `dep_add_blocker.id` is the *blocked* bead
/// (the parent) and `.blocker` is the child — matching NEEDLE's
/// `add_dependency(child, parent)` semantics (child blocks parent). No `close`
/// op is emitted: a split parent stays open/blocked.
fn build_split_batch_ops(parent_id: &BeadId, children: &[NewChild<'_>]) -> Vec<serde_json::Value> {
    let mut ops = Vec::with_capacity(children.len() * 2);
    for child in children {
        ops.push(serde_json::json!({
            "op": "create",
            "title": child.title,
            "description": child.body,
            "labels": child.labels,
        }));
    }
    let parent = parent_id.as_ref();
    for idx in 0..children.len() {
        ops.push(serde_json::json!({
            "op": "dep_add_blocker",
            "id": parent,
            "blocker": format!("@{idx}"),
        }));
    }
    ops
}

/// Parse the child IDs created by `bf batch` from its stdout.
///
/// `bf batch` prints one line per op: `"[op N] ok: <id>"` for `create` ops and
/// `"[op N] ok"` (no id) for `dep_add_blocker`/`close`. Only creates carry an
/// id, so the ids returned here — in op order — are exactly the new children.
fn parse_batch_created_ids(stdout: &str) -> Vec<BeadId> {
    stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("[op ")?;
            let (_n, tail) = rest.split_once(']')?;
            let id = tail.trim().strip_prefix("ok:")?.trim();
            if id.is_empty() {
                None
            } else {
                Some(BeadId::from(id))
            }
        })
        .collect()
}

#[async_trait::async_trait]
impl super::BeadStore for BrCliBeadStore {
    async fn list_all(&self) -> Result<Vec<Bead>> {
        // Use a large explicit limit instead of --limit 0, which returns
        // an empty set on bead-forge 0.2.0 (bug). 999999 effectively means "no limit".
        let stdout = self
            .run_br(&["list", "--json", "--limit", "999999"])
            .await?;
        Self::parse_beads(&stdout, "br list --json")
    }

    async fn ready(&self, filters: &Filters) -> Result<Vec<Bead>> {
        // Always pass an explicit large limit to avoid default truncation that
        // hides low-priority beads in busy stores, and to avoid the --limit 0
        // bug in bead-forge 0.2.0 (which returns an empty set).
        let mut args = vec!["ready", "--json", "--limit", "10000"];

        // Build filter args — stored so they live long enough for the slice.
        let assignee_arg;
        if let Some(ref assignee) = filters.assignee {
            args.push("--assignee");
            assignee_arg = assignee.clone();
            args.push(&assignee_arg);
        }

        let stdout = self.run_br(&args).await?;
        let mut beads = Self::parse_beads(&stdout, "br ready --json")?;

        // Apply label exclusion filter (br CLI doesn't support this natively).
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
            .run_br(&["show", id_str, "--json"])
            .await
            .with_context(|| format!("br show {id_str} failed"))?;
        Self::parse_single_bead(&stdout, &format!("br show {id_str} --json"))
    }

    async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
        let id_str = id.as_ref();

        // CRITICAL: Verify the bead is actually claimable BEFORE attempting to claim.
        // This prevents duplicate dispatches where two workers race to claim the same
        // bead. Without this check, the second worker can overwrite the first's claim.
        // See bead bf-1ne6u for details.
        let bead_before = self.show(id).await?;
        if bead_before.status != crate::types::BeadStatus::Open {
            // Bead is already in progress - another worker won this race
            let claimed_by = bead_before
                .assignee
                .clone()
                .unwrap_or_else(|| "(unknown)".to_string());
            return Ok(ClaimResult::RaceLost { claimed_by });
        }
        if let Some(claimed_by) = bead_before.assignee {
            // Bead has a stale assignee - not claimable
            return Ok(ClaimResult::RaceLost { claimed_by });
        }

        // Attempt claim by setting status=in_progress and assignee.
        //
        // Routed through `bf batch` (op=update) rather than `bf update ...
        // --assignee`: bf 0.4.1 dropped --assignee from the `update`
        // subcommand entirely (bf-1hmey), but `batch`'s update op still
        // accepts id/status/assignee together.
        let batch_json = serde_json::to_string(&serde_json::json!([{
            "op": "update",
            "id": id_str,
            "status": "in_progress",
            "assignee": actor,
        }]))
        .context("failed to serialize claim batch payload")?;
        let (code, _stdout) = self
            .run_br_with_status(&["batch", "--json", &batch_json])
            .await
            .with_context(|| format!("br batch update {id_str} (claim) failed to spawn"))?;

        match code {
            0 => {
                // Verify we actually won by reading back the bead.
                let bead = self.show(id).await?;
                // Verify BOTH status and assignee to catch races
                if bead.status == crate::types::BeadStatus::InProgress
                    && bead.assignee.as_deref() == Some(actor)
                {
                    Ok(ClaimResult::Claimed(bead))
                } else if bead.assignee.as_deref() == Some(actor) {
                    // Assignee matches but status is wrong - still treat as claimed
                    // (this handles edge cases where status didn't update but assignee did)
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
            _ => Ok(ClaimResult::ClaimError {
                reason: format!("br batch update exited with code {code}"),
            }),
        }
    }

    async fn release(&self, id: &BeadId) -> Result<()> {
        let id_str = id.as_ref();
        // See claim() above: --assignee no longer exists on `bf update` (bf-1hmey).
        let batch_json = serde_json::to_string(&serde_json::json!([{
            "op": "update",
            "id": id_str,
            "status": "open",
            "assignee": "",
        }]))
        .context("failed to serialize release batch payload")?;
        self.run_br(&["batch", "--json", &batch_json])
            .await
            .with_context(|| format!("br batch release {id_str} failed"))?;
        Ok(())
    }

    async fn block(&self, id: &BeadId) -> Result<()> {
        let id_str = id.as_ref();
        self.run_br(&["update", id_str, "--status", "blocked"])
            .await
            .with_context(|| format!("br block {id_str} failed"))?;
        Ok(())
    }

    async fn clear_assignee(&self, id: &BeadId) -> Result<()> {
        let id_str = id.as_ref();
        // See claim() above: --assignee no longer exists on `bf update` (bf-1hmey).
        let batch_json = serde_json::to_string(&serde_json::json!([{
            "op": "update",
            "id": id_str,
            "assignee": "",
        }]))
        .context("failed to serialize clear_assignee batch payload")?;
        self.run_br(&["batch", "--json", &batch_json])
            .await
            .with_context(|| format!("br batch clear_assignee {id_str} failed"))?;
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        self.run_br(&["sync", "--flush-only"]).await?;
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
        self.run_br(&["label", "add", id_str, "--label", label])
            .await
            .with_context(|| format!("br label add {id_str} {label} failed"))?;
        Ok(())
    }

    async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
        let id_str = id.as_ref();
        self.run_br(&["label", "remove", id_str, "--label", label])
            .await
            .with_context(|| format!("br label remove {id_str} {label} failed"))?;
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
        let stdout = self.run_br(&arg_refs).await?;
        let id_str = stdout.trim();
        if id_str.is_empty() {
            bail!("br create returned empty ID");
        }
        Ok(BeadId::from(id_str))
    }

    async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
        // blocker_id blocks blocked_id (child blocks parent)
        // bf dep add <BLOCKER> --blocks <BLOCKED>
        // BLOCKED depends on BLOCKER, so blocked_id depends on blocker_id
        let blocker = blocker_id.as_ref();
        let blocked = blocked_id.as_ref();
        self.run_br(&["dep", "add", blocker, "--blocks", blocked])
            .await
            .with_context(|| format!("br dep add {blocker} --blocks {blocked} failed"))?;
        Ok(())
    }

    /// Crash-safe bead split via a single `bf batch` transaction.
    ///
    /// Creates every child, then links each as a blocker of `parent_id`, all
    /// inside one `BEGIN IMMEDIATE` transaction. A kill/OOM/eviction mid-split
    /// rolls the whole batch back — no orphaned children (plan.md Phase 5.3,
    /// Race 3). If `bf` is missing or the batch fails, we log and fall back to
    /// the historical non-atomic sequence, mirroring `run_bf_claim`'s degrade-
    /// gracefully behavior so this never becomes a hard dependency.
    async fn split_bead(
        &self,
        parent_id: &BeadId,
        children: &[NewChild<'_>],
    ) -> Result<Vec<BeadId>> {
        if children.is_empty() {
            return Ok(Vec::new());
        }

        // Build one atomic batch: N creates, then N dep_add_blocker ops linking
        // each freshly-created child (@0..@N-1) as a blocker of the parent. No
        // `close` op — a split parent stays open/blocked.
        let ops = build_split_batch_ops(parent_id, children);
        match serde_json::to_string(&ops) {
            Ok(ops_json) => match self.run_bf_batch(&ops_json).await {
                Ok(stdout) => {
                    // The batch committed atomically (bf exited 0). Trust it and
                    // return — we must NOT fall back here, or a parse hiccup
                    // would double-create the children that already exist.
                    let ids = parse_batch_created_ids(&stdout);
                    if ids.len() != children.len() {
                        tracing::warn!(
                            parent_id = %parent_id,
                            expected = children.len(),
                            parsed = ids.len(),
                            stdout = %stdout,
                            "bf batch mitosis committed but the child-id parse \
                             count mismatched; returning parsed ids as-is"
                        );
                    }
                    return Ok(ids);
                }
                Err(e) => {
                    // A non-zero exit / timeout / spawn failure means the batch
                    // rolled back (nothing was created), so retrying the
                    // sequential path is safe.
                    tracing::warn!(
                        parent_id = %parent_id,
                        error = %e,
                        "bf batch mitosis failed; falling back to sequential create+dep"
                    );
                }
            },
            Err(e) => {
                tracing::warn!(
                    parent_id = %parent_id,
                    error = %e,
                    "failed to serialize bf batch ops; falling back to sequential create+dep"
                );
            }
        }

        // Fallback: historical non-atomic sequence (same as the trait default).
        let mut created = Vec::with_capacity(children.len());
        for child in children {
            let child_id = self
                .create_bead(child.title, child.body, child.labels)
                .await
                .with_context(|| format!("failed to create child bead: {}", child.title))?;
            self.add_dependency(&child_id, parent_id)
                .await
                .with_context(|| {
                    format!("failed to add dependency: {child_id} blocks {parent_id}")
                })?;
            created.push(child_id);
        }
        Ok(created)
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
        // Use bf claim's atomic select-score-update to eliminate TOCTOU race.
        // bf claim performs scoring (downstream_impact + critical_float + priority)
        // and the UPDATE in a single BEGIN IMMEDIATE transaction, guaranteeing
        // that concurrent workers receive distinct beads.
        match self.run_bf_claim(actor).await {
            Ok(stdout) => {
                // bf claim returns JSON with bead_id or empty object for no candidates
                let trimmed = stdout.trim();
                if trimmed.is_empty() || trimmed == "{}" || trimmed == "null" {
                    return Ok(ClaimResult::NotClaimable {
                        reason: "no beads available".to_string(),
                    });
                }

                // Parse the JSON response from bf claim
                #[derive(serde::Deserialize)]
                struct BfClaimResponse {
                    bead_id: Option<String>,
                    #[allow(dead_code)]
                    assignee: Option<String>,
                }

                let response: BfClaimResponse = serde_json::from_str(trimmed)
                    .with_context(|| format!("bf claim returned invalid JSON: {}", trimmed))?;

                if let Some(bead_id) = response.bead_id {
                    // Fetch the full bead details
                    self.show(&BeadId::from(bead_id))
                        .await
                        .map(ClaimResult::Claimed)
                } else {
                    Ok(ClaimResult::NotClaimable {
                        reason: "no beads available".to_string(),
                    })
                }
            }
            Err(e) => {
                // If bf is not available, fall back to the old br-style pattern
                tracing::warn!(error = %e, "bf claim failed, falling back to br-style ready+claim");
                let filters = Filters::default();
                let mut candidates = self.ready(&filters).await?;
                // Filter to only Open beads with no assignee - prevents claiming in_progress beads
                candidates
                    .retain(|b| b.status == crate::types::BeadStatus::Open && b.assignee.is_none());
                if let Some(bead) = candidates.first() {
                    self.claim(&bead.id, actor).await
                } else {
                    Ok(ClaimResult::NotClaimable {
                        reason: "no beads available".to_string(),
                    })
                }
            }
        }
    }

    fn has_valid_store(&self) -> bool {
        has_valid_bead_store(&self.workspace)
    }
}

impl BrCliBeadStore {
    /// Parse `br doctor` output into a `RepairReport`.
    pub(super) fn parse_doctor_output(stdout: &str) -> RepairReport {
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

#[cfg(test)]
mod tests {
    use super::*;

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

    fn minimal_bead_json(id: &str, status: &str) -> String {
        format!(
            r#"{{"id":"{id}","title":"Test bead","description":"desc","priority":2,"status":"{status}","assignee":null,"source_repo":"/home/coding/NEEDLE","dependencies":[],"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}}"#
        )
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

    #[tokio::test]
    async fn br_cli_bead_store_ready_passes_explicit_limit() {
        // Verify that ready() passes an explicit limit of 10000
        let tmp_dir = tempfile::tempdir().unwrap();
        let workspace = tmp_dir.path();
        std::fs::create_dir_all(workspace.join(".beads")).unwrap();

        // Use test-specific args file in temp dir to avoid race conditions
        let args_file = tmp_dir.path().join("br-ready-args.txt");

        // Create a fake br that logs its arguments
        let fake_br = tmp_dir.path().join("fake-br-ready-limit");
        std::fs::write(
            &fake_br,
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
            &fake_br,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let store = BrCliBeadStore::new(
            fake_br.clone(),
            workspace.to_path_buf(),
            None, // model
            None, // harness
            None, // harness_version
        )
        .unwrap();
        let filters = Filters::default();

        let _ = store.ready(&filters).await;

        // Read back the arguments that were passed
        let args = std::fs::read_to_string(&args_file).unwrap();
        assert!(args.contains("--limit"), "ready() must pass --limit flag");
        assert!(args.contains("10000"), "ready() must pass limit of 10000");

        // Cleanup handled by tmp_dir drop
    }

    #[tokio::test]
    async fn br_cli_bead_store_list_all_passes_large_explicit_limit() {
        // Verify that list_all() passes an explicit limit of 999999 (not 0)
        let tmp_dir = tempfile::tempdir().unwrap();
        let workspace = tmp_dir.path();
        std::fs::create_dir_all(workspace.join(".beads")).unwrap();

        // Use test-specific args file in temp dir to avoid race conditions
        let args_file = tmp_dir.path().join("br-list-args.txt");

        // Create a fake br that logs its arguments
        let fake_br = tmp_dir.path().join("fake-br-list-limit");
        std::fs::write(
            &fake_br,
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
            &fake_br,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let store = BrCliBeadStore::new(
            fake_br.clone(),
            workspace.to_path_buf(),
            None, // model
            None, // harness
            None, // harness_version
        )
        .unwrap();

        let _ = store.list_all().await;

        // Read back the arguments that were passed
        let args = std::fs::read_to_string(&args_file).unwrap();
        assert!(
            args.contains("--limit"),
            "list_all() must pass --limit flag"
        );
        assert!(
            args.contains("999999"),
            "list_all() must pass limit of 999999"
        );
        assert!(
            !args.contains("--limit 0"),
            "list_all() must NOT pass limit of 0"
        );

        // Cleanup handled by tmp_dir drop
    }

    #[tokio::test]
    async fn br_cli_bead_store_ready_filters_by_exclude_ids() {
        use std::collections::HashSet;

        // Test that ready() filters out beads whose IDs are in exclude_ids
        let tmp_dir = tempfile::tempdir().unwrap();
        let workspace = tmp_dir.path();
        std::fs::create_dir_all(workspace.join(".beads")).unwrap();

        // Create a fake br that returns multiple beads
        let fake_br = tmp_dir.path().join("fake-br-ready-exclude");
        std::fs::write(
            &fake_br,
            r#"#!/bin/sh
echo '[{"id":"bf-abc","title":"Test bead ABC","description":"desc","priority":2,"status":"open","assignee":null,"source_repo":"/home/coding/NEEDLE","dependencies":[],"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"},{"id":"bf-def","title":"Test bead DEF","description":"desc","priority":2,"status":"open","assignee":null,"source_repo":"/home/coding/NEEDLE","dependencies":[],"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}]'
"#,
        )
        .unwrap();
        std::fs::set_permissions(
            &fake_br,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let store = BrCliBeadStore::new(fake_br.clone(), workspace.to_path_buf(), None, None, None)
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
}
