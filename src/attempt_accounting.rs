//! Complete attempt accounting (ADR-030, plan section 4.9).
//!
//! The `attempt.resolved` ledger is the evidence every learning consumer
//! reads: `needle stats`, evidence routing, prompt-variant canaries, and the
//! ADR-029 proposal generator after them. The classification rules that keep
//! that evidence honest live here as pure functions. The outcome handler
//! applies them to the row it emits and consumers read the resulting class,
//! so no consumer re-derives a rule and a new consumer cannot get one wrong
//! (the alternative ADR-030 rejected).
//!
//! # N-T46: a decomposition is not a success
//!
//! When an attempt splits its bead into children instead of delivering the
//! work, its row resolves [`DECOMPOSED`]. It earns no verified credit and is
//! no failure; delivered work on the children earns credit on the children's
//! own attempts. Before this class existed the auto-split template's
//! successes were booked as `verified_success`, which is why verified yield
//! read 86% on fourth attempts and 97% on fifth-or-later attempts in the
//! 2026-09-12..14 ledger (plan section 4.9).
//!
//! # N-T47: every dispatched attempt is charged
//!
//! Cost used to come only from the final `type="result"` envelope, which a
//! killed process never writes: 124 of 852 attempts (15%) timed out in the
//! same window and every one was booked at $0. [`resolve_usage`] keeps the
//! envelope as the total of record when there is one and otherwise sums the
//! adapter's own per-turn usage reports ([`UsageAccumulator`]), priced from
//! the pricing table. An attempt with no usage at all is `costed: false`:
//! unknown, never free.

use std::collections::HashMap;

use crate::cost::{PricingConfig, UsageBreakdown};
use crate::dispatch::TokenUsage;
use crate::mitosis::AUTO_SPLIT_PARENT_LABEL;

/// Wire outcome of an attempt that decomposed its bead instead of delivering
/// it: the `decomposed` variant of [`crate::telemetry::AttemptOutcome`].
pub const DECOMPOSED: &str = "decomposed";

/// Prompt template the worker dispatches once a bead reaches its auto-split
/// threshold (`strands.pluck.split_after_failures`).
pub const SPLIT_TEMPLATE: &str = "split";

const VERIFIED_SUCCESS: &str = "verified_success";

/// Outcome recorded in the bead backend for a decomposed attempt.
///
/// bead-rs's attempt-outcome-v1 vocabulary has no decomposition class and
/// rejects outcomes it does not know. `indeterminate` is the member with the
/// right effect there: it neither resets nor extends the bead's
/// consecutive-failure run and does not move its attempt tier. The row's
/// `decomposed:…` terminal reason travels with it as the resolution reason.
const BACKEND_DECOMPOSED: &str = "indeterminate";

/// Why an attempt resolved [`DECOMPOSED`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decomposition {
    /// The dispatch itself asked for a split: the auto-split template.
    SplitTemplate,
    /// The attempt made its bead an auto-split parent and delivered no commit.
    SplitParentWithoutCommits,
}

impl Decomposition {
    /// The ledger row's `terminal_reason`.
    pub fn terminal_reason(self) -> &'static str {
        match self {
            Decomposition::SplitTemplate => "decomposed:split_template",
            Decomposition::SplitParentWithoutCommits => "decomposed:split_parent_without_commits",
        }
    }
}

/// The facts about one resolved attempt that decide whether it decomposed.
#[derive(Debug, Clone, Copy)]
pub struct AttemptShape<'a> {
    /// Semantic outcome the attempt would otherwise resolve with.
    pub outcome: &'a str,
    /// Prompt template that built the dispatch prompt.
    pub prompt_template: &'a str,
    /// Whether the attempt created any commit in its workspace.
    pub delivered_commits: bool,
    /// The bead's labels when it was dispatched.
    pub labels_before: &'a [String],
    /// The bead's labels after the attempt, when they were read back.
    pub labels_after: Option<&'a [String]>,
}

/// Whether classifying an attempt needs the bead's post-attempt labels.
///
/// Only a commit-less verified success outside the split template can be a
/// decomposition the template name does not already reveal, so only that
/// shape pays for a backend read.
pub fn needs_labels_after(outcome: &str, prompt_template: &str, delivered_commits: bool) -> bool {
    outcome == VERIFIED_SUCCESS && prompt_template != SPLIT_TEMPLATE && !delivered_commits
}

/// Decide whether an attempt decomposed its bead (ADR-030 decision 1).
///
/// Only an attempt that would otherwise earn verified credit is
/// reclassified. A split attempt that failed or timed out already earns
/// none, and its failure evidence is what retries and routing need.
pub fn classify_decomposition(shape: &AttemptShape<'_>) -> Option<Decomposition> {
    if shape.outcome != VERIFIED_SUCCESS {
        return None;
    }
    if shape.prompt_template == SPLIT_TEMPLATE {
        return Some(Decomposition::SplitTemplate);
    }
    let is_split_parent = |labels: &[String]| labels.iter().any(|l| l == AUTO_SPLIT_PARENT_LABEL);
    let became_split_parent = shape
        .labels_after
        .is_some_and(|after| is_split_parent(after) && !is_split_parent(shape.labels_before));
    (became_split_parent && !shape.delivered_commits)
        .then_some(Decomposition::SplitParentWithoutCommits)
}

/// The outcome to record in the bead backend's own attempt ledger for a
/// NEEDLE ledger outcome. Every outcome but [`DECOMPOSED`] passes through.
pub fn backend_outcome(ledger_outcome: &str) -> &str {
    if ledger_outcome == DECOMPOSED {
        BACKEND_DECOMPOSED
    } else {
        ledger_outcome
    }
}

/// What one attempt consumed, as its ledger row records it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AttemptUsage {
    /// Input tokens, excluding cache reads and writes.
    pub tokens_in: Option<u64>,
    /// Output tokens.
    pub tokens_out: Option<u64>,
    /// Estimated cost in USD; absent when it could not be established.
    pub estimated_cost_usd: Option<f64>,
    /// Whether a cost was established. `false` means unknown, never free.
    pub costed: bool,
}

/// Resolve what an attempt consumed (ADR-030 decision 2).
///
/// The agent's own totals win when they exist: the configured extractor or
/// the final result envelope, whose reported cost is reproduced exactly. A
/// timed-out, crashed or interrupted attempt writes no envelope, so its
/// per-turn usage is summed from the stream ([`UsageAccumulator`]) and priced
/// from the pricing table. An adapter that reported no usage at all resolves
/// with no cost and `costed: false`.
pub fn resolve_usage(
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
    let Some(streamed) = UsageAccumulator::from_stream(stdout).total() else {
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

/// Sums per-message usage from a Claude stream-json transcript, one line at a
/// time.
///
/// Claude Code reports a message's usage more than once: each content block of
/// an assistant message is its own `assistant` line carrying the same
/// `message.usage`, and with partial messages enabled the `message_start` and
/// `message_delta` stream events carry it again. Which of those hold the counts
/// depends on the provider. For glm-5.3 through zai-proxy the `assistant` and
/// `message_start` usage is all zeros and only `message_delta` carries it; for
/// Anthropic models `message_start` carries the input and cache tokens and
/// `message_delta` the output. Each message therefore keeps the per-field
/// maximum over every report of it, and the attempt's usage is the sum over
/// messages.
///
/// A `message_delta` names no message. It belongs to the message most recently
/// started in its lane: the session plus the parent tool use, because a
/// subagent's messages interleave with the main conversation's.
///
/// Replayed over twelve completed glm-5.3 attempts from 2026-09-15, the
/// accumulated input tokens equal each result envelope's `usage.input_tokens`
/// exactly.
#[derive(Debug, Default)]
pub struct UsageAccumulator {
    per_message: HashMap<String, UsageBreakdown>,
    open_messages: HashMap<(Option<String>, Option<String>), String>,
    unattributed_deltas: usize,
}

impl UsageAccumulator {
    /// Accumulate every usage report in a captured stream.
    pub fn from_stream(stdout: &str) -> Self {
        let mut accumulator = Self::default();
        for line in stdout.lines() {
            accumulator.observe_line(line);
        }
        accumulator
    }

    /// Fold one stream line in. Lines that carry no usage are skipped before
    /// any parsing, which keeps a multi-megabyte transcript cheap.
    pub fn observe_line(&mut self, line: &str) {
        if !line.contains("\"usage\"") {
            return;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return;
        };
        let text = |v: &serde_json::Value, key: &str| {
            v.get(key).and_then(|s| s.as_str()).map(str::to_owned)
        };
        match value.get("type").and_then(|t| t.as_str()) {
            Some("assistant") => {
                let message = &value["message"];
                if let Some(id) = text(message, "id") {
                    self.record(id, &message["usage"]);
                }
            }
            Some("stream_event") => {
                let lane = (
                    text(&value, "session_id"),
                    text(&value, "parent_tool_use_id"),
                );
                let event = &value["event"];
                match event.get("type").and_then(|t| t.as_str()) {
                    Some("message_start") => {
                        let message = &event["message"];
                        if let Some(id) = text(message, "id") {
                            self.open_messages.insert(lane, id.clone());
                            self.record(id, &message["usage"]);
                        }
                    }
                    Some("message_delta") => {
                        let id = match self.open_messages.get(&lane) {
                            Some(id) => id.clone(),
                            // A delta whose start was never seen still counts,
                            // as a message of its own.
                            None => {
                                self.unattributed_deltas += 1;
                                format!("unattributed-delta-{}", self.unattributed_deltas)
                            }
                        };
                        self.record(id, &event["usage"]);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// The attempt's usage so far, or `None` when no report carried a nonzero
    /// count.
    pub fn total(&self) -> Option<UsageBreakdown> {
        let total = self
            .per_message
            .values()
            .fold(UsageBreakdown::default(), |sum, message| UsageBreakdown {
                input: sum.input + message.input,
                output: sum.output + message.output,
                cache_read: sum.cache_read + message.cache_read,
                cache_write: sum.cache_write + message.cache_write,
            });
        (total != UsageBreakdown::default()).then_some(total)
    }

    fn record(&mut self, message_id: String, usage: &serde_json::Value) {
        let count = |key: &str| usage.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
        let slot = self.per_message.entry(message_id).or_default();
        slot.input = slot.input.max(count("input_tokens"));
        slot.output = slot.output.max(count("output_tokens"));
        slot.cache_read = slot.cache_read.max(count("cache_read_input_tokens"));
        slot.cache_write = slot.cache_write.max(count("cache_creation_input_tokens"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KILLED_GLM_ATTEMPT: &str =
        include_str!("../tests/fixtures/claude-stream-json/killed-glm-attempt.jsonl");

    /// What the worker does with a finished dispatch's stdout.
    fn usage_of(stdout: &str, model: &str) -> AttemptUsage {
        let (extracted, reported_cost) = crate::dispatch::extract_tokens_with_envelope(
            &crate::dispatch::TokenExtraction::None,
            stdout,
            "",
        );
        resolve_usage(
            &extracted,
            reported_cost,
            stdout,
            model,
            &crate::cost::default_pricing(),
        )
    }

    #[test]
    fn nt47_a_killed_stream_is_charged_for_the_usage_it_reported() {
        let usage = usage_of(KILLED_GLM_ATTEMPT, "glm-5.3-flash");
        assert_eq!(
            (usage.tokens_in, usage.tokens_out),
            (Some(36_007), Some(1_100))
        );
        let cost = usage
            .estimated_cost_usd
            .expect("a killed attempt is charged");
        // Per million: 36,007 input × $5 + 1,100 output × $25
        // + 63,968 cache reads × $0.50.
        assert!((cost - 0.239_519).abs() < 1e-9, "cost {cost}");
        assert!(usage.costed);
    }

    #[test]
    fn nt47_a_result_envelope_reconciles_to_its_own_total_exactly() {
        let completed = format!(
            "{KILLED_GLM_ATTEMPT}{}\n",
            r#"{"type":"result","subtype":"success","is_error":false,"total_cost_usd":0.2481,"usage":{"input_tokens":36007,"output_tokens":1100,"cache_read_input_tokens":63968,"cache_creation_input_tokens":0}}"#
        );
        assert_eq!(
            usage_of(&completed, "glm-5.3-flash"),
            AttemptUsage {
                tokens_in: Some(36_007),
                tokens_out: Some(1_100),
                estimated_cost_usd: Some(0.2481),
                costed: true,
            }
        );
    }

    #[test]
    fn nt47_no_usage_reports_leave_the_attempt_uncosted_never_free() {
        let started_only: String = KILLED_GLM_ATTEMPT
            .lines()
            .take(3)
            .map(|line| format!("{line}\n"))
            .collect();
        for stdout in [
            "",
            "plain text from an adapter that reports no usage\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"done\"}}\n",
            // Killed before any message finished: only zero reports.
            started_only.as_str(),
        ] {
            assert_eq!(
                usage_of(stdout, "glm-5.3-flash"),
                AttemptUsage::default(),
                "{stdout:?}"
            );
        }
        // Tokens from a model without a price are known tokens at unknown cost.
        let unpriced = usage_of(KILLED_GLM_ATTEMPT, "unpriced-model");
        assert_eq!(unpriced.tokens_in, Some(36_007));
        assert_eq!(unpriced.estimated_cost_usd, None);
        assert!(!unpriced.costed);
    }

    #[test]
    fn nt47_repeated_and_interleaved_reports_count_each_message_once() {
        // Anthropic shape: message_start carries input and cache tokens, every
        // content block repeats them, and message_delta carries the output.
        let stream = [
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"m1","usage":{"input_tokens":100,"cache_read_input_tokens":4000,"cache_creation_input_tokens":50,"output_tokens":1}}},"session_id":"s","parent_tool_use_id":null}"#,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"a"}],"usage":{"input_tokens":100,"cache_read_input_tokens":4000,"cache_creation_input_tokens":50,"output_tokens":1}},"session_id":"s","parent_tool_use_id":null}"#,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Read","input":{}}],"usage":{"input_tokens":100,"cache_read_input_tokens":4000,"cache_creation_input_tokens":50,"output_tokens":1}},"session_id":"s","parent_tool_use_id":null}"#,
            r#"{"type":"stream_event","event":{"type":"message_delta","usage":{"output_tokens":420}},"session_id":"s","parent_tool_use_id":null}"#,
        ]
        .join("\n");
        assert_eq!(
            UsageAccumulator::from_stream(&stream).total(),
            Some(UsageBreakdown {
                input: 100,
                output: 420,
                cache_read: 4_000,
                cache_write: 50,
            })
        );

        // In the fixture the subagent's delta arrives after the main lane has
        // started its next message; it still belongs to the subagent's message.
        let mut accumulator = UsageAccumulator::default();
        for line in KILLED_GLM_ATTEMPT.lines() {
            accumulator.observe_line(line);
        }
        assert_eq!(
            accumulator.total(),
            Some(UsageBreakdown {
                input: 36_007,
                output: 1_100,
                cache_read: 63_968,
                cache_write: 0,
            })
        );
    }

    fn labels(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    fn shape<'a>(
        outcome: &'a str,
        prompt_template: &'a str,
        delivered_commits: bool,
        before: &'a [String],
        after: Option<&'a [String]>,
    ) -> AttemptShape<'a> {
        AttemptShape {
            outcome,
            prompt_template,
            delivered_commits,
            labels_before: before,
            labels_after: after,
        }
    }

    #[test]
    fn nt46_decomposed_is_the_attempt_outcome_wire_string() {
        assert_eq!(
            DECOMPOSED,
            crate::telemetry::AttemptOutcome::Decomposed.as_str()
        );
    }

    #[test]
    fn nt46_a_split_template_success_is_decomposed_with_or_without_commits() {
        for delivered_commits in [false, true] {
            assert_eq!(
                classify_decomposition(&shape(
                    "verified_success",
                    "split",
                    delivered_commits,
                    &[],
                    None
                )),
                Some(Decomposition::SplitTemplate)
            );
        }
        assert_eq!(
            Decomposition::SplitTemplate.terminal_reason(),
            "decomposed:split_template"
        );
    }

    #[test]
    fn nt46_a_split_attempt_that_earned_no_credit_keeps_its_outcome() {
        for outcome in [
            "work_failure",
            "indeterminate",
            "infrastructure_failure",
            "cancelled",
        ] {
            assert_eq!(
                classify_decomposition(&shape(outcome, "split", false, &[], None)),
                None,
                "{outcome} must stay {outcome}"
            );
        }
    }

    #[test]
    fn nt46_a_new_split_parent_without_commits_is_decomposed() {
        let none = labels(&[]);
        let parent = labels(&["umbrella", AUTO_SPLIT_PARENT_LABEL]);
        assert_eq!(
            classify_decomposition(&shape(
                "verified_success",
                "pluck",
                false,
                &none,
                Some(&parent)
            )),
            Some(Decomposition::SplitParentWithoutCommits)
        );
        // A delivered commit is an artifact: the attempt did work.
        assert_eq!(
            classify_decomposition(&shape(
                "verified_success",
                "pluck",
                true,
                &none,
                Some(&parent)
            )),
            None
        );
        // A bead that was already a split parent was not split by this attempt.
        assert_eq!(
            classify_decomposition(&shape(
                "verified_success",
                "pluck",
                false,
                &parent,
                Some(&parent)
            )),
            None
        );
        // Labels that were never read back decide nothing.
        assert_eq!(
            classify_decomposition(&shape("verified_success", "pluck", false, &none, None)),
            None
        );
    }

    #[test]
    fn nt46_only_a_commit_less_verified_success_outside_split_needs_a_read() {
        assert!(needs_labels_after("verified_success", "pluck", false));
        assert!(!needs_labels_after("verified_success", "pluck", true));
        assert!(!needs_labels_after("verified_success", "split", false));
        assert!(!needs_labels_after("work_failure", "pluck", false));
    }

    #[test]
    fn nt46_the_backend_records_decomposed_as_indeterminate() {
        assert_eq!(backend_outcome("decomposed"), "indeterminate");
        for outcome in [
            "verified_success",
            "work_failure",
            "infrastructure_failure",
            "cancelled",
            "indeterminate",
        ] {
            assert_eq!(backend_outcome(outcome), outcome);
        }
    }
}
