//! Prompt-template canaries with automatic stop on regression (plan section
//! 4.4 step 5 / ledger N-T19; L2 in the section 5.7 envelope).
//!
//! `prompt.variants` already assigns workers deterministically to template
//! variants and stamps `template_version` on every attempt — an A/B harness
//! with a human in the analysis seat and no way to pull a bad variant. This
//! module closes the loop the safe way round: it never promotes a variant,
//! it only *stops* one whose verified-success rate falls behind the default
//! by more than the configured margin once both have enough attempts.
//!
//! A stop is a receipt file under `~/.needle/state/experiments/`
//! (`<template>--<variant>.stopped.json`) carrying the numbers that stopped
//! it. `PromptBuilder::select_variant` consults the directory and falls back
//! to the built-in template for a stopped variant, so the stop takes effect
//! on the next build without a config edit. Removing the file re-arms the
//! variant; changing the config promotes one — both are operator actions,
//! by design (ADR-026/027: automatic adaptation selects among approved
//! variants and may withdraw one; it does not create policy).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::config::{ExperimentConfig, VariantConfig};

/// Verified outcomes for one template version in the window.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VariantOutcome {
    pub template: String,
    pub template_version: String,
    pub attempts: u64,
    pub verified: u64,
    pub infrastructure: u64,
    /// Rows resolved `decomposed`: a split is neither a win nor a loss for
    /// the variant (ADR-030).
    #[serde(default)]
    pub decomposed: u64,
}

impl VariantOutcome {
    pub fn judged(&self) -> u64 {
        self.attempts
            .saturating_sub(self.infrastructure)
            .saturating_sub(self.decomposed)
    }
    pub fn success_rate(&self) -> Option<f64> {
        let judged = self.judged();
        (judged > 0).then(|| self.verified as f64 / judged as f64)
    }
}

/// What the evaluator decided for one variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    /// Not enough evidence yet, or the variant is holding up.
    Continue {
        template: String,
        variant: String,
        variant_rate: Option<f64>,
        baseline_rate: Option<f64>,
        variant_attempts: u64,
        baseline_attempts: u64,
    },
    /// The variant regressed past the margin: stop exposing it.
    Stop {
        template: String,
        variant: String,
        variant_rate: f64,
        baseline_rate: f64,
        variant_attempts: u64,
        baseline_attempts: u64,
        margin: f64,
        stopped_at: String,
    },
}

/// The receipt written for a stop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StopReceipt {
    pub schema_version: u32,
    #[serde(flatten)]
    pub decision: Decision,
}

/// Aggregate `attempt.resolved` rows by template version. Rows resolved
/// inside a gate-degraded window (`gate_degraded: true`, N-T21) are
/// excluded: a canary must not stop or earn credit on window noise.
pub fn variant_outcomes(rows: &[serde_json::Value]) -> HashMap<String, VariantOutcome> {
    let mut out: HashMap<String, VariantOutcome> = HashMap::new();
    for row in rows {
        if !crate::evidence_routing::is_authoritative_attempt_row(row)
            || crate::evidence_routing::is_degraded_window_row(row)
        {
            continue;
        }
        let version = row
            .get("template_version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if version.is_empty() {
            continue;
        }
        let template = row
            .get("prompt_template")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let entry = out
            .entry(version.clone())
            .or_insert_with(|| VariantOutcome {
                template,
                template_version: version,
                ..VariantOutcome::default()
            });
        entry.attempts += 1;
        match row.get("outcome").and_then(|v| v.as_str()) {
            Some("verified_success") => entry.verified += 1,
            Some("infrastructure_failure") => entry.infrastructure += 1,
            Some(crate::attempt_accounting::DECOMPOSED) => entry.decomposed += 1,
            _ => {}
        }
    }
    out
}

/// Evaluate every configured variant of `template` against the default.
pub fn evaluate(
    template: &str,
    variants: &[VariantConfig],
    outcomes: &HashMap<String, VariantOutcome>,
    config: &ExperimentConfig,
) -> Vec<Decision> {
    let baseline = outcomes.get(&format!("{template}-default"));
    let baseline_attempts = baseline.map(|b| b.judged()).unwrap_or(0);
    let baseline_rate = baseline.and_then(|b| b.success_rate());
    variants
        .iter()
        .map(|variant| {
            let key = format!("{template}-{}", variant.name);
            let outcome = outcomes.get(&key);
            let attempts = outcome.map(|o| o.judged()).unwrap_or(0);
            let rate = outcome.and_then(|o| o.success_rate());
            match (rate, baseline_rate) {
                (Some(vr), Some(br))
                    if attempts >= config.min_attempts
                        && baseline_attempts >= config.min_attempts
                        && vr + config.regression_margin < br =>
                {
                    Decision::Stop {
                        template: template.to_string(),
                        variant: variant.name.clone(),
                        variant_rate: vr,
                        baseline_rate: br,
                        variant_attempts: attempts,
                        baseline_attempts,
                        margin: config.regression_margin,
                        stopped_at: chrono::Utc::now().to_rfc3339(),
                    }
                }
                _ => Decision::Continue {
                    template: template.to_string(),
                    variant: variant.name.clone(),
                    variant_rate: rate,
                    baseline_rate,
                    variant_attempts: attempts,
                    baseline_attempts,
                },
            }
        })
        .collect()
}

/// Default receipt directory: the state root's `state/experiments`
/// (`~/.needle/state/experiments` by default; ADR-030 decision 5).
pub fn default_state_dir() -> PathBuf {
    crate::state_dir::experiments_dir()
}

/// Receipt path for one variant.
pub fn stop_file(state_dir: &Path, template: &str, variant: &str) -> PathBuf {
    let safe = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    state_dir.join(format!(
        "{}--{}.stopped.json",
        safe(template),
        safe(variant)
    ))
}

/// Whether a variant has a stop receipt.
pub fn is_stopped(state_dir: &Path, template: &str, variant: &str) -> bool {
    if stop_file(state_dir, template, variant).is_file() {
        return true;
    }
    load_experiment(state_dir, template, variant)
        .ok()
        .flatten()
        .is_some_and(|record| {
            matches!(
                record.status,
                ExperimentStatus::Stopped | ExperimentStatus::RolledBack
            )
        })
}

/// Write the receipt for a `Stop` decision. Returns `Ok(false)` when one
/// already exists (idempotent), `Ok(true)` when written.
pub fn record_stop(state_dir: &Path, decision: &Decision) -> Result<bool> {
    let Decision::Stop {
        template, variant, ..
    } = decision
    else {
        return Ok(false);
    };
    let path = stop_file(state_dir, template, variant);
    if path.is_file() {
        return Ok(false);
    }
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("failed to create {}", state_dir.display()))?;
    let receipt = StopReceipt {
        schema_version: 1,
        decision: decision.clone(),
    };
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&receipt)?)
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("failed to commit {}", path.display()))?;
    Ok(true)
}

/// The declared primary metric for a prompt-template experiment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentPrimaryMetric {
    VerifiedSuccessRate,
}

/// Lifecycle of a policy experiment. `Promotable` is only a recommendation;
/// changing prompt configuration remains an operator or audited-operation act.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentStatus {
    Running,
    Promotable,
    /// Exposure and evaluation are held while the owning workspace's gate is
    /// degraded. This is recoverable and must never be treated as a stop.
    Paused,
    Stopped,
    RolledBack,
}

impl ExperimentStatus {
    fn is_terminal(self) -> bool {
        matches!(self, Self::Stopped | Self::RolledBack)
    }
}

/// Evidence floor and candidate regression tolerance declared for an experiment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperimentStopCondition {
    pub min_attempts: u64,
    pub regression_tolerance: f64,
}

/// Metric values observed when the controller writes a decision receipt.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExperimentMetrics {
    pub attempts: u64,
    pub judged_attempts: u64,
    pub verified_closes: u64,
    pub verified_success_rate: Option<f64>,
    pub cost_per_verified_close_usd: Option<f64>,
    pub retry_amplification: Option<f64>,
    pub gate_error_rate: Option<f64>,
}

/// Audit record created for a stop, rollback, or promotable recommendation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperimentReceipt {
    pub schema_version: u32,
    pub decided_at: DateTime<Utc>,
    pub status: ExperimentStatus,
    pub reason: String,
    pub baseline: ExperimentMetrics,
    pub candidate: ExperimentMetrics,
}

/// Durable controller record for one configured prompt variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperimentRecord {
    pub id: String,
    pub template: String,
    pub baseline_version: String,
    pub candidate_version: String,
    /// Maximum candidate allocation as a fraction in `[0.0, 1.0]`.
    pub share: f64,
    pub primary_metric: ExperimentPrimaryMetric,
    pub guardrails: crate::config::ExperimentGuardrailsConfig,
    pub stop_condition: ExperimentStopCondition,
    pub started_at: DateTime<Utc>,
    pub status: ExperimentStatus,
    /// Status to restore after a gate-degraded pause. Missing on records
    /// written before N-T21 and defaults to `Running` when resumed.
    #[serde(default)]
    pub paused_status: Option<ExperimentStatus>,
    /// Workspace whose degraded gate caused this pause, when known.
    #[serde(default)]
    pub paused_workspace: Option<String>,
    pub receipt: Option<ExperimentReceipt>,
}

/// Construct an experiment record from the configured variant and its share.
pub fn experiment_record(
    template: &str,
    variant: &VariantConfig,
    config: &ExperimentConfig,
    started_at: DateTime<Utc>,
) -> ExperimentRecord {
    let candidate_version = format!("{template}-{}", variant.name);
    ExperimentRecord {
        id: format!("{template}--{}", variant.name),
        template: template.to_string(),
        baseline_version: format!("{template}-default"),
        candidate_version,
        share: f64::from(variant.weight) / 100.0,
        primary_metric: ExperimentPrimaryMetric::VerifiedSuccessRate,
        guardrails: config.guardrails.clone(),
        stop_condition: ExperimentStopCondition {
            min_attempts: config.min_attempts,
            regression_tolerance: config.regression_margin,
        },
        started_at,
        status: ExperimentStatus::Running,
        paused_status: None,
        paused_workspace: None,
        receipt: None,
    }
}

/// Pause an active experiment without creating a terminal stop decision.
/// Repeated calls are idempotent and preserve a prior promotable status so it
/// can be reconsidered after the workspace gate is restored.
pub fn pause_experiment(
    record: &ExperimentRecord,
    workspace: &Path,
    paused_at: DateTime<Utc>,
) -> ExperimentRecord {
    if record.status.is_terminal() || record.status == ExperimentStatus::Paused {
        return record.clone();
    }

    let mut paused = record.clone();
    paused.paused_status = Some(record.status);
    paused.paused_workspace = Some(
        workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf())
            .to_string_lossy()
            .into_owned(),
    );
    paused.status = ExperimentStatus::Paused;
    let prior = record.receipt.as_ref();
    paused.receipt = Some(ExperimentReceipt {
        schema_version: 1,
        decided_at: paused_at,
        status: ExperimentStatus::Paused,
        reason: "workspace gate-degraded; evaluation and exposure paused".to_string(),
        baseline: prior
            .map(|receipt| receipt.baseline.clone())
            .unwrap_or_default(),
        candidate: prior
            .map(|receipt| receipt.candidate.clone())
            .unwrap_or_default(),
    });
    paused
}

/// Evaluate the current ledger window for one configured variant.
///
/// Baseline and candidate attempts must be authoritative `attempt.resolved`
/// rows. The caller supplies the already bounded ledger window and retains the
/// previous record so `started_at` and terminal decisions survive restarts.
pub fn evaluate_policy_experiment(
    template: &str,
    variant: &VariantConfig,
    rows: &[serde_json::Value],
    config: &ExperimentConfig,
    previous: Option<&ExperimentRecord>,
    evaluated_at: DateTime<Utc>,
) -> ExperimentRecord {
    let started_at = previous
        .map(|record| record.started_at)
        .unwrap_or(evaluated_at);
    let mut record = experiment_record(template, variant, config, started_at);

    if let Some(previous) = previous {
        if previous.status.is_terminal() {
            return previous.clone();
        }
    }

    let baseline = experiment_metrics(rows, template, &record.baseline_version);
    let candidate = experiment_metrics(rows, template, &record.candidate_version);
    let candidate_rate = candidate.verified_success_rate;
    let baseline_rate = baseline.verified_success_rate;
    let min_attempts = record.stop_condition.min_attempts;

    let guardrail_failure = guardrail_breach(&record.guardrails, &candidate);
    let (status, reason) = if let Some(reason) = guardrail_failure {
        (ExperimentStatus::Stopped, reason)
    } else if candidate.judged_attempts >= min_attempts && baseline.judged_attempts >= min_attempts
    {
        match (candidate_rate, baseline_rate) {
            (Some(candidate_rate), Some(baseline_rate))
                if candidate_rate + record.stop_condition.regression_tolerance < baseline_rate =>
            {
                (
                    ExperimentStatus::Stopped,
                    "verified-success rate regressed beyond the declared tolerance".to_string(),
                )
            }
            (Some(candidate_rate), Some(baseline_rate))
                if candidate_rate > baseline_rate + record.stop_condition.regression_tolerance =>
            {
                (
                    ExperimentStatus::Promotable,
                    "verified-success rate improved beyond the declared tolerance".to_string(),
                )
            }
            _ => (
                ExperimentStatus::Running,
                "evidence is within tolerance".to_string(),
            ),
        }
    } else {
        (
            ExperimentStatus::Running,
            "waiting for the declared minimum judged attempts".to_string(),
        )
    };

    // Once evidence makes a candidate promotable, keep that recommendation
    // while monitoring it. A later regression or guardrail breach still wins.
    let previous_status = previous.map(|prior| {
        if prior.status == ExperimentStatus::Paused {
            prior.paused_status.unwrap_or(ExperimentStatus::Running)
        } else {
            prior.status
        }
    });
    let (status, reason) = if status == ExperimentStatus::Running
        && previous_status == Some(ExperimentStatus::Promotable)
    {
        (
            ExperimentStatus::Promotable,
            "candidate remains promotable while guardrails hold".to_string(),
        )
    } else {
        (status, reason)
    };

    record.status = status;
    if status != ExperimentStatus::Running {
        record.receipt = Some(ExperimentReceipt {
            schema_version: 1,
            decided_at: evaluated_at,
            status,
            reason,
            baseline,
            candidate,
        });
    }
    record
}

fn experiment_metrics(
    rows: &[serde_json::Value],
    template: &str,
    version: &str,
) -> ExperimentMetrics {
    let mut metrics = ExperimentMetrics::default();
    let mut beads = BTreeSet::new();
    let mut known_cost_usd = 0.0;
    let mut costed_rows = 0u64;
    let mut gate_error_attempts = 0u64;
    let mut infrastructure_attempts = 0u64;
    let mut decomposed_attempts = 0u64;

    for row in rows {
        if !crate::evidence_routing::is_authoritative_attempt_row(row)
            || crate::evidence_routing::is_degraded_window_row(row)
            || row.get("prompt_template").and_then(|value| value.as_str()) != Some(template)
            || row.get("template_version").and_then(|value| value.as_str()) != Some(version)
        {
            continue;
        }
        metrics.attempts += 1;
        match row.get("outcome").and_then(|value| value.as_str()) {
            Some("verified_success") => metrics.verified_closes += 1,
            Some("infrastructure_failure") => infrastructure_attempts += 1,
            Some(crate::attempt_accounting::DECOMPOSED) => decomposed_attempts += 1,
            _ => {}
        }
        if let Some(bead_id) = row.get("bead_id").and_then(|value| value.as_str()) {
            beads.insert(bead_id.to_string());
        }
        if row.get("costed").and_then(|value| value.as_bool()) == Some(true) {
            if let Some(cost) = row
                .get("estimated_cost_usd")
                .and_then(|value| value.as_f64())
            {
                known_cost_usd += cost.max(0.0);
                costed_rows += 1;
            }
        }
        let terminal_gate_error = row
            .get("terminal_reason")
            .and_then(|value| value.as_str())
            .is_some_and(|reason| reason.starts_with("gate_error"));
        let gate_result_error = row
            .get("gate_results")
            .and_then(|value| value.as_array())
            .is_some_and(|gates| {
                gates.iter().any(|gate| {
                    gate.get("status").and_then(|value| value.as_str()) == Some("execution_error")
                })
            });
        if terminal_gate_error || gate_result_error {
            gate_error_attempts += 1;
        }
    }

    metrics.judged_attempts = metrics
        .attempts
        .saturating_sub(infrastructure_attempts)
        .saturating_sub(decomposed_attempts);
    if metrics.judged_attempts > 0 {
        metrics.verified_success_rate =
            Some(metrics.verified_closes as f64 / metrics.judged_attempts as f64);
    }
    if !beads.is_empty() {
        metrics.retry_amplification = Some(metrics.attempts as f64 / beads.len() as f64);
    }
    if metrics.attempts > 0 {
        metrics.gate_error_rate = Some(gate_error_attempts as f64 / metrics.attempts as f64);
    }
    if costed_rows == metrics.attempts && metrics.attempts > 0 && metrics.verified_closes > 0 {
        metrics.cost_per_verified_close_usd = Some(known_cost_usd / metrics.verified_closes as f64);
    }
    metrics
}

fn guardrail_breach(
    guardrails: &crate::config::ExperimentGuardrailsConfig,
    metrics: &ExperimentMetrics,
) -> Option<String> {
    if let (Some(limit), Some(value)) = (
        guardrails.max_cost_per_verified_close_usd,
        metrics.cost_per_verified_close_usd,
    ) {
        if value > limit {
            return Some(format!(
                "cost per verified close {value:.4} USD exceeded {limit:.4} USD"
            ));
        }
    }
    if let (Some(limit), Some(value)) = (
        guardrails.max_retry_amplification,
        metrics.retry_amplification,
    ) {
        if value > limit {
            return Some(format!(
                "retry amplification {value:.4} exceeded {limit:.4}"
            ));
        }
    }
    if let (Some(limit), Some(value)) = (guardrails.max_gate_error_rate, metrics.gate_error_rate) {
        if value > limit {
            return Some(format!("gate-error rate {value:.4} exceeded {limit:.4}"));
        }
    }
    None
}

/// Path for the durable experiment record, outside prompt and template files.
pub fn experiment_path(state_dir: &Path, template: &str, variant: &str) -> PathBuf {
    state_dir.join(format!(
        "{}--{}.experiment.json",
        safe_component(template),
        safe_component(variant)
    ))
}

fn safe_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

/// Load an experiment record if it has been created.
pub fn load_experiment(
    state_dir: &Path,
    template: &str,
    variant: &str,
) -> Result<Option<ExperimentRecord>> {
    let path = experiment_path(state_dir, template, variant);
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))
        .map(Some)
}

/// Persist an experiment record atomically under the experiment state dir.
/// Terminal states cannot be overwritten by a stale running evaluation.
pub fn record_experiment(state_dir: &Path, record: &ExperimentRecord) -> Result<bool> {
    record_experiment_inner(state_dir, record, false)
}

/// Persist an evaluation that explicitly resumed a paused experiment after
/// workspace gate restoration. Ordinary stale evaluations cannot clear a
/// pause written by another worker.
pub fn record_resumed_experiment(state_dir: &Path, record: &ExperimentRecord) -> Result<bool> {
    anyhow::ensure!(
        record.status != ExperimentStatus::Paused,
        "a resumed experiment must leave the paused status"
    );
    record_experiment_inner(state_dir, record, true)
}

fn record_experiment_inner(
    state_dir: &Path,
    record: &ExperimentRecord,
    allow_resume: bool,
) -> Result<bool> {
    anyhow::ensure!(
        record.share.is_finite() && (0.0..=1.0).contains(&record.share),
        "experiment share must be between 0 and 1"
    );
    with_template_lock(state_dir, &record.template, || {
        let variant = record
            .candidate_version
            .strip_prefix(&format!("{}-", record.template))
            .unwrap_or(&record.candidate_version);
        let path = experiment_path(state_dir, &record.template, variant);
        if let Some(current) = load_experiment(state_dir, &record.template, variant)? {
            if current.status == ExperimentStatus::Paused
                && record.status != ExperimentStatus::Paused
                && !allow_resume
            {
                return Ok(false);
            }
            if current.status.is_terminal() && !record.status.is_terminal() {
                return Ok(false);
            }
            if current.status == ExperimentStatus::Promotable
                && record.status == ExperimentStatus::Running
            {
                return Ok(false);
            }
            if current == *record {
                return Ok(false);
            }
        }
        write_json_atomic(&path, record)?;
        Ok(true)
    })
}

/// Revert future exposure to the baseline and write a rollback receipt.
pub fn rollback_experiment(
    state_dir: &Path,
    record: &ExperimentRecord,
    reason: impl Into<String>,
    rolled_back_at: DateTime<Utc>,
) -> Result<ExperimentRecord> {
    let mut rolled_back = record.clone();
    rolled_back.status = ExperimentStatus::RolledBack;
    let prior_metrics = record.receipt.as_ref();
    rolled_back.receipt = Some(ExperimentReceipt {
        schema_version: 1,
        decided_at: rolled_back_at,
        status: ExperimentStatus::RolledBack,
        reason: reason.into(),
        baseline: prior_metrics
            .map(|receipt| receipt.baseline.clone())
            .unwrap_or_default(),
        candidate: prior_metrics
            .map(|receipt| receipt.candidate.clone())
            .unwrap_or_default(),
    });
    record_experiment(state_dir, &rolled_back)?;
    Ok(rolled_back)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ExposureLedger {
    #[serde(default)]
    total_attempts: u64,
    #[serde(default)]
    candidate_attempts: BTreeMap<String, u64>,
    /// Attempt ID to the assigned variant name; an empty string means baseline.
    #[serde(default)]
    assignments: BTreeMap<String, String>,
}

/// Assign one attempt to a prompt variant without exceeding that variant's
/// declared share at any prefix of the assignment sequence. Repeated calls for
/// an attempt ID return the original assignment.
pub fn assign_prompt_variant(
    state_dir: &Path,
    template: &str,
    variants: &[VariantConfig],
    attempt_id: &str,
) -> Result<Option<String>> {
    anyhow::ensure!(!attempt_id.is_empty(), "attempt ID must not be empty");
    anyhow::ensure!(
        variants.iter().all(|variant| variant.weight <= 100),
        "prompt variant weight must be between 0 and 100"
    );
    with_template_lock(state_dir, template, || {
        let path = exposure_path(state_dir, template);
        let mut ledger = if path.is_file() {
            let bytes =
                fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
            serde_json::from_slice::<ExposureLedger>(&bytes)
                .with_context(|| format!("failed to parse {}", path.display()))?
        } else {
            ExposureLedger::default()
        };
        if let Some(assigned) = ledger.assignments.get(attempt_id) {
            return Ok((!assigned.is_empty()).then(|| assigned.clone()));
        }

        let next_total = ledger.total_attempts.saturating_add(1);
        let mut eligible: Vec<&VariantConfig> = Vec::new();
        for variant in variants {
            if variant.weight == 0 {
                continue;
            }
            if load_experiment(state_dir, template, &variant.name)?.is_some_and(|record| {
                matches!(
                    record.status,
                    ExperimentStatus::Stopped | ExperimentStatus::RolledBack
                )
            }) {
                continue;
            }
            let cap = (u128::from(next_total) * u128::from(variant.weight)) / 100;
            let assigned = ledger
                .candidate_attempts
                .get(&variant.name)
                .copied()
                .unwrap_or(0);
            if u128::from(assigned) < cap {
                eligible.push(variant);
            }
        }
        let selected = if eligible.is_empty() {
            None
        } else {
            let bucket = assignment_bucket(attempt_id) as usize % eligible.len();
            Some(eligible.swap_remove(bucket).name.clone())
        };
        ledger.total_attempts = next_total;
        if let Some(name) = &selected {
            *ledger.candidate_attempts.entry(name.clone()).or_default() += 1;
        }
        ledger
            .assignments
            .insert(attempt_id.to_string(), selected.clone().unwrap_or_default());
        // Keep idempotency bounded; attempt.resolved remains the durable row
        // describing the actual version used on each completed attempt.
        while ledger.assignments.len() > 20_000 {
            if let Some(oldest) = ledger.assignments.keys().next().cloned() {
                ledger.assignments.remove(&oldest);
            }
        }
        write_json_atomic(&path, &ledger)?;
        Ok(selected)
    })
}

fn assignment_bucket(attempt_id: &str) -> u64 {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(attempt_id.as_bytes());
    u64::from_be_bytes(digest[..8].try_into().unwrap_or([0; 8]))
}

fn exposure_path(state_dir: &Path, template: &str) -> PathBuf {
    state_dir.join(format!("{}.exposure.json", safe_component(template)))
}

fn with_template_lock<T>(
    state_dir: &Path,
    template: &str,
    action: impl FnOnce() -> Result<T>,
) -> Result<T> {
    fs::create_dir_all(state_dir)
        .with_context(|| format!("failed to create {}", state_dir.display()))?;
    let lock_path = state_dir.join(format!("{}.lock", safe_component(template)));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("failed to open {}", lock_path.display()))?;
    lock.lock_exclusive()
        .with_context(|| format!("failed to lock {}", lock_path.display()))?;
    let result = action();
    FileExt::unlock(&lock).with_context(|| format!("failed to unlock {}", lock_path.display()))?;
    result
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<()> {
    let temp_path = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut file = File::create(&temp_path)
        .with_context(|| format!("failed to create {}", temp_path.display()))?;
    file.write_all(&bytes)
        .with_context(|| format!("failed to write {}", temp_path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", temp_path.display()))?;
    fs::rename(&temp_path, path).with_context(|| format!("failed to commit {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ExperimentConfig {
        ExperimentConfig {
            enabled: true,
            min_attempts: 30,
            regression_margin: 0.15,
            window_days: 14,
            refresh_secs: 600,
            guardrails: crate::config::ExperimentGuardrailsConfig::default(),
        }
    }

    fn variants() -> Vec<VariantConfig> {
        vec![VariantConfig {
            name: "v2".into(),
            weight: 50,
            content_file: PathBuf::from("prompts/pluck-v2.md"),
        }]
    }

    fn row(version: &str, outcome: &str) -> serde_json::Value {
        serde_json::json!({
            "provisional": false,
            "prompt_template": "pluck",
            "template_version": version,
            "outcome": outcome
        })
    }

    #[test]
    fn provisional_rows_do_not_change_prompt_variant_evidence() {
        let mut provisional = rows((40, 30), (40, 10));
        for row in &mut provisional {
            row["provisional"] = serde_json::json!(true);
        }
        provisional.push(serde_json::json!({
            "prompt_template": "pluck",
            "template_version": "pluck-v3",
            "outcome": "verified_success"
        }));

        assert!(variant_outcomes(&provisional).is_empty());
    }

    fn rows(default: (u64, u64), v2: (u64, u64)) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        for i in 0..default.0 {
            out.push(row(
                "pluck-default",
                if i < default.1 {
                    "verified_success"
                } else {
                    "work_failure"
                },
            ));
        }
        for i in 0..v2.0 {
            out.push(row(
                "pluck-v2",
                if i < v2.1 {
                    "verified_success"
                } else {
                    "work_failure"
                },
            ));
        }
        out
    }

    fn policy_rows(default: (u64, u64), v2: (u64, u64), cost: f64) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        for (version, (attempts, successes)) in [("pluck-default", default), ("pluck-v2", v2)] {
            for index in 0..attempts {
                out.push(serde_json::json!({
                    "provisional": false,
                    "attempt_id": format!("{version}-{index}"),
                    "bead_id": format!("{version}-bead-{index}"),
                    "prompt_template": "pluck",
                    "template_version": version,
                    "outcome": if index < successes { "verified_success" } else { "work_failure" },
                    "estimated_cost_usd": cost,
                    "costed": true,
                    "gate_results": [{"name": "dod", "status": "pass"}]
                }));
            }
        }
        out
    }

    fn fast_cfg() -> ExperimentConfig {
        ExperimentConfig {
            min_attempts: 10,
            regression_margin: 0.10,
            ..cfg()
        }
    }

    #[test]
    fn nt46_decomposed_rows_are_neither_verified_nor_judged() {
        let mut r = rows((40, 30), (0, 0));
        for _ in 0..10 {
            r.push(row("pluck-default", "decomposed"));
        }
        let outcomes = variant_outcomes(&r);
        let baseline = &outcomes["pluck-default"];
        assert_eq!(
            (
                baseline.attempts,
                baseline.verified,
                baseline.decomposed,
                baseline.judged()
            ),
            (50, 30, 10, 40)
        );
        assert_eq!(baseline.success_rate(), Some(0.75));
    }

    #[test]
    fn a_regressing_variant_is_stopped_only_with_enough_evidence() {
        let outcomes = variant_outcomes(&rows((40, 30), (40, 10)));
        let decisions = evaluate("pluck", &variants(), &outcomes, &cfg());
        assert!(
            matches!(decisions[0], Decision::Stop { .. }),
            "{decisions:?}"
        );

        let thin = variant_outcomes(&rows((40, 30), (10, 1)));
        let decisions = evaluate("pluck", &variants(), &thin, &cfg());
        assert!(matches!(decisions[0], Decision::Continue { .. }));

        let fine = variant_outcomes(&rows((40, 30), (40, 27)));
        let decisions = evaluate("pluck", &variants(), &fine, &cfg());
        assert!(matches!(decisions[0], Decision::Continue { .. }));
    }

    #[test]
    fn infrastructure_rows_are_excluded_from_the_rate() {
        let mut r = rows((40, 30), (40, 30));
        for _ in 0..20 {
            r.push(row("pluck-v2", "infrastructure_failure"));
        }
        let outcomes = variant_outcomes(&r);
        assert_eq!(outcomes["pluck-v2"].judged(), 40);
        assert_eq!(outcomes["pluck-v2"].success_rate(), Some(0.75));
    }

    #[test]
    fn stop_receipt_round_trips_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let outcomes = variant_outcomes(&rows((40, 30), (40, 10)));
        let decision = evaluate("pluck", &variants(), &outcomes, &cfg()).remove(0);
        assert!(!is_stopped(dir.path(), "pluck", "v2"));
        assert!(record_stop(dir.path(), &decision).unwrap());
        assert!(is_stopped(dir.path(), "pluck", "v2"));
        assert!(!record_stop(dir.path(), &decision).unwrap());
        let receipt: StopReceipt = serde_json::from_str(
            &std::fs::read_to_string(stop_file(dir.path(), "pluck", "v2")).unwrap(),
        )
        .unwrap();
        assert!(matches!(receipt.decision, Decision::Stop { margin, .. } if margin == 0.15));
        let cont = Decision::Continue {
            template: "pluck".into(),
            variant: "v3".into(),
            variant_rate: None,
            baseline_rate: None,
            variant_attempts: 0,
            baseline_attempts: 0,
        };
        assert!(!record_stop(dir.path(), &cont).unwrap());
    }

    #[test]
    fn fixture_experiment_marks_better_candidate_promotable_with_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let prompt_dir = dir.path().join("templates");
        std::fs::create_dir_all(&prompt_dir).unwrap();
        let template_path = prompt_dir.join("pluck-v2.md");
        let config_path = dir.path().join("prompt.yaml");
        std::fs::write(&template_path, "candidate prompt bytes").unwrap();
        std::fs::write(&config_path, "prompt: { variants: {} }\n").unwrap();
        let template_before = std::fs::read(&template_path).unwrap();
        let config_before = std::fs::read(&config_path).unwrap();

        let rows = policy_rows((20, 10), (20, 19), 0.10);
        let record = evaluate_policy_experiment(
            "pluck",
            &variants()[0],
            &rows,
            &fast_cfg(),
            None,
            Utc::now(),
        );
        assert_eq!(record.status, ExperimentStatus::Promotable);
        assert_eq!(record.share, 0.5);
        assert_eq!(
            record.primary_metric,
            ExperimentPrimaryMetric::VerifiedSuccessRate
        );
        assert!(record.receipt.is_some());
        assert!(record_experiment(&dir.path().join("state"), &record).unwrap());
        let saved = load_experiment(&dir.path().join("state"), "pluck", "v2")
            .unwrap()
            .unwrap();
        assert_eq!(saved, record);

        assert_eq!(std::fs::read(template_path).unwrap(), template_before);
        assert_eq!(std::fs::read(config_path).unwrap(), config_before);
    }

    #[test]
    fn degraded_window_pauses_without_stopping_and_restoration_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state");
        let variant = &variants()[0];
        let config = fast_cfg();
        let promotable = evaluate_policy_experiment(
            "pluck",
            variant,
            &policy_rows((20, 10), (20, 19), 0.10),
            &config,
            None,
            Utc::now(),
        );
        assert_eq!(promotable.status, ExperimentStatus::Promotable);
        assert!(record_experiment(&state_dir, &promotable).unwrap());

        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let paused = pause_experiment(&promotable, &workspace, Utc::now());
        assert_eq!(paused.status, ExperimentStatus::Paused);
        assert_eq!(paused.paused_status, Some(ExperimentStatus::Promotable));
        assert_eq!(
            paused.paused_workspace.as_deref(),
            Some(workspace.to_str().unwrap())
        );
        assert!(!is_stopped(&state_dir, "pluck", "v2"));
        assert!(record_experiment(&state_dir, &paused).unwrap());

        // A stale evaluator cannot erase a pause. Once the gate is healthy,
        // the explicit resume write restores the prior promotable state.
        let evaluated = evaluate_policy_experiment(
            "pluck",
            variant,
            &policy_rows((20, 10), (20, 19), 0.10),
            &config,
            Some(&paused),
            Utc::now(),
        );
        assert_eq!(evaluated.status, ExperimentStatus::Promotable);
        assert_eq!(evaluated.paused_status, None);
        assert!(!record_experiment(&state_dir, &evaluated).unwrap());
        assert!(record_resumed_experiment(&state_dir, &evaluated).unwrap());
        let restored = load_experiment(&state_dir, "pluck", "v2").unwrap().unwrap();
        assert_eq!(restored.status, ExperimentStatus::Promotable);
        assert_eq!(restored.paused_status, None);
        assert_eq!(restored.paused_workspace, None);
    }

    #[test]
    fn degraded_attempts_do_not_change_canary_evidence() {
        let mut rows = policy_rows((20, 10), (20, 19), 0.10);
        for row in &mut rows {
            row["gate_degraded"] = serde_json::json!(true);
        }
        let outcomes = variant_outcomes(&rows);
        assert!(outcomes.is_empty());

        let record = evaluate_policy_experiment(
            "pluck",
            &variants()[0],
            &rows,
            &fast_cfg(),
            None,
            Utc::now(),
        );
        assert_eq!(record.status, ExperimentStatus::Running);
        assert!(record.receipt.is_none());
    }

    #[test]
    fn fixture_guardrail_stop_writes_receipt_and_withdraws_exposure() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = fast_cfg();
        config.guardrails.max_cost_per_verified_close_usd = Some(0.05);
        let rows = policy_rows((20, 15), (20, 16), 0.10);
        let record =
            evaluate_policy_experiment("pluck", &variants()[0], &rows, &config, None, Utc::now());
        assert_eq!(record.status, ExperimentStatus::Stopped);
        let receipt = record.receipt.as_ref().expect("guardrail receipt");
        assert!(receipt.reason.contains("cost per verified close"));
        assert!(record_experiment(dir.path(), &record).unwrap());
        assert!(is_stopped(dir.path(), "pluck", "v2"));
        assert_eq!(
            assign_prompt_variant(dir.path(), "pluck", &variants(), "after-stop").unwrap(),
            None
        );
    }

    #[test]
    fn fixture_rollback_writes_receipt_and_exposure_never_exceeds_share() {
        let dir = tempfile::tempdir().unwrap();
        let quota_dir = tempfile::tempdir().unwrap();
        let mut assignments = Vec::new();
        for index in 0..100 {
            assignments.push(
                assign_prompt_variant(
                    quota_dir.path(),
                    "pluck",
                    &variants(),
                    &format!("attempt-{index:03}"),
                )
                .unwrap(),
            );
        }
        let candidate_count = assignments.iter().filter(|item| item.is_some()).count();
        assert!(
            candidate_count <= 50,
            "candidate exposure was {candidate_count}%"
        );
        assert_eq!(
            assign_prompt_variant(quota_dir.path(), "pluck", &variants(), "attempt-099").unwrap(),
            assignments[99]
        );

        let rows = policy_rows((20, 10), (20, 19), 0.10);
        let record = evaluate_policy_experiment(
            "pluck",
            &variants()[0],
            &rows,
            &fast_cfg(),
            None,
            Utc::now(),
        );
        let rolled_back =
            rollback_experiment(dir.path(), &record, "operator rollback fixture", Utc::now())
                .unwrap();
        assert_eq!(rolled_back.status, ExperimentStatus::RolledBack);
        assert_eq!(
            rolled_back.receipt.as_ref().unwrap().status,
            ExperimentStatus::RolledBack
        );
        assert!(load_experiment(dir.path(), "pluck", "v2")
            .unwrap()
            .is_some());
        assert!(is_stopped(dir.path(), "pluck", "v2"));
        assert_eq!(
            assign_prompt_variant(dir.path(), "pluck", &variants(), "after-rollback").unwrap(),
            None
        );
    }
}
