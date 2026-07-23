//! Load simulation infrastructure for NEEDLE regression tests.
//!
//! This module provides utilities for simulating different load scenarios
//! to test worker behavior under various capacity conditions.
//!
//! # Load Simulation Scenarios
//!
//! - **Saturated Load**: Fixed capacity of 1 worker (no parallelism)
//! - **Rising Load**: Capacity increases over time (simulating scale-up)
//! - **Burst Load**: Sudden spikes in demand with fixed capacity
//!
//! # Example
//!
//! ```ignore
//! use needle::integration_t::{LoadSimulator, saturated_load_setup};
//!
//! // Create a saturated load scenario (single worker)
//! let simulator = saturated_load_setup(temp_dir.path()).await?;
//! simulator.run().await?;
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::Utc;
use tempfile::TempDir;

use needle::bead_store::{BeadStore, Filters};
use needle::config::{Config, WorkerConfig};
use needle::registry::Registry;
use needle::telemetry::Telemetry;
use needle::types::{Bead, BeadId, BeadStatus};
use needle::worker::Worker;

// ──────────────────────────────────────────────────────────────────────────────
// LoadSimulator
// ──────────────────────────────────────────────────────────────────────────────

/// Load simulator for testing worker behavior under different capacity constraints.
///
/// `LoadSimulator` controls worker capacity limits and records spawn attempts
/// to measure inter-launch delays and throughput characteristics.
pub struct LoadSimulator {
    /// Custom worker capacity limit (0 = unlimited, 1 = single worker).
    worker_capacity: u32,
    /// Timestamps of worker spawn attempts.
    spawn_attempts: Vec<Instant>,
    /// Base configuration for workers.
    base_config: Config,
    /// Temporary directory for test workspace.
    temp_dir: TempDir,
    /// Registry for worker coordination.
    registry: Arc<Registry>,
    /// Telemetry instance.
    telemetry: Telemetry,
}

impl LoadSimulator {
    /// Create a new `LoadSimulator` with the specified worker capacity.
    ///
    /// # Arguments
    ///
    /// * `worker_capacity` - Maximum number of concurrent workers (0 = unlimited)
    /// * `temp_dir` - Temporary directory for test isolation
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let temp_dir = tempfile::tempdir().unwrap();
    /// let simulator = LoadSimulator::new(1, temp_dir).unwrap();
    /// ```
    pub fn new(worker_capacity: u32, temp_dir: TempDir) -> Result<Self> {
        let mut config = Config::default();
        config.worker.max_workers = worker_capacity;
        config.workspace.home = temp_dir.path().to_path_buf();
        config.workspace.default = temp_dir.path().to_path_buf();

        let registry_dir = temp_dir.path().join("registry");
        std::fs::create_dir_all(&registry_dir)?;
        let registry = Arc::new(Registry::new(&registry_dir));

        let telemetry = Telemetry::new("load-simulator".to_string());

        Ok(LoadSimulator {
            worker_capacity,
            spawn_attempts: Vec::new(),
            base_config: config,
            temp_dir,
            registry,
            telemetry,
        })
    }

    /// Create a load simulator configured for saturation testing (capacity = 1).
    ///
    /// This is the most constrained scenario — only one worker can run at a time,
    /// maximizing contention and testing serialization behavior.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let temp_dir = tempfile::tempdir().unwrap();
    /// let simulator = LoadSimulator::saturated(temp_dir).unwrap();
    /// ```
    pub fn saturated(temp_dir: TempDir) -> Result<Self> {
        Self::new(1, temp_dir)
    }

    /// Create a load simulator with unlimited capacity.
    ///
    /// This removes the worker capacity constraint to test pure throughput
    /// without serialization bottlenecks.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let temp_dir = tempfile::tempdir().unwrap();
    /// let simulator = LoadSimulator::unlimited(temp_dir).unwrap();
    /// ```
    pub fn unlimited(temp_dir: TempDir) -> Result<Self> {
        Self::new(0, temp_dir)
    }

    /// Get the current worker capacity limit.
    ///
    /// Returns 0 if capacity is unlimited.
    pub fn worker_capacity(&self) -> u32 {
        self.worker_capacity
    }

    /// Set a new worker capacity limit.
    ///
    /// This allows dynamic adjustment of capacity during a test to simulate
    /// rising or falling load conditions.
    ///
    /// # Arguments
    ///
    /// * `capacity` - New capacity limit (0 = unlimited)
    pub fn set_worker_capacity(&mut self, capacity: u32) {
        self.worker_capacity = capacity;
        self.base_config.worker.max_workers = capacity;
    }

    /// Record a worker spawn attempt timestamp.
    ///
    /// Call this before spawning each worker to track inter-launch delays.
    pub fn record_spawn_attempt(&mut self) {
        self.spawn_attempts.push(Instant::now());
    }

    /// Get the number of recorded spawn attempts.
    pub fn spawn_attempt_count(&self) -> usize {
        self.spawn_attempts.len()
    }

    /// Calculate inter-launch delays between consecutive spawn attempts.
    ///
    /// Returns a vector of durations between each pair of consecutive spawns.
    /// Returns an empty vector if fewer than 2 spawn attempts were recorded.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// simulator.record_spawn_attempt();
    /// std::thread::sleep(Duration::from_millis(100));
    /// simulator.record_spawn_attempt();
    ///
    /// let delays = simulator.inter_launch_delays();
    /// assert!(delays.len() == 1);
    /// assert!(delays[0] >= Duration::from_millis(95));
    /// ```
    pub fn inter_launch_delays(&self) -> Vec<Duration> {
        self.spawn_attempts
            .windows(2)
            .map(|window| window[1].saturating_duration_since(window[0]))
            .collect()
    }

    /// Calculate the average inter-launch delay.
    ///
    /// Returns None if fewer than 2 spawn attempts were recorded.
    pub fn average_inter_launch_delay(&self) -> Option<Duration> {
        let delays = self.inter_launch_delays();
        if delays.is_empty() {
            None
        } else {
            let total: Duration = delays.iter().sum();
            Some(total / delays.len() as u32)
        }
    }

    /// Calculate the minimum inter-launch delay.
    ///
    /// Returns None if fewer than 2 spawn attempts were recorded.
    pub fn min_inter_launch_delay(&self) -> Option<Duration> {
        let delays = self.inter_launch_delays();
        if delays.is_empty() {
            None
        } else {
            Some(*delays.iter().min().unwrap())
        }
    }

    /// Calculate the maximum inter-launch delay.
    ///
    /// Returns None if fewer than 2 spawn attempts were recorded.
    pub fn max_inter_launch_delay(&self) -> Option<Duration> {
        let delays = self.inter_launch_delays();
        if delays.is_empty() {
            None
        } else {
            Some(*delays.iter().max().unwrap())
        }
    }

    /// Reset all spawn attempt records.
    ///
    /// Use this to start a new measurement phase without creating a new simulator.
    pub fn reset_spawn_attempts(&mut self) {
        self.spawn_attempts.clear();
    }

    /// Get the base configuration for workers.
    ///
    /// This configuration has the workspace and registry paths pre-configured
    /// for the test environment.
    pub fn base_config(&self) -> &Config {
        &self.base_config
    }

    /// Get a mutable reference to the base configuration.
    pub fn base_config_mut(&mut self) -> &mut Config {
        &mut self.base_config
    }

    /// Get the registry instance.
    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    /// Get the telemetry instance.
    pub fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }

    /// Get the temporary directory path.
    pub fn temp_dir(&self) -> &Path {
        self.temp_dir.path()
    }

    /// Simulate rising load by gradually increasing capacity.
    ///
    /// This method increases worker capacity in steps, recording spawn attempts
    /// at each level. It's useful for testing auto-scaling behavior.
    ///
    /// # Arguments
    ///
    /// * `initial_capacity` - Starting capacity (must be >= 1)
    /// * `final_capacity` - Target capacity (must be >= initial_capacity)
    /// * `steps` - Number of capacity increases (must be >= 1)
    /// * `delay_per_step` - Delay between capacity increases
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Simulate scale-up from 1 to 4 workers in 3 steps
    /// simulator.simulate_rising_load(1, 4, 3, Duration::from_secs(5)).await?;
    /// ```
    pub async fn simulate_rising_load(
        &mut self,
        initial_capacity: u32,
        final_capacity: u32,
        steps: u32,
        delay_per_step: Duration,
    ) -> Result<()> {
        if initial_capacity < 1 {
            anyhow::bail!("initial_capacity must be >= 1, got {}", initial_capacity);
        }
        if final_capacity < initial_capacity {
            anyhow::bail!(
                "final_capacity must be >= initial_capacity, got {} < {}",
                final_capacity,
                initial_capacity
            );
        }
        if steps < 1 {
            anyhow::bail!("steps must be >= 1, got {}", steps);
        }

        // Set initial capacity
        self.set_worker_capacity(initial_capacity);
        self.record_spawn_attempt();

        // Calculate capacity increment per step
        let capacity_increment = if steps == 1 {
            final_capacity.saturating_sub(initial_capacity)
        } else {
            (final_capacity.saturating_sub(initial_capacity)) / (steps - 1)
        };

        // Increase capacity step by step
        for step in 1..=steps {
            tokio::time::sleep(delay_per_step).await;

            let new_capacity = if step == steps {
                final_capacity
            } else {
                initial_capacity.saturating_add(capacity_increment * step)
            };

            self.set_worker_capacity(new_capacity);
            self.record_spawn_attempt();

            tracing::info!(
                step,
                new_capacity,
                "simulated rising load capacity increase"
            );
        }

        Ok(())
    }

    /// Create a test bead store with mock beads.
    ///
    /// This is a convenience method for creating a bead store populated with
    /// test beads for load simulation scenarios.
    ///
    /// # Arguments
    ///
    /// * `count` - Number of beads to create
    /// * `priority` - Priority for all beads (1 = highest)
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let store = simulator.mock_bead_store(10, 1).unwrap();
    /// assert_eq!(store.ready(&Filters::default()).await.unwrap().len(), 10);
    /// ```
    pub fn mock_bead_store(&self, count: usize, priority: u8) -> Result<Arc<dyn BeadStore>> {
        let mut beads = Vec::new();
        for i in 0..count {
            beads.push(Bead {
                id: BeadId::from(format!("load-sim-bead-{:03}", i)),
                title: format!("Load simulation bead {}", i),
                body: Some("Test bead for load simulation".to_string()),
                priority,
                status: BeadStatus::Open,
                assignee: None,
                labels: vec![],
                workspace: self.temp_dir.path().to_path_buf(),
                dependencies: vec![],
                dependents: vec![],
                created_at: Utc::now(),
                updated_at: Utc::now(),
            });
        }

        Ok(Arc::new(MockBeadStore::new(beads)))
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// MockBeadStore
// ──────────────────────────────────────────────────────────────────────────────

/// Mock bead store for load simulation tests.
///
/// This minimal implementation provides the necessary BeadStore methods
/// for testing worker behavior without requiring a real br workspace.
struct MockBeadStore {
    beads: Vec<Bead>,
}

impl MockBeadStore {
    fn new(beads: Vec<Bead>) -> Self {
        MockBeadStore { beads }
    }
}

#[async_trait::async_trait]
impl BeadStore for MockBeadStore {
    async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
        Ok(self
            .beads
            .iter()
            .filter(|b| b.status == BeadStatus::Open && b.assignee.is_none())
            .cloned()
            .collect())
    }

    async fn list_all(&self) -> Result<Vec<Bead>> {
        Ok(self.beads.clone())
    }

    async fn show(&self, id: &BeadId) -> Result<Bead> {
        self.beads
            .iter()
            .find(|b| b.id == *id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("bead not found: {}", id))
    }

    async fn claim(&self, id: &BeadId, actor: &str) -> Result<needle::types::ClaimResult> {
        for bead in &mut self.beads {
            if bead.id == *id {
                if bead.status != BeadStatus::Open || bead.assignee.is_some() {
                    return Ok(needle::types::ClaimResult::NotClaimable {
                        reason: "bead not open or already assigned".to_string(),
                    });
                }
                bead.status = BeadStatus::InProgress;
                bead.assignee = Some(actor.to_string());
                return Ok(needle::types::ClaimResult::Claimed(bead.clone()));
            }
        }
        Ok(needle::types::ClaimResult::NotClaimable {
            reason: "bead not found".to_string(),
        })
    }

    async fn claim_auto(&self, _actor: &str) -> Result<needle::types::ClaimResult> {
        Ok(needle::types::ClaimResult::NotClaimable {
            reason: "no beads available".to_string(),
        })
    }

    async fn release(&self, _id: &BeadId) -> Result<()> {
        // For load simulation, we just remove released beads to prevent re-selection
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn reopen(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
        Ok(vec![])
    }

    async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        Ok(())
    }

    async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        Ok(())
    }

    async fn create_bead(
        &self,
        _title: &str,
        _body: &str,
        _labels: &[&str],
    ) -> Result<BeadId> {
        Ok(BeadId::from("new-mock-bead"))
    }

    async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn remove_dependency(&self, _blocked_id: &BeadId, _blocker_id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn doctor_repair(&self) -> Result<needle::bead_store::RepairReport> {
        Ok(needle::bead_store::RepairReport::default())
    }

    async fn doctor_check(&self) -> Result<needle::bead_store::RepairReport> {
        Ok(needle::bead_store::RepairReport::default())
    }

    async fn full_rebuild(&self) -> Result<()> {
        Ok(())
    }

    fn has_valid_store(&self) -> bool {
        true
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Integration Test Helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Set up a saturated load test scenario.
///
/// Creates a `LoadSimulator` configured with capacity = 1 (single worker)
/// for testing serialization behavior and maximum contention scenarios.
///
/// # Returns
///
/// A tuple of `(LoadSimulator, TempDir)` where the TempDir must be kept
/// alive for the test duration.
///
/// # Examples
///
/// ```ignore
/// let (simulator, _temp_dir) = saturated_load_setup().await.unwrap();
/// assert_eq!(simulator.worker_capacity(), 1);
/// ```
pub async fn saturated_load_setup() -> Result<(LoadSimulator, TempDir)> {
    let temp_dir = tempfile::tempdir()?;
    let simulator = LoadSimulator::saturated(temp_dir)?;
    Ok((simulator, temp_dir))
}

/// Set up a rising load test scenario.
///
/// Creates a `LoadSimulator` configured to simulate capacity increasing
/// over time, starting from a single worker and scaling up.
///
/// # Arguments
///
/// * `initial_capacity` - Starting worker capacity (default: 1)
/// * `final_capacity` - Target worker capacity (default: 4)
/// * `steps` - Number of capacity increases (default: 3)
/// * `delay_per_step` - Delay between increases (default: 5 seconds)
///
/// # Returns
///
/// A tuple of `(LoadSimulator, TempDir)` where the TempDir must be kept
/// alive for the test duration.
///
/// # Examples
///
/// ```ignore
/// let (simulator, _temp_dir) = rising_load_setup(1, 4, 3, Duration::from_secs(5)).await.unwrap();
///
/// // Verify spawn attempts were recorded at each step
/// assert_eq!(simulator.spawn_attempt_count(), 4); // initial + 3 steps
/// ```
pub async fn rising_load_setup(
    initial_capacity: Option<u32>,
    final_capacity: Option<u32>,
    steps: Option<u32>,
    delay_per_step: Option<Duration>,
) -> Result<(LoadSimulator, TempDir)> {
    let temp_dir = tempfile::tempdir()?;
    let mut simulator = LoadSimulator::saturated(temp_dir)?;

    let initial = initial_capacity.unwrap_or(1);
    let final_cap = final_capacity.unwrap_or(4);
    let step_count = steps.unwrap_or(3);
    let delay = delay_per_step.unwrap_or(Duration::from_secs(5));

    simulator
        .simulate_rising_load(initial, final_cap, step_count, delay)
        .await?;

    Ok((simulator, temp_dir))
}

/// Set up a burst load test scenario.
///
/// Creates a `LoadSimulator` configured with a fixed capacity but a large
/// number of beads to test behavior under sudden load spikes.
///
/// # Arguments
///
/// * `capacity` - Worker capacity (default: 4)
/// * `bead_count` - Number of mock beads to create (default: 20)
///
/// # Returns
///
/// A tuple of `(LoadSimulator, Arc<dyn BeadStore>, TempDir)` where the
/// TempDir must be kept alive for the test duration.
///
/// # Examples
///
/// ```ignore
/// let (simulator, store, _temp_dir) = burst_load_setup(Some(4), Some(20)).await.unwrap();
///
/// // Verify beads are available
/// let ready = store.ready(&Filters::default()).await.unwrap();
/// assert_eq!(ready.len(), 20);
/// ```
pub async fn burst_load_setup(
    capacity: Option<u32>,
    bead_count: Option<usize>,
) -> Result<(LoadSimulator, Arc<dyn BeadStore>, TempDir)> {
    let temp_dir = tempfile::tempdir()?;
    let worker_cap = capacity.unwrap_or(4);
    let bead_cnt = bead_count.unwrap_or(20);

    let simulator = LoadSimulator::new(worker_cap, temp_dir)?;
    let store = simulator.mock_bead_store(bead_cnt, 1)?;

    Ok((simulator, store, temp_dir))
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_load_simulator_creation() {
        let temp_dir = tempfile::tempdir().unwrap();
        let simulator = LoadSimulator::new(2, temp_dir).unwrap();
        assert_eq!(simulator.worker_capacity(), 2);
    }

    #[tokio::test]
    async fn test_saturated_load_setup() {
        let (simulator, _temp_dir) = saturated_load_setup().await.unwrap();
        assert_eq!(simulator.worker_capacity(), 1);
    }

    #[tokio::test]
    async fn test_unlimited_load_simulator() {
        let temp_dir = tempfile::tempdir().unwrap();
        let simulator = LoadSimulator::unlimited(temp_dir).unwrap();
        assert_eq!(simulator.worker_capacity(), 0);
    }

    #[tokio::test]
    async fn test_spawn_attempt_tracking() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut simulator = LoadSimulator::new(1, temp_dir).unwrap();

        simulator.record_spawn_attempt();
        assert_eq!(simulator.spawn_attempt_count(), 1);

        simulator.record_spawn_attempt();
        assert_eq!(simulator.spawn_attempt_count(), 2);
    }

    #[tokio::test]
    async fn test_inter_launch_delays() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut simulator = LoadSimulator::new(1, temp_dir).unwrap();

        simulator.record_spawn_attempt();
        std::thread::sleep(Duration::from_millis(100));
        simulator.record_spawn_attempt();

        let delays = simulator.inter_launch_delays();
        assert_eq!(delays.len(), 1);
        assert!(delays[0] >= Duration::from_millis(95));
    }

    #[tokio::test]
    async fn test_average_inter_launch_delay() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut simulator = LoadSimulator::new(1, temp_dir).unwrap();

        simulator.record_spawn_attempt();
        std::thread::sleep(Duration::from_millis(100));
        simulator.record_spawn_attempt();
        std::thread::sleep(Duration::from_millis(50));
        simulator.record_spawn_attempt();

        let avg = simulator.average_inter_launch_delay().unwrap();
        assert!(avg >= Duration::from_millis(70));
        assert!(avg <= Duration::from_millis(80));
    }

    #[tokio::test]
    async fn test_reset_spawn_attempts() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut simulator = LoadSimulator::new(1, temp_dir).unwrap();

        simulator.record_spawn_attempt();
        simulator.record_spawn_attempt();
        assert_eq!(simulator.spawn_attempt_count(), 2);

        simulator.reset_spawn_attempts();
        assert_eq!(simulator.spawn_attempt_count(), 0);
    }

    #[tokio::test]
    async fn test_set_worker_capacity() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut simulator = LoadSimulator::new(1, temp_dir).unwrap();

        assert_eq!(simulator.worker_capacity(), 1);
        simulator.set_worker_capacity(4);
        assert_eq!(simulator.worker_capacity(), 4);
    }

    #[tokio::test]
    async fn test_simulate_rising_load() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut simulator = LoadSimulator::saturated(temp_dir).unwrap();

        simulator
            .simulate_rising_load(1, 3, 3, Duration::from_millis(100))
            .await
            .unwrap();

        // Should have recorded spawn attempts at initial + each step
        assert_eq!(simulator.spawn_attempt_count(), 3);

        // Verify capacity was increased
        assert_eq!(simulator.worker_capacity(), 3);
    }

    #[tokio::test]
    async fn test_simulate_rising_load_single_step() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut simulator = LoadSimulator::saturated(temp_dir).unwrap();

        simulator
            .simulate_rising_load(1, 4, 1, Duration::from_millis(50))
            .await
            .unwrap();

        // Single step should go directly to final capacity
        assert_eq!(simulator.spawn_attempt_count(), 1);
        assert_eq!(simulator.worker_capacity(), 4);
    }

    #[tokio::test]
    async fn test_simulate_rising_load_validation() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut simulator = LoadSimulator::saturated(temp_dir).unwrap();

        // initial_capacity < 1 should fail
        let result = simulator
            .simulate_rising_load(0, 4, 3, Duration::from_millis(100))
            .await;
        assert!(result.is_err());

        // final_capacity < initial_capacity should fail
        let result = simulator
            .simulate_rising_load(4, 2, 3, Duration::from_millis(100))
            .await;
        assert!(result.is_err());

        // steps < 1 should fail
        let result = simulator
            .simulate_rising_load(1, 4, 0, Duration::from_millis(100))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_mock_bead_store() {
        let temp_dir = tempfile::tempdir().unwrap();
        let simulator = LoadSimulator::new(1, temp_dir).unwrap();

        let store = simulator.mock_bead_store(5, 1).unwrap();
        let ready = store.ready(&Filters::default()).await.unwrap();

        assert_eq!(ready.len(), 5);
    }

    #[tokio::test]
    async fn test_rising_load_setup_helper() {
        let (simulator, _temp_dir) = rising_load_setup(Some(1), Some(3), Some(2), None).await.unwrap();

        // Should have recorded initial + 2 steps = 3 spawn attempts
        assert_eq!(simulator.spawn_attempt_count(), 3);
        assert_eq!(simulator.worker_capacity(), 3);
    }

    #[tokio::test]
    async fn test_burst_load_setup_helper() {
        let (simulator, store, _temp_dir) = burst_load_setup(Some(4), Some(15)).await.unwrap();

        assert_eq!(simulator.worker_capacity(), 4);

        let ready = store.ready(&Filters::default()).await.unwrap();
        assert_eq!(ready.len(), 15);
    }

    #[tokio::test]
    async fn test_inter_launch_delay_statistics() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut simulator = LoadSimulator::new(1, temp_dir).unwrap();

        simulator.record_spawn_attempt();
        std::thread::sleep(Duration::from_millis(100));
        simulator.record_spawn_attempt();
        std::thread::sleep(Duration::from_millis(150));
        simulator.record_spawn_attempt();

        let delays = simulator.inter_launch_delays();
        assert_eq!(delays.len(), 2);

        let min = simulator.min_inter_launch_delay().unwrap();
        let max = simulator.max_inter_launch_delay().unwrap();
        let avg = simulator.average_inter_launch_delay().unwrap();

        assert!(min < avg);
        assert!(max > avg);
    }
}
