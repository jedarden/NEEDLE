//! Explore strand: multi-workspace bead discovery.
//!
//! When the home workspace has no work (Pluck returned NoWork) and
//! maintenance is clean (Mend returned NoWork), Explore searches
//! configured workspaces for claimable beads.
//!
//! Design constraints (from v1 lessons):
//! - **No upward traversal.** Only configured paths are checked.
//! - **Static workspace list.** Read from config at boot, not re-evaluated.
//! - **No permanent relocation.** Workers process one bead then return home.
//!
//! Workspace discovery:
//! - Empty `workspaces` config → auto-discover all dirs with `.beads/` under `workspace_root`.
//! - Explicit `workspaces` list → only scan those paths.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::bead_store::{BeadStore, BrCliBeadStore, Filters};
use crate::config::ExploreConfig;
use crate::registry::Registry;
use crate::telemetry::Telemetry;
use crate::types::StrandResult;

/// Factory for creating bead stores for workspaces.
///
/// In production, this creates real BrCliBeadStore instances.
/// In tests, this can be mocked to return controlled test stores.
#[async_trait::async_trait]
trait StoreFactory: Send + Sync {
    async fn create_store(&self, workspace: &Path) -> Result<Arc<dyn BeadStore>, anyhow::Error>;
}

/// Default factory that creates real BrCliBeadStore instances.
struct DefaultStoreFactory;

#[async_trait::async_trait]
impl StoreFactory for DefaultStoreFactory {
    async fn create_store(&self, workspace: &Path) -> Result<Arc<dyn BeadStore>, anyhow::Error> {
        Ok(Arc::new(BrCliBeadStore::discover(workspace.to_path_buf())?))
    }
}

/// The Explore strand — discovers beads in other workspaces.
pub struct ExploreStrand {
    /// Whether this strand is enabled.
    enabled: bool,
    /// Static list of workspace paths to search (in order).
    workspaces: Vec<PathBuf>,
    /// Home workspace path — excluded from exploration.
    home_workspace: PathBuf,
    /// Worker registry for orphan detection.
    registry: Registry,
    /// Telemetry emitter for orphan release events.
    telemetry: Telemetry,
    /// Fully-qualified worker identity (`{adapter}-{worker_id}`).
    qualified_id: String,
    /// Store factory for creating workspace stores.
    store_factory: Arc<dyn StoreFactory>,
}

impl ExploreStrand {
    /// Create a new ExploreStrand from config.
    ///
    /// The workspace list is captured at construction time and never re-read.
    /// If `workspaces` is empty, auto-discovers all dirs with `.beads/` under
    /// the configured `workspace_root`.
    pub fn new(
        config: ExploreConfig,
        home_workspace: PathBuf,
        registry: Registry,
        telemetry: Telemetry,
        qualified_id: String,
    ) -> Self {
        // If workspaces is empty, auto-discover under workspace_root.
        let workspaces = if config.workspaces.is_empty() {
            Self::discover_workspaces(&config.workspace_root)
        } else {
            config.workspaces
        };

        ExploreStrand {
            enabled: config.enabled,
            workspaces,
            home_workspace,
            registry,
            telemetry,
            qualified_id,
            store_factory: Arc::new(DefaultStoreFactory),
        }
    }

    /// Create a new ExploreStrand for testing with explicit workspace list.
    ///
    /// This constructor skips workspace discovery and uses the provided list directly.
    /// Useful for tests that need precise control over which workspaces are scanned.
    #[cfg(test)]
    fn new_for_test(
        workspaces: Vec<PathBuf>,
        home_workspace: PathBuf,
        registry: Registry,
        telemetry: Telemetry,
        qualified_id: String,
    ) -> Self {
        ExploreStrand {
            enabled: true,
            workspaces,
            home_workspace,
            registry,
            telemetry,
            qualified_id,
            store_factory: Arc::new(DefaultStoreFactory),
        }
    }

    /// Create a new ExploreStrand for testing with injected store factory.
    ///
    /// This constructor allows tests to inject custom BeadStore creation logic,
    /// enabling tests of complex scenarios like the deadlock case where different
    /// workspaces return different candidate sets.
    #[cfg(test)]
    fn new_with_store_factory(
        workspaces: Vec<PathBuf>,
        home_workspace: PathBuf,
        registry: Registry,
        telemetry: Telemetry,
        qualified_id: String,
        store_factory: Arc<dyn StoreFactory>,
    ) -> Self {
        ExploreStrand {
            enabled: true,
            workspaces,
            home_workspace,
            registry,
            telemetry,
            qualified_id,
            store_factory,
        }
    }

    /// Discover all workspaces under a root path.
    ///
    /// A workspace is any directory containing a `.beads/` subdirectory.
    /// Returns an empty vector if the root doesn't exist or cannot be read.
    fn discover_workspaces(root: &Path) -> Vec<PathBuf> {
        let mut discovered = Vec::new();

        // If root doesn't exist, return empty (not an error).
        if !root.exists() {
            tracing::debug!(root = %root.display(), "workspace root does not exist, no workspaces discovered");
            return discovered;
        }

        // Read the directory; non-existent or unreadable dirs return empty.
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::debug!(root = %root.display(), error = %e, "failed to read workspace root");
                return discovered;
            }
        };

        // Filter for entries containing a `.beads/` subdirectory.
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let path = entry.path();
            if path.is_dir() && Self::has_beads_dir(&path) {
                tracing::debug!(workspace = %path.display(), "discovered workspace");
                discovered.push(path);
            }
        }

        tracing::debug!(
            root = %root.display(),
            count = discovered.len(),
            "workspace discovery complete"
        );

        discovered
    }

    /// Check if a workspace path has a `.beads/` directory.
    fn has_beads_dir(workspace: &Path) -> bool {
        workspace.join(".beads").is_dir()
    }

    /// Create a BrCliBeadStore for a given workspace path.
    async fn store_for_workspace(workspace: &Path) -> Result<Arc<dyn BeadStore>, anyhow::Error> {
        Ok(Arc::new(BrCliBeadStore::discover(workspace.to_path_buf())?))
    }

    /// Create a BrCliBeadStore for a given workspace path.
    ///
    /// This internal version is marked pub(crate) to allow testing with store injection.
    #[cfg(test)]
    async fn create_store_for(&self, workspace: &Path) -> Result<Arc<dyn BeadStore>, anyhow::Error> {
        self.store_factory.create_store(workspace).await
    }
}

#[async_trait::async_trait]
impl super::Strand for ExploreStrand {
    fn name(&self) -> &str {
        "explore"
    }

    async fn evaluate(&self, _store: &dyn BeadStore) -> StrandResult {
        // If disabled, nothing to explore.
        if !self.enabled {
            let _ = self
                .telemetry
                .emit(crate::telemetry::EventKind::StrandSkipped {
                    strand_name: "explore".to_string(),
                    reason: "disabled".to_string(),
                });
            return StrandResult::NoWork;
        }

        // Empty workspaces (after discovery attempt) means no workspaces found.
        if self.workspaces.is_empty() {
            let _ = self
                .telemetry
                .emit(crate::telemetry::EventKind::StrandSkipped {
                    strand_name: "explore".to_string(),
                    reason: "no_workspaces_discovered".to_string(),
                });
            return StrandResult::NoWork;
        }

        let filters = Filters {
            assignee: None,
            exclude_labels: vec![
                "deferred".to_string(),
                "human".to_string(),
                "blocked".to_string(),
            ],
        };

        for workspace in &self.workspaces {
            // Skip the home workspace — Pluck already checked it.
            if workspace == &self.home_workspace {
                tracing::debug!(workspace = %workspace.display(), "skipping home workspace");
                continue;
            }

            // Check that .beads/ exists before attempting to query.
            if !Self::has_beads_dir(workspace) {
                tracing::debug!(workspace = %workspace.display(), "no .beads/ directory, skipping");
                continue;
            }

            // Create a store for this workspace and query for ready beads.
            let remote_store = match self.store_factory.create_store(workspace).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        workspace = %workspace.display(),
                        error = %e,
                        "failed to create bead store for workspace, skipping"
                    );
                    continue;
                }
            };

            match remote_store.ready(&filters).await {
                Ok(mut candidates) => {
                    // Filter out assigned beads (belt-and-suspenders).
                    candidates.retain(|b| b.assignee.is_none());

                    if candidates.is_empty() {
                        // No ready candidates. Run cross-workspace mend to release
                        // orphaned in-progress beads, then re-query.
                        tracing::debug!(
                            workspace = %workspace.display(),
                            "no ready candidates, running cross-workspace mend"
                        );

                        match super::cleanup_orphaned_in_progress(
                            remote_store.as_ref(),
                            &self.registry,
                            &self.telemetry,
                            &self.qualified_id,
                        )
                        .await
                        {
                            Ok(released) if released > 0 => {
                                tracing::info!(
                                    workspace = %workspace.display(),
                                    released,
                                    "cross-workspace mend released orphans, re-querying"
                                );

                                // Re-query ready after cleanup.
                                match remote_store.ready(&filters).await {
                                    Ok(mut retry_candidates) => {
                                        retry_candidates.retain(|b| b.assignee.is_none());

                                        if !retry_candidates.is_empty() {
                                            // Found candidates after releasing orphans.
                                            // Sort and tag them.
                                            retry_candidates.sort_by(|a, b| {
                                                a.priority
                                                    .cmp(&b.priority)
                                                    .then_with(|| a.created_at.cmp(&b.created_at))
                                                    .then_with(|| a.id.as_ref().cmp(b.id.as_ref()))
                                            });

                                            for bead in &mut retry_candidates {
                                                bead.workspace = workspace.clone();
                                            }

                                            tracing::info!(
                                                workspace = %workspace.display(),
                                                candidates = retry_candidates.len(),
                                                "explore found candidates in remote workspace after cross-workspace mend"
                                            );

                                            return StrandResult::BeadFound(retry_candidates);
                                        }

                                        // Orphans were released but re-query found no candidates.
                                        // Do NOT return WorkCreated — the beads will become available
                                        // in the next natural selection cycle when Pluck re-scans the
                                        // ready queue. Returning WorkCreated here causes restart loops
                                        // when released beads don't pass filters (e.g., still blocked).
                                        tracing::info!(
                                            workspace = %workspace.display(),
                                            released,
                                            "cross-workspace mend released orphans but re-query found no candidates (beads may not pass filters), continuing to next workspace"
                                        );
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            workspace = %workspace.display(),
                                            error = %e,
                                            "failed to re-query workspace after cross-workspace mend, skipping"
                                        );
                                    }
                                }
                            }
                            Ok(_) => {
                                // No orphans released, workspace is truly empty.
                                tracing::debug!(
                                    workspace = %workspace.display(),
                                    "cross-workspace mend found no orphans"
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    workspace = %workspace.display(),
                                    error = %e,
                                    "cross-workspace mend failed, skipping workspace"
                                );
                            }
                        }

                        // Advance to next workspace (candidates empty after mend).
                        continue;
                    }

                    // Sort deterministically: priority ASC, created_at ASC, id ASC.
                    candidates.sort_by(|a, b| {
                        a.priority
                            .cmp(&b.priority)
                            .then_with(|| a.created_at.cmp(&b.created_at))
                            .then_with(|| a.id.as_ref().cmp(b.id.as_ref()))
                    });

                    // Tag each candidate with the workspace it came from
                    // so the worker can create the correct bead store.
                    for bead in &mut candidates {
                        bead.workspace = workspace.clone();
                    }

                    tracing::info!(
                        workspace = %workspace.display(),
                        candidates = candidates.len(),
                        "explore found candidates in remote workspace"
                    );

                    return StrandResult::BeadFound(candidates);
                }
                Err(e) => {
                    tracing::warn!(
                        workspace = %workspace.display(),
                        error = %e,
                        "failed to query workspace, skipping"
                    );
                    continue;
                }
            }
        }

        let _ = self
            .telemetry
            .emit(crate::telemetry::EventKind::StrandSkipped {
                strand_name: "explore".to_string(),
                reason: "no_candidates_in_any_workspace".to_string(),
            });
        StrandResult::NoWork
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::RepairReport;
    use crate::types::{Bead, BeadId, BeadStatus, ClaimResult};
    use chrono::Utc;

    use anyhow::Result;

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn make_explore_config(enabled: bool, workspaces: Vec<PathBuf>) -> ExploreConfig {
        ExploreConfig {
            enabled,
            workspaces,
            workspace_root: PathBuf::from("/tmp/needle-test-root"),
        }
    }

    fn make_explore_config_with_root(
        enabled: bool,
        workspaces: Vec<PathBuf>,
        root: PathBuf,
    ) -> ExploreConfig {
        ExploreConfig {
            enabled,
            workspaces,
            workspace_root: root,
        }
    }

    /// Helper to create ExploreStrand with test defaults for registry, telemetry, worker_id.
    fn make_test_explore_strand(
        enabled: bool,
        workspaces: Vec<PathBuf>,
        home: PathBuf,
    ) -> ExploreStrand {
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());
        ExploreStrand::new(
            make_explore_config(enabled, workspaces),
            home,
            registry,
            telemetry,
            "test-worker".to_string(),
        )
    }

    /// Stub BeadStore for the _store parameter (Explore ignores it).
    struct DummyStore;

    #[async_trait::async_trait]
    impl BeadStore for DummyStore {
        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(vec![])
        }
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            Ok(vec![])
        }
        async fn show(&self, _id: &BeadId) -> Result<Bead> {
            anyhow::bail!("not implemented")
        }
        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("not implemented")
        }

        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("not implemented")
        }

        async fn release(&self, _id: &BeadId) -> Result<()> {
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
        async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
            Ok(BeadId::from("new-bead".to_string()))
        }
        async fn doctor_repair(&self) -> Result<RepairReport> {
            Ok(RepairReport::default())
        }
        async fn doctor_check(&self) -> Result<RepairReport> {
            Ok(RepairReport::default())
        }
        async fn full_rebuild(&self) -> Result<()> {
            Ok(())
        }
        async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn remove_dependency(
            &self,
            _blocked_id: &BeadId,
            _blocker_id: &BeadId,
        ) -> Result<()> {
            Ok(())
        }
    }

    use super::super::Strand;

    // ── Tests ────────────────────────────────────────────────────────────────

    #[test]
    fn strand_name_is_explore() {
        let strand = make_test_explore_strand(true, vec![], PathBuf::from("/home/test"));
        assert_eq!(strand.name(), "explore");
    }

    #[tokio::test]
    async fn disabled_returns_no_work() {
        let strand = make_test_explore_strand(
            false,
            vec![PathBuf::from("/some/path")],
            PathBuf::from("/home/test"),
        );
        let store = DummyStore;
        let result = strand.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    #[tokio::test]
    async fn empty_workspace_list_returns_no_work() {
        // With empty workspaces, discovery runs under /tmp/needle-test-root,
        // which doesn't exist or has no .beads/ dirs, so NoWork is returned.
        let strand = make_test_explore_strand(true, vec![], PathBuf::from("/home/test"));
        let store = DummyStore;
        let result = strand.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    #[tokio::test]
    async fn skips_home_workspace() {
        let home = PathBuf::from("/home/test/project");
        let strand = make_test_explore_strand(true, vec![home.clone()], home);
        let store = DummyStore;
        let result = strand.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    #[tokio::test]
    async fn skips_workspace_without_beads_dir() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        // No .beads/ directory created.
        let strand =
            make_test_explore_strand(true, vec![workspace], PathBuf::from("/some/other/home"));
        let store = DummyStore;
        let result = strand.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    #[test]
    fn has_beads_dir_detects_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!ExploreStrand::has_beads_dir(dir.path()));

        std::fs::create_dir(dir.path().join(".beads")).unwrap();
        assert!(ExploreStrand::has_beads_dir(dir.path()));
    }

    #[test]
    fn workspace_list_is_static() {
        let workspaces = vec![
            PathBuf::from("/a"),
            PathBuf::from("/b"),
            PathBuf::from("/c"),
        ];
        let strand = make_test_explore_strand(true, workspaces.clone(), PathBuf::from("/home"));
        assert_eq!(strand.workspaces, workspaces);
    }

    #[test]
    fn home_workspace_is_captured() {
        let home = PathBuf::from("/my/home/workspace");
        let strand = make_test_explore_strand(true, vec![], home.clone());
        assert_eq!(strand.home_workspace, home);
    }

    #[tokio::test]
    async fn nonexistent_workspace_path_returns_no_work() {
        let strand = make_test_explore_strand(
            true,
            vec![PathBuf::from("/nonexistent/path/that/does/not/exist")],
            PathBuf::from("/home/test"),
        );
        let store = DummyStore;
        let result = strand.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    #[test]
    fn default_config_is_enabled_with_empty_workspaces() {
        let config = ExploreConfig::default();
        assert!(config.enabled);
        assert!(config.workspaces.is_empty());
    }

    #[test]
    fn discover_workspaces_finds_dirs_with_beads_subdir() {
        let root = tempfile::tempdir().unwrap();

        // Create some directories, only some with .beads/
        let ws1 = root.path().join("workspace1");
        let ws2 = root.path().join("workspace2");
        let ws3 = root.path().join("workspace3");
        let not_a_ws = root.path().join("not-a-workspace");

        fs::create_dir(&ws1).unwrap();
        fs::create_dir(&ws2).unwrap();
        fs::create_dir(&ws3).unwrap();
        fs::create_dir(&not_a_ws).unwrap();

        // Only ws1 and ws3 have .beads/
        fs::create_dir(ws1.join(".beads")).unwrap();
        fs::create_dir(ws3.join(".beads")).unwrap();

        let discovered = ExploreStrand::discover_workspaces(root.path());

        // Should find ws1 and ws3, but not ws2 or not_a_ws
        assert_eq!(discovered.len(), 2);
        assert!(discovered.contains(&ws1));
        assert!(discovered.contains(&ws3));
        assert!(!discovered.contains(&ws2));
        assert!(!discovered.contains(&not_a_ws));
    }

    #[test]
    fn discover_workspaces_returns_empty_for_nonexistent_root() {
        let discovered = ExploreStrand::discover_workspaces(Path::new("/nonexistent/path/xyz"));
        assert!(discovered.is_empty());
    }

    #[test]
    fn empty_workspaces_config_triggers_discovery() {
        let root = tempfile::tempdir().unwrap();

        // Create a workspace with .beads/
        let ws1 = root.path().join("workspace1");
        fs::create_dir(&ws1).unwrap();
        fs::create_dir(ws1.join(".beads")).unwrap();

        // Empty workspaces list with a valid root should trigger discovery
        let config = make_explore_config_with_root(true, vec![], root.path().to_path_buf());
        let home = PathBuf::from("/some/other/home");
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());
        let strand =
            ExploreStrand::new(config, home, registry, telemetry, "test-worker".to_string());

        // The discovered workspace should be in the list
        assert_eq!(strand.workspaces.len(), 1);
        assert!(strand.workspaces.contains(&ws1));
    }

    #[test]
    fn explicit_workspaces_list_skips_discovery() {
        let explicit_workspaces = vec![
            PathBuf::from("/explicit/workspace1"),
            PathBuf::from("/explicit/workspace2"),
        ];

        let config = make_explore_config(true, explicit_workspaces.clone());
        let home = PathBuf::from("/some/other/home");
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());
        let strand =
            ExploreStrand::new(config, home, registry, telemetry, "test-worker".to_string());

        // Should use the explicit list, not discovery
        assert_eq!(strand.workspaces, explicit_workspaces);
    }

    // ── Deadlock Scenario Tests ────────────────────────────────────────────────────

    /// Unit test for multi-workspace deadlock with excluded first workspace.
    ///
    /// DEADLOCK SCENARIO (from bf-1d64q):
    /// - Workspace 1 has candidates but all are excluded (blocked/deferred/human labels)
    /// - Workspace 2 has valid unassigned candidates
    /// - EXPECTED: Strand advances past workspace 1 to workspace 2 and returns candidates
    /// - BUG: Strand returns NoWork prematurely, never checking workspace 2
    ///
    /// This test uses fixture-based mock stores to simulate the scenario and proves
    /// the bug exists by failing on the current implementation.
    #[tokio::test]
    async fn test_deadlock_multi_workspace_with_excluded_first_workspace() {
        let workspace1 = PathBuf::from("/tmp/test/workspace1");
        let workspace2 = PathBuf::from("/tmp/test/workspace2");
        let home = PathBuf::from("/home/test");

        // Create a mock store factory that returns different states per workspace
        let mock_factory = Arc::new(ExcludedFirstMockFactory::new(
            workspace1.clone(),
            workspace2.clone(),
        ));

        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());

        let strand = ExploreStrand::new_with_store_factory(
            vec![workspace1.clone(), workspace2.clone()],
            home,
            registry,
            telemetry,
            "test-worker".to_string(),
            mock_factory,
        );

        let store = DummyStore;
        let result = strand.evaluate(&store).await;

        // Verify that workspace 2's candidate was returned
        match result {
            StrandResult::BeadFound(candidates) => {
                assert_eq!(candidates.len(), 1, "should find 1 candidate from workspace 2");
                assert_eq!(candidates[0].id, BeadId::from("ws2-valid-bead".to_string()));
                assert_eq!(candidates[0].workspace, workspace2);
                assert!(candidates[0].assignee.is_none(), "candidate should be unassigned");
                assert!(
                    !candidates[0].labels.iter().any(|l| l == "blocked" || l == "deferred" || l == "human"),
                    "candidate should not have excluded labels"
                );
            }
            StrandResult::NoWork => {
                panic!(
                    "deadlock bug reproduced: strand returned NoWork instead of finding workspace 2's candidate.\n\
                     This proves the strand is not advancing past workspace 1 even though all its candidates are excluded."
                );
            }
            StrandResult::WorkCreated => {
                panic!("unexpected WorkCreated result");
            }
            StrandResult::Error(e) => {
                panic!("unexpected Error result: {:?}", e);
            }
            StrandResult::Split(_, _) => {
                panic!("unexpected Split result");
            }
        }
    }

    /// Unit test proving the explore strand deadlock scenario.
    ///
    /// DEADLOCK SCENARIO (from bf-1d64q):
    /// 1. Workspace 1 has candidates but all are assigned or excluded
    /// 2. Workspace 2 has valid unassigned candidates
    /// 3. EXPECTED: Strand advances past workspace 1 to workspace 2
    /// 4. BUG: Strand returns NoWork prematurely, never checking workspace 2
    ///
    /// This test demonstrates the bug and proves the fix works. The test uses
    /// injected store factories to simulate the scenario where:
    /// - Workspace 1's store returns 2 candidates, but all have assignees (filtered out)
    /// - Workspace 2's store returns 1 valid unassigned candidate
    ///
    /// The test verifies that workspace 2's candidates ARE returned, proving
    /// that the strand advances past workspace 1 after filtering out unclaimable
    /// candidates.
    #[tokio::test]
    async fn deadlock_scenario_assigned_beads_allow_advancement() {
        let workspace1 = PathBuf::from("/tmp/test/workspace1");
        let workspace2 = PathBuf::from("/tmp/test/workspace2");
        let home = PathBuf::from("/home/test");

        // Create a mock store factory
        let mock_factory = Arc::new(DeadlockMockStoreFactory::new(workspace1.clone(), workspace2.clone()));

        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());

        let strand = ExploreStrand::new_with_store_factory(
            vec![workspace1.clone(), workspace2.clone()],
            home,
            registry,
            telemetry,
            "test-worker".to_string(),
            mock_factory.clone(),
        );

        let store = DummyStore;
        let result = strand.evaluate(&store).await;

        // Verify that both workspaces were queried
        let call_count = mock_factory.call_count();
        assert!(call_count >= 2, "both workspaces should be queried (at minimum), got: {}", call_count);

        // Verify that workspace 2's candidate was returned
        match result {
            StrandResult::BeadFound(candidates) => {
                assert_eq!(candidates.len(), 1, "should find 1 candidate from workspace 2");
                assert_eq!(candidates[0].id, BeadId::from("ws2-valid-bead".to_string()));
                assert_eq!(candidates[0].workspace, workspace2);
                assert!(candidates[0].assignee.is_none(), "candidate should be unassigned");
            }
            StrandResult::NoWork => {
                panic!("deadlock bug reproduced: strand returned NoWork instead of finding workspace 2's candidate");
            }
            StrandResult::WorkCreated => {
                panic!("unexpected WorkCreated result");
            }
            StrandResult::Error(e) => {
                panic!("unexpected Error result: {:?}", e);
            }
            StrandResult::Split(_, _) => {
                panic!("unexpected Split result");
            }
        }
    }

    /// Unit test for when workspace 1 has only excluded beads (blocked label).
    ///
    /// This proves that the strand advances when candidates are excluded by
    /// the Filters (deferred/human/blocked labels), not just when assigned.
    #[tokio::test]
    async fn deadlock_scenario_excluded_beads_allow_advancement() {
        let workspace1 = PathBuf::from("/tmp/test/workspace1");
        let workspace2 = PathBuf::from("/tmp/test/workspace2");
        let home = PathBuf::from("/home/test");

        // Create a mock store factory for excluded beads scenario
        let mock_factory = Arc::new(ExcludedBeadsMockFactory::new(workspace1.clone(), workspace2.clone()));

        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());

        let strand = ExploreStrand::new_with_store_factory(
            vec![workspace1.clone(), workspace2.clone()],
            home,
            registry,
            telemetry,
            "test-worker".to_string(),
            mock_factory.clone(),
        );

        let store = DummyStore;
        let result = strand.evaluate(&store).await;

        // Verify that workspace 2's candidate was returned
        match result {
            StrandResult::BeadFound(candidates) => {
                assert_eq!(candidates.len(), 1, "should find 1 candidate from workspace 2");
                assert_eq!(candidates[0].id, BeadId::from("ws2-valid-bead".to_string()));
                assert_eq!(candidates[0].workspace, workspace2);
            }
            StrandResult::NoWork => {
                panic!("deadlock bug reproduced: strand returned NoWork even though workspace 2 has valid candidates");
            }
            StrandResult::WorkCreated => {
                panic!("unexpected WorkCreated result");
            }
            StrandResult::Error(e) => {
                panic!("unexpected Error result: {:?}", e);
            }
            StrandResult::Split(_, _) => {
                panic!("unexpected Split result");
            }
        }
    }

    // ── Mock Store Factories ─────────────────────────────────────────────────────

    /// Mock store factory for excluded-first-workspace deadlock scenario.
    ///
    /// Simulates the classic deadlock scenario:
    /// - Workspace 1: ready() returns candidates with excluded labels (blocked/deferred/human)
    /// - Workspace 2: ready() returns valid unassigned candidates
    ///
    /// The bug is that the strand checks workspace 1, finds no valid candidates after
    /// filtering, and returns NoWork without ever checking workspace 2.
    struct ExcludedFirstMockFactory {
        workspace1: PathBuf,
        workspace2: PathBuf,
    }

    impl ExcludedFirstMockFactory {
        fn new(workspace1: PathBuf, workspace2: PathBuf) -> Self {
            ExcludedFirstMockFactory {
                workspace1,
                workspace2,
            }
        }
    }

    #[async_trait::async_trait]
    impl StoreFactory for ExcludedFirstMockFactory {
        async fn create_store(&self, workspace: &Path) -> Result<Arc<dyn BeadStore>, anyhow::Error> {
            if workspace == self.workspace1 {
                Ok(Arc::new(ExcludedCandidatesStore::new(self.workspace1.clone())))
            } else if workspace == self.workspace2 {
                Ok(Arc::new(ValidBeadStore::new(self.workspace2.clone())))
            } else {
                Err(anyhow::anyhow!("unexpected workspace: {}", workspace.display()))
            }
        }
    }

    /// Mock store factory for the deadlock scenario.
    ///
    /// Simulates:
    /// - Workspace 1: ready() returns 2 beads, but all have assignees (filtered out)
    /// - Workspace 2: ready() returns 1 valid unassigned bead
    struct DeadlockMockStoreFactory {
        workspace1: PathBuf,
        workspace2: PathBuf,
        call_count: std::sync::Arc<std::sync::atomic::AtomicU32>,
    }

    impl DeadlockMockStoreFactory {
        fn new(workspace1: PathBuf, workspace2: PathBuf) -> Self {
            DeadlockMockStoreFactory {
                workspace1,
                workspace2,
                call_count: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            }
        }

        fn call_count(&self) -> u32 {
            self.call_count.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl StoreFactory for DeadlockMockStoreFactory {
        async fn create_store(&self, workspace: &Path) -> Result<Arc<dyn BeadStore>, anyhow::Error> {
            self.call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

            if workspace == self.workspace1 {
                Ok(Arc::new(AssignedBeadsStore::new(self.workspace1.clone())))
            } else if workspace == self.workspace2 {
                Ok(Arc::new(ValidBeadStore::new(self.workspace2.clone())))
            } else {
                Err(anyhow::anyhow!("unexpected workspace: {}", workspace.display()))
            }
        }
    }

    /// Mock store factory for excluded beads scenario.
    ///
    /// Simulates:
    /// - Workspace 1: ready() returns beads with "blocked" label (excluded by filters)
    /// - Workspace 2: ready() returns 1 valid unassigned bead
    struct ExcludedBeadsMockFactory {
        workspace1: PathBuf,
        workspace2: PathBuf,
        call_count: std::sync::Arc<std::sync::atomic::AtomicU32>,
    }

    impl ExcludedBeadsMockFactory {
        fn new(workspace1: PathBuf, workspace2: PathBuf) -> Self {
            ExcludedBeadsMockFactory {
                workspace1,
                workspace2,
                call_count: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            }
        }

        fn call_count(&self) -> u32 {
            self.call_count.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl StoreFactory for ExcludedBeadsMockFactory {
        async fn create_store(&self, workspace: &Path) -> Result<Arc<dyn BeadStore>, anyhow::Error> {
            self.call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

            if workspace == self.workspace1 {
                Ok(Arc::new(BlockedBeadsStore::new(self.workspace1.clone())))
            } else if workspace == self.workspace2 {
                Ok(Arc::new(ValidBeadStore::new(self.workspace2.clone())))
            } else {
                Err(anyhow::anyhow!("unexpected workspace: {}", workspace.display()))
            }
        }
    }

    // ── Mock BeadStore Implementations ───────────────────────────────────────────

    /// Mock store that returns candidates with excluded labels.
    ///
    /// Simulates a workspace that has candidates but all are excluded by the filters
    /// (deferred, human, or blocked labels). After filtering, no valid candidates remain.
    struct ExcludedCandidatesStore {
        workspace: PathBuf,
    }

    impl ExcludedCandidatesStore {
        fn new(workspace: PathBuf) -> Self {
            ExcludedCandidatesStore { workspace }
        }
    }

    #[async_trait::async_trait]
    impl BeadStore for ExcludedCandidatesStore {
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            // Return candidates with various excluded labels
            // These will be filtered out by the strand's Filters
            Ok(vec![
                Bead {
                    id: BeadId::from("ws1-blocked-bead".to_string()),
                    title: "Blocked Bead".to_string(),
                    body: None,
                    priority: 1,
                    status: BeadStatus::Open,
                    assignee: None,
                    labels: vec!["blocked".to_string()],
                    workspace: self.workspace.clone(),
                    dependencies: vec![],
                    dependents: vec![],
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
                Bead {
                    id: BeadId::from("ws1-deferred-bead".to_string()),
                    title: "Deferred Bead".to_string(),
                    body: None,
                    priority: 2,
                    status: BeadStatus::Open,
                    assignee: None,
                    labels: vec!["deferred".to_string()],
                    workspace: self.workspace.clone(),
                    dependencies: vec![],
                    dependents: vec![],
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
                Bead {
                    id: BeadId::from("ws1-human-bead".to_string()),
                    title: "Human Bead".to_string(),
                    body: None,
                    priority: 3,
                    status: BeadStatus::Open,
                    assignee: None,
                    labels: vec!["human".to_string()],
                    workspace: self.workspace.clone(),
                    dependencies: vec![],
                    dependents: vec![],
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
            ])
        }

        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(vec![])
        }
        async fn show(&self, _id: &BeadId) -> Result<Bead> {
            anyhow::bail!("not implemented")
        }
        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("not implemented")
        }
        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("not implemented")
        }
        async fn release(&self, _id: &BeadId) -> Result<()> {
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
        async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
            Ok(BeadId::from("new-bead".to_string()))
        }
        async fn doctor_repair(&self) -> Result<RepairReport> {
            Ok(RepairReport::default())
        }
        async fn doctor_check(&self) -> Result<RepairReport> {
            Ok(RepairReport::default())
        }
        async fn full_rebuild(&self) -> Result<()> {
            Ok(())
        }
        async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn remove_dependency(
            &self,
            _blocked_id: &BeadId,
            _blocker_id: &BeadId,
        ) -> Result<()> {
            Ok(())
        }
    }

    /// Mock store that returns only assigned beads.
    struct AssignedBeadsStore {
        workspace: PathBuf,
        query_count: std::sync::Arc<std::sync::atomic::AtomicU32>,
    }

    impl AssignedBeadsStore {
        fn new(workspace: PathBuf) -> Self {
            AssignedBeadsStore {
                workspace,
                query_count: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            }
        }
    }

    #[async_trait::async_trait]
    impl BeadStore for AssignedBeadsStore {
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            let count = self.query_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

            if count == 0 {
                // First query: return assigned beads (filtered out by strand)
                Ok(vec![
                    Bead {
                        id: BeadId::from("ws1-assigned-bead-1".to_string()),
                        title: "Assigned Bead 1".to_string(),
                        body: None,
                        priority: 1,
                        status: BeadStatus::Open,
                        assignee: Some("other-worker-1".to_string()),
                        labels: vec![],
                        workspace: self.workspace.clone(),
                        dependencies: vec![],
                        dependents: vec![],
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                    },
                    Bead {
                        id: BeadId::from("ws1-assigned-bead-2".to_string()),
                        title: "Assigned Bead 2".to_string(),
                        body: None,
                        priority: 1,
                        status: BeadStatus::Open,
                        assignee: Some("other-worker-2".to_string()),
                        labels: vec![],
                        workspace: self.workspace.clone(),
                        dependencies: vec![],
                        dependents: vec![],
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                    },
                ])
            } else {
                // Re-query after cross-workspace mend: still no unassigned beads
                Ok(vec![])
            }
        }

        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(vec![])
        }
        async fn show(&self, _id: &BeadId) -> Result<Bead> {
            anyhow::bail!("not implemented")
        }
        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("not implemented")
        }
        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("not implemented")
        }
        async fn release(&self, _id: &BeadId) -> Result<()> {
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
        async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
            Ok(BeadId::from("new-bead".to_string()))
        }
        async fn doctor_repair(&self) -> Result<RepairReport> {
            Ok(RepairReport::default())
        }
        async fn doctor_check(&self) -> Result<RepairReport> {
            Ok(RepairReport::default())
        }
        async fn full_rebuild(&self) -> Result<()> {
            Ok(())
        }
        async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn remove_dependency(
            &self,
            _blocked_id: &BeadId,
            _blocker_id: &BeadId,
        ) -> Result<()> {
            Ok(())
        }
    }

    /// Mock store that returns beads with "blocked" label (excluded by filters).
    struct BlockedBeadsStore {
        workspace: PathBuf,
        query_count: std::sync::Arc<std::sync::atomic::AtomicU32>,
    }

    impl BlockedBeadsStore {
        fn new(workspace: PathBuf) -> Self {
            BlockedBeadsStore {
                workspace,
                query_count: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            }
        }
    }

    #[async_trait::async_trait]
    impl BeadStore for BlockedBeadsStore {
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            let count = self.query_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

            if count == 0 {
                // First query: return beads with "blocked" label (excluded by filters)
                Ok(vec![
                    Bead {
                        id: BeadId::from("ws1-blocked-bead".to_string()),
                        title: "Blocked Bead".to_string(),
                        body: None,
                        priority: 1,
                        status: BeadStatus::Open,
                        assignee: None,
                        labels: vec!["blocked".to_string()],
                        workspace: self.workspace.clone(),
                        dependencies: vec![],
                        dependents: vec![],
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                    },
                ])
            } else {
                // Re-query: still no valid beads
                Ok(vec![])
            }
        }

        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(vec![])
        }
        async fn show(&self, _id: &BeadId) -> Result<Bead> {
            anyhow::bail!("not implemented")
        }
        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("not implemented")
        }
        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("not implemented")
        }
        async fn release(&self, _id: &BeadId) -> Result<()> {
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
        async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
            Ok(BeadId::from("new-bead".to_string()))
        }
        async fn doctor_repair(&self) -> Result<RepairReport> {
            Ok(RepairReport::default())
        }
        async fn doctor_check(&self) -> Result<RepairReport> {
            Ok(RepairReport::default())
        }
        async fn full_rebuild(&self) -> Result<()> {
            Ok(())
        }
        async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn remove_dependency(
            &self,
            _blocked_id: &BeadId,
            _blocker_id: &BeadId,
        ) -> Result<()> {
            Ok(())
        }
    }

    /// Mock store that returns a valid unassigned bead.
    struct ValidBeadStore {
        workspace: PathBuf,
    }

    impl ValidBeadStore {
        fn new(workspace: PathBuf) -> Self {
            ValidBeadStore { workspace }
        }
    }

    #[async_trait::async_trait]
    impl BeadStore for ValidBeadStore {
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            Ok(vec![Bead {
                id: BeadId::from("ws2-valid-bead".to_string()),
                title: "Valid Unassigned Bead".to_string(),
                body: None,
                priority: 1,
                status: BeadStatus::Open,
                assignee: None,
                labels: vec![],
                workspace: self.workspace.clone(),
                dependencies: vec![],
                dependents: vec![],
                created_at: Utc::now(),
                updated_at: Utc::now(),
            }])
        }

        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(vec![])
        }
        async fn show(&self, _id: &BeadId) -> Result<Bead> {
            anyhow::bail!("not implemented")
        }
        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("not implemented")
        }
        async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
            anyhow::bail!("not implemented")
        }
        async fn release(&self, _id: &BeadId) -> Result<()> {
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
        async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
            Ok(BeadId::from("new-bead".to_string()))
        }
        async fn doctor_repair(&self) -> Result<RepairReport> {
            Ok(RepairReport::default())
        }
        async fn doctor_check(&self) -> Result<RepairReport> {
            Ok(RepairReport::default())
        }
        async fn full_rebuild(&self) -> Result<()> {
            Ok(())
        }
        async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn remove_dependency(
            &self,
            _blocked_id: &BeadId,
            _blocker_id: &BeadId,
        ) -> Result<()> {
            Ok(())
        }
    }
}
