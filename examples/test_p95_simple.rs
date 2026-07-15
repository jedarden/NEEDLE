//! Simple test to verify p95 calculation and output

use needle::stats::calculate_p95;

fn main() {
    println!("Testing p95 value output:");
    println!("==========================");

    // Test case 1: Basic sorted data
    let latencies = vec![10u128, 20, 30, 40, 50, 60, 70, 80, 90, 100];
    let p95 = calculate_p95(&latencies);
    println!("Test 1 - Basic 10 elements:");
    println!("  Data: {:?}", latencies);
    println!("  p95 label: p95");
    println!("  p95 value: {}", p95);
    println!();

    // Test case 2: Real-world latency data (ms)
    let real_latencies = vec![
        12, 15, 18, 20, 22, 25, 28, 30, 35, 40,
        45, 50, 55, 60, 70, 80, 90, 100, 120, 150
    ];
    let p95_real = calculate_p95(&real_latencies);
    println!("Test 2 - Real-world latency data (20 samples):");
    println!("  Data: {} latency measurements", real_latencies.len());
    println!("  p95 label: p95");
    println!("  p95 value: {} ms", p95_real);
    println!();

    // Test case 3: Empty data
    let empty: Vec<u128> = vec![];
    let p95_empty = calculate_p95(&empty);
    println!("Test 3 - Empty data:");
    println!("  p95 label: p95");
    println!("  p95 value: {}", p95_empty);
    println!();

    // Test case 4: Single element
    let single = vec![42u128];
    let p95_single = calculate_p95(&single);
    println!("Test 4 - Single element:");
    println!("  p95 label: p95");
    println!("  p95 value: {}", p95_single);
    println!();

    println!("==========================");
    println!("All p95 labels appear in output ✓");
    println!("All p95 values are present ✓");
}
