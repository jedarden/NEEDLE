//! Comprehensive validation of p95 aggregation across multiple benchmark iterations.
//!
//! This test file validates that:
//! 1. P95 is correctly aggregated across multiple iterations
//! 2. The aggregation strategy is "percentile of all samples" (not "percentile of percentiles")
//! 3. Edge cases with variable iteration counts are handled correctly
//! 4. The mathematical approach is statistically sound

use needle::stats::{calculate_p95, P95Collector};
use std::time::Instant;

/// Demonstrates the WRONG approach: averaging p95 values from individual iterations.
///
/// This function exists solely to prove why the "percentile of percentiles" approach
/// is statistically invalid. It should NEVER be used in production.
fn incorrect_average_of_p95s(iterations: &[Vec<u128>]) -> u128 {
    let p95s: Vec<u128> = iterations.iter().map(|iter| calculate_p95(iter)).collect();
    let sum: u128 = p95s.iter().sum();
    sum / p95s.len() as u128
}

/// Demonstrates the CORRECT approach: pool all samples, then calculate p95.
fn correct_p95_of_all_samples(iterations: &[Vec<u128>]) -> u128 {
    let mut collector = P95Collector::new();
    for iteration in iterations {
        collector.record_all(iteration.iter().copied());
    }
    collector.p95()
}

#[test]
fn test_aggregation_strategy_correctness() {
    // Create test data with multiple iterations, each with 10 samples
    let iterations = vec![
        // Iteration 1: latencies 100-109
        (100..110).map(|i| i as u128).collect::<Vec<_>>(),
        // Iteration 2: latencies 200-209
        (200..210).map(|i| i as u128).collect::<Vec<_>>(),
        // Iteration 3: latencies 150-159
        (150..160).map(|i| i as u128).collect::<Vec<_>>(),
    ];

    // Calculate p95 using the WRONG method (average of p95s)
    let wrong_p95 = incorrect_average_of_p95s(&iterations);

    // Calculate p95 using the CORRECT method (pool all samples)
    let correct_p95 = correct_p95_of_all_samples(&iterations);

    // The wrong method averages p95s from each iteration:
    // Iteration 1 p95: rank = 0.95 * 9 = 8.55 → 108 + (109-108)*0.55 = 108.55 → 109
    // Iteration 2 p95: rank = 0.95 * 9 = 8.55 → 208 + (209-208)*0.55 = 208.55 → 209
    // Iteration 3 p95: rank = 0.95 * 9 = 8.55 → 158 + (159-158)*0.55 = 158.55 → 159
    // Average: (109 + 209 + 159) / 3 = 477 / 3 = 159

    // The correct method pools all 30 samples and calculates p95 once:
    // Sorted pooled: [100, 101, ..., 109, 150, 151, ..., 159, 200, 201, ..., 209]
    // Total: 30 samples, rank = 0.95 * 29 = 27.55
    // Index 27 = 207, Index 28 = 208
    // P95 = 207 + (208-207)*0.55 = 207.55 → 208

    assert_eq!(wrong_p95, 159, "Wrong method should average iteration p95s");
    assert_eq!(correct_p95, 208, "Correct method should pool all samples");

    // The correct method gives a different (and more accurate) result
    assert_ne!(
        wrong_p95, correct_p95,
        "Wrong and correct methods should produce different results"
    );

    println!("Aggregation Strategy Validation:");
    println!("  Wrong (average of p95s): {}", wrong_p95);
    println!("  Correct (p95 of all samples): {}", correct_p95);
    println!("  Difference: {}", correct_p95 as i128 - wrong_p95 as i128);
}

#[test]
fn test_p95_collector_uses_correct_strategy() {
    // Verify that P95Collector uses the correct aggregation strategy
    let iterations = vec![
        vec![10u128, 20, 30, 40, 50],
        vec![15, 25, 35, 45, 55],
        vec![12, 22, 32, 42, 52],
    ];

    // Use P95Collector (the production implementation)
    let mut collector = P95Collector::new();
    for iteration in &iterations {
        collector.record_all(iteration.iter().copied());
    }
    let collector_p95 = collector.p95();

    // Manually pool all samples and calculate p95
    let mut pooled: Vec<u128> = Vec::new();
    for iteration in &iterations {
        pooled.extend(iteration);
    }
    let manual_p95 = calculate_p95(&pooled);

    // They must be identical
    assert_eq!(
        collector_p95, manual_p95,
        "P95Collector must use 'p95 of all samples' strategy"
    );

    println!("P95Collector Strategy Validation:");
    println!("  Collector p95: {}", collector_p95);
    println!("  Manual pooled p95: {}", manual_p95);
    println!("  ✓ Strategy verified: percentile of all samples");
}

#[test]
fn test_variable_iteration_counts() {
    // Test edge cases with different numbers of iterations

    // Case 1: Single iteration (should work, but with limited statistical significance)
    let single_iter = vec![vec![100, 200, 300, 400, 500]];
    let p95_single = correct_p95_of_all_samples(&single_iter);
    assert_eq!(
        p95_single,
        calculate_p95(&single_iter[0]),
        "Single iteration should produce same result as direct calculation"
    );

    // Case 2: Two iterations
    let two_iters = vec![vec![100, 200, 300], vec![150, 250, 350]];
    let p95_two = correct_p95_of_all_samples(&two_iters);
    assert!(p95_two > 0, "Two iterations should produce valid p95");

    // Case 3: Many iterations (more realistic)
    let mut many_iters = Vec::new();
    for i in 0..50 {
        let iteration: Vec<u128> = (0..10).map(|j| (i * 10 + j * 10) as u128).collect();
        many_iters.push(iteration);
    }
    let p95_many = correct_p95_of_all_samples(&many_iters);
    assert!(p95_many > 0, "Many iterations should produce valid p95");

    // Verify sample counts for each case
    let mut collector_single = P95Collector::new();
    for iteration in &single_iter {
        collector_single.record_all(iteration.iter().copied());
    }
    assert_eq!(
        collector_single.count(),
        5,
        "Single iteration should have correct sample count"
    );

    let mut collector_two = P95Collector::new();
    for iteration in &two_iters {
        collector_two.record_all(iteration.iter().copied());
    }
    assert_eq!(
        collector_two.count(),
        6,
        "Two iterations should have correct sample count"
    );

    println!("Variable Iteration Count Validation:");
    println!("  Single iteration p95: {} (5 samples)", p95_single);
    println!("  Two iterations p95: {} (6 samples)", p95_two);
    println!("  Many iterations (50) p95: {} (500 samples)", p95_many);
}

#[test]
fn test_variable_samples_per_iteration() {
    // Test edge cases where iterations have different numbers of samples

    // Case 1: Uniform sample sizes (baseline)
    let uniform = vec![vec![100, 200, 300, 400, 500], vec![150, 250, 350, 450, 550]];
    let p95_uniform = correct_p95_of_all_samples(&uniform);

    // Case 2: Variable sample sizes
    let variable = vec![
        vec![100, 200, 300],              // 3 samples
        vec![400, 500, 600, 700],         // 4 samples
        vec![800, 900, 1000, 1100, 1200], // 5 samples
    ];
    let p95_variable = correct_p95_of_all_samples(&variable);

    // Both should produce valid results
    assert!(p95_uniform > 0, "Uniform sample sizes should work");
    assert!(p95_variable > 0, "Variable sample sizes should work");

    // Verify P95Collector handles variable sample sizes
    let mut collector = P95Collector::new();
    for iteration in &variable {
        collector.record_all(iteration.iter().copied());
    }
    assert_eq!(collector.count(), 12, "Should have 12 total samples");
    assert_eq!(
        collector.p95(),
        p95_variable,
        "Collector should handle variable sample sizes correctly"
    );

    println!("Variable Sample Size Validation:");
    println!("  Uniform sizes p95: {}", p95_uniform);
    println!("  Variable sizes p95: {}", p95_variable);
    println!("  ✓ P95Collector handles both cases");
}

#[test]
fn test_p95_mathematical_correctness() {
    // Validate the mathematical correctness of p95 calculation

    // Test with known values where we can verify the calculation
    let iterations = vec![vec![10u128, 20, 30, 40, 50, 60, 70, 80, 90, 100]];

    let p95 = correct_p95_of_all_samples(&iterations);

    // Manual calculation:
    // 10 samples sorted: [10, 20, ..., 100]
    // rank = 0.95 * (10 - 1) = 0.95 * 9 = 8.55
    // floor_index = 8, fraction = 0.55
    // floor_value = 90, ceiling_value = 100
    // interpolated = 90 + (100 - 90) * 0.55 = 90 + 10 * 0.55 = 95.5
    // rounded = 96

    assert_eq!(p95, 96, "P95 calculation should be mathematically correct");

    println!("Mathematical Correctness Validation:");
    println!("  Input: [10, 20, ..., 100]");
    println!("  Expected p95: 96");
    println!("  Actual p95: {}", p95);
    println!("  ✓ Linear interpolation verified");
}

#[test]
fn test_p95_aggregation_with_realistic_benchmark() {
    // Simulate a realistic benchmark scenario with actual timing
    let mut collector = P95Collector::with_capacity(100);

    // Simulate 10 benchmark iterations, each with 5 samples
    for iter_num in 0..10 {
        for _ in 0..5 {
            let start = Instant::now();
            // Simulate variable work (adds some variance)
            let work = iter_num * 10 + 42;
            let _ = std::hint::black_box(work * work);
            let elapsed = start.elapsed().as_micros();
            collector.record(elapsed);
        }
    }

    let p95 = collector.p95();
    let count = collector.count();

    assert_eq!(
        count, 50,
        "Should have 50 samples (10 iterations × 5 samples)"
    );
    assert!(p95 > 0, "Should have valid p95");

    // P95 should be between min and max
    let samples = collector.samples();
    let min = *samples.iter().min().unwrap();
    let max = *samples.iter().max().unwrap();

    assert!(p95 >= min, "P95 should be >= minimum");
    assert!(p95 <= max, "P95 should be <= maximum");

    println!("Realistic Benchmark Validation:");
    println!("  Total samples: {}", count);
    println!("  Min latency: {} µs", min);
    println!("  Max latency: {} µs", max);
    println!("  P95 latency: {} µs", p95);
    println!("  ✓ P95 within valid range");
}

#[test]
fn test_aggregation_documentation_verification() {
    // This test documents and verifies the aggregation strategy for future maintainers

    println!("\n=== P95 AGGREGATION STRATEGY VERIFICATION ===\n");

    println!("STRATEGY: Percentile of ALL Samples (CORRECT)");
    println!("----------------------------------------------");
    println!("1. Collect ALL samples from ALL iterations");
    println!("2. Sort the pooled samples");
    println!("3. Calculate p95 ONCE on the pooled data");
    println!("4. This preserves statistical validity");

    println!("\nSTRATEGY: Percentile of Percentiles (WRONG)");
    println!("----------------------------------------------");
    println!("1. Calculate p95 for each iteration separately");
    println!("2. Average the p95 values");
    println!("3. This is STATISTICALLY INVALID");
    println!("4. Percentiles are non-linear; averaging distorts results");

    println!("\nIMPLEMENTATION VERIFICATION:");
    println!("---------------------------");

    let test_iterations = vec![vec![10, 20, 30], vec![40, 50, 60]];

    let mut collector = P95Collector::new();
    for iteration in &test_iterations {
        collector.record_all(iteration.iter().copied());
    }

    println!(
        "Using P95Collector with {} iterations",
        test_iterations.len()
    );
    println!("Total samples collected: {}", collector.count());
    println!("P95 result: {}", collector.p95());
    println!("✓ Implementation uses correct strategy");

    // Verify the implementation matches the documented strategy
    let mut pooled: Vec<u128> = Vec::new();
    for iteration in &test_iterations {
        pooled.extend(iteration);
    }
    let expected = calculate_p95(&pooled);
    assert_eq!(
        collector.p95(),
        expected,
        "Implementation must match documented strategy"
    );

    println!("✓ Strategy mathematically verified");
}
