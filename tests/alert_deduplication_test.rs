//! Tests for alert bead fingerprinting and deduplication.
//!
//! This test file validates that:
//! - Two identical alert causes produce the same fingerprint
//! - Different causes produce different fingerprints
//! - Open beads with matching fingerprints are deduplicated
//! - Recently closed beads suppress creation within 24h
//! - After 24h, new beads can be created again

use chrono::{DateTime, Utc};
use needle::fingerprint::{compute_fingerprint, AlertDeduplication, AlertKind};
use needle::types::{Bead, BeadId, BeadStatus};
use std::collections::HashMap;
use std::path::PathBuf;

/// Mock BeadStore for testing alert deduplication.
struct MockBeadStore {
    beads: HashMap<BeadId, Bead>,
}

impl MockBeadStore {
    fn new() -> Self {
        MockBeadStore {
            beads: HashMap::new(),
        }
    }

    fn add_bead(&mut self, bead: Bead) {
        self.beads.insert(bead.id.clone(), bead);
    }
}

#[async_trait::async_trait]
impl needle::bead_store::BeadStore for MockBeadStore {
    async fn list_all(&self) -> anyhow::Result<Vec<needle::types::Bead>> {
        Ok(self.beads.values().cloned().collect())
    }

    async fn ready(
        &self,
        _filters: &needle::bead_store::Filters,
    ) -> anyhow::Result<Vec<needle::types::Bead>> {
        Ok(vec![])
    }

    async fn show(&self, _id: &needle::types::BeadId) -> anyhow::Result<needle::types::Bead> {
        needle::anyhow::bail!("not implemented")
    }

    async fn claim(
        &self,
        _id: &needle::types::BeadId,
        _actor: &str,
    ) -> anyhow::Result<needle::types::ClaimResult> {
        needle::anyhow::bail!("not implemented")
    }

    async fn release(&self, _id: &needle::types::BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn block(&self, _id: &needle::types::BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn reopen(&self, _id: &needle::types::BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn labels(&self, _id: &needle::types::BeadId) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }

    async fn add_label(&self, _id: &needle::types::BeadId, _label: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remove_label(&self, _id: &needle::types::BeadId, _label: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn create_bead(
        &self,
        _title: &str,
        _body: &str,
        _labels: &[&str],
    ) -> anyhow::Result<needle::types::BeadId> {
        Ok(BeadId::from("test-bead".to_string()))
    }

    async fn doctor_repair(&self) -> anyhow::Result<needle::bead_store::RepairReport> {
        Ok(needle::bead_store::RepairReport::default())
    }

    async fn doctor_check(&self) -> anyhow::Result<needle::bead_store::RepairReport> {
        Ok(needle::bead_store::RepairReport::default())
    }

    async fn full_rebuild(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn add_dependency(
        &self,
        _blocker_id: &needle::types::BeadId,
        _blocked_id: &needle::types::BeadId,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remove_dependency(
        &self,
        _blocked_id: &needle::types::BeadId,
        _blocker_id: &needle::types::BeadId,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn clear_assignee(&self, _id: &needle::types::BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn claim_auto(&self, _actor: &str) -> anyhow::Result<needle::types::ClaimResult> {
        Ok(needle::types::ClaimResult::NotClaimable {
            reason: "not implemented".to_string(),
        })
    }

    fn has_valid_store(&self) -> bool {
        true
    }
}

fn make_test_bead(id: &str, title: &str, status: BeadStatus, labels: Vec<&str>) -> Bead {
    Bead {
        id: BeadId::from(id.to_string()),
        title: title.to_string(),
        body: Some("Test body".to_string()),
        priority: 1,
        status,
        assignee: None,
        labels: labels.iter().map(|s| s.to_string()).collect(),
        workspace: PathBuf::from("/tmp/test"),
        dependencies: vec![],
        dependents: vec![],
        comments: vec![],
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[test]
fn test_fingerprint_stability() {
    // Same inputs should produce the same fingerprint
    let fp1 = compute_fingerprint(
        "/home/coding/icg",
        &AlertKind::PluckStarvation,
        "open=10, excluded=5, reasons=blocked",
    );
    let fp2 = compute_fingerprint(
        "/home/coding/icg",
        &AlertKind::PluckStarvation,
        "open=10, excluded=5, reasons=blocked",
    );

    assert_eq!(
        fp1, fp2,
        "Identical inputs should produce identical fingerprints"
    );
}

#[test]
fn test_fingerprint_different_causes() {
    let fp1 = compute_fingerprint(
        "/home/coding/icg",
        &AlertKind::PluckStarvation,
        "open=10, excluded=5, reasons=blocked",
    );
    let fp2 = compute_fingerprint(
        "/home/coding/icg",
        &AlertKind::PluckStarvation,
        "open=15, excluded=3, reasons=deferred",
    );

    assert_ne!(
        fp1, fp2,
        "Different causes should produce different fingerprints"
    );
}

#[test]
fn test_fingerprint_different_workspaces() {
    let fp1 = compute_fingerprint(
        "/home/coding/icg",
        &AlertKind::PluckStarvation,
        "open=10, excluded=5",
    );
    let fp2 = compute_fingerprint(
        "/home/coding/needle",
        &AlertKind::PluckStarvation,
        "open=10, excluded=5",
    );

    assert_ne!(
        fp1, fp2,
        "Different workspaces should produce different fingerprints"
    );
}

#[test]
fn test_fingerprint_format() {
    let fp = compute_fingerprint(
        "/home/coding/icg",
        &AlertKind::PluckStarvation,
        "open=10, excluded=5",
    );

    // Check format: "fingerprint:" followed by 12 hex characters
    assert!(
        fp.starts_with("fingerprint:"),
        "Fingerprint should start with 'fingerprint:'"
    );
    assert_eq!(
        fp.len(),
        23,
        "Fingerprint should be 'fingerprint:' + 12 hex chars (23 total)"
    );

    // Check that the suffix is valid hex
    let hex_part = &fp[12..];
    assert!(
        hex_part.chars().all(|c| c.is_ascii_hexdigit()),
        "Fingerprint suffix should be valid hexadecimal"
    );
}

#[tokio::test]
async fn test_deduplication_open_bead() {
    let mut store = MockBeadStore::new();

    // Create an existing open bead with a fingerprint
    let fp = compute_fingerprint(
        "/home/coding/icg",
        &AlertKind::PluckStarvation,
        "open=10, excluded=5",
    );

    let existing_bead = make_test_bead(
        "starvation-1",
        "Starvation alert",
        BeadStatus::Open,
        vec![&fp],
    );
    store.add_bead(existing_bead);

    // Check for deduplication
    let result = needle::fingerprint::check_alert_deduplication(
        &store,
        "/home/coding/icg",
        &AlertKind::PluckStarvation,
        "open=10, excluded=5",
    )
    .await
    .unwrap();

    match result {
        AlertDeduplication::Deduplicated {
            bead_id,
            fingerprint,
        } => {
            assert_eq!(bead_id.as_ref(), "starvation-1");
            assert_eq!(fingerprint, fp);
        }
        _ => panic!("Expected Deduplicated result, got {:?}", result),
    }
}

#[tokio::test]
async fn test_deduplication_recently_closed() {
    let mut store = MockBeadStore::new();

    // Create a recently closed bead (within 24h)
    let fp = compute_fingerprint(
        "/home/coding/icg",
        &AlertKind::PluckStarvation,
        "open=10, excluded=5",
    );

    let closed_at = Utc::now() - chrono::Duration::hours(12); // 12 hours ago
    let closed_bead = make_test_bead(
        "starvation-1",
        "Starvation alert",
        BeadStatus::Closed,
        vec![&fp],
    );
    store.add_bead(closed_bead);

    // Check for deduplication
    let result = needle::fingerprint::check_alert_deduplication(
        &store,
        "/home/coding/icg",
        &AlertKind::PluckStarvation,
        "open=10, excluded=5",
    )
    .await
    .unwrap();

    match result {
        AlertDeduplication::Suppressed {
            bead_id,
            closed_at: closed,
        } => {
            assert_eq!(bead_id.as_ref(), "starvation-1");
            assert_eq!(closed, closed_at);
        }
        _ => panic!("Expected Suppressed result, got {:?}", result),
    }
}

#[tokio::test]
async fn test_deduplication_old_closed_bead() {
    let mut store = MockBeadStore::new();

    // Create a closed bead older than 24h
    let fp = compute_fingerprint(
        "/home/coding/icg",
        &AlertKind::PluckStarvation,
        "open=10, excluded=5",
    );

    let closed_at = Utc::now() - chrono::Duration::hours(48); // 48 hours ago
    let old_bead = make_test_bead(
        "starvation-1",
        "Starvation alert",
        BeadStatus::Closed,
        vec![&fp],
    );
    store.add_bead(old_bead);

    // Check for deduplication - should create new since old bead is outside suppression window
    let result = needle::fingerprint::check_alert_deduplication(
        &store,
        "/home/coding/icg",
        &AlertKind::PluckStarvation,
        "open=10, excluded=5",
    )
    .await
    .unwrap();

    match result {
        AlertDeduplication::CreateNew => {
            // Expected - old closed bead is outside suppression window
        }
        _ => panic!("Expected CreateNew result, got {:?}", result),
    }
}

#[tokio::test]
async fn test_deduplication_no_existing_bead() {
    let store = MockBeadStore::new();

    // No existing beads - should create new
    let result = needle::fingerprint::check_alert_deduplication(
        &store,
        "/home/coding/icg",
        &AlertKind::PluckStarvation,
        "open=10, excluded=5",
    )
    .await
    .unwrap();

    match result {
        AlertDeduplication::CreateNew => {
            // Expected - no existing beads
        }
        _ => panic!("Expected CreateNew result, got {:?}", result),
    }
}

#[tokio::test]
async fn test_deduplication_priority_recently_closed_over_open() {
    let mut store = MockBeadStore::new();

    let fp = compute_fingerprint(
        "/home/coding/icg",
        &AlertKind::PluckStarvation,
        "open=10, excluded=5",
    );

    // Add both an open bead and a recently closed bead
    let open_bead = make_test_bead(
        "starvation-open",
        "Starvation alert",
        BeadStatus::Open,
        vec![&fp],
    );

    let closed_at = Utc::now() - chrono::Duration::hours(6);
    let closed_bead = make_test_bead(
        "starvation-closed",
        "Starvation alert",
        BeadStatus::Closed,
        vec![&fp],
    );

    store.add_bead(open_bead);
    store.add_bead(closed_bead);

    // Priority: recently closed should take precedence over open
    let result = needle::fingerprint::check_alert_deduplication(
        &store,
        "/home/coding/icg",
        &AlertKind::PluckStarvation,
        "open=10, excluded=5",
    )
    .await
    .unwrap();

    match result {
        AlertDeduplication::Suppressed { bead_id, .. } => {
            // Recently closed should take priority
            assert_eq!(bead_id.as_ref(), "starvation-closed");
        }
        _ => panic!(
            "Expected Suppressed result (recently closed takes priority), got {:?}",
            result
        ),
    }
}
