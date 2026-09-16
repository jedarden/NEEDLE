//! Cost tracking: token extraction integration, pricing, cost estimation,
//! and budget enforcement.
//!
//! Token extraction itself lives in `dispatch` (adapter-specific). This module
//! ties token counts to per-model pricing, records effort telemetry, and
//! enforces daily budget thresholds.
//!
//! Depends on: `types`, `config`, `dispatch` (for `TokenUsage`).

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::{ConfigTier, ReloadTier};
use crate::dispatch::TokenUsage;

// ──────────────────────────────────────────────────────────────────────────────
// ModelPricing
// ──────────────────────────────────────────────────────────────────────────────

/// Per-model pricing in USD per million tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    /// Cost per million input tokens (USD).
    pub input_per_million: f64,
    /// Cost per million output tokens (USD).
    pub output_per_million: f64,
    /// Cost per million cache-read input tokens (USD). When absent, a tenth
    /// of the input rate: the ratio Anthropic publishes and the Claude CLI
    /// applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_per_million: Option<f64>,
    /// Cost per million cache-write input tokens (USD). When absent, 1.25×
    /// the input rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_per_million: Option<f64>,
}

/// Pricing configuration: maps model name → pricing.
pub type PricingConfig = HashMap<String, ModelPricing>;

/// Return built-in default pricing for known models.
pub fn default_pricing() -> PricingConfig {
    let mut m = HashMap::new();
    m.insert(
        "claude-sonnet-4-6".to_string(),
        ModelPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
            cache_read_per_million: None,
            cache_write_per_million: None,
        },
    );
    m.insert(
        "claude-opus-4-6".to_string(),
        ModelPricing {
            input_per_million: 15.0,
            output_per_million: 75.0,
            cache_read_per_million: None,
            cache_write_per_million: None,
        },
    );
    m.insert(
        "gpt-4".to_string(),
        ModelPricing {
            input_per_million: 30.0,
            output_per_million: 60.0,
            cache_read_per_million: None,
            cache_write_per_million: None,
        },
    );
    // The fleet's GLM models (through zai-proxy), at the rates the Claude CLI
    // applies to them in its result envelope. Pricing twelve completed
    // attempts from 2026-09-15 at these rates reproduces the envelope's
    // `total_cost_usd` to within 1-6%, the gap being side requests the stream
    // never shows. One basis keeps a killed attempt's estimate comparable with
    // a completed attempt's reported cost (N-T47, ADR-030).
    for model in ["glm-5.3", "glm-5.3-flash"] {
        m.insert(
            model.to_string(),
            ModelPricing {
                input_per_million: 5.0,
                output_per_million: 25.0,
                cache_read_per_million: Some(0.5),
                cache_write_per_million: Some(6.25),
            },
        );
    }
    m
}

// ──────────────────────────────────────────────────────────────────────────────
// BudgetConfig
// ──────────────────────────────────────────────────────────────────────────────

/// Daily budget thresholds in USD.
///
/// `warn_usd`: emit a warning event when daily spend exceeds this.
/// `stop_usd`: halt all workers when daily spend exceeds this.
/// A value of 0.0 disables the respective threshold.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    /// Emit warning when daily cost exceeds this (0 = disabled).
    #[serde(default)]
    pub warn_usd: f64,
    /// Stop workers when daily cost exceeds this (0 = disabled).
    #[serde(default)]
    pub stop_usd: f64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        BudgetConfig {
            warn_usd: 0.0,
            stop_usd: 0.0,
        }
    }
}

impl ConfigTier for BudgetConfig {
    fn reload_tier(&self) -> ReloadTier {
        ReloadTier::Live
    }
}

impl ConfigTier for PricingConfig {
    fn reload_tier(&self) -> ReloadTier {
        ReloadTier::Live
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Cost estimation
// ──────────────────────────────────────────────────────────────────────────────

/// Estimate cost in USD from token usage and model pricing.
///
/// Returns `None` if:
/// - The model is not in the pricing config
/// - Token usage has no input or output tokens
///
/// Cost formula: `(input_tokens * input_rate + output_tokens * output_rate) / 1_000_000`
pub fn estimate_cost(tokens: &TokenUsage, model: &str, pricing: &PricingConfig) -> Option<f64> {
    let price = pricing.get(model)?;

    let input = tokens.input_tokens.unwrap_or(0) as f64;
    let output = tokens.output_tokens.unwrap_or(0) as f64;

    // If both are zero, there's nothing to price.
    if input == 0.0 && output == 0.0 {
        return None;
    }

    let cost = (input * price.input_per_million + output * price.output_per_million) / 1_000_000.0;
    Some(cost)
}

/// Token counts by kind for one attempt, as summed from its stream.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageBreakdown {
    /// Input tokens, excluding cache reads and writes.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Input tokens read from the prompt cache.
    pub cache_read: u64,
    /// Input tokens written to the prompt cache.
    pub cache_write: u64,
}

/// Estimate cost in USD from a usage breakdown, cache tokens included.
///
/// Cache reads and writes are priced at the model's cache rates (see
/// [`ModelPricing`]); on a long cached session they are most of the bill.
/// Returns `None` when the model is not priced or nothing was used.
pub fn estimate_breakdown_cost(
    usage: &UsageBreakdown,
    model: &str,
    pricing: &PricingConfig,
) -> Option<f64> {
    let price = pricing.get(model)?;
    if *usage == UsageBreakdown::default() {
        return None;
    }
    let cache_read_rate = price
        .cache_read_per_million
        .unwrap_or(price.input_per_million * 0.1);
    let cache_write_rate = price
        .cache_write_per_million
        .unwrap_or(price.input_per_million * 1.25);
    let cost = (usage.input as f64 * price.input_per_million
        + usage.output as f64 * price.output_per_million
        + usage.cache_read as f64 * cache_read_rate
        + usage.cache_write as f64 * cache_write_rate)
        / 1_000_000.0;
    Some(cost)
}

// ──────────────────────────────────────────────────────────────────────────────
// EffortData — transient data for a single bead processing cycle
// ──────────────────────────────────────────────────────────────────────────────

/// Effort data collected during a single bead processing cycle.
///
/// Populated incrementally as the worker progresses through states, then
/// emitted as an `effort.recorded` telemetry event in the LOGGING state.
#[derive(Debug, Clone)]
pub struct EffortData {
    /// Time the cycle started (claim time).
    pub cycle_start: std::time::Instant,
    /// Agent adapter name used.
    pub agent_name: String,
    /// Model identifier (from adapter config).
    pub model: Option<String>,
    /// Provider name (e.g., "anthropic", "openai").
    pub provider: Option<String>,
    /// Extracted token usage from agent output.
    pub tokens: TokenUsage,
    /// Estimated cost in USD (None if pricing not configured for this model).
    pub estimated_cost_usd: Option<f64>,
}

// ──────────────────────────────────────────────────────────────────────────────
// BudgetCheck
// ──────────────────────────────────────────────────────────────────────────────

/// Result of checking daily spend against budget thresholds.
#[derive(Debug, Clone, PartialEq)]
pub enum BudgetCheck {
    /// Spend is within limits (or budget is disabled).
    Ok,
    /// Spend exceeds warn_usd threshold.
    Warn { daily_cost: f64, threshold: f64 },
    /// Spend exceeds stop_usd threshold — worker should halt.
    Stop { daily_cost: f64, threshold: f64 },
}

/// Check daily cost against budget thresholds.
///
/// Returns `BudgetCheck::Stop` if stop threshold is exceeded,
/// `BudgetCheck::Warn` if only warn threshold is exceeded,
/// `BudgetCheck::Ok` otherwise.
///
/// A threshold of 0.0 means "disabled" — never triggers.
pub fn check_budget(daily_cost: f64, budget: &BudgetConfig) -> BudgetCheck {
    // Stop takes precedence over warn.
    if budget.stop_usd > 0.0 && daily_cost >= budget.stop_usd {
        return BudgetCheck::Stop {
            daily_cost,
            threshold: budget.stop_usd,
        };
    }
    if budget.warn_usd > 0.0 && daily_cost >= budget.warn_usd {
        return BudgetCheck::Warn {
            daily_cost,
            threshold: budget.warn_usd,
        };
    }
    BudgetCheck::Ok
}

// ──────────────────────────────────────────────────────────────────────────────
// Daily cost scanning from telemetry logs
// ──────────────────────────────────────────────────────────────────────────────

/// Scan today's telemetry log files and sum estimated costs from
/// `effort.recorded` events.
///
/// Scans all `*.jsonl` files in `log_dir`. Only events from today (UTC)
/// with a non-null `estimated_cost_usd` data field are included.
///
/// This is best-effort: corrupt lines are skipped, missing directories
/// return 0.0.
pub fn scan_daily_cost(log_dir: &Path) -> f64 {
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let entries = match std::fs::read_dir(log_dir) {
        Ok(e) => e,
        Err(_) => return 0.0,
    };

    let mut total = 0.0;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }

        let contents = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        for line in contents.lines() {
            // Quick filter before parsing full JSON.
            if !line.contains("effort.recorded") {
                continue;
            }

            let parsed: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            // Verify event_type and today's date.
            if parsed.get("event_type").and_then(|v| v.as_str()) != Some("effort.recorded") {
                continue;
            }
            let ts = match parsed.get("timestamp").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => continue,
            };
            if !ts.starts_with(&today) {
                continue;
            }

            // Extract estimated_cost_usd from the data payload.
            if let Some(cost) = parsed
                .get("data")
                .and_then(|d| d.get("estimated_cost_usd"))
                .and_then(|v| v.as_f64())
            {
                total += cost;
            }
        }
    }

    total
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pricing() -> PricingConfig {
        let mut m = HashMap::new();
        m.insert(
            "claude-sonnet-4-6".to_string(),
            ModelPricing {
                input_per_million: 3.0,
                output_per_million: 15.0,
                cache_read_per_million: None,
                cache_write_per_million: None,
            },
        );
        m.insert(
            "claude-opus-4-6".to_string(),
            ModelPricing {
                input_per_million: 15.0,
                output_per_million: 75.0,
                cache_read_per_million: None,
                cache_write_per_million: None,
            },
        );
        m
    }

    // ── estimate_breakdown_cost (N-T47) ──

    #[test]
    fn nt47_breakdown_cost_prices_cache_tokens_at_cache_rates() {
        let million = UsageBreakdown {
            input: 1_000_000,
            output: 1_000_000,
            cache_read: 1_000_000,
            cache_write: 1_000_000,
        };
        // No cache rates configured: a tenth and 1.25x of the input rate.
        let cost = estimate_breakdown_cost(&million, "claude-sonnet-4-6", &test_pricing()).unwrap();
        assert!(
            (cost - (3.0 + 15.0 + 0.3 + 3.75)).abs() < 1e-9,
            "cost {cost}"
        );
        // Configured cache rates win.
        let cost = estimate_breakdown_cost(&million, "glm-5.3-flash", &default_pricing()).unwrap();
        assert!(
            (cost - (5.0 + 25.0 + 0.5 + 6.25)).abs() < 1e-9,
            "cost {cost}"
        );
        // Nothing used, or nothing priced, is no estimate.
        assert!(estimate_breakdown_cost(
            &UsageBreakdown::default(),
            "claude-sonnet-4-6",
            &test_pricing()
        )
        .is_none());
        assert!(estimate_breakdown_cost(&million, "unpriced-model", &test_pricing()).is_none());
    }

    #[test]
    fn nt47_default_pricing_covers_the_fleet_glm_models() {
        let pricing = default_pricing();
        for model in ["glm-5.3", "glm-5.3-flash"] {
            let price = &pricing[model];
            assert_eq!(
                (
                    price.input_per_million,
                    price.output_per_million,
                    price.cache_read_per_million,
                    price.cache_write_per_million
                ),
                (5.0, 25.0, Some(0.5), Some(6.25)),
                "{model}"
            );
        }
    }

    // ── estimate_cost ──

    #[test]
    fn estimate_cost_sonnet_typical() {
        let tokens = TokenUsage {
            input_tokens: Some(10_000),
            output_tokens: Some(2_000),
        };
        let cost = estimate_cost(&tokens, "claude-sonnet-4-6", &test_pricing()).unwrap();
        // 10000 * 3.0 / 1M + 2000 * 15.0 / 1M = 0.030 + 0.030 = 0.060
        assert!((cost - 0.060).abs() < 0.0001, "expected ~0.060, got {cost}");
    }

    #[test]
    fn estimate_cost_opus_typical() {
        let tokens = TokenUsage {
            input_tokens: Some(50_000),
            output_tokens: Some(10_000),
        };
        let cost = estimate_cost(&tokens, "claude-opus-4-6", &test_pricing()).unwrap();
        // 50000 * 15.0 / 1M + 10000 * 75.0 / 1M = 0.75 + 0.75 = 1.50
        assert!((cost - 1.50).abs() < 0.001, "expected ~1.50, got {cost}");
    }

    #[test]
    fn estimate_cost_unknown_model_returns_none() {
        let tokens = TokenUsage {
            input_tokens: Some(1000),
            output_tokens: Some(500),
        };
        assert!(estimate_cost(&tokens, "unknown-model", &test_pricing()).is_none());
    }

    #[test]
    fn estimate_cost_zero_tokens_returns_none() {
        let tokens = TokenUsage {
            input_tokens: Some(0),
            output_tokens: Some(0),
        };
        assert!(estimate_cost(&tokens, "claude-sonnet-4-6", &test_pricing()).is_none());
    }

    #[test]
    fn estimate_cost_no_tokens_returns_none() {
        let tokens = TokenUsage::default();
        assert!(estimate_cost(&tokens, "claude-sonnet-4-6", &test_pricing()).is_none());
    }

    #[test]
    fn estimate_cost_input_only() {
        let tokens = TokenUsage {
            input_tokens: Some(1_000_000),
            output_tokens: None,
        };
        let cost = estimate_cost(&tokens, "claude-sonnet-4-6", &test_pricing()).unwrap();
        // 1M * 3.0 / 1M = 3.00
        assert!((cost - 3.0).abs() < 0.001, "expected ~3.00, got {cost}");
    }

    #[test]
    fn estimate_cost_output_only() {
        let tokens = TokenUsage {
            input_tokens: None,
            output_tokens: Some(1_000_000),
        };
        let cost = estimate_cost(&tokens, "claude-sonnet-4-6", &test_pricing()).unwrap();
        // 1M * 15.0 / 1M = 15.00
        assert!((cost - 15.0).abs() < 0.001, "expected ~15.00, got {cost}");
    }

    // ── default_pricing ──

    #[test]
    fn default_pricing_has_known_models() {
        let p = default_pricing();
        assert!(p.contains_key("claude-sonnet-4-6"));
        assert!(p.contains_key("claude-opus-4-6"));
        assert!(p.contains_key("gpt-4"));
    }

    #[test]
    fn default_pricing_rates_are_positive() {
        for (model, pricing) in default_pricing() {
            assert!(
                pricing.input_per_million > 0.0,
                "{model}: input rate should be positive"
            );
            assert!(
                pricing.output_per_million > 0.0,
                "{model}: output rate should be positive"
            );
        }
    }

    // ── check_budget ──

    #[test]
    fn budget_disabled_returns_ok() {
        let budget = BudgetConfig {
            warn_usd: 0.0,
            stop_usd: 0.0,
        };
        assert_eq!(check_budget(100.0, &budget), BudgetCheck::Ok);
    }

    #[test]
    fn budget_below_warn_returns_ok() {
        let budget = BudgetConfig {
            warn_usd: 10.0,
            stop_usd: 50.0,
        };
        assert_eq!(check_budget(5.0, &budget), BudgetCheck::Ok);
    }

    #[test]
    fn budget_at_warn_returns_warn() {
        let budget = BudgetConfig {
            warn_usd: 10.0,
            stop_usd: 50.0,
        };
        assert_eq!(
            check_budget(10.0, &budget),
            BudgetCheck::Warn {
                daily_cost: 10.0,
                threshold: 10.0,
            }
        );
    }

    #[test]
    fn budget_between_warn_and_stop() {
        let budget = BudgetConfig {
            warn_usd: 10.0,
            stop_usd: 50.0,
        };
        assert_eq!(
            check_budget(25.0, &budget),
            BudgetCheck::Warn {
                daily_cost: 25.0,
                threshold: 10.0,
            }
        );
    }

    #[test]
    fn budget_at_stop_returns_stop() {
        let budget = BudgetConfig {
            warn_usd: 10.0,
            stop_usd: 50.0,
        };
        assert_eq!(
            check_budget(50.0, &budget),
            BudgetCheck::Stop {
                daily_cost: 50.0,
                threshold: 50.0,
            }
        );
    }

    #[test]
    fn budget_above_stop_returns_stop() {
        let budget = BudgetConfig {
            warn_usd: 10.0,
            stop_usd: 50.0,
        };
        assert_eq!(
            check_budget(100.0, &budget),
            BudgetCheck::Stop {
                daily_cost: 100.0,
                threshold: 50.0,
            }
        );
    }

    #[test]
    fn budget_warn_only_no_stop() {
        let budget = BudgetConfig {
            warn_usd: 5.0,
            stop_usd: 0.0,
        };
        assert_eq!(
            check_budget(10.0, &budget),
            BudgetCheck::Warn {
                daily_cost: 10.0,
                threshold: 5.0,
            }
        );
    }

    #[test]
    fn budget_stop_only_no_warn() {
        let budget = BudgetConfig {
            warn_usd: 0.0,
            stop_usd: 20.0,
        };
        // Below stop, no warn configured
        assert_eq!(check_budget(15.0, &budget), BudgetCheck::Ok);
        // At stop
        assert_eq!(
            check_budget(20.0, &budget),
            BudgetCheck::Stop {
                daily_cost: 20.0,
                threshold: 20.0,
            }
        );
    }

    // ── scan_daily_cost ──

    #[test]
    fn scan_daily_cost_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(scan_daily_cost(dir.path()), 0.0);
    }

    #[test]
    fn scan_daily_cost_missing_dir() {
        assert_eq!(scan_daily_cost(Path::new("/nonexistent/path")), 0.0);
    }

    #[test]
    fn scan_daily_cost_sums_todays_events() {
        let dir = tempfile::tempdir().unwrap();
        let today = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

        let log_content = format!(
            r#"{{"timestamp":"{today}","event_type":"effort.recorded","worker_id":"w1","session_id":"aa","sequence":0,"data":{{"estimated_cost_usd":1.50}}}}
{{"timestamp":"{today}","event_type":"effort.recorded","worker_id":"w1","session_id":"aa","sequence":1,"data":{{"estimated_cost_usd":2.25}}}}
{{"timestamp":"{today}","event_type":"bead.completed","worker_id":"w1","session_id":"aa","sequence":2,"data":{{"bead_id":"nd-x"}}}}
{{"timestamp":"2025-01-01T00:00:00Z","event_type":"effort.recorded","worker_id":"w1","session_id":"aa","sequence":3,"data":{{"estimated_cost_usd":99.99}}}}"#,
        );

        std::fs::write(dir.path().join("worker-aa.jsonl"), log_content).unwrap();

        let total = scan_daily_cost(dir.path());
        assert!((total - 3.75).abs() < 0.001, "expected ~3.75, got {total}");
    }

    #[test]
    fn scan_daily_cost_handles_null_cost() {
        let dir = tempfile::tempdir().unwrap();
        let today = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

        let log_content = format!(
            r#"{{"timestamp":"{today}","event_type":"effort.recorded","worker_id":"w1","session_id":"aa","sequence":0,"data":{{"estimated_cost_usd":null,"tokens_in":null}}}}
{{"timestamp":"{today}","event_type":"effort.recorded","worker_id":"w1","session_id":"aa","sequence":1,"data":{{"estimated_cost_usd":1.00}}}}"#,
        );

        std::fs::write(dir.path().join("worker-bb.jsonl"), log_content).unwrap();

        let total = scan_daily_cost(dir.path());
        assert!((total - 1.0).abs() < 0.001, "expected ~1.00, got {total}");
    }

    #[test]
    fn scan_daily_cost_skips_corrupt_lines() {
        let dir = tempfile::tempdir().unwrap();
        let today = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

        let valid_line = format!(
            r#"{{"timestamp":"{today}","event_type":"effort.recorded","worker_id":"w1","session_id":"aa","sequence":0,"data":{{"estimated_cost_usd":5.0}}}}"#,
        );
        let log_content = format!("not valid json effort.recorded\n{valid_line}\n");

        std::fs::write(dir.path().join("worker-cc.jsonl"), log_content).unwrap();

        let total = scan_daily_cost(dir.path());
        assert!((total - 5.0).abs() < 0.001, "expected ~5.0, got {total}");
    }

    #[test]
    fn scan_daily_cost_ignores_non_jsonl_files() {
        let dir = tempfile::tempdir().unwrap();
        let today = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

        let content = format!(
            r#"{{"timestamp":"{today}","event_type":"effort.recorded","worker_id":"w1","session_id":"aa","sequence":0,"data":{{"estimated_cost_usd":10.0}}}}"#,
        );

        std::fs::write(dir.path().join("data.txt"), &content).unwrap();
        std::fs::write(dir.path().join("data.jsonl"), &content).unwrap();

        let total = scan_daily_cost(dir.path());
        assert!(
            (total - 10.0).abs() < 0.001,
            "expected ~10.0 (only .jsonl), got {total}"
        );
    }
}
