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
        self.ready_variants()
            .into_iter()
            .max_by(|a, b| {
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
                    if let Some((name, version)) =
                        self.pending_dispatch_start.remove(bead_id)
                    {
                        if let Some(entry) = self
                            .stats
                            .get_mut(&name)
                            .and_then(|m| m.get_mut(&version))
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
                        if let Some(entry) = self
                            .stats
                            .get_mut(&name)
                            .and_then(|m| m.get_mut(&version))
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
        self.stats.get(template_name).map(|by_version| VariantComparison {
            template_name: template_name.to_string(),
            min_dispatches: self.min_dispatches,
            variants: by_version.clone(),
        })
    }
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
        agg.process_event(
            &serde_json::from_str(&make_completed_event("nd-1", 5000)).unwrap(),
        );
        agg.process_event(
            &serde_json::from_str(&make_outcome_event("nd-1", "Success")).unwrap(),
        );

        agg.process_event(
            &serde_json::from_str(&make_dispatch_event("nd-2", "pluck", "pluck-default")).unwrap(),
        );
        agg.process_event(
            &serde_json::from_str(&make_outcome_event("nd-2", "Failure")).unwrap(),
        );

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
        agg.process_event(
            &serde_json::from_str(&make_outcome_event("nd-a", "Success")).unwrap(),
        );

        agg.process_event(
            &serde_json::from_str(&make_dispatch_event("nd-b", "pluck", "pluck-v2")).unwrap(),
        );
        agg.process_event(
            &serde_json::from_str(&make_outcome_event("nd-b", "Failure")).unwrap(),
        );

        let cmp = agg.comparison_for("pluck").unwrap();
        assert_eq!(cmp.variants.len(), 2);

        let best = cmp.best_variant().unwrap();
        assert_eq!(best.version, "pluck-default");
    }

    #[test]
    fn aggregator_load_from_file() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("telemetry.jsonl");

        let lines = vec![
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

        let lines_a = vec![
            make_dispatch_event("nd-1", "pluck", "pluck-default"),
            make_outcome_event("nd-1", "Success"),
        ];
        std::fs::write(dir.path().join("2026-01-01.jsonl"), lines_a.join("\n")).unwrap();

        let lines_b = vec![
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
}
