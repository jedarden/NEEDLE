use needle::stats::{calculate_p95, calculate_p99};

fn main() {
    println!("=== Verify All Three Latency Metrics ===\n");

    // Simulated benchmark latency data
    let latencies = vec![
        850, 920, 880, 950, 900,
        1100, 1050, 1150, 1080, 1120,
        1250, 1200, 1300, 1220, 1280,
        1450, 1400, 1500, 1420, 1480,
        1800, 1750, 1850, 1780, 1820,
    ]; // 25 samples total

    // Calculate all three metrics
    let median_us = {
        let mut sorted = latencies.clone();
        sorted.sort();
        sorted[latencies.len() / 2]
    };
    let p95_us = calculate_p95(&latencies);
    let p99_us = calculate_p99(&latencies);

    // Display all three metrics
    println!("Latency Metrics ({} samples):", latencies.len());
    println!("  Median: {} µs ({:.2} ms)", median_us, median_us as f64 / 1000.0);
    println!("  P95: {} µs ({:.2} ms)", p95_us, p95_us as f64 / 1000.0);
    println!("  P99: {} µs ({:.2} ms)", p99_us, p99_us as f64 / 1000.0);

    println!("\n=== Verification ===");
    println!("✓ Median latency is reported");
    println!("✓ P95 latency is reported");
    println!("✓ P99 latency is reported");
    println!("✓ All three metrics visible in output");

    // Verify expected values
    assert_eq!(median_us, 1250, "Median should be 1250 µs");
    assert_eq!(p95_us, 1816, "P95 should be 1816 µs");
    assert_eq!(p99_us, 1843, "P99 should be 1843 µs");

    println!("\n=== All Metrics Verified ===");
}
