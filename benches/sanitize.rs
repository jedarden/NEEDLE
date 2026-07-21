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

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use needle::sanitize::Sanitizer;
use needle::stats::{calculate_p95, calculate_p99};

/// Configure Criterion for p95 and p99 latency reporting.
///
/// Creates a Criterion instance configured to capture accurate p95 and p99 percentiles:
/// - Confidence level: 0.95 (95% confidence interval for reported statistics)
/// - Sample size: 100 measurements (more accurate percentiles via bootstrap)
/// - Warm-up time: 3 seconds (allows CPU cache/JIT warm-up)
/// - Measurement time: 5 seconds (sufficient samples for stable percentiles)
/// - Noise threshold: 0.02 (2% noise filtering for stable measurements)
///
/// P95 calculation is done using `needle::stats::calculate_p95()` and p99 using
/// `needle::stats::calculate_p99()`, which implement linear interpolation for accurate
/// percentile estimation. Criterion.rs also calculates percentiles automatically via
/// bootstrap analysis (see criterion.toml).
///
/// The confidence_level affects the confidence interval around the mean,
/// not the percentile calculation itself. Percentiles use bootstrap analysis.
fn configure_criterion() -> Criterion {
    Criterion::default()
        .confidence_level(0.95)
        .sample_size(100)
        .warm_up_time(std::time::Duration::from_secs(3))
        .measurement_time(std::time::Duration::from_secs(5))
        .noise_threshold(0.02)
}

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
        .unwrap_or({
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
/// - Mix of system events, stream events, thinking deltas, and tool results
/// - Includes realistic code snippets (Rust, shell, JSON)
/// - Deterministic: same input produces identical output
///
/// # Arguments
///
/// * `target_bytes` - Approximate target size in bytes
///
/// # Returns
///
/// A string of approximately the target size.
fn generate_trace_content(target_bytes: usize) -> String {
    // System events from real traces (init, hooks, status)
    let system_events = [
        r#"{"type":"system","subtype":"init","cwd":"/home/coding/NEEDLE","session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769","tools":["Task","Bash","Read","Write","Edit"],"mcp_servers":[],"model":"glm-4.7","permissionMode":"bypassPermissions"}"#,
        r#"{"type":"system","subtype":"status","status":"requesting","uuid":"a26811cd-e0c3-411c-9ef1-ab7630de71d0","session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769"}"#,
        r#"{"type":"system","subtype":"hook_started","hook_id":"df3f5e15-9390-4080-a979-4d2ced90215f","hook_name":"SessionStart:startup","hook_event":"SessionStart","uuid":"e7553e67-80ef-426a-927b-c652a6fcf08e","session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769"}"#,
        r#"{"type":"system","subtype":"hook_response","hook_id":"df3f5e15-9390-4080-a979-4d2ced90215f","hook_name":"SessionStart:startup","hook_event":"SessionStart","output":"","stdout":"","stderr":"","exit_code":0,"outcome":"success","uuid":"3ab2daae-1f5a-4eba-9338-f7a36f4e1cd0","session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769"}"#,
        r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":15,"estimated_tokens_delta":2,"uuid":"6e7d97ec-d48b-43b1-8f2e-fdb80234b714","session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769"}"#,
    ];

    // Stream events with thinking deltas (word-by-word output from real traces)
    let thinking_events = [
        r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":"4b94f30d883f44b4b168c5b7"}},"session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769","parent_tool_use_id":null,"uuid":"03d826c2-944b-45dc-be63-d1befa23437a"}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let"}},"session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769","parent_tool_use_id":null,"uuid":"6785a332-dc25-48c7-942d-422c7eb3fe16"}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":" me"}},"session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769","parent_tool_use_id":null,"uuid":"51c1dd21-22da-4a0a-a871-46b394d3cb4e"}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":" break"}},"session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769","parent_tool_use_id":null,"uuid":"42385a6c-563f-41e5-8cf2-23f998586e43"}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":" down"}},"session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769","parent_tool_use_id":null,"uuid":"aeb6a89f-a233-4acd-b015-45278d92f14c"}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":" this"}},"session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769","parent_tool_use_id":null,"uuid":"d44c0e7d-5053-4890-b1e0-f70c7c9aa416"}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":" task"}},"session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769","parent_tool_use_id":null,"uuid":"8006bffd-da97-4105-9beb-ca4e301b86ed"}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":":"}},"session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769","parent_tool_use_id":null,"uuid":"2d0206c7-407a-4311-982a-3c5a6c624e5f"}"#,
    ];

    // Text delta events (normal output text)
    let text_events = [
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"The user wants me to implement a feature."}},"session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769","parent_tool_use_id":null,"uuid":"b75f0256-90d2-4886-85cb-402428ed7a72"}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"I need to read the source files first."}},"session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769","parent_tool_use_id":null,"uuid":"c71a1034-2189-4674-b9d4-adaa09a61d0d"}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Let me check the existing implementation."}},"session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769","parent_tool_use_id":null,"uuid":"3ab2daae-1f5a-4eba-9338-f7a36f4e1cd0"}"#,
    ];

    // Tool use events (agent invoking tools)
    let tool_use_events = [
        r#"{"type":"tool_use","id":"toolu_01","name":"read","input":{"file_path":"/home/coding/NEEDLE/src/lib.rs"},"session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769"}"#,
        r#"{"type":"tool_use","id":"toolu_02","name":"bash","input":{"command":"cargo test --lib sanitize"},"session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769"}"#,
        r#"{"type":"tool_use","id":"toolu_03","name":"edit","input":{"file_path":"/home/coding/NEEDLE/src/sanitize/mod.rs","old_string":"fn foo() {}","new_string":"fn bar() {}"},"session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769"}"#,
    ];

    // Tool result events with realistic code snippets
    let tool_result_events = [
        r#"{"type":"tool_result","id":"toolu_01","output":"pub mod sanitize;\npub mod telemetry;\npub mod config;\n\nuse anyhow::Result;\n","session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769"}"#,
        r#"{"type":"tool_result","id":"toolu_02","output":"running 3 tests\ntest sanitize::tests::test_basic ... ok\ntest sanitize::tests::test_entropy ... ok\ntest sanitize::tests::test_patterns ... ok\n\nresult: ok. 3 passed; 0 failed","session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769"}"#,
        r#"{"type":"tool_result","id":"toolu_03","output":"// Sanitizer implementation\npub struct Sanitizer {\n    rules: Vec<Rule>,\n    ac_matcher: AhoCorasick,\n}\n\nimpl Sanitizer {\n    pub fn new(patterns: &[&str]) -> Result<Self> {\n        // Build Aho-Corasick automaton\n        let ac_matcher = AhoCorasick::new(patterns)?;\n        Ok(Self { rules: vec![], ac_matcher })\n    }\n}\n","session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769"}"#,
    ];

    // Shell command patterns (should never be redacted)
    let safe_commands = [
        r#"{"type":"tool_use","name":"Bash","input":{"command":"git status"},"session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769"}"#,
        r#"{"type":"tool_use","name":"Bash","input":{"command":"echo 'Processing bead needle-wysd.2.2'"},"session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769"}"#,
        r#"{"type":"tool_use","name":"Bash","input":{"command":"cargo fmt && cargo clippy"},"session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769"}"#,
        r#"{"type":"tool_result","output":"On branch main\nChanges not staged for commit:\n  modified:   src/sanitize/mod.rs\n  modified:   benches/sanitize.rs","session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769"}"#,
    ];

    // Patterns that look like secrets but are safe (low entropy, placeholders)
    let safe_patterns = [
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Set API_KEY=sk-placeholder-key-for-testing"}}}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Configure DATABASE_URL=postgresql://localhost:5432/testdb"}}}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Use TOKEN=abc123-example-token"}}}"#,
    ];

    // Already redacted content (should pass through unchanged)
    let redacted_content = [
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Previous output: token=[REDACTED:anthropic-api-key]"}}}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Password: [REDACTED:user-password]"}}}"#,
    ];

    // Combine all event categories for deterministic cycling
    let all_events: Vec<&str> = [
        &system_events[..],
        &thinking_events[..],
        &text_events[..],
        &tool_use_events[..],
        &tool_result_events[..],
        &safe_commands[..],
        &safe_patterns[..],
        &redacted_content[..],
    ]
    .concat();

    // Calculate approximate bytes per event (including newline)
    let avg_event_size = all_events.iter().map(|s| s.len() + 1).sum::<usize>() / all_events.len();
    let events_needed = target_bytes / avg_event_size;

    let mut result = String::with_capacity(target_bytes);
    for i in 0..events_needed {
        let event = all_events[i % all_events.len()];
        result.push_str(event);
        result.push('\n');
    }

    // Pad to reach exact target size if needed
    while result.len() < target_bytes {
        result.push_str("{\"type\":\"padding\",\"line\":\"");
        result.push_str(&"x".repeat(100));
        result.push_str("\"}\n");
    }

    // Truncate to exact target size
    result.truncate(target_bytes);
    result
}

/// Benchmarks sanitization at 10KB trace size with explicit p95 output.
fn bench_sanitize_10kb(c: &mut Criterion) {
    let sanitizer = Sanitizer::new(&[]).expect("failed to build sanitizer");
    let content = generate_trace_content(SIZE_10KB);

    // Warm-up
    for _ in 0..5 {
        let _ = sanitizer.sanitize(&content);
    }

    // Collect samples for explicit p95 calculation
    let mut latencies = Vec::with_capacity(ASSERTION_SAMPLE_COUNT);
    for _ in 0..ASSERTION_SAMPLE_COUNT {
        let start = std::time::Instant::now();
        let _ = sanitizer.sanitize(&content);
        latencies.push(start.elapsed().as_micros());
    }

    let p95_us = calculate_p95(&latencies);
    let p95_ms = p95_us as f64 / 1000.0;
    let p99_us = calculate_p99(&latencies);
    let p99_ms = p99_us as f64 / 1000.0;

    eprintln!(
        "10KB trace p95 latency: {:.2} ms, p99: {:.2} ms ({} samples)",
        p95_ms, p99_ms, ASSERTION_SAMPLE_COUNT
    );

    let mut group = c.benchmark_group("sanitize_10kb");
    group.throughput(Throughput::Bytes(SIZE_10KB as u64));
    group.bench_function("throughput_bytes", |b| {
        b.iter(|| {
            let result = sanitizer.sanitize(black_box(&content));
            black_box(result);
        });
    });
    group.finish();
}

/// Benchmarks sanitization at 10KB trace size (ops/sec).
fn bench_sanitize_10kb_ops(c: &mut Criterion) {
    let sanitizer = Sanitizer::new(&[]).expect("failed to build sanitizer");
    let content = generate_trace_content(SIZE_10KB);

    let mut group = c.benchmark_group("sanitize_10kb");
    group.throughput(Throughput::Elements(1));
    group.bench_function("throughput_ops", |b| {
        b.iter(|| {
            let result = sanitizer.sanitize(black_box(&content));
            black_box(result);
        });
    });
    group.finish();
}

/// Benchmarks sanitization at 100KB trace size with explicit p95 output.
fn bench_sanitize_100kb(c: &mut Criterion) {
    let sanitizer = Sanitizer::new(&[]).expect("failed to build sanitizer");
    let content = generate_trace_content(SIZE_100KB);

    // Warm-up
    for _ in 0..5 {
        let _ = sanitizer.sanitize(&content);
    }

    // Collect samples for explicit p95 calculation
    let mut latencies = Vec::with_capacity(ASSERTION_SAMPLE_COUNT);
    for _ in 0..ASSERTION_SAMPLE_COUNT {
        let start = std::time::Instant::now();
        let _ = sanitizer.sanitize(&content);
        latencies.push(start.elapsed().as_micros());
    }

    let p95_us = calculate_p95(&latencies);
    let p95_ms = p95_us as f64 / 1000.0;
    let p99_us = calculate_p99(&latencies);
    let p99_ms = p99_us as f64 / 1000.0;

    eprintln!(
        "100KB trace p95 latency: {:.2} ms, p99: {:.2} ms ({} samples)",
        p95_ms, p99_ms, ASSERTION_SAMPLE_COUNT
    );

    let mut group = c.benchmark_group("sanitize_100kb");
    group.throughput(Throughput::Bytes(SIZE_100KB as u64));
    group.bench_function("throughput_bytes", |b| {
        b.iter(|| {
            let result = sanitizer.sanitize(black_box(&content));
            black_box(result);
        });
    });
    group.finish();
}

/// Benchmarks sanitization at 100KB trace size (ops/sec).
fn bench_sanitize_100kb_ops(c: &mut Criterion) {
    let sanitizer = Sanitizer::new(&[]).expect("failed to build sanitizer");
    let content = generate_trace_content(SIZE_100KB);

    let mut group = c.benchmark_group("sanitize_100kb");
    group.throughput(Throughput::Elements(1));
    group.bench_function("throughput_ops", |b| {
        b.iter(|| {
            let result = sanitizer.sanitize(black_box(&content));
            black_box(result);
        });
    });
    group.finish();
}

/// Benchmarks sanitization at 1MB trace size with explicit p95 output.
fn bench_sanitize_1mb(c: &mut Criterion) {
    let sanitizer = Sanitizer::new(&[]).expect("failed to build sanitizer");
    let content = generate_trace_content(SIZE_1MB);

    // Warm-up
    for _ in 0..5 {
        let _ = sanitizer.sanitize(&content);
    }

    // Collect samples for explicit p95 calculation
    let mut latencies = Vec::with_capacity(ASSERTION_SAMPLE_COUNT);
    for _ in 0..ASSERTION_SAMPLE_COUNT {
        let start = std::time::Instant::now();
        let _ = sanitizer.sanitize(&content);
        latencies.push(start.elapsed().as_micros());
    }

    let p95_us = calculate_p95(&latencies);
    let p95_ms = p95_us as f64 / 1000.0;
    let p99_us = calculate_p99(&latencies);
    let p99_ms = p99_us as f64 / 1000.0;

    eprintln!(
        "1MB trace p95 latency: {:.2} ms, p99: {:.2} ms ({} samples)",
        p95_ms, p99_ms, ASSERTION_SAMPLE_COUNT
    );

    let mut group = c.benchmark_group("sanitize_1mb");
    group.throughput(Throughput::Bytes(SIZE_1MB as u64));
    group.bench_function("throughput_bytes", |b| {
        b.iter(|| {
            let result = sanitizer.sanitize(black_box(&content));
            black_box(result);
        });
    });
    group.finish();
}

/// Benchmarks sanitization at 1MB trace size (ops/sec).
fn bench_sanitize_1mb_ops(c: &mut Criterion) {
    let sanitizer = Sanitizer::new(&[]).expect("failed to build sanitizer");
    let content = generate_trace_content(SIZE_1MB);

    let mut group = c.benchmark_group("sanitize_1mb");
    group.throughput(Throughput::Elements(1));
    group.bench_function("throughput_ops", |b| {
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
    let p95 = calculate_p95(&latencies);
    let p99 = calculate_p99(&latencies);

    eprintln!(
        "Sanitizer latency assertion test (100KB trace, {} iterations):",
        latencies.len()
    );
    eprintln!("  Min:     {} ms", min);
    eprintln!("  Median:  {} ms", median);
    eprintln!("  Avg:     {} ms", avg);
    eprintln!("  P95:     {} ms", p95);
    eprintln!("  P99:     {} ms", p99);
    eprintln!("  Max:     {} ms", max);
    eprintln!("  Threshold: {} ms", threshold);

    assert!(
        median < threshold,
        "Sanitizer median latency ({} ms) exceeds threshold ({} ms)",
        median,
        threshold
    );
}

/// Specialized benchmark that reports median and p95 latency directly.
///
/// This complements the criterion benchmark by providing explicit
/// percentile measurements that can be compared against performance thresholds.
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
    let p95_us = calculate_p95(&latencies);
    let p95_ms = p95_us as f64 / 1000.0;
    let p99_us = calculate_p99(&latencies);
    let p99_ms = p99_us as f64 / 1000.0;

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
    eprintln!("  P95: {:.2} ms", p95_ms);
    eprintln!("  P99: {:.2} ms", p99_ms);

    // Report to criterion for plotting with proper p95 measurement configuration.
    let mut group = c.benchmark_group("latency_percentiles");
    group.bench_function("p95_100kb", |b| {
        b.iter(|| {
            let _ = sanitizer.sanitize(black_box(&content));
        });
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = configure_criterion();
    targets = bench_sanitize_10kb, bench_sanitize_10kb_ops, bench_sanitize_100kb, bench_sanitize_100kb_ops, bench_sanitize_1mb, bench_sanitize_1mb_ops, bench_median_latency, report_skip_stats
}
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

    #[test]
    fn generator_creates_all_size_variants() {
        // Test that the generator creates all 3 required size variants
        let content_10kb = super::generate_trace_content(super::SIZE_10KB);
        let content_100kb = super::generate_trace_content(super::SIZE_100KB);
        let content_1mb = super::generate_trace_content(super::SIZE_1MB);

        // Each should be exactly the target size
        assert_eq!(content_10kb.len(), SIZE_10KB);
        assert_eq!(content_100kb.len(), SIZE_100KB);
        assert_eq!(content_1mb.len(), SIZE_1MB);
    }

    #[test]
    fn generator_is_deterministic() {
        // Test that generator produces identical output for same input
        let content1 = super::generate_trace_content(super::SIZE_10KB);
        let content2 = super::generate_trace_content(super::SIZE_10KB);

        assert_eq!(content1, content2, "Generator must be deterministic");
    }

    #[test]
    fn generator_includes_realistic_patterns() {
        // Test that generated content includes expected patterns
        let content = super::generate_trace_content(super::SIZE_10KB);

        // Should include system events
        assert!(
            content.contains("\"type\":\"system\""),
            "Should include system events"
        );

        // Should include stream events
        assert!(
            content.contains("\"type\":\"stream_event\""),
            "Should include stream events"
        );

        // Should include thinking events
        assert!(
            content.contains("\"type\":\"thinking_delta\""),
            "Should include thinking deltas"
        );

        // Should include tool use events
        assert!(
            content.contains("\"type\":\"tool_use\""),
            "Should include tool use events"
        );

        // Should include tool results with code
        assert!(
            content.contains("\"type\":\"tool_result\""),
            "Should include tool results"
        );

        // Should include code snippets
        assert!(
            content.contains("pub fn"),
            "Should include Rust code patterns"
        );
        assert!(
            content.contains("cargo test"),
            "Should include shell commands"
        );

        // Should be JSONL format (one JSON per line)
        let lines: Vec<&str> = content.lines().collect();
        assert!(!lines.is_empty(), "Should produce multiple lines");

        // First non-empty line should be valid JSON
        let first_json = lines.iter().find(|l| !l.is_empty()).unwrap();
        assert!(
            first_json.starts_with('{'),
            "Lines should start with JSON object"
        );
        assert!(
            first_json.ends_with('}'),
            "Lines should end with JSON object"
        );
    }
}
