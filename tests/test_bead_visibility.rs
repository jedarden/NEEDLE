//! Automated bead visibility validation test for Pluck's candidate selection.
//!
//! This test validates that beads with known "claimable" properties are visible
//! to Pluck's candidate selection. If the assertion fails, it automatically outputs
//! the query parameters, filter states, and bead metadata showing why the bead was
//! excluded.
//!
//! Division of responsibility under test (see `src/strand/pluck.rs`):
//! - The store's ready frontier owns dependency and assignee gating — `ready()`
//!   is what production `br ready` / bead-rs computes.
//! - Pluck re-applies its own client-side guards on top of whatever the
//!   frontier returned: never-relaxed labels (`deferred`, `human`, `blocked`,
//!   active quarantines), then status + assignee. These tests use a frontier
//!   that filters nothing, so every exclusion observed here is Pluck's.
//!
//! Test workflow:
//! 1. Insert a test bead with known properties
//! 2. Run Pluck's candidate query through the public `Strand::evaluate` API
//! 3. Assert the expected candidates
//! 4. On failure, output detailed diagnostics

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use chrono::Utc;

use needle::bead_store::{BeadStore, Filters, RepairReport};
use needle::strand::{PluckStrand, Strand};
use needle::telemetry::Telemetry;
use needle::types::{Bead, BeadId, BeadStatus, BrDependency, ClaimResult, StrandResult};

/// Mock bead store whose frontier filters nothing.
///
/// Production frontiers exclude assigned/blocked/labelled beads; returning
/// everything here means any exclusion Pluck performs is observable as its
/// own client-side guard, not a store behaviour.
struct MockBeadStore {
    beads: Arc<Mutex<Vec<Bead>>>,
}

impl MockBeadStore {
    fn new(beads: Vec<Bead>) -> Self {
        Self {
            beads: Arc::new(Mutex::new(beads)),
        }
    }

    fn all_beads(&self) -> Vec<Bead> {
        self.beads.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl BeadStore for MockBeadStore {
    async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
        Ok(self.all_beads())
    }

    async fn list_all(&self) -> Result<Vec<Bead>> {
        Ok(self.all_beads())
    }

    async fn show(&self, id: &BeadId) -> Result<Bead> {
        self.all_beads()
            .into_iter()
            .find(|bead| &bead.id == id)
            .ok_or_else(|| anyhow::anyhow!("bead {id} not found"))
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
        anyhow::bail!("not implemented")
    }

    async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn remove_dependency(&self, _blocked_id: &BeadId, _blocker_id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn clear_assignee(&self, id: &BeadId) -> Result<()> {
        let mut beads = self.beads.lock().unwrap();
        match beads.iter_mut().find(|bead| &bead.id == id) {
            Some(bead) => {
                bead.assignee = None;
                Ok(())
            }
            None => anyhow::bail!("bead {id} not found"),
        }
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

    fn has_valid_store(&self) -> bool {
        true
    }
}

/// A fake-but-stable workspace slug for diagnostics. Pluck writes its
/// no-candidate and query-execution diagnostics under
/// `$HOME/.needle/diagnostics/<slug>/` — only the slug of this path is used,
/// never the path itself.
const FIXTURE_WORKSPACE: &str = "/test/workspace";

/// Create a test bead with known "claimable" properties
fn create_test_bead() -> Bead {
    Bead {
        id: BeadId::from("test-visibility-bead-001"),
        title: "Automated Visibility Test Bead".to_string(),
        body: Some(
            "This bead validates Pluck visibility. It should always appear in candidate queries."
                .to_string(),
        ),
        priority: 0, // Highest priority
        status: BeadStatus::Open,
        assignee: None, // No assignee
        labels: vec![], // No exclusion labels
        workspace: PathBuf::from(FIXTURE_WORKSPACE),
        dependencies: vec![], // No blocking dependencies
        dependents: vec![],
        comments: vec![],
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

/// An open `blocks` edge to a bead that is still open — the shape the ready
/// frontier is responsible for keeping out of `ready()`.
fn open_blocks_dependency(id: &str) -> BrDependency {
    BrDependency {
        id: BeadId::from(id.to_string()),
        title: String::new(),
        status: "open".to_string(),
        priority: 0,
        dependency_type: "blocks".to_string(),
    }
}

/// Dump every bead in the store with the fields each Pluck filter consults.
fn print_store_inventory(store: &MockBeadStore) {
    println!("\nAll Beads in Store:");
    for (i, bead) in store.all_beads().iter().enumerate() {
        println!("  Bead {}:", i + 1);
        println!("    ID: {}", bead.id);
        println!("    Title: {}", bead.title);
        println!("    Status: {:?}", bead.status);
        println!("    Priority: {}", bead.priority);
        println!("    Assignee: {:?}", bead.assignee);
        println!("    Labels: {:?}", bead.labels);
        println!("    Dependencies: {:?}", bead.dependencies);
    }
}

#[tokio::test]
async fn test_bead_visibility_claimable_bead_appears_in_candidates() {
    // Initialize test environment
    let store = MockBeadStore::new(vec![create_test_bead()]);

    println!("=== BEAD VISIBILITY TEST ===");
    println!("Step 1: Inserted claimable test bead");

    // Step 2: Run Pluck's candidate query through the public strand API
    println!("\nStep 2: Running Pluck candidate query...");

    let telemetry = Telemetry::new("test-worker".to_string());
    let pluck = PluckStrand::new(vec![], telemetry);
    let exclusions = HashSet::new();

    let result = pluck.evaluate(&store, &exclusions).await;

    // Step 3: Assert test bead appears in results
    println!("\nStep 3: Checking results...");

    match result {
        StrandResult::BeadFound(candidates) => {
            println!("  Found {} candidate(s)", candidates.len());

            let found = candidates
                .iter()
                .any(|b| b.id.as_ref() == "test-visibility-bead-001");

            if found {
                println!("  ✓ Test bead FOUND in candidates");
                println!("\n=== TEST PASSED ===");
            } else {
                // Step 4: Output diagnostic information on failure
                println!("  ✗ Test bead NOT FOUND in candidates");
                println!("\n=== DIAGNOSTIC INFORMATION ===");

                println!("\nQuery Parameters:");
                println!("  Assignee filter: None (unassigned work)");
                println!("  Exclude labels: []");
                println!("  Exclude IDs: []");
                println!("  Relaxation tier: initial");

                println!("\nFilter States:");
                println!("  Label filter: PASSED (bead has no labels)");
                println!("  Status filter: PASSED (bead is Open)");
                println!("  Assignee filter: PASSED (bead has no assignee)");
                println!("  Dependency filter: PASSED (bead has no dependencies)");

                println!("\nCandidate List:");
                if candidates.is_empty() {
                    println!("  (empty - no candidates returned)");
                } else {
                    for (i, candidate) in candidates.iter().enumerate() {
                        println!("  Candidate {}:", i + 1);
                        println!("    ID: {}", candidate.id);
                        println!("    Title: {}", candidate.title);
                        println!("    Status: {:?}", candidate.status);
                        println!("    Priority: {}", candidate.priority);
                        println!("    Assignee: {:?}", candidate.assignee);
                        println!("    Labels: {:?}", candidate.labels);
                    }
                }

                let (open_count, excluded_count, exclusion_reasons) = pluck.last_filtering_stats();
                println!("\nFiltering Statistics:");
                println!("  Open beads: {}", open_count);
                println!("  Excluded beads: {}", excluded_count);
                println!("  Exclusion reasons: {:?}", exclusion_reasons);

                print_store_inventory(&store);

                println!("\n=== TEST FAILED ===");
                panic!(
                    "Test bead with known claimable properties was not found in Pluck candidates"
                );
            }
        }
        StrandResult::NoWork => {
            println!("  No candidates found");
            println!("\n=== DIAGNOSTIC INFORMATION ===");

            println!("\nQuery Parameters:");
            println!("  Assignee filter: None (unassigned work)");
            println!("  Exclude labels: []");
            println!("  Exclude IDs: []");
            println!("  Relaxation tier: initial");

            println!("\nFilter States:");
            println!("  Label filter: PASSED (bead has no labels)");
            println!("  Status filter: PASSED (bead is Open)");
            println!("  Assignee filter: PASSED (bead has no assignee)");
            println!("  Dependency filter: PASSED (bead has no dependencies)");

            let (open_count, excluded_count, exclusion_reasons) = pluck.last_filtering_stats();
            println!("\nFiltering Statistics:");
            println!("  Open beads: {}", open_count);
            println!("  Excluded beads: {}", excluded_count);
            println!("  Exclusion reasons: {:?}", exclusion_reasons);

            print_store_inventory(&store);

            println!("\n=== TEST FAILED ===");
            panic!("Pluck returned NoWork despite having a claimable bead");
        }
        StrandResult::Skipped { reason } => {
            panic!("Pluck strand was skipped: {reason}");
        }
        StrandResult::Error(err) => {
            panic!("Pluck strand returned error: {err:?}");
        }
        // `StrandResult` is `#[non_exhaustive]`, so a wildcard arm is required
        // from outside the crate. The remaining variants are impossible for a
        // single-bead Pluck evaluation on this store.
        other => {
            panic!("Pluck returned unexpected result: {other:?}");
        }
    }
}

/// An unassigned, unlabelled control bead.
///
/// Every exclusion test pairs the excluded bead with one of these: without it
/// the candidate set empties and Pluck enters the starvation path, where an
/// all-assigned queue is auto-repaired by *clearing* the assignees — a real
/// behaviour, but not the one these tests are pinning.
fn control_bead() -> Bead {
    Bead {
        id: BeadId::from("test-visibility-control-002"),
        ..create_test_bead()
    }
}

/// Assert `candidates` is exactly the control bead, with diagnostics.
fn assert_control_is_sole_candidate(candidates: &[Bead], excluded_reason: &str) {
    let has_control = candidates
        .iter()
        .any(|b| b.id.as_ref() == "test-visibility-control-002");
    assert!(
        has_control,
        "control bead should remain a candidate; got: {candidates:?}"
    );
    let has_excluded = candidates
        .iter()
        .any(|b| b.id.as_ref() == "test-visibility-bead-001");
    assert!(
        !has_excluded,
        "bead should be excluded ({excluded_reason}); got: {candidates:?}"
    );
}

#[tokio::test]
async fn test_bead_visibility_with_exclusion_label() {
    // A bead carrying an excluded label is filtered out by Pluck's own label
    // guard, even when the frontier returned it. The control bead stays.
    let mut labelled = create_test_bead();
    labelled.labels = vec!["deferred".to_string()];
    let store = MockBeadStore::new(vec![labelled, control_bead()]);

    println!("=== BEAD EXCLUSION LABEL TEST ===");
    println!("Test bead with 'deferred' label should be excluded; control bead should not");

    let telemetry = Telemetry::new("test-worker".to_string());
    let pluck = PluckStrand::new(vec!["deferred".to_string()], telemetry);
    let exclusions = HashSet::new();

    let result = pluck.evaluate(&store, &exclusions).await;

    match result {
        StrandResult::BeadFound(candidates) => {
            assert_control_is_sole_candidate(&candidates, "its 'deferred' label");
            println!("✓ Bead with 'deferred' label correctly excluded");
        }
        StrandResult::NoWork => {
            panic!("control bead is claimable, so Pluck must not return NoWork")
        }
        other => {
            panic!("Unexpected result: {other:?}");
        }
    }
}

#[tokio::test]
async fn test_bead_visibility_with_assignee() {
    // A bead with an assignee is filtered out by Pluck's status/assignee
    // guard, even when the frontier returned it. The control bead stays.
    // (A sole assigned bead would instead trip the stale-assignee
    // auto-repair, which clears the assignee and re-queries.)
    let mut assigned = create_test_bead();
    assigned.assignee = Some("some-worker".to_string());
    let store = MockBeadStore::new(vec![assigned, control_bead()]);

    println!("=== BEAD ASSIGNEE TEST ===");
    println!("Test bead with assignee should be excluded; control bead should not");

    let telemetry = Telemetry::new("test-worker".to_string());
    let pluck = PluckStrand::new(vec![], telemetry);
    let exclusions = HashSet::new();

    let result = pluck.evaluate(&store, &exclusions).await;

    match result {
        StrandResult::BeadFound(candidates) => {
            assert_control_is_sole_candidate(&candidates, "its assignee");
            println!("✓ Bead with assignee correctly excluded");
        }
        StrandResult::NoWork => {
            panic!("control bead is claimable, so Pluck must not return NoWork");
        }
        other => {
            panic!("Unexpected result: {other:?}");
        }
    }
}

#[tokio::test]
async fn test_bead_visibility_dependency_edges_are_the_frontiers_concern() {
    // The ready frontier owns dependency gating: a bead that reaches Pluck
    // with an open `blocks` edge is ordered dependency-aware but stays a
    // candidate. If Pluck starts re-filtering on dependencies, the frontier
    // contract has changed and this test is the notice.
    let mut test_bead = create_test_bead();
    test_bead.dependencies = vec![open_blocks_dependency("some-other-bead")];
    let store = MockBeadStore::new(vec![test_bead]);

    println!("=== BEAD DEPENDENCY EDGE TEST ===");
    println!("Frontier-returned bead with an open blocks edge must stay visible");

    let telemetry = Telemetry::new("test-worker".to_string());
    let pluck = PluckStrand::new(vec![], telemetry);
    let exclusions = HashSet::new();

    let result = pluck.evaluate(&store, &exclusions).await;

    match result {
        StrandResult::BeadFound(candidates) => {
            let found = candidates
                .iter()
                .any(|b| b.id.as_ref() == "test-visibility-bead-001");
            assert!(
                found,
                "bead returned by the frontier with an open dependency edge must remain a candidate"
            );
            println!("✓ Bead with open dependency edge stayed visible (frontier owns gating)");
        }
        StrandResult::NoWork => {
            println!("\n=== DIAGNOSTIC INFORMATION ===");
            print_store_inventory(&store);
            panic!(
                "Pluck dropped a frontier-returned bead solely for its dependency edge — \
                 dependency gating belongs to the ready frontier"
            );
        }
        other => {
            panic!("Unexpected result: {other:?}");
        }
    }
}
