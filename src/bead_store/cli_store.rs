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
        let values = HashMap::from([("limit", "999999".to_string())]);
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
        let values = HashMap::from([("limit", "999999".to_string())]);
        let stdout = self.run_operation("list_all", &values).await?;
        self.parse_beads("list_all", &stdout)
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
        if self.backend.name == "bead-forge" {
            return self.run_bf_update_batch(id, Some("open"), Some("")).await;
        }
        self.mutate("release", &[("id", id.to_string())]).await
    }
    async fn block(&self, id: &BeadId) -> Result<()> {
        self.mutate("block", &[("id", id.to_string())]).await
    }
    async fn clear_assignee(&self, id: &BeadId) -> Result<()> {
        if self.backend.name == "bead-forge" {
            return self.run_bf_update_batch(id, None, Some("")).await;
        }
        self.mutate("clear_assignee", &[("id", id.to_string())])
            .await
    }
    async fn flush(&self) -> Result<()> {
        self.mutate("flush", &[]).await
    }
    async fn reopen(&self, id: &BeadId) -> Result<()> {
        self.mutate("reopen", &[("id", id.to_string())]).await
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
        if !matches!(self.backend.name.as_str(), "bead-rs" | "bead-forge") {
            bail!(
                "descriptor-driven full rebuild is not enabled for backend '{}'",
                self.backend.name
            );
        }

        let db_path = self.workspace.join(".beads/beads.db");
        if db_path.exists() {
            tokio::fs::remove_file(&db_path)
                .await
                .with_context(|| format!("failed to remove {}", db_path.display()))?;
        }
        for suffix in ["-wal", "-shm"] {
            let sidecar = self.workspace.join(format!(".beads/beads.db{suffix}"));
            if sidecar.exists() {
                tokio::fs::remove_file(&sidecar)
                    .await
                    .with_context(|| format!("failed to remove {}", sidecar.display()))?;
            }
        }

        if self.backend.name == "bead-forge" {
            self.run_argv(
                "full_rebuild.import",
                &["sync".to_string(), "--import-only".to_string()],
                DEFAULT_TIMEOUT_SECS,
            )
            .await?;
            let report = self.doctor_check().await?;
            if !report.warnings.is_empty() {
                bail!(
                    "database still has issues after rebuild: {:?}",
                    report.warnings
                );
            }
            return Ok(());
        }

        self.run_argv(
            "full_rebuild.init",
            &["init".to_string()],
            DEFAULT_TIMEOUT_SECS,
        )
        .await?;
        let checkpoint = self.workspace.join(".beads/checkpoint");
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
    matches!(name, "model" | "harness" | "harness_version")
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
        ParseShape::JsonLines => output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).context("invalid JSON line"))
            .collect(),
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
        ParseShape::JsonLines => output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(serde_json::from_str::<BeadWithClaimHistory>)
            .collect::<std::result::Result<Vec<_>, _>>()?,
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
    use super::{parse_beads_with_claim_history, ParseShape};

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
}
