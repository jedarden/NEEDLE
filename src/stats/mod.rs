//! Template versioning and A/B variant statistics.
//!
//! Reads `agent.dispatched` and `outcome.classified` telemetry events from
//! JSONL log files and aggregates per-variant outcome counts and durations.
//! Once enough beads have been dispatched (default: 50 per variant), a
//! `VariantComparison` can be produced for the `needle stats` command.
//!
//! Leaf module — depends only on `types`.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

// ──────────────────────────────────────────────────────────────────────────────
// Per-variant aggregates
// ──────────────────────────────────────────────────────────────────────────────

/// Aggregate statistics for one template variant.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct VariantStats {
    /// Variant version tag (e.g., `"pluck-default"`, `"pluck-v2"`).
    pub version: String,
    /// Total number of dispatches observed for this variant.
    pub dispatches: u64,
    /// Number of beads that completed with `"Success"` outcome.
    pub successes: u64,
    /// Number of beads that completed with `"Failure"` outcome.
    pub failures: u64,
    /// Number of beads that completed with `"Timeout"` outcome.
    pub timeouts: u64,
    /// Sum of dispatch durations in milliseconds (from `agent.completed`).
    pub total_duration_ms: u64,
}

impl VariantStats {
    /// Success rate as a fraction in `[0.0, 1.0]`.  Returns `None` if no
    /// dispatches have been recorded.
    pub fn success_rate(&self) -> Option<f64> {
        if self.dispatches == 0 {
            None
        } else {
            Some(self.successes as f64 / self.dispatches as f64)
        }
    }

    /// Average dispatch duration in milliseconds.  Returns `None` if no
    /// durations have been recorded.
    pub fn avg_duration_ms(&self) -> Option<f64> {
        if self.dispatches == 0 {
            None
        } else {
            Some(self.total_duration_ms as f64 / self.dispatches as f64)
        }
    }

    /// Whether this variant has accumulated enough dispatches to be
    /// considered statistically meaningful.
    pub fn has_sufficient_data(&self, min_dispatches: u64) -> bool {
        self.dispatches >= min_dispatches
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Comparison report
// ──────────────────────────────────────────────────────────────────────────────

/// Comparison of all variants for a single template name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantComparison {
    /// Template name (e.g., `"pluck"`).
    pub template_name: String,
    /// Minimum dispatch count required before a variant is included.
    pub min_dispatches: u64,
    /// Stats for each variant, keyed by version tag.
    pub variants: BTreeMap<String, VariantStats>,
}

impl VariantComparison {
    /// Returns variants that have at least `min_dispatches` observations.
    pub fn ready_variants(&self) -> Vec<&VariantStats> {
        self.variants
            .values()
            .filter(|v| v.has_sufficient_data(self.min_dispatches))
            .collect()
    }

    /// Returns the variant with the highest success rate among ready variants,
    /// or `None` if no variants are ready.
    pub fn best_variant(&self) -> Option<&VariantStats> {
        self.ready_variants().into_iter().max_by(|a, b| {
            a.success_rate()
                .unwrap_or(0.0)
                .partial_cmp(&b.success_rate().unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Aggregator
// ──────────────────────────────────────────────────────────────────────────────

/// Aggregates template variant statistics from JSONL telemetry log files.
///
/// # Usage
///
/// ```no_run
/// use needle::stats::StatsAggregator;
///
/// let mut agg = StatsAggregator::new(50);
/// agg.load_logs(std::path::Path::new("~/.needle/logs")).unwrap();
/// for (template, cmp) in agg.comparisons() {
///     println!("{template}: {:?}", cmp.best_variant());
/// }
/// ```
pub struct StatsAggregator {
    /// Minimum dispatches per variant before comparisons are produced.
    min_dispatches: u64,
    /// Per-template, per-version stats.
    ///
    /// Outer key: template name (e.g., `"pluck"`).
    /// Inner key: version tag (e.g., `"pluck-v2"`).
    stats: BTreeMap<String, BTreeMap<String, VariantStats>>,
    /// Pending dispatch events waiting for an outcome, keyed by bead_id.
    ///
    /// Maps bead_id → (template_name, template_version).
    pending: BTreeMap<String, (String, String)>,
    /// Pending dispatch durations waiting for an `agent.completed` event,
    /// keyed by bead_id.
    pending_dispatch_start: BTreeMap<String, (String, String)>,
}

impl StatsAggregator {
    /// Create a new aggregator.
    ///
    /// `min_dispatches` — minimum observations per variant before a variant
    /// is included in comparisons (default: 50).
    pub fn new(min_dispatches: u64) -> Self {
        StatsAggregator {
            min_dispatches,
            stats: BTreeMap::new(),
            pending: BTreeMap::new(),
            pending_dispatch_start: BTreeMap::new(),
        }
    }

    /// Load and process all `*.jsonl` files under `log_dir`.
    ///
    /// Files are sorted by name (which sorts chronologically for date-prefixed
    /// log files) so events are processed in order.
    pub fn load_logs(&mut self, log_dir: &Path) -> Result<()> {
        let mut paths: Vec<_> = std::fs::read_dir(log_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
            .collect();
        paths.sort();

        for path in paths {
            self.load_file(&path)?;
        }
        Ok(())
    }

    /// Load and process a single JSONL file.
    pub fn load_file(&mut self, path: &Path) -> Result<()> {
        let content = std::fs::read_to_string(path)?;
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
                self.process_event(&event);
            }
        }
        Ok(())
    }

    /// Process a single telemetry event JSON value.
    fn process_event(&mut self, event: &serde_json::Value) {
        let event_type = event.get("event_type").and_then(|v| v.as_str());
        let data = match event.get("data") {
            Some(d) => d,
            None => return,
        };

        match event_type {
            Some("agent.dispatched") => {
                let bead_id = data.get("bead_id").and_then(|v| v.as_str());
                let template_name = data.get("template_name").and_then(|v| v.as_str());
                let template_version = data.get("template_version").and_then(|v| v.as_str());

                if let (Some(bead_id), Some(name), Some(version)) =
                    (bead_id, template_name, template_version)
                {
                    // Record a dispatch for this variant.
                    let entry = self
                        .stats
                        .entry(name.to_string())
                        .or_default()
                        .entry(version.to_string())
                        .or_insert_with(|| VariantStats {
                            version: version.to_string(),
                            ..Default::default()
                        });
                    entry.dispatches += 1;

                    // Track pending for outcome correlation.
                    self.pending
                        .insert(bead_id.to_string(), (name.to_string(), version.to_string()));
                    self.pending_dispatch_start
                        .insert(bead_id.to_string(), (name.to_string(), version.to_string()));
                }
            }

            Some("agent.completed") => {
                let bead_id = data.get("bead_id").and_then(|v| v.as_str());
                let duration_ms = data.get("duration_ms").and_then(|v| v.as_u64());

                if let (Some(bead_id), Some(duration)) = (bead_id, duration_ms) {
                    if let Some((name, version)) = self.pending_dispatch_start.remove(bead_id) {
                        if let Some(entry) =
                            self.stats.get_mut(&name).and_then(|m| m.get_mut(&version))
                        {
                            entry.total_duration_ms += duration;
                        }
                    }
                }
            }

            Some("outcome.classified") => {
                let bead_id = data.get("bead_id").and_then(|v| v.as_str());
                let outcome = data.get("outcome").and_then(|v| v.as_str());

                if let (Some(bead_id), Some(outcome)) = (bead_id, outcome) {
                    if let Some((name, version)) = self.pending.remove(bead_id) {
                        if let Some(entry) =
                            self.stats.get_mut(&name).and_then(|m| m.get_mut(&version))
                        {
                            match outcome {
                                "Success" => entry.successes += 1,
                                "Failure" => entry.failures += 1,
                                "Timeout" => entry.timeouts += 1,
                                _ => {}
                            }
                        }
                    }
                }
            }

            _ => {}
        }
    }

    /// Produce a `VariantComparison` for every template that has been observed.
    pub fn comparisons(&self) -> BTreeMap<String, VariantComparison> {
        self.stats
            .iter()
            .map(|(template_name, by_version)| {
                let comparison = VariantComparison {
                    template_name: template_name.clone(),
                    min_dispatches: self.min_dispatches,
                    variants: by_version.clone(),
                };
                (template_name.clone(), comparison)
            })
            .collect()
    }

    /// Produce a comparison for a specific template name, or `None` if no
    /// events have been observed for that template.
    pub fn comparison_for(&self, template_name: &str) -> Option<VariantComparison> {
        self.stats
            .get(template_name)
            .map(|by_version| VariantComparison {
                template_name: template_name.to_string(),
                min_dispatches: self.min_dispatches,
                variants: by_version.clone(),
            })
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Stats engine — multi-dimensional outcome aggregation
// ──────────────────────────────────────────────────────────────────────────────

/// Grouping dimension for the `needle stats` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatsDimension {
    /// Group by template version tag (e.g., `"pluck-v2"`).
    TemplateVersion,
    /// Group by template name / task type (e.g., `"pluck"`).
    TaskType,
    /// Group by worker identifier (e.g., `"needle-alpha"`).
    Worker,
}

/// Aggregated statistics row for one value of a grouping dimension.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StatsRow {
    /// The dimension value (version, task type, or worker id).
    pub key: String,
    /// Total number of beads dispatched in this group.
    pub beads: u64,
    /// Number of beads that completed with `"Success"` outcome.
    pub pass: u64,
    /// Number of beads that completed with `"Failure"` outcome.
    pub fail: u64,
    /// Number of beads that completed with `"Timeout"` outcome.
    pub timeout: u64,
    /// Sum of (tokens_in + tokens_out) across all effort events in this group.
    pub total_tokens: u64,
    /// Sum of `estimated_cost_usd` across all effort events in this group.
    pub total_cost_usd: f64,
    /// Number of effort events with token/cost data (denominator for averages).
    pub effort_events: u64,
}

impl StatsRow {
    /// Pass rate as a fraction in `[0.0, 1.0]`. `None` when `beads == 0`.
    pub fn pass_rate(&self) -> Option<f64> {
        if self.beads == 0 {
            None
        } else {
            Some(self.pass as f64 / self.beads as f64)
        }
    }

    /// Average total tokens (in + out) per effort event. `None` when no effort data.
    pub fn avg_tokens(&self) -> Option<f64> {
        if self.effort_events == 0 {
            None
        } else {
            Some(self.total_tokens as f64 / self.effort_events as f64)
        }
    }

    /// Average cost in USD per effort event. `None` when no effort data.
    pub fn avg_cost_usd(&self) -> Option<f64> {
        if self.effort_events == 0 {
            None
        } else {
            Some(self.total_cost_usd / self.effort_events as f64)
        }
    }
}

/// Compute per-group statistics from a pre-filtered slice of telemetry events.
///
/// Correlates `agent.dispatched`, `outcome.classified`, and `effort.recorded`
/// events by `bead_id`, grouping each bead under the chosen `dimension`.
///
/// Pass the result of [`telemetry::read_logs`] (already time-filtered) here.
pub fn compute_stats(
    events: &[crate::telemetry::TelemetryEvent],
    dimension: StatsDimension,
) -> Vec<StatsRow> {
    use std::collections::HashMap;

    // bead_id → dimension key (populated from agent.dispatched events)
    let mut bead_key: HashMap<String, String> = HashMap::new();
    let mut rows: BTreeMap<String, StatsRow> = BTreeMap::new();

    for event in events {
        match event.event_type.as_str() {
            "agent.dispatched" => {
                let bead_id = match event.bead_id.as_ref() {
                    Some(b) => b.as_ref().to_string(),
                    None => continue,
                };
                let key = match dimension {
                    StatsDimension::TemplateVersion => event
                        .data
                        .get("template_version")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    StatsDimension::TaskType => event
                        .data
                        .get("template_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    StatsDimension::Worker => event.worker_id.clone(),
                };
                bead_key.insert(bead_id, key.clone());
                let row = rows.entry(key.clone()).or_insert_with(|| StatsRow {
                    key: key.clone(),
                    ..Default::default()
                });
                row.beads += 1;
            }

            "outcome.classified" => {
                let bead_id = match event.bead_id.as_ref() {
                    Some(b) => b.as_ref().to_string(),
                    None => continue,
                };
                if let Some(key) = bead_key.get(&bead_id) {
                    if let Some(row) = rows.get_mut(key) {
                        match event.data.get("outcome").and_then(|v| v.as_str()) {
                            Some("Success") => row.pass += 1,
                            Some("Failure") => row.fail += 1,
                            Some("Timeout") => row.timeout += 1,
                            _ => {}
                        }
                    }
                }
            }

            "effort.recorded" => {
                let bead_id = match event.bead_id.as_ref() {
                    Some(b) => b.as_ref().to_string(),
                    None => continue,
                };
                if let Some(key) = bead_key.get(&bead_id) {
                    if let Some(row) = rows.get_mut(key) {
                        let tokens_in = event
                            .data
                            .get("tokens_in")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let tokens_out = event
                            .data
                            .get("tokens_out")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        row.total_tokens += tokens_in + tokens_out;
                        if let Some(cost) = event
                            .data
                            .get("estimated_cost_usd")
                            .and_then(|v| v.as_f64())
                        {
                            row.total_cost_usd += cost;
                        }
                        row.effort_events += 1;
                    }
                }
            }

            _ => {}
        }
    }

    rows.into_values().collect()
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_dispatch_event(bead_id: &str, template_name: &str, template_version: &str) -> String {
        serde_json::json!({
            "event_type": "agent.dispatched",
            "data": {
                "bead_id": bead_id,
                "agent": "claude-sonnet",
                "prompt_len": 1000,
                "template_name": template_name,
                "template_version": template_version,
                "prompt_hash": "sha256:abc123"
            }
        })
        .to_string()
    }

    fn make_completed_event(bead_id: &str, duration_ms: u64) -> String {
        serde_json::json!({
            "event_type": "agent.completed",
            "data": {
                "bead_id": bead_id,
                "exit_code": 0,
                "duration_ms": duration_ms
            }
        })
        .to_string()
    }

    fn make_outcome_event(bead_id: &str, outcome: &str) -> String {
        serde_json::json!({
            "event_type": "outcome.classified",
            "data": {
                "bead_id": bead_id,
                "outcome": outcome,
                "exit_code": 0
            }
        })
        .to_string()
    }

    #[test]
    fn variant_stats_success_rate_empty() {
        let stats = VariantStats::default();
        assert_eq!(stats.success_rate(), None);
        assert_eq!(stats.avg_duration_ms(), None);
    }

    #[test]
    fn variant_stats_success_rate_computed() {
        let stats = VariantStats {
            version: "pluck-default".to_string(),
            dispatches: 4,
            successes: 3,
            failures: 1,
            timeouts: 0,
            total_duration_ms: 8000,
        };
        assert!((stats.success_rate().unwrap() - 0.75).abs() < f64::EPSILON);
        assert!((stats.avg_duration_ms().unwrap() - 2000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn variant_stats_sufficient_data() {
        let stats = VariantStats {
            version: "v1".to_string(),
            dispatches: 49,
            ..Default::default()
        };
        assert!(!stats.has_sufficient_data(50));

        let stats = VariantStats {
            dispatches: 50,
            ..Default::default()
        };
        assert!(stats.has_sufficient_data(50));
    }

    #[test]
    fn aggregator_counts_dispatches() {
        let mut agg = StatsAggregator::new(50);

        for i in 0..3 {
            agg.process_event(
                &serde_json::from_str(&make_dispatch_event(
                    &format!("nd-{i}"),
                    "pluck",
                    "pluck-default",
                ))
                .unwrap(),
            );
        }

        let cmp = agg.comparison_for("pluck").unwrap();
        let stats = cmp.variants.get("pluck-default").unwrap();
        assert_eq!(stats.dispatches, 3);
        assert_eq!(stats.successes, 0);
    }

    #[test]
    fn aggregator_correlates_outcomes() {
        let mut agg = StatsAggregator::new(50);

        agg.process_event(
            &serde_json::from_str(&make_dispatch_event("nd-1", "pluck", "pluck-default")).unwrap(),
        );
        agg.process_event(&serde_json::from_str(&make_completed_event("nd-1", 5000)).unwrap());
        agg.process_event(&serde_json::from_str(&make_outcome_event("nd-1", "Success")).unwrap());

        agg.process_event(
            &serde_json::from_str(&make_dispatch_event("nd-2", "pluck", "pluck-default")).unwrap(),
        );
        agg.process_event(&serde_json::from_str(&make_outcome_event("nd-2", "Failure")).unwrap());

        let cmp = agg.comparison_for("pluck").unwrap();
        let stats = cmp.variants.get("pluck-default").unwrap();
        assert_eq!(stats.dispatches, 2);
        assert_eq!(stats.successes, 1);
        assert_eq!(stats.failures, 1);
        assert_eq!(stats.total_duration_ms, 5000);
    }

    #[test]
    fn aggregator_tracks_multiple_variants() {
        let mut agg = StatsAggregator::new(1);

        agg.process_event(
            &serde_json::from_str(&make_dispatch_event("nd-a", "pluck", "pluck-default")).unwrap(),
        );
        agg.process_event(&serde_json::from_str(&make_outcome_event("nd-a", "Success")).unwrap());

        agg.process_event(
            &serde_json::from_str(&make_dispatch_event("nd-b", "pluck", "pluck-v2")).unwrap(),
        );
        agg.process_event(&serde_json::from_str(&make_outcome_event("nd-b", "Failure")).unwrap());

        let cmp = agg.comparison_for("pluck").unwrap();
        assert_eq!(cmp.variants.len(), 2);

        let best = cmp.best_variant().unwrap();
        assert_eq!(best.version, "pluck-default");
    }

    #[test]
    fn aggregator_load_from_file() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("telemetry.jsonl");

        let lines = [
            make_dispatch_event("nd-1", "pluck", "pluck-default"),
            make_completed_event("nd-1", 3000),
            make_outcome_event("nd-1", "Success"),
        ];
        std::fs::write(&log_path, lines.join("\n")).unwrap();

        let mut agg = StatsAggregator::new(50);
        agg.load_file(&log_path).unwrap();

        let cmp = agg.comparison_for("pluck").unwrap();
        let stats = cmp.variants.get("pluck-default").unwrap();
        assert_eq!(stats.dispatches, 1);
        assert_eq!(stats.successes, 1);
        assert_eq!(stats.total_duration_ms, 3000);
    }

    #[test]
    fn aggregator_load_logs_scans_directory() {
        let dir = TempDir::new().unwrap();

        let lines_a = [
            make_dispatch_event("nd-1", "pluck", "pluck-default"),
            make_outcome_event("nd-1", "Success"),
        ];
        std::fs::write(dir.path().join("2026-01-01.jsonl"), lines_a.join("\n")).unwrap();

        let lines_b = [
            make_dispatch_event("nd-2", "pluck", "pluck-default"),
            make_outcome_event("nd-2", "Failure"),
        ];
        std::fs::write(dir.path().join("2026-01-02.jsonl"), lines_b.join("\n")).unwrap();

        // A non-jsonl file should be ignored.
        std::fs::write(dir.path().join("notes.txt"), "ignore me").unwrap();

        let mut agg = StatsAggregator::new(50);
        agg.load_logs(dir.path()).unwrap();

        let cmp = agg.comparison_for("pluck").unwrap();
        let stats = cmp.variants.get("pluck-default").unwrap();
        assert_eq!(stats.dispatches, 2);
        assert_eq!(stats.successes, 1);
        assert_eq!(stats.failures, 1);
    }

    #[test]
    fn comparison_ready_variants_threshold() {
        let mut variants = BTreeMap::new();
        variants.insert(
            "pluck-default".to_string(),
            VariantStats {
                version: "pluck-default".to_string(),
                dispatches: 30,
                ..Default::default()
            },
        );
        variants.insert(
            "pluck-v2".to_string(),
            VariantStats {
                version: "pluck-v2".to_string(),
                dispatches: 60,
                ..Default::default()
            },
        );

        let cmp = VariantComparison {
            template_name: "pluck".to_string(),
            min_dispatches: 50,
            variants,
        };

        let ready = cmp.ready_variants();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].version, "pluck-v2");
    }

    // ── compute_stats tests ──────────────────────────────────────────────────

    fn make_tel_event(
        event_type: &str,
        worker_id: &str,
        bead_id: Option<&str>,
        data: serde_json::Value,
    ) -> crate::telemetry::TelemetryEvent {
        crate::telemetry::TelemetryEvent {
            timestamp: chrono::Utc::now(),
            event_type: event_type.to_string(),
            worker_id: worker_id.to_string(),
            session_id: "test0000".to_string(),
            sequence: 0,
            bead_id: bead_id.map(crate::types::BeadId::from),
            workspace: None,
            data,
            duration_ms: None,
            trace_id: None,
            span_id: None,
        }
    }

    #[test]
    fn compute_stats_by_template_version() {
        let events = vec![
            make_tel_event(
                "agent.dispatched",
                "needle-alpha",
                Some("nd-1"),
                serde_json::json!({"template_name": "pluck", "template_version": "pluck-v1"}),
            ),
            make_tel_event(
                "outcome.classified",
                "needle-alpha",
                Some("nd-1"),
                serde_json::json!({"outcome": "Success"}),
            ),
            make_tel_event(
                "effort.recorded",
                "needle-alpha",
                Some("nd-1"),
                serde_json::json!({"tokens_in": 100, "tokens_out": 50, "estimated_cost_usd": 0.01}),
            ),
            make_tel_event(
                "agent.dispatched",
                "needle-alpha",
                Some("nd-2"),
                serde_json::json!({"template_name": "pluck", "template_version": "pluck-v2"}),
            ),
            make_tel_event(
                "outcome.classified",
                "needle-alpha",
                Some("nd-2"),
                serde_json::json!({"outcome": "Failure"}),
            ),
        ];

        let mut rows = compute_stats(&events, StatsDimension::TemplateVersion);
        rows.sort_by(|a, b| a.key.cmp(&b.key));

        assert_eq!(rows.len(), 2);

        let v1 = rows.iter().find(|r| r.key == "pluck-v1").unwrap();
        assert_eq!(v1.beads, 1);
        assert_eq!(v1.pass, 1);
        assert_eq!(v1.fail, 0);
        assert_eq!(v1.total_tokens, 150);
        assert!((v1.avg_cost_usd().unwrap() - 0.01).abs() < f64::EPSILON);

        let v2 = rows.iter().find(|r| r.key == "pluck-v2").unwrap();
        assert_eq!(v2.beads, 1);
        assert_eq!(v2.pass, 0);
        assert_eq!(v2.fail, 1);
        assert_eq!(v2.effort_events, 0);
    }

    #[test]
    fn compute_stats_by_task_type() {
        let events = vec![
            make_tel_event(
                "agent.dispatched",
                "needle-alpha",
                Some("nd-a"),
                serde_json::json!({"template_name": "pluck", "template_version": "pluck-v1"}),
            ),
            make_tel_event(
                "outcome.classified",
                "needle-alpha",
                Some("nd-a"),
                serde_json::json!({"outcome": "Timeout"}),
            ),
            make_tel_event(
                "agent.dispatched",
                "needle-alpha",
                Some("nd-b"),
                serde_json::json!({"template_name": "strand", "template_version": "strand-v1"}),
            ),
            make_tel_event(
                "outcome.classified",
                "needle-alpha",
                Some("nd-b"),
                serde_json::json!({"outcome": "Success"}),
            ),
        ];

        let rows = compute_stats(&events, StatsDimension::TaskType);
        let pluck = rows.iter().find(|r| r.key == "pluck").unwrap();
        assert_eq!(pluck.timeout, 1);
        assert_eq!(pluck.pass, 0);

        let strand = rows.iter().find(|r| r.key == "strand").unwrap();
        assert_eq!(strand.pass, 1);
    }

    #[test]
    fn compute_stats_by_worker() {
        let events = vec![
            make_tel_event(
                "agent.dispatched",
                "needle-alpha",
                Some("nd-1"),
                serde_json::json!({"template_name": "pluck", "template_version": "pluck-v1"}),
            ),
            make_tel_event(
                "outcome.classified",
                "needle-alpha",
                Some("nd-1"),
                serde_json::json!({"outcome": "Success"}),
            ),
            make_tel_event(
                "agent.dispatched",
                "needle-bravo",
                Some("nd-2"),
                serde_json::json!({"template_name": "pluck", "template_version": "pluck-v1"}),
            ),
            make_tel_event(
                "outcome.classified",
                "needle-bravo",
                Some("nd-2"),
                serde_json::json!({"outcome": "Success"}),
            ),
            make_tel_event(
                "agent.dispatched",
                "needle-bravo",
                Some("nd-3"),
                serde_json::json!({"template_name": "pluck", "template_version": "pluck-v1"}),
            ),
            make_tel_event(
                "outcome.classified",
                "needle-bravo",
                Some("nd-3"),
                serde_json::json!({"outcome": "Failure"}),
            ),
        ];

        let rows = compute_stats(&events, StatsDimension::Worker);
        let alpha = rows.iter().find(|r| r.key == "needle-alpha").unwrap();
        assert_eq!(alpha.beads, 1);
        assert_eq!(alpha.pass, 1);
        assert!((alpha.pass_rate().unwrap() - 1.0).abs() < f64::EPSILON);

        let bravo = rows.iter().find(|r| r.key == "needle-bravo").unwrap();
        assert_eq!(bravo.beads, 2);
        assert_eq!(bravo.pass, 1);
        assert_eq!(bravo.fail, 1);
        assert!((bravo.pass_rate().unwrap() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn stats_row_defaults_give_none() {
        let row = StatsRow::default();
        assert!(row.pass_rate().is_none());
        assert!(row.avg_tokens().is_none());
        assert!(row.avg_cost_usd().is_none());
    }
}
