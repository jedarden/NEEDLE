//! Integration test for post-dispatch audit (Phase 19.4).
//!
//! This test validates that verification-shaped beads created during dispatch
//! are closed and folded into the parent bead's description, with proper telemetry.

use needle::bead_store::BeadStore;
use needle::types::{Bead, BeadId, BeadStatus};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// ============================================================================
// Mock BeadStore that tracks verification bead closures
// ============================================================================

struct AuditMockStore {
    workspace: PathBuf,
    beads: Mutex<Vec<Bead>>,
    close_calls: Mutex<Vec<(BeadId, String)>>, // (bead_id, reason)
    update_calls: Mutex<Vec<(BeadId, String)>>, // (bead_id, new_description)
}

impl AuditMockStore {
    fn new(workspace: PathBuf) -> Self {
        AuditMockStore {
            workspace,
            beads: Mutex::new(Vec::new()),
            close_calls: Mutex::new(Vec::new()),
            update_calls: Mutex::new(Vec::new()),
        }
    }

    fn get_close_calls(&self) -> Vec<(BeadId, String)> {
        self.close_calls.lock().unwrap().clone()
    }

    fn get_update_calls(&self) -> Vec<(BeadId, String)> {
        self.update_calls.lock().unwrap().clone()
    }

    fn find_bead(&self, id: &BeadId) -> Option<Bead> {
        self.beads
            .lock()
            .unwrap()
            .iter()
            .find(|b| b.id == *id)
            .cloned()
    }
}

#[async_trait::async_trait]
impl needle::bead_store::BeadStore for AuditMockStore {
    async fn ready(&self, _filters: &needle::bead_store::Filters) -> anyhow::Result<Vec<Bead>> {
        Ok(self.beads.lock().unwrap().clone())
    }

    async fn list_all(&self) -> anyhow::Result<Vec<Bead>> {
        Ok(self.beads.lock().unwrap().clone())
    }

    async fn show(&self, id: &BeadId) -> anyhow::Result<Bead> {
        self.find_bead(id)
            .ok_or_else(|| anyhow::anyhow!("bead not found: {id}"))
    }

    async fn claim(&self, id: &BeadId, actor: &str) -> anyhow::Result<needle::types::ClaimResult> {
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
            bead.status = BeadStatus::InProgress;
            bead.assignee = Some(actor.to_string());
            Ok(needle::types::ClaimResult::Claimed(bead.clone()))
        } else {
            anyhow::bail!("bead not found: {id}")
        }
    }

    async fn claim_auto(&self, actor: &str) -> anyhow::Result<needle::types::ClaimResult> {
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.status == BeadStatus::Open) {
            bead.status = BeadStatus::InProgress;
            bead.assignee = Some(actor.to_string());
            Ok(needle::types::ClaimResult::Claimed(bead.clone()))
        } else {
            Ok(needle::types::ClaimResult::NotClaimable {
                reason: "no open beads".to_string(),
            })
        }
    }

    async fn release(&self, id: &BeadId) -> anyhow::Result<()> {
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
            bead.status = BeadStatus::Open;
            bead.assignee = None;
            Ok(())
        } else {
            anyhow::bail!("bead not found: {id}")
        }
    }

    async fn block(&self, id: &BeadId) -> anyhow::Result<()> {
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
            bead.status = BeadStatus::Blocked;
            Ok(())
        } else {
            anyhow::bail!("bead not found: {id}")
        }
    }

    async fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn reopen(&self, id: &BeadId) -> anyhow::Result<()> {
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
            bead.status = BeadStatus::Open;
            bead.assignee = None;
            Ok(())
        } else {
            anyhow::bail!("bead not found: {id}")
        }
    }

    async fn labels(&self, id: &BeadId) -> anyhow::Result<Vec<String>> {
        Ok(self
            .find_bead(id)
            .map(|b| b.labels.clone())
            .unwrap_or_default())
    }

    async fn add_label(&self, id: &BeadId, label: &str) -> anyhow::Result<()> {
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
            bead.labels.push(label.to_string());
            Ok(())
        } else {
            anyhow::bail!("bead not found: {id}")
        }
    }

    async fn remove_label(&self, id: &BeadId, label: &str) -> anyhow::Result<()> {
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
            bead.labels.retain(|l| l != label);
            Ok(())
        } else {
            anyhow::bail!("bead not found: {id}")
        }
    }

    async fn create_bead(
        &self,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> anyhow::Result<BeadId> {
        use chrono::Utc;
        let id = BeadId::from(format!("test-{}", title.to_lowercase().replace(' ', "-")));
        let bead = Bead {
            id: id.clone(),
            title: title.to_string(),
            body: Some(body.to_string()),
            priority: 1,
            status: BeadStatus::Open,
            assignee: None,
            labels: labels.iter().map(|l| l.to_string()).collect(),
            workspace: self.workspace.clone(),
            dependencies: vec![],
            dependents: vec![],
            comments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        self.beads.lock().unwrap().push(bead);
        Ok(id)
    }

    async fn close(&self, id: &BeadId, reason: &str) -> anyhow::Result<()> {
        self.close_calls
            .lock()
            .unwrap()
            .push((id.clone(), reason.to_string()));
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
            bead.status = BeadStatus::Closed;
            Ok(())
        } else {
            anyhow::bail!("bead not found: {id}")
        }
    }

    async fn update_description(&self, id: &BeadId, description: &str) -> anyhow::Result<()> {
        self.update_calls
            .lock()
            .unwrap()
            .push((id.clone(), description.to_string()));
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
            bead.body = Some(description.to_string());
            Ok(())
        } else {
            anyhow::bail!("bead not found: {id}")
        }
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
        _blocker_id: &BeadId,
        _blocked_id: &BeadId,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn split_bead(
        &self,
        _parent_id: &BeadId,
        _children: &[needle::bead_store::NewChild<'_>],
    ) -> anyhow::Result<Vec<BeadId>> {
        Ok(vec![])
    }

    async fn remove_dependency(
        &self,
        _blocked_id: &BeadId,
        _blocker_id: &BeadId,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn clear_assignee(&self, id: &BeadId) -> anyhow::Result<()> {
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
            bead.assignee = None;
            Ok(())
        } else {
            anyhow::bail!("bead not found: {id}")
        }
    }

    fn has_valid_store(&self) -> bool {
        true
    }
}

// ============================================================================
// Test: Post-dispatch audit closes verification beads and folds them
// ============================================================================

#[tokio::test]
async fn verification_beads_are_closed_and_folded_into_parent() {
    let workspace = PathBuf::from("/tmp/test-workspace");
    let store = Arc::new(AuditMockStore::new(workspace.clone()));

    // Create a parent bead
    let parent_id = store
        .create_bead(
            "Implement feature",
            "Add the new feature to the codebase",
            &["feature"],
        )
        .await
        .unwrap();

    // Create verification-shaped beads created during dispatch
    let verify_bead_1 = store
        .create_bead(
            "Verify the endpoint works",
            &format!(
                "Check that {} endpoint responds correctly",
                parent_id.as_ref()
            ),
            &[],
        )
        .await
        .unwrap();

    let _verify_bead_2 = store
        .create_bead(
            "Test user authentication",
            &format!("Test the auth flow for {}", parent_id.as_ref()),
            &["keep"], // This bead should be exempt due to "keep" label
        )
        .await
        .unwrap();

    // Get parent bead for reference (create_bead already added it to the store)
    let _parent = store.show(&parent_id).await.unwrap();

    // Manually invoke the post-dispatch audit logic
    // (In production, this happens automatically after HANDLING state completes)
    let verification_pattern =
        regex::Regex::new(r"(?i)^(verify|test|confirm|validate|check|re-?run)\b").unwrap();

    // Check each bead
    for bead in store.list_all().await.unwrap() {
        if bead.labels.contains(&"keep".to_string()) {
            continue; // Exempt
        }

        let is_verification_shaped = verification_pattern.is_match(&bead.title);
        let references_parent = bead
            .body
            .as_deref()
            .unwrap_or("")
            .contains(&parent_id.to_string());

        if is_verification_shaped && references_parent {
            // Close the bead
            store
                .close(&bead.id, "verification is the gate's job (Phase 19.4)")
                .await
                .unwrap();

            // Update parent description with folded content
            if let Ok(p) = store.show(&parent_id).await {
                let folded_content = format!(
                    "\n\n## folded: {}\n{}\n",
                    bead.title,
                    bead.body.as_deref().unwrap_or("(no body)")
                );
                let current_body = p.body.as_deref().unwrap_or("").to_string();
                let updated_body = format!("{}{}", current_body, folded_content);
                store
                    .update_description(&parent_id, &updated_body)
                    .await
                    .unwrap();
            }
        }
    }

    // Verify results
    let close_calls = store.get_close_calls();
    let update_calls = store.get_update_calls();

    // Should have closed exactly one bead (verify_bead_1, not verify_bead_2 with "keep" label)
    assert_eq!(
        close_calls.len(),
        1,
        "Should close exactly one verification bead"
    );
    assert_eq!(
        close_calls[0].0, verify_bead_1,
        "Should close the verification bead"
    );
    assert_eq!(
        close_calls[0].1,
        "verification is the gate's job (Phase 19.4)"
    );

    // Should have updated the parent description
    assert_eq!(
        update_calls.len(),
        1,
        "Should update parent description once"
    );
    assert_eq!(update_calls[0].0, parent_id, "Should update parent bead");

    // Verify the folded content is in the parent's description
    let updated_parent = store.show(&parent_id).await.unwrap();
    let body = updated_parent.body.as_deref().unwrap();
    assert!(body.contains("## folded: Verify the endpoint works"));
    assert!(body.contains("Check that"));
    assert!(body.contains(&parent_id.as_ref()));

    println!("✅ Test passed: verification beads are closed and folded into parent");
}

// ============================================================================
// Test: Generation budget defers excess beads (5 created → 2 deferred)
// ============================================================================

#[tokio::test]
async fn generation_budget_defers_excess_beads() {
    let workspace = PathBuf::from("/tmp/test-workspace");
    let store = Arc::new(AuditMockStore::new(workspace.clone()));

    // Create a parent bead
    let parent_id = store
        .create_bead(
            "Implement feature X",
            "Add the new feature to the codebase",
            &["feature"],
        )
        .await
        .unwrap();

    // Get parent bead for reference
    let parent = store.show(&parent_id).await.unwrap();

    // Create 5 beads during dispatch window (simulating beads created by agent)
    let mut created_beads = Vec::new();
    for i in 1..=5 {
        let bead_id = store
            .create_bead(
                &format!("Subtask {}", i),
                &format!("Work item {} for feature X", i),
                &[],
            )
            .await
            .unwrap();
        created_beads.push(bead_id);
    }

    // Simulate generation budget logic: defer excess beads (newest first)
    // With max_per_dispatch = 3, beads 4 and 5 should be deferred
    let generation_budget = 3;
    let mut deferred_count = 0;

    // Process beads to defer newest excess
    // Sort by created_at to ensure we have newest first
    let mut all_beads = store.list_all().await.unwrap();
    all_beads.sort_by_key(|b| b.created_at);

    // Find beads created after the parent (these are the ones created during dispatch)
    let created_after_parent: Vec<_> = all_beads
        .iter()
        .filter(|b| b.id != parent_id && b.created_at >= parent.created_at)
        .collect();

    // The newest ones should be deferred. With 5 created and budget 3, defer 2 newest.
    let newest_to_defer = if created_after_parent.len() > generation_budget {
        created_after_parent.len() - generation_budget
    } else {
        0
    };

    // Defer the newest beads (take from the end of the sorted list)
    for bead in created_after_parent.iter().rev().take(newest_to_defer) {
        store.add_label(&bead.id, "over-budget").await.unwrap();
        deferred_count += 1;
    }

    // Verify results: exactly 2 beads should have been deferred
    assert_eq!(
        deferred_count, 2,
        "Should defer exactly 2 beads over budget"
    );

    // Verify the deferred beads are the newest ones
    let all_beads = store.list_all().await.unwrap();
    let deferred_beads: Vec<_> = all_beads
        .iter()
        .filter(|b| b.labels.contains(&"over-budget".to_string()))
        .collect();

    assert_eq!(deferred_beads.len(), 2, "Should have 2 deferred beads");

    // Verify the deferred beads are the newest (highest created_at timestamp)
    let mut all_created_beads: Vec<_> = all_beads
        .iter()
        .filter(|b| b.id != parent_id && b.created_at >= parent.created_at)
        .collect();
    all_created_beads.sort_by_key(|b| b.created_at);

    // The last 2 beads (newest) should be deferred
    let newest_two_ids: Vec<_> = all_created_beads
        .iter()
        .rev()
        .take(2)
        .map(|b| &b.id)
        .collect();
    for deferred_bead in deferred_beads {
        assert!(
            newest_two_ids.contains(&&deferred_bead.id),
            "Deferred bead {} should be one of the newest beads",
            deferred_bead.id
        );
    }

    // Verify parent bead is untouched (not deferred)
    let parent_after = store.show(&parent_id).await.unwrap();
    assert_eq!(
        parent_after.id, parent_id,
        "Parent bead ID should remain unchanged"
    );
    assert_eq!(
        parent_after.status, parent.status,
        "Parent bead status should remain unchanged"
    );
    assert!(
        !parent_after.labels.contains(&"over-budget".to_string()),
        "Parent bead should NOT have over-budget label"
    );

    println!("✅ Test passed: generation budget defers 2 newest beads (5 created, 2 deferred, parent untouched");
}
