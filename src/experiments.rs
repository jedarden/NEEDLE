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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
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
}

impl VariantOutcome {
    pub fn judged(&self) -> u64 {
        self.attempts.saturating_sub(self.infrastructure)
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

/// Aggregate `attempt.resolved` rows by template version.
pub fn variant_outcomes(rows: &[serde_json::Value]) -> HashMap<String, VariantOutcome> {
    let mut out: HashMap<String, VariantOutcome> = HashMap::new();
    for row in rows {
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

/// Default receipt directory: `~/.needle/state/experiments`.
pub fn default_state_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    home.join(".needle").join("state").join("experiments")
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
    stop_file(state_dir, template, variant).is_file()
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
            "prompt_template": "pluck",
            "template_version": version,
            "outcome": outcome
        })
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
}
