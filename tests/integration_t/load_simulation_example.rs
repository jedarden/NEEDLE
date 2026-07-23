//! Example integration test demonstrating LoadSimulator usage.
//!
//! This test shows how to use the LoadSimulator to test worker behavior
//! under different load conditions.

use std::time::Duration;

use needle::integration_t::{burst_load_setup, saturated_load_setup, rising_load_setup};

#[tokio::test]
async fn example_saturated_load_test() {
    // Set up a saturated load scenario (single worker)
    let (simulator, _temp_dir) = saturated_load_setup().await.unwrap();

    // Verify the simulator is configured for saturation
    assert_eq!(simulator.worker_capacity(), 1);

    // Record spawn attempts and calculate delays
    simulator.record_spawn_attempt();
    std::thread::sleep(Duration::from_millis(100));
    simulator.record_spawn_attempt();

    let delays = simulator.inter_launch_delays();
    assert_eq!(delays.len(), 1);
    assert!(delays[0] >= Duration::from_millis(95));
}

#[tokio::test]
async fn example_rising_load_test() {
    // Simulate scale-up from 1 to 4 workers
    let (simulator, _temp_dir) = rising_load_setup(
        Some(1),                    // initial capacity
        Some(4),                    // final capacity
        Some(3),                    // number of steps
        Some(Duration::from_millis(100))  // fast delay for test
    ).await.unwrap();

    // Verify spawn attempts were recorded
    assert_eq!(simulator.spawn_attempt_count(), 3);

    // Verify final capacity
    assert_eq!(simulator.worker_capacity(), 4);
}

#[tokio::test]
async fn example_burst_load_test() {
    // Create burst load with 4 workers and 20 beads
    let (simulator, store, _temp_dir) = burst_load_setup(Some(4), Some(20)).await.unwrap();

    // Verify capacity
    assert_eq!(simulator.worker_capacity(), 4);

    // Verify beads are available
    use needle::bead_store::Filters;
    let ready = store.ready(&Filters::default()).await.unwrap();
    assert_eq!(ready.len(), 20);
}

#[tokio::test]
async fn example_custom_load_scenario() {
    use tempfile::TempDir;
    use needle::integration_t::LoadSimulator;

    // Create a custom load simulator
    let temp_dir = TempDir::new().unwrap();
    let mut simulator = LoadSimulator::new(2, temp_dir).unwrap();

    // Record some spawn attempts
    for i in 0..5 {
        simulator.record_spawn_attempt();
        if i < 4 {
            std::thread::sleep(Duration::from_millis(50 * (i + 1)));
        }
    }

    // Calculate statistics
    let avg_delay = simulator.average_inter_launch_delay().unwrap();
    let min_delay = simulator.min_inter_launch_delay().unwrap();
    let max_delay = simulator.max_inter_launch_delay().unwrap();

    // Verify statistics make sense
    assert!(min_delay <= avg_delay);
    assert!(max_delay >= avg_delay);

    println!("Average inter-launch delay: {:?}", avg_delay);
    println!("Min inter-launch delay: {:?}", min_delay);
    println!("Max inter-launch delay: {:?}", max_delay);
}
