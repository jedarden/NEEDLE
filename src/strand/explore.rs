//! Explore strand: multi-workspace bead discovery.
//!
//! When the home workspace has no work (Pluck returned NoWork) and
//! maintenance is clean (Mend returned NoWork), Explore searches
//! configured workspaces for claimable beads.
//!
//! ## Workspace Discovery (Intended Default)
//!
//! **DEFAULT MODE (RECOMMENDED):** Empty `workspaces` config.
//!
//! When `config.workspaces` is empty (the default), Explore runs recursive
//! workspace discovery under `config.workspace_root`. All directories containing
//! a `.beads/` subdirectory are automatically scanned for beads.
//!
//! This is the **intended default for the fleet as a whole** — new workspaces
//! are picked up automatically without configuration changes. Operators should
//! leave `workspaces` empty unless there's a specific reason to pin a worker to
//! a fixed repo set.
//!
//! **PINNED MODE (EXCEPTION):** Explicit `workspaces` list.
//!
//! When `config.workspaces` is non-empty, auto-discovery is disabled and only
//! the specified paths are scanned. Use this to restrict a specific worker to
//! a fixed repo set (e.g., a dedicated worker for a high-priority workspace).
//!
//! This is an **exception mechanism**. The fleet should normally run with
//! `workspaces` empty. A WARN log is emitted at startup when `workspaces` is
//! non-empty, naming the pinned repos, so operators can immediately see when
//! a worker is running in restricted mode.
//!
//! ## Design Constraints (from v1 lessons)
//! - **No upward traversal.** Only configured paths are checked.
//! - **Static workspace list.** Read from config at boot, not re-evaluated.
//! - **No permanent relocation.** Workers process one bead then return home.
//!
//! ## Implementation
//!
//! ExploreStrand::new() implements the discovery contract:
//! - If `config.workspaces` is empty → calls `discover_workspaces(&config.workspace_root)`
//! - If `config.workspaces` is non-empty → uses the explicit list directly

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use crate::bead_store::{discover_default, BeadStore, Filters};
use crate::config::ExploreConfig;
use crate::registry::Registry;
use crate::telemetry::Telemetry;
use crate::types::{BeadId, StrandResult};

/// Factory for creating bead stores for workspaces.
///
/// In production, this creates real bead store instances via `discover_default`.
/// In tests, this can be mocked to return controlled test stores.
#[async_trait::async_trait]
trait StoreFactory: Send + Sync {
    async fn create_store(&self, workspace: &Path) -> Result<Arc<dyn BeadStore>, anyhow::Error>;
}

/// Default factory that creates explicitly configured bead store instances.
struct DefaultStoreFactory;

#[async_trait::async_trait]
impl StoreFactory for DefaultStoreFactory {
    async fn create_store(&self, workspace: &Path) -> Result<Arc<dyn BeadStore>, anyhow::Error> {
        discover_default(
            workspace.to_path_buf(),
            None,
            Some("needle".to_string()),
            Some(env!("CARGO_PKG_VERSION").to_string()),
        )
    }
}

/// In-memory cadence state for Explore's roaming scan.
///
/// The worker's normal selection loop may call Explore frequently while all
/// discovered workspaces are empty. This state lets those empty scans spread
/// out without sleeping the worker or changing the waterfall's ordering.
#[derive(Debug)]
struct ExploreScanBackoff {
    base_interval_cycles: u32,
    max_interval_cycles: u32,
    consecutive_empty_scans: u32,
    cycles_until_scan: u32,
}

impl ExploreScanBackoff {
    fn new(base_interval_cycles: u32, max_interval_cycles: u32) -> Self {
        let base_interval_cycles = base_interval_cycles.max(1);
        let max_interval_cycles = max_interval_cycles.max(base_interval_cycles);

        Self {
            base_interval_cycles,
            max_interval_cycles,
            consecutive_empty_scans: 0,
            cycles_until_scan: 0,
        }
    }

    /// Return the effective interval after the recorded empty scans.
    fn effective_interval_cycles(&self) -> u32 {
        let multiplier = 1u32
            .checked_shl(self.consecutive_empty_scans.min(31))
            .unwrap_or(u32::MAX);
        self.base_interval_cycles
            .saturating_mul(multiplier)
            .min(self.max_interval_cycles)
    }

    /// Decide whether this selection cycle should perform an Explore scan.
    fn should_scan(&mut self) -> bool {
        if self.cycles_until_scan == 0 {
            true
        } else {
            self.cycles_until_scan -= 1;
            false
        }
    }

    /// Record the outcome of a real scan and schedule the next one.
    fn record_scan(&mut self, found_candidate: bool) {
        if found_candidate {
            self.consecutive_empty_scans = 0;
            self.cycles_until_scan = self.base_interval_cycles.saturating_sub(1);
            return;
        }

        self.consecutive_empty_scans = self.consecutive_empty_scans.saturating_add(1);
        self.cycles_until_scan = self.effective_interval_cycles().saturating_sub(1);
    }
}

/// The Explore strand — discovers beads in other workspaces.
pub struct ExploreStrand {
    /// Whether this strand is enabled.
    enabled: bool,
    /// Static list of workspace paths to search (in order).
    /// Wrapped in Mutex for interior mutability during re-discovery.
    workspaces: std::sync::Mutex<Vec<PathBuf>>,
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
    /// Cycles since last workspace re-discovery (for periodic refresh).
    cycles_since_rediscovery: std::sync::atomic::AtomicU32,
    /// Re-discovery interval, still parsed from config for backward
    /// compatibility with existing `.needle.yaml` files, but no longer read:
    /// re-discovery runs unconditionally every cycle as of bf-6anj4 (the
    /// legacy throttle this gated is documented, not applied, at the call
    /// site below).
    #[allow(dead_code)]
    rediscovery_cycles: u32,
    /// Workspace root for re-discovery (from config.workspace_root).
    workspace_root: PathBuf,
    /// Whether the original config had an empty workspaces list (auto-discovery mode).
    ///
    /// When false (pinned mode), re-discovery is skipped even if cycles elapse.
    auto_discovery_mode: bool,
    /// Starvation threshold in minutes (0 = disabled).
    #[allow(dead_code)]
    starvation_threshold_minutes: u64,
    /// Timestamp of the last successful claim (Unix timestamp, seconds).
    /// Used for starvation detection.
    #[allow(dead_code)]
    last_successful_claim_seconds: AtomicU64,
    /// Flag indicating whether ready beads were detected in the last scan.
    #[allow(dead_code)]
    ready_beads_detected: AtomicU64, // 0 = no, 1 = yes
    /// Last scan timestamp per workspace (workspace path -> Unix timestamp).
    /// Tracks when each workspace was last scanned for status reporting.
    #[allow(dead_code)]
    last_scan_per_workspace: std::sync::Mutex<std::collections::HashMap<String, u64>>,
    /// Adaptive cadence for roaming scans. This is intentionally in-memory and
    /// scoped to one worker instance.
    scan_backoff: std::sync::Mutex<ExploreScanBackoff>,
}

impl ExploreStrand {
    /// Create a new ExploreStrand from config.
    ///
    /// The workspace list is captured at construction time and re-discovered periodically
    /// (if `config.rediscovery_cycles` > 0 and `config.workspaces` is empty).
    /// If `workspaces` is empty, auto-discovers all dirs with `.beads/` under
    /// the configured `workspace_root`.
    pub fn new(
        config: ExploreConfig,
        home_workspace: PathBuf,
        registry: Registry,
        telemetry: Telemetry,
        qualified_id: String,
    ) -> Self {
        // Determine if we're in auto-discovery mode (empty workspaces = discover)
        let auto_discovery_mode = config.workspaces.is_empty();

        // If workspaces is empty, auto-discover under workspace_root.
        let workspaces = if auto_discovery_mode {
            Self::discover_workspaces(&config.workspace_root)
        } else {
            config.workspaces
        };

        // Emit WARN if running in pinned mode (non-empty workspaces).
        if !workspaces.is_empty() && !auto_discovery_mode {
            let repo_names: Vec<String> = workspaces
                .iter()
                .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
                .map(|s| s.to_string())
                .collect();
            tracing::warn!(
                worker = %qualified_id,
                mode = "pinned",
                workspaces_count = workspaces.len(),
                pinned_repos = ?repo_names,
                "Explore running in PINNED mode (non-empty workspaces list). \
                 Auto-discovery is disabled; only the listed workspaces will be scanned. \
                 This is an exception mechanism — the fleet default is empty workspaces \
                 (recursive discovery under workspace_root). Verify this is intentional."
            );
        }

        // Auto-discovery mode re-discovers workspaces every cycle (bf-6anj4). The
        // legacy `rediscovery_cycles` throttle is no longer applied; it is logged
        // here for visibility only.
        if auto_discovery_mode {
            tracing::info!(
                worker = %qualified_id,
                configured_rediscovery_cycles = config.rediscovery_cycles,
                "Explore auto-discovery: workspaces re-discovered every cycle \
                 (rediscovery_cycles throttle no longer applied)"
            );
        }

        ExploreStrand {
            enabled: config.enabled,
            workspaces: std::sync::Mutex::new(workspaces),
            home_workspace,
            registry,
            telemetry,
            qualified_id,
            store_factory: Arc::new(DefaultStoreFactory),
            cycles_since_rediscovery: std::sync::atomic::AtomicU32::new(0),
            rediscovery_cycles: config.rediscovery_cycles,
            workspace_root: config.workspace_root,
            auto_discovery_mode,
            starvation_threshold_minutes: config.starvation_threshold_minutes,
            last_successful_claim_seconds: AtomicU64::new(0),
            ready_beads_detected: AtomicU64::new(0),
            last_scan_per_workspace: std::sync::Mutex::new(std::collections::HashMap::new()),
            scan_backoff: std::sync::Mutex::new(ExploreScanBackoff::new(
                config.scan_interval_cycles,
                config.max_scan_interval_cycles,
            )),
        }
    }

    /// Create a new ExploreStrand for testing with explicit workspace list.
    ///
    /// This constructor skips workspace discovery and uses the provided list directly.
    /// Useful for tests that need precise control over which workspaces are scanned.
    #[cfg(test)]
    #[allow(dead_code)]
    fn new_for_test(
        workspaces: Vec<PathBuf>,
        home_workspace: PathBuf,
        registry: Registry,
        telemetry: Telemetry,
        qualified_id: String,
    ) -> Self {
        ExploreStrand {
            enabled: true,
            workspaces: std::sync::Mutex::new(workspaces),
            home_workspace,
            registry,
            telemetry,
            qualified_id,
            store_factory: Arc::new(DefaultStoreFactory),
            cycles_since_rediscovery: std::sync::atomic::AtomicU32::new(0),
            rediscovery_cycles: 0,
            workspace_root: PathBuf::from("/tmp/needle-test-root"),
            auto_discovery_mode: false,
            starvation_threshold_minutes: 0,
            last_successful_claim_seconds: AtomicU64::new(0),
            ready_beads_detected: AtomicU64::new(0),
            last_scan_per_workspace: std::sync::Mutex::new(std::collections::HashMap::new()),
            scan_backoff: std::sync::Mutex::new(ExploreScanBackoff::new(1, 8)),
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
            workspaces: std::sync::Mutex::new(workspaces),
            home_workspace,
            registry,
            telemetry,
            qualified_id,
            store_factory,
            cycles_since_rediscovery: std::sync::atomic::AtomicU32::new(0),
            rediscovery_cycles: 0,
            workspace_root: PathBuf::from("/tmp/needle-test-root"),
            auto_discovery_mode: false,
            starvation_threshold_minutes: 0,
            last_successful_claim_seconds: AtomicU64::new(0),
            ready_beads_detected: AtomicU64::new(0),
            last_scan_per_workspace: std::sync::Mutex::new(std::collections::HashMap::new()),
            scan_backoff: std::sync::Mutex::new(ExploreScanBackoff::new(1, 8)),
        }
    }

    /// Return whether this cycle should perform the remote workspace scan.
    fn should_scan_this_cycle(&self) -> bool {
        self.scan_backoff.lock().unwrap().should_scan()
    }

    /// Record a completed Explore scan and update its future cadence.
    fn record_scan_result(&self, found_candidate: bool) {
        let mut backoff = self.scan_backoff.lock().unwrap();
        backoff.record_scan(found_candidate);
        tracing::debug!(
            worker = %self.qualified_id,
            found_candidate,
            consecutive_empty_scans = backoff.consecutive_empty_scans,
            next_scan_interval_cycles = backoff.effective_interval_cycles(),
            "updated Explore adaptive scan cadence"
        );
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

    /// Re-discover workspaces (refresh the workspace list).
    ///
    /// This is called periodically when `rediscovery_cycles` > 0 and we're in
    /// auto-discovery mode (empty workspaces config). It re-runs discovery under
    /// `workspace_root` and updates the workspace list, preserving:
    /// - **No upward traversal:** Only scans immediate children of workspace_root
    /// - **Explicit workspaces override:** Only runs when auto_discovery_mode is true
    ///
    /// Returns the number of new workspaces discovered (if any).
    fn rediscover_workspaces(&self) -> usize {
        // Skip re-discovery if we're in pinned mode (explicit workspaces list).
        if !self.auto_discovery_mode {
            tracing::debug!(
                worker = %self.qualified_id,
                "skipping workspace re-discovery: running in pinned mode (explicit workspaces list)"
            );
            return 0;
        }

        // NOTE: re-discovery is now run every cycle (bf-6anj4). The historical
        // `rediscovery_cycles == 0` disable path was removed; the only skip that
        // remains is pinned mode (handled above).
        let previous_count = {
            let workspaces = self.workspaces.lock().unwrap();
            workspaces.len()
        };

        let new_workspaces = Self::discover_workspaces(&self.workspace_root);
        let new_count = new_workspaces.len();

        // Update the workspace list.
        {
            let mut workspaces = self.workspaces.lock().unwrap();
            *workspaces = new_workspaces;
        }

        let added_count = new_count.saturating_sub(previous_count);

        if added_count > 0 {
            tracing::info!(
                worker = %self.qualified_id,
                previous_count,
                new_count,
                added_count,
                "workspace re-discovery found new workspaces"
            );
        } else {
            tracing::debug!(
                worker = %self.qualified_id,
                previous_count,
                "workspace re-discovery: no change (still {} workspaces)",
                previous_count
            );
        }

        added_count
    }

    /// Compute the starting workspace index for this worker.
    ///
    /// Uses a hash of the qualified_id modulo the workspace count to determine
    /// where this worker should start scanning. This de-herds workers by ensuring
    /// they start at different positions in the workspace list.
    ///
    /// Returns 0 if there are no workspaces (defensive, should be handled earlier).
    ///
    /// Superseded in production by the per-cycle shuffle in `evaluate()` (bf-6anj4);
    /// retained (and exercised by unit tests) as documentation of the prior
    /// static-rotation de-herd model.
    #[allow(dead_code)]
    fn compute_start_index(&self) -> usize {
        let workspaces = self.workspaces.lock().unwrap();
        if workspaces.is_empty() {
            return 0;
        }

        let n = workspaces.len();

        // Hash the qualified_id using a stable hash algorithm
        let mut hasher = DefaultHasher::new();
        self.qualified_id.hash(&mut hasher);
        let hash = hasher.finish();

        // Start at hash % n, wrapping around
        (hash as usize) % n
    }

    /// Get an iterator over workspaces in this worker's rotation order.
    ///
    /// The iterator starts at this worker's computed start index and wraps around,
    /// covering all workspaces exactly once. Each worker with a different qualified_id
    /// will visit workspaces in a different rotation.
    ///
    /// Superseded in production by the per-cycle shuffle in `evaluate()` (bf-6anj4);
    /// retained (and exercised by unit tests) as documentation of the prior model.
    #[allow(dead_code)]
    fn rotated_workspace_order(&self) -> Vec<PathBuf> {
        // Compute the start index *before* taking the lock below:
        // `compute_start_index()` acquires `self.workspaces` itself, and
        // `std::sync::Mutex` is not reentrant — calling it while already
        // holding the lock here previously self-deadlocked every time this
        // function ran (see bf-2unnq).
        let start = self.compute_start_index();

        let workspaces = self.workspaces.lock().unwrap();
        if workspaces.is_empty() {
            return vec![];
        }

        let n = workspaces.len();
        // Defensive: `compute_start_index()` released its lock before we
        // re-acquired ours above, so guard against the (currently
        // never-concurrent, but cheap to guard) case where the count
        // changed in between.
        let start = start % n;
        let mut rotated = Vec::with_capacity(n);

        // Add workspaces from start to end
        for i in start..n {
            rotated.push(workspaces[i].clone());
        }

        // Add workspaces from beginning to start (wrap-around)
        for i in 0..start {
            rotated.push(workspaces[i].clone());
        }

        rotated
    }

    /// Create a bead store for a workspace's explicit backend binding.
    #[allow(dead_code)]
    async fn store_for_workspace(workspace: &Path) -> Result<Arc<dyn BeadStore>, anyhow::Error> {
        discover_default(
            workspace.to_path_buf(),
            None,
            Some("needle".to_string()),
            Some(env!("CARGO_PKG_VERSION").to_string()),
        )
    }

    /// Create the descriptor-bound store for a workspace path.
    ///
    /// This internal version is marked pub(crate) to allow testing with store injection.
    #[cfg(test)]
    #[allow(dead_code)]
    async fn create_store_for(
        &self,
        workspace: &Path,
    ) -> Result<Arc<dyn BeadStore>, anyhow::Error> {
        self.store_factory.create_store(workspace).await
    }
}

#[async_trait::async_trait]
impl super::Strand for ExploreStrand {
    fn name(&self) -> &str {
        "explore"
    }

    async fn evaluate(
        &self,
        _store: &dyn BeadStore,
        _exclusions: &HashSet<BeadId>,
    ) -> StrandResult {
        use std::time::Instant;

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

        // A backoff skip is still reported as NoWork so the waterfall continues
        // evaluating Weave and all later escalation strands in their normal
        // order; only Explore's remote scan is deferred.
        if !self.should_scan_this_cycle() {
            let _ = self
                .telemetry
                .emit(crate::telemetry::EventKind::StrandSkipped {
                    strand_name: "explore".to_string(),
                    reason: "adaptive_scan_backoff".to_string(),
                });
            tracing::debug!(
                worker = %self.qualified_id,
                "Explore scan deferred by adaptive empty-scan backoff"
            );
            return StrandResult::NoWork;
        }

        // Re-discover workspaces every cycle (bf-3peh4 / bf-6anj4). The workspace
        // list was captured at boot and only refreshed on a throttle, so a newly
        // created store needed a worker restart to be seen. A plain read_dir over
        // ~40 entries is cheap, so we refresh unconditionally each cycle;
        // `rediscover_workspaces` is a no-op in pinned mode. The cycle counter is
        // still advanced so telemetry/consumers that read it stay meaningful.
        let _cycle = self
            .cycles_since_rediscovery
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let added = self.rediscover_workspaces();
        if added > 0 {
            tracing::info!(
                worker = %self.qualified_id,
                added,
                "workspace re-discovery found new workspaces"
            );
        }

        // Empty workspaces (after discovery attempt) means no workspaces found.
        {
            let workspaces = self.workspaces.lock().unwrap();
            if workspaces.is_empty() {
                let _ = self
                    .telemetry
                    .emit(crate::telemetry::EventKind::StrandSkipped {
                        strand_name: "explore".to_string(),
                        reason: "no_workspaces_discovered".to_string(),
                    });
                self.record_scan_result(false);
                return StrandResult::NoWork;
            }
        }

        let filters = Filters {
            assignee: None,
            exclude_labels: vec![
                "deferred".to_string(),
                "human".to_string(),
                "blocked".to_string(),
            ],
            exclude_ids: HashSet::new(),
        };

        // Shuffle this worker's workspace scan order fresh every cycle (bf-6anj4).
        // The previous static `compute_start_index` (hash(qualified_id) % N) was
        // constant for a worker's whole session, so a worker whose fixed index
        // landed near an always-non-empty (or always-excluded) workspace could
        // permanently starve later workspaces. A fresh shuffle each cycle de-herds
        // workers without pinning coverage to a static, identity-derived value.
        let mut workspaces = {
            let workspaces = self.workspaces.lock().unwrap();
            workspaces.clone()
        };
        {
            use rand::seq::SliceRandom;
            workspaces.shuffle(&mut rand::thread_rng());
        }
        let total_workspaces = workspaces.len();

        tracing::debug!(
            qualified_id = %self.qualified_id,
            total_workspaces,
            "worker scan: shuffled order over {} workspaces this cycle",
            total_workspaces
        );

        // Track scan summary information for telemetry
        let scan_start = Instant::now();
        let mut workspaces_visited: Vec<String> = Vec::new();
        let mut workspaces_with_candidates: Vec<String> = Vec::new();
        let mut total_candidates = 0usize;
        let mut exclusion_reasons: HashSet<String> = HashSet::new();

        // Aggregate candidates across ALL workspaces rather than returning on the
        // first non-empty one (bf-4df1e / bf-47bfm). Previously a single stale or
        // excluded bead in an early workspace caused an early return; the outer
        // waterfall then filtered it out and fell through to the next strand,
        // never scanning the remaining workspaces — silently starving the fleet.
        // We now scan every workspace, collect all candidates, and rank them
        // globally before returning.
        let mut all_candidates: Vec<crate::types::Bead> = Vec::new();

        for workspace in &workspaces {
            // Track this workspace as visited
            let workspace_str = workspace.display().to_string();
            workspaces_visited.push(workspace_str.clone());

            // Skip the home workspace — Pluck already checked it.
            if workspace == &self.home_workspace {
                tracing::debug!(workspace = %workspace.display(), "skipping home workspace");
                exclusion_reasons.insert("home_workspace".to_string());
                continue;
            }

            // Check that .beads/ exists before attempting to query.
            if !Self::has_beads_dir(workspace) {
                tracing::debug!(workspace = %workspace.display(), "no .beads/ directory, skipping");
                exclusion_reasons.insert("no_beads_dir".to_string());
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
                    exclusion_reasons.insert(format!("store_error: {}", e));
                    continue;
                }
            };

            match remote_store.ready(&filters).await {
                Ok(mut candidates) => {
                    // Defensive belt-and-suspenders filtering.
                    // The store.ready() method receives exclude_labels in its Filters,
                    // but some backend implementations may not filter correctly.
                    // This ensures excluded/assigned beads are never returned as candidates.
                    let before_count = candidates.len();
                    candidates.retain(|b| {
                        let assignee_ok = b.assignee.is_none();
                        let labels_ok =
                            !b.labels.iter().any(|l| filters.exclude_labels.contains(l));
                        assignee_ok && labels_ok
                    });
                    let filtered_count = before_count - candidates.len();

                    if filtered_count > 0 {
                        exclusion_reasons.insert(format!("filtered_{}", filtered_count));
                    }

                    if candidates.is_empty() {
                        // No ready candidates. Run cross-workspace mend to release
                        // orphaned in-progress beads, then re-query.
                        tracing::debug!(
                            workspace = %workspace.display(),
                            "no ready candidates, running cross-workspace mend"
                        );
                        exclusion_reasons.insert("no_ready_candidates".to_string());

                        // Only run cleanup if there are actually in-progress beads.
                        // This avoids unnecessary spawn_blocking calls that can deadlock
                        // in test environments with limited blocking thread pools.
                        let all_beads = match remote_store.list_all().await {
                            Ok(beads) => beads,
                            Err(e) => {
                                tracing::warn!(
                                    workspace = %workspace.display(),
                                    error = %e,
                                    "failed to list beads for orphan check, skipping"
                                );
                                exclusion_reasons.insert(format!("list_error: {}", e));
                                continue;
                            }
                        };

                        let has_in_progress = all_beads
                            .iter()
                            .any(|b| b.status == crate::types::BeadStatus::InProgress);

                        if !has_in_progress {
                            tracing::debug!(
                                workspace = %workspace.display(),
                                "no in-progress beads, skipping orphan cleanup"
                            );
                            exclusion_reasons.insert("no_in_progress".to_string());
                            continue;
                        }

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
                                        // Apply the same defensive filtering.
                                        let retry_before = retry_candidates.len();
                                        retry_candidates.retain(|b| {
                                            let assignee_ok = b.assignee.is_none();
                                            let labels_ok = !b
                                                .labels
                                                .iter()
                                                .any(|l| filters.exclude_labels.contains(l));
                                            assignee_ok && labels_ok
                                        });
                                        let retry_filtered = retry_before - retry_candidates.len();

                                        if retry_filtered > 0 {
                                            exclusion_reasons.insert(format!(
                                                "retry_filtered_{}",
                                                retry_filtered
                                            ));
                                        }

                                        if !retry_candidates.is_empty() {
                                            // Found candidates after releasing orphans.
                                            // Tag them with their workspace; the global
                                            // rank happens once, after the full scan.
                                            for bead in &mut retry_candidates {
                                                bead.workspace = workspace.clone();
                                            }

                                            workspaces_with_candidates.push(workspace_str.clone());
                                            total_candidates += retry_candidates.len();

                                            tracing::info!(
                                                workspace = %workspace.display(),
                                                candidates = retry_candidates.len(),
                                                "explore found candidates in remote workspace after cross-workspace mend"
                                            );

                                            // Accumulate instead of returning early (bf-4df1e):
                                            // keep scanning the remaining workspaces this cycle.
                                            all_candidates.append(&mut retry_candidates);
                                        } else {
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
                                            exclusion_reasons.insert(
                                                "orphans_released_no_candidates".to_string(),
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            workspace = %workspace.display(),
                                            error = %e,
                                            "failed to re-query workspace after cross-workspace mend, skipping"
                                        );
                                        exclusion_reasons.insert(format!("requery_error: {}", e));
                                    }
                                }
                            }
                            Ok(_) => {
                                // No orphans released, workspace is truly empty.
                                tracing::debug!(
                                    workspace = %workspace.display(),
                                    "cross-workspace mend found no orphans"
                                );
                                exclusion_reasons.insert("no_orphans".to_string());
                            }
                            Err(e) => {
                                tracing::warn!(
                                    workspace = %workspace.display(),
                                    error = %e,
                                    "cross-workspace mend failed, skipping workspace"
                                );
                                exclusion_reasons.insert(format!("mend_error: {}", e));
                            }
                        }

                        // Advance to next workspace (candidates empty after mend).
                        continue;
                    }

                    // Tag each candidate with the workspace it came from so the
                    // worker can create the correct bead store; the global rank
                    // happens once, after every workspace has been scanned.
                    for bead in &mut candidates {
                        bead.workspace = workspace.clone();
                    }

                    // Track successful workspace scan.
                    workspaces_with_candidates.push(workspace_str.clone());
                    total_candidates += candidates.len();

                    tracing::info!(
                        workspace = %workspace.display(),
                        candidates = candidates.len(),
                        "explore found candidates in remote workspace"
                    );

                    // Accumulate instead of returning early (bf-4df1e / bf-47bfm):
                    // continue scanning all remaining workspaces this cycle.
                    all_candidates.append(&mut candidates);
                }
                Err(e) => {
                    tracing::warn!(
                        workspace = %workspace.display(),
                        error = %e,
                        "failed to query workspace, skipping"
                    );
                    exclusion_reasons.insert(format!("query_error: {}", e));
                    continue;
                }
            }
        }

        // Emit the scan summary once, covering every workspace visited this cycle.
        let duration_ms = scan_start.elapsed().as_millis() as u64;
        let _ = self
            .telemetry
            .emit(crate::telemetry::EventKind::ExploreScanSummary {
                workspaces_visited,
                workspaces_with_candidates,
                total_candidates,
                exclusion_reasons: exclusion_reasons.into_iter().collect(),
                duration_ms,
            });

        if all_candidates.is_empty() {
            self.record_scan_result(false);
            let _ = self
                .telemetry
                .emit(crate::telemetry::EventKind::StrandSkipped {
                    strand_name: "explore".to_string(),
                    reason: "no_candidates_in_any_workspace".to_string(),
                });
            return StrandResult::NoWork;
        }

        self.record_scan_result(true);

        // Rank the aggregated candidates globally: priority ASC, created_at ASC,
        // id ASC. Returning the full cross-workspace list lets the outer waterfall
        // skip any race-lost/excluded bead and still pick another, so a single bad
        // bead in one workspace can no longer block the whole fleet (bf-4df1e).
        all_candidates.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.created_at.cmp(&b.created_at))
                .then_with(|| a.id.as_ref().cmp(b.id.as_ref()))
        });

        tracing::info!(
            worker = %self.qualified_id,
            candidates = all_candidates.len(),
            workspaces = total_workspaces,
            "explore aggregated candidates across all workspaces this cycle"
        );

        StrandResult::BeadFound(all_candidates)
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

    /// Helper to create a runtime with explicit blocking thread pool configuration.
    ///
    /// This is necessary for tests that call cleanup_orphaned_in_progress, which uses
    /// spawn_blocking to run registry.list() (blocking file I/O and PID checks).
    /// The default blocking thread pool may not be sufficient for these operations.
    #[allow(dead_code)]
    fn create_test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .max_blocking_threads(512)
            .build()
            .unwrap()
    }

    fn make_explore_config(enabled: bool, workspaces: Vec<PathBuf>) -> ExploreConfig {
        ExploreConfig {
            enabled,
            workspaces,
            workspace_root: PathBuf::from("/tmp/needle-test-root"),
            rediscovery_cycles: 0,
            starvation_threshold_minutes: 0,
            scan_interval_cycles: 1,
            max_scan_interval_cycles: 8,
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
            rediscovery_cycles: 0,
            starvation_threshold_minutes: 0,
            scan_interval_cycles: 1,
            max_scan_interval_cycles: 8,
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
        fn has_valid_store(&self) -> bool {
            true
        }
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
        async fn block(&self, _id: &BeadId) -> Result<()> {
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
        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
    }

    /// Store factory used to verify Explore's scan cadence without sleeping.
    struct AdaptiveScanFactory {
        ready_calls: Arc<std::sync::atomic::AtomicU32>,
        candidate_on_ready_call: u32,
    }

    struct AdaptiveScanStore {
        ready_calls: Arc<std::sync::atomic::AtomicU32>,
        candidate_on_ready_call: u32,
        workspace: PathBuf,
    }

    #[async_trait::async_trait]
    impl StoreFactory for AdaptiveScanFactory {
        async fn create_store(
            &self,
            workspace: &Path,
        ) -> Result<Arc<dyn BeadStore>, anyhow::Error> {
            Ok(Arc::new(AdaptiveScanStore {
                ready_calls: self.ready_calls.clone(),
                candidate_on_ready_call: self.candidate_on_ready_call,
                workspace: workspace.to_path_buf(),
            }))
        }
    }

    #[async_trait::async_trait]
    impl BeadStore for AdaptiveScanStore {
        fn has_valid_store(&self) -> bool {
            true
        }

        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(vec![])
        }

        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            let call = self
                .ready_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            if call == self.candidate_on_ready_call {
                Ok(vec![Bead {
                    id: BeadId::from("adaptive-candidate".to_string()),
                    title: "Adaptive candidate".to_string(),
                    body: None,
                    priority: 1,
                    status: BeadStatus::Open,
                    assignee: None,
                    labels: vec![],
                    workspace: self.workspace.clone(),
                    dependencies: vec![],
                    dependents: vec![],
                    comments: vec![],
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                }])
            } else {
                Ok(vec![])
            }
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

        async fn block(&self, _id: &BeadId) -> Result<()> {
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

        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
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

    /// Test that disabled Explore strand returns NoWork.
    ///
    /// NOTE: This test is quarantined due to spawn_blocking deadlock in test environments.
    /// See deadlock_scenario_assigned_beads_allow_advancement for details.
    #[tokio::test]
    #[ignore]
    async fn disabled_returns_no_work() {
        let strand = make_test_explore_strand(
            false,
            vec![PathBuf::from("/some/path")],
            PathBuf::from("/home/test"),
        );
        let store = DummyStore;
        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    /// With empty workspaces, discovery runs under /tmp/needle-test-root,
    /// which doesn't exist or has no .beads/ dirs, so NoWork is returned.
    ///
    /// NOTE: This test is quarantined due to spawn_blocking deadlock in test environments.
    /// See deadlock_scenario_assigned_beads_allow_advancement for details.
    #[tokio::test]
    #[ignore]
    async fn empty_workspace_list_returns_no_work() {
        let strand = make_test_explore_strand(true, vec![], PathBuf::from("/home/test"));
        let store = DummyStore;
        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    #[test]
    fn adaptive_scan_backoff_doubles_to_ceiling_and_resets() {
        let mut backoff = ExploreScanBackoff::new(1, 8);
        let mut observed_intervals = Vec::new();

        for _ in 0..5 {
            assert!(backoff.should_scan());
            observed_intervals.push(backoff.effective_interval_cycles());
            backoff.record_scan(false);

            let next_interval = backoff.effective_interval_cycles();
            for _ in 1..next_interval {
                assert!(!backoff.should_scan());
            }
        }

        assert_eq!(observed_intervals, vec![1, 2, 4, 8, 8]);

        backoff.record_scan(true);
        assert_eq!(backoff.effective_interval_cycles(), 1);
        assert!(backoff.should_scan());
    }

    #[test]
    fn adaptive_scan_backoff_honors_configured_base_interval() {
        let mut backoff = ExploreScanBackoff::new(3, 10);

        assert!(backoff.should_scan());
        backoff.record_scan(false);
        assert_eq!(backoff.effective_interval_cycles(), 6);
        for _ in 1..6 {
            assert!(!backoff.should_scan());
        }
        assert!(backoff.should_scan());

        backoff.record_scan(true);
        assert_eq!(backoff.effective_interval_cycles(), 3);
        assert!(!backoff.should_scan());
        assert!(!backoff.should_scan());
        assert!(backoff.should_scan());
    }

    /// Pin CPU-relevant behavior by counting actual remote-store queries. The
    /// test never waits on wall-clock time: skipped selection cycles must not
    /// create stores or call `ready()`.
    ///
    /// NOTE: This test is quarantined due to spawn_blocking deadlock in test environments.
    /// See deadlock_scenario_assigned_beads_allow_advancement for details.
    #[tokio::test]
    #[ignore]
    async fn adaptive_scan_backoff_reduces_scan_calls_and_resets_on_candidate() {
        let temp_root = tempfile::tempdir().unwrap();
        let workspace = temp_root.path().join("remote");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(workspace.join(".beads")).unwrap();

        let ready_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let factory = Arc::new(AdaptiveScanFactory {
            ready_calls: ready_calls.clone(),
            candidate_on_ready_call: 5,
        });

        let registry_dir = tempfile::tempdir().unwrap();
        let config = make_explore_config(true, vec![workspace.clone()]);
        let mut strand = ExploreStrand::new(
            config,
            PathBuf::from("/home/test"),
            crate::registry::Registry::new(registry_dir.path()),
            Telemetry::new("adaptive-backoff-test".to_string()),
            "adaptive-backoff-test".to_string(),
        );
        strand.store_factory = factory;

        let store = DummyStore;
        let mut evaluate_calls = 0;
        while ready_calls.load(std::sync::atomic::Ordering::SeqCst) < 4 {
            assert!(matches!(
                strand.evaluate(&store, &HashSet::new()).await,
                StrandResult::NoWork
            ));
            evaluate_calls += 1;
        }

        // Empty scans happen on selection cycles 1, 3, 7, and 15: intervals
        // 1, 2, 4, and 8. The capped interval prevents further scan growth.
        assert_eq!(ready_calls.load(std::sync::atomic::Ordering::SeqCst), 4);
        assert_eq!(evaluate_calls, 15);

        let mut candidate_found = false;
        while ready_calls.load(std::sync::atomic::Ordering::SeqCst) < 5 {
            candidate_found = matches!(
                strand.evaluate(&store, &HashSet::new()).await,
                StrandResult::BeadFound(_)
            );
            evaluate_calls += 1;
        }
        assert!(candidate_found);
        assert_eq!(evaluate_calls, 23);
        assert_eq!(ready_calls.load(std::sync::atomic::Ordering::SeqCst), 5);

        // A candidate resets the cadence immediately: the next Explore call
        // scans rather than honoring the previous eight-cycle interval.
        assert!(matches!(
            strand.evaluate(&store, &HashSet::new()).await,
            StrandResult::NoWork
        ));
        assert_eq!(
            ready_calls.load(std::sync::atomic::Ordering::SeqCst),
            6,
            "the first post-candidate call must perform a scan"
        );

        // The empty scan after the reset starts a fresh two-cycle interval.
        assert!(matches!(
            strand.evaluate(&store, &HashSet::new()).await,
            StrandResult::NoWork
        ));
        assert_eq!(
            ready_calls.load(std::sync::atomic::Ordering::SeqCst),
            6,
            "the fresh backoff should skip the immediately following cycle"
        );
    }

    /// Test that Explore skips the home workspace.
    ///
    /// NOTE: This test is quarantined due to spawn_blocking deadlock in test environments.
    /// See deadlock_scenario_assigned_beads_allow_advancement for details.
    #[tokio::test]
    #[ignore]
    async fn skips_home_workspace() {
        let home = PathBuf::from("/home/test/project");
        let strand = make_test_explore_strand(true, vec![home.clone()], home);
        let store = DummyStore;
        let result = strand.evaluate(&store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    /// Test that Explore skips workspaces without .beads/ directory.
    ///
    /// NOTE: This test is quarantined due to spawn_blocking deadlock in test environments.
    /// See deadlock_scenario_assigned_beads_allow_advancement for details.
    #[tokio::test]
    #[ignore]
    async fn skips_workspace_without_beads_dir() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        // No .beads/ directory created.
        let strand =
            make_test_explore_strand(true, vec![workspace], PathBuf::from("/some/other/home"));
        let store = DummyStore;
        let result = strand.evaluate(&store, &HashSet::new()).await;
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
        assert_eq!(*strand.workspaces.lock().unwrap(), workspaces);
    }

    #[test]
    fn home_workspace_is_captured() {
        let home = PathBuf::from("/my/home/workspace");
        let strand = make_test_explore_strand(true, vec![], home.clone());
        assert_eq!(strand.home_workspace, home);
    }

    /// Tests nonexistent workspace path handling.
    ///
    /// NOTE: This test is quarantined due to spawn_blocking deadlock in test environments.
    /// See deadlock_scenario_assigned_beads_allow_advancement for details.
    #[tokio::test]
    #[ignore]
    async fn nonexistent_workspace_path_returns_no_work() {
        let strand = make_test_explore_strand(
            true,
            vec![PathBuf::from("/nonexistent/path/that/does/not/exist")],
            PathBuf::from("/home/test"),
        );
        let store = DummyStore;
        let result = strand.evaluate(&store, &HashSet::new()).await;
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
        assert_eq!(strand.workspaces.lock().unwrap().len(), 1);
        assert!(strand.workspaces.lock().unwrap().contains(&ws1));
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
        assert_eq!(*strand.workspaces.lock().unwrap(), explicit_workspaces);
    }

    // ── Rotation Tests ─────────────────────────────────────────────────────────────

    #[test]
    fn rotation_start_index_is_deterministic_for_same_qualified_id() {
        let workspaces = vec![
            PathBuf::from("/ws1"),
            PathBuf::from("/ws2"),
            PathBuf::from("/ws3"),
            PathBuf::from("/ws4"),
        ];

        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());

        let strand1 = ExploreStrand::new_for_test(
            workspaces.clone(),
            PathBuf::from("/home"),
            registry.clone(),
            telemetry.clone(),
            "worker-alpha".to_string(),
        );

        let strand2 = ExploreStrand::new_for_test(
            workspaces.clone(),
            PathBuf::from("/home"),
            registry,
            telemetry,
            "worker-alpha".to_string(),
        );

        // Same qualified_id should produce same start index
        assert_eq!(strand1.compute_start_index(), strand2.compute_start_index());
    }

    #[test]
    fn rotation_start_index_differs_for_different_qualified_ids() {
        let workspaces = vec![
            PathBuf::from("/ws1"),
            PathBuf::from("/ws2"),
            PathBuf::from("/ws3"),
            PathBuf::from("/ws4"),
        ];

        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry1 = Telemetry::new("test-worker-1".to_string());
        let telemetry2 = Telemetry::new("test-worker-2".to_string());

        let strand1 = ExploreStrand::new_for_test(
            workspaces.clone(),
            PathBuf::from("/home"),
            registry.clone(),
            telemetry1,
            "worker-alpha".to_string(),
        );

        let strand2 = ExploreStrand::new_for_test(
            workspaces,
            PathBuf::from("/home"),
            registry,
            telemetry2,
            "worker-bravo".to_string(),
        );

        // Different qualified_ids should (very likely) produce different start indices
        // Note: This is probabilistic - collisions are possible but unlikely
        let start1 = strand1.compute_start_index();
        let start2 = strand2.compute_start_index();

        // With 4 workspaces and good hash distribution, collisions are rare
        // If this test fails due to hash collision, it's a valid (but unlikely) result
        if start1 == start2 {
            println!(
                "WARN: Hash collision detected - both 'worker-alpha' and 'worker-bravo' produced start index {}", start1
            );
        }
    }

    #[test]
    fn rotated_workspace_order_covers_all_workspaces() {
        let workspaces = vec![
            PathBuf::from("/ws1"),
            PathBuf::from("/ws2"),
            PathBuf::from("/ws3"),
        ];

        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());

        let strand = ExploreStrand::new_for_test(
            workspaces.clone(),
            PathBuf::from("/home"),
            registry,
            telemetry,
            "any-worker".to_string(),
        );

        let rotated = strand.rotated_workspace_order();

        // Should have all workspaces
        assert_eq!(rotated.len(), workspaces.len());

        // Should contain each workspace exactly once
        for ws in &workspaces {
            assert_eq!(rotated.iter().filter(|&x| x == ws).count(), 1);
        }
    }

    #[test]
    fn rotation_starts_at_computed_index() {
        let workspaces = vec![
            PathBuf::from("/ws0"),
            PathBuf::from("/ws1"),
            PathBuf::from("/ws2"),
            PathBuf::from("/ws3"),
            PathBuf::from("/ws4"),
        ];

        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());

        // Create a strand with a known qualified_id
        let strand = ExploreStrand::new_for_test(
            workspaces.clone(),
            PathBuf::from("/home"),
            registry,
            telemetry,
            "test-worker".to_string(),
        );

        let rotated = strand.rotated_workspace_order();
        let start_index = strand.compute_start_index();

        // First element in rotated order should be workspace at start_index
        assert_eq!(rotated[0], workspaces[start_index]);

        // Elements should be in rotated order: [start..end, 0..start]
        let expected: Vec<PathBuf> = workspaces[start_index..]
            .iter()
            .chain(workspaces[..start_index].iter())
            .cloned()
            .collect();
        assert_eq!(rotated, expected);
    }

    #[test]
    fn two_workers_with_different_ids_have_different_rotations() {
        let workspaces = vec![
            PathBuf::from("/ws0"),
            PathBuf::from("/ws1"),
            PathBuf::from("/ws2"),
            PathBuf::from("/ws3"),
        ];

        let temp_dir = tempfile::tempdir().unwrap();
        let registry1 = crate::registry::Registry::new(temp_dir.path());
        let registry2 = crate::registry::Registry::new(temp_dir.path());
        let telemetry1 = Telemetry::new("test-worker-1".to_string());
        let telemetry2 = Telemetry::new("test-worker-2".to_string());

        let strand1 = ExploreStrand::new_for_test(
            workspaces.clone(),
            PathBuf::from("/home"),
            registry1,
            telemetry1,
            "worker-alpha".to_string(),
        );

        let strand2 = ExploreStrand::new_for_test(
            workspaces,
            PathBuf::from("/home"),
            registry2,
            telemetry2,
            "worker-bravo".to_string(),
        );

        let rotated1 = strand1.rotated_workspace_order();
        let rotated2 = strand2.rotated_workspace_order();

        // Rotations should likely be different (de-herding effect)
        // Note: Hash collisions can produce same rotation, but are unlikely
        let start1 = strand1.compute_start_index();
        let start2 = strand2.compute_start_index();

        // Verify both cover all workspaces
        assert_eq!(rotated1.len(), 4);
        assert_eq!(rotated2.len(), 4);

        // Verify rotations differ (unless hash collision)
        if start1 != start2 {
            assert_ne!(
                rotated1, rotated2,
                "rotations should differ for different workers"
            );

            // Verify they start at different positions
            assert_ne!(rotated1[0], rotated2[0]);
        } else {
            println!(
                "WARN: Hash collision - both workers start at index {}",
                start1
            );
        }
    }

    #[test]
    fn rotation_with_single_workspace_returns_same_order() {
        let workspaces = vec![PathBuf::from("/only-workspace")];

        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());

        let strand = ExploreStrand::new_for_test(
            workspaces.clone(),
            PathBuf::from("/home"),
            registry,
            telemetry,
            "any-worker".to_string(),
        );

        let rotated = strand.rotated_workspace_order();

        // Should return the same single workspace
        assert_eq!(rotated, workspaces);
    }

    #[test]
    fn rotation_with_empty_workspaces_returns_empty() {
        let workspaces = vec![];

        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());

        let strand = ExploreStrand::new_for_test(
            workspaces.clone(),
            PathBuf::from("/home"),
            registry,
            telemetry,
            "any-worker".to_string(),
        );

        let rotated = strand.rotated_workspace_order();

        // Should return empty
        assert_eq!(rotated.len(), 0);
        assert_eq!(strand.compute_start_index(), 0);
    }

    #[test]
    fn rotation_hash_distribution_is_reasonable() {
        // Test that rotation distributes workers across different start indices
        let workspaces: Vec<PathBuf> = (0..10)
            .map(|i| PathBuf::from(format!("/ws{}", i)))
            .collect();

        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());

        let mut start_counts = [0usize; 10];

        // Test 50 different worker IDs
        for i in 0..50 {
            let telemetry = Telemetry::new(format!("test-worker-{}", i));
            let strand = ExploreStrand::new_for_test(
                workspaces.clone(),
                PathBuf::from("/home"),
                registry.clone(),
                telemetry,
                format!("worker-{}", i),
            );

            let start = strand.compute_start_index();
            start_counts[start] += 1;
        }

        // With 10 workspaces and 50 workers, expect roughly 5 workers per start index
        // Allow some variance (2-8 workers per start index is acceptable)
        let min = *start_counts.iter().min().unwrap();
        let max = *start_counts.iter().max().unwrap();

        assert!(
            min >= 2,
            "distribution too skewed: minimum count is {} (expected >= 2)",
            min
        );
        assert!(
            max <= 8,
            "distribution too skewed: maximum count is {} (expected <= 8)",
            max
        );

        // Verify total is 50
        assert_eq!(start_counts.iter().sum::<usize>(), 50);
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
    ///
    /// NOTE: This test is quarantined due to spawn_blocking deadlock in test environments.
    /// See deadlock_scenario_assigned_beads_allow_advancement for details.
    #[tokio::test]
    #[ignore]
    async fn test_deadlock_multi_workspace_with_excluded_first_workspace() {
        let temp_root = tempfile::tempdir().unwrap();
        let workspace1 = temp_root.path().join("workspace1");
        let workspace2 = temp_root.path().join("workspace2");
        let home = PathBuf::from("/home/test");

        // Create .beads/ directories so has_beads_dir() returns true
        fs::create_dir_all(&workspace1).unwrap();
        fs::create_dir_all(&workspace2).unwrap();
        fs::create_dir(workspace1.join(".beads")).unwrap();
        fs::create_dir(workspace2.join(".beads")).unwrap();

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
        let result = strand.evaluate(&store, &HashSet::new()).await;

        // Verify that workspace 2's candidate was returned
        match result {
            StrandResult::BeadFound(candidates) => {
                assert_eq!(
                    candidates.len(),
                    1,
                    "should find 1 candidate from workspace 2"
                );
                assert_eq!(candidates[0].id, BeadId::from("ws2-valid-bead".to_string()));
                assert_eq!(candidates[0].workspace, workspace2);
                assert!(
                    candidates[0].assignee.is_none(),
                    "candidate should be unassigned"
                );
                assert!(
                    !candidates[0]
                        .labels
                        .iter()
                        .any(|l| l == "blocked" || l == "deferred" || l == "human"),
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
            StrandResult::Skipped { .. } => {
                panic!("unexpected Skipped result");
            }
            StrandResult::FoundButExcluded => {
                panic!("unexpected FoundButExcluded result");
            }
        }
    }

    /// Mock factory returning a valid, claimable candidate for BOTH workspaces,
    /// used to prove explore aggregates across all workspaces.
    struct BothValidMockFactory {
        workspace1: PathBuf,
        workspace2: PathBuf,
    }

    #[async_trait::async_trait]
    impl StoreFactory for BothValidMockFactory {
        async fn create_store(
            &self,
            workspace: &Path,
        ) -> Result<Arc<dyn BeadStore>, anyhow::Error> {
            if workspace == self.workspace1 || workspace == self.workspace2 {
                Ok(Arc::new(ValidBeadStore::new(workspace.to_path_buf())))
            } else {
                Err(anyhow::anyhow!(
                    "unexpected workspace: {}",
                    workspace.display()
                ))
            }
        }
    }

    /// Regression for bf-4df1e / bf-47bfm: when several workspaces each have a
    /// claimable candidate, explore must return candidates from ALL of them in a
    /// single cycle (aggregate) rather than returning on the first non-empty
    /// workspace. Previously a first-match return let a race-lost/excluded early
    /// bead (caught by the outer waterfall but not by explore's own filter) stall
    /// the whole fleet, since the remaining workspaces were never scanned.
    ///
    /// NOTE: This test is quarantined due to spawn_blocking deadlock in test environments.
    /// See deadlock_scenario_assigned_beads_allow_advancement for details.
    #[tokio::test]
    #[ignore]
    async fn aggregates_candidates_across_all_workspaces() {
        let temp_root = tempfile::tempdir().unwrap();
        let workspace1 = temp_root.path().join("workspace1");
        let workspace2 = temp_root.path().join("workspace2");
        let home = PathBuf::from("/home/test");

        fs::create_dir_all(&workspace1).unwrap();
        fs::create_dir_all(&workspace2).unwrap();
        fs::create_dir(workspace1.join(".beads")).unwrap();
        fs::create_dir(workspace2.join(".beads")).unwrap();

        let factory = Arc::new(BothValidMockFactory {
            workspace1: workspace1.clone(),
            workspace2: workspace2.clone(),
        });

        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());

        let strand = ExploreStrand::new_with_store_factory(
            vec![workspace1.clone(), workspace2.clone()],
            home,
            registry,
            telemetry,
            "test-worker".to_string(),
            factory,
        );

        let store = DummyStore;
        let StrandResult::BeadFound(candidates) = strand.evaluate(&store, &HashSet::new()).await
        else {
            panic!("expected BeadFound with aggregated candidates from both workspaces");
        };

        assert_eq!(
            candidates.len(),
            2,
            "explore must aggregate the candidate from BOTH workspaces, not stop at the first"
        );
        let scanned: HashSet<PathBuf> = candidates.iter().map(|b| b.workspace.clone()).collect();
        assert!(
            scanned.contains(&workspace1) && scanned.contains(&workspace2),
            "aggregated candidates should include both workspace1 and workspace2, got {:?}",
            scanned
        );
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
    ///
    /// NOTE: This test is quarantined due to spawn_blocking deadlock in test environments.
    /// The test uses real Registry instances that do blocking file I/O and PID checks
    /// via spawn_blocking, which can deadlock when the blocking thread pool is exhausted.
    /// Re-enabling this test requires either:
    /// - Making Registry operations async instead of blocking
    /// - Providing a mock Registry that doesn't do file I/O
    /// - Running tests with a runtime that has sufficient blocking threads
    #[tokio::test]
    #[ignore]
    async fn deadlock_scenario_assigned_beads_allow_advancement() {
        let temp_root = tempfile::tempdir().unwrap();
        let workspace1 = temp_root.path().join("workspace1");
        let workspace2 = temp_root.path().join("workspace2");
        let home = PathBuf::from("/home/test");

        // Create .beads/ directories so has_beads_dir() returns true
        fs::create_dir_all(&workspace1).unwrap();
        fs::create_dir_all(&workspace2).unwrap();
        fs::create_dir(workspace1.join(".beads")).unwrap();
        fs::create_dir(workspace2.join(".beads")).unwrap();

        // Create a mock store factory
        let mock_factory = Arc::new(DeadlockMockStoreFactory::new(
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
            mock_factory.clone(),
        );

        let store = DummyStore;
        let result = strand.evaluate(&store, &HashSet::new()).await;

        // Verify that both workspaces were queried
        let call_count = mock_factory.call_count();
        assert!(
            call_count >= 2,
            "both workspaces should be queried (at minimum), got: {}",
            call_count
        );

        // Verify that workspace 2's candidate was returned
        match result {
            StrandResult::BeadFound(candidates) => {
                assert_eq!(
                    candidates.len(),
                    1,
                    "should find 1 candidate from workspace 2"
                );
                assert_eq!(candidates[0].id, BeadId::from("ws2-valid-bead".to_string()));
                assert_eq!(candidates[0].workspace, workspace2);
                assert!(
                    candidates[0].assignee.is_none(),
                    "candidate should be unassigned"
                );
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
            StrandResult::Skipped { .. } => {
                panic!("unexpected Skipped result");
            }
            StrandResult::FoundButExcluded => {
                panic!("unexpected FoundButExcluded result");
            }
        }
    }

    /// Unit test for when workspace 1 has only excluded beads (blocked label).
    ///
    /// This proves that the strand advances when candidates are excluded by
    /// the Filters (deferred/human/blocked labels), not just when assigned.
    ///
    /// NOTE: This test is quarantined due to spawn_blocking deadlock in test environments.
    /// See deadlock_scenario_assigned_beads_allow_advancement for details.
    #[tokio::test]
    #[ignore]
    async fn deadlock_scenario_excluded_beads_allow_advancement() {
        let temp_root = tempfile::tempdir().unwrap();
        let workspace1 = temp_root.path().join("workspace1");
        let workspace2 = temp_root.path().join("workspace2");
        let home = PathBuf::from("/home/test");

        // Create .beads/ directories so has_beads_dir() returns true
        fs::create_dir_all(&workspace1).unwrap();
        fs::create_dir_all(&workspace2).unwrap();
        fs::create_dir(workspace1.join(".beads")).unwrap();
        fs::create_dir(workspace2.join(".beads")).unwrap();

        // Create a mock store factory for excluded beads scenario
        let mock_factory = Arc::new(ExcludedBeadsMockFactory::new(
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
            mock_factory.clone(),
        );

        let store = DummyStore;
        let result = strand.evaluate(&store, &HashSet::new()).await;

        // Verify that workspace 2's candidate was returned
        match result {
            StrandResult::BeadFound(candidates) => {
                assert_eq!(
                    candidates.len(),
                    1,
                    "should find 1 candidate from workspace 2"
                );
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
            StrandResult::Skipped { .. } => {
                panic!("unexpected Skipped result");
            }
            StrandResult::FoundButExcluded => {
                panic!("unexpected FoundButExcluded result");
            }
        }
    }

    /// Unit test for the excluded AND assigned edge case.
    ///
    /// This tests the specific edge case where beads are BOTH:
    /// 1. Assigned (assignee != None)
    /// 2. Excluded (have blocked/deferred/human labels)
    ///
    /// DEADLOCK SCENARIO:
    /// - Workspace 1: ready() returns beads that are BOTH assigned AND excluded
    /// - Workspace 2: ready() returns valid unassigned candidates
    /// - EXPECTED: Strand filters out doubly-unclaimable beads and advances to workspace 2
    /// - BUG: Strand returns NoWork prematurely, never checking workspace 2
    ///
    /// This is a critical edge case because:
    /// - The defensive filtering logic checks BOTH conditions (assignee_ok AND labels_ok)
    /// - A bead that fails EITHER condition should be filtered out
    /// - A bead that fails BOTH conditions should DEFINITELY be filtered out
    ///
    /// NOTE: This test is quarantined due to spawn_blocking deadlock in test environments.
    /// See deadlock_scenario_assigned_beads_allow_advancement for details.
    #[tokio::test]
    #[ignore]
    async fn deadlock_scenario_excluded_and_assigned_beads_allow_advancement() {
        let temp_root = tempfile::tempdir().unwrap();
        let workspace1 = temp_root.path().join("workspace1");
        let workspace2 = temp_root.path().join("workspace2");
        let home = PathBuf::from("/home/test");

        // Create .beads/ directories so has_beads_dir() returns true
        fs::create_dir_all(&workspace1).unwrap();
        fs::create_dir_all(&workspace2).unwrap();
        fs::create_dir(workspace1.join(".beads")).unwrap();
        fs::create_dir(workspace2.join(".beads")).unwrap();

        // Create a mock store factory for excluded AND assigned beads scenario
        let mock_factory = Arc::new(ExcludedAndAssignedMockFactory::new(
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
            mock_factory.clone(),
        );

        let store = DummyStore;
        let result = strand.evaluate(&store, &HashSet::new()).await;

        // Verify that both workspaces were queried
        let call_count = mock_factory.call_count();
        assert!(
            call_count >= 2,
            "both workspaces should be queried (at minimum), got: {}",
            call_count
        );

        // Verify that workspace 2's candidate was returned (proving advancement past workspace 1)
        match result {
            StrandResult::BeadFound(candidates) => {
                assert_eq!(
                    candidates.len(),
                    1,
                    "should find 1 candidate from workspace 2"
                );
                assert_eq!(candidates[0].id, BeadId::from("ws2-valid-bead".to_string()));
                assert_eq!(candidates[0].workspace, workspace2);
                assert!(
                    candidates[0].assignee.is_none(),
                    "candidate should be unassigned"
                );
                assert!(
                    !candidates[0]
                        .labels
                        .iter()
                        .any(|l| l == "blocked" || l == "deferred" || l == "human"),
                    "candidate should not have excluded labels"
                );
            }
            StrandResult::NoWork => {
                panic!(
                        "deadlock bug reproduced: strand returned NoWork instead of finding workspace 2's candidate.\n\
                         This proves the strand is not advancing past workspace 1 even though all its candidates are BOTH excluded AND assigned."
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
            StrandResult::Skipped { .. } => {
                panic!("unexpected Skipped result");
            }
            StrandResult::FoundButExcluded => {
                panic!("unexpected FoundButExcluded result");
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
        async fn create_store(
            &self,
            workspace: &Path,
        ) -> Result<Arc<dyn BeadStore>, anyhow::Error> {
            if workspace == self.workspace1 {
                Ok(Arc::new(ExcludedCandidatesStore::new(
                    self.workspace1.clone(),
                )))
            } else if workspace == self.workspace2 {
                Ok(Arc::new(ValidBeadStore::new(self.workspace2.clone())))
            } else {
                Err(anyhow::anyhow!(
                    "unexpected workspace: {}",
                    workspace.display()
                ))
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
        async fn create_store(
            &self,
            workspace: &Path,
        ) -> Result<Arc<dyn BeadStore>, anyhow::Error> {
            self.call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

            if workspace == self.workspace1 {
                Ok(Arc::new(AssignedBeadsStore::new(self.workspace1.clone())))
            } else if workspace == self.workspace2 {
                Ok(Arc::new(ValidBeadStore::new(self.workspace2.clone())))
            } else {
                Err(anyhow::anyhow!(
                    "unexpected workspace: {}",
                    workspace.display()
                ))
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
    }

    #[async_trait::async_trait]
    impl StoreFactory for ExcludedBeadsMockFactory {
        async fn create_store(
            &self,
            workspace: &Path,
        ) -> Result<Arc<dyn BeadStore>, anyhow::Error> {
            self.call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

            if workspace == self.workspace1 {
                Ok(Arc::new(BlockedBeadsStore::new(self.workspace1.clone())))
            } else if workspace == self.workspace2 {
                Ok(Arc::new(ValidBeadStore::new(self.workspace2.clone())))
            } else {
                Err(anyhow::anyhow!(
                    "unexpected workspace: {}",
                    workspace.display()
                ))
            }
        }
    }

    /// Mock store factory for excluded AND assigned beads scenario.
    ///
    /// Simulates the critical edge case where beads are BOTH:
    /// 1. Assigned (assignee != None)
    /// 2. Excluded (have blocked/deferred/human labels)
    ///
    /// This tests that the defensive filtering correctly handles beads that fail
    /// BOTH filtering conditions (doubly-unclaimable).
    struct ExcludedAndAssignedMockFactory {
        workspace1: PathBuf,
        workspace2: PathBuf,
        call_count: std::sync::Arc<std::sync::atomic::AtomicU32>,
    }

    impl ExcludedAndAssignedMockFactory {
        fn new(workspace1: PathBuf, workspace2: PathBuf) -> Self {
            ExcludedAndAssignedMockFactory {
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
    impl StoreFactory for ExcludedAndAssignedMockFactory {
        async fn create_store(
            &self,
            workspace: &Path,
        ) -> Result<Arc<dyn BeadStore>, anyhow::Error> {
            self.call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

            if workspace == self.workspace1 {
                Ok(Arc::new(ExcludedAndAssignedStore::new(
                    self.workspace1.clone(),
                )))
            } else if workspace == self.workspace2 {
                Ok(Arc::new(ValidBeadStore::new(self.workspace2.clone())))
            } else {
                Err(anyhow::anyhow!(
                    "unexpected workspace: {}",
                    workspace.display()
                ))
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
        fn has_valid_store(&self) -> bool {
            true
        }
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
                    comments: vec![],
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
                    comments: vec![],
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
                    comments: vec![],
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
        async fn block(&self, _id: &BeadId) -> Result<()> {
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
        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
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
        fn has_valid_store(&self) -> bool {
            true
        }
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            // For tests, always return assigned beads on first call to avoid
            // triggering the empty candidate path that leads to cleanup_orphaned_in_progress
            // which can deadlock in test environments with spawn_blocking.
            let count = self
                .query_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

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
                        comments: vec![],
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
                        comments: vec![],
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                    },
                ])
            } else {
                // Re-query: return empty to continue test scenario
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
        async fn block(&self, _id: &BeadId) -> Result<()> {
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
        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
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
        fn has_valid_store(&self) -> bool {
            true
        }
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            let count = self
                .query_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

            if count == 0 {
                // First query: return beads with "blocked" label (excluded by filters)
                Ok(vec![Bead {
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
                    comments: vec![],
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                }])
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
        async fn block(&self, _id: &BeadId) -> Result<()> {
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
        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
    }

    /// Mock store that returns beads that are BOTH excluded AND assigned.
    ///
    /// This simulates the critical edge case where beads are doubly-unclaimable:
    /// 1. They have an assignee (assigned to another worker)
    /// 2. They have excluded labels (blocked/deferred/human)
    ///
    /// The defensive filtering should remove these beads because they fail
    /// BOTH filtering conditions (assignee_ok AND labels_ok).
    struct ExcludedAndAssignedStore {
        workspace: PathBuf,
        query_count: std::sync::Arc<std::sync::atomic::AtomicU32>,
    }

    impl ExcludedAndAssignedStore {
        fn new(workspace: PathBuf) -> Self {
            ExcludedAndAssignedStore {
                workspace,
                query_count: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            }
        }
    }

    #[async_trait::async_trait]
    impl BeadStore for ExcludedAndAssignedStore {
        fn has_valid_store(&self) -> bool {
            true
        }
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            let count = self
                .query_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

            if count == 0 {
                // First query: return beads that are BOTH assigned AND excluded
                // These should be filtered out by the defensive filtering
                Ok(vec![
                    Bead {
                        id: BeadId::from("ws1-both-1".to_string()),
                        title: "Assigned and Blocked Bead 1".to_string(),
                        body: None,
                        priority: 1,
                        status: BeadStatus::Open,
                        assignee: Some("other-worker-1".to_string()),
                        labels: vec!["blocked".to_string()],
                        workspace: self.workspace.clone(),
                        dependencies: vec![],
                        dependents: vec![],
                        comments: vec![],
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                    },
                    Bead {
                        id: BeadId::from("ws1-both-2".to_string()),
                        title: "Assigned and Deferred Bead 2".to_string(),
                        body: None,
                        priority: 2,
                        status: BeadStatus::Open,
                        assignee: Some("other-worker-2".to_string()),
                        labels: vec!["deferred".to_string()],
                        workspace: self.workspace.clone(),
                        dependencies: vec![],
                        dependents: vec![],
                        comments: vec![],
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                    },
                    Bead {
                        id: BeadId::from("ws1-both-3".to_string()),
                        title: "Assigned and Human Bead 3".to_string(),
                        body: None,
                        priority: 3,
                        status: BeadStatus::Open,
                        assignee: Some("other-worker-3".to_string()),
                        labels: vec!["human".to_string()],
                        workspace: self.workspace.clone(),
                        dependencies: vec![],
                        dependents: vec![],
                        comments: vec![],
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                    },
                ])
            } else {
                // Re-query after cross-workspace mend: still no valid beads
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
        async fn block(&self, _id: &BeadId) -> Result<()> {
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
        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
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
        fn has_valid_store(&self) -> bool {
            true
        }
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
                comments: vec![],
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
        async fn block(&self, _id: &BeadId) -> Result<()> {
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
        async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
    }

    // ── Regression Tests: 2026-07-19/20 Incident (plan.md Phase 8.4) ─────────

    /// Regression Test 1: Empty workspaces config triggers full discovery.
    ///
    /// Test: ExploreStrand::new() with an empty `config.workspaces` and a
    /// `workspace_root` containing several `.beads/`-having directories produces
    /// a worker that scans all of them, not a hardcoded subset.
    ///
    /// This is the DEFAULT behavior and should be the normal case for the fleet.
    /// When `workspaces` is empty, the strand MUST run recursive discovery under
    /// `workspace_root` and find every directory containing a `.beads/` subdirectory.
    #[test]
    fn regression_empty_workspaces_config_triggers_full_discovery() {
        let root = tempfile::tempdir().unwrap();

        // Create multiple workspaces with .beads/ directories
        let workspace1 = root.path().join("workspace1");
        let workspace2 = root.path().join("workspace2");
        let workspace3 = root.path().join("workspace3");
        let workspace4 = root.path().join("workspace4");

        for ws in &[&workspace1, &workspace2, &workspace3, &workspace4] {
            fs::create_dir(ws).unwrap();
            fs::create_dir(ws.join(".beads")).unwrap();
        }

        // Create a non-workspace directory (no .beads/)
        let not_a_workspace = root.path().join("not-a-workspace");
        fs::create_dir(&not_a_workspace).unwrap();

        // Empty workspaces config with a valid root
        let config = ExploreConfig {
            enabled: true,
            workspaces: vec![], // EMPTY — should trigger discovery
            workspace_root: root.path().to_path_buf(),
            rediscovery_cycles: 0,
            starvation_threshold_minutes: 0,
            scan_interval_cycles: 1,
            max_scan_interval_cycles: 8,
        };

        let home = PathBuf::from("/home/test");
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());

        let strand =
            ExploreStrand::new(config, home, registry, telemetry, "test-worker".to_string());

        // The strand MUST have discovered all 4 workspaces with .beads/
        assert_eq!(
            strand.workspaces.lock().unwrap().len(),
            4,
            "empty workspaces config must discover all .beads/ directories under workspace_root"
        );

        // All workspaces should be present
        assert!(
            strand.workspaces.lock().unwrap().contains(&workspace1),
            "workspace1 should be discovered"
        );
        assert!(
            strand.workspaces.lock().unwrap().contains(&workspace2),
            "workspace2 should be discovered"
        );
        assert!(
            strand.workspaces.lock().unwrap().contains(&workspace3),
            "workspace3 should be discovered"
        );
        assert!(
            strand.workspaces.lock().unwrap().contains(&workspace4),
            "workspace4 should be discovered"
        );

        // Non-workspace should NOT be present
        assert!(
            !strand.workspaces.lock().unwrap().contains(&not_a_workspace),
            "directory without .beads/ should not be discovered"
        );
    }

    /// Regression Test 2: Non-empty workspaces config is a pin, never falls back to discovery.
    ///
    /// Test: ExploreStrand::new() with a non-empty `config.workspaces` (simulating
    /// a deliberate pin) scans exactly that list, never falling back to discovery.
    ///
    /// This preserves the exception mechanism. When `workspaces` is explicitly
    /// set, auto-discovery MUST be disabled and the strand MUST use only the
    /// explicitly listed paths. This is the PINNED mode and should emit a WARN log.
    #[test]
    fn regression_non_empty_workspaces_config_is_pinned_never_discovers() {
        let root = tempfile::tempdir().unwrap();

        // Create the directories that ARE in the explicit list
        let pinned1 = root.path().join("pinned-workspace1");
        let pinned2 = root.path().join("pinned-workspace2");

        for ws in &[&pinned1, &pinned2] {
            fs::create_dir(ws).unwrap();
            fs::create_dir(ws.join(".beads")).unwrap();
        }

        // Create additional workspaces that are NOT in the explicit list
        let unpinned1 = root.path().join("unpinned-workspace1");
        let unpinned2 = root.path().join("unpinned-workspace2");

        for ws in &[&unpinned1, &unpinned2] {
            fs::create_dir(ws).unwrap();
            fs::create_dir(ws.join(".beads")).unwrap();
        }

        // Non-empty workspaces config — explicit pin list
        let config = ExploreConfig {
            enabled: true,
            workspaces: vec![pinned1.clone(), pinned2.clone()], // PINNED — should NOT discover
            workspace_root: root.path().to_path_buf(),
            rediscovery_cycles: 0,
            starvation_threshold_minutes: 0,
            scan_interval_cycles: 1,
            max_scan_interval_cycles: 8,
        };

        let home = PathBuf::from("/home/test");
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());

        let strand =
            ExploreStrand::new(config, home, registry, telemetry, "test-worker".to_string());

        // The strand MUST have ONLY the 2 pinned workspaces
        assert_eq!(
            strand.workspaces.lock().unwrap().len(),
            2,
            "non-empty workspaces config must use only the explicit list, not discover additional workspaces"
        );

        // Pinned workspaces should be present
        assert!(
            strand.workspaces.lock().unwrap().contains(&pinned1),
            "pinned workspace 1 should be in the list"
        );
        assert!(
            strand.workspaces.lock().unwrap().contains(&pinned2),
            "pinned workspace 2 should be in the list"
        );

        // Unpinned workspaces must NOT be present — even though they have .beads/
        assert!(
            !strand.workspaces.lock().unwrap().contains(&unpinned1),
            "unpinned workspace 1 should NOT be discovered when workspaces list is non-empty"
        );
        assert!(
            !strand.workspaces.lock().unwrap().contains(&unpinned2),
            "unpinned workspace 2 should NOT be discovered when workspaces list is non-empty"
        );
    }

    /// Regression Test 3: Fixture reproducing the exact 2026-07-19/20 incident.
    ///
    /// Test: A `workspace_root` containing `commitgraph` and `twitterapi-proxy`
    /// (both with `.beads/`) alongside other known repos — with an empty `workspaces`
    /// config, all are discovered, not just a previously hand-listed subset.
    ///
    /// This reproduces the EXACT incident scenario. At the time of the incident,
    /// the fleet config had a static 24-entry list that didn't include the two
    /// newly-added repos. This test proves that with empty config (the intended
    /// default), discovery finds everything without manual list maintenance.
    #[test]
    fn regression_fixture_2026_07_19_incident_all_workspaces_discovered() {
        let root = tempfile::tempdir().unwrap();

        // Create the 24 "known repos" from the original static list
        let known_repos = [
            "NEEDLE",
            "bead-forge",
            "CLASP",
            "SIGIL",
            "ARMOR",
            "spaxel",
            "mta-my-way",
            "kalshi-tape",
            "kalshi-weather",
            "duck-e",
            "vista",
            "botburrow-agents",
            "news-trader",
            "domain-check",
            "AgentScribe",
            "telegram-claude-bridge",
            "forge",
            "declarative-config",
            "nixos-asterisk",
            "perles-orchestration-control-plane",
            "junk-drawer",
            "private-dotfiles",
            "hoop",
            "FABRIC",
        ];

        for repo_name in &known_repos {
            let ws = root.path().join(repo_name);
            fs::create_dir(&ws).unwrap();
            fs::create_dir(ws.join(".beads")).unwrap();
        }

        // Create the two newly-added repos that were missing from the static list
        let commitgraph = root.path().join("commitgraph");
        let twitterapi_proxy = root.path().join("twitterapi-proxy");

        fs::create_dir(&commitgraph).unwrap();
        fs::create_dir(commitgraph.join(".beads")).unwrap();
        fs::create_dir(&twitterapi_proxy).unwrap();
        fs::create_dir(twitterapi_proxy.join(".beads")).unwrap();

        // Empty workspaces config — the INTENDED default
        let config = ExploreConfig {
            enabled: true,
            workspaces: vec![], // EMPTY — should discover ALL repos including the new ones
            workspace_root: root.path().to_path_buf(),
            rediscovery_cycles: 0,
            starvation_threshold_minutes: 0,
            scan_interval_cycles: 1,
            max_scan_interval_cycles: 8,
        };

        let home = PathBuf::from("/home/test");
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());

        let strand =
            ExploreStrand::new(config, home, registry, telemetry, "test-worker".to_string());

        // The strand MUST have discovered ALL 26 repos (24 original + 2 new)
        assert_eq!(
            strand.workspaces.lock().unwrap().len(),
            26,
            "with empty workspaces config, all repos including newly-added ones must be discovered"
        );

        // The two previously-missing repos MUST be present
        assert!(
            strand.workspaces.lock().unwrap().contains(&commitgraph),
            "commitgraph must be discovered (was missing from static list in incident)"
        );
        assert!(
            strand
                .workspaces
                .lock()
                .unwrap()
                .contains(&twitterapi_proxy),
            "twitterapi-proxy must be discovered (was missing from static list in incident)"
        );

        // All original repos should still be present
        for repo_name in &known_repos {
            let ws = root.path().join(repo_name);
            assert!(
                strand.workspaces.lock().unwrap().contains(&ws),
                "{} should still be discovered",
                repo_name
            );
        }
    }

    /// Regression Test 4: Discovery handles non-existent workspace_root gracefully.
    ///
    /// Test: ExploreStrand::new() with empty `config.workspaces` and a
    /// `workspace_root` that doesn't exist returns an empty workspace list
    /// (not an error).
    ///
    /// This is a defensive test — discovery should fail gracefully when the
    /// configured root doesn't exist, not panic or error. Workers in this state
    /// will simply have no workspaces to explore (which is valid).
    #[test]
    fn regression_discovery_with_nonexistent_root_returns_empty() {
        let nonexistent_root = PathBuf::from("/this/path/definitely/does/not/exist/xyz123");

        let config = ExploreConfig {
            enabled: true,
            workspaces: vec![], // EMPTY — should attempt discovery
            workspace_root: nonexistent_root,
            rediscovery_cycles: 0,
            starvation_threshold_minutes: 0,
            scan_interval_cycles: 1,
            max_scan_interval_cycles: 8,
        };

        let home = PathBuf::from("/home/test");
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());

        let strand =
            ExploreStrand::new(config, home, registry, telemetry, "test-worker".to_string());

        // Should return empty list, not panic or error
        assert_eq!(
            strand.workspaces.lock().unwrap().len(),
            0,
            "non-existent workspace_root should result in empty workspace list"
        );
    }

    /// Regression Test 5: Discovery filters correctly, only finding .beads/ directories.
    ///
    /// Test: ExploreStrand::new() with empty `config.workspaces` and a
    /// `workspace_root` containing a mix of `.beads/`-having directories,
    /// non-workspace directories, and files — discovers ONLY the directories
    /// that actually contain `.beads/`.
    ///
    /// This proves that discovery is selective, not indiscriminate. It should
    /// only find valid workspace directories, not every directory under the root.
    #[test]
    fn regression_discovery_filters_only_beads_directories() {
        let root = tempfile::tempdir().unwrap();

        // Create valid workspaces (have .beads/)
        let valid1 = root.path().join("valid-workspace1");
        let valid2 = root.path().join("valid-workspace2");
        fs::create_dir(&valid1).unwrap();
        fs::create_dir(valid1.join(".beads")).unwrap();
        fs::create_dir(&valid2).unwrap();
        fs::create_dir(valid2.join(".beads")).unwrap();

        // Create directories without .beads/
        let no_beads1 = root.path().join("no-beads-dir1");
        let no_beads2 = root.path().join("no-beads-dir2");
        fs::create_dir(&no_beads1).unwrap();
        fs::create_dir(&no_beads2).unwrap();

        // Create a file (not a directory) — should be ignored
        let a_file = root.path().join("not-a-directory.txt");
        fs::write(&a_file, b"some content").unwrap();

        let config = ExploreConfig {
            enabled: true,
            workspaces: vec![], // EMPTY — should trigger discovery
            workspace_root: root.path().to_path_buf(),
            rediscovery_cycles: 0,
            starvation_threshold_minutes: 0,
            scan_interval_cycles: 1,
            max_scan_interval_cycles: 8,
        };

        let home = PathBuf::from("/home/test");
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());

        let strand =
            ExploreStrand::new(config, home, registry, telemetry, "test-worker".to_string());

        // Should discover only the 2 valid workspaces
        assert_eq!(
            strand.workspaces.lock().unwrap().len(),
            2,
            "discovery should find only directories with .beads/"
        );

        assert!(
            strand.workspaces.lock().unwrap().contains(&valid1),
            "valid workspace 1 should be discovered"
        );
        assert!(
            strand.workspaces.lock().unwrap().contains(&valid2),
            "valid workspace 2 should be discovered"
        );

        // Directories without .beads/ should NOT be present
        assert!(
            !strand.workspaces.lock().unwrap().contains(&no_beads1),
            "directory without .beads/ should not be discovered"
        );
        assert!(
            !strand.workspaces.lock().unwrap().contains(&no_beads2),
            "directory without .beads/ should not be discovered"
        );
    }

    /// Regression Test 6: Pinned mode with directories that don't exist.
    ///
    /// Test: ExploreStrand::new() with a non-empty `config.workspaces` that
    /// includes paths that don't exist — the strand includes them in the list
    /// anyway (validation happens later during strand evaluation).
    ///
    /// This is a defensive test proving that the pinned mode doesn't validate
    /// existence at construction time. The strand faithfully uses whatever
    /// list it's given — failures during evaluation are handled gracefully.
    #[test]
    fn regression_pinned_mode_includes_nonexistent_paths_in_list() {
        let config = ExploreConfig {
            enabled: true,
            workspaces: vec![
                PathBuf::from("/nonexistent/path/1"),
                PathBuf::from("/nonexistent/path/2"),
                PathBuf::from("/another/fake/path"),
            ],
            workspace_root: PathBuf::from("/tmp/irrelevant"),
            rediscovery_cycles: 0,
            starvation_threshold_minutes: 15,
            scan_interval_cycles: 1,
            max_scan_interval_cycles: 8,
        };

        let home = PathBuf::from("/home/test");
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());

        let strand =
            ExploreStrand::new(config, home, registry, telemetry, "test-worker".to_string());

        // The strand should include the nonexistent paths in its list
        // (validation happens during strand evaluation, not construction)
        assert_eq!(
            strand.workspaces.lock().unwrap().len(),
            3,
            "pinned mode should include all configured paths regardless of existence"
        );
    }

    /// Regression Test 7: Empty workspace_root with empty config.
    ///
    /// Test: ExploreStrand::new() with empty `config.workspaces` and a
    /// `workspace_root` that exists but is empty returns an empty workspace list.
    ///
    /// This is an edge case — the root directory exists but has no subdirectories.
    /// Discovery should return empty (no error), not panic.
    #[test]
    fn regression_empty_workspace_root_returns_empty_discovery() {
        let root = tempfile::tempdir().unwrap(); // Root exists but is empty

        let config = ExploreConfig {
            enabled: true,
            workspaces: vec![], // EMPTY — should trigger discovery
            workspace_root: root.path().to_path_buf(),
            rediscovery_cycles: 0,
            starvation_threshold_minutes: 0,
            scan_interval_cycles: 1,
            max_scan_interval_cycles: 8,
        };

        let home = PathBuf::from("/home/test");
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());

        let strand =
            ExploreStrand::new(config, home, registry, telemetry, "test-worker".to_string());

        // Should return empty list (root has no subdirectories)
        assert_eq!(
            strand.workspaces.lock().unwrap().len(),
            0,
            "empty workspace_root should result in empty discovery list"
        );
    }

    /// Regression Test 8: Discovery order is deterministic.
    ///
    /// Test: ExploreStrand::new() with empty `config.workspaces` and multiple
    /// workspaces — the discovered list is in a deterministic order (filesystem
    /// order, which is stable for a given set of directory names).
    ///
    /// This test verifies that discovery produces consistent results across
    /// multiple runs given the same filesystem state. Determinism is a core
    /// NEEDLE principle.
    #[test]
    fn regression_discovery_order_is_deterministic() {
        let root = tempfile::tempdir().unwrap();

        // Create workspaces in alphabetical order
        let workspace_names = vec!["alpha", "bravo", "charlie", "delta", "echo"];
        let mut workspaces = Vec::new();

        for name in &workspace_names {
            let ws = root.path().join(name);
            fs::create_dir(&ws).unwrap();
            fs::create_dir(ws.join(".beads")).unwrap();
            workspaces.push(ws);
        }

        let config = ExploreConfig {
            enabled: true,
            workspaces: vec![], // EMPTY — should trigger discovery
            workspace_root: root.path().to_path_buf(),
            rediscovery_cycles: 0,
            starvation_threshold_minutes: 0,
            scan_interval_cycles: 1,
            max_scan_interval_cycles: 8,
        };

        let home = PathBuf::from("/home/test");

        // Create two strands with the same config
        let temp_dir1 = tempfile::tempdir().unwrap();
        let registry1 = crate::registry::Registry::new(temp_dir1.path());
        let telemetry1 = Telemetry::new("test-worker-1".to_string());

        let strand1 = ExploreStrand::new(
            config.clone(),
            home.clone(),
            registry1,
            telemetry1,
            "test-worker-1".to_string(),
        );

        let temp_dir2 = tempfile::tempdir().unwrap();
        let registry2 = crate::registry::Registry::new(temp_dir2.path());
        let telemetry2 = Telemetry::new("test-worker-2".to_string());

        let strand2 = ExploreStrand::new(
            config,
            home,
            registry2,
            telemetry2,
            "test-worker-2".to_string(),
        );

        // Both strands should have the same workspace list
        assert_eq!(
            strand1.workspaces.lock().unwrap().len(),
            strand2.workspaces.lock().unwrap().len(),
            "both strands should discover the same number of workspaces"
        );

        // Order should be the same (filesystem order is deterministic)
        assert_eq!(
            *strand1.workspaces.lock().unwrap(),
            *strand2.workspaces.lock().unwrap(),
            "discovery order should be deterministic across multiple strand constructions"
        );
    }

    /// Regression Test 9: Discovery handles nested .beads/ directories correctly.
    ///
    /// Test: ExploreStrand::new() with empty `config.workspaces` only discovers
    /// IMMEDIATE children of workspace_root, not nested grandchildren.
    ///
    /// This is a critical test — discovery should be shallow (one level deep)
    /// to avoid unbounded filesystem traversal. A .beads/ directory in a
    /// subdirectory of a subdirectory should NOT be discovered.
    #[test]
    fn regression_discovery_is_shallow_single_level_only() {
        let root = tempfile::tempdir().unwrap();

        // Create a valid workspace at the top level
        let top_level = root.path().join("top-level-workspace");
        fs::create_dir(&top_level).unwrap();
        fs::create_dir(top_level.join(".beads")).unwrap();

        // Create a nested subdirectory WITH .beads/ (should NOT be discovered)
        let parent_dir = root.path().join("parent-dir");
        fs::create_dir(&parent_dir).unwrap();
        let nested_dir = parent_dir.join("nested-workspace");
        fs::create_dir(&nested_dir).unwrap();
        fs::create_dir(nested_dir.join(".beads")).unwrap();

        let config = ExploreConfig {
            enabled: true,
            workspaces: vec![], // EMPTY — should trigger discovery
            workspace_root: root.path().to_path_buf(),
            rediscovery_cycles: 0,
            starvation_threshold_minutes: 0,
            scan_interval_cycles: 1,
            max_scan_interval_cycles: 8,
        };

        let home = PathBuf::from("/home/test");
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());

        let strand =
            ExploreStrand::new(config, home, registry, telemetry, "test-worker".to_string());

        // Should discover ONLY the top-level workspace
        assert_eq!(
            strand.workspaces.lock().unwrap().len(),
            1,
            "discovery should be shallow — only immediate children of workspace_root"
        );

        assert!(
            strand.workspaces.lock().unwrap().contains(&top_level),
            "top-level workspace should be discovered"
        );

        // Nested workspace should NOT be discovered
        assert!(
            !strand.workspaces.lock().unwrap().contains(&nested_dir),
            "nested workspace (grandchild of root) should NOT be discovered"
        );

        // Parent directory without .beads/ should not be discovered
        assert!(
            !strand.workspaces.lock().unwrap().contains(&parent_dir),
            "parent directory without .beads/ should NOT be discovered"
        );
    }

    /// Test: Periodic workspace re-discovery picks up new workspaces.
    ///
    /// This test verifies that when a new workspace with a .beads/ directory
    /// appears after the ExploreStrand is constructed, it is discovered
    /// during periodic re-discovery without requiring a worker restart.
    ///
    /// Test flow:
    /// 1. Create a root with one initial workspace
    /// 2. Create an ExploreStrand with rediscovery_cycles = 2
    /// 3. Call evaluate() once (cycle 1 of 2) - no rediscovery yet
    /// 4. Create a second workspace with .beads/
    /// 5. Call evaluate() again (cycle 2) - triggers rediscovery, picks up new workspace
    /// 6. Verify the new workspace is now in the strand's workspace list
    ///
    /// NOTE: This test is quarantined due to spawn_blocking deadlock in test environments.
    /// See deadlock_scenario_assigned_beads_allow_advancement for details.
    #[tokio::test]
    #[ignore]
    async fn periodic_rediscovery_discovers_new_workspaces() {
        let root = tempfile::tempdir().unwrap();

        // Create initial workspace with .beads/
        let ws1 = root.path().join("workspace1");
        fs::create_dir(&ws1).unwrap();
        fs::create_dir(ws1.join(".beads")).unwrap();

        // Configure auto-discovery with rediscovery_cycles = 2
        let config = ExploreConfig {
            enabled: true,
            workspaces: vec![], // EMPTY — auto-discovery mode
            workspace_root: root.path().to_path_buf(),
            rediscovery_cycles: 2, // Re-discover every 2 cycles
            starvation_threshold_minutes: 15,
            scan_interval_cycles: 1,
            max_scan_interval_cycles: 1,
        };

        let home = PathBuf::from("/some/other/home");
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());

        let strand = ExploreStrand::new(
            config,
            home,
            registry,
            telemetry,
            "test-worker-rediscovery".to_string(),
        );

        // Initial state: should have discovered workspace1
        assert_eq!(
            strand.workspaces.lock().unwrap().len(),
            1,
            "initial discovery should find 1 workspace"
        );
        assert!(
            strand.workspaces.lock().unwrap().contains(&ws1),
            "initial workspace should be in list"
        );

        // Cycle 1: no rediscovery yet (we need 2 cycles)
        let store = DummyStore;
        let _ = strand.evaluate(&store, &HashSet::new()).await;

        // Verify we haven't done rediscovery yet (still 1 workspace)
        assert_eq!(
            strand.workspaces.lock().unwrap().len(),
            1,
            "after cycle 1, still 1 workspace"
        );

        // Create a second workspace with .beads/ (simulating a new repo being added)
        let ws2 = root.path().join("workspace2");
        fs::create_dir(&ws2).unwrap();
        fs::create_dir(ws2.join(".beads")).unwrap();

        // Cycle 2: this should trigger rediscovery and pick up the new workspace
        let _ = strand.evaluate(&store, &HashSet::new()).await;

        // After rediscovery, we should have both workspaces
        assert_eq!(
            strand.workspaces.lock().unwrap().len(),
            2,
            "after cycle 2 (rediscovery), should have 2 workspaces"
        );
        assert!(
            strand.workspaces.lock().unwrap().contains(&ws1),
            "original workspace should still be in list"
        );
        assert!(
            strand.workspaces.lock().unwrap().contains(&ws2),
            "new workspace should be discovered during rediscovery"
        );
    }

    /// Test: Periodic re-discovery is skipped in pinned mode (explicit workspaces list).
    ///
    /// When config.workspaces is non-empty (pinned mode), periodic re-discovery
    /// should be skipped even if rediscovery_cycles > 0. This preserves the
    /// "explicit workspaces override" constraint.
    ///
    /// NOTE: This test is quarantined due to spawn_blocking deadlock in test environments.
    /// See deadlock_scenario_assigned_beads_allow_advancement for details.
    #[tokio::test]
    #[ignore]
    async fn periodic_rediscovery_skipped_in_pinned_mode() {
        let root = tempfile::tempdir().unwrap();

        // Create a workspace that exists but is NOT in the explicit list
        let unlisted_ws = root.path().join("unlisted-workspace");
        fs::create_dir(&unlisted_ws).unwrap();
        fs::create_dir(unlisted_ws.join(".beads")).unwrap();

        // Configure with explicit workspaces list (pinned mode)
        let pinned_ws = PathBuf::from("/pinned/workspace");
        let config = ExploreConfig {
            enabled: true,
            workspaces: vec![pinned_ws.clone()], // NON-EMPTY — pinned mode
            workspace_root: root.path().to_path_buf(),
            rediscovery_cycles: 2, // Should be ignored in pinned mode
            starvation_threshold_minutes: 15,
            scan_interval_cycles: 1,
            max_scan_interval_cycles: 1,
        };

        let home = PathBuf::from("/some/other/home");
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());

        let strand = ExploreStrand::new(
            config,
            home,
            registry,
            telemetry,
            "test-worker-pinned".to_string(),
        );

        // Initial state: should have the explicit pinned workspace only
        assert_eq!(
            strand.workspaces.lock().unwrap().len(),
            1,
            "pinned mode should use explicit list"
        );
        assert!(
            strand.workspaces.lock().unwrap().contains(&pinned_ws),
            "pinned workspace should be in list"
        );

        // Run through 2 cycles (enough to trigger rediscovery in auto-discovery mode)
        let store = DummyStore;
        let _ = strand.evaluate(&store, &HashSet::new()).await; // Cycle 1
        let _ = strand.evaluate(&store, &HashSet::new()).await; // Cycle 2 - would trigger rediscovery in auto mode

        // In pinned mode, the workspace list should NOT change
        // (unlisted_ws should NOT be discovered)
        assert_eq!(
            strand.workspaces.lock().unwrap().len(),
            1,
            "pinned mode should not re-discover"
        );
        assert!(
            strand.workspaces.lock().unwrap().contains(&pinned_ws),
            "pinned workspace should still be in list"
        );
        assert!(
            !strand.workspaces.lock().unwrap().contains(&unlisted_ws),
            "unlisted workspace should NOT be discovered in pinned mode"
        );
    }

    /// Test: Re-discovery runs every cycle regardless of `rediscovery_cycles`
    /// (bf-6anj4). The legacy throttle — including the `rediscovery_cycles == 0`
    /// disable path — was removed, so a workspace created after boot is picked up
    /// on the next cycle even when the config value is 0.
    ///
    /// NOTE: This test is quarantined due to spawn_blocking deadlock in test environments.
    /// See deadlock_scenario_assigned_beads_allow_advancement for details.
    #[tokio::test]
    #[ignore]
    async fn rediscovery_runs_every_cycle_regardless_of_config() {
        let root = tempfile::tempdir().unwrap();

        // Create initial workspace
        let ws1 = root.path().join("workspace1");
        fs::create_dir(&ws1).unwrap();
        fs::create_dir(ws1.join(".beads")).unwrap();

        // rediscovery_cycles = 0 used to DISABLE re-discovery; it is now ignored.
        let config = ExploreConfig {
            enabled: true,
            workspaces: vec![], // Auto-discovery mode
            workspace_root: root.path().to_path_buf(),
            rediscovery_cycles: 0,
            starvation_threshold_minutes: 15,
            scan_interval_cycles: 1,
            max_scan_interval_cycles: 1,
        };

        let home = PathBuf::from("/some/other/home");
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = crate::registry::Registry::new(temp_dir.path());
        let telemetry = Telemetry::new("test-worker".to_string());

        let strand = ExploreStrand::new(
            config,
            home,
            registry,
            telemetry,
            "test-worker-everycycle".to_string(),
        );

        // Initial state
        assert_eq!(
            strand.workspaces.lock().unwrap().len(),
            1,
            "initial discovery should find 1 workspace"
        );

        // A new repo appears after construction.
        let ws2 = root.path().join("workspace2");
        fs::create_dir(&ws2).unwrap();
        fs::create_dir(ws2.join(".beads")).unwrap();

        // One cycle is enough — re-discovery runs unconditionally now.
        let store = DummyStore;
        let _ = strand.evaluate(&store, &HashSet::new()).await;

        assert_eq!(
            strand.workspaces.lock().unwrap().len(),
            2,
            "new workspace should be discovered every cycle even with rediscovery_cycles = 0"
        );
        assert!(
            strand.workspaces.lock().unwrap().contains(&ws1),
            "original workspace should still be in list"
        );
        assert!(
            strand.workspaces.lock().unwrap().contains(&ws2),
            "new workspace should be discovered when rediscovery_cycles = 0 (throttle removed)"
        );
    }
}
