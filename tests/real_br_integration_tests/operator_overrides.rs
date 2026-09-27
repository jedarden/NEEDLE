use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;

use needle::operator_override::{
    detect_workspace_overrides, record_detections, OverrideKind, OPERATOR_OVERRIDE_LOG,
    REFLECTION_TRIGGER_LOG,
};

fn git(workspace: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(["-C", workspace.to_str().expect("fixture path is UTF-8")])
        .args(args)
        .output()
        .expect("git is installed");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git fixture output is UTF-8")
        .trim()
        .to_string()
}

fn commit(workspace: &Path, message: &str) -> String {
    let output = Command::new("git")
        .args(["-C", workspace.to_str().expect("fixture path is UTF-8")])
        .args([
            "-c",
            "user.name=operator-alice",
            "-c",
            "user.email=operator@example.invalid",
            "commit",
            "--quiet",
            "-m",
            message,
        ])
        .output()
        .expect("git is installed");
    assert!(
        output.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    git(workspace, &["rev-parse", "HEAD"])
}

#[test]
fn fixture_workspace_detects_operator_reopen_status_reopen_and_revert_once() {
    let workspace = tempfile::tempdir().expect("isolated workspace");
    let beads = workspace.path().join(".beads/checkpoint");
    fs::create_dir_all(&beads).expect("checkpoint directory");
    git(workspace.path(), &["init", "--quiet"]);

    fs::write(workspace.path().join("feature.txt"), "operator fixture\n").expect("feature file");
    git(workspace.path(), &["add", "feature.txt"]);
    let feature_commit = commit(workspace.path(), "Add operator fixture feature");
    git(
        workspace.path(),
        &[
            "-c",
            "user.name=operator-alice",
            "-c",
            "user.email=operator@example.invalid",
            "revert",
            "--no-edit",
            &feature_commit,
        ],
    );

    let records = [
        serde_json::json!({
            "record_type": "attempt_outcome",
            "attempt_outcome": {
                "attempt_id": "attempt-reopen",
                "issue_id": "bead-reopen",
                "evidence_refs": [format!("commit:{feature_commit}")]
            }
        }),
        serde_json::json!({
            "record_type": "attempt_outcome",
            "attempt_outcome": {
                "attempt_id": "attempt-status",
                "issue_id": "bead-status"
            }
        }),
        serde_json::json!({
            "record_type": "attempt_outcome",
            "attempt_outcome": {
                "attempt_id": "attempt-mend",
                "issue_id": "bead-mend"
            }
        }),
        serde_json::json!({
            "record_type": "event",
            "event": {
                "origin_store_uuid": "fixture-store",
                "origin_event_sequence": 10,
                "issue_id": "bead-reopen",
                "kind": "reopened",
                "actor": "operator-alice",
                "detail": {"prior_base_status": "closed", "resulting_base_status": "open"}
            }
        }),
        serde_json::json!({
            "record_type": "event",
            "event": {
                "origin_store_uuid": "fixture-store",
                "origin_event_sequence": 11,
                "issue_id": "bead-status",
                "kind": "status_changed",
                "actor": "operator-alice",
                "detail": {"from": "deferred", "to": "open"}
            }
        }),
        serde_json::json!({
            "record_type": "event",
            "event": {
                "origin_store_uuid": "fixture-store",
                "origin_event_sequence": 12,
                "issue_id": "bead-mend",
                "kind": "reopened",
                "actor": "system",
                "detail": {"source": "mend", "prior_assignee": "worker-mend"}
            }
        }),
    ];
    fs::write(
        workspace.path().join(".beads/checkpoint/forensic.jsonl"),
        records
            .iter()
            .map(|record| serde_json::to_string(record).expect("fixture JSON"))
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .expect("forensic fixture");

    let workers = BTreeSet::from(["worker-mend".to_string()]);
    let report = detect_workspace_overrides(workspace.path(), &workers).expect("detect overrides");
    assert_eq!(report.overrides.len(), 3);
    assert!(report
        .overrides
        .iter()
        .any(|event| event.attempt_id == "attempt-reopen" && event.kind == OverrideKind::Reopen));
    assert!(report
        .overrides
        .iter()
        .any(|event| event.attempt_id == "attempt-status" && event.kind == OverrideKind::Reopen));
    assert!(report.overrides.iter().any(|event| {
        event.attempt_id == "attempt-reopen"
            && event.kind == OverrideKind::Revert
            && event.actor == "operator-alice"
    }));
    assert_eq!(report.reflection_triggers.len(), 3);

    let first = record_detections(workspace.path(), &report).expect("record first detection");
    assert_eq!(first.overrides.len(), 3);
    let second = record_detections(workspace.path(), &report).expect("replay detection");
    assert!(second.overrides.is_empty(), "replay must be idempotent");
    assert_eq!(
        fs::read_to_string(workspace.path().join(OPERATOR_OVERRIDE_LOG))
            .expect("override log")
            .lines()
            .count(),
        3
    );
    assert_eq!(
        fs::read_to_string(workspace.path().join(REFLECTION_TRIGGER_LOG))
            .expect("reflection log")
            .lines()
            .count(),
        3
    );
}
