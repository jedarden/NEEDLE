//! Explore strand: multi-workspace bead discovery.
//!
//! When the home workspace has no work (Pluck returned NoWork) and
//! maintenance is clean (Mend returned NoWork), Explore searches
//! configured workspaces for claimable beads.
//!
//! Design constraints (from v1 lessons):
//! - **No filesystem scanning.** Workspaces must be explicitly configured.
//! - **No upward traversal.** Only configured paths are checked.
//! - **Static workspace list.** Read from config at boot, not re-evaluated.
//! - **No permanent relocation.** Workers process one bead then return home.

use std::path::{Path, PathBuf};

use crate::bead_store::{BeadStore, BrCliBeadStore, Filters};
use crate::config::ExploreConfig;
use crate::registry::Registry;
use crate::telemetry::Telemetry;
use crate::types::StrandResult;

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
}

impl ExploreStrand {
    /// Create a new ExploreStrand from config.
    ///
    /// The workspace list is captured at construction time and never re-read.
    pub fn new(
        config: ExploreConfig,
        home_workspace: PathBuf,
        registry: Registry,
        telemetry: Telemetry,
        qualified_id: String,
    ) -> Self {
        ExploreStrand {
            enabled: config.enabled,
            workspaces: config.workspaces,
            home_workspace,
            registry,
            telemetry,
            qualified_id,
        }
    }

    /// Check if a workspace path has a `.beads/` directory.
    fn has_beads_dir(workspace: &Path) -> bool {
        workspace.join(".beads").is_dir()
    }

    /// Create a BrCliBeadStore for a given workspace path.
    fn store_for_workspace(workspace: &Path) -> Result<BrCliBeadStore, anyhow::Error> {
        BrCliBeadStore::discover(workspace.to_path_buf())
    }
}

#[async_trait::async_trait]
impl super::Strand for ExploreStrand {
    fn name(&self) -> &str {
        "explore"
    }

    async fn evaluate(&self, _store: &dyn BeadStore) -> StrandResult {
        // If disabled or no workspaces configured, nothing to explore.
        if !self.enabled {
            let _ = self
                .telemetry
                .emit(crate::telemetry::EventKind::StrandSkipped {
                    strand_name: "explore".to_string(),
                    reason: "disabled".to_string(),
                });
            return StrandResult::NoWork;
        }
        if self.workspaces.is_empty() {
            let _ = self
                .telemetry
                .emit(crate::telemetry::EventKind::StrandSkipped {
                    strand_name: "explore".to_string(),
                    reason: "no_workspaces_configured".to_string(),
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
            let remote_store = match Self::store_for_workspace(workspace) {
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
                            &remote_store,
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
    use crate::types::{Bead, BeadId, ClaimResult};

    use anyhow::Result;

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn make_explore_config(enabled: bool, workspaces: Vec<PathBuf>) -> ExploreConfig {
        ExploreConfig {
            enabled,
            workspaces,
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
}
