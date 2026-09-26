//! Adapter-specific usage extraction for complete attempt accounting.
//!
//! The common attempt resolver already understands Claude Code's stream-json
//! usage. This module adds the native JSONL vocabularies used by Codex,
//! OpenCode, and oh-my-pi, plus Aider's plain-text session summary, then
//! hands their totals to the same pricing table. Unknown formats deliberately
//! produce an uncosted attempt: unknown is not free.

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
        UsageFormat::AiderSummary => aider_summary_usage(stdout),
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

/// Parse the Aider `Tokens: … sent, … received.` session summary line.
///
/// Aider prints the line once when a run finishes, assembling it as
/// `Tokens: {sent} sent[, {n} cache write][, {n} cache hit], {received} received.`
/// (`aider/coders/base_coder.py`). Every count goes through `format_tokens`
/// (`aider/utils.py`): below 1,000 it prints in full, at or above it
/// abbreviates to `1.5k` / `15k`. Two consequences the legacy two-group
/// regex missed: abbreviated counts never match a digits-only group, and on
/// cached models (the default for Anthropic) the cache segments sit between
/// `sent` and `received`, breaking their adjacency.
///
/// Mapping follows aider's own semantics: `sent` is the full input the
/// provider was billed for prompt tokens on (aider does not decompose it
/// further on this line), `received` is output; `cache write`/`cache hit`
/// land in the corresponding breakdown buckets.
pub fn aider_summary_usage(stdout: &str) -> Option<UsageBreakdown> {
    let line = stdout.lines().find(|line| {
        let line = line.trim_start();
        line.starts_with("Tokens:") && line.contains(" sent") && line.contains(" received")
    })?;
    let body = line.trim_start_matches("Tokens:").trim_end_matches('.');
    let mut breakdown = UsageBreakdown::default();
    let mut saw_sent = false;
    let mut saw_received = false;
    for segment in body.split(',') {
        let mut fields = segment.split_whitespace();
        let (Some(count), Some(label)) = (fields.next(), fields.next()) else {
            continue;
        };
        let Some(count) = parse_aider_count(count) else {
            continue;
        };
        match label {
            "sent" => {
                breakdown.input = count;
                saw_sent = true;
            }
            "received" => {
                breakdown.output = count;
                saw_received = true;
            }
            "cache" => match fields.next() {
                Some("write") => breakdown.cache_write = count,
                Some("hit") => breakdown.cache_read = count,
                _ => {}
            },
            _ => {}
        }
    }
    (saw_sent && saw_received).then_some(breakdown)
}

/// Scale one aider `format_tokens` count back to a token total.
///
/// `1.5k` → 1500, `15k` → 15000, `850` → 850. Anything else (including the
/// `m` suffix this helper never emits today) yields `None` rather than a
/// wrong number.
fn parse_aider_count(field: &str) -> Option<u64> {
    let (digits, scale) = match field.strip_suffix('k') {
        Some(digits) => (digits, 1_000_f64),
        None => (field, 1.0),
    };
    let value: f64 = digits.parse().ok()?;
    if !(value.is_finite() && value >= 0.0) {
        return None;
    }
    Some((value * scale).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The recorded fixture: every summary shape aider emits, provenance in
    /// its header (source-verified against Aider-AI/aider@main, 2026-09-26).
    const AIDER_FIXTURE: &str = include_str!("../tests/fixtures/adapter-usage/aider-summary.txt");

    #[test]
    fn aider_summary_scales_abbreviations_and_tracks_cache_segments() {
        let usage = stream_usage(UsageFormat::AiderSummary, AIDER_FIXTURE)
            .expect("cached fixture line should carry usage");
        assert_eq!(usage.input, 12_500, "'12.5k sent' scales to 12500");
        assert_eq!(usage.output, 4_300, "'4.3k received.' scales to 4300");
        assert_eq!(usage.cache_write, 8_100);
        assert_eq!(usage.cache_read, 2_100);
    }

    #[test]
    fn aider_summary_handles_uncached_and_full_number_forms() {
        let uncached = stream_usage(
            UsageFormat::AiderSummary,
            "Tokens: 3.2k sent, 1.1k received.\n",
        )
        .expect("uncached line should parse");
        assert_eq!((uncached.input, uncached.output), (3_200, 1_100));
        assert_eq!((uncached.cache_write, uncached.cache_read), (0, 0));

        let small = stream_usage(
            UsageFormat::AiderSummary,
            "Tokens: 850 sent, 120 received.\n",
        )
        .expect("sub-1000 counts parse without a suffix");
        assert_eq!((small.input, small.output), (850, 120));
    }

    #[test]
    fn aider_summary_rejects_malformed_and_empty_output() {
        assert!(aider_summary_usage("no summary here").is_none());
        assert!(aider_summary_usage("Tokens: not-a-number sent, 5 received.").is_none());
        assert!(aider_summary_usage("").is_none());
        // A "sent" of zero with a real "received" still counts as usage.
        let usage =
            aider_summary_usage("Tokens: 0 sent, 7 received.\n").expect("asymmetric line parses");
        assert_eq!((usage.input, usage.output), (0, 7));
    }

    #[test]
    fn aider_legacy_regex_extraction_misses_real_summary_lines() {
        // Why the AiderSummary parser replaced the adapter's original
        // `Tokens:\s+([\d,]+)\s+sent,\s+([\d,]+)\s+received` regex: aider
        // abbreviates every count >= 1000 (`12.5k`) and interleaves cache
        // segments between `sent` and `received`, so the numeric sample the
        // regex was built against matches neither real form.
        let legacy = crate::dispatch::TokenExtraction::Regex {
            pattern: r"Tokens:\s+([\d,]+)\s+sent,\s+([\d,]+)\s+received".to_string(),
            input_group: 1,
            output_group: 2,
        };
        for line in [
            "Tokens: 12.5k sent, 8.1k cache write, 2.1k cache hit, 4.3k received.",
            "Tokens: 3.2k sent, 1.1k received.",
        ] {
            let usage = crate::dispatch::extract_tokens(&legacy, line, "");
            assert!(
                usage.input_tokens.is_none() && usage.output_tokens.is_none(),
                "legacy regex must not match real aider output: {line}"
            );
        }
    }
}
