//! Verify all latency metrics and skip rate for all size variants.
//!
//! This example runs comprehensive tests for 10KB, 100KB, and 1MB trace sizes,
//! measuring throughput, latency percentiles (median, p95, p99), and skip rate
//! from the keyword pre-filter.

use needle::sanitize::Sanitizer;
use needle::stats::{calculate_p95, calculate_p99};
use std::time::Instant;

/// Size variants for testing
const SIZE_10KB: usize = 10 * 1024;
const SIZE_100KB: usize = 100 * 1024;
const SIZE_1MB: usize = 1024 * 1024;

/// Sample count for latency measurements
const SAMPLE_COUNT: usize = 20;

/// Generate representative trace content in Claude JSON format.
fn generate_trace_content(target_bytes: usize) -> String {
    let events = [
        r#"{"type":"system","subtype":"init","cwd":"/home/coding/NEEDLE","session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769","tools":["Task","Bash","Read","Write","Edit"],"mcp_servers":[],"model":"glm-4.7","permissionMode":"bypassPermissions"}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Processing data"}}}"#,
        r#"{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}"#,
        r#"{"type":"tool_result","output":"test result: ok"}"#,
        r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":15,"estimated_tokens_delta":2}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me break down this task"}}}"#,
        r#"{"type":"tool_use","name":"Read","input":{"file_path":"/home/coding/NEEDLE/src/lib.rs"}}"#,
        r#"{"type":"tool_result","output":"pub mod sanitize;\npub mod telemetry;\n"}"#,
    ];

    let avg_size = events.iter().map(|s| s.len() + 1).sum::<usize>() / events.len();
    let count = target_bytes / avg_size;

    let mut result = String::with_capacity(target_bytes);
    for i in 0..count {
        result.push_str(events[i % events.len()]);
        result.push('\n');
    }

    while result.len() < target_bytes {
        result.push_str("{\"type\":\"pad\"}\n");
    }
    result.truncate(target_bytes);
    result
}

/// Run comprehensive benchmark for a single size variant
fn benchmark_size_variant(sanitizer: &Sanitizer, size: usize, size_label: &str) -> BenchmarkResult {
    let content = generate_trace_content(size);

    // Warm-up
    for _ in 0..5 {
        let _ = sanitizer.sanitize(&content);
    }

    // Measure throughput and latency
    let start_total = Instant::now();
    let mut latencies = Vec::with_capacity(SAMPLE_COUNT);

    for _ in 0..SAMPLE_COUNT {
        let start = Instant::now();
        let _ = sanitizer.sanitize(&content);
        latencies.push(start.elapsed().as_micros());
    }

    let total_duration = start_total.elapsed();

    // Calculate latency percentiles
    let mut sorted_latencies = latencies.clone();
    sorted_latencies.sort();
    let median_us = sorted_latencies[SAMPLE_COUNT / 2];
    let p95_us = calculate_p95(&latencies);
    let p99_us = calculate_p99(&latencies);

    // Calculate throughput (bytes/second and ops/second)
    let total_bytes = size * SAMPLE_COUNT;
    let throughput_bytes_per_sec = total_bytes as f64 / total_duration.as_secs_f64();
    let throughput_ops_per_sec = SAMPLE_COUNT as f64 / total_duration.as_secs_f64();

    // Measure skip rate
    let skip_stats = sanitizer.measure_skip_stats(&content);

    BenchmarkResult {
        size_label: size_label.to_string(),
        size_bytes: size,
        sample_count: SAMPLE_COUNT,
        median_us,
        p95_us,
        p99_us,
        throughput_bytes_per_sec,
        throughput_ops_per_sec,
        skip_stats,
    }
}

struct BenchmarkResult {
    size_label: String,
    size_bytes: usize,
    sample_count: usize,
    median_us: u128,
    p95_us: u128,
    p99_us: u128,
    throughput_bytes_per_sec: f64,
    throughput_ops_per_sec: f64,
    skip_stats: needle::sanitize::SkipStats,
}

fn format_bytes_per_sec(bps: f64) -> String {
    if bps >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.2} GB/s", bps / (1024.0 * 1024.0 * 1024.0))
    } else if bps >= 1024.0 * 1024.0 {
        format!("{:.2} MB/s", bps / (1024.0 * 1024.0))
    } else if bps >= 1024.0 {
        format!("{:.2} KB/s", bps / 1024.0)
    } else {
        format!("{:.2} B/s", bps)
    }
}

fn print_benchmark_result(result: &BenchmarkResult) {
    println!("\n=== {} Trace ({}) ===", result.size_label, result.size_bytes);
    println!("Latency Metrics ({} samples):", result.sample_count);
    println!(
        "  Median: {} µs ({:.2} ms)",
        result.median_us,
        result.median_us as f64 / 1000.0
    );
    println!(
        "  P95: {} µs ({:.2} ms)",
        result.p95_us,
        result.p95_us as f64 / 1000.0
    );
    println!(
        "  P99: {} µs ({:.2} ms)",
        result.p99_us,
        result.p99_us as f64 / 1000.0
    );

    println!("\nThroughput Metrics:");
    println!(
        "  Data rate: {} ({:.2} bytes/sec)",
        format_bytes_per_sec(result.throughput_bytes_per_sec),
        result.throughput_bytes_per_sec
    );
    println!(
        "  Ops rate: {:.2} ops/sec",
        result.throughput_ops_per_sec
    );

    println!("\nKeyword Pre-filter Skip Rate:");
    println!(
        "  Total rule checks: {}",
        result.skip_stats.total_checks
    );
    println!(
        "  Skipped by keywords: {}",
        result.skip_stats.skipped_by_keywords
    );
    println!(
        "  Skip rate: {:.1}%",
        result.skip_stats.skip_rate * 100.0
    );
}

fn verify_benchmark_result(result: &BenchmarkResult) {
    // Verify latency metrics are reasonable
    assert!(
        result.median_us > 0,
        "{}: Median latency must be positive",
        result.size_label
    );
    assert!(
        result.p95_us >= result.median_us,
        "{}: P95 must be >= median",
        result.size_label
    );
    assert!(
        result.p99_us >= result.p95_us,
        "{}: P99 must be >= P95",
        result.size_label
    );

    // Verify throughput is positive
    assert!(
        result.throughput_bytes_per_sec > 0.0,
        "{}: Throughput must be positive",
        result.size_label
    );
    assert!(
        result.throughput_ops_per_sec > 0.0,
        "{}: Ops/sec must be positive",
        result.size_label
    );

    // Verify skip rate statistics
    assert!(
        result.skip_stats.total_checks > 0,
        "{}: Must have performed rule checks",
        result.size_label
    );
    assert!(
        result.skip_stats.skip_rate >= 0.0 && result.skip_stats.skip_rate <= 1.0,
        "{}: Skip rate must be between 0.0 and 1.0",
        result.size_label
    );
    assert!(
        result.skip_stats.skipped_by_keywords <= result.skip_stats.total_checks,
        "{}: Skipped checks must not exceed total checks",
        result.size_label
    );

    println!("  ✓ All metrics verified for {}", result.size_label);
}

fn main() {
    println!("=== Verify All Latency Metrics and Skip Rate ===");
    println!("Testing 3 size variants: 10KB, 100KB, 1MB\n");

    // Build sanitizer
    let sanitizer = Sanitizer::new(&[]).expect("failed to build sanitizer");
    println!("Built sanitizer with {} rules\n", sanitizer.rule_count());

    // Benchmark all 3 size variants
    let result_10kb = benchmark_size_variant(&sanitizer, SIZE_10KB, "10KB");
    let result_100kb = benchmark_size_variant(&sanitizer, SIZE_100KB, "100KB");
    let result_1mb = benchmark_size_variant(&sanitizer, SIZE_1MB, "1MB");

    // Print all results
    print_benchmark_result(&result_10kb);
    verify_benchmark_result(&result_10kb);

    print_benchmark_result(&result_100kb);
    verify_benchmark_result(&result_100kb);

    print_benchmark_result(&result_1mb);
    verify_benchmark_result(&result_1mb);

    // Cross-size verification
    println!("\n=== Cross-Size Verification ===");

    // Verify skip rate increases with size (more content = more keyword matches)
    assert!(
        result_100kb.skip_stats.total_checks > result_10kb.skip_stats.total_checks,
        "100KB should perform more total checks than 10KB"
    );
    println!("✓ Total checks increase with trace size");

    // Verify latency scales appropriately (larger traces take longer)
    assert!(
        result_1mb.median_us > result_10kb.median_us,
        "1MB median latency should be higher than 10KB"
    );
    println!("✓ Latency scales with trace size");

    // Verify all percentiles are present
    println!("\n=== Final Verification ===");
    println!("✓ Median latency is reported for all 3 size variants");
    println!("✓ P95 latency is reported for all 3 size variants");
    println!("✓ P99 latency is reported for all 3 size variants");
    println!("✓ Throughput (bytes/sec) is reported for all 3 size variants");
    println!("✓ Throughput (ops/sec) is reported for all 3 size variants");
    println!("✓ Skip rate is reported as percentage for all 3 size variants");
    println!("✓ Total rule checks are tracked for all 3 size variants");

    println!("\n=== All Metrics Verified Successfully ===");
}
