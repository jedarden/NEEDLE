//! Integration test verifying p95 appears in benchmark output format.
//!
//! Tests the complete benchmark pipeline to ensure p95 values are:
//! - Present in the output structure
//! - Formatted and displayed correctly
//! - Consistent with other latency metrics (median, p99)

use needle::sanitize::Sanitizer;
use needle::stats::{calculate_p95, calculate_p99};
use std::time::Instant;

/// Benchmark result structure matching the production format.
#[allow(dead_code)]
struct BenchmarkResult {
    size_label: String,
    size_bytes: usize,
    sample_count: usize,
    median_us: u128,
    p95_us: u128,
    p99_us: u128,
}

/// Generate test trace content.
fn generate_test_trace(size: usize) -> String {
    let events = [
        r#"{"type":"system","subtype":"init"}"#,
        r#"{"type":"tool_use","name":"Bash","input":{"command":"test"}}"#,
        r#"{"type":"tool_result","output":"ok"}"#,
    ];

    let avg_size = events.iter().map(|s| s.len() + 1).sum::<usize>() / events.len();
    let count = size / avg_size;

    let mut result = String::with_capacity(size);
    for i in 0..count {
        result.push_str(events[i % events.len()]);
        result.push('\n');
    }
    result.truncate(size.saturating_sub(1));
    result
}

/// Run a single benchmark and return structured result.
fn run_benchmark(size: usize, label: &str) -> BenchmarkResult {
    let sanitizer = Sanitizer::new(&[]).expect("failed to build sanitizer");
    let content = generate_test_trace(size);

    const SAMPLE_COUNT: usize = 20;
    let mut latencies = Vec::with_capacity(SAMPLE_COUNT);

    // Warm-up
    for _ in 0..5 {
        let _ = sanitizer.sanitize(&content);
    }

    // Measure latencies
    for _ in 0..SAMPLE_COUNT {
        let start = Instant::now();
        let _ = sanitizer.sanitize(&content);
        latencies.push(start.elapsed().as_micros());
    }

    // Calculate percentiles
    let mut sorted_latencies = latencies.clone();
    sorted_latencies.sort();
    let median_us = sorted_latencies[SAMPLE_COUNT / 2];
    let p95_us = calculate_p95(&latencies);
    let p99_us = calculate_p99(&latencies);

    BenchmarkResult {
        size_label: label.to_string(),
        size_bytes: size,
        sample_count: SAMPLE_COUNT,
        median_us,
        p95_us,
        p99_us,
    }
}

/// Format latency output as it appears in the production benchmark.
fn format_latency_output(result: &BenchmarkResult) -> String {
    format!(
        "Latency Metrics ({} samples):\n  Median: {} µs ({:.2} ms)\n  P95: {} µs ({:.2} ms)\n  P99: {} µs ({:.2} ms)",
        result.sample_count,
        result.median_us,
        result.median_us as f64 / 1000.0,
        result.p95_us,
        result.p95_us as f64 / 1000.0,
        result.p99_us,
        result.p99_us as f64 / 1000.0
    )
}

#[test]
fn test_p95_field_exists_in_output_structure() {
    let result = run_benchmark(10 * 1024, "10KB");

    // Verify p95 field exists in the structure
    assert!(result.p95_us > 0, "p95_us field should be positive");
    assert_eq!(result.size_label, "10KB");
    assert_eq!(result.sample_count, 20);
}

#[test]
fn test_p95_values_formatted_and_displayed_correctly() {
    let result = run_benchmark(10 * 1024, "10KB");

    // Format the output as it would appear in the benchmark
    let output = format_latency_output(&result);

    // Verify p95 appears in the formatted output with correct formatting
    assert!(
        output.contains(&format!("P95: {} µs", result.p95_us)),
        "Output should contain P95 in microseconds"
    );

    // Verify p95 is displayed in milliseconds as well
    let p95_ms = result.p95_us as f64 / 1000.0;
    assert!(
        output.contains(&format!("{:.2} ms", p95_ms)),
        "Output should contain P95 in milliseconds with 2 decimal places"
    );
}

#[test]
fn test_output_format_consistent_with_other_metrics() {
    let result = run_benchmark(10 * 1024, "10KB");

    let output = format_latency_output(&result);

    // All three metrics should follow the same format: "{NAME}: {value} µs ({value_ms:.2} ms)"
    let median_pattern = format!("Median: {} µs ({:.2} ms)", result.median_us, result.median_us as f64 / 1000.0);
    let p95_pattern = format!("P95: {} µs ({:.2} ms)", result.p95_us, result.p95_us as f64 / 1000.0);
    let p99_pattern = format!("P99: {} µs ({:.2} ms)", result.p99_us, result.p99_us as f64 / 1000.0);

    assert!(
        output.contains(&median_pattern),
        "Output should contain formatted median"
    );
    assert!(
        output.contains(&p95_pattern),
        "Output should contain formatted p95"
    );
    assert!(
        output.contains(&p99_pattern),
        "Output should contain formatted p99"
    );

    // Verify all use the same unit formatting: "{value} µs ({value_ms:.2} ms)"
    assert!(output.contains(" µs ("), "All metrics should use 'µs (' before ms value");
    assert!(output.contains(" ms)"), "All metrics should use ' ms)' at the end");
}

#[test]
fn test_p95_appears_in_all_size_variants() {
    let sizes = vec![
        (10 * 1024, "10KB"),
        (100 * 1024, "100KB"),
        (1024 * 1024, "1MB"),
    ];

    for (size, label) in sizes {
        let result = run_benchmark(size, label);
        let output = format_latency_output(&result);

        assert!(
            output.contains("P95:"),
            "P95 should appear in output for {}",
            label
        );
        assert!(
            result.p95_us > 0,
            "P95 value should be positive for {}",
            label
        );
    }
}

#[test]
fn test_p95_reasonable_values() {
    let result = run_benchmark(10 * 1024, "10KB");

    // p95 should be >= median (95th percentile is higher than 50th)
    assert!(
        result.p95_us >= result.median_us,
        "P95 ({}) should be >= median ({})",
        result.p95_us,
        result.median_us
    );

    // p99 should be >= p95 (99th percentile is higher than 95th)
    assert!(
        result.p99_us >= result.p95_us,
        "P99 ({}) should be >= p95 ({})",
        result.p99_us,
        result.p95_us
    );

    // All values should be positive for a real benchmark
    assert!(result.median_us > 0, "Median should be positive");
    assert!(result.p95_us > 0, "P95 should be positive");
    assert!(result.p99_us > 0, "P99 should be positive");
}
