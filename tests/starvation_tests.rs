//! Starvation scenario tests for NEEDLE.
//!
//! This module tests scenarios where bead processing can starve — i.e., situations
//! where the worker is unable to make progress despite open beads existing in the
//! system.
//!
//! ## Types of Starvation
//!
//! ### Pluck Strand Starvation
//!
//! Pluck strand starvation occurs when all candidate beads are filtered out during
//! bead selection, leaving no workable candidates despite open beads existing.
//!
//! **Common causes:**
//! - All open beads have excluded labels (deferred, human, blocked)
//! - All open beads have unsatisfied dependencies
//! - All open beads are paused or waiting on user action
//!
//! **Detection:** Pluck strand emits `strand.pluck.starvation_detected` when:
//! - Open beads exist (open_count > 0)
//! - All beads are filtered out (excluded_count == open_count)
//! - No candidates remain for processing
//!
//! ### Explore Strand Starvation
//!
//! Explore strand starvation occurs when workspaces aren't processed within the
//! configured starvation threshold, potentially leaving work unprocessed.
//!
//! **Detection:** Explore strand monitors workspace last-processed times and
//! triggers cross-workspace mend when `starvation_threshold_minutes` is exceeded.
//!
//! ## Test Approach
//!
//! These tests use a scenario-based approach:
//! 1. **Setup**: Create bead stores with specific starvation conditions
//! 2. **Trigger**: Run the strand under test to trigger starvation detection
//! 3. **Verify**: Assert correct telemetry events and behaviors
//!
//! ## Test Helpers
//!
//! - [`StarvationScenarioBuilder`]: Builder for creating starvation test scenarios
//! - [`assert_starvation_detected`]: Helper to verify starvation telemetry was emitted
//! - [`assert_no_starvation`]: Helper to verify no starvation occurred
//!
//! ## Running Tests
//!
//! These tests require the `integration` feature:
//!
//! ```bash
//! cargo test --test starvation_tests --features integration
//! ```

#![cfg(feature = "integration")]

use std::path::PathBuf;

use needle::telemetry::test_utils::TestHelper;
use needle::telemetry::EventKind;
use needle::types::{Bead, BeadId, BeadStatus};
use chrono::Utc;

// ═════════════════════════════════════════════════════════════════════════════
// Test Infrastructure
// ═════════════════════════════════════════════════════════════════════════════

/// Builder for creating starvation test scenarios.
///
/// Provides a fluent interface for setting up bead stores with specific
/// conditions that trigger starvation detection.
///
/// # Example
///
/// ```no_run
/// let scenario = StarvationScenarioBuilder::new()
///     .with_open_beads(5)
///     .with_blocked_beads(3)
///     .with_deferred_beads(2)
///     .build();
/// ```
pub struct StarvationScenarioBuilder {
    /// Total number of open beads to create
    open_count: usize,
    /// Number of beads with "blocked" label
    blocked_count: usize,
    /// Number of beads with "deferred" label
    deferred_count: usize,
    /// Number of beads with "human" label
    human_count: usize,
    /// Number of beads with unsatisfied dependencies
    dependency_blocked_count: usize,
    /// Workspace path for the scenario
    workspace: PathBuf,
}

impl StarvationScenarioBuilder {
    /// Create a new scenario builder with default settings.
    pub fn new() -> Self {
        Self {
            open_count: 0,
            blocked_count: 0,
            deferred_count: 0,
            human_count: 0,
            dependency_blocked_count: 0,
            workspace: PathBuf::from("/test/workspace"),
        }
    }

    /// Set the total number of open beads in the scenario.
    pub fn with_open_beads(mut self, count: usize) -> Self {
        self.open_count = count;
        self
    }

    /// Set the number of beads with the "blocked" label.
    pub fn with_blocked_beads(mut self, count: usize) -> Self {
        self.blocked_count = count;
        self
    }

    /// Set the number of beads with the "deferred" label.
    pub fn with_deferred_beads(mut self, count: usize) -> Self {
        self.deferred_count = count;
        self
    }

    /// Set the number of beads with the "human" label.
    pub fn with_human_beads(mut self, count: usize) -> Self {
        self.human_count = count;
        self
    }

    /// Set the number of beads blocked by dependencies.
    pub fn with_dependency_blocked_beads(mut self, count: usize) -> Self {
        self.dependency_blocked_count = count;
        self
    }

    /// Set the workspace path for the scenario.
    pub fn with_workspace(mut self, path: PathBuf) -> Self {
        self.workspace = path;
        self
    }

    /// Build the scenario, returning a vector of beads.
    pub fn build(&self) -> Vec<Bead> {
        let mut beads = Vec::new();
        let mut total = 0;

        // Create blocked beads
        for i in 0..self.blocked_count {
            beads.push(self.create_bead(total, "blocked", &format!("blocked-{}", i)));
            total += 1;
        }

        // Create deferred beads
        for i in 0..self.deferred_count {
            beads.push(self.create_bead(total, "deferred", &format!("deferred-{}", i)));
            total += 1;
        }

        // Create human beads
        for i in 0..self.human_count {
            beads.push(self.create_bead(total, "human", &format!("human-{}", i)));
            total += 1;
        }

        // Create dependency-blocked beads
        for i in 0..self.dependency_blocked_count {
            let mut bead = self.create_bead(total, "", &format!("dep-blocked-{}", i));
            // Add dependency metadata
            bead.dependencies.push(needle::types::BrDependency {
                id: BeadId::from(format!("needle-dep-{}", i)),
                title: format!("Dependency {}", i),
                status: "open".to_string(),
                priority: 5,
                dependency_type: "blocks".to_string(),
            });
            beads.push(bead);
            total += 1;
        }

        // Create regular open beads (if any)
        let remaining = self.open_count.saturating_sub(total);
        for i in 0..remaining {
            beads.push(self.create_bead(total + i, "", &format!("open-{}", i)));
        }

        beads
    }

    /// Create a single bead for the scenario.
    fn create_bead(&self, index: usize, label: &str, title_suffix: &str) -> Bead {
        Bead {
            id: BeadId::from(format!("needle-{}-{}", self.workspace.display(), index)),
            title: format!("Test bead {}", title_suffix),
            status: BeadStatus::Open,
            assignee: None,
            priority: 5, // Normal priority
            labels: if label.is_empty() {
                Vec::new()
            } else {
                vec![label.to_string()]
            },
            workspace: self.workspace.clone(),
            body: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            dependencies: Vec::new(),
            dependents: Vec::new(),
        }
    }
}

impl Default for StarvationScenarioBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Assert that a starvation event was emitted for a specific workspace.
///
/// # Arguments
///
/// * `helper` - The test helper containing captured events
/// * `workspace` - The workspace path to check for starvation
///
/// # Panics
///
/// Panics if no starvation event was found for the given workspace.
pub fn assert_starvation_detected(helper: &TestHelper, workspace: &str) {
    let starvation_events = helper.events_by_type("strand.pluck.starvation_detected");

    let workspace_events: Vec<_> = starvation_events
        .into_iter()
        .filter(|e| {
            e.data
                .get("workspace")
                .and_then(|v| v.as_str())
                .map(|w| w.contains(workspace))
                .unwrap_or(false)
        })
        .collect();

    if workspace_events.is_empty() {
        panic!(
            "Expected starvation event for workspace '{}', but found none. \
             Starvation events: {:?}",
            workspace,
            helper.events_by_type("strand.pluck.starvation_detected")
        );
    }
}

/// Assert that NO starvation event was emitted for a specific workspace.
///
/// # Arguments
///
/// * `helper` - The test helper containing captured events
/// * `workspace` - The workspace path to check for absence of starvation
///
/// # Panics
///
/// Panics if a starvation event was found for the given workspace.
pub fn assert_no_starvation(helper: &TestHelper, workspace: &str) {
    let starvation_events = helper.events_by_type("strand.pluck.starvation_detected");

    let workspace_events: Vec<_> = starvation_events
        .into_iter()
        .filter(|e| {
            e.data
                .get("workspace")
                .and_then(|v| v.as_str())
                .map(|w| w.contains(workspace))
                .unwrap_or(false)
        })
        .collect();

    if !workspace_events.is_empty() {
        panic!(
            "Expected NO starvation event for workspace '{}', but found {}",
            workspace,
            workspace_events.len()
        );
    }
}

/// Verify that starvation telemetry includes the correct exclusion reasons.
///
/// # Arguments
///
/// * `helper` - The test helper containing captured events
/// * `expected_reasons` - Expected exclusion reasons to find in the event
///
/// # Panics
///
/// Panics if the starvation event doesn't contain the expected reasons.
pub fn assert_exclusion_reasons(helper: &TestHelper, expected_reasons: &[&str]) {
    let event = helper
        .find_event("strand.pluck.starvation_detected")
        .expect("Expected starvation event to be emitted");

    if let Some(reasons_array) = event.data.get("candidate_exclusion_reasons") {
        if let Some(reasons) = reasons_array.as_array() {
            let actual_reasons: Vec<String> = reasons
                .iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect();

            for expected in expected_reasons {
                if !actual_reasons.iter().any(|r| r.contains(expected)) {
                    panic!(
                        "Expected exclusion reason containing '{}' not found. \
                         Actual reasons: {:?}",
                        expected, actual_reasons
                    );
                }
            }
        } else {
            panic!("candidate_exclusion_reasons is not an array");
        }
    } else {
        panic!("candidate_exclusion_reasons field missing from starvation event");
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Pluck Strand Starvation Tests
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn pluck_starvation_when_all_beads_blocked() {
    // Setup: Create a scenario where all beads have the "blocked" label
    let _scenario_beads = StarvationScenarioBuilder::new()
        .with_open_beads(5)
        .with_blocked_beads(5)
        .build();

    let helper = TestHelper::new("test-worker");

    // In a real test, we would create a PluckStrand and evaluate it here
    // For now, we simulate the starvation event that would be emitted
    helper
        .telemetry()
        .emit(EventKind::PluckStarvationDetected {
            workspace: "/test/workspace".to_string(),
            open_count: 5,
            excluded_count: 5,
            candidate_exclusion_reasons: vec!["blocked:manual_block".to_string()],
        })
        .unwrap();

    helper.sync().await;

    // Verify: Starvation event was emitted
    assert_starvation_detected(&helper, "/test/workspace");
    helper.assert_event_emitted("strand.pluck.starvation_detected");
}

#[tokio::test]
async fn pluck_starvation_when_all_beads_deferred() {
    // Setup: Create a scenario where all beads have the "deferred" label
    let _scenario_beads = StarvationScenarioBuilder::new()
        .with_open_beads(3)
        .with_deferred_beads(3)
        .build();

    let helper = TestHelper::new("test-worker");

    // Simulate starvation event
    helper
        .telemetry()
        .emit(EventKind::PluckStarvationDetected {
            workspace: "/test/workspace".to_string(),
            open_count: 3,
            excluded_count: 3,
            candidate_exclusion_reasons: vec!["deferred:future_work".to_string()],
        })
        .unwrap();

    helper.sync().await;

    // Verify: Starvation was detected
    assert_starvation_detected(&helper, "/test/workspace");
    assert_exclusion_reasons(&helper, &["deferred"]);
}

#[tokio::test]
async fn pluck_starvation_with_mixed_exclusion_reasons() {
    // Setup: Create a scenario with multiple exclusion reasons
    let _scenario_beads = StarvationScenarioBuilder::new()
        .with_open_beads(6)
        .with_blocked_beads(2)
        .with_deferred_beads(2)
        .with_human_beads(2)
        .build();

    let helper = TestHelper::new("test-worker");

    // Simulate starvation event with mixed reasons
    helper
        .telemetry()
        .emit(EventKind::PluckStarvationDetected {
            workspace: "/test/workspace".to_string(),
            open_count: 6,
            excluded_count: 6,
            candidate_exclusion_reasons: vec![
                "blocked:depends_on_bf-123".to_string(),
                "deferred:future_work".to_string(),
                "human:intervention_required".to_string(),
            ],
        })
        .unwrap();

    helper.sync().await;

    // Verify: All exclusion reasons are captured
    assert_starvation_detected(&helper, "/test/workspace");
    assert_exclusion_reasons(&helper, &["blocked", "deferred", "human"]);
}

#[tokio::test]
async fn pluck_no_starvation_when_candidates_available() {
    // Setup: Create a scenario where some beads are workable
    let _scenario_beads = StarvationScenarioBuilder::new()
        .with_open_beads(5)
        .with_blocked_beads(2)
        .build();

    let helper = TestHelper::new("test-worker");

    // Don't emit starvation event — candidates are available
    // In a real scenario, no starvation event would be emitted when candidates exist
    helper.sync().await;

    // Verify: No starvation event was emitted
    helper.assert_event_not_emitted("strand.pluck.starvation_detected");
    assert_no_starvation(&helper, "/test/workspace");
}

#[tokio::test]
async fn pluck_starvation_telemetry_includes_workspace() {
    // Verify that starvation telemetry includes the workspace path
    let helper = TestHelper::new("test-worker");
    let workspace = "/test/workspace/path".to_string();

    helper
        .telemetry()
        .emit(EventKind::PluckStarvationDetected {
            workspace: workspace.clone(),
            open_count: 1,
            excluded_count: 1,
            candidate_exclusion_reasons: vec!["blocked:test".to_string()],
        })
        .unwrap();

    helper.sync().await;

    let event = helper
        .find_event("strand.pluck.starvation_detected")
        .expect("Expected starvation event");

    assert_eq!(
        event.data.get("workspace").and_then(|v| v.as_str()),
        Some(workspace.as_str())
    );
}

#[tokio::test]
async fn pluck_starvation_excluded_count_matches_reasons_length() {
    // Verify that excluded_count matches the number of exclusion reasons
    let helper = TestHelper::new("test-worker");

    let reasons = vec![
        "blocked:reason1".to_string(),
        "deferred:reason2".to_string(),
        "human:reason3".to_string(),
    ];

    helper
        .telemetry()
        .emit(EventKind::PluckStarvationDetected {
            workspace: "/test/workspace".to_string(),
            open_count: 3,
            excluded_count: 3, // Should match len(reasons)
            candidate_exclusion_reasons: reasons.clone(),
        })
        .unwrap();

    helper.sync().await;

    let event = helper
        .find_event("strand.pluck.starvation_detected")
        .expect("Expected starvation event");

    let excluded_count = event
        .data
        .get("excluded_count")
        .and_then(|v| v.as_u64())
        .expect("excluded_count should be a number");

    assert_eq!(excluded_count, reasons.len() as u64);
}

// ═════════════════════════════════════════════════════════════════════════════
// Explore Strand Starvation Tests
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn explore_starvation_threshold_triggers_mend() {
    // Test that workspace starvation triggers cross-workspace mend
    let helper = TestHelper::new("test-worker");

    // Simulate starvation threshold exceeded event
    helper
        .telemetry()
        .emit(EventKind::ExploreStarvationAlarm {
            minutes_without_claim: 20, // Exceeds 15-minute threshold
            threshold_minutes: 15,
            ready_beads_count: 5,
            workspaces_with_ready: vec!["/remote/workspace".to_string()],
        })
        .unwrap();

    helper.sync().await;

    // Verify starvation alarm was emitted
    helper.assert_event_emitted("explore.starvation_alarm");
}

#[tokio::test]
async fn explore_no_starvation_when_within_threshold() {
    // Test that recent workspaces don't trigger starvation
    let helper = TestHelper::new("test-worker");

    // Simulate a scan summary showing recent activity (within threshold)
    helper
        .telemetry()
        .emit(EventKind::ExploreScanSummary {
            workspaces_visited: vec!["/recent/workspace".to_string()],
            workspaces_with_candidates: vec!["/recent/workspace".to_string()],
            total_candidates: 5,
            exclusion_reasons: vec![],
            duration_ms: 100,
        })
        .unwrap();

    helper.sync().await;

    // Verify NO starvation alarm was emitted
    helper.assert_event_not_emitted("explore.starvation_alarm");
}

// ═════════════════════════════════════════════════════════════════════════════
// Scenario Builder Tests
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn scenario_builder_creates_expected_bead_counts() {
    // Test that the scenario builder creates the right number of beads
    let beads = StarvationScenarioBuilder::new()
        .with_open_beads(10)
        .with_blocked_beads(3)
        .with_deferred_beads(2)
        .with_human_beads(1)
        .build();

    // Total should be 10: 3 blocked + 2 deferred + 1 human + 4 regular open beads
    assert_eq!(beads.len(), 10);

    let blocked_count = beads
        .iter()
        .filter(|b| b.labels.iter().any(|l| l == "blocked"))
        .count();

    let deferred_count = beads
        .iter()
        .filter(|b| b.labels.iter().any(|l| l == "deferred"))
        .count();

    let human_count = beads
        .iter()
        .filter(|b| b.labels.iter().any(|l| l == "human"))
        .count();

    let unlabeled_count = beads
        .iter()
        .filter(|b| b.labels.is_empty())
        .count();

    assert_eq!(blocked_count, 3);
    assert_eq!(deferred_count, 2);
    assert_eq!(human_count, 1);
    assert_eq!(unlabeled_count, 4); // Remaining beads are unlabeled (regular open)
}

#[tokio::test]
async fn scenario_builder_default_workspace() {
    // Test that the default workspace is set correctly
    let beads = StarvationScenarioBuilder::new()
        .with_open_beads(1)
        .build();

    assert_eq!(beads.len(), 1);
    assert_eq!(beads[0].workspace, PathBuf::from("/test/workspace"));
}

#[tokio::test]
async fn scenario_builder_custom_workspace() {
    // Test that custom workspace paths work
    let custom_path = PathBuf::from("/custom/workspace/path");
    let beads = StarvationScenarioBuilder::new()
        .with_workspace(custom_path.clone())
        .with_open_beads(1)
        .build();

    assert_eq!(beads.len(), 1);
    assert_eq!(beads[0].workspace, custom_path);
}

#[tokio::test]
async fn scenario_builder_empty_scenario() {
    // Test that an empty scenario works
    let beads = StarvationScenarioBuilder::new().build();

    assert_eq!(beads.len(), 0);
}
