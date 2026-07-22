//! Sanitization latency assertion test.
//!
//! This test validates that the sanitization pipeline meets the 10ms threshold
//! for median latency on 100KB traces. The threshold is configurable via the
//! SANITIZE_THRESHOLD_MS environment variable (default: 10ms for release builds).
//!
//! ## Usage
//!
//! Run with:
//! ```bash
//! cargo test sanitize_latency_below_threshold -- --nocapture
//! ```
//!
//! ## Environment Variables
//!
//! - `SANITIZE_THRESHOLD_MS`: Latency threshold in milliseconds (default: 10ms for release, 500ms for debug)
//! - `SANITIZER_BENCH_SAMPLE_COUNT`: Number of iterations (default: 50)
//!
//! ## CI Configuration
//!
//! The needle-ci workflow template automatically runs this test on every push.
//! To customize the threshold for CI, set the SANITIZE_THRESHOLD_MS environment
//! variable in the workflow template at:
//!
//! ```yaml
//! # declarative-config/k8s/iad-ci/argo-workflows/needle-workflowtemplate.yml
//! env:
//!   - name: SANITIZE_THRESHOLD_MS
//!     value: "10"
//! ```

use needle::sanitize::Sanitizer;
use needle::stats::{calculate_median, calculate_p95, calculate_p99};

/// Trace size for the latency assertion test (100KB).
const SIZE_100KB: usize = 100 * 1024;

/// Default number of samples for latency measurement.
const DEFAULT_SAMPLE_COUNT: usize = 50;

/// Latency threshold for the assertion-style test (milliseconds).
/// Configurable via SANITIZE_THRESHOLD_MS environment variable.
fn latency_threshold_ms() -> u128 {
    std::env::var("SANITIZE_THRESHOLD_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or({
            // Default threshold: 10ms for release builds, 500ms for debug builds.
            if cfg!(debug_assertions) {
                500
            } else {
                10
            }
        })
}

/// Generates representative trace content in Claude JSON format.
///
/// This is a simplified version of the generator in benches/sanitize.rs,
/// optimized for the assertion test.
fn generate_trace_content(target_bytes: usize) -> String {
    // System events from real traces
    let system_events = [
        r#"{"type":"system","subtype":"init","cwd":"/home/coding/NEEDLE","session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769","tools":["Task","Bash","Read","Write","Edit"],"mcp_servers":[],"model":"glm-4.7","permissionMode":"bypassPermissions"}"#,
        r#"{"type":"system","subtype":"status","status":"requesting","uuid":"a26811cd-e0c3-411c-9ef1-ab7630de71d0","session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769"}"#,
        r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":15,"estimated_tokens_delta":2,"uuid":"6e7d97ec-d48b-43b1-8f2e-fdb80234b714","session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769"}"#,
    ];

    // Stream events
    let stream_events = [
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me break down this task"}}}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"The user wants me to implement a feature."}}}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"I need to read the source files first."}}}"#,
    ];

    // Tool use and result events
    let tool_events = [
        r#"{"type":"tool_use","id":"toolu_01","name":"read","input":{"file_path":"/home/coding/NEEDLE/src/lib.rs"},"session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769"}"#,
        r#"{"type":"tool_result","id":"toolu_01","output":"pub mod sanitize;\npub mod telemetry;\npub mod config;\n\nuse anyhow::Result;\n","session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769"}"#,
        r#"{"type":"tool_use","id":"toolu_02","name":"bash","input":{"command":"cargo test --lib sanitize"},"session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769"}"#,
        r#"{"type":"tool_result","id":"toolu_02","output":"running 3 tests\ntest sanitize::tests::test_basic ... ok\nresult: ok. 3 passed; 0 failed","session_id":"dda3d9a1-e07a-45f0-9c18-09ee9e00d769"}"#,
    ];

    // Combine all events
    let all_events: Vec<&str> = [&system_events[..], &stream_events[..], &tool_events[..]].concat();

    // Calculate approximate bytes per event
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

/// Measures and returns median latency for sanitizing a 100KB trace.
///
/// # Returns
///
/// A tuple of (latencies in microseconds, median latency in microseconds)
fn measure_median_latency_100kb() -> (Vec<u128>, u128) {
    let sanitizer = Sanitizer::new(&[]).expect("failed to build sanitizer");
    let content = generate_trace_content(SIZE_100KB);
    let sample_count = std::env::var("SANITIZER_BENCH_SAMPLE_COUNT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_SAMPLE_COUNT);

    let mut latencies = Vec::with_capacity(sample_count);

    // Warm-up
    for _ in 0..5 {
        let _ = sanitizer.sanitize(&content);
    }

    for _ in 0..sample_count {
        let start = std::time::Instant::now();
        let _ = sanitizer.sanitize(&content);
        let elapsed_us = start.elapsed().as_micros();
        latencies.push(elapsed_us);
    }

    latencies.sort();
    let median = calculate_median(&latencies);

    (latencies, median)
}

/// Assertion test that fails if median latency exceeds threshold.
///
/// # Success Criterion
///
/// Phase 4 success criterion: sanitization must complete in <10ms per 100KB trace
/// on a single core, with Aho-Corasick pre-filter demonstrably skipping irrelevant
/// rules.
#[test]
fn sanitize_latency_below_threshold() {
    let (latencies, median_us) = measure_median_latency_100kb();
    let threshold_ms = latency_threshold_ms();
    let threshold_us = threshold_ms * 1000;

    // Calculate statistics
    let min_us = *latencies.first().unwrap();
    let max_us = *latencies.last().unwrap();
    let avg_us = latencies.iter().sum::<u128>() / latencies.len() as u128;
    let p95_us = calculate_p95(&latencies);
    let p99_us = calculate_p99(&latencies);

    // Convert to milliseconds for display
    let median_ms = median_us as f64 / 1000.0;
    let avg_ms = avg_us as f64 / 1000.0;
    let p95_ms = p95_us as f64 / 1000.0;
    let p99_ms = p99_us as f64 / 1000.0;
    let min_ms = min_us as f64 / 1000.0;
    let max_ms = max_us as f64 / 1000.0;

    eprintln!(
        "Sanitizer latency assertion test (100KB trace, {} iterations):",
        latencies.len()
    );
    eprintln!("  Min:     {:.2} ms", min_ms);
    eprintln!("  Median:  {:.2} ms", median_ms);
    eprintln!("  Avg:     {:.2} ms", avg_ms);
    eprintln!("  P95:     {:.2} ms", p95_ms);
    eprintln!("  P99:     {:.2} ms", p99_ms);
    eprintln!("  Max:     {:.2} ms", max_ms);
    eprintln!("  Threshold: {} ms", threshold_ms);

    assert!(
        median_us < threshold_us,
        "Sanitizer median latency ({:.2} ms) exceeds threshold ({} ms)",
        median_ms,
        threshold_ms
    );
}

/// Test that the trace generator produces the correct size.
#[test]
fn generator_produces_correct_size() {
    let content = generate_trace_content(SIZE_100KB);
    assert_eq!(content.len(), SIZE_100KB);
}

/// Test that the trace generator is deterministic.
#[test]
fn generator_is_deterministic() {
    let content1 = generate_trace_content(SIZE_100KB);
    let content2 = generate_trace_content(SIZE_100KB);
    assert_eq!(content1, content2, "Generator must be deterministic");
}

/// Test that the environment variable parsing works correctly.
#[test]
fn latency_threshold_parsing() {
    // Test default value (no env var set)
    std::env::remove_var("SANITIZE_THRESHOLD_MS");
    let default = latency_threshold_ms();
    if cfg!(debug_assertions) {
        assert_eq!(default, 500);
    } else {
        assert_eq!(default, 10);
    }

    // Test custom value
    std::env::set_var("SANITIZE_THRESHOLD_MS", "25");
    let custom = latency_threshold_ms();
    assert_eq!(custom, 25);

    // Test invalid value (falls back to default)
    std::env::set_var("SANITIZE_THRESHOLD_MS", "invalid");
    let fallback = latency_threshold_ms();
    if cfg!(debug_assertions) {
        assert_eq!(fallback, 500);
    } else {
        assert_eq!(fallback, 10);
    }

    // Clean up
    std::env::remove_var("SANITIZE_THRESHOLD_MS");
}
