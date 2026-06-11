//! Trace sanitization performance benchmark.
//!
//! Benchmarks the sanitizer pipeline (Aho-Corasick keyword pre-filter → regex →
//! entropy check) on representative trace content sizes: 10KB, 100KB, and 1MB.
//!
//! ## Usage
//!
//! Run with:
//! ```bash
//! cargo bench --bench sanitize
//! ```
//!
//! ## Environment Variables
//!
//! - `SANITIZER_LATENCY_THRESHOLD_MS`: Configurable latency threshold in milliseconds
//!   for the assertion-style test (default: 10ms for release builds).
//! - `SANITIZER_BENCH_SAMPLE_COUNT`: Number of iterations for the assertion test
//!   (default: 50).
//!
//! ## Success Criterion
//!
//! Phase 4 success criterion: sanitization must complete in <10ms per 100KB trace
//! on a single core, with Aho-Corasick pre-filter demonstrably skipping irrelevant
//! rules.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use needle::sanitize::Sanitizer;

/// Bytes per trace size benchmark.
const SIZE_10KB: usize = 10 * 1024;
const SIZE_100KB: usize = 100 * 1024;
const SIZE_1MB: usize = 1024 * 1024;

/// Number of iterations for the assertion-style latency test.
/// Higher sample count gives more stable median measurements.
const ASSERTION_SAMPLE_COUNT: usize = 50;

/// Latency threshold for the assertion-style test (milliseconds).
/// Configurable via SANITIZER_LATENCY_THRESHOLD_MS environment variable.
#[allow(dead_code)]
fn latency_threshold_ms() -> u128 {
    std::env::var("SANITIZER_LATENCY_THRESHOLD_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            // Default threshold: 10ms for release builds, 500ms for debug builds.
            // This matches the existing sanitizer_performance test in src/sanitize/mod.rs.
            if cfg!(debug_assertions) {
                500
            } else {
                10
            }
        })
}

/// Generates representative trace content in Claude JSON format.
///
/// The output mimics real agent traces from `.beads/traces/*/stdout.txt`:
/// - JSONL format (one JSON object per line)
/// - Mix of system events, stream events, and tool results
/// - Includes some potential secret patterns (for testing detection)
///
/// # Arguments
///
/// * `target_bytes` - Approximate target size in bytes
///
/// # Returns
///
/// A string of approximately the target size.
fn generate_trace_content(target_bytes: usize) -> String {
    // Sample trace events from real trace files.
    let events = [
        r#"{"type":"system","subtype":"init","cwd":"/home/coding/NEEDLE","session_id":"9d9228c7-0ad5-4b01-a2b8-50f7801c482e","tools":["Task","AskUserQuestion","Bash","Read","Write"],"mcp_servers":[],"model":"claude-sonnet-4-6","permissionMode":"bypassPermissions"}"#,
        r#"{"type":"system","subtype":"status","status":"requesting","uuid":"194bd49c-e341-4ec9-8a56-e8755c4476d8","session_id":"9d9228c7-0ad5-4b01-a2b8-50f7801c482e"}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"The user wants me to implement a feature."},"session_id":"9d9228c7-0ad5-4b01-a2b8-50f7801c482e"}"#,
        r#"{"type":"tool_use","id":"toolu_01","name":"read","input":{"file_path":"/home/coding/NEEDLE/src/main.rs"},"session_id":"9d9228c7-0ad5-4b01-a2b8-50f7801c482e"}"#,
        r#"{"type":"tool_result","id":"toolu_01","output":"// Main entry point for needle binary\nfn main() { ... }","session_id":"9d9228c7-0ad5-4b01-a2b8-50f7801c482e"}"#,
        // Include some patterns that might trigger secret detection (but shouldn't match
        // due to low entropy or other filters).
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Set API_KEY=sk-placeholder-key-for-testing"}}}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Configure DATABASE_URL=postgresql://localhost:5432/testdb"}}}"#,
        // Safe passthrough patterns (should never be redacted).
        r#"{"type":"system","subtype":"bead_update","bead_id":"needle-test-abc123","status":"in_progress"}"#,
        r#"{"type":"tool_use","name":"Bash","input":{"command":"echo 'Processing bead needle-wysd.2.2'}}}"#,
        // Already redacted content (should pass through unchanged).
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Previous output: token=[REDACTED:anthropic-api-key]"}}}"#,
    ];

    // Calculate approximate bytes per event (including newline).
    let avg_event_size = events.iter().map(|s| s.len() + 1).sum::<usize>() / events.len();
    let events_needed = target_bytes / avg_event_size;

    let mut result = String::with_capacity(target_bytes);
    for i in 0..events_needed {
        let event = events[i % events.len()];
        result.push_str(event);
        result.push('\n');
    }

    // Pad to reach exact target size if needed.
    while result.len() < target_bytes {
        result.push_str("{\"type\":\"padding\",\"line\":\"");
        result.push_str(&"x".repeat(100));
        result.push_str("\"}\n");
    }

    // Truncate to exact target size.
    result.truncate(target_bytes);
    result
}

/// Benchmarks sanitization at 10KB trace size.
fn bench_sanitize_10kb(c: &mut Criterion) {
    let sanitizer = Sanitizer::new(&[]).expect("failed to build sanitizer");
    let content = generate_trace_content(SIZE_10KB);

    let mut group = c.benchmark_group("sanitize_10kb");
    group.bench_function("throughput", |b| {
        b.iter(|| {
            let result = sanitizer.sanitize(black_box(&content));
            black_box(result);
        });
    });
    group.finish();
}

/// Benchmarks sanitization at 100KB trace size.
fn bench_sanitize_100kb(c: &mut Criterion) {
    let sanitizer = Sanitizer::new(&[]).expect("failed to build sanitizer");
    let content = generate_trace_content(SIZE_100KB);

    let mut group = c.benchmark_group("sanitize_100kb");
    group.bench_function("throughput", |b| {
        b.iter(|| {
            let result = sanitizer.sanitize(black_box(&content));
            black_box(result);
        });
    });
    group.finish();
}

/// Benchmarks sanitization at 1MB trace size.
fn bench_sanitize_1mb(c: &mut Criterion) {
    let sanitizer = Sanitizer::new(&[]).expect("failed to build sanitizer");
    let content = generate_trace_content(SIZE_1MB);

    let mut group = c.benchmark_group("sanitize_1mb");
    group.bench_function("throughput", |b| {
        b.iter(|| {
            let result = sanitizer.sanitize(black_box(&content));
            black_box(result);
        });
    });
    group.finish();
}

/// Report skip rate statistics for all trace sizes.
fn report_skip_stats(_c: &mut Criterion) {
    let sanitizer = Sanitizer::new(&[]).expect("failed to build sanitizer");
    eprintln!("Sanitizer built with {} rules", sanitizer.rule_count());

    eprintln!("\nKeyword pre-filter skip rate by trace size:");
    for size in [SIZE_10KB, SIZE_100KB, SIZE_1MB].iter() {
        let content = generate_trace_content(*size);
        let stats = sanitizer.measure_skip_stats(&content);
        let size_label = if *size >= 1024 * 1024 {
            format!("{}MB", *size / 1024 / 1024)
        } else if *size >= 1024 {
            format!("{}KB", *size / 1024)
        } else {
            format!("{}B", *size)
        };
        eprintln!("  {}: {}", size_label, stats.format());
    }
}

/// Measures and reports median latency for a 100KB trace.
///
/// This function performs multiple iterations and returns the sorted
/// latency measurements. Used by both the criterion benchmark and the
/// assertion-style test.
#[allow(dead_code)]
fn measure_median_latency_100kb() -> (Vec<u128>, u128) {
    let sanitizer = Sanitizer::new(&[]).expect("failed to build sanitizer");
    let content = generate_trace_content(SIZE_100KB);
    let sample_count = std::env::var("SANITIZER_BENCH_SAMPLE_COUNT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(ASSERTION_SAMPLE_COUNT);

    let mut latencies = Vec::with_capacity(sample_count);

    for _ in 0..sample_count {
        let start = std::time::Instant::now();
        let _ = sanitizer.sanitize(&content);
        let elapsed_ms = start.elapsed().as_millis();
        latencies.push(elapsed_ms);
    }

    latencies.sort();
    let median = latencies[sample_count / 2];

    (latencies, median)
}

/// Assertion-style test that fails if median latency exceeds threshold.
///
/// This test runs independently of criterion (which is expensive) and
/// can be run in CI to enforce the performance requirement.
///
/// To run:
/// ```bash
/// cargo test --bench sanitize -- --nocapture assertion_test
/// ```
#[allow(dead_code)]
fn assertion_test() {
    let (latencies, median) = measure_median_latency_100kb();
    let threshold = latency_threshold_ms();

    // Calculate statistics.
    let min = *latencies.first().unwrap();
    let max = *latencies.last().unwrap();
    let avg = latencies.iter().sum::<u128>() / latencies.len() as u128;
    let p95 = latencies[(latencies.len() * 95) / 100];

    eprintln!(
        "Sanitizer latency assertion test (100KB trace, {} iterations):",
        latencies.len()
    );
    eprintln!("  Min:     {} ms", min);
    eprintln!("  Median:  {} ms", median);
    eprintln!("  Avg:     {} ms", avg);
    eprintln!("  P95:     {} ms", p95);
    eprintln!("  Max:     {} ms", max);
    eprintln!("  Threshold: {} ms", threshold);

    assert!(
        median < threshold,
        "Sanitizer median latency ({} ms) exceeds threshold ({} ms)",
        median,
        threshold
    );
}

/// Specialized benchmark that reports median latency directly.
///
/// This complements the criterion benchmark by providing a simple
/// median measurement that can be easily compared against the threshold.
fn bench_median_latency(c: &mut Criterion) {
    let sanitizer = Sanitizer::new(&[]).expect("failed to build sanitizer");
    let content = generate_trace_content(SIZE_100KB);

    // Warm-up.
    for _ in 0..5 {
        let _ = sanitizer.sanitize(&content);
    }

    let mut latencies = Vec::with_capacity(ASSERTION_SAMPLE_COUNT);
    for _ in 0..ASSERTION_SAMPLE_COUNT {
        let start = std::time::Instant::now();
        let _ = sanitizer.sanitize(&content);
        latencies.push(start.elapsed().as_micros());
    }

    latencies.sort();
    let median_us = latencies[ASSERTION_SAMPLE_COUNT / 2];
    let median_ms = median_us as f64 / 1000.0;

    eprintln!(
        "Median latency for 100KB trace: {:.2} ms ({} samples)",
        median_ms, ASSERTION_SAMPLE_COUNT
    );
    eprintln!(
        "  Min: {:.2} ms",
        *latencies.first().unwrap() as f64 / 1000.0
    );
    eprintln!(
        "  Max: {:.2} ms",
        *latencies.last().unwrap() as f64 / 1000.0
    );
    eprintln!(
        "  P95: {:.2} ms",
        latencies[(latencies.len() * 95) / 100] as f64 / 1000.0
    );

    // Report to criterion for plotting.
    c.bench_function("median_latency_100kb", |b| {
        b.iter(|| {
            let _ = sanitizer.sanitize(black_box(&content));
        });
    });
}

criterion_group!(
    benches,
    bench_sanitize_10kb,
    bench_sanitize_100kb,
    bench_sanitize_1mb,
    bench_median_latency,
    report_skip_stats
);
criterion_main!(benches);

/// Entry point for running the assertion test as a standalone binary.
///
/// This allows running the test without criterion's overhead:
/// ```bash
/// cargo test --bench sanitize -- --nocapture --test-threads=1
/// ```
#[cfg(test)]
mod assertion_tests {
    #[test]
    fn sanitizer_latency_below_threshold() {
        super::assertion_test();
    }
}
