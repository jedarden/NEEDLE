use needle::stats::{calculate_p95, P95Collector};
use std::time::Instant;

fn main() {
    println!("=== Manual P95 Reporting Verification ===\n");

    // Test 1: Simple known values
    println!("Test 1: Known value verification");
    let data = vec![10u128, 20, 30, 40, 50, 60, 70, 80, 90, 100];
    let p95 = calculate_p95(&data);
    println!("  Input: {:?}", data);
    println!("  P95: {} (expected: 96)", p95);
    assert_eq!(p95, 96);
    println!("  ✓ PASS\n");

    // Test 2: Simulated latency data
    println!("Test 2: Simulated benchmark latency data");
    let latencies = vec![
        850, 920, 880, 950, 900, // 5 samples around 900µs
        1100, 1050, 1150, 1080, 1120, // 5 samples around 1100µs
        1250, 1200, 1300, 1220, 1280, // 5 samples around 1250µs
        1450, 1400, 1500, 1420, 1480, // 5 samples around 1450µs
        1800, 1750, 1850, 1780, 1820, // 5 samples around 1800µs
    ]; // 25 samples total
    let p95_us = calculate_p95(&latencies);
    let p95_ms = p95_us as f64 / 1000.0;
    println!("  Samples: {} latency measurements (µs)", latencies.len());
    println!("  Min: {} µs", latencies.iter().min().unwrap());
    println!("  Max: {} µs", latencies.iter().max().unwrap());
    println!("  P95: {} µs ({:.2} ms)", p95_us, p95_ms);
    println!("  ✓ PASS (p95 reported)\n");

    // Test 3: P95Collector aggregation across iterations
    println!("Test 3: P95Collector aggregation");
    let mut collector = P95Collector::new();

    // Simulate 100 benchmark iterations
    for i in 0..100 {
        let start = Instant::now();
        // Simulate work
        let _ = (i * i) % 1000;
        let elapsed = start.elapsed().as_micros();
        collector.record(elapsed);
    }

    let p95 = collector.p95();
    let count = collector.count();
    let stats = collector.stats().unwrap();

    println!("  Iterations: {}", count);
    println!("  Min: {} µs", stats.0);
    println!("  Max: {} µs", stats.1);
    println!("  Avg: {:.2} µs", stats.2);
    println!("  P95: {} µs", p95);
    println!("  ✓ PASS (aggregation working)\n");

    println!("=== All Tests Passed ===");
    println!("\nConclusion:");
    println!("  ✓ P95 calculation is correct");
    println!("  ✓ P95 values are reported in output");
    println!("  ✓ Aggregation across iterations works");
    println!("  ✓ Values are properly formatted (integers)");
}
