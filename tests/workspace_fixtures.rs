//! Test fixtures and mock helpers for workspace scenarios.
//!
//! This module provides reusable test infrastructure for creating mock workspaces
//! and candidates with various states (excluded, assigned, claimable) to support
//! testing of multi-workspace discovery and deadlock scenarios.

use std::path::PathBuf;

use anyhow::Result;
use chrono::Utc;
use needle::bead_store::{BeadStore, Filters, RepairReport};
use needle::registry::Registry;
use needle::telemetry::Telemetry;
use needle::types::{Bead, BeadId, BeadStatus, ClaimResult};

// ────────────────────────────────────────────────────────────────────────────────
// Mock Workspace Structures
// ────────────────────────────────────────────────────────────────────────────────

/// Represents a mock workspace with a specific state.
#[derive(Debug, Clone)]
pub struct MockWorkspace {
    /// Workspace path
    pub path: PathBuf,
    /// Workspace state (dead or alive)
    pub state: WorkspaceState,
}

/// State of a mock workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceState {
    /// Workspace has no claimable candidates (all excluded or assigned)
    Dead,
    /// Workspace has valid claimable candidates
    Alive,
}

/// Represents the state of a candidate bead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateState {
    /// Candidate is excluded by labels (deferred, human, blocked)
    Excluded { labels: Vec<String> },
    /// Candidate is assigned to another worker
    Assigned { assignee: String },
    /// Candidate is available to be claimed
    Claimable,
}

/// Mock candidate bead with controlled state.
#[derive(Debug, Clone)]
pub struct MockCandidate {
    /// Unique identifier
    pub id: String,
    /// Candidate title
    pub title: String,
    /// Candidate state
    pub state: CandidateState,
    /// Priority (lower = higher priority)
    pub priority: u8,
    /// Workspace this candidate belongs to
    pub workspace: PathBuf,
}

impl MockCandidate {
    /// Create a new mock candidate.
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        state: CandidateState,
        workspace: PathBuf,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            state,
            priority: 1,
            workspace,
        }
    }

    /// Create a new mock candidate with custom priority.
    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// Convert to a Bead for use in mock stores.
    pub fn to_bead(&self) -> Bead {
        let (assignee, labels) = match &self.state {
            CandidateState::Excluded { labels } => (None, labels.clone()),
            CandidateState::Assigned { assignee } => (Some(assignee.clone()), vec![]),
            CandidateState::Claimable => (None, vec![]),
        };

        Bead {
            id: BeadId::from(self.id.clone()),
            title: self.title.clone(),
            body: None,
            priority: self.priority,
            status: BeadStatus::Open,
            assignee,
            labels,
            workspace: self.workspace.clone(),
            dependencies: vec![],
            dependents: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────────
// Scenario Builders
// ────────────────────────────────────────────────────────────────────────────────

/// Builder for creating test workspace scenarios.
pub struct ScenarioBuilder {
    workspaces: Vec<MockWorkspace>,
    candidates: Vec<MockCandidate>,
}

impl ScenarioBuilder {
    /// Create a new scenario builder.
    pub fn new() -> Self {
        Self {
            workspaces: vec![],
            candidates: vec![],
        }
    }

    /// Add a workspace to the scenario.
    pub fn add_workspace(mut self, path: PathBuf, state: WorkspaceState) -> Self {
        self.workspaces.push(MockWorkspace { path, state });
        self
    }

    /// Add a candidate to the scenario.
    pub fn add_candidate(mut self, candidate: MockCandidate) -> Self {
        self.candidates.push(candidate);
        self
    }

    /// Add multiple candidates to the scenario.
    pub fn add_candidates(mut self, candidates: impl IntoIterator<Item = MockCandidate>) -> Self {
        self.candidates.extend(candidates);
        self
    }

    /// Build the scenario and return the workspace list and candidate map.
    pub fn build(self) -> (Vec<PathBuf>, Vec<MockCandidate>) {
        let workspace_paths: Vec<PathBuf> =
            self.workspaces.iter().map(|w| w.path.clone()).collect();
        (workspace_paths, self.candidates)
    }

    /// Get workspace states for verification.
    pub fn workspace_states(&self) -> Vec<(PathBuf, WorkspaceState)> {
        self.workspaces
            .iter()
            .map(|w| (w.path.clone(), w.state.clone()))
            .collect()
    }
}

impl Default for ScenarioBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ────────────────────────────────────────────────────────────────────────────────
// Helper Functions
// ────────────────────────────────────────────────────────────────────────────────

/// Create a basic test workspace path.
pub fn test_workspace_path(name: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/test/workspace-{}", name))
}

/// Create a test home workspace path.
pub fn test_home_path() -> PathBuf {
    PathBuf::from("/tmp/test/home")
}

/// Create the classic deadlock scenario:
/// - Workspace 1: All candidates excluded or assigned (dead)
/// - Workspace 2: Valid claimable candidates (alive)
pub fn deadlock_scenario() -> (Vec<PathBuf>, Vec<MockCandidate>, PathBuf) {
    let ws1 = test_workspace_path("ws1");
    let ws2 = test_workspace_path("ws2");
    let home = test_home_path();

    let _workspace1_dead = MockWorkspace {
        path: ws1.clone(),
        state: WorkspaceState::Dead,
    };

    let _workspace2_alive = MockWorkspace {
        path: ws2.clone(),
        state: WorkspaceState::Alive,
    };

    // Workspace 1 candidates: all assigned or excluded
    let candidates = vec![
        // Assigned candidates (filtered out by strand)
        MockCandidate::new(
            "ws1-assigned-1",
            "Assigned Task 1",
            CandidateState::Assigned {
                assignee: "other-worker-1".to_string(),
            },
            ws1.clone(),
        ),
        MockCandidate::new(
            "ws1-assigned-2",
            "Assigned Task 2",
            CandidateState::Assigned {
                assignee: "other-worker-2".to_string(),
            },
            ws1.clone(),
        ),
        // Excluded candidates (filtered out by labels)
        MockCandidate::new(
            "ws1-blocked",
            "Blocked Task",
            CandidateState::Excluded {
                labels: vec!["blocked".to_string()],
            },
            ws1.clone(),
        ),
        MockCandidate::new(
            "ws1-deferred",
            "Deferred Task",
            CandidateState::Excluded {
                labels: vec!["deferred".to_string()],
            },
            ws1.clone(),
        ),
        // Workspace 2 candidates: valid claimable
        MockCandidate::new(
            "ws2-valid-1",
            "Valid Task 1",
            CandidateState::Claimable,
            ws2.clone(),
        ),
        MockCandidate::new(
            "ws2-valid-2",
            "Valid Task 2",
            CandidateState::Claimable,
            ws2.clone(),
        ),
    ];

    let workspaces = vec![ws1.clone(), ws2.clone()];
    (workspaces, candidates, home)
}

/// Create a scenario where all workspaces are dead (no claimable candidates).
pub fn all_dead_scenario() -> (Vec<PathBuf>, Vec<MockCandidate>, PathBuf) {
    let ws1 = test_workspace_path("ws1");
    let ws2 = test_workspace_path("ws2");
    let home = test_home_path();

    let candidates = vec![
        MockCandidate::new(
            "ws1-assigned",
            "Assigned Task",
            CandidateState::Assigned {
                assignee: "worker-1".to_string(),
            },
            ws1.clone(),
        ),
        MockCandidate::new(
            "ws2-blocked",
            "Blocked Task",
            CandidateState::Excluded {
                labels: vec!["blocked".to_string()],
            },
            ws2.clone(),
        ),
    ];

    let workspaces = vec![ws1, ws2];
    (workspaces, candidates, home)
}

/// Create a scenario where all workspaces are alive (all have claimable candidates).
pub fn all_alive_scenario() -> (Vec<PathBuf>, Vec<MockCandidate>, PathBuf) {
    let ws1 = test_workspace_path("ws1");
    let ws2 = test_workspace_path("ws2");
    let ws3 = test_workspace_path("ws3");
    let home = test_home_path();

    let candidates = vec![
        MockCandidate::new("ws1-task", "Task 1", CandidateState::Claimable, ws1.clone()),
        MockCandidate::new("ws2-task", "Task 2", CandidateState::Claimable, ws2.clone()),
        MockCandidate::new("ws3-task", "Task 3", CandidateState::Claimable, ws3.clone()),
    ];

    let workspaces = vec![ws1, ws2, ws3];
    (workspaces, candidates, home)
}

/// Create a scenario with mixed candidate states in a single workspace.
pub fn mixed_states_scenario() -> (Vec<PathBuf>, Vec<MockCandidate>, PathBuf) {
    let ws1 = test_workspace_path("ws1");
    let home = test_home_path();

    let candidates = vec![
        MockCandidate::new(
            "assigned",
            "Assigned Task",
            CandidateState::Assigned {
                assignee: "worker-1".to_string(),
            },
            ws1.clone(),
        ),
        MockCandidate::new(
            "blocked",
            "Blocked Task",
            CandidateState::Excluded {
                labels: vec!["blocked".to_string()],
            },
            ws1.clone(),
        ),
        MockCandidate::new(
            "valid",
            "Valid Task",
            CandidateState::Claimable,
            ws1.clone(),
        ),
    ];

    let workspaces = vec![ws1];
    (workspaces, candidates, home)
}

/// Create a standard test registry with temp directory.
pub fn test_registry() -> Registry {
    let temp_dir = tempfile::tempdir().unwrap();
    Registry::new(temp_dir.path())
}

/// Create a standard test telemetry instance.
pub fn test_telemetry() -> Telemetry {
    Telemetry::new("test-worker".to_string())
}

/// Create standard test components (registry, telemetry, worker_id).
pub fn test_components() -> (Registry, Telemetry, String) {
    let registry = test_registry();
    let telemetry = test_telemetry();
    let worker_id = "test-worker".to_string();
    (registry, telemetry, worker_id)
}

// ────────────────────────────────────────────────────────────────────────────────
// Mock BeadStore Implementations
// ────────────────────────────────────────────────────────────────────────────────

/// Mock bead store that returns predefined candidates.
pub struct MockCandidateStore {
    /// Candidates to return from ready()
    pub candidates: Vec<Bead>,
    /// Whether the store should succeed or fail
    pub should_fail: bool,
    /// Error message if should_fail is true
    pub error_message: String,
}

impl MockCandidateStore {
    /// Create a new mock store with the given candidates.
    pub fn new(candidates: Vec<Bead>) -> Self {
        Self {
            candidates,
            should_fail: false,
            error_message: "Mock store error".to_string(),
        }
    }

    /// Create a mock store that always returns empty results.
    pub fn empty() -> Self {
        Self::new(vec![])
    }

    /// Create a mock store that always fails.
    pub fn failing(message: impl Into<String>) -> Self {
        Self {
            candidates: vec![],
            should_fail: true,
            error_message: message.into(),
        }
    }
}

#[async_trait::async_trait]
impl BeadStore for MockCandidateStore {
    async fn list_all(&self) -> Result<Vec<Bead>> {
        if self.should_fail {
            anyhow::bail!(self.error_message.clone());
        }
        Ok(self.candidates.clone())
    }

    async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
        if self.should_fail {
            anyhow::bail!(self.error_message.clone());
        }
        Ok(self.candidates.clone())
    }

    async fn show(&self, _id: &BeadId) -> Result<Bead> {
        if self.should_fail {
            anyhow::bail!(self.error_message.clone());
        }
        anyhow::bail!("not found")
    }

    async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
        if self.should_fail {
            anyhow::bail!(self.error_message.clone());
        }
        anyhow::bail!("not implemented")
    }

    async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
        if self.should_fail {
            anyhow::bail!(self.error_message.clone());
        }
        anyhow::bail!("not implemented")
    }

    async fn release(&self, _id: &BeadId) -> Result<()> {
        if self.should_fail {
            anyhow::bail!(self.error_message.clone());
        }
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        if self.should_fail {
            anyhow::bail!(self.error_message.clone());
        }
        Ok(())
    }

    async fn reopen(&self, _id: &BeadId) -> Result<()> {
        if self.should_fail {
            anyhow::bail!(self.error_message.clone());
        }
        Ok(())
    }

    async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
        if self.should_fail {
            anyhow::bail!(self.error_message.clone());
        }
        Ok(vec![])
    }

    async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        if self.should_fail {
            anyhow::bail!(self.error_message.clone());
        }
        Ok(())
    }

    async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        if self.should_fail {
            anyhow::bail!(self.error_message.clone());
        }
        Ok(())
    }

    async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
        if self.should_fail {
            anyhow::bail!(self.error_message.clone());
        }
        Ok(BeadId::from("new-bead".to_string()))
    }

    async fn doctor_repair(&self) -> Result<RepairReport> {
        if self.should_fail {
            anyhow::bail!(self.error_message.clone());
        }
        Ok(RepairReport::default())
    }

    async fn doctor_check(&self) -> Result<RepairReport> {
        if self.should_fail {
            anyhow::bail!(self.error_message.clone());
        }
        Ok(RepairReport::default())
    }

    async fn full_rebuild(&self) -> Result<()> {
        if self.should_fail {
            anyhow::bail!(self.error_message.clone());
        }
        Ok(())
    }

    async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
        if self.should_fail {
            anyhow::bail!(self.error_message.clone());
        }
        Ok(())
    }

    async fn remove_dependency(&self, _blocked_id: &BeadId, _blocker_id: &BeadId) -> Result<()> {
        if self.should_fail {
            anyhow::bail!(self.error_message.clone());
        }
        Ok(())
    }
}

// ────────────────────────────────────────────────────────────────────────────────
// Utility Functions
// ────────────────────────────────────────────────────────────────────────────────

/// Count candidates by state.
pub fn count_candidates_by_state(candidates: &[MockCandidate]) -> (usize, usize, usize) {
    let mut assigned = 0;
    let mut excluded = 0;
    let mut claimable = 0;

    for candidate in candidates {
        match &candidate.state {
            CandidateState::Assigned { .. } => assigned += 1,
            CandidateState::Excluded { .. } => excluded += 1,
            CandidateState::Claimable => claimable += 1,
        }
    }

    (assigned, excluded, claimable)
}

/// Filter candidates by workspace.
pub fn candidates_for_workspace(
    candidates: &[MockCandidate],
    workspace: &std::path::Path,
) -> Vec<MockCandidate> {
    candidates
        .iter()
        .filter(|c| c.workspace == workspace)
        .cloned()
        .collect()
}

/// Filter candidates by state.
pub fn candidates_by_state(
    candidates: &[MockCandidate],
    state: &CandidateState,
) -> Vec<MockCandidate> {
    candidates
        .iter()
        .filter(|c| &c.state == state)
        .cloned()
        .collect()
}

/// Convert mock candidates to beads.
pub fn candidates_to_beads(candidates: &[MockCandidate]) -> Vec<Bead> {
    candidates.iter().map(|c| c.to_bead()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deadlock_scenario_structure() {
        let (workspaces, candidates, home) = deadlock_scenario();

        // Should have 2 workspaces
        assert_eq!(workspaces.len(), 2);

        // Home should not be in workspaces list
        assert!(!workspaces.contains(&home));

        // Should have candidates from both workspaces
        assert_eq!(candidates.len(), 6);

        let (assigned, excluded, claimable) = count_candidates_by_state(&candidates);
        assert_eq!(assigned, 2); // 2 assigned in ws1
        assert_eq!(excluded, 2); // 2 excluded in ws1
        assert_eq!(claimable, 2); // 2 claimable in ws2
    }

    #[test]
    fn test_all_dead_scenario() {
        let (workspaces, candidates, _home) = all_dead_scenario();

        assert_eq!(workspaces.len(), 2);

        let (assigned, excluded, claimable) = count_candidates_by_state(&candidates);
        assert_eq!(assigned, 1);
        assert_eq!(excluded, 1);
        assert_eq!(claimable, 0); // No claimable candidates
    }

    #[test]
    fn test_all_alive_scenario() {
        let (workspaces, candidates, _home) = all_alive_scenario();

        assert_eq!(workspaces.len(), 3);

        let (assigned, excluded, claimable) = count_candidates_by_state(&candidates);
        assert_eq!(assigned, 0);
        assert_eq!(excluded, 0);
        assert_eq!(claimable, 3); // All claimable
    }

    #[test]
    fn test_mixed_states_scenario() {
        let (workspaces, candidates, _home) = mixed_states_scenario();

        assert_eq!(workspaces.len(), 1);

        let (assigned, excluded, claimable) = count_candidates_by_state(&candidates);
        assert_eq!(assigned, 1);
        assert_eq!(excluded, 1);
        assert_eq!(claimable, 1);
    }

    #[test]
    fn test_scenario_builder() {
        let ws1 = test_workspace_path("ws1");
        let ws2 = test_workspace_path("ws2");

        let (workspaces, candidates) = ScenarioBuilder::new()
            .add_workspace(ws1.clone(), WorkspaceState::Dead)
            .add_workspace(ws2.clone(), WorkspaceState::Alive)
            .add_candidates(vec![
                MockCandidate::new("task1", "Task 1", CandidateState::Claimable, ws1.clone()),
                MockCandidate::new(
                    "task2",
                    "Task 2",
                    CandidateState::Assigned {
                        assignee: "worker".to_string(),
                    },
                    ws2.clone(),
                ),
            ])
            .build();

        assert_eq!(workspaces.len(), 2);
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn test_candidate_to_bead_conversion() {
        let ws = test_workspace_path("test");
        let candidate = MockCandidate::new(
            "test-id",
            "Test Title",
            CandidateState::Assigned {
                assignee: "worker".to_string(),
            },
            ws.clone(),
        );

        let bead = candidate.to_bead();

        assert_eq!(bead.id.as_ref(), "test-id");
        assert_eq!(bead.title, "Test Title");
        assert_eq!(bead.assignee, Some("worker".to_string()));
        assert_eq!(bead.workspace, ws);
    }

    #[test]
    fn test_filter_candidates() {
        let ws1 = test_workspace_path("ws1");
        let ws2 = test_workspace_path("ws2");

        let candidates = vec![
            MockCandidate::new("ws1-1", "Task 1", CandidateState::Claimable, ws1.clone()),
            MockCandidate::new("ws2-1", "Task 2", CandidateState::Claimable, ws2.clone()),
            MockCandidate::new("ws1-2", "Task 3", CandidateState::Claimable, ws1.clone()),
        ];

        let ws1_candidates = candidates_for_workspace(&candidates, &ws1);
        assert_eq!(ws1_candidates.len(), 2);

        let ws2_candidates = candidates_for_workspace(&candidates, &ws2);
        assert_eq!(ws2_candidates.len(), 1);
    }

    #[tokio::test]
    async fn test_mock_candidate_store() {
        let candidates = vec![MockCandidate::new(
            "test",
            "Test",
            CandidateState::Claimable,
            test_workspace_path("ws"),
        )
        .to_bead()];

        let store = MockCandidateStore::new(candidates);
        assert!(store.ready(&Filters::default()).await.is_ok());
    }
}
