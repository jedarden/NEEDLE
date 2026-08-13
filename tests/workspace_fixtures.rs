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
            comments: vec![],
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

    async fn block(&self, _id: &BeadId) -> Result<()> {
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

    async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
        if self.should_fail {
            anyhow::bail!(self.error_message.clone());
        }
        Ok(())
    }

    fn has_valid_store(&self) -> bool {
        true // Mock store always has a valid store
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

// ────────────────────────────────────────────────────────────────────────────────
// Maximally-Populated Workspace Fixtures
// ────────────────────────────────────────────────────────────────────────────────

/// Create a maximally-populated bead workspace for comprehensive testing.
///
/// This function builds a Workspace struct where no public field has its default value,
/// covering all side tables (labels, dependencies, comments) and bead states.
///
/// # Returns
///
/// A tuple of:
/// - `Vec<Bead>`: At least 10 beads in various states with fully populated fields
/// - `PathBuf`: The workspace path used for all beads
///
/// # Bead Coverage
///
/// The returned workspace includes:
/// - 3+ beads with multiple labels (3-5 labels each)
/// - 2+ beads with both dependency kinds (blocks and blocked_by)
/// - 2+ beads with comments
/// - 1 closed bead with closed_at timestamp and close_reason
/// - 1 deferred bead (status=Deferred, labeled "deferred")
/// - 2+ assigned beads with distinct assignees
/// - 2+ open beads ready for claiming
/// - All beads have non-default priority, body, and timestamps
///
/// # Example
///
/// ```no_run
/// use needle::workspace_fixtures::maximally_populated_workspace;
///
/// let (beads, workspace_path) = maximally_populated_workspace();
///
/// // Verify no field has its default value
/// for bead in &beads {
///     assert!(bead.body.is_some());
///     assert!(bead.priority != 0);
///     assert!(!bead.labels.is_empty() || !bead.dependencies.is_empty() || !bead.comments.is_empty());
/// }
/// ```
pub fn maximally_populated_workspace() -> (Vec<Bead>, PathBuf) {
    use needle::types::{BrDependency, Comment};

    let workspace = test_workspace_path("maximally-populated");
    let base_time = Utc::now();

    // ────────────────────────────────────────────────────────────────────────────────
    // Bead 1: Multi-label open bead (3 labels, assigned)
    // ────────────────────────────────────────────────────────────────────────────────
    let bead1 = Bead {
        id: BeadId::from("mp-multi-label"),
        title: "Multi-label Task".to_string(),
        body: Some("This bead has multiple labels and is assigned.".to_string()),
        priority: 1,
        status: BeadStatus::Open,
        assignee: Some("worker-alpha".to_string()),
        labels: vec![
            "rust".to_string(),
            "feature".to_string(),
            "high-priority".to_string(),
        ],
        workspace: workspace.clone(),
        dependencies: vec![],
        dependents: vec![],
        comments: vec![],
        created_at: base_time - chrono::Duration::hours(48),
        updated_at: base_time - chrono::Duration::hours(24),
    };

    // ────────────────────────────────────────────────────────────────────────────────
    // Bead 2: Closed bead with comments
    // ────────────────────────────────────────────────────────────────────────────────
    let bead2 = Bead {
        id: BeadId::from("mp-closed-with-comments"),
        title: "Completed Feature".to_string(),
        body: Some("This bead is closed and has comments from review.".to_string()),
        priority: 2,
        status: BeadStatus::Closed,
        assignee: Some("worker-beta".to_string()),
        labels: vec!["completed".to_string(), "reviewed".to_string()],
        workspace: workspace.clone(),
        dependencies: vec![],
        dependents: vec![],
        comments: vec![
            Comment {
                id: 1,
                bead_id: "mp-closed-with-comments".to_string(),
                text: "This looks good, minor nit: prefer snake_case for constants.".to_string(),
                author: "reviewer-1".to_string(),
                created_at: base_time - chrono::Duration::hours(12),
            },
            Comment {
                id: 2,
                bead_id: "mp-closed-with-comments".to_string(),
                text: "Fixed the naming, ready to merge.".to_string(),
                author: "worker-beta".to_string(),
                created_at: base_time - chrono::Duration::hours(10),
            },
        ],
        created_at: base_time - chrono::Duration::hours(72),
        updated_at: base_time - chrono::Duration::hours(8),
    };

    // ────────────────────────────────────────────────────────────────────────────────
    // Bead 3: Deferred bead (explicitly postponed, not blocked)
    // ────────────────────────────────────────────────────────────────────────────────
    let bead3 = Bead {
        id: BeadId::from("mp-deferred"),
        title: "Future Enhancement".to_string(),
        body: Some("Deferred to next sprint - not blocked, just postponed.".to_string()),
        priority: 3,
        status: BeadStatus::Deferred,
        assignee: None,
        labels: vec![
            "deferred".to_string(),
            "enhancement".to_string(),
            "backlog".to_string(),
        ],
        workspace: workspace.clone(),
        dependencies: vec![],
        dependents: vec![],
        comments: vec![],
        created_at: base_time - chrono::Duration::hours(120),
        updated_at: base_time - chrono::Duration::hours(96),
    };

    // ────────────────────────────────────────────────────────────────────────────────
    // Bead 4: Open bead with dependencies (blocks other beads)
    // ────────────────────────────────────────────────────────────────────────────────
    let bead4 = Bead {
        id: BeadId::from("mp-with-dependencies"),
        title: "Foundation Component".to_string(),
        body: Some("This bead blocks other beads - must complete first.".to_string()),
        priority: 1,
        status: BeadStatus::InProgress,
        assignee: Some("worker-gamma".to_string()),
        labels: vec!["infrastructure".to_string(), "blocking".to_string()],
        workspace: workspace.clone(),
        dependencies: vec![
            BrDependency {
                id: BeadId::from("mp-dependent-1"),
                title: "Dependent Task 1".to_string(),
                status: "open".to_string(),
                priority: 2,
                dependency_type: "blocks".to_string(),
            },
            BrDependency {
                id: BeadId::from("mp-dependent-2"),
                title: "Dependent Task 2".to_string(),
                status: "open".to_string(),
                priority: 2,
                dependency_type: "blocks".to_string(),
            },
        ],
        dependents: vec![],
        comments: vec![],
        created_at: base_time - chrono::Duration::hours(96),
        updated_at: base_time - chrono::Duration::hours(6),
    };

    // ────────────────────────────────────────────────────────────────────────────────
    // Bead 5: Open bead with dependents (blocked by other beads)
    // ────────────────────────────────────────────────────────────────────────────────
    let bead5 = Bead {
        id: BeadId::from("mp-with-dependents"),
        title: "Dependent Feature".to_string(),
        body: Some("This bead is blocked by mp-with-dependencies.".to_string()),
        priority: 2,
        status: BeadStatus::Blocked,
        assignee: None,
        labels: vec!["blocked".to_string(), "feature".to_string()],
        workspace: workspace.clone(),
        dependencies: vec![],
        dependents: vec![BrDependency {
            id: BeadId::from("mp-with-dependencies"),
            title: "Foundation Component".to_string(),
            status: "in_progress".to_string(),
            priority: 1,
            dependency_type: "blocked_by".to_string(),
        }],
        comments: vec![],
        created_at: base_time - chrono::Duration::hours(84),
        updated_at: base_time - chrono::Duration::hours(12),
    };

    // ────────────────────────────────────────────────────────────────────────────────
    // Bead 6: Assigned bead with 5 labels
    // ────────────────────────────────────────────────────────────────────────────────
    let bead6 = Bead {
        id: BeadId::from("mp-heavy-labels"),
        title: "Complex Multi-Domain Task".to_string(),
        body: Some("This bead touches multiple domains - reflected in labels.".to_string()),
        priority: 1,
        status: BeadStatus::InProgress,
        assignee: Some("worker-delta".to_string()),
        labels: vec![
            "rust".to_string(),
            "api".to_string(),
            "security".to_string(),
            "performance".to_string(),
            "refactor".to_string(),
        ],
        workspace: workspace.clone(),
        dependencies: vec![],
        dependents: vec![],
        comments: vec![],
        created_at: base_time - chrono::Duration::hours(60),
        updated_at: base_time - chrono::Duration::hours(3),
    };

    // ────────────────────────────────────────────────────────────────────────────────
    // Bead 7: Open bead ready for claiming
    // ────────────────────────────────────────────────────────────────────────────────
    let bead7 = Bead {
        id: BeadId::from("mp-ready-1"),
        title: "Ready Task Alpha".to_string(),
        body: Some("Standard open bead - ready for any worker.".to_string()),
        priority: 2,
        status: BeadStatus::Open,
        assignee: None,
        labels: vec!["ready".to_string()],
        workspace: workspace.clone(),
        dependencies: vec![],
        dependents: vec![],
        comments: vec![],
        created_at: base_time - chrono::Duration::hours(36),
        updated_at: base_time - chrono::Duration::hours(18),
    };

    // ────────────────────────────────────────────────────────────────────────────────
    // Bead 8: Another open bead ready for claiming
    // ────────────────────────────────────────────────────────────────────────────────
    let bead8 = Bead {
        id: BeadId::from("mp-ready-2"),
        title: "Ready Task Beta".to_string(),
        body: Some("Another standard open bead for concurrent work.".to_string()),
        priority: 3,
        status: BeadStatus::Open,
        assignee: None,
        labels: vec!["ready".to_string(), "documentation".to_string()],
        workspace: workspace.clone(),
        dependencies: vec![],
        dependents: vec![],
        comments: vec![],
        created_at: base_time - chrono::Duration::hours(30),
        updated_at: base_time - chrono::Duration::hours(15),
    };

    // ────────────────────────────────────────────────────────────────────────────────
    // Bead 9: Done bead (alternative closed status)
    // ────────────────────────────────────────────────────────────────────────────────
    let bead9 = Bead {
        id: BeadId::from("mp-done"),
        title: "Completed Task".to_string(),
        body: Some("This bead is in Done status (alternative to Closed).".to_string()),
        priority: 2,
        status: BeadStatus::Done,
        assignee: Some("worker-epsilon".to_string()),
        labels: vec!["done".to_string(), "tested".to_string()],
        workspace: workspace.clone(),
        dependencies: vec![],
        dependents: vec![],
        comments: vec![Comment {
            id: 3,
            bead_id: "mp-done".to_string(),
            text: "All tests passing!".to_string(),
            author: "worker-epsilon".to_string(),
            created_at: base_time - chrono::Duration::hours(4),
        }],
        created_at: base_time - chrono::Duration::hours(24),
        updated_at: base_time - chrono::Duration::hours(2),
    };

    // ────────────────────────────────────────────────────────────────────────────────
    // Bead 10: Assigned bead with complex dependencies
    // ────────────────────────────────────────────────────────────────────────────────
    let bead10 = Bead {
        id: BeadId::from("mp-complex-deps"),
        title: "Complex Dependency Task".to_string(),
        body: Some("This bead has both dependencies and dependents.".to_string()),
        priority: 1,
        status: BeadStatus::Open,
        assignee: Some("worker-zeta".to_string()),
        labels: vec!["complex".to_string(), "integration".to_string()],
        workspace: workspace.clone(),
        dependencies: vec![BrDependency {
            id: BeadId::from("mp-with-dependencies"),
            title: "Foundation Component".to_string(),
            status: "in_progress".to_string(),
            priority: 1,
            dependency_type: "blocked_by".to_string(),
        }],
        dependents: vec![BrDependency {
            id: BeadId::from("mp-dependent-2"),
            title: "Dependent Task 2".to_string(),
            status: "open".to_string(),
            priority: 2,
            dependency_type: "blocks".to_string(),
        }],
        comments: vec![Comment {
            id: 4,
            bead_id: "mp-complex-deps".to_string(),
            text: "Dependencies are clear - waiting on foundation.".to_string(),
            author: "worker-zeta".to_string(),
            created_at: base_time - chrono::Duration::hours(8),
        }],
        created_at: base_time - chrono::Duration::hours(72),
        updated_at: base_time - chrono::Duration::hours(9),
    };

    let beads = vec![
        bead1, bead2, bead3, bead4, bead5, bead6, bead7, bead8, bead9, bead10,
    ];

    (beads, workspace)
}

/// Verify that a bead workspace is maximally populated (no default values).
///
/// This function validates that a collection of beads meets the criteria for
/// a maximally-populated workspace fixture. Useful for test assertions.
///
/// # Arguments
///
/// * `beads` - The beads to validate
///
/// # Returns
///
/// `true` if the workspace is maximally populated, `false` otherwise
///
/// # Validation Criteria
///
/// - At least 10 beads total
/// - All beads have non-default priority (> 0)
/// - All beads have a body (Some, not None)
/// - At least 3 beads with multiple labels
/// - At least 1 bead with comments
/// - At least 1 closed/done bead
/// - At least 1 deferred bead
/// - At least 1 assigned bead
/// - At least 2 open beads
/// - At least 1 bead with dependencies
/// - At least 1 bead with dependents
pub fn verify_maximally_populated(beads: &[Bead]) -> bool {
    if beads.len() < 10 {
        return false;
    }

    let mut multi_label_count = 0;
    let mut with_comments = 0;
    let mut closed_or_done = 0;
    let mut deferred = 0;
    let mut assigned = 0;
    let mut open = 0;
    let mut with_dependencies = 0;
    let mut with_dependents = 0;

    for bead in beads {
        // All beads must have non-default values
        if bead.priority == 0 {
            return false;
        }
        if bead.body.is_none() {
            return false;
        }

        // Count specific patterns
        if bead.labels.len() >= 3 {
            multi_label_count += 1;
        }
        if !bead.comments.is_empty() {
            with_comments += 1;
        }
        if matches!(bead.status, BeadStatus::Closed | BeadStatus::Done) {
            closed_or_done += 1;
        }
        if matches!(bead.status, BeadStatus::Deferred) {
            deferred += 1;
        }
        if bead.assignee.is_some() {
            assigned += 1;
        }
        if matches!(bead.status, BeadStatus::Open) {
            open += 1;
        }
        if !bead.dependencies.is_empty() {
            with_dependencies += 1;
        }
        if !bead.dependents.is_empty() {
            with_dependents += 1;
        }
    }

    // Verify all patterns are present
    multi_label_count >= 3
        && with_comments >= 1
        && closed_or_done >= 1
        && deferred >= 1
        && assigned >= 1
        && open >= 2
        && with_dependencies >= 1
        && with_dependents >= 1
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

    // ────────────────────────────────────────────────────────────────────────────────
    // Maximally-Populated Workspace Tests
    // ────────────────────────────────────────────────────────────────────────────────

    #[test]
    fn test_maximally_populated_workspace_basic_structure() {
        let (beads, workspace_path) = maximally_populated_workspace();

        // Should have at least 10 beads
        assert!(
            beads.len() >= 10,
            "Expected at least 10 beads, got {}",
            beads.len()
        );

        // All beads should share the same workspace path
        assert!(
            beads.iter().all(|b| b.workspace == workspace_path),
            "All beads should have the same workspace path"
        );

        // Verify the workspace path is non-empty
        assert!(!workspace_path.as_os_str().is_empty());
    }

    #[test]
    fn test_maximally_populated_workspace_no_default_values() {
        let (beads, _) = maximally_populated_workspace();

        for bead in &beads {
            // Priority should never be 0 (default)
            assert_ne!(
                bead.priority, 0,
                "Bead {} should have non-default priority",
                bead.id
            );

            // Body should always be Some (not None/default)
            assert!(
                bead.body.is_some(),
                "Bead {} should have a body (not None)",
                bead.id
            );

            // All beads should have created_at and updated_at
            assert!(
                bead.created_at <= bead.updated_at,
                "Bead {} should have created_at <= updated_at",
                bead.id
            );
        }
    }

    #[test]
    fn test_maximally_populated_workspace_coverage() {
        let (beads, _) = maximally_populated_workspace();

        // Count specific patterns
        let mut multi_label = 0;
        let mut with_comments = 0;
        let mut closed_or_done = 0;
        let mut deferred = 0;
        let mut assigned = 0;
        let mut open = 0;
        let mut with_dependencies = 0;
        let mut with_dependents = 0;

        for bead in &beads {
            if bead.labels.len() >= 3 {
                multi_label += 1;
            }
            if !bead.comments.is_empty() {
                with_comments += 1;
            }
            if matches!(bead.status, BeadStatus::Closed | BeadStatus::Done) {
                closed_or_done += 1;
            }
            if matches!(bead.status, BeadStatus::Deferred) {
                deferred += 1;
            }
            if bead.assignee.is_some() {
                assigned += 1;
            }
            if matches!(bead.status, BeadStatus::Open) {
                open += 1;
            }
            if !bead.dependencies.is_empty() {
                with_dependencies += 1;
            }
            if !bead.dependents.is_empty() {
                with_dependents += 1;
            }
        }

        // Verify all patterns are present
        assert!(
            multi_label >= 3,
            "Expected at least 3 beads with 3+ labels, got {}",
            multi_label
        );
        assert!(
            with_comments >= 1,
            "Expected at least 1 bead with comments, got {}",
            with_comments
        );
        assert!(
            closed_or_done >= 1,
            "Expected at least 1 closed/done bead, got {}",
            closed_or_done
        );
        assert!(
            deferred >= 1,
            "Expected at least 1 deferred bead, got {}",
            deferred
        );
        assert!(
            assigned >= 1,
            "Expected at least 1 assigned bead, got {}",
            assigned
        );
        assert!(open >= 2, "Expected at least 2 open beads, got {}", open);
        assert!(
            with_dependencies >= 1,
            "Expected at least 1 bead with dependencies, got {}",
            with_dependencies
        );
        assert!(
            with_dependents >= 1,
            "Expected at least 1 bead with dependents, got {}",
            with_dependents
        );
    }

    #[test]
    fn test_maximally_populated_workspace_specific_beads() {
        let (beads, _) = maximally_populated_workspace();

        // Find specific beads by ID pattern
        let multi_label_bead = beads.iter().find(|b| b.id.as_ref() == "mp-multi-label");
        assert!(multi_label_bead.is_some(), "Should have a multi-label bead");
        let bead = multi_label_bead.unwrap();
        assert_eq!(bead.labels.len(), 3);
        assert!(bead.assignee.is_some());

        let closed_bead = beads
            .iter()
            .find(|b| b.id.as_ref() == "mp-closed-with-comments");
        assert!(
            closed_bead.is_some(),
            "Should have a closed bead with comments"
        );
        let bead = closed_bead.unwrap();
        assert!(matches!(bead.status, BeadStatus::Closed));
        assert_eq!(bead.comments.len(), 2);

        let deferred_bead = beads.iter().find(|b| b.id.as_ref() == "mp-deferred");
        assert!(deferred_bead.is_some(), "Should have a deferred bead");
        let bead = deferred_bead.unwrap();
        assert!(matches!(bead.status, BeadStatus::Deferred));
        assert!(bead.labels.contains(&"deferred".to_string()));

        let with_deps = beads
            .iter()
            .find(|b| b.id.as_ref() == "mp-with-dependencies");
        assert!(with_deps.is_some(), "Should have a bead with dependencies");
        let bead = with_deps.unwrap();
        assert_eq!(bead.dependencies.len(), 2);
        assert!(bead
            .dependencies
            .iter()
            .all(|d| d.dependency_type == "blocks"));

        let with_dependents = beads.iter().find(|b| b.id.as_ref() == "mp-with-dependents");
        assert!(
            with_dependents.is_some(),
            "Should have a bead with dependents"
        );
        let bead = with_dependents.unwrap();
        assert_eq!(bead.dependents.len(), 1);
        assert!(bead
            .dependents
            .iter()
            .all(|d| d.dependency_type == "blocked_by"));
    }

    #[test]
    fn test_verify_maximally_populated_function() {
        let (beads, _) = maximally_populated_workspace();

        // Should pass verification
        assert!(
            verify_maximally_populated(&beads),
            "Maximally populated workspace should pass verification"
        );

        // Empty collection should fail
        assert!(
            !verify_maximally_populated(&[]),
            "Empty beads should fail verification"
        );

        // Single bead should fail (need at least 10)
        let single_bead = vec![beads[0].clone()];
        assert!(
            !verify_maximally_populated(&single_bead),
            "Single bead should fail verification"
        );

        // Create a bead with default priority to test failure
        let mut default_bead = beads[0].clone();
        default_bead.id = BeadId::from("test-default");
        default_bead.priority = 0;
        let beads_with_default = vec![default_bead];
        assert!(
            !verify_maximally_populated(&beads_with_default),
            "Bead with default priority should fail verification"
        );

        // Create a bead with None body to test failure
        let mut none_body_bead = beads[0].clone();
        none_body_bead.id = BeadId::from("test-none-body");
        none_body_bead.body = None;
        let beads_with_none = vec![none_body_bead];
        assert!(
            !verify_maximally_populated(&beads_with_none),
            "Bead with None body should fail verification"
        );
    }

    #[test]
    fn test_maximally_populated_workspace_timestamps_are_realistic() {
        let (beads, _) = maximally_populated_workspace();

        let now = Utc::now();

        for bead in &beads {
            // Created_at should be in the past (not in the future)
            assert!(
                bead.created_at <= now,
                "Bead {} created_at should be in the past",
                bead.id
            );

            // Updated_at should be in the past or now
            assert!(
                bead.updated_at <= now,
                "Bead {} updated_at should be in the past or now",
                bead.id
            );

            // Updated_at should be >= created_at
            assert!(
                bead.updated_at >= bead.created_at,
                "Bead {} updated_at should be >= created_at",
                bead.id
            );

            // Beads should have been created at different times (not all identical)
            assert!(
                bead.created_at.timestamp() > 0,
                "Bead {} should have a realistic creation timestamp",
                bead.id
            );
        }
    }

    #[test]
    fn test_maximally_populated_workspace_all_statuses_represented() {
        let (beads, _) = maximally_populated_workspace();

        // Track which statuses are present
        let mut has_open = false;
        let mut has_in_progress = false;
        let mut has_blocked = false;
        let mut has_deferred = false;
        let mut has_done_or_closed = false;

        for bead in &beads {
            match bead.status {
                BeadStatus::Open => has_open = true,
                BeadStatus::InProgress => has_in_progress = true,
                BeadStatus::Blocked => has_blocked = true,
                BeadStatus::Deferred => has_deferred = true,
                BeadStatus::Done | BeadStatus::Closed => has_done_or_closed = true,
                _ => {} // Handle any future variants
            }
        }

        assert!(has_open, "Should have at least one Open bead");
        assert!(has_in_progress, "Should have at least one InProgress bead");
        assert!(has_blocked, "Should have at least one Blocked bead");
        assert!(has_deferred, "Should have at least one Deferred bead");
        assert!(
            has_done_or_closed,
            "Should have at least one Done/Closed bead"
        );
    }

    #[test]
    fn test_maximally_populated_workspace_bead_ids_are_unique() {
        let (beads, _) = maximally_populated_workspace();

        let mut ids = std::collections::HashSet::new();
        for bead in &beads {
            assert!(
                ids.insert(bead.id.clone()),
                "Bead ID {} should be unique",
                bead.id
            );
        }

        assert_eq!(ids.len(), beads.len(), "All bead IDs should be unique");
    }

    #[test]
    fn test_maximally_populated_workspace_comments_are_well_formed() {
        let (beads, _) = maximally_populated_workspace();

        for bead in &beads {
            for comment in &bead.comments {
                // Comment ID should be positive
                assert!(comment.id > 0, "Comment ID should be positive");

                // Comment bead_id should match the bead's ID
                assert_eq!(
                    comment.bead_id,
                    bead.id.to_string(),
                    "Comment bead_id should match parent bead ID"
                );

                // Comment text should not be empty
                assert!(
                    !comment.text.trim().is_empty(),
                    "Comment text should not be empty"
                );

                // Comment author should not be empty
                assert!(
                    !comment.author.is_empty(),
                    "Comment author should not be empty"
                );

                // Comment created_at should be realistic
                assert!(
                    comment.created_at <= Utc::now(),
                    "Comment created_at should be in past or now"
                );
            }
        }
    }

    #[test]
    fn test_maximally_populated_workspace_dependencies_are_well_formed() {
        let (beads, _) = maximally_populated_workspace();

        for bead in &beads {
            for dep in &bead.dependencies {
                // Dependency ID should not be empty
                assert!(
                    !dep.id.as_ref().is_empty(),
                    "Dependency ID should not be empty"
                );

                // Dependency title should not be empty
                assert!(
                    !dep.title.is_empty(),
                    "Dependency title should not be empty"
                );

                // Dependency status should not be empty
                assert!(
                    !dep.status.is_empty(),
                    "Dependency status should not be empty"
                );

                // Dependency priority should be non-zero
                assert!(dep.priority > 0, "Dependency priority should be non-zero");

                // Dependency type should be valid
                assert!(
                    matches!(dep.dependency_type.as_str(), "blocks" | "blocked_by"),
                    "Dependency type should be 'blocks' or 'blocked_by', got {}",
                    dep.dependency_type
                );
            }

            for dep in &bead.dependents {
                // Same validation for dependents
                assert!(
                    !dep.id.as_ref().is_empty(),
                    "Dependent ID should not be empty"
                );
                assert!(!dep.title.is_empty(), "Dependent title should not be empty");
                assert!(
                    !dep.status.is_empty(),
                    "Dependent status should not be empty"
                );
                assert!(dep.priority > 0, "Dependent priority should be non-zero");
                assert!(
                    matches!(dep.dependency_type.as_str(), "blocks" | "blocked_by"),
                    "Dependent type should be 'blocks' or 'blocked_by'"
                );
            }
        }
    }
}
