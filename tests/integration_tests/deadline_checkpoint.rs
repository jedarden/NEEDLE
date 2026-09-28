//! Focused end-to-end coverage for N-T25 deadline checkpoint accounting.
//!
//! This stays inside the existing integration harness so it can run as
//! `cargo test --test integration_tests nt25_deadline_checkpoint_contract`.
//! It never spawns a worker or Explore subprocess; the only filesystem state
//! it touches is temporary HOME, workspace, trace, and telemetry state.

use std::env;
use std::sync::{Mutex, MutexGuard};

use chrono::{Duration, Utc};
use needle::config::{Config, PromptConfig};
use needle::dispatch::AgentAdapter;
use needle::outcome::{AttemptContext, OutcomeHandler};
use needle::prompt::PromptBuilder;
use needle::telemetry::Telemetry;
use needle::types::{AgentOutcome, Bead, BeadId, BeadStatus};
use tempfile::TempDir;

static HOME_LOCK: Mutex<()> = Mutex::new(());

struct IsolatedHome {
    home: TempDir,
    original: Option<std::ffi::OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl IsolatedHome {
    fn new() -> Self {
        let lock = HOME_LOCK.lock().expect("HOME lock");
        let original = env::var_os("HOME");
        let home = tempfile::tempdir().expect("isolated HOME");
        env::set_var("HOME", home.path());
        Self {
            home,
            original,
            _lock: lock,
        }
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        match self.original.as_ref() {
            Some(home) => env::set_var("HOME", home),
            None => env::remove_var("HOME"),
        }
    }
}

fn adapter(yaml: &str) -> AgentAdapter {
    serde_yaml::from_str(yaml).expect("adapter fixture should parse")
}

fn deadline_notice(timeout_secs: u64) -> String {
    format!(
        "You have {timeout_secs} seconds of wall clock. By the 75% mark ({} seconds in), commit the working checkpoint, then continue.",
        timeout_secs * 3 / 4
    )
}

fn bead(workspace: &std::path::Path) -> Bead {
    Bead {
        id: BeadId::from("needle-nt25-deadline-checkpoint"),
        title: "N-T25 checkpoint fixture".to_string(),
        body: Some("Verify deadline checkpoint tracking.".to_string()),
        priority: 2,
        status: BeadStatus::InProgress,
        assignee: Some("nt25-test-worker".to_string()),
        labels: Vec::new(),
        workspace: workspace.to_path_buf(),
        dependencies: Vec::new(),
        dependents: Vec::new(),
        comments: Vec::new(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn attempt_context(
    started_at: chrono::DateTime<Utc>,
    commit_offsets_seconds: &[i64],
) -> AttemptContext {
    AttemptContext {
        adapter: "nt25-fixture-adapter".to_string(),
        actor: "nt25-test-worker".to_string(),
        prompt_template: "pluck".to_string(),
        template_version: "pluck-default".to_string(),
        commits: commit_offsets_seconds
            .iter()
            .enumerate()
            .map(|(index, _)| format!("commit-{index}"))
            .collect(),
        commit_timestamps: Some(
            commit_offsets_seconds
                .iter()
                .map(|offset| started_at + Duration::seconds(*offset))
                .collect(),
        ),
        adapter_timeout_secs: 20,
        started_at_wall: Some(started_at),
        ..AttemptContext::default()
    }
}

fn resolved_rows(log_dir: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_dir(log_dir)
        .expect("telemetry directory")
        .flatten()
        .flat_map(|entry| {
            std::fs::read_to_string(entry.path())
                .expect("telemetry log")
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSONL row"))
                .collect::<Vec<_>>()
        })
        .filter(|row| row["event_type"] == "attempt.resolved")
        .collect()
}

#[tokio::test]
async fn nt25_deadline_checkpoint_contract() {
    let isolated_home = IsolatedHome::new();
    let workspace = tempfile::tempdir().expect("temporary workspace");

    // Adapter-specific policy is the source of the rendered deadline. A
    // legacy adapter and a hard-deadline adapter must not share a constant.
    let legacy = adapter(
        "name: nt25-legacy\nagent_cli: codex\ninvoke_template: fixture\ntimeout_secs: 3600\n",
    );
    let hard = adapter(
        "name: nt25-hard\nagent_cli: codex\ninvoke_template: fixture\ntimeout_secs: 0\nidle_timeout_secs: 90\nhard_timeout_secs: 2400\n",
    );
    assert_eq!(legacy.wall_clock_timeout_secs(600), 3600);
    assert_eq!(hard.wall_clock_timeout_secs(600), 2400);

    let prompt_builder = PromptBuilder::new(&PromptConfig::default());
    let work_bead = bead(workspace.path());
    let legacy_prompt = prompt_builder
        .build_pluck_with_history(
            &work_bead,
            workspace.path(),
            "nt25-test-worker",
            "",
            "",
            &deadline_notice(legacy.wall_clock_timeout_secs(600)),
        )
        .expect("legacy prompt");
    let hard_prompt = prompt_builder
        .build_pluck_with_history(
            &work_bead,
            workspace.path(),
            "nt25-test-worker",
            "",
            "",
            &deadline_notice(hard.wall_clock_timeout_secs(600)),
        )
        .expect("hard-deadline prompt");
    assert!(legacy_prompt.content.contains("3600 seconds of wall clock"));
    assert!(legacy_prompt.content.contains("2700 seconds in"));
    assert!(hard_prompt.content.contains("2400 seconds of wall clock"));
    assert!(hard_prompt.content.contains("1800 seconds in"));
    assert!(!hard_prompt.content.contains("3600 seconds"));

    // No hard deadline means no checkpoint claim is made in the prompt.
    let idle_only = adapter(
        "name: nt25-idle-only\nagent_cli: codex\ninvoke_template: fixture\ntimeout_secs: 0\nidle_timeout_secs: 90\nhard_timeout_secs: 0\n",
    );
    assert_eq!(idle_only.wall_clock_timeout_secs(600), 0);
    let unlimited_prompt = prompt_builder
        .build_pluck_with_history(&work_bead, workspace.path(), "nt25-test-worker", "", "", "")
        .expect("unlimited prompt");
    assert!(!unlimited_prompt.content.contains("wall clock"));

    // Resolve two terminal attempts through the shared ledger choke point:
    // one commit beats 75% of the 20-second deadline, while the other does
    // not. Both rows must carry the field, including a counted zero.
    let log_dir = isolated_home.home.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("telemetry log directory");
    let telemetry = Telemetry::with_log_dir("nt25-test-worker".to_string(), &log_dir);
    telemetry.start();
    let mut config = Config::default();
    config.strands.explore.enabled = false;
    config.strands.explore.workspace_root = isolated_home.home.path().join("explore-root");
    let handler = OutcomeHandler::new(config, telemetry.clone());
    let started_at = Utc::now();
    let output = AgentOutcome {
        exit_code: 124,
        stdout: String::new(),
        stderr: "deadline fixture".to_string(),
    };

    handler.set_attempt_context(attempt_context(started_at, &[14]));
    handler.handle_stale_ownership(&work_bead, &output).await;
    handler.set_attempt_context(attempt_context(started_at, &[15]));
    handler.handle_stale_ownership(&work_bead, &output).await;
    telemetry.shutdown().await;

    let rows = resolved_rows(&log_dir);
    assert_eq!(rows.len(), 2, "each terminal fixture must resolve once");
    assert_eq!(rows[0]["data"]["schema_version"], 3);
    assert_eq!(rows[1]["data"]["schema_version"], 3);
    assert_eq!(rows[0]["data"]["commits_before_deadline"], 1);
    assert_eq!(rows[1]["data"]["commits_before_deadline"], 0);
    assert!(rows.iter().all(|row| {
        row["data"]
            .get("commits_before_deadline")
            .is_some_and(serde_json::Value::is_number)
    }));
}
