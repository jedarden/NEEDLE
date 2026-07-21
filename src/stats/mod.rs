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
// Percentile helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Calculate the 95th percentile from a slice of values.
///
/// The 95th percentile (p95) is the value below which 95% of observations
/// fall. It is commonly used to understand the "tail" of a distribution
/// — for example, in latency metrics, p95 tells you that 95% of requests
/// completed within this time, while the slowest 5% took longer.
///
/// # Edge Cases
///
/// This function handles all edge cases gracefully:
///
/// - **Empty slice**: Returns `0` (no data available)
/// - **Single element**: Returns that element (the only value available)
/// - **Two elements**: Uses linear interpolation to estimate p95 between the two values
/// - **Small samples (2-3 elements)**: Linear interpolation provides a reasonable estimate
///
/// All edge cases return sensible results without panicking.
///
/// # Algorithm
///
/// This function uses **linear interpolation**, which is the same method
/// used by Criterion.rs and is more accurate than the nearest-rank method:
///
/// 1. If the slice is empty, return 0 (no data case)
/// 2. If the slice has one element, return that element
/// 3. Sort the values in ascending order
/// 4. Calculate the rank: `rank = 0.95 * (n - 1)` where `n` is the number of elements
/// 5. Split the rank into integer and fractional parts
/// 6. Return the linear interpolation: `floor_value + (ceiling_value - floor_value) * fraction`
/// 7. Round to the nearest integer for the final result
///
/// This method was chosen because:
/// - **Accurate**: Uses linear interpolation like Criterion.rs for smooth percentile estimates
/// - **Standard**: Matches the behavior of common benchmarking libraries
/// - **Well-documented**: The algorithm is described in statistical literature
/// - **Deterministic**: Always produces the same result for the same input
/// - **Handles all sample sizes**: Works correctly from 0 to very large datasets
///
/// Note: This uses linear interpolation, not nearest-rank. For example, with 10 elements
/// `[10, 20, ..., 100]`, the 95th percentile is approximately `95.5` (rounded to `96`),
/// not the maximum value `100`.
///
/// # Examples
///
/// ## Basic usage with sorted data
///
/// ```
/// use needle::stats::calculate_p95;
///
/// let latencies = vec![10u128, 20, 30, 40, 50, 60, 70, 80, 90, 100];
/// let p95 = calculate_p95(&latencies);
/// // rank = 0.95 * 9 = 8.55, floor_index = 8, fraction = 0.55
/// // floor_value = 90, ceiling_value = 100
/// // interpolated = 90 + 10 * 0.55 = 95.5 → 96
/// assert_eq!(p95, 96);
/// ```
///
/// ## Works with unsorted input (sorts internally)
///
/// ```
/// use needle::stats::calculate_p95;
///
/// let unsorted = vec![100u128, 10, 50, 30, 70, 40, 60, 20, 80, 90];
/// let p95 = calculate_p95(&unsorted);
/// assert_eq!(p95, 96); // Function sorts internally → same result
/// ```
///
/// ## Larger dataset
///
/// ```
/// use needle::stats::calculate_p95;
///
/// // 20 elements: rank = 0.95 * 19 = 18.05, floor_index = 18, fraction = 0.05
/// let data = vec![10u128, 20, 30, 40, 50, 60, 70, 80, 90, 100,
///                  110, 120, 130, 140, 150, 160, 170, 180, 190, 200];
/// let p95 = calculate_p95(&data);
/// // floor_value = 190, ceiling_value = 200
/// // interpolated = 190 + 10 * 0.05 = 190.5 → 191
/// assert_eq!(p95, 191);
/// ```
///
/// ## Single element (degenerate case)
///
/// ```
/// use needle::stats::calculate_p95;
///
/// let single = vec![42u128];
/// let p95 = calculate_p95(&single);
/// assert_eq!(p95, 42); // Only value available
/// ```
///
/// ## Empty input
///
/// ```
/// use needle::stats::calculate_p95;
///
/// let empty: Vec<u128> = vec![];
/// let p95 = calculate_p95(&empty);
/// assert_eq!(p95, 0); // No data → returns 0
/// ```
///
/// ## Two elements (small sample)
///
/// ```
/// use needle::stats::calculate_p95;
///
/// let two = vec![10u128, 20];
/// let p95 = calculate_p95(&two);
/// // rank = 0.95 * 1 = 0.95, floor_index = 0, fraction = 0.95
/// // floor_value = 10, ceiling_value = 20
/// // interpolated = 10 + 10 * 0.95 = 19.5 → 20
/// assert_eq!(p95, 20); // Linear interpolation estimates p95
/// ```
///
/// ## Three elements (small sample)
///
/// ```
/// use needle::stats::calculate_p95;
///
/// let three = vec![10u128, 20, 30];
/// let p95 = calculate_p95(&three);
/// // rank = 0.95 * 2 = 1.9, floor_index = 1, fraction = 0.9
/// // floor_value = 20, ceiling_value = 30
/// // interpolated = 20 + 10 * 0.9 = 29.0 → 29
/// assert_eq!(p95, 29); // Linear interpolation estimates p95
/// ```
///
/// ## Real-world latency example
///
/// ```
/// use needle::stats::calculate_p95;
///
/// // Simulated latency data in milliseconds
/// let latencies = vec![
///     12, 15, 18, 20, 22, 25, 28, 30, 35, 40,
///     45, 50, 55, 60, 70, 80, 90, 100, 120, 150
/// ];
/// let p95 = calculate_p95(&latencies);
/// // rank = 0.95 * 19 = 18.05, floor_index = 18, fraction = 0.05
/// // floor_value = 120, ceiling_value = 150
/// // interpolated = 120 + 30 * 0.05 = 121.5 → 122
/// assert_eq!(p95, 122);
/// ```
///
/// # See Also
///
/// - [`docs/p95-calculation-algorithms.md`](../../docs/p95-calculation-algorithms.html) — Comprehensive survey of p95 algorithms and recommendations
pub fn calculate_p95(latencies: &[u128]) -> u128 {
    if latencies.is_empty() {
        return 0;
    }

    let n = latencies.len();
    if n == 1 {
        return latencies[0];
    }

    let mut sorted = Vec::from(latencies);
    sorted.sort();

    // Linear interpolation method (like Criterion.rs)
    // Formula: rank = (p / 100) * (n - 1)
    // For p95: rank = 0.95 * (n - 1)
    let rank = 0.95 * (n - 1) as f64;
    let floor_index = rank.floor() as usize;
    let fraction = rank - floor_index as f64;

    let floor_value = sorted[floor_index];
    let ceiling_value = sorted[floor_index + 1];

    // Linear interpolation: floor + (ceiling - floor) * fraction
    let interpolated = floor_value as f64 + (ceiling_value - floor_value) as f64 * fraction;

    // Round to nearest integer.
    // Add a small epsilon to handle floating point precision issues (e.g., 95.5 → 95.4999... → 95).
    // Standard rounding: 95.0-95.499... → 95, 95.5-96.499... → 96
    let epsilon = 1e-9;
    (interpolated + epsilon).round() as u128
}

/// Calculate the 99th percentile from a slice of values.
///
/// The 99th percentile (p99) is the value below which 99% of observations
/// fall. It is commonly used to understand the extreme tail of a distribution
/// — for example, in latency metrics, p99 tells you that 99% of requests
/// completed within this time, while the slowest 1% took longer.
///
/// # Edge Cases
///
/// This function handles all edge cases gracefully:
///
/// - **Empty slice**: Returns `0` (no data available)
/// - **Single element**: Returns that element (the only value available)
/// - **Two elements**: Uses linear interpolation to estimate p99 between the two values
/// - **Small samples (2-3 elements)**: Linear interpolation provides a reasonable estimate
///
/// All edge cases return sensible results without panicking.
///
/// # Algorithm
///
/// This function uses **linear interpolation**, which is the same method
/// used by Criterion.rs and is more accurate than the nearest-rank method:
///
/// 1. If the slice is empty, return 0 (no data case)
/// 2. If the slice has one element, return that element
/// 3. Sort the values in ascending order
/// 4. Calculate the rank: `rank = 0.99 * (n - 1)` where `n` is the number of elements
/// 5. Split the rank into integer and fractional parts
/// 6. Return the linear interpolation: `floor_value + (ceiling_value - floor_value) * fraction`
/// 7. Round to the nearest integer for the final result
///
/// This method was chosen because:
/// - **Accurate**: Uses linear interpolation like Criterion.rs for smooth percentile estimates
/// - **Standard**: Matches the behavior of common benchmarking libraries
/// - **Well-documented**: The algorithm is described in statistical literature
/// - **Deterministic**: Always produces the same result for the same input
/// - **Handles all sample sizes**: Works correctly from 0 to very large datasets
///
/// Note: This uses linear interpolation, not nearest-rank. For example, with 10 elements
/// `[10, 20, ..., 100]`, the 99th percentile is approximately `99.1` (rounded to `99`),
/// not the maximum value `100`.
///
/// # Examples
///
/// ## Basic usage with sorted data
///
/// ```
/// use needle::stats::calculate_p99;
///
/// let latencies = vec![10u128, 20, 30, 40, 50, 60, 70, 80, 90, 100];
/// let p99 = calculate_p99(&latencies);
/// // rank = 0.99 * 9 = 8.91, floor_index = 8, fraction = 0.91
/// // floor_value = 90, ceiling_value = 100
/// // interpolated = 90 + 10 * 0.91 = 99.1 → 99
/// assert_eq!(p99, 99);
/// ```
///
/// ## Works with unsorted input (sorts internally)
///
/// ```
/// use needle::stats::calculate_p99;
///
/// let unsorted = vec![100u128, 10, 50, 30, 70, 40, 60, 20, 80, 90];
/// let p99 = calculate_p99(&unsorted);
/// assert_eq!(p99, 99); // Function sorts internally → same result
/// ```
///
/// ## Larger dataset
///
/// ```
/// use needle::stats::calculate_p99;
///
/// // 20 elements: rank = 0.99 * 19 = 18.81, floor_index = 18, fraction = 0.81
/// let data = vec![10u128, 20, 30, 40, 50, 60, 70, 80, 90, 100,
///                  110, 120, 130, 140, 150, 160, 170, 180, 190, 200];
/// let p99 = calculate_p99(&data);
/// // floor_value = 190, ceiling_value = 200
/// // interpolated = 190 + 10 * 0.81 = 198.1 → 198
/// assert_eq!(p99, 198);
/// ```
///
/// ## Single element (degenerate case)
///
/// ```
/// use needle::stats::calculate_p99;
///
/// let single = vec![42u128];
/// let p99 = calculate_p99(&single);
/// assert_eq!(p99, 42); // Only value available
/// ```
///
/// ## Empty input
///
/// ```
/// use needle::stats::calculate_p99;
///
/// let empty: Vec<u128> = vec![];
/// let p99 = calculate_p99(&empty);
/// assert_eq!(p99, 0); // No data → returns 0
/// ```
///
/// ## Two elements (small sample)
///
/// ```
/// use needle::stats::calculate_p99;
///
/// let two = vec![10u128, 20];
/// let p99 = calculate_p99(&two);
/// // rank = 0.99 * 1 = 0.99, floor_index = 0, fraction = 0.99
/// // floor_value = 10, ceiling_value = 20
/// // interpolated = 10 + 10 * 0.99 = 19.9 → 20
/// assert_eq!(p99, 20); // Linear interpolation estimates p99
/// ```
///
/// ## Three elements (small sample)
///
/// ```
/// use needle::stats::calculate_p99;
///
/// let three = vec![10u128, 20, 30];
/// let p99 = calculate_p99(&three);
/// // rank = 0.99 * 2 = 1.98, floor_index = 1, fraction = 0.98
/// // floor_value = 20, ceiling_value = 30
/// // interpolated = 20 + 10 * 0.98 = 29.8 → 30
/// assert_eq!(p99, 30); // Linear interpolation estimates p99
/// ```
///
/// ## Real-world latency example
///
/// ```
/// use needle::stats::calculate_p99;
///
/// // Simulated latency data in milliseconds
/// let latencies = vec![
///     12, 15, 18, 20, 22, 25, 28, 30, 35, 40,
///     45, 50, 55, 60, 70, 80, 90, 100, 120, 150
/// ];
/// let p99 = calculate_p99(&latencies);
/// // rank = 0.99 * 19 = 18.81, floor_index = 18, fraction = 0.81
/// // floor_value = 120, ceiling_value = 150
/// // interpolated = 120 + 30 * 0.81 = 144.3 → 144
/// assert_eq!(p99, 144);
/// ```
///
/// # See Also
///
/// - [`calculate_p95`](fn@calculate_p95) — Calculate the 95th percentile
pub fn calculate_p99(latencies: &[u128]) -> u128 {
    if latencies.is_empty() {
        return 0;
    }

    let n = latencies.len();
    if n == 1 {
        return latencies[0];
    }

    let mut sorted = Vec::from(latencies);
    sorted.sort();

    // Linear interpolation method (like Criterion.rs)
    // Formula: rank = (p / 100) * (n - 1)
    // For p99: rank = 0.99 * (n - 1)
    let rank = 0.99 * (n - 1) as f64;
    let floor_index = rank.floor() as usize;
    let fraction = rank - floor_index as f64;

    let floor_value = sorted[floor_index];
    let ceiling_value = sorted[floor_index + 1];

    // Linear interpolation: floor + (ceiling - floor) * fraction
    let interpolated = floor_value as f64 + (ceiling_value - floor_value) as f64 * fraction;

    // Round to nearest integer.
    // Add a small epsilon to handle floating point precision issues (e.g., 99.5 → 99.4999... → 99).
    // Standard rounding: 99.0-99.499... → 99, 99.5-100.499... → 100
    let epsilon = 1e-9;
    (interpolated + epsilon).round() as u128
}

// ──────────────────────────────────────────────────────────────────────────────
// P95 aggregation utilities
// ──────────────────────────────────────────────────────────────────────────────

/// Collector for aggregating samples across multiple benchmark iterations.
///
/// This struct provides a statistically sound way to aggregate latency measurements
/// across multiple iterations and calculate a single p95 percentile.
///
/// # Statistical Approach
///
/// The correct way to aggregate percentiles across multiple iterations is to:
/// 1. **Pool all samples** from all iterations into a single dataset
/// 2. **Calculate one p95** on the pooled data
///
/// **Do NOT average p95 values** from individual iterations — this is statistically
/// invalid because percentiles are non-linear statistics. Averaging them produces
/// misleading results.
///
/// # Example
///
/// ```no_run
/// use needle::stats::P95Collector;
/// use std::time::Instant;
///
/// let mut collector = P95Collector::new();
///
/// // Run benchmark for 50 iterations
/// for _ in 0..50 {
///     let start = Instant::now();
///     // ... perform work ...
///     collector.record(start.elapsed().as_micros());
/// }
///
/// // Calculate p95 across all iterations
/// let p95_us = collector.p95();
/// println!("p95 latency: {} μs", p95_us);
/// ```
#[derive(Debug, Clone, Default)]
pub struct P95Collector {
    /// All recorded latency samples in microseconds.
    samples: Vec<u128>,
}

impl P95Collector {
    /// Create a new empty collector.
    pub fn new() -> Self {
        Self {
            samples: Vec::new(),
        }
    }

    /// Create a new collector with pre-allocated capacity.
    ///
    /// Use this when you know how many samples you'll collect to avoid
    /// reallocations during benchmarking.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            samples: Vec::with_capacity(capacity),
        }
    }

    /// Record a single latency sample in microseconds.
    pub fn record(&mut self, latency_us: u128) {
        self.samples.push(latency_us);
    }

    /// Record multiple latency samples at once.
    pub fn record_all(&mut self, latencies: impl IntoIterator<Item = u128>) {
        self.samples.extend(latencies);
    }

    /// Calculate the p95 percentile across all recorded samples.
    ///
    /// Returns `0` if no samples have been recorded.
    pub fn p95(&self) -> u128 {
        calculate_p95(&self.samples)
    }

    /// Return the number of samples collected.
    pub fn count(&self) -> usize {
        self.samples.len()
    }

    /// Clear all recorded samples.
    pub fn clear(&mut self) {
        self.samples.clear();
    }

    /// Get a reference to the underlying samples.
    pub fn samples(&self) -> &[u128] {
        &self.samples
    }

    /// Calculate additional statistics on the collected samples.
    ///
    /// Returns `(min, max, avg)` in microseconds, or `None` if no samples.
    pub fn stats(&self) -> Option<(u128, u128, f64)> {
        if self.samples.is_empty() {
            return None;
        }
        let min = *self.samples.iter().min().unwrap();
        let max = *self.samples.iter().max().unwrap();
        let sum: u128 = self.samples.iter().sum();
        let avg = sum as f64 / self.samples.len() as f64;
        Some((min, max, avg))
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

    // ── calculate_p95 tests ───────────────────────────────────────────────────────

    #[test]
    fn calculate_p95_empty() {
        let empty: Vec<u128> = vec![];
        assert_eq!(calculate_p95(&empty), 0);
    }

    #[test]
    fn calculate_p95_single_element() {
        let data = vec![42u128];
        assert_eq!(calculate_p95(&data), 42);
    }

    #[test]
    fn calculate_p95_sorted() {
        let data = vec![10u128, 20, 30, 40, 50, 60, 70, 80, 90, 100];
        // Linear interpolation: rank = 0.95 * 9 = 8.55, floor=8, frac=0.55
        // 90 + (100-90) * 0.55 = 95.5 → 96
        assert_eq!(calculate_p95(&data), 96);
    }

    #[test]
    fn calculate_p95_unsorted() {
        let data = vec![100u128, 10, 50, 30, 70, 40, 60, 20, 80, 90];
        // Same as sorted test after internal sorting
        assert_eq!(calculate_p95(&data), 96);
    }

    #[test]
    fn calculate_p95_twenty_elements() {
        let data = vec![
            10u128, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160, 170, 180,
            190, 200,
        ];
        // Linear interpolation: rank = 0.95 * 19 = 18.05, floor=18, frac=0.05
        // 190 + (200-190) * 0.05 = 190.5 → 191
        assert_eq!(calculate_p95(&data), 191);
    }

    // ── P95Collector tests ─────────────────────────────────────────────────────────

    #[test]
    fn p95_collector_empty() {
        let collector = P95Collector::new();
        assert_eq!(collector.p95(), 0);
        assert_eq!(collector.count(), 0);
        assert!(collector.stats().is_none());
    }

    #[test]
    fn p95_collector_single_sample() {
        let mut collector = P95Collector::new();
        collector.record(42);
        assert_eq!(collector.p95(), 42);
        assert_eq!(collector.count(), 1);
        let stats = collector.stats().unwrap();
        assert_eq!(stats.0, 42); // min
        assert_eq!(stats.1, 42); // max
        assert_eq!(stats.2, 42.0); // avg
    }

    #[test]
    fn p95_collector_multiple_samples() {
        let mut collector = P95Collector::new();
        for i in 1..=10 {
            collector.record(i * 10);
        }
        assert_eq!(collector.count(), 10);
        // Should match calculate_p95 on the same data
        let data: Vec<u128> = (1..=10).map(|i| i * 10).collect();
        assert_eq!(collector.p95(), calculate_p95(&data));
    }

    #[test]
    fn p95_collector_with_capacity() {
        let mut collector = P95Collector::with_capacity(100);
        assert_eq!(collector.count(), 0);
        // Should not reallocate
        for i in 0..100 {
            collector.record(i);
        }
        assert_eq!(collector.count(), 100);
    }

    #[test]
    fn p95_collector_record_all() {
        let mut collector = P95Collector::new();
        let samples = vec![10u128, 20, 30, 40, 50];
        collector.record_all(samples.iter().copied());
        assert_eq!(collector.count(), 5);
        assert_eq!(collector.p95(), calculate_p95(&samples));
    }

    #[test]
    fn p95_collector_stats() {
        let mut collector = P95Collector::new();
        collector.record(10);
        collector.record(20);
        collector.record(30);

        let stats = collector.stats().unwrap();
        assert_eq!(stats.0, 10); // min
        assert_eq!(stats.1, 30); // max
        assert_eq!(stats.2, 20.0); // avg = (10+20+30)/3
    }

    #[test]
    fn p95_collector_clear() {
        let mut collector = P95Collector::new();
        collector.record(100);
        collector.record(200);
        assert_eq!(collector.count(), 2);

        collector.clear();
        assert_eq!(collector.count(), 0);
        assert_eq!(collector.p95(), 0);
        assert!(collector.stats().is_none());
    }

    #[test]
    fn p95_collector_samples_ref() {
        let mut collector = P95Collector::new();
        collector.record(10);
        collector.record(20);
        collector.record(30);

        let samples = collector.samples();
        assert_eq!(samples.len(), 3);
        assert_eq!(samples, &[10, 20, 30]);
    }

    // ── P99 aggregation utilities ─────────────────────────────────────────────────────────

    /// Collector for aggregating samples across multiple benchmark iterations.
    ///
    /// This struct provides a statistically sound way to aggregate latency measurements
    /// across multiple iterations and calculate a single p99 percentile.
    ///
    /// # Statistical Approach
    ///
    /// The correct way to aggregate percentiles across multiple iterations is to:
    /// 1. **Pool all samples** from all iterations into a single dataset
    /// 2. **Calculate one p99** on the pooled data
    ///
    /// **Do NOT average p99 values** from individual iterations — this is statistically
    /// invalid because percentiles are non-linear statistics. Averaging them produces
    /// misleading results.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use needle::stats::P99Collector;
    /// use std::time::Instant;
    ///
    /// let mut collector = P99Collector::new();
    ///
    /// // Run benchmark for 50 iterations
    /// for _ in 0..50 {
    ///     let start = Instant::now();
    ///     // ... perform work ...
    ///     collector.record(start.elapsed().as_micros());
    /// }
    ///
    /// // Calculate p99 across all iterations
    /// let p99_us = collector.p99();
    /// println!("p99 latency: {} μs", p99_us);
    /// ```
    #[derive(Debug, Clone, Default)]
    pub struct P99Collector {
        /// All recorded latency samples in microseconds.
        samples: Vec<u128>,
    }

    impl P99Collector {
        /// Create a new empty collector.
        pub fn new() -> Self {
            Self {
                samples: Vec::new(),
            }
        }

        /// Create a new collector with pre-allocated capacity.
        ///
        /// Use this when you know how many samples you'll collect to avoid
        /// reallocations during benchmarking.
        pub fn with_capacity(capacity: usize) -> Self {
            Self {
                samples: Vec::with_capacity(capacity),
            }
        }

        /// Record a single latency sample in microseconds.
        pub fn record(&mut self, latency_us: u128) {
            self.samples.push(latency_us);
        }

        /// Record multiple latency samples at once.
        pub fn record_all(&mut self, latencies: impl IntoIterator<Item = u128>) {
            self.samples.extend(latencies);
        }

        /// Calculate the p99 percentile across all recorded samples.
        ///
        /// Returns `0` if no samples have been recorded.
        pub fn p99(&self) -> u128 {
            calculate_p99(&self.samples)
        }

        /// Return the number of samples collected.
        pub fn count(&self) -> usize {
            self.samples.len()
        }

        /// Clear all recorded samples.
        pub fn clear(&mut self) {
            self.samples.clear();
        }

        /// Get a reference to the underlying samples.
        pub fn samples(&self) -> &[u128] {
            &self.samples
        }

        /// Calculate additional statistics on the collected samples.
        ///
        /// Returns `(min, max, avg)` in microseconds, or `None` if no samples.
        pub fn stats(&self) -> Option<(u128, u128, f64)> {
            if self.samples.is_empty() {
                return None;
            }
            let min = *self.samples.iter().min().unwrap();
            let max = *self.samples.iter().max().unwrap();
            let sum: u128 = self.samples.iter().sum();
            let avg = sum as f64 / self.samples.len() as f64;
            Some((min, max, avg))
        }
    }

    // ── P99Collector tests ─────────────────────────────────────────────────────────

    #[test]
    fn p99_collector_empty() {
        let collector = P99Collector::new();
        assert_eq!(collector.p99(), 0);
        assert_eq!(collector.count(), 0);
        assert!(collector.stats().is_none());
    }

    #[test]
    fn p99_collector_single_sample() {
        let mut collector = P99Collector::new();
        collector.record(42);
        assert_eq!(collector.p99(), 42);
        assert_eq!(collector.count(), 1);
        let stats = collector.stats().unwrap();
        assert_eq!(stats.0, 42); // min
        assert_eq!(stats.1, 42); // max
        assert_eq!(stats.2, 42.0); // avg
    }

    #[test]
    fn p99_collector_multiple_samples() {
        let mut collector = P99Collector::new();
        for i in 1..=10 {
            collector.record(i * 10);
        }
        assert_eq!(collector.count(), 10);
        // Should match calculate_p99 on the same data
        let data: Vec<u128> = (1..=10).map(|i| i * 10).collect();
        assert_eq!(collector.p99(), calculate_p99(&data));
    }

    #[test]
    fn p99_collector_with_capacity() {
        let mut collector = P99Collector::with_capacity(100);
        assert_eq!(collector.count(), 0);
        // Should not reallocate
        for i in 0..100 {
            collector.record(i);
        }
        assert_eq!(collector.count(), 100);
    }

    #[test]
    fn p99_collector_record_all() {
        let mut collector = P99Collector::new();
        let samples = vec![10u128, 20, 30, 40, 50];
        collector.record_all(samples.iter().copied());
        assert_eq!(collector.count(), 5);
        assert_eq!(collector.p99(), calculate_p99(&samples));
    }

    #[test]
    fn p99_collector_stats() {
        let mut collector = P99Collector::new();
        collector.record(10);
        collector.record(20);
        collector.record(30);

        let stats = collector.stats().unwrap();
        assert_eq!(stats.0, 10); // min
        assert_eq!(stats.1, 30); // max
        assert_eq!(stats.2, 20.0); // avg = (10+20+30)/3
    }

    #[test]
    fn p99_collector_clear() {
        let mut collector = P99Collector::new();
        collector.record(100);
        collector.record(200);
        assert_eq!(collector.count(), 2);

        collector.clear();
        assert_eq!(collector.count(), 0);
        assert_eq!(collector.p99(), 0);
        assert!(collector.stats().is_none());
    }

    #[test]
    fn p99_collector_samples_ref() {
        let mut collector = P99Collector::new();
        collector.record(10);
        collector.record(20);
        collector.record(30);

        let samples = collector.samples();
        assert_eq!(samples.len(), 3);
        assert_eq!(samples, &[10, 20, 30]);
    }

    // ── calculate_p99 tests ───────────────────────────────────────────────────────

    #[test]
    fn calculate_p99_empty() {
        let empty: Vec<u128> = vec![];
        assert_eq!(calculate_p99(&empty), 0);
    }

    #[test]
    fn calculate_p99_single_element() {
        let data = vec![42u128];
        assert_eq!(calculate_p99(&data), 42);
    }

    #[test]
    fn calculate_p99_sorted() {
        let data = vec![10u128, 20, 30, 40, 50, 60, 70, 80, 90, 100];
        // Linear interpolation: rank = 0.99 * 9 = 8.91, floor=8, frac=0.91
        // 90 + (100-90) * 0.91 = 99.1 → 99
        assert_eq!(calculate_p99(&data), 99);
    }

    #[test]
    fn calculate_p99_unsorted() {
        let data = vec![100u128, 10, 50, 30, 70, 40, 60, 20, 80, 90];
        // Same as sorted test after internal sorting
        assert_eq!(calculate_p99(&data), 99);
    }

    #[test]
    fn calculate_p99_twenty_elements() {
        let data = vec![
            10u128, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160, 170, 180,
            190, 200,
        ];
        // Linear interpolation: rank = 0.99 * 19 = 18.81, floor=18, frac=0.81
        // 190 + (200-190) * 0.81 = 198.1 → 198
        assert_eq!(calculate_p99(&data), 198);
    }
}
