//! Regression coverage for per-workspace validation-gate resolution.
//!
//! The worker configuration is loaded from the gate-declaring home fixture,
//! while the bead is deliberately assigned to a second fixture.  The command
//! gates append to sentinel files, so these tests observe which workspace's
//! gate actually ran instead of inferring it from the final outcome alone.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;

use needle::bead_store::{BeadStore, Filters, RepairReport};
use needle::config::{Config, ConfigLoader};
use needle::outcome::OutcomeHandler;
use needle::telemetry::Telemetry;
use needle::types::{AgentOutcome, Bead, BeadId, BeadStatus, ClaimResult, ClaimStatus, Outcome};
use needle::validation::RunIn;

const WORKER: &str = "gate-resolution-regression-worker";

/// The outcome handler only needs a small part of the store contract for a
/// successful dispatch.  Keeping this store in the integration target makes
/// the test exercise the public handler boundary without creating or mutating
/// a real operator bead database.
struct OutcomeStore {
    bead: Bead,
    labels: Mutex<Vec<String>>,
}

impl OutcomeStore {
    fn new(bead: Bead) -> Self {
        Self {
            labels: Mutex::new(bead.labels.clone()),
            bead,
        }
    }
}

#[async_trait]
impl BeadStore for OutcomeStore {
    async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
        Ok(Vec::new())
    }

    async fn list_all(&self) -> Result<Vec<Bead>> {
        Ok(vec![self.bead.clone()])
    }

    async fn show(&self, _id: &BeadId) -> Result<Bead> {
        let mut bead = self.bead.clone();
        bead.status = BeadStatus::Done;
        bead.labels = self.labels.lock().unwrap().clone();
        Ok(bead)
    }

    async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
        anyhow::bail!("claim is not used by gate-resolution tests")
    }

    async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
        anyhow::bail!("claim_auto is not used by gate-resolution tests")
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
        Ok(self.labels.lock().unwrap().clone())
    }

    async fn add_label(&self, _id: &BeadId, label: &str) -> Result<()> {
        self.labels.lock().unwrap().push(label.to_string());
        Ok(())
    }

    async fn remove_label(&self, _id: &BeadId, label: &str) -> Result<()> {
        self.labels
            .lock()
            .unwrap()
            .retain(|current| current != label);
        Ok(())
    }

    async fn create_bead(&self, _title: &str, _body: &str, _labels: &[&str]) -> Result<BeadId> {
        Ok(BeadId::from("needle-gate-resolution-child"))
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

    fn has_valid_store(&self) -> bool {
        true
    }

    async fn claim_status(&self, _id: &BeadId) -> Result<ClaimStatus> {
        Ok(ClaimStatus {
            status: BeadStatus::InProgress,
            assignee: Some(WORKER.to_string()),
            revision: None,
            claim_epoch: None,
        })
    }
}

struct TwoWorkspaceFixture {
    _root: tempfile::TempDir,
    home: PathBuf,
    target: PathBuf,
    home_sentinel: PathBuf,
    target_sentinel: PathBuf,
}

impl TwoWorkspaceFixture {
    fn new(mode: RunIn, target_has_gate: bool) -> Self {
        let root = tempfile::tempdir().expect("fixture root");
        let home = root.path().join("home-gates");
        let target = root.path().join("target-workspace");
        let sentinel_dir = root.path().join("sentinels");
        fs::create_dir_all(&home).expect("home fixture");
        fs::create_dir_all(&target).expect("target fixture");
        fs::create_dir_all(&sentinel_dir).expect("sentinel directory");

        let home_sentinel = sentinel_dir.join("home-gate-ran");
        let target_sentinel = sentinel_dir.join("target-gate-ran");
        let home_command = append_sentinel_command("home", &home_sentinel);
        let target_command = append_sentinel_command("target", &target_sentinel);

        fs::write(home.join(".needle.yaml"), gate_config(&home_command, mode))
            .expect("home workspace config");
        fs::write(
            target.join(".needle.yaml"),
            if target_has_gate {
                gate_config(&target_command, mode)
            } else {
                // An explicit empty declaration is important: it identifies
                // the fixture as gate-less and opts out of language defaults.
                "gates: []\n".to_string()
            },
        )
        .expect("target workspace config");

        init_git_workspace(&home);
        init_git_workspace(&target);

        Self {
            _root: root,
            home,
            target,
            home_sentinel,
            target_sentinel,
        }
    }

    fn worker_config(&self) -> Config {
        let mut config = Config::isolated_for_test();
        config.workspace.default = self.home.clone();
        config.strands.explore.workspace_root = self.home.clone();
        config.worker.enforce_shipped_work = false;

        // Load the worker's home workspace declaration exactly as worker boot
        // does.  The regression is that this home gate must not be reused for
        // a bead whose workspace is `self.target`.
        let overrides = ConfigLoader::load_workspace(&self.home)
            .expect("load home workspace config")
            .expect("home fixture must declare a gate");
        let mut sources = Default::default();
        ConfigLoader::apply_workspace(&mut config, &overrides, &self.home, &mut sources);
        assert_eq!(config.gates.len(), 1, "home fixture gate must load");
        config
    }

    fn bead(&self) -> Bead {
        Bead {
            id: BeadId::from("needle-gate-resolution-regression"),
            title: "gate workspace resolution regression".to_string(),
            body: Some("fixture bead".to_string()),
            priority: 1,
            status: BeadStatus::InProgress,
            assignee: Some(WORKER.to_string()),
            labels: Vec::new(),
            workspace: self.target.clone(),
            dependencies: Vec::new(),
            dependents: Vec::new(),
            comments: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }
}

fn append_sentinel_command(marker: &str, sentinel: &Path) -> String {
    format!("printf '%s\\n' {marker} >> {}", shell_quote(sentinel))
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn gate_config(command: &str, mode: RunIn) -> String {
    let mode = match mode {
        RunIn::Clean => "clean",
        RunIn::Workspace => "workspace",
    };
    format!("gates:\n  - type: command\n    commands:\n      - \"{command}\"\n    run_in: {mode}\n")
}

fn init_git_workspace(workspace: &Path) {
    run_git(workspace, &["init", "--quiet"]);
    run_git(workspace, &["add", ".needle.yaml"]);
    run_git(
        workspace,
        &[
            "-c",
            "user.name=NEEDLE fixture",
            "-c",
            "user.email=needle-fixture@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    );
}

fn run_git(workspace: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .expect("spawn git fixture command");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn dispatch_fixture(fixture: &TwoWorkspaceFixture) -> Result<()> {
    let log_dir = tempfile::tempdir().expect("telemetry directory");
    let telemetry = Telemetry::with_log_dir(WORKER.to_string(), log_dir.path());
    telemetry.start();
    let handler = OutcomeHandler::new(fixture.worker_config(), telemetry.clone());
    let bead = fixture.bead();
    let store = OutcomeStore::new(bead.clone());
    let result = handler
        .handle(
            &store,
            &bead,
            &AgentOutcome {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            },
            false,
        )
        .await;
    telemetry.shutdown().await;
    let result = result?;
    assert_eq!(result.outcome, Outcome::Success);
    Ok(())
}

pub(super) async fn run_gate_workspace_resolution_regression() -> Result<()> {
    for (mode, target_has_gate) in [
        (RunIn::Clean, false),
        (RunIn::Workspace, false),
        (RunIn::Clean, true),
        (RunIn::Workspace, true),
    ] {
        let fixture = TwoWorkspaceFixture::new(mode, target_has_gate);
        dispatch_fixture(&fixture).await?;

        assert!(
            !fixture.home_sentinel.exists(),
            "worker home gate unexpectedly ran for {mode:?} / target_has_gate={target_has_gate}"
        );
        if target_has_gate {
            assert_eq!(
                fs::read_to_string(&fixture.target_sentinel).expect("target gate sentinel"),
                "target\n"
            );
        } else {
            assert!(
                !fixture.target_sentinel.exists(),
                "target must be gate-less for {mode:?}"
            );
        }
    }
    Ok(())
}
