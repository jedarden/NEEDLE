//! Descriptor-driven bead CLI command engine.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use async_trait::async_trait;
use serde::de::{self, SeqAccess, Visitor};
use serde::Deserialize;
use std::fmt;

use crate::types::{Bead, BeadId, BeadStatus, ClaimResult};

use super::{
    execute_create_id_strategy, execute_labels_strategy, spawn_with_etxtbsy_retry_child,
    validate_strategy_name, BeadBackend, BeadOperationSpec, BeadStore, ClaimStrategy, Filters,
    NewChild, ParseShape, ParsedStrategy, RepairReport,
};

const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// One descriptor-bound CLI store. The descriptor and binary are inseparable.
pub struct CliBeadStore {
    backend: BeadBackend,
    binary: PathBuf,
    workspace: PathBuf,
    model: Option<String>,
    harness: Option<String>,
    harness_version: Option<String>,
}

impl CliBeadStore {
    pub fn new(
        backend: BeadBackend,
        binary: PathBuf,
        workspace: PathBuf,
        model: Option<String>,
        harness: Option<String>,
        harness_version: Option<String>,
    ) -> Result<Self> {
        backend.validate(Path::new("<resolved-backend>"))?;
        if !binary.is_file() {
            bail!("bead backend binary not found at {}", binary.display());
        }
        Ok(Self {
            backend,
            binary,
            workspace,
            model,
            harness,
            harness_version,
        })
    }

    pub fn backend(&self) -> &BeadBackend {
        &self.backend
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    fn operation(&self, name: &str) -> Result<&BeadOperationSpec> {
        self.backend.operations.get(name).ok_or_else(|| {
            anyhow::anyhow!(
                "backend '{}' does not define operation '{}'",
                self.backend.name,
                name
            )
        })
    }

    /// Render argv, removing an optional flag plus placeholder when its value
    /// is absent. Required embedded placeholders fail explicitly.
    pub fn render_operation(
        &self,
        name: &str,
        values: &HashMap<&str, String>,
    ) -> Result<Vec<String>> {
        let spec = self.operation(name)?;
        let mut argv = Vec::with_capacity(spec.argv.len());
        for template in &spec.argv {
            let names = placeholders(template)?;
            if names.is_empty() {
                argv.push(template.clone());
                continue;
            }
            let mut rendered = template.clone();
            let mut omit = false;
            for placeholder in names {
                let value = self
                    .implicit_value(&placeholder)
                    .or_else(|| values.get(placeholder.as_str()).cloned());
                match value {
                    Some(value) if !value.is_empty() => {
                        rendered = rendered.replace(&format!("{{{placeholder}}}"), &value);
                    }
                    _ if template == &format!("{{{placeholder}}}")
                        && is_optional_placeholder(&placeholder) =>
                    {
                        omit = true;
                    }
                    _ => bail!(
                        "backend '{}' operation '{}' requires placeholder '{{{}}}'",
                        self.backend.name,
                        name,
                        placeholder
                    ),
                }
            }
            if omit {
                if argv
                    .last()
                    .is_some_and(|argument| argument.starts_with('-'))
                {
                    argv.pop();
                }
            } else {
                argv.push(rendered);
            }
        }
        Ok(argv)
    }

    fn implicit_value(&self, name: &str) -> Option<String> {
        match name {
            "model" => self.model.clone(),
            "harness" => self.harness.clone(),
            "harness_version" => self.harness_version.clone(),
            _ => None,
        }
    }

    /// Check if a specific quirk applies to the current backend version.
    /// Returns true only if the quirk exists and its version_requirement matches
    /// the backend's verified version (or has no version requirement).
    fn has_quirk(&self, quirk_name: &str) -> bool {
        self.backend
            .quirks
            .iter()
            .any(|q| q.name == quirk_name && self.quirk_version_matches(q))
    }

    /// Check if a quirk's version requirement matches the backend's verified version.
    fn quirk_version_matches(&self, quirk: &crate::bead_store::backend::BeadBackendQuirk) -> bool {
        match &quirk.version_requirement {
            None => true, // No version requirement means it always applies
            Some(requirement) => {
                // Parse the version requirement and check against verified version
                // For now, since we only have "<= 0.2.0" and verified_against is "bf 0.4.1",
                // the quirk should NOT apply (0.4.1 > 0.2.0)
                // This is a simplified check - a full implementation would use semver parsing
                let verified = &self.backend.verified_against;
                // Extract version number (e.g., "bf 0.4.1" -> "0.4.1")
                let version_part = verified.split_whitespace().nth(1).unwrap_or(verified);
                // Simple check: if requirement says "<= 0.2.0" and we're at "0.4.1", it doesn't match
                if let Some(req_version) = requirement.strip_prefix("<=") {
                    let req_version = req_version.trim();
                    // Very basic version comparison - should use semver crate in production
                    // For now: "0.4.1" > "0.2.0" means quirk doesn't apply (return false)
                    // If current version > required version, quirk does NOT match
                    version_part <= req_version
                } else {
                    // Unknown requirement format - conservatively apply the quirk
                    true
                }
            }
        }
    }

    pub async fn run_operation(
        &self,
        name: &str,
        values: &HashMap<&str, String>,
    ) -> Result<String> {
        let args = self.render_operation(name, values)?;
        let timeout_secs = self
            .operation(name)?
            .timeout_secs
            .unwrap_or(DEFAULT_TIMEOUT_SECS);
        self.run_argv(name, &args, timeout_secs).await
    }

    pub(super) async fn run_argv(
        &self,
        name: &str,
        args: &[String],
        timeout_secs: u64,
    ) -> Result<String> {
        let binary = self.binary.clone();
        let workspace = self.workspace.clone();
        let owned_args = args.to_vec();
        let child = spawn_with_etxtbsy_retry_child(
            || async {
                let mut command = tokio::process::Command::new(&binary);
                command
                    .args(&owned_args)
                    .current_dir(&workspace)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true);
                command.spawn()
            },
            5,
            20,
        )
        .await
        .with_context(|| {
            format!(
                "failed to spawn backend '{}' operation '{}' using {}",
                self.backend.name,
                name,
                self.binary.display()
            )
        })?;
        let output =
            tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output())
                .await
                .with_context(|| {
                    format!(
                        "backend '{}' operation '{}' timed out after {}s",
                        self.backend.name, name, timeout_secs
                    )
                })?
                .with_context(|| {
                    format!(
                        "backend '{}' operation '{}' failed",
                        self.backend.name, name
                    )
                })?;
        let stdout = String::from_utf8(output.stdout).with_context(|| {
            format!(
                "backend '{}' operation '{}' stdout was not UTF-8",
                self.backend.name, name
            )
        })?;
        if !output.status.success() {
            bail!(
                "backend '{}' operation '{}' exited with code {}: {}",
                self.backend.name,
                name,
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(stdout)
    }

    pub fn parse_beads(&self, name: &str, output: &str) -> Result<Vec<Bead>> {
        let shape = self.operation(name)?.parse.ok_or_else(|| {
            anyhow::anyhow!(
                "backend '{}' operation '{}' has no declared parse shape",
                self.backend.name,
                name
            )
        })?;
        parse_beads(shape, output).with_context(|| {
            format!(
                "failed to parse backend '{}' operation '{}' as {:?}",
                self.backend.name, name, shape
            )
        })
    }

    fn parse_beads_with_claim_history(
        &self,
        name: &str,
        output: &str,
    ) -> Result<Vec<(Bead, Option<u32>)>> {
        let shape = self.operation(name)?.parse.ok_or_else(|| {
            anyhow::anyhow!(
                "backend '{}' operation '{}' has no declared parse shape",
                self.backend.name,
                name
            )
        })?;
        parse_beads_with_claim_history(shape, output).with_context(|| {
            format!(
                "failed to parse backend '{}' operation '{}' with claim history as {:?}",
                self.backend.name, name, shape
            )
        })
    }

    fn strategy(&self, name: &str) -> Result<ParsedStrategy> {
        let strategy = self.operation(name)?.strategy.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "backend '{}' operation '{}' has no strategy",
                self.backend.name,
                name
            )
        })?;
        validate_strategy_name(Path::new("<resolved-backend>"), name, strategy)
    }

    async fn mutate(&self, name: &str, values: &[(&str, String)]) -> Result<()> {
        let values = values.iter().cloned().collect();
        self.run_operation(name, &values).await?;
        Ok(())
    }

    async fn claim_auto_inner(&self, actor: &str) -> Result<ClaimResult> {
        let values = HashMap::from([("actor", actor.to_string())]);
        let stdout = self.run_operation("claim_auto", &values).await?;
        let json: serde_json::Value = serde_json::from_str(&stdout)
            .with_context(|| format!("{} claim returned invalid JSON", self.backend.name))?;
        let bead_id = json
            .get("bead_id")
            .or_else(|| json.pointer("/data/bead_id"))
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty());
        match bead_id {
            Some(id) => Ok(ClaimResult::Claimed(self.show(&BeadId::from(id)).await?)),
            None => Ok(ClaimResult::NotClaimable {
                reason: "no beads available".to_string(),
            }),
        }
    }

    /// Add the bead-rs manual-block marker to the common bead projection.
    ///
    /// bead-rs deliberately keeps `manual_blocked` separate from the base
    /// `status` in `list --json`, while `why --json` exposes the effective
    /// blocking flag.  Pluck's starvation pass needs that flag when it joins
    /// the full inventory back to the ready frontier.  Represent it as an
    /// internal label so the common `Bead` model remains compatible with both
    /// backends and existing callers.
    async fn enrich_manual_block_labels(&self, beads: &mut [Bead]) {
        if self.backend.name != "bead-rs" {
            return;
        }

        for bead in beads.iter_mut() {
            if bead.status != BeadStatus::Open {
                continue;
            }

            let args = vec![
                "why".to_string(),
                "--id".to_string(),
                bead.id.to_string(),
                "--json".to_string(),
            ];
            let output = match self.run_argv("why", &args, DEFAULT_TIMEOUT_SECS).await {
                Ok(output) => output,
                Err(error) => {
                    tracing::debug!(
                        bead_id = %bead.id,
                        error = %error,
                        "Unable to inspect bead-rs manual block flag"
                    );
                    continue;
                }
            };

            let Ok(value) = serde_json::from_str::<serde_json::Value>(output.trim()) else {
                tracing::debug!(
                    bead_id = %bead.id,
                    "bead-rs why response was not JSON"
                );
                continue;
            };
            let value = value
                .as_array()
                .and_then(|items| items.first())
                .unwrap_or(&value);
            if value
                .get("manual_blocked")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                bead.labels.push("manual_blocked".to_string());
            }
        }
    }
}

#[async_trait]
impl BeadStore for CliBeadStore {
    fn is_corruption_error(&self, message: &str) -> bool {
        self.backend
            .error_contains_any(message, &self.backend.error_markers.corruption)
    }

    fn is_lock_error(&self, message: &str) -> bool {
        self.backend
            .error_contains_any(message, &self.backend.error_markers.lock)
    }

    fn is_sync_conflict(&self, message: &str) -> bool {
        self.backend
            .error_contains_any(message, &self.backend.error_markers.sync_conflict)
    }

    async fn ready(&self, filters: &Filters) -> Result<Vec<Bead>> {
        // Apply limit workaround only if backend has the quirk
        let limit = if self.has_quirk("limit_zero_returns_empty_set") {
            "999999"
        } else {
            "0" // Use 0 for backends that handle --limit correctly
        };
        let values = HashMap::from([("limit", limit.to_string())]);
        let stdout = self.run_operation("ready", &values).await?;
        let mut beads = self.parse_beads("ready", &stdout)?;
        if let Some(assignee) = &filters.assignee {
            beads.retain(|bead| bead.assignee.as_ref() == Some(assignee));
        }
        beads.retain(|bead| {
            !filters.exclude_ids.contains(&bead.id)
                && !bead
                    .labels
                    .iter()
                    .any(|label| filters.exclude_labels.contains(label))
        });
        Ok(beads)
    }

    async fn list_all(&self) -> Result<Vec<Bead>> {
        // Apply limit workaround only if backend has the quirk
        let limit = if self.has_quirk("limit_zero_returns_empty_set") {
            "999999"
        } else {
            "0" // Use 0 for backends that handle --limit correctly
        };
        let values = HashMap::from([("limit", limit.to_string())]);
        let stdout = self.run_operation("list_all", &values).await?;
        self.parse_beads("list_all", &stdout)
    }

    async fn starvation_inventory(&self) -> Result<Vec<Bead>> {
        let limit = if self.has_quirk("limit_zero_returns_empty_set") {
            "999999"
        } else {
            "0"
        };
        let values = HashMap::from([("limit", limit.to_string())]);
        let stdout = self.run_operation("list_all", &values).await?;
        let mut beads = self.parse_beads("list_all", &stdout)?;
        self.enrich_manual_block_labels(&mut beads).await;
        Ok(beads)
    }

    async fn show(&self, id: &BeadId) -> Result<Bead> {
        let values = HashMap::from([("id", id.to_string())]);
        let stdout = self.run_operation("show", &values).await?;
        self.parse_beads("show", &stdout)?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("{} show {} returned no bead", self.backend.name, id))
    }

    async fn show_with_claim_history(&self, id: &BeadId) -> Result<(Bead, Option<u32>)> {
        let values = HashMap::from([("id", id.to_string())]);
        let stdout = self.run_operation("show", &values).await?;
        let (bead, claim_events) = self
            .parse_beads_with_claim_history("show", &stdout)?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("{} show {} returned no bead", self.backend.name, id))?;
        Ok((bead, claim_events))
    }

    async fn notes(&self, id: &BeadId) -> Result<Option<String>> {
        let values = HashMap::from([("id", id.to_string())]);
        let stdout = self.run_operation("show", &values).await?;
        let value: serde_json::Value = serde_json::from_str(stdout.trim())?;
        let object = value
            .as_array()
            .and_then(|items| items.first())
            .unwrap_or(&value);
        Ok(object
            .get("notes")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string))
    }

    async fn claim_status(&self, id: &BeadId) -> Result<crate::types::ClaimStatus> {
        let values = HashMap::from([("id", id.to_string())]);
        let raw = self.run_operation("show", &values).await?;
        let value: serde_json::Value = serde_json::from_str(raw.trim())?;
        let value = value
            .as_array()
            .and_then(|items| items.first())
            .unwrap_or(&value);

        let status_str = value
            .get("status")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("backend show response omitted status"))?;
        let status =
            serde_json::from_str::<BeadStatus>(&serde_json::to_string(status_str).unwrap())
                .with_context(|| format!("failed to parse status: {}", status_str))?;

        let assignee = value
            .get("assignee")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let revision = value.get("revision").and_then(|v| v.as_u64());

        Ok(crate::types::ClaimStatus {
            status,
            assignee,
            revision,
        })
    }

    async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
        if matches!(
            self.strategy("claim")?,
            ParsedStrategy::Claim(ClaimStrategy::BatchOp)
        ) {
            return self.claim_via_batch(id, actor).await;
        }
        let shown = self.show(id).await?;
        if shown.status != BeadStatus::Open {
            return Ok(ClaimResult::NotClaimable {
                reason: format!("bead is {}, not open", shown.status),
            });
        }
        if let Some(claimed_by) = shown.assignee {
            return Ok(ClaimResult::RaceLost { claimed_by });
        }

        // bead-rs exposes its monotonic revision in raw `show` JSON. Read the
        // state and revision together, then guard the update with that revision.
        let show_values = HashMap::from([("id", id.to_string())]);
        let raw = self.run_operation("show", &show_values).await?;
        let value: serde_json::Value = serde_json::from_str(raw.trim())?;
        let value = value
            .as_array()
            .and_then(|items| items.first())
            .unwrap_or(&value);
        if value.get("status").and_then(|v| v.as_str()) != Some("open") {
            return Ok(ClaimResult::NotClaimable {
                reason: "bead changed state before claim".to_string(),
            });
        }
        if let Some(claimed_by) = value.get("assignee").and_then(|v| v.as_str()) {
            if !claimed_by.is_empty() {
                return Ok(ClaimResult::RaceLost {
                    claimed_by: claimed_by.to_string(),
                });
            }
        }
        let revision = value
            .get("revision")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("bead-rs show response omitted revision"))?;
        let args = vec![
            "update".to_string(),
            id.to_string(),
            "--status".to_string(),
            "in_progress".to_string(),
            "--assignee".to_string(),
            actor.to_string(),
            "--if-revision".to_string(),
            revision.to_string(),
        ];
        match self.run_argv("claim", &args, DEFAULT_TIMEOUT_SECS).await {
            Ok(_) => Ok(ClaimResult::Claimed(self.show(id).await?)),
            Err(error) if error.to_string().contains("code 4") => {
                let latest = self.show(id).await?;
                match latest.assignee {
                    Some(claimed_by) => Ok(ClaimResult::RaceLost { claimed_by }),
                    None => Ok(ClaimResult::NotClaimable {
                        reason: "bead changed during claim".to_string(),
                    }),
                }
            }
            Err(error) => Err(error),
        }
    }

    async fn claim_auto(&self, actor: &str) -> Result<ClaimResult> {
        self.claim_auto_inner(actor).await
    }

    async fn release(&self, id: &BeadId) -> Result<()> {
        self.mutate("release", &[("id", id.to_string())]).await
    }
    async fn block(&self, id: &BeadId) -> Result<()> {
        self.mutate("block", &[("id", id.to_string())]).await
    }
    async fn clear_assignee(&self, id: &BeadId) -> Result<()> {
        self.mutate("clear_assignee", &[("id", id.to_string())])
            .await
    }
    async fn flush(&self) -> Result<()> {
        self.mutate("flush", &[]).await
    }
    async fn reopen(&self, id: &BeadId) -> Result<()> {
        self.mutate("reopen", &[("id", id.to_string())]).await
    }
    async fn close(&self, id: &BeadId, reason: &str) -> Result<()> {
        self.mutate(
            "close",
            &[("id", id.to_string()), ("reason", reason.to_string())],
        )
        .await
    }
    async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
        Ok(self.show(id).await?.labels)
    }
    async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
        self.mutate(
            "label_add",
            &[("id", id.to_string()), ("label", label.to_string())],
        )
        .await
    }
    async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
        self.mutate(
            "label_remove",
            &[("id", id.to_string()), ("label", label.to_string())],
        )
        .await
    }

    async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId> {
        let values = HashMap::from([("title", title.to_string()), ("body", body.to_string())]);
        let mut args = self.render_operation("create", &values)?;
        let labels_strategy = match self.strategy("labels")? {
            ParsedStrategy::Labels(strategy) => strategy,
            _ => bail!(
                "backend '{}' has invalid labels strategy",
                self.backend.name
            ),
        };
        args.extend(execute_labels_strategy(labels_strategy, labels));
        let stdout = self.run_argv("create", &args, DEFAULT_TIMEOUT_SECS).await?;
        let id_strategy = match self.strategy("create_id")? {
            ParsedStrategy::CreateId(strategy) => strategy,
            _ => bail!(
                "backend '{}' has invalid create_id strategy",
                self.backend.name
            ),
        };
        Ok(BeadId::from(execute_create_id_strategy(
            id_strategy,
            &stdout,
        )?))
    }

    async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
        self.mutate(
            "dep_add",
            &[
                ("blocker", blocker_id.to_string()),
                ("blocked", blocked_id.to_string()),
            ],
        )
        .await
    }

    async fn split_bead(
        &self,
        parent_id: &BeadId,
        children: &[NewChild<'_>],
    ) -> Result<Vec<BeadId>> {
        if matches!(
            self.strategy("split")?,
            ParsedStrategy::Split(super::SplitStrategy::TransactionalBatch)
        ) {
            if children.is_empty() {
                return Ok(Vec::new());
            }
            let mut operations = Vec::with_capacity(children.len() * 2);
            for child in children {
                operations.push(serde_json::json!({
                    "op": "create",
                    "title": child.title,
                    "description": child.body,
                    "labels": child.labels,
                }));
            }
            for index in 0..children.len() {
                operations.push(serde_json::json!({
                    "op": "dep_add_blocker",
                    "id": parent_id.as_ref(),
                    "blocker": format!("@{index}"),
                }));
            }
            let payload = serde_json::to_string(&operations)?;
            let args = vec!["batch".to_string(), "--json".to_string(), payload];
            let stdout = self.run_argv("split", &args, DEFAULT_TIMEOUT_SECS).await?;
            let ids = parse_batch_created_ids(&stdout);
            if ids.len() != children.len() {
                bail!(
                    "backend '{}' committed split for {} children but returned {} IDs",
                    self.backend.name,
                    children.len(),
                    ids.len()
                );
            }
            return Ok(ids);
        }

        let mut ids = Vec::with_capacity(children.len());
        for child in children {
            let id = self
                .create_bead(child.title, child.body, child.labels)
                .await?;
            self.add_dependency(&id, parent_id).await?;
            ids.push(id);
        }
        Ok(ids)
    }

    async fn remove_dependency(&self, blocked_id: &BeadId, blocker_id: &BeadId) -> Result<()> {
        self.mutate(
            "dep_remove",
            &[
                ("blocked", blocked_id.to_string()),
                ("blocker", blocker_id.to_string()),
            ],
        )
        .await
    }
    async fn doctor_repair(&self) -> Result<RepairReport> {
        let stdout = self.run_operation("doctor_repair", &HashMap::new()).await?;
        Ok(parse_doctor_output(&stdout))
    }
    async fn doctor_check(&self) -> Result<RepairReport> {
        let stdout = self.run_operation("doctor_check", &HashMap::new()).await?;
        Ok(parse_doctor_output(&stdout))
    }
    async fn full_rebuild(&self) -> Result<()> {
        if !matches!(self.backend.name.as_str(), "bead-rs") {
            bail!(
                "descriptor-driven full rebuild is not enabled for backend '{}'",
                self.backend.name
            );
        }

        let checkpoint = self.workspace.join(".beads/checkpoint");
        if !checkpoint.exists() {
            bail!(
                "refusing to rebuild backend '{}' without checkpoint {}",
                self.backend.name,
                checkpoint.display()
            );
        }

        // Keep the complete SQLite file set recoverable until import and
        // doctor verification both succeed. A failed import must never turn a
        // repairable store into an empty or partially initialized one.
        let db_paths = [
            self.workspace.join(".beads/beads.db"),
            self.workspace.join(".beads/beads.db-wal"),
            self.workspace.join(".beads/beads.db-shm"),
        ];
        let backup_paths = db_paths
            .iter()
            .map(|path| {
                path.with_extension(format!(
                    "{}needle-rebuild-backup",
                    path.extension()
                        .and_then(|extension| extension.to_str())
                        .map(|extension| format!("{extension}."))
                        .unwrap_or_default()
                ))
            })
            .collect::<Vec<_>>();
        for backup in &backup_paths {
            if backup.exists() {
                bail!(
                    "refusing to overwrite prior rebuild backup {}",
                    backup.display()
                );
            }
        }
        for (path, backup) in db_paths.iter().zip(&backup_paths) {
            if path.exists() {
                if let Err(preserve_error) = tokio::fs::rename(path, backup).await {
                    for (original, preserved) in db_paths.iter().zip(&backup_paths) {
                        if preserved.exists() {
                            let _ = tokio::fs::rename(preserved, original).await;
                        }
                    }
                    return Err(preserve_error).with_context(|| {
                        format!(
                            "failed to preserve {} as {}; prior files were restored",
                            path.display(),
                            backup.display()
                        )
                    });
                }
            }
        }

        let rebuild = async {
            self.run_argv(
                "full_rebuild.init",
                &["init".to_string()],
                DEFAULT_TIMEOUT_SECS,
            )
            .await?;
            let checkpoint = checkpoint
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("checkpoint path is not valid UTF-8"))?;
            let args = vec![
                "sync".to_string(),
                "import-only".to_string(),
                "--input".to_string(),
                checkpoint.to_string(),
                "--restore-into-empty".to_string(),
                "--actor".to_string(),
                "needle".to_string(),
            ];
            self.run_argv("full_rebuild.import", &args, DEFAULT_TIMEOUT_SECS)
                .await?;
            let report = self.doctor_check().await?;
            if !report.warnings.is_empty() {
                bail!(
                    "database still has issues after rebuild: {:?}",
                    report.warnings
                );
            }
            Ok(())
        }
        .await;

        if let Err(rebuild_error) = rebuild {
            let mut rollback_errors = Vec::new();
            for path in &db_paths {
                if path.exists() {
                    if let Err(error) = tokio::fs::remove_file(path).await {
                        rollback_errors.push(format!(
                            "failed to remove incomplete rebuild {}: {error}",
                            path.display()
                        ));
                    }
                }
            }
            for (path, backup) in db_paths.iter().zip(&backup_paths) {
                if backup.exists() {
                    if let Err(error) = tokio::fs::rename(backup, path).await {
                        rollback_errors.push(format!(
                            "failed to restore {} from {}: {error}",
                            path.display(),
                            backup.display()
                        ));
                    }
                }
            }
            if !rollback_errors.is_empty() {
                bail!(
                    "rebuild failed ({rebuild_error:#}); rollback was incomplete: {}",
                    rollback_errors.join("; ")
                );
            }
            return Err(rebuild_error.context("rebuild failed; original database restored"));
        }

        for backup in &backup_paths {
            if backup.exists() {
                tokio::fs::remove_file(backup).await.with_context(|| {
                    format!(
                        "rebuild succeeded but failed to remove {}",
                        backup.display()
                    )
                })?;
            }
        }
        Ok(())
    }
    fn has_valid_store(&self) -> bool {
        self.workspace.join(".beads").is_dir()
    }
}

impl CliBeadStore {
    async fn claim_via_batch(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
        let shown = self.show(id).await?;
        if shown.status != BeadStatus::Open {
            return Ok(ClaimResult::NotClaimable {
                reason: format!("bead is {}, not open", shown.status),
            });
        }
        if let Some(claimed_by) = shown.assignee {
            return Ok(ClaimResult::RaceLost { claimed_by });
        }

        // bf 0.4.1 removed --assignee from `update`, but its transactional
        // batch update operation still accepts status and assignee together.
        self.run_bf_update_batch(id, Some("in_progress"), Some(actor))
            .await
            .with_context(|| format!("bf batch update {id} (claim) failed"))?;

        let claimed = self.show(id).await?;
        if claimed.assignee.as_deref() == Some(actor) {
            Ok(ClaimResult::Claimed(claimed))
        } else {
            Ok(ClaimResult::RaceLost {
                claimed_by: claimed.assignee.unwrap_or_else(|| "(unknown)".to_string()),
            })
        }
    }

    async fn run_bf_update_batch(
        &self,
        id: &BeadId,
        status: Option<&str>,
        assignee: Option<&str>,
    ) -> Result<()> {
        let mut operation = serde_json::Map::new();
        operation.insert("op".to_string(), serde_json::json!("update"));
        operation.insert("id".to_string(), serde_json::json!(id.as_ref()));
        if let Some(status) = status {
            operation.insert("status".to_string(), serde_json::json!(status));
        }
        if let Some(assignee) = assignee {
            operation.insert("assignee".to_string(), serde_json::json!(assignee));
        }
        let payload = serde_json::to_string(&vec![serde_json::Value::Object(operation)])?;
        self.run_argv(
            "batch_update",
            &["batch".to_string(), "--json".to_string(), payload],
            DEFAULT_TIMEOUT_SECS,
        )
        .await?;
        Ok(())
    }

    /// Attempt public-CLI recovery, escalating from doctor repair to the
    /// backend-specific checkpoint rebuild path.
    pub async fn recover_db(&self) -> super::RecoveryOutcome {
        match self.doctor_repair().await {
            Ok(report) => super::RecoveryOutcome::Repaired(report),
            Err(repair_error) => match self.full_rebuild().await {
                Ok(()) => super::RecoveryOutcome::Rebuilt,
                Err(rebuild_error) => {
                    super::RecoveryOutcome::Failed(rebuild_error.context(format!(
                        "{} doctor repair failed before rebuild: {repair_error:#}",
                        self.backend.name
                    )))
                }
            },
        }
    }
}

fn parse_batch_created_ids(output: &str) -> Vec<BeadId> {
    output
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("[op ")?;
            let (_, tail) = rest.split_once(']')?;
            let id = tail.trim().strip_prefix("ok:")?.trim();
            (!id.is_empty()).then(|| BeadId::from(id))
        })
        .collect()
}

fn parse_doctor_output(output: &str) -> RepairReport {
    let mut report = RepairReport::default();
    for line in output.lines() {
        if let Some(message) = line.strip_prefix("WARN ") {
            if !message.contains("sqlite3 not available") && !message.contains("recovery_artifacts")
            {
                report.warnings.push(message.to_string());
            }
        } else if let Some(message) = line.strip_prefix("FIXED ") {
            report.fixed.push(message.to_string());
        }
    }
    report
}

fn is_optional_placeholder(name: &str) -> bool {
    matches!(name, "model" | "harness" | "harness_version" | "limit")
}

fn placeholders(template: &str) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let mut remainder = template;
    while let Some(open) = remainder.find('{') {
        let after_open = &remainder[open + 1..];
        let close = after_open
            .find('}')
            .ok_or_else(|| anyhow::anyhow!("malformed placeholder in {template:?}"))?;
        names.push(after_open[..close].to_string());
        remainder = &after_open[close + 1..];
    }
    if remainder.contains('}') {
        bail!("malformed placeholder in {template:?}");
    }
    Ok(names)
}

fn parse_beads(shape: ParseShape, output: &str) -> Result<Vec<Bead>> {
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }
    match shape {
        ParseShape::JsonArray => serde_json::from_str(output).context("invalid JSON array"),
        ParseShape::JsonObject => {
            if let Ok(bead) = serde_json::from_str::<Bead>(output) {
                return Ok(vec![bead]);
            }
            let beads: Vec<Bead> = serde_json::from_str(output).context("invalid JSON object")?;
            if beads.len() != 1 {
                bail!("expected one JSON object, found {}", beads.len());
            }
            Ok(beads)
        }
        ParseShape::JsonLines => {
            // bead-rs emits a JSON array (`[]`) for an empty result set but
            // newline-delimited objects otherwise. Accept both shapes: treating
            // `[]` as a parse error made Explore skip the whole workspace and
            // Pluck fail its ready query instead of reading "no beads".
            let mut beads = Vec::new();
            for line in output.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if line.starts_with('[') {
                    let batch: Vec<Bead> =
                        serde_json::from_str(line).context("invalid JSON array line")?;
                    beads.extend(batch);
                    continue;
                }
                beads.push(serde_json::from_str(line).context("invalid JSON line")?);
            }
            Ok(beads)
        }
        ParseShape::BareId | ParseShape::None => {
            bail!("parse shape {shape:?} cannot produce bead records")
        }
    }
}

#[derive(Deserialize)]
struct BeadWithClaimHistory {
    #[serde(flatten)]
    bead: Bead,
    #[serde(default, deserialize_with = "count_claim_events")]
    events: Option<u32>,
}

#[derive(Deserialize)]
struct EventTypeOnly {
    #[serde(rename = "type", alias = "event_type", default)]
    event_type: Option<String>,
}

fn count_claim_events<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct ClaimEventVisitor;

    impl<'de> Visitor<'de> for ClaimEventVisitor {
        type Value = Option<u32>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an array of bead events")
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut count = 0u32;
            while let Some(event) = sequence.next_element::<EventTypeOnly>()? {
                if matches!(
                    event.event_type.as_deref(),
                    Some("claimed") | Some("assignee_changed") | Some("claim")
                ) {
                    count = count.saturating_add(1);
                }
            }
            Ok(Some(count))
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(Some(0))
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(Some(0))
        }
    }

    deserializer.deserialize_any(ClaimEventVisitor)
}

fn parse_beads_with_claim_history(
    shape: ParseShape,
    output: &str,
) -> Result<Vec<(Bead, Option<u32>)>> {
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }

    let records = match shape {
        ParseShape::JsonArray => serde_json::from_str::<Vec<BeadWithClaimHistory>>(output)?,
        ParseShape::JsonObject => {
            if let Ok(record) = serde_json::from_str::<BeadWithClaimHistory>(output) {
                vec![record]
            } else {
                serde_json::from_str::<Vec<BeadWithClaimHistory>>(output)?
            }
        }
        ParseShape::JsonLines => {
            // Same dual shape as `parse_beads`: bead-rs emits `[]` when the
            // result set is empty and newline-delimited objects otherwise.
            let mut records = Vec::new();
            for line in output.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if line.starts_with('[') {
                    records.extend(serde_json::from_str::<Vec<BeadWithClaimHistory>>(line)?);
                    continue;
                }
                records.push(serde_json::from_str::<BeadWithClaimHistory>(line)?);
            }
            records
        }
        ParseShape::BareId | ParseShape::None => {
            bail!("parse shape {shape:?} cannot produce bead records")
        }
    };

    Ok(records
        .into_iter()
        .map(|record| (record.bead, record.events))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{parse_beads, parse_beads_with_claim_history, CliBeadStore, ParseShape};
    use crate::bead_store::{builtin_bead_backends, BeadStore};
    use std::collections::HashMap;

    #[test]
    fn claim_history_parser_counts_only_claim_mutations() {
        let json = r#"[
            {
                "id": "bf-test",
                "title": "history",
                "priority": 1,
                "status": "open",
                "created_at": "2026-08-13T00:00:00Z",
                "events": [
                    {"type": "created"},
                    {"type": "claimed"},
                    {"type": "assignee_changed"},
                    {"type": "commented"},
                    {"type": "claim"}
                ]
            }
        ]"#;

        let parsed = parse_beads_with_claim_history(ParseShape::JsonArray, json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0.id.as_ref(), "bf-test");
        assert_eq!(parsed[0].1, Some(3));
    }

    #[test]
    fn claim_history_parser_defaults_when_events_are_omitted() {
        let json = r#"{
            "id": "bf-test",
            "title": "history",
            "priority": 1,
            "status": "open",
            "created_at": "2026-08-13T00:00:00Z"
        }"#;

        let parsed = parse_beads_with_claim_history(ParseShape::JsonObject, json).unwrap();
        assert_eq!(parsed[0].1, None);
    }

    #[test]
    fn json_lines_parser_reads_empty_array_as_no_beads() {
        // bead-rs prints `[]` (a JSON array) when a query matches nothing, but
        // newline-delimited objects when it matches something. Treating `[]` as
        // a parse error made Explore skip the workspace and Pluck fail outright.
        let parsed = parse_beads(ParseShape::JsonLines, "[]").unwrap();
        assert!(parsed.is_empty());

        let parsed = parse_beads(ParseShape::JsonLines, "  []  \n").unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn json_lines_parser_still_reads_newline_delimited_objects() {
        let json = concat!(
            r#"{"id":"bf-one","title":"first","priority":1,"status":"open","created_at":"2026-08-13T00:00:00Z"}"#,
            "\n",
            r#"{"id":"bf-two","title":"second","priority":2,"status":"open","created_at":"2026-08-13T00:00:00Z"}"#,
            "\n"
        );

        let parsed = parse_beads(ParseShape::JsonLines, json).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id.as_ref(), "bf-one");
        assert_eq!(parsed[1].id.as_ref(), "bf-two");
    }

    #[test]
    fn json_lines_parser_reads_populated_array_line() {
        let json = r#"[{"id":"bf-one","title":"first","priority":1,"status":"open","created_at":"2026-08-13T00:00:00Z"}]"#;

        let parsed = parse_beads(ParseShape::JsonLines, json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id.as_ref(), "bf-one");
    }

    #[test]
    fn claim_history_parser_reads_empty_array_as_no_beads() {
        let parsed = parse_beads_with_claim_history(ParseShape::JsonLines, "[]").unwrap();
        assert!(parsed.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_native_rebuild_restores_original_database() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let beads = workspace.path().join(".beads");
        std::fs::create_dir_all(beads.join("checkpoint")).unwrap();
        std::fs::write(beads.join("checkpoint/forensic.jsonl"), "checkpoint\n").unwrap();
        std::fs::write(beads.join("beads.db"), "original database").unwrap();

        let binary = workspace.path().join("fake-bead");
        std::fs::write(
            &binary,
            "#!/bin/sh\nif [ \"$1\" = init ]; then printf incomplete > .beads/beads.db; exit 0; fi\necho import-failed >&2\nexit 1\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&binary, permissions).unwrap();

        let backend = builtin_bead_backends()
            .into_iter()
            .find(|backend| backend.name == "bead-rs")
            .unwrap();
        let store = CliBeadStore::new(
            backend,
            binary,
            workspace.path().to_path_buf(),
            None,
            None,
            None,
        )
        .unwrap();

        let error = store.full_rebuild().await.unwrap_err();
        assert!(error.to_string().contains("original database restored"));
        assert_eq!(
            std::fs::read_to_string(beads.join("beads.db")).unwrap(),
            "original database"
        );
        assert!(!beads.join("beads.db.needle-rebuild-backup").exists());
    }

    #[test]
    fn cli_store_ready_limit_is_rendered_as_flag_and_value() {
        use crate::bead_store::backend::builtin_bead_backends;

        let temp_dir = tempfile::tempdir().unwrap();
        let fake_binary = temp_dir.path().join("fake-bead");
        std::fs::write(&fake_binary, "#!/bin/sh\necho bead 0.1.3\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake_binary).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake_binary, perms).unwrap();
        }

        // Test with bead-rs
        let bead_rs = builtin_bead_backends()
            .into_iter()
            .find(|backend| backend.name == "bead-rs")
            .unwrap();

        let store = CliBeadStore::new(
            bead_rs,
            fake_binary,
            temp_dir.path().to_path_buf(),
            None,
            None,
            None,
        )
        .unwrap();

        // bead-rs has the quirk (declared without version requirement in builtin_bead_rs)
        assert!(store.has_quirk("limit_zero_returns_empty_set"));
        // When the quirk applies, "0" is still passed - the workaround is applied by the caller
        // by using a large explicit limit instead of 0
        let values = HashMap::from([("limit", "0".to_string())]);
        assert_eq!(
            store.render_operation("ready", &values).unwrap(),
            ["list", "--ready", "--json", "--limit", "0"]
        );
    }

    // ─── Template rendering tests ────────────────────────────────────────────────

    #[test]
    fn render_operation_substitutes_single_placeholder() {
        use crate::bead_store::backend::builtin_bead_backends;

        let bead_rs = builtin_bead_backends()
            .into_iter()
            .find(|backend| backend.name == "bead-rs")
            .unwrap();

        let temp_dir = tempfile::tempdir().unwrap();
        let fake_binary = temp_dir.path().join("fake-bead");
        std::fs::write(&fake_binary, "#!/bin/sh\necho bead 0.1.3\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake_binary).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake_binary, perms).unwrap();
        }

        let store = CliBeadStore::new(
            bead_rs,
            fake_binary,
            temp_dir.path().to_path_buf(),
            None,
            None,
            None,
        )
        .unwrap();

        // Test single placeholder substitution
        let values = HashMap::from([("id", "bf-123".to_string())]);
        let result = store.render_operation("show", &values).unwrap();
        assert_eq!(result, vec!["show", "bf-123", "--json"]);
    }

    #[test]
    fn render_operation_substitutes_multiple_placeholders() {
        use crate::bead_store::backend::builtin_bead_backends;

        let bead_rs = builtin_bead_backends()
            .into_iter()
            .find(|backend| backend.name == "bead-rs")
            .unwrap();

        let temp_dir = tempfile::tempdir().unwrap();
        let fake_binary = temp_dir.path().join("fake-bead");
        std::fs::write(&fake_binary, "#!/bin/sh\necho bead 0.1.3\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake_binary).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake_binary, perms).unwrap();
        }

        let store = CliBeadStore::new(
            bead_rs,
            fake_binary,
            temp_dir.path().to_path_buf(),
            None,
            None,
            None,
        )
        .unwrap();

        // Test multiple placeholders in one argument
        let values = HashMap::from([
            ("blocked", "bf-parent".to_string()),
            ("blocker", "bf-child".to_string()),
        ]);
        let result = store.render_operation("dep_add", &values).unwrap();
        assert_eq!(
            result,
            vec!["dep", "add", "bf-parent", "bf-child", "--kind", "blocks"]
        );
    }

    #[test]
    fn render_operation_omits_optional_placeholder_when_empty() {
        use crate::bead_store::backend::builtin_bead_backends;

        let bead_rs = builtin_bead_backends()
            .into_iter()
            .find(|backend| backend.name == "bead-rs")
            .unwrap();

        let temp_dir = tempfile::tempdir().unwrap();
        let fake_binary = temp_dir.path().join("fake-bead");
        std::fs::write(&fake_binary, "#!/bin/sh\necho bead 0.1.3\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake_binary).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake_binary, perms).unwrap();
        }

        let store = CliBeadStore::new(
            bead_rs,
            fake_binary,
            temp_dir.path().to_path_buf(),
            None,
            None,
            None,
        )
        .unwrap();

        // Test that optional {limit} is omitted when not provided
        let values = HashMap::new();
        let result = store.render_operation("ready", &values).unwrap();
        // {limit} is optional and not provided, so --limit flag should be omitted entirely
        assert!(!result.contains(&"--limit".to_string()));
    }

    #[test]
    fn render_operation_errors_on_missing_required_placeholder() {
        use crate::bead_store::backend::builtin_bead_backends;

        let bead_rs = builtin_bead_backends()
            .into_iter()
            .find(|backend| backend.name == "bead-rs")
            .unwrap();

        let temp_dir = tempfile::tempdir().unwrap();
        let fake_binary = temp_dir.path().join("fake-bead");
        std::fs::write(&fake_binary, "#!/bin/sh\necho bead 0.1.3\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake_binary).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake_binary, perms).unwrap();
        }

        let store = CliBeadStore::new(
            bead_rs,
            fake_binary,
            temp_dir.path().to_path_buf(),
            None,
            None,
            None,
        )
        .unwrap();

        // Test that missing required placeholder errors
        let values = HashMap::new(); // Missing required {id}
        let result = store.render_operation("show", &values);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("requires placeholder"));
        assert!(err_msg.contains("{id}"));
    }

    #[test]
    fn render_operation_substitutes_implicit_values() {
        use crate::bead_store::backend::builtin_bead_backends;

        let bead_rs = builtin_bead_backends()
            .into_iter()
            .find(|backend| backend.name == "bead-rs")
            .unwrap();

        let temp_dir = tempfile::tempdir().unwrap();
        let fake_binary = temp_dir.path().join("fake-bead");
        std::fs::write(&fake_binary, "#!/bin/sh\necho bead 0.1.3\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake_binary).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake_binary, perms).unwrap();
        }

        let store = CliBeadStore::new(
            bead_rs,
            fake_binary,
            temp_dir.path().to_path_buf(),
            Some("gpt-4".to_string()),
            Some("claude-code".to_string()),
            Some("1.0.0".to_string()),
        )
        .unwrap();

        // Test that implicit values are substituted when placeholders exist
        let values = HashMap::from([("actor", "worker-1".to_string())]);
        let result = store.render_operation("claim_auto", &values).unwrap();
        assert!(result.contains(&"worker-1".to_string()));
        // model/harness placeholders are not used in bead-rs claim_auto, so these should not appear
        assert!(!result.contains(&"gpt-4".to_string()));
        assert!(!result.contains(&"claude-code".to_string()));
    }

    #[test]
    fn render_operation_handles_special_characters_in_values() {
        use crate::bead_store::backend::builtin_bead_backends;

        let bead_rs = builtin_bead_backends()
            .into_iter()
            .find(|backend| backend.name == "bead-rs")
            .unwrap();

        let temp_dir = tempfile::tempdir().unwrap();
        let fake_binary = temp_dir.path().join("fake-bead");
        std::fs::write(&fake_binary, "#!/bin/sh\necho bead 0.1.3\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake_binary).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake_binary, perms).unwrap();
        }

        let store = CliBeadStore::new(
            bead_rs,
            fake_binary,
            temp_dir.path().to_path_buf(),
            None,
            None,
            None,
        )
        .unwrap();

        // Test special characters in placeholder values
        let title_with_special = "Fix: bug in 'feature' (urgent) #123";
        let body_with_special = "Description with \"quotes\" and \\backslashes\\";
        let values = HashMap::from([
            ("title", title_with_special.to_string()),
            ("body", body_with_special.to_string()),
        ]);
        let result = store.render_operation("create", &values).unwrap();
        assert!(result.contains(&title_with_special.to_string()));
        assert!(result.contains(&body_with_special.to_string()));
    }

    #[test]
    fn render_operation_handles_empty_string_values() {
        use crate::bead_store::backend::builtin_bead_backends;

        let bead_rs = builtin_bead_backends()
            .into_iter()
            .find(|backend| backend.name == "bead-rs")
            .unwrap();

        let temp_dir = tempfile::tempdir().unwrap();
        let fake_binary = temp_dir.path().join("fake-bead");
        std::fs::write(&fake_binary, "#!/bin/sh\necho bead 0.1.3\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake_binary).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake_binary, perms).unwrap();
        }

        let store = CliBeadStore::new(
            bead_rs,
            fake_binary,
            temp_dir.path().to_path_buf(),
            Some("gpt-4".to_string()),
            Some("claude-code".to_string()),
            Some("1.0.0".to_string()),
        )
        .unwrap();

        // Test empty string values for optional placeholders
        let values = HashMap::from([("actor", "worker-1".to_string()), ("limit", "".to_string())]);
        let result = store.render_operation("ready", &values).unwrap();
        // Empty limit should omit the --limit flag entirely
        assert!(!result.iter().any(|arg| arg == "--limit"));
        // But actor placeholder doesn't exist in ready operation
    }

    #[test]
    fn render_operation_preserves_static_arguments() {
        use crate::bead_store::backend::builtin_bead_backends;

        let bead_rs = builtin_bead_backends()
            .into_iter()
            .find(|backend| backend.name == "bead-rs")
            .unwrap();

        let temp_dir = tempfile::tempdir().unwrap();
        let fake_binary = temp_dir.path().join("fake-bead");
        std::fs::write(&fake_binary, "#!/bin/sh\necho bead 0.1.3\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake_binary).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake_binary, perms).unwrap();
        }

        let store = CliBeadStore::new(
            bead_rs,
            fake_binary,
            temp_dir.path().to_path_buf(),
            None,
            None,
            None,
        )
        .unwrap();

        // Test that static arguments (no placeholders) are preserved
        let values = HashMap::from([("id", "bf-123".to_string())]);
        let result = store.render_operation("show", &values).unwrap();
        assert!(result.contains(&"--json".to_string()));
        assert!(result.contains(&"show".to_string()));
    }

    #[test]
    fn claim_status_parser_handles_bead_rs_response() {
        use crate::types::BeadStatus;

        // Test parsing a bead-rs response with revision
        let json = r#"{
            "id": "bead-test",
            "title": "Test Bead",
            "status": "open",
            "assignee": "worker-01",
            "revision": 42,
            "created_at": "2026-08-28T00:00:00Z"
        }"#;

        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        let status_str = value.get("status").and_then(|v| v.as_str()).unwrap();
        let status =
            serde_json::from_str::<BeadStatus>(&serde_json::to_string(status_str).unwrap())
                .unwrap();

        let assignee = value
            .get("assignee")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let revision = value.get("revision").and_then(|v| v.as_u64());

        assert_eq!(status, BeadStatus::Open);
        assert_eq!(assignee, Some("worker-01".to_string()));
        assert_eq!(revision, Some(42));
    }

    #[test]
    fn claim_status_parser_handles_unassigned_bead() {
        // Test parsing a bead with no assignee
        let json = r#"{
            "id": "bead-test",
            "title": "Test Bead",
            "status": "in_progress",
            "assignee": "",
            "revision": 15,
            "created_at": "2026-08-28T00:00:00Z"
        }"#;

        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        let assignee = value
            .get("assignee")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        assert_eq!(assignee, None);
    }

    #[test]
    fn claim_status_parser_handles_missing_revision() {
        // Test that JSON responses without a revision field are handled correctly
        let json = r#"{
            "id": "test",
            "title": "Test Bead",
            "status": "open",
            "created_at": "2026-08-28T00:00:00Z"
        }"#;

        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        let revision = value.get("revision").and_then(|v| v.as_u64());

        assert_eq!(revision, None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn show_returns_bead_on_successful_lookup() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let binary = workspace.path().join("fake-bead");

        // Create a fake binary that returns a valid bead JSON
        std::fs::write(
            &binary,
            r#"#!/bin/sh
if [ "$1" = "show" ]; then
  echo '{
    "id": "bf-test-123",
    "title": "Test Bead",
    "description": "Test body",
    "priority": 1,
    "status": "open",
    "assignee": null,
    "labels": ["test", "example"],
    "source_repo": "/test/workspace",
    "dependencies": [],
    "dependents": [],
    "comments": [],
    "created_at": "2026-08-28T00:00:00Z",
    "updated_at": "2026-08-28T01:00:00Z"
  }'
else
  echo 'bead 0.1.3'
fi
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&binary, permissions).unwrap();

        let backend = builtin_bead_backends()
            .into_iter()
            .find(|backend| backend.name == "bead-rs")
            .unwrap();
        let store = CliBeadStore::new(
            backend,
            binary,
            workspace.path().to_path_buf(),
            None,
            None,
            None,
        )
        .unwrap();

        // Test successful lookup
        let bead_id = crate::types::BeadId::from("bf-test-123");
        let result = store.show(&bead_id).await;

        assert!(result.is_ok(), "show() should succeed for valid bead ID");
        let bead = result.unwrap();
        assert_eq!(bead.id.as_ref(), "bf-test-123");
        assert_eq!(bead.title, "Test Bead");
        assert_eq!(bead.body, Some("Test body".to_string()));
        assert_eq!(bead.priority, 1);
        assert_eq!(bead.status, crate::types::BeadStatus::Open);
        assert!(bead.assignee.is_none());
        assert_eq!(bead.labels, vec!["test".to_string(), "example".to_string()]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn show_returns_error_on_not_found() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let binary = workspace.path().join("fake-bead");

        // Create a fake binary that returns empty output (bead not found)
        std::fs::write(
            &binary,
            r#"#!/bin/sh
if [ "$1" = "show" ]; then
  # Return empty JSON array to simulate "not found"
  echo '[]'
else
  echo 'bead 0.1.3'
fi
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&binary, permissions).unwrap();

        let backend = builtin_bead_backends()
            .into_iter()
            .find(|backend| backend.name == "bead-rs")
            .unwrap();
        let store = CliBeadStore::new(
            backend,
            binary,
            workspace.path().to_path_buf(),
            None,
            None,
            None,
        )
        .unwrap();

        // Test not-found case
        let bead_id = crate::types::BeadId::from("bf-nonexistent");
        let result = store.show(&bead_id).await;

        assert!(
            result.is_err(),
            "show() should return error for non-existent bead"
        );
        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("returned no bead") || error_msg.contains("bf-nonexistent"),
            "Error message should indicate bead was not found: {}",
            error_msg
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn show_handles_backend_command_failure() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let binary = workspace.path().join("fake-bead");

        // Create a fake binary that exits with error for unknown beads
        std::fs::write(
            &binary,
            r#"#!/bin/sh
if [ "$1" = "show" ]; then
  echo "Error: bead not found" >&2
  exit 1
else
  echo 'bead 0.1.3'
fi
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&binary, permissions).unwrap();

        let backend = builtin_bead_backends()
            .into_iter()
            .find(|backend| backend.name == "bead-rs")
            .unwrap();
        let store = CliBeadStore::new(
            backend,
            binary,
            workspace.path().to_path_buf(),
            None,
            None,
            None,
        )
        .unwrap();

        // Test backend command failure
        let bead_id = crate::types::BeadId::from("bf-error-case");
        let result = store.show(&bead_id).await;

        assert!(result.is_err(), "show() should propagate backend errors");
        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("exited with code") || error_msg.contains("not found"),
            "Error message should indicate backend failure: {}",
            error_msg
        );
    }
}
