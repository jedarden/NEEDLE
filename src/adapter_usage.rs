//! Adapter-specific usage extraction for complete attempt accounting.
//!
//! The common attempt resolver already understands Claude Code's stream-json
//! usage. This module adds the native JSONL vocabularies used by Codex,
//! OpenCode, and oh-my-pi, then hands their totals to the same pricing table.
//! Unknown formats deliberately produce an uncosted attempt: unknown is not
//! free.

use crate::attempt_accounting::{AttemptUsage, UsageAccumulator};
use crate::cost::{PricingConfig, UsageBreakdown};
use crate::dispatch::{TokenUsage, UsageFormat};

/// Resolve captured output using the adapter's declared usage format.
pub fn resolve_usage(
    format: UsageFormat,
    extracted: &TokenUsage,
    reported_cost: Option<f64>,
    stdout: &str,
    model: &str,
    pricing: &PricingConfig,
) -> AttemptUsage {
    if extracted.input_tokens.is_some() || extracted.output_tokens.is_some() {
        let cost = reported_cost.or_else(|| crate::cost::estimate_cost(extracted, model, pricing));
        return AttemptUsage {
            tokens_in: extracted.input_tokens,
            tokens_out: extracted.output_tokens,
            estimated_cost_usd: cost,
            costed: cost.is_some(),
        };
    }

    let Some(streamed) = stream_usage(format, stdout) else {
        return AttemptUsage {
            estimated_cost_usd: reported_cost,
            costed: reported_cost.is_some(),
            ..AttemptUsage::default()
        };
    };
    let cost =
        reported_cost.or_else(|| crate::cost::estimate_breakdown_cost(&streamed, model, pricing));
    AttemptUsage {
        tokens_in: Some(streamed.input),
        tokens_out: Some(streamed.output),
        estimated_cost_usd: cost,
        costed: cost.is_some(),
    }
}

/// Replay captured adapter output into a priced usage breakdown.
pub fn stream_usage(format: UsageFormat, stdout: &str) -> Option<UsageBreakdown> {
    match format {
        UsageFormat::ClaudeStreamJson => UsageAccumulator::from_stream(stdout).total(),
        UsageFormat::CodexJsonl => codex_jsonl_usage(stdout),
        UsageFormat::OpencodeJsonl => opencode_jsonl_usage(stdout),
        UsageFormat::OmpJsonl => omp_jsonl_usage(stdout),
        UsageFormat::Unknown => None,
    }
}

/// Sum Codex `turn.completed` usage events.
pub fn codex_jsonl_usage(stdout: &str) -> Option<UsageBreakdown> {
    let mut total = UsageBreakdown::default();
    for line in stdout.lines() {
        if !line.contains("\"turn.completed\"") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("type").and_then(|value| value.as_str()) != Some("turn.completed") {
            continue;
        }
        let usage = &value["usage"];
        let count = |key: &str| usage.get(key).and_then(|value| value.as_u64()).unwrap_or(0);
        let cache_read = count("cached_input_tokens");
        total.input += count("input_tokens").saturating_sub(cache_read);
        total.output += count("output_tokens");
        total.cache_read += cache_read;
        total.cache_write += count("cache_write_input_tokens");
    }
    (total != UsageBreakdown::default()).then_some(total)
}

/// Sum OpenCode `step_finish.part.tokens` usage events.
pub fn opencode_jsonl_usage(stdout: &str) -> Option<UsageBreakdown> {
    let mut total = UsageBreakdown::default();
    for line in stdout.lines() {
        if !line.contains("\"step_finish\"") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("type").and_then(|value| value.as_str()) != Some("step_finish") {
            continue;
        }
        let Some(tokens) = value.pointer("/part/tokens") else {
            continue;
        };
        let count = |path: &str| {
            tokens
                .pointer(path)
                .and_then(|value| value.as_u64())
                .unwrap_or(0)
        };
        let input = count("/input");
        let output = count("/output") + count("/reasoning");
        if input == 0 && output == 0 {
            continue;
        }
        total.input += input;
        total.output += output;
        total.cache_read += count("/cache/read");
        total.cache_write += count("/cache/write");
    }
    (total != UsageBreakdown::default()).then_some(total)
}

/// Sum OMP assistant `message_end.message.usage` events.
pub fn omp_jsonl_usage(stdout: &str) -> Option<UsageBreakdown> {
    let mut total = UsageBreakdown::default();
    for line in stdout.lines() {
        if !line.contains("\"message_end\"") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("type").and_then(|value| value.as_str()) != Some("message_end") {
            continue;
        }
        let usage = &value["message"]["usage"];
        let count = |key: &str| usage.get(key).and_then(|value| value.as_u64()).unwrap_or(0);
        let input = count("input");
        let output = count("output");
        if input == 0 && output == 0 {
            continue;
        }
        total.input += input;
        total.output += output;
        total.cache_read += count("cacheRead");
        total.cache_write += count("cacheWrite");
    }
    (total != UsageBreakdown::default()).then_some(total)
}
