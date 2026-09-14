//! Real child-process and operating-system lifecycle contracts.
//!
//! Every child is wrapped before any assertion so a panic cannot leak a live
//! process into another test or into the CI worker.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use needle::bead_store::{BeadStore, Filters, RepairReport};
use needle::cli::find_all_descendants;
use needle::config::{Config, GenerationConfig, PulseConfig, ScannerConfig};
use needle::dispatch::{AgentAdapter, Dispatcher, TokenExtraction};
use needle::registry::is_pid_alive;
use needle::sanitize::Sanitizer;
use needle::strand::pulse::{PulseState, PulseStrand};
use needle::strand::Strand;
#[cfg(unix)]
use needle::supervisor::reap_exited_child;
use needle::telemetry::{Telemetry, TelemetryEvent};
use needle::types::{
    Bead, BeadId, BeadStatus, ClaimResult, IdleAction, InputMethod, StrandResult, WorkerState,
};
use needle::worker::Worker;
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

struct LifecycleStore {
    beads: Mutex<Vec<Bead>>,
    created: Mutex<Vec<(String, String, Vec<String>)>>,
}

impl LifecycleStore {
    fn new(beads: Vec<Bead>) -> Self {
        Self {
            beads: Mutex::new(beads),
            created: Mutex::new(Vec::new()),
        }
    }

    fn empty() -> Self {
        Self::new(Vec::new())
    }

    fn close(&self, id: &BeadId) {
        self.beads
            .lock()
            .unwrap()
            .iter_mut()
            .find(|bead| bead.id == *id)
            .expect("fixture bead exists")
            .status = BeadStatus::Closed;
    }

    fn created_beads(&self) -> Vec<(String, String, Vec<String>)> {
        self.created.lock().unwrap().clone()
    }
}

#[async_trait]
impl BeadStore for LifecycleStore {
    async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
        Ok(self
            .beads
            .lock()
            .unwrap()
            .iter()
            .filter(|bead| bead.status == BeadStatus::Open)
            .cloned()
            .collect())
    }

    async fn list_all(&self) -> Result<Vec<Bead>> {
        Ok(self.beads.lock().unwrap().clone())
    }

    async fn show(&self, id: &BeadId) -> Result<Bead> {
        self.beads
            .lock()
            .unwrap()
            .iter()
            .find(|bead| bead.id == *id)
            .cloned()
            .ok_or_else(|| anyhow!("fixture has no bead {id}"))
    }

    async fn claim(&self, id: &BeadId, actor: &str) -> Result<ClaimResult> {
        let mut beads = self.beads.lock().unwrap();
        let bead = beads
            .iter_mut()
            .find(|bead| bead.id == *id)
            .ok_or_else(|| anyhow!("fixture has no bead {id}"))?;
        if bead.status != BeadStatus::Open {
            return Ok(ClaimResult::NotClaimable {
                reason: format!("fixture bead is {:?}", bead.status),
            });
        }
        bead.status = BeadStatus::InProgress;
        bead.assignee = Some(actor.to_string());
        Ok(ClaimResult::Claimed(bead.clone()))
    }

    async fn claim_auto(&self, actor: &str) -> Result<ClaimResult> {
        let mut beads = self.beads.lock().unwrap();
        let Some(bead) = beads
            .iter_mut()
            .find(|bead| bead.status == BeadStatus::Open)
        else {
            return Ok(ClaimResult::NotClaimable {
                reason: "no open fixture beads".to_string(),
            });
        };
        bead.status = BeadStatus::InProgress;
        bead.assignee = Some(actor.to_string());
        Ok(ClaimResult::Claimed(bead.clone()))
    }

    async fn release(&self, id: &BeadId) -> Result<()> {
        let mut beads = self.beads.lock().unwrap();
        let bead = beads
            .iter_mut()
            .find(|bead| bead.id == *id)
            .ok_or_else(|| anyhow!("fixture has no bead {id}"))?;
        bead.status = BeadStatus::Open;
        bead.assignee = None;
        Ok(())
    }

    async fn block(&self, id: &BeadId) -> Result<()> {
        self.beads
            .lock()
            .unwrap()
            .iter_mut()
            .find(|bead| bead.id == *id)
            .ok_or_else(|| anyhow!("fixture has no bead {id}"))?
            .status = BeadStatus::Blocked;
        Ok(())
    }

    async fn clear_assignee(&self, id: &BeadId) -> Result<()> {
        self.beads
            .lock()
            .unwrap()
            .iter_mut()
            .find(|bead| bead.id == *id)
            .ok_or_else(|| anyhow!("fixture has no bead {id}"))?
            .assignee = None;
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn reopen(&self, id: &BeadId) -> Result<()> {
        let mut beads = self.beads.lock().unwrap();
        let bead = beads
            .iter_mut()
            .find(|bead| bead.id == *id)
            .ok_or_else(|| anyhow!("fixture has no bead {id}"))?;
        bead.status = BeadStatus::Open;
        bead.assignee = None;
        Ok(())
    }

    async fn labels(&self, id: &BeadId) -> Result<Vec<String>> {
        Ok(self.show(id).await?.labels)
    }

    async fn add_label(&self, id: &BeadId, label: &str) -> Result<()> {
        let mut beads = self.beads.lock().unwrap();
        let bead = beads
            .iter_mut()
            .find(|bead| bead.id == *id)
            .ok_or_else(|| anyhow!("fixture has no bead {id}"))?;
        if !bead.labels.iter().any(|existing| existing == label) {
            bead.labels.push(label.to_string());
        }
        Ok(())
    }

    async fn remove_label(&self, id: &BeadId, label: &str) -> Result<()> {
        self.beads
            .lock()
            .unwrap()
            .iter_mut()
            .find(|bead| bead.id == *id)
            .ok_or_else(|| anyhow!("fixture has no bead {id}"))?
            .labels
            .retain(|existing| existing != label);
        Ok(())
    }

    async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId> {
        let mut created = self.created.lock().unwrap();
        created.push((
            title.to_string(),
            body.to_string(),
            labels.iter().map(|label| (*label).to_string()).collect(),
        ));
        Ok(BeadId::from(format!("pulse-{}", created.len())))
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
}

struct ChildGuard {
    child: Option<Child>,
    pid: u32,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        let pid = child.id();
        Self {
            child: Some(child),
            pid,
        }
    }

    fn id(&self) -> u32 {
        self.pid
    }

    fn output(mut self) -> Output {
        self.child
            .take()
            .expect("guard owns a child")
            .wait_with_output()
            .expect("wait for child output")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct ThreadGuard<T> {
    handle: Option<JoinHandle<T>>,
}

impl<T> ThreadGuard<T> {
    fn new(handle: JoinHandle<T>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    fn join(mut self) -> T {
        self.handle
            .take()
            .expect("guard owns a thread")
            .join()
            .expect("fixture thread did not panic")
    }
}

impl<T> Drop for ThreadGuard<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[test]
fn needle_binary_runs_with_the_inner_process_marker() {
    let home = tempfile::tempdir().expect("create isolated HOME");
    let child = Command::new(env!("CARGO_BIN_EXE_needle"))
        .arg("version")
        .env("HOME", home.path())
        .env("NEEDLE_INNER", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn NEEDLE binary");
    let output = ChildGuard::new(child).output();

    assert!(
        output.status.success(),
        "inner NEEDLE process failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("needle "));
}

#[serial_test::serial]
#[tokio::test]
async fn worker_empty_queue_runs_through_boot_selection_and_shutdown() {
    let _admission = EnvGuard::set("NEEDLE_SKIP_LAUNCH_RESOURCE_CHECK", "1");
    let home = tempfile::tempdir().expect("create isolated worker home");
    let workspace = tempfile::tempdir().expect("create isolated workspace");
    let mut config = Config::default();
    config.agent.default = "claude-sonnet".to_string();
    config.agent.routing = None;
    config.worker.idle_action = IdleAction::Exit;
    config.worker.allow_exit_without_supervisor = true;
    config.self_modification.hot_reload = false;
    config.workspace.home = home.path().join(".needle");
    config.workspace.default = workspace.path().to_path_buf();
    config.strands.explore.enabled = false;
    config.strands.explore.workspace_root = workspace.path().to_path_buf();
    config.strands.explore.workspaces.clear();

    let mut worker = Worker::new(
        config.clone(),
        "empty-queue".to_string(),
        Arc::new(LifecycleStore::empty()),
    );
    let terminal = worker.run().await.expect("run empty worker lifecycle");
    assert!(matches!(
        terminal,
        WorkerState::Stopped | WorkerState::Exhausted
    ));
    assert_eq!(worker.beads_processed(), 0);

    let mut stopped = Worker::new(
        config,
        "pre-stopped".to_string(),
        Arc::new(LifecycleStore::empty()),
    );
    stopped.request_shutdown();
    assert_eq!(stopped.run().await.unwrap(), WorkerState::Stopped);
}

#[serial_test::serial]
#[tokio::test]
async fn worker_processes_a_bead_through_the_real_dispatch_lifecycle() {
    let _admission = EnvGuard::set("NEEDLE_SKIP_LAUNCH_RESOURCE_CHECK", "1");
    let home = tempfile::tempdir().expect("create isolated worker home");
    let workspace = tempfile::tempdir().expect("create isolated workspace");
    std::fs::create_dir(workspace.path().join(".beads")).unwrap();
    let _home = EnvGuard::set("HOME", home.path());

    let mut config = isolated_worker_config(home.path(), workspace.path());
    config.agent.default = "echo-test".to_string();
    config.agent.timeout = 5;

    let bead = fixture_bead("needle-process-lifecycle", workspace.path());
    let bead_id = bead.id.clone();
    let store = Arc::new(LifecycleStore::new(vec![bead]));
    let mut worker = Worker::new(config, "full-cycle".to_string(), store.clone());
    let dispatch_started = workspace.path().join("dispatch-started");
    let release_dispatch = workspace.path().join("release-dispatch");
    let adapter = blocking_adapter("echo-test", &dispatch_started, &release_dispatch, "done");
    worker.set_dispatcher(Dispatcher::with_adapters(
        HashMap::from([(adapter.name.clone(), adapter)]),
        Telemetry::new("process-lifecycle-dispatch".to_string()),
        5,
    ));

    let closer = ThreadGuard::new(std::thread::spawn(move || {
        let started = wait_until(Duration::from_secs(5), || dispatch_started.exists());
        if started {
            store.close(&bead_id);
            std::fs::write(release_dispatch, b"").unwrap();
        }
        started
    }));

    let terminal = worker.run().await.expect("run one full worker cycle");
    assert!(closer.join(), "worker never started its adapter child");
    assert!(matches!(
        terminal,
        WorkerState::Stopped | WorkerState::Exhausted
    ));
    assert_eq!(worker.beads_processed(), 1);
}

#[serial_test::serial]
#[tokio::test]
async fn worker_applies_a_config_change_only_after_the_adapter_child_exits() {
    let _admission = EnvGuard::set("NEEDLE_SKIP_LAUNCH_RESOURCE_CHECK", "1");
    let home = tempfile::tempdir().expect("create isolated worker home");
    let workspace = tempfile::tempdir().expect("create isolated workspace");
    std::fs::create_dir(workspace.path().join(".beads")).unwrap();
    let _home = EnvGuard::set("HOME", home.path());
    let config_path = home.path().join(".config/needle/config.yaml");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();

    let mut config = isolated_worker_config(home.path(), workspace.path());
    config.agent.default = "old-agent".to_string();
    config.agent.timeout = 10;
    config.worker.config_reload_check_interval_secs = 1;
    std::fs::write(&config_path, serde_yaml::to_string(&config).unwrap()).unwrap();

    let bead = fixture_bead("needle-reload-boundary", workspace.path());
    let bead_id = bead.id.clone();
    let store = Arc::new(LifecycleStore::new(vec![bead]));
    let log_dir = home.path().join("telemetry");
    let telemetry = Telemetry::with_log_dir("reload-boundary".to_string(), &log_dir);
    let mut worker = Worker::new_with_telemetry(
        config.clone(),
        "reload-boundary".to_string(),
        store.clone(),
        telemetry.clone(),
    );

    let dispatch_started = workspace.path().join("dispatch-started");
    let release_dispatch = workspace.path().join("release-dispatch");
    let old_adapter = blocking_adapter(
        "old-agent",
        &dispatch_started,
        &release_dispatch,
        "old-config",
    );
    let new_adapter = immediate_adapter("new-agent", "new-config");
    worker.set_dispatcher(Dispatcher::with_adapters(
        HashMap::from([
            (old_adapter.name.clone(), old_adapter),
            (new_adapter.name.clone(), new_adapter),
        ]),
        telemetry,
        config.agent.timeout,
    ));

    let mut candidate = config;
    candidate.agent.default = "new-agent".to_string();
    let candidate_yaml = serde_yaml::to_string(&candidate).unwrap();
    let watcher = ThreadGuard::new(std::thread::spawn(move || {
        let started = wait_until(Duration::from_secs(5), || dispatch_started.exists());
        if started {
            std::fs::write(config_path, candidate_yaml).unwrap();
            store.close(&bead_id);
            std::fs::write(release_dispatch, b"").unwrap();
        }
        started
    }));

    let terminal = worker.run().await.expect("run reload worker lifecycle");
    assert!(watcher.join(), "old adapter child never started");
    assert!(matches!(
        terminal,
        WorkerState::Stopped | WorkerState::Exhausted
    ));
    assert_eq!(
        std::fs::read_to_string(
            workspace
                .path()
                .join(".beads/traces/needle-reload-boundary/stdout.txt")
        )
        .unwrap(),
        "old-config"
    );

    let events = read_telemetry_events(&log_dir);
    let completed = event(&events, "agent.completed");
    let detected = event(&events, "config.reload.detected");
    let applied = event(&events, "config.reload.applied");
    assert_eq!(completed.data["agent"], "old-agent");
    assert!(completed.sequence < detected.sequence);
    assert!(detected.sequence < applied.sequence);
    assert!(applied.data["changed_keys"]
        .as_array()
        .unwrap()
        .iter()
        .any(|key| key == "agent.default"));
}

#[serial_test::serial]
#[tokio::test]
async fn worker_rejects_an_invalid_reload_without_failing_its_lifecycle() {
    let _admission = EnvGuard::set("NEEDLE_SKIP_LAUNCH_RESOURCE_CHECK", "1");
    let home = tempfile::tempdir().expect("create isolated worker home");
    let workspace = tempfile::tempdir().expect("create isolated workspace");
    std::fs::create_dir(workspace.path().join(".beads")).unwrap();
    let _home = EnvGuard::set("HOME", home.path());
    let config_path = home.path().join(".config/needle/config.yaml");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();

    let mut config = isolated_worker_config(home.path(), workspace.path());
    config.agent.default = "old-agent".to_string();
    config.agent.timeout = 10;
    config.worker.config_reload_check_interval_secs = 1;
    std::fs::write(&config_path, serde_yaml::to_string(&config).unwrap()).unwrap();

    let bead = fixture_bead("needle-invalid-reload", workspace.path());
    let bead_id = bead.id.clone();
    let store = Arc::new(LifecycleStore::new(vec![bead]));
    let log_dir = home.path().join("telemetry");
    let telemetry = Telemetry::with_log_dir("invalid-reload".to_string(), &log_dir);
    let mut worker = Worker::new_with_telemetry(
        config.clone(),
        "invalid-reload".to_string(),
        store.clone(),
        telemetry.clone(),
    );
    let dispatch_started = workspace.path().join("dispatch-started");
    let release_dispatch = workspace.path().join("release-dispatch");
    let adapter = blocking_adapter(
        "old-agent",
        &dispatch_started,
        &release_dispatch,
        "old-config",
    );
    worker.set_dispatcher(Dispatcher::with_adapters(
        HashMap::from([(adapter.name.clone(), adapter)]),
        telemetry,
        config.agent.timeout,
    ));

    let watcher = ThreadGuard::new(std::thread::spawn(move || {
        let started = wait_until(Duration::from_secs(5), || dispatch_started.exists());
        if started {
            std::fs::write(config_path, "worker:\n  max_workers: 0\n").unwrap();
            store.close(&bead_id);
            std::fs::write(release_dispatch, b"").unwrap();
        }
        started
    }));

    let terminal = worker
        .run()
        .await
        .expect("invalid reload must not fail the worker");
    assert!(watcher.join(), "adapter child never started");
    assert!(matches!(
        terminal,
        WorkerState::Stopped | WorkerState::Exhausted
    ));

    let events = read_telemetry_events(&log_dir);
    let rejected = event(&events, "config.reload.rejected");
    assert!(rejected.data["validation_errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|error| error.as_str().unwrap().contains("max_workers")));
    assert!(!events
        .iter()
        .any(|event| event.event_type == "config.reload.applied"));
    assert!(!events
        .iter()
        .any(|event| event.event_type == "worker.errored"));
}

#[serial_test::serial]
#[tokio::test]
async fn worker_admission_hold_resumes_after_the_resource_probe_clears() {
    let home = tempfile::tempdir().expect("create isolated worker home");
    let workspace = tempfile::tempdir().expect("create isolated workspace");
    let probe = tempfile::tempdir().expect("create resource probe");
    std::fs::write(
        probe.path().join("loadavg"),
        "64.00 32.00 16.00 1/123 456\n",
    )
    .unwrap();
    std::fs::write(probe.path().join("meminfo"), "MemAvailable: 16777216 kB\n").unwrap();
    let _probe = EnvGuard::set("NEEDLE_LAUNCH_RESOURCE_PROBE", probe.path());
    let _base = EnvGuard::set("NEEDLE_ADMISSION_BACKOFF_BASE_MS", "50");
    let _cap = EnvGuard::set("NEEDLE_ADMISSION_BACKOFF_CAP_MS", "50");
    let _skip = EnvGuard::set("NEEDLE_SKIP_LAUNCH_RESOURCE_CHECK", "0");

    let mut config = Config::default();
    config.agent.default = "claude-sonnet".to_string();
    config.agent.routing = None;
    config.worker.cpu_load_warn = 1.0;
    config.worker.memory_free_warn_mb = 64;
    config.worker.idle_action = IdleAction::Exit;
    config.worker.allow_exit_without_supervisor = true;
    config.self_modification.hot_reload = false;
    config.workspace.home = home.path().join(".needle");
    config.workspace.default = workspace.path().to_path_buf();
    config.strands.explore.enabled = false;
    config.strands.explore.workspace_root = workspace.path().to_path_buf();
    config.strands.explore.workspaces.clear();

    let registry_path = config.workspace.home.join("state/workers.json");
    let probe_path = probe.path().to_path_buf();
    let flipper = ThreadGuard::new(std::thread::spawn(move || {
        let observed = wait_until(Duration::from_secs(5), || {
            std::fs::read_to_string(&registry_path)
                .is_ok_and(|contents| contents.contains("ADMISSION_BLOCKED"))
        });
        if observed {
            std::fs::write(probe_path.join("loadavg"), "0.10 0.10 0.10 1/123 456\n").unwrap();
        }
        observed
    }));

    let mut worker = Worker::new(
        config,
        "admission-resume".to_string(),
        Arc::new(LifecycleStore::empty()),
    );
    let terminal = worker.run().await.expect("run admission lifecycle");
    assert!(flipper.join(), "worker never published ADMISSION_BLOCKED");
    assert!(matches!(
        terminal,
        WorkerState::Stopped | WorkerState::Exhausted
    ));
    assert_eq!(worker.beads_processed(), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn proc_descendant_discovery_reads_the_linux_ppid_field() {
    let child = Command::new("sleep")
        .arg("30")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sleep child");
    let guard = ChildGuard::new(child);

    let discovered = wait_until(Duration::from_secs(2), || {
        find_all_descendants(std::process::id()).contains(&guard.id())
    });
    assert!(
        discovered,
        "a direct child must be discoverable through /proc/PID/status PPid"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn zombie_detection_and_supervisor_reaping_use_real_children() {
    let zombie = ChildGuard::new(Command::new("true").spawn().expect("spawn zombie probe"));
    wait_for_zombie(zombie.id());
    assert!(
        !is_pid_alive(zombie.id()),
        "zombie PID must not consume worker capacity"
    );
    drop(zombie);

    let reaped = ChildGuard::new(Command::new("true").spawn().expect("spawn reap probe"));
    let reaped_pid = reaped.id();
    let stat_path = format!("/proc/{reaped_pid}/stat");
    wait_for_zombie(reaped_pid);
    reap_exited_child(reaped_pid);
    assert!(
        !std::path::Path::new(&stat_path).exists(),
        "supervisor did not reap {reaped_pid}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn sanitizer_memory_budget_is_measured_in_an_isolated_process() {
    const PROBE: &str = "NEEDLE_SANITIZER_MEMORY_PROBE";
    if std::env::var_os(PROBE).is_some() {
        let before = peak_memory_kib();
        let sanitizer = Sanitizer::new(&[]).expect("build vendored sanitizer");
        let after = peak_memory_kib();
        assert!(sanitizer.rule_count() >= 200);
        assert!(
            after.saturating_sub(before) < 64 * 1024,
            "sanitizer startup grew peak RSS by {} KiB; budget is 64 MiB",
            after.saturating_sub(before)
        );
        return;
    }

    let child = Command::new(std::env::current_exe().expect("resolve integration test binary"))
        .args([
            "--exact",
            "process_lifecycle::sanitizer_memory_budget_is_measured_in_an_isolated_process",
            "--nocapture",
        ])
        .env(PROBE, "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sanitizer memory probe");
    let output = ChildGuard::new(child).output();
    assert!(
        output.status.success(),
        "memory probe failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(target_os = "linux")]
fn wait_for_zombie(pid: u32) {
    let stat_path = format!("/proc/{pid}/stat");
    let became_zombie = wait_until(Duration::from_secs(2), || {
        std::fs::read_to_string(&stat_path)
            .ok()
            .and_then(|stat| stat.rfind(')').map(|end| stat[end + 1..].to_string()))
            .is_some_and(|tail| tail.trim_start().starts_with('Z'))
    });
    assert!(became_zombie, "child {pid} did not become a zombie");
}

#[cfg(target_os = "linux")]
fn peak_memory_kib() -> usize {
    std::fs::read_to_string("/proc/self/status")
        .expect("read process status")
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .expect("VmHWM is present")
        .split_whitespace()
        .next()
        .expect("VmHWM has a value")
        .parse()
        .expect("VmHWM is numeric")
}

fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    predicate()
}

fn isolated_worker_config(home: &std::path::Path, workspace: &std::path::Path) -> Config {
    let mut config = Config::default();
    config.agent.routing = None;
    config.worker.idle_action = IdleAction::Exit;
    config.worker.allow_exit_without_supervisor = true;
    config.worker.enforce_shipped_work = false;
    config.self_modification.hot_reload = false;
    config.workspace.home = home.join(".needle");
    config.workspace.default = workspace.to_path_buf();
    config.strands.explore.enabled = false;
    config.strands.explore.workspace_root = workspace.to_path_buf();
    config.strands.explore.workspaces.clear();
    config
}

fn fixture_bead(id: &str, workspace: &std::path::Path) -> Bead {
    Bead {
        id: BeadId::from(id),
        title: format!("Test bead {id}"),
        body: Some("Exercise the worker lifecycle".to_string()),
        priority: 1,
        status: BeadStatus::Open,
        assignee: None,
        labels: Vec::new(),
        workspace: workspace.to_path_buf(),
        dependencies: Vec::new(),
        dependents: Vec::new(),
        comments: Vec::new(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn blocking_adapter(
    name: &str,
    dispatch_started: &std::path::Path,
    release_dispatch: &std::path::Path,
    output: &str,
) -> AgentAdapter {
    AgentAdapter {
        name: name.to_string(),
        description: None,
        agent_cli: "bash".to_string(),
        version_command: None,
        input_method: InputMethod::Stdin,
        invoke_template: format!(
            "touch \"$NEEDLE_TEST_DISPATCH_STARTED\"; \
             while [ ! -e \"$NEEDLE_TEST_RELEASE_DISPATCH\" ]; do sleep 0.01; done; \
             printf '%s' '{output}'"
        ),
        environment: HashMap::from([
            (
                "NEEDLE_TEST_DISPATCH_STARTED".to_string(),
                dispatch_started.display().to_string(),
            ),
            (
                "NEEDLE_TEST_RELEASE_DISPATCH".to_string(),
                release_dispatch.display().to_string(),
            ),
        ]),
        timeout_secs: 10,
        idle_timeout_secs: 0,
        hard_timeout_secs: 0,
        provider: None,
        model: None,
        token_extraction: TokenExtraction::None,
        output_transform: None,
        harness: None,
        harness_version: None,
    }
}

fn immediate_adapter(name: &str, output: &str) -> AgentAdapter {
    AgentAdapter {
        name: name.to_string(),
        description: None,
        agent_cli: "printf".to_string(),
        version_command: None,
        input_method: InputMethod::Stdin,
        invoke_template: format!("printf '%s' '{output}'"),
        environment: HashMap::new(),
        timeout_secs: 10,
        idle_timeout_secs: 0,
        hard_timeout_secs: 0,
        provider: None,
        model: None,
        token_extraction: TokenExtraction::None,
        output_transform: None,
        harness: None,
        harness_version: None,
    }
}

fn read_telemetry_events(log_dir: &std::path::Path) -> Vec<TelemetryEvent> {
    needle::telemetry::discover_log_files(log_dir)
        .expect("discover telemetry files")
        .into_iter()
        .flat_map(|path| {
            std::fs::read_to_string(path)
                .expect("read telemetry file")
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str(line).expect("parse telemetry event"))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn event<'a>(events: &'a [TelemetryEvent], event_type: &str) -> &'a TelemetryEvent {
    events
        .iter()
        .find(|event| event.event_type == event_type)
        .unwrap_or_else(|| panic!("missing {event_type}; saw {events:?}"))
}

#[tokio::test]
async fn worker_strand_process_contracts_low_water_overrides_pulse_cooldown() {
    let state_dir = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let warmup = PulseStrand::new(
        PulseConfig {
            enabled: true,
            scanners: vec![ScannerConfig {
                name: "warmup".to_string(),
                command: "printf 'all checks passed\\n'".to_string(),
                severity_threshold: None,
            }],
            cooldown_hours: 0,
            ..PulseConfig::default()
        },
        workspace_dir.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        Telemetry::new("warmup".to_string()),
    );
    assert!(matches!(
        warmup
            .evaluate(&LifecycleStore::empty(), &HashSet::new())
            .await,
        StrandResult::NoWork
    ));

    let strand = PulseStrand::new(
        PulseConfig {
            enabled: true,
            scanners: vec![ScannerConfig {
                name: "echo-scanner".to_string(),
                command: "echo 'src/foo.rs:10:1: error: unused import'".to_string(),
                severity_threshold: None,
            }],
            cooldown_hours: 48,
            severity_threshold: 5,
            max_beads_per_run: 10,
            ..PulseConfig::default()
        },
        workspace_dir.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        Telemetry::new("worker".to_string()),
    )
    .with_generation(
        GenerationConfig {
            enabled: true,
            low_water_reserve: 1,
            lease_ttl_secs: 300,
        },
        Vec::new(),
    );
    let store = LifecycleStore::empty();
    let result = strand.evaluate(&store, &HashSet::new()).await;

    assert!(matches!(result, StrandResult::WorkCreated));
    assert_eq!(store.created_beads().len(), 1);
}

#[tokio::test]
async fn worker_strand_process_contracts_contended_pulse_lease_skips_generation() {
    let state_dir = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let scanner = || ScannerConfig {
        name: "echo-scanner".to_string(),
        command: "echo 'src/foo.rs:10:1: error: unused import'".to_string(),
        severity_threshold: None,
    };
    let generation = GenerationConfig {
        enabled: true,
        low_water_reserve: 1,
        lease_ttl_secs: 300,
    };
    let pulse = |worker: &str, generation: GenerationConfig| {
        PulseStrand::new(
            PulseConfig {
                enabled: true,
                scanners: vec![scanner()],
                cooldown_hours: 0,
                severity_threshold: 5,
                max_beads_per_run: 10,
                ..PulseConfig::default()
            },
            workspace_dir.path().to_path_buf(),
            state_dir.path().to_path_buf(),
            Telemetry::new(worker.to_string()),
        )
        .with_generation(generation, Vec::new())
    };

    let first = pulse("worker-a", generation.clone());
    assert!(matches!(
        first
            .evaluate(&LifecycleStore::empty(), &HashSet::new())
            .await,
        StrandResult::WorkCreated
    ));

    let second = pulse("worker-b", generation);
    let store = LifecycleStore::empty();
    let result = second.evaluate(&store, &HashSet::new()).await;
    assert!(store.created_beads().is_empty());
    assert!(matches!(
        result,
        StrandResult::Skipped { ref reason } if reason == "generation_lease_contended"
    ));
}

#[tokio::test]
async fn worker_strand_process_contracts_scanner_failures_emit_creator_failure() {
    let state_dir = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let log_dir = tempfile::tempdir().unwrap();
    let telemetry = Telemetry::with_log_dir("worker".to_string(), log_dir.path());
    telemetry.start();
    let telemetry_observer = telemetry.clone();
    let strand = PulseStrand::new(
        PulseConfig {
            enabled: true,
            scanners: vec![
                ScannerConfig {
                    name: "failing-scanner".to_string(),
                    command: "exit 1".to_string(),
                    severity_threshold: None,
                },
                ScannerConfig {
                    name: "also-failing".to_string(),
                    command: "exit 2".to_string(),
                    severity_threshold: None,
                },
            ],
            cooldown_hours: 0,
            ..PulseConfig::default()
        },
        workspace_dir.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        telemetry,
    );
    let store = LifecycleStore::empty();
    assert!(matches!(
        strand.evaluate(&store, &HashSet::new()).await,
        StrandResult::NoWork
    ));
    assert!(store.created_beads().is_empty());
    drop(strand);
    telemetry_observer
        .force_flush_async(Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(
        read_telemetry_events(log_dir.path())
            .iter()
            .filter(|event| event.event_type == "generation.creator_failed")
            .count(),
        1
    );
    telemetry_observer.shutdown().await;
}

#[tokio::test]
async fn worker_strand_process_contracts_scanner_creates_bead() {
    let state_dir = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let strand = PulseStrand::new(
        PulseConfig {
            enabled: true,
            scanners: vec![ScannerConfig {
                name: "echo-scanner".to_string(),
                command: "echo 'src/foo.rs:10:1: error: unused import'".to_string(),
                severity_threshold: None,
            }],
            cooldown_hours: 0,
            severity_threshold: 5,
            max_beads_per_run: 10,
            ..PulseConfig::default()
        },
        workspace_dir.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        Telemetry::new("worker".to_string()),
    );
    let store = LifecycleStore::empty();
    assert!(matches!(
        strand.evaluate(&store, &HashSet::new()).await,
        StrandResult::WorkCreated
    ));
    let beads = store.created_beads();
    assert_eq!(beads.len(), 1);
    assert!(beads[0].0.contains("[Pulse]"));
    assert!(beads[0].0.contains("error"));
    assert!(beads[0].2.contains(&"pulse-finding".to_string()));
}

#[tokio::test]
async fn worker_strand_process_contracts_pulse_deduplicates_across_scans() {
    let state_dir = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let config = PulseConfig {
        enabled: true,
        scanners: vec![ScannerConfig {
            name: "echo-scanner".to_string(),
            command: "echo 'error: same issue every time'".to_string(),
            severity_threshold: None,
        }],
        cooldown_hours: 0,
        severity_threshold: 5,
        max_beads_per_run: 10,
        ..PulseConfig::default()
    };

    let first = PulseStrand::new(
        config.clone(),
        workspace_dir.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        Telemetry::new("worker-a".to_string()),
    );
    let first_store = LifecycleStore::empty();
    assert!(matches!(
        first.evaluate(&first_store, &HashSet::new()).await,
        StrandResult::WorkCreated
    ));

    let second = PulseStrand::new(
        config,
        workspace_dir.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        Telemetry::new("worker-b".to_string()),
    );
    let second_store = LifecycleStore::empty();
    assert!(matches!(
        second.evaluate(&second_store, &HashSet::new()).await,
        StrandResult::NoWork
    ));
    assert!(second_store.created_beads().is_empty());
}

#[tokio::test]
async fn worker_strand_process_contracts_pulse_caps_beads_per_run() {
    let state_dir = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let strand = PulseStrand::new(
        PulseConfig {
            enabled: true,
            scanners: vec![ScannerConfig {
                name: "multi-error".to_string(),
                command: "printf 'error: issue one\\nerror: issue two\\nerror: issue three\\n'"
                    .to_string(),
                severity_threshold: None,
            }],
            cooldown_hours: 0,
            severity_threshold: 5,
            max_beads_per_run: 2,
            ..PulseConfig::default()
        },
        workspace_dir.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        Telemetry::new("worker".to_string()),
    );
    let store = LifecycleStore::empty();
    assert!(matches!(
        strand.evaluate(&store, &HashSet::new()).await,
        StrandResult::WorkCreated
    ));
    assert_eq!(store.created_beads().len(), 2);
}

#[tokio::test]
async fn worker_strand_process_contracts_clean_scanner_returns_no_work() {
    let state_dir = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let strand = PulseStrand::new(
        PulseConfig {
            enabled: true,
            scanners: vec![ScannerConfig {
                name: "clean-scanner".to_string(),
                command: "echo 'all checks passed'".to_string(),
                severity_threshold: None,
            }],
            cooldown_hours: 0,
            severity_threshold: 5,
            max_beads_per_run: 10,
            ..PulseConfig::default()
        },
        workspace_dir.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        Telemetry::new("worker".to_string()),
    );
    let store = LifecycleStore::empty();
    assert!(matches!(
        strand.evaluate(&store, &HashSet::new()).await,
        StrandResult::NoWork
    ));
    assert!(store.created_beads().is_empty());
}

#[tokio::test]
async fn worker_strand_process_contracts_pulse_persists_state() {
    let state_dir = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let strand = PulseStrand::new(
        PulseConfig {
            enabled: true,
            scanners: vec![ScannerConfig {
                name: "persist-test".to_string(),
                command: "echo 'error: test finding'".to_string(),
                severity_threshold: None,
            }],
            cooldown_hours: 0,
            severity_threshold: 5,
            max_beads_per_run: 10,
            ..PulseConfig::default()
        },
        workspace_dir.path().to_path_buf(),
        state_dir.path().to_path_buf(),
        Telemetry::new("worker".to_string()),
    );
    strand
        .evaluate(&LifecycleStore::empty(), &HashSet::new())
        .await;

    let state_paths: Vec<_> = std::fs::read_dir(state_dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    assert_eq!(state_paths.len(), 1);
    let state: PulseState =
        serde_json::from_str(&std::fs::read_to_string(&state_paths[0]).unwrap()).unwrap();
    assert!(state.last_run.is_some());
    assert!(!state.seen_fingerprints.is_empty());
}
