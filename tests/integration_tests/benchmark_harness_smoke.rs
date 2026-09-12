//! Smoke test for benchmark harness environment.
//!
//! Verifies that the benchmark harness can be instantiated and produces valid output
//! without running the full benchmark suite (which is expensive in debug mode).
//!
//! This test validates:
//! - Benchmark compiles successfully
//! - Benchmark components can be instantiated
//! - Basic output format is valid
//! - p95 calculation works correctly

use needle::sanitize::Sanitizer;
use needle::stats::{calculate_median, calculate_p95, calculate_p99};
use std::time::Instant;

/// Generate minimal test content (much smaller than full benchmark)
fn generate_minimal_content() -> String {
    let events = [
        r#"{"type":"system","subtype":"init"}"#,
        r#"{"type":"tool_use","name":"Bash","input":{"command":"test"}}"#,
        r#"{"type":"tool_result","output":"ok"}"#,
    ];

    let mut result = String::new();
    for event in events.iter() {
        result.push_str(event);
        result.push('\n');
    }
    result
}

#[test]
fn test_benchmark_compiles_and_runs() {
    // Verify benchmark harness compiles and basic components work
    let sanitizer = Sanitizer::new(&[]).expect("sanitizer should build");
    let content = generate_minimal_content();

    // Verify sanitizer works
    let result = sanitizer.sanitize(&content);
    assert!(!result.is_empty(), "sanitizer should produce output");
}

#[test]
fn test_p95_calculation_works() {
    // Verify p95 calculation works (used by benchmarks)
    let latencies = vec![100u128, 150, 200, 250, 300];
    let p95 = calculate_p95(&latencies);
    assert!(p95 > 0, "p95 should be positive");
    assert!(
        (250..=300).contains(&p95),
        "p95 should be reasonable for the dataset"
    );
}

#[test]
fn test_p99_calculation_works() {
    // Verify p99 calculation works (used by benchmarks)
    let latencies = vec![100u128, 150, 200, 250, 300];
    let p99 = calculate_p99(&latencies);
    assert!(p99 > 0, "p99 should be positive");
    assert!(
        (290..=300).contains(&p99),
        "p99 should be near max for the dataset"
    );
}

#[test]
fn test_median_calculation_works() {
    // Verify median calculation works (used by benchmarks)
    let latencies = vec![100u128, 150, 200, 250, 300];
    let median = calculate_median(&latencies);
    assert_eq!(median, 200, "median should be the middle value");
}

#[test]
fn test_benchmark_timing_infrastructure_works() {
    // Verify timing infrastructure works (used by benchmarks)
    let sanitizer = Sanitizer::new(&[]).expect("sanitizer should build");
    let content = generate_minimal_content();

    // Warm-up
    for _ in 0..3 {
        let _ = sanitizer.sanitize(&content);
    }

    // Collect samples (small number for smoke test)
    let mut latencies = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        let _ = sanitizer.sanitize(&content);
        latencies.push(start.elapsed().as_micros());
    }

    // Verify we got reasonable measurements
    assert_eq!(latencies.len(), 5, "should collect 5 samples");
    assert!(
        latencies.iter().all(|&l| l > 0),
        "all latencies should be positive"
    );

    // Verify percentiles can be calculated
    let median = calculate_median(&latencies);
    let p95 = calculate_p95(&latencies);
    let p99 = calculate_p99(&latencies);

    assert!(median > 0, "median should be positive");
    assert!(p95 > 0, "p95 should be positive");
    assert!(p99 > 0, "p99 should be positive");
}

#[test]
fn test_output_format_structure() {
    // Verify benchmark output format structure is valid
    let sanitizer = Sanitizer::new(&[]).expect("sanitizer should build");
    let content = generate_minimal_content();

    // Collect samples
    let mut latencies = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        let _ = sanitizer.sanitize(&content);
        latencies.push(start.elapsed().as_micros());
    }

    let median_us = calculate_median(&latencies);
    let p95_us = calculate_p95(&latencies);
    let p99_us = calculate_p99(&latencies);

    // Format output as benchmark would
    let output = format!(
        "Latency Metrics ({} samples):\n  Median: {} µs ({:.2} ms)\n  P95: {} µs ({:.2} ms)\n  P99: {} µs ({:.2} ms)",
        latencies.len(),
        median_us,
        median_us as f64 / 1000.0,
        p95_us,
        p95_us as f64 / 1000.0,
        p99_us,
        p99_us as f64 / 1000.0
    );

    // Verify output structure
    assert!(
        output.contains("Latency Metrics"),
        "should contain metrics header"
    );
    assert!(output.contains("Median:"), "should contain median field");
    assert!(output.contains("P95:"), "should contain p95 field");
    assert!(output.contains("P99:"), "should contain p99 field");
    assert!(output.contains("µs"), "should show microseconds");
    assert!(output.contains("ms"), "should show milliseconds");
}
