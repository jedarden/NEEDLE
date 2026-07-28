//! Integration tests for p95 aggregation across benchmark iterations.
//!
//! Verifies that P95Collector correctly aggregates latency samples
//! across multiple iterations and produces statistically valid results.

use needle::stats::{calculate_p95, P95Collector};
use std::time::Instant;

#[test]
fn test_p95_collector_aggregates_across_iterations() {
    // Test that P95Collector correctly aggregates samples from multiple iterations
    let mut collector = P95Collector::new();

    // Simulate 3 separate benchmark runs with different latency profiles
    let run1: Vec<u128> = vec![100, 110, 120, 130, 140];
    let run2: Vec<u128> = vec![105, 115, 125, 135, 145];
    let run3: Vec<u128> = vec![95, 108, 118, 128, 138];

    // Record all samples from all runs
    collector.record_all(run1.iter().copied());
    collector.record_all(run2.iter().copied());
    collector.record_all(run3.iter().copied());

    // Calculate p95 on aggregated data
    let aggregated_p95 = collector.p95();

    // Pool all samples manually and calculate expected p95
    let mut pooled: Vec<u128> = Vec::new();
    pooled.extend(&run1);
    pooled.extend(&run2);
    pooled.extend(&run3);
    let expected_p95 = calculate_p95(&pooled);

    assert_eq!(
        aggregated_p95, expected_p95,
        "P95Collector should produce same result as manual pooling"
    );

    // Verify we have all samples
    assert_eq!(collector.count(), 15, "Should have 15 total samples");
}

#[test]
fn test_p95_collector_with_realistic_benchmark_pattern() {
    // Test with a realistic benchmark pattern: warm-up iterations followed by measured runs
    let mut collector = P95Collector::with_capacity(100);

    // Simulate warm-up (these would normally be discarded, but we'll include them)
    for _ in 0..5 {
        let _ = Instant::now();
        let _ = std::hint::black_box(42);
    }

    // Simulate 50 measured iterations with varying latencies
    for i in 0..50 {
        let start = Instant::now();
        // Simulate work with variable duration
        let work = i % 10;
        let _ = std::hint::black_box(work * work);
        let elapsed = start.elapsed().as_micros();
        collector.record(elapsed);
    }

    // Verify we collected all samples
    assert_eq!(collector.count(), 50, "Should have 50 samples");

    // P95 should be between median and max
    let samples = collector.samples();
    let mut sorted_samples = samples.to_vec();
    sorted_samples.sort();
    let median_idx = sorted_samples.len() / 2;
    let median = sorted_samples[median_idx];
    let max = *sorted_samples.last().unwrap();
    let p95 = collector.p95();

    assert!(
        p95 >= median && p95 <= max,
        "P95 ({}) should be between median ({}) and max ({})",
        p95, median, max
    );

    // Verify stats are consistent
    let stats = collector.stats();
    assert!(stats.is_some(), "Stats should be available");
    let (min, max, avg) = stats.unwrap();
    assert!(min <= avg as u128 && avg as u128 <= max, "Min ≤ avg ≤ max");
}

#[test]
fn test_p95_collector_preserves_statistical_validity() {
    // Test that aggregating via P95Collector produces the same result
    // as pooling all samples and calculating p95 once
    let mut collector = P95Collector::new();

    // Create data with known distribution
    let data: Vec<u128> = (1..=100).map(|i| i * 10).collect();

    // Record all samples
    collector.record_all(data.iter().copied());

    // Calculate p95 via collector
    let collector_p95 = collector.p95();

    // Calculate p95 directly
    let direct_p95 = calculate_p95(&data);

    assert_eq!(
        collector_p95, direct_p95,
        "P95Collector should preserve statistical validity"
    );
}

#[test]
fn test_p95_collector_with_outliers() {
    // Test that P95Collector handles outliers correctly
    let mut collector = P95Collector::new();

    // Normal data plus some extreme outliers
    let normal: Vec<u128> = vec![100, 105, 110, 115, 120, 125, 130, 135, 140];
    let outliers: Vec<u128> = vec![1000, 2000];

    collector.record_all(normal.iter().copied());
    collector.record_all(outliers.iter().copied());

    let p95 = collector.p95();

    // P95 should be influenced by outliers but not dominated by them
    // With 11 samples, rank = 0.95 * 10 = 9.5, so p95 is between sorted[9] and sorted[10]
    // Sorted: [100, 105, 110, 115, 120, 125, 130, 135, 140, 1000, 2000]
    // Linear interpolation: 140 + (1000 - 140) * 0.5 = 570
    assert!(p95 > 140, "P95 should be above normal range");
    assert!(p95 < 2000, "P95 should not be max outlier");
}

#[test]
fn test_p95_collector_clear_and_reuse() {
    // Test that collector can be cleared and reused for multiple benchmark runs
    let mut collector = P95Collector::new();

    // First benchmark run
    collector.record_all(vec![10, 20, 30].iter().copied());
    let p95_run1 = collector.p95();
    assert_eq!(collector.count(), 3);

    // Clear for second run
    collector.clear();
    assert_eq!(collector.count(), 0);
    assert_eq!(collector.p95(), 0);

    // Second benchmark run
    collector.record_all(vec![100, 200, 300].iter().copied());
    let p95_run2 = collector.p95();
    assert_eq!(collector.count(), 3);

    // Results should be different
    assert_ne!(p95_run1, p95_run2, "Different runs should produce different p95");
}

#[test]
fn test_p95_collector_large_dataset() {
    // Test with a large dataset to verify scalability
    let mut collector = P95Collector::with_capacity(10000);

    // Generate 10000 samples
    let samples: Vec<u128> = (1..=10000).map(|i| i).collect();
    collector.record_all(samples.iter().copied());

    assert_eq!(collector.count(), 10000);

    // P95 of 1..10000 should be around 9500
    let p95 = collector.p95();
    assert!(p95 > 9400 && p95 < 9600, "P95 should be around 9500");
}
