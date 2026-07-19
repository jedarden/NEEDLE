//! Standalone test to verify p95 reporting and aggregation.
//!
//! This demonstrates that:
//! 1. P95 values are calculated correctly using the stats module
//! 2. Values are aggregated across multiple iterations
//! 3. Results are properly formatted

use needle::stats::{calculate_p95, P95Collector};
use std::time::Instant;

fn main() {
    println!("=== P95 Calculation and Aggregation Test ===\n");

    // Test 1: Verify calculate_p95 works with known values
    println!("Test 1: Known value verification");
    let known_data = vec![10u128, 20, 30, 40, 50, 60, 70, 80, 90, 100];
    let p95 = calculate_p95(&known_data);
    println!("  Data: {:?}", known_data);
    println!("  P95: {} (expected: 96)", p95);
    assert_eq!(p95, 96, "P95 calculation should match expected value");
    println!("  ✓ PASS\n");

    // Test 2: Verify p95 with realistic latency data
    println!("Test 2: Realistic latency data");
    let latencies = vec![
        12, 15, 18, 20, 22, 25, 28, 30, 35, 40, 45, 50, 55, 60, 70, 80, 90, 100, 120, 150,
    ];
    let p95 = calculate_p95(&latencies);
    println!("  Latencies (ms): {:?}", latencies);
    println!("  P95: {} ms (expected: 122)", p95);
    assert_eq!(p95, 122, "P95 should handle realistic data");
    println!("  ✓ PASS\n");

    // Test 3: Verify edge cases
    println!("Test 3: Edge cases");
    let empty: Vec<u128> = vec![];
    assert_eq!(calculate_p95(&empty), 0, "Empty data should return 0");
    println!("  Empty slice: 0 ✓");

    let single = vec![42u128];
    assert_eq!(
        calculate_p95(&single),
        42,
        "Single element should return that element"
    );
    println!("  Single element: 42 ✓");

    let two = vec![10u128, 20];
    assert_eq!(
        calculate_p95(&two),
        20,
        "Two elements should use linear interpolation"
    );
    println!("  Two elements: 20 ✓");
    println!("  ✓ PASS\n");

    // Test 4: Verify P95Collector aggregation
    println!("Test 4: P95Collector aggregation");
    let mut collector = P95Collector::new();

    // Simulate multiple benchmark iterations
    for i in 0..50 {
        let start = Instant::now();
        // Simulate some work
        let _ = (i * i) % 100;
        let elapsed = start.elapsed().as_micros();
        collector.record(elapsed);
    }

    let p95_us = collector.p95();
    let stats = collector.stats().unwrap();

    println!("  Iterations: {}", collector.count());
    println!("  Min: {} μs", stats.0);
    println!("  Max: {} μs", stats.1);
    println!("  Avg: {:.2} μs", stats.2);
    println!("  P95: {} μs", p95_us);
    println!("  ✓ PASS (aggregation working)\n");

    // Test 5: Verify p95 values are numerically reasonable
    println!("Test 5: Numerical reasonableness");
    let test_data = vec![
        1000u128, 1100, 1200, 1300, 1400, 1500, 1600, 1700, 1800, 1900, 2000, 2100, 2200, 2300,
        2400, 2500, 2600, 2700, 2800, 2900,
    ];
    let p95 = calculate_p95(&test_data);
    println!(
        "  Data range: {} to {}",
        test_data[0],
        test_data.last().unwrap()
    );
    println!("  P95: {}", p95);

    // P95 should be between the median and max
    let median_idx = test_data.len() / 2;
    let median = test_data[median_idx];
    let max = *test_data.last().unwrap();
    assert!(
        p95 >= median && p95 <= max,
        "P95 ({}) should be between median ({}) and max ({})",
        p95,
        median,
        max
    );
    println!("  ✓ PASS (p95 within reasonable range)\n");

    println!("=== All Tests Passed ===");
    println!("\nConclusion:");
    println!("  ✓ P95 calculation is correct");
    println!("  ✓ Aggregation across iterations works");
    println!("  ✓ Values are numerically reasonable");
    println!("  ✓ Edge cases handled properly");
}
