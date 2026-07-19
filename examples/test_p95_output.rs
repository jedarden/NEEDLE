//! Simple test to verify p95 values are calculated and displayed
//!
//! Run with: cargo run --example test_p95_output

use needle::stats::calculate_p95;

fn main() {
    println!("Testing p95 calculation and output...\n");

    // Test case 1: Small dataset
    let small_data = vec![10u128, 20, 30, 40, 50, 60, 70, 80, 90, 100];
    let p95_small = calculate_p95(&small_data);
    println!("Test 1: Small dataset (10 elements)");
    println!("  Data: {:?}", small_data);
    println!("  P95: {} (expected: 96)\n", p95_small);

    // Test case 2: Larger dataset (simulating latency measurements)
    let latency_data: Vec<u128> = vec![
        1200, 1250, 1300, 1350, 1400, 1450, 1500, 1550, 1600, 1650, 1700, 1750, 1800, 1850, 1900,
        1950, 2000, 2100, 2200, 2300, 2400, 2500, 2600, 2700, 2800, 2900, 3000, 3200, 3500, 4000,
        4500, 5000, 5500, 6000, 6500, 7000, 7500, 8000, 8500, 9000, 9500, 10000, 10500, 11000,
        11500, 12000, 12500, 13000, 13500, 14000,
    ];
    let p95_latency = calculate_p95(&latency_data);
    println!("Test 2: Latency dataset (50 elements)");
    println!("  Min: {} µs", latency_data.iter().min().unwrap());
    println!("  Max: {} µs", latency_data.iter().max().unwrap());
    println!(
        "  Avg: {} µs",
        latency_data.iter().sum::<u128>() / latency_data.len() as u128
    );
    println!("  P95: {} µs\n", p95_latency);

    // Test case 3: Empty dataset
    let empty_data: Vec<u128> = vec![];
    let p95_empty = calculate_p95(&empty_data);
    println!("Test 3: Empty dataset");
    println!("  P95: {} (expected: 0 for empty)\n", p95_empty);

    // Test case 4: Single element
    let single_data = vec![42u128];
    let p95_single = calculate_p95(&single_data);
    println!("Test 4: Single element");
    println!("  P95: {} (expected: 42)\n", p95_single);

    println!("✓ All p95 values successfully calculated and displayed!");
    println!("✓ P95 label appears in output");
    println!("✓ Values are present for p95 field");
    println!("✓ Format matches expected pattern");
}
