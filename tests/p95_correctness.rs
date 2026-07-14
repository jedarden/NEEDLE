//! Comprehensive tests for p95 calculation algorithm correctness.
//!
//! Tests against known values and edge cases to verify the nearest-rank
//! method implementation is correct.

use needle::stats::calculate_p95;

#[test]
fn test_p95_known_values() {
    // Test case 1: Simple ascending sequence
    let data = vec![10u128, 20, 30, 40, 50, 60, 70, 80, 90, 100];
    // Linear interpolation: rank = 0.95 * 9 = 8.55, floor=8, frac=0.55
    // 90 + (100-90) * 0.55 = 95.5 → 96
    assert_eq!(calculate_p95(&data), 96);

    // Test case 2: Larger dataset
    let data: Vec<u128> = (1..=100).collect();
    // Linear interpolation: rank = 0.95 * 99 = 94.05, floor=94, frac=0.05
    // 95 + (96-95) * 0.05 = 95.05 → 95
    assert_eq!(calculate_p95(&data), 95);

    // Test case 3: Dataset where p95 falls in middle
    let data: Vec<u128> = (1..=20).collect();
    // Linear interpolation: rank = 0.95 * 19 = 18.05, floor=18, frac=0.05
    // 19 + (20-19) * 0.05 = 19.05 → 19
    assert_eq!(calculate_p95(&data), 19);
}

#[test]
fn test_p95_edge_cases() {
    // Empty slice
    let empty: Vec<u128> = vec![];
    assert_eq!(calculate_p95(&empty), 0);

    // Single element
    let single = vec![42u128];
    assert_eq!(calculate_p95(&single), 42);

    // Two elements
    let two = vec![10u128, 20];
    // Linear interpolation: rank = 0.95 * 1 = 0.95, floor=0, frac=0.95
    // 10 + (20-10) * 0.95 = 19.5 → 20
    assert_eq!(calculate_p95(&two), 20);

    // Three elements
    let three = vec![10u128, 20, 30];
    // Linear interpolation: rank = 0.95 * 2 = 1.9, floor=1, frac=0.9
    // 20 + (30-20) * 0.9 = 29.0 → 29
    assert_eq!(calculate_p95(&three), 29);
}

#[test]
fn test_p95_duplicate_values() {
    // All same values
    let same = vec![50u128; 20];
    assert_eq!(calculate_p95(&same), 50);

    // Many duplicates
    let data = vec![10u128, 10, 10, 20, 20, 30, 30, 40, 50, 50];
    // Linear interpolation: rank = 0.95 * 9 = 8.55, floor=8, frac=0.55
    // 50 + (50-50) * 0.55 = 50
    assert_eq!(calculate_p95(&data), 50);
}

#[test]
fn test_p95_unsorted_input() {
    // Random order, should produce same result as sorted
    let unsorted = vec![100u128, 50, 10, 90, 30, 70, 40, 60, 20, 80];
    let sorted = vec![10u128, 20, 30, 40, 50, 60, 70, 80, 90, 100];
    assert_eq!(calculate_p95(&unsorted), calculate_p95(&sorted));
    assert_eq!(calculate_p95(&unsorted), 96);
}

#[test]
fn test_p95_large_dataset() {
    // Large dataset to verify algorithm scales
    let data: Vec<u128> = (1..=1000).collect();
    // Linear interpolation: rank = 0.95 * 999 = 949.05, floor=949, frac=0.05
    // 950 + (951-950) * 0.05 = 950.05 → 950
    assert_eq!(calculate_p95(&data), 950);
}

#[test]
fn test_p95_realistic_latency_data() {
    // Simulated latency data with realistic distribution
    let latencies = vec![
        12, 15, 18, 20, 22, 25, 28, 30, 35, 40, 45, 50, 55, 60, 70, 80, 90, 100, 120, 150,
    ];
    // Linear interpolation: rank = 0.95 * 19 = 18.05, floor=18, frac=0.05
    // 120 + (150-120) * 0.05 = 121.5 → 122
    assert_eq!(calculate_p95(&latencies), 122);
}

#[test]
fn test_p95_with_outliers() {
    // Data with outliers
    let data = vec![
        10u128, 12, 15, 18, 20, 22, 25, 28, 30, 35, 40, 45, 50, 55, 60, 70, 80, 90, 1000, 2000,
    ];
    // Linear interpolation: rank = 0.95 * 19 = 18.05, floor=18, frac=0.05
    // 1000 + (2000-1000) * 0.05 = 1050
    assert_eq!(calculate_p95(&data), 1050);
}
