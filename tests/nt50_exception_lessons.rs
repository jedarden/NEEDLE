//! Focused behavioral contracts for N-T50 candidate lessons.

use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use needle::attempt_history::{
    append_candidate_lesson, lessons_path, record_candidate_lesson, AttemptRecord, FailureEvidence,
    GateDiagnostic, ToolErrorEvidence, LESSONS_DATA_NAMESPACE, LESSONS_DATA_SCHEMA_REF,
    SCHEMA_VERSION,
};
use needle::bead_store::{BeadStore, Filters, RepairReport};
use needle::config::{Config, RetrievalConfig};
use needle::learning::{
    build_candidate_lesson, candidate_lessons_from_data_value, candidate_lessons_to_data_value,
    CandidateConfidence, InterventionSummary,
};
use needle::retrieval::{render as render_retrieval, retrieve, RetrievalRequest};
use needle::sanitize::{CustomPattern, Sanitizer};
use needle::types::{Bead, BeadId, ClaimResult};

#[derive(Default)]
struct MirrorStore {
    writes: Mutex<Vec<(String, String, serde_json::Value)>>,
}

#[async_trait]
impl BeadStore for MirrorStore {
    async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
        Ok(Vec::new())
    }

    async fn list_all(&self) -> Result<Vec<Bead>> {
        Ok(Vec::new())
    }

    async fn show(&self, _id: &BeadId) -> Result<Bead> {
        anyhow::bail!("fixture store does not show beads")
    }

    async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
        Ok(ClaimResult::NotClaimable {
            reason: "fixture".to_string(),
        })
    }

    async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
        Ok(ClaimResult::NotClaimable {
            reason: "fixture".to_string(),
        })
    }

    async fn release(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn block(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn reopen(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        Ok(())
    }

    async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        Ok(())
    }

    async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
        Ok(BeadId::from("needle-fixture-child"))
    }

    async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn remove_dependency(&self, _blocked_id: &BeadId, _blocker_id: &BeadId) -> Result<()> {
        Ok(())
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

    async fn set_data(
        &self,
        _id: &BeadId,
        namespace: &str,
        schema_ref: &str,
        value: &serde_json::Value,
    ) -> Result<bool> {
        self.writes.lock().unwrap().push((
            namespace.to_string(),
            schema_ref.to_string(),
            value.clone(),
        ));
        Ok(true)
    }

    fn has_valid_store(&self) -> bool {
        true
    }
}

fn sanitizer() -> Sanitizer {
    Sanitizer::new(&[CustomPattern {
        id: "fixture-secret".to_string(),
        pattern: r"secret=([A-Za-z0-9_-]+)".to_string(),
        entropy: None,
    }])
    .expect("fixture sanitizer")
}

fn failure_record() -> AttemptRecord {
    AttemptRecord {
        schema_version: SCHEMA_VERSION,
        attempt_id: "att-failed".to_string(),
        recorded_at: "2026-09-16T10:00:00.000Z".to_string(),
        worker: "fixture-worker".to_string(),
        adapter: "fixture-failing-adapter".to_string(),
        model: Some("fixture-model".to_string()),
        outcome: "work_failure".to_string(),
        terminal_reason: Some("gate:cargo-test".to_string()),
        exit_code: 1,
        requested_action: "Released".to_string(),
        commits: Vec::new(),
        duration_ms: 100,
        failure_summary: Some("secret=summary-secret".to_string()),
        failure_evidence: Some(FailureEvidence {
            final_message: Some("secret=final-secret".to_string()),
            tool_errors: vec![ToolErrorEvidence {
                tool_name: "shell".to_string(),
                signature: "error[E0308] secret=signature-secret".to_string(),
                excerpt: "mismatched types; secret=excerpt-secret".to_string(),
            }],
            gate_diagnostics: vec![GateDiagnostic {
                gate_name: "cargo-test".to_string(),
                error_block: "test result: FAILED; secret=gate-secret".to_string(),
            }],
        }),
        wip_patch: None,
    }
}

fn success_record(outcome: &str) -> AttemptRecord {
    AttemptRecord {
        schema_version: SCHEMA_VERSION,
        attempt_id: "att-succeeded".to_string(),
        recorded_at: "2026-09-16T10:05:00.000Z".to_string(),
        worker: "fixture-worker".to_string(),
        adapter: "fixture-success-adapter".to_string(),
        model: Some("fixture-model".to_string()),
        outcome: outcome.to_string(),
        terminal_reason: None,
        exit_code: 0,
        requested_action: "Closed".to_string(),
        commits: vec!["abc123".to_string()],
        duration_ms: 200,
        failure_summary: None,
        failure_evidence: None,
        wip_patch: None,
    }
}

fn intervention() -> InterventionSummary {
    InterventionSummary {
        changed_paths: vec!["src/lib.rs".to_string(), "secret=path-secret".to_string()],
        commit_subjects: vec!["fix: secret=subject-secret".to_string()],
        gate_deltas: vec!["cargo-test: fail -> pass secret=delta-secret".to_string()],
    }
}

fn candidate() -> needle::learning::CandidateLesson {
    build_candidate_lesson(
        &[failure_record(), success_record("verified_success")],
        intervention(),
        "needle-nt50-fixture",
        "/workspace/secret=workspace-secret",
        "adapter/secret=adapter-secret",
        &sanitizer(),
    )
    .expect("fail-then-success pair should produce a candidate")
}

#[test]
fn fail_then_success_produces_one_sanitized_unevaluated_candidate() {
    let lesson = candidate();

    assert_eq!(lesson.schema_version, 1);
    assert_eq!(lesson.evidence_refs, vec!["att-failed", "att-succeeded"]);
    assert_eq!(lesson.confidence, CandidateConfidence::Unevaluated);
    assert!(lesson
        .failure_signatures
        .iter()
        .any(|signature| signature.contains("error[E0308]")));
    assert_eq!(lesson.intervention_summary.changed_paths.len(), 2);

    let encoded = serde_json::to_string(&lesson).expect("candidate JSON");
    assert!(encoded.contains("[REDACTED:fixture-secret]"));
    for secret in [
        "summary-secret",
        "final-secret",
        "signature-secret",
        "excerpt-secret",
        "gate-secret",
        "path-secret",
        "subject-secret",
        "delta-secret",
        "workspace-secret",
        "adapter-secret",
    ] {
        assert!(!encoded.contains(secret), "secret leaked in {encoded}");
    }
}

#[test]
fn success_only_and_decomposed_resolutions_produce_no_candidate() {
    assert!(build_candidate_lesson(
        &[success_record("verified_success")],
        intervention(),
        "needle-success-only",
        "/fixture",
        "fixture",
        &sanitizer(),
    )
    .is_none());
    assert!(build_candidate_lesson(
        &[failure_record(), success_record("decomposed")],
        intervention(),
        "needle-decomposed",
        "/fixture",
        "fixture",
        &sanitizer(),
    )
    .is_none());
}

#[test]
fn local_journal_and_bead_data_shape_are_idempotent() {
    let root = tempfile::tempdir().expect("fixture workspace");
    let bead_id = needle::types::BeadId::from("needle-nt50-fixture");
    let lesson = candidate();

    assert!(append_candidate_lesson(root.path(), &bead_id, &lesson).expect("first append"));
    assert!(!append_candidate_lesson(root.path(), &bead_id, &lesson).expect("replay append"));
    let text = std::fs::read_to_string(lessons_path(root.path(), &bead_id)).expect("lesson file");
    assert_eq!(text.lines().count(), 1);

    let value = candidate_lessons_to_data_value(std::slice::from_ref(&lesson));
    assert_eq!(LESSONS_DATA_NAMESPACE, "needle-lessons");
    assert_eq!(
        LESSONS_DATA_SCHEMA_REF,
        "urn:needle:schema:candidate-lesson:v1"
    );
    assert_eq!(candidate_lessons_from_data_value(&value), vec![lesson]);
}

#[tokio::test]
async fn candidate_is_mirrored_to_bead_data_once() {
    let root = tempfile::tempdir().expect("fixture workspace");
    let bead_id = BeadId::from("needle-nt50-fixture");
    let lesson = candidate();
    let store = MirrorStore::default();

    record_candidate_lesson(root.path(), &bead_id, lesson.clone(), &store, true).await;
    record_candidate_lesson(root.path(), &bead_id, lesson, &store, true).await;

    let writes = store.writes.lock().unwrap();
    assert_eq!(writes.len(), 1, "replaying the pair must not mirror again");
    assert_eq!(writes[0].0, LESSONS_DATA_NAMESPACE);
    assert_eq!(writes[0].1, LESSONS_DATA_SCHEMA_REF);
    assert_eq!(writes[0].2["lessons"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn retrieval_stub_receives_local_candidate_and_labels_returned_hit() {
    let lesson = candidate();
    let request = RetrievalRequest {
        bead_id: "needle-nt50-fixture".to_string(),
        title: "fixture".to_string(),
        workspace: "/fixture".to_string(),
        attempt: 2,
        failure_summary: "mismatched types".to_string(),
        terminal_reason: Some("gate:cargo-test".to_string()),
        local_candidates: vec![lesson.clone()],
    };
    let config = RetrievalConfig {
        enabled: true,
        command: Some(
            r#"read -r payload
case "$payload" in
  *candidate-lesson-*) printf '%s\n' '{"id":"candidate-lesson-fixture","source":"candidate","title":"unevaluated lesson","text":"inspect only"}' ;;
  *) exit 9 ;;
esac"#
                .to_string(),
        ),
        timeout_secs: 5,
        max_results: 3,
        max_bytes: 2000,
        min_attempt: 2,
    };

    let result = retrieve(&config, &request).await;
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].source, "candidate");
    assert!(render_retrieval(&result.items, 2000).contains("[candidate]"));

    let no_output = RetrievalConfig {
        command: Some("true".to_string()),
        ..config
    };
    assert!(retrieve(&no_output, &request).await.items.is_empty());
}

#[test]
fn candidate_production_is_disabled_by_default() {
    assert!(!Config::default().strands.learning.candidate_lessons.enabled);
}
