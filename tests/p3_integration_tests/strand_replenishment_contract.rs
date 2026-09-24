//! Deterministic end-to-end coverage for the self-replenishment incident contract.
//!
//! This file is included by both the P3 strand target and the real bead-rs
//! target. Every fixture owns its temporary native bead-rs store and its
//! telemetry directory, so the same assertions exercise both target paths.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use needle::bead_store::{builtin_bead_backends, BeadStore, CliBeadStore, Filters};
use needle::claim::Claimer;
use needle::config::{ExploreConfig, GenerationConfig, KnotConfig, UnravelConfig, WeaveConfig};
use needle::strand::{
    ExploreStrand, FleetWeaveStrand, KnotStrand, PluckStrand, Strand, StrandRunner, UnravelStrand,
    WeaveStrand,
};
use needle::telemetry::Telemetry;
use needle::types::{Bead, BeadId, BeadStatus, ClaimOutcome, ClaimResult, StrandResult};
use tempfile::TempDir;

fn native_bead_path() -> PathBuf {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("BEAD_RS_BIN") {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(paths) = which::which_all("bead") {
        candidates.extend(paths);
    }
    candidates.push(PathBuf::from("/home/coding/.cargo/bin/bead"));

    candidates
        .into_iter()
        .find(|path| path.is_file())
        .expect("native bead-rs executable is required for strand contract fixtures")
}

fn bead_command(workspace: &Path) -> std::process::Command {
    let home = workspace.join(".fixture-home");
    fs::create_dir_all(&home).expect("fixture HOME should be creatable");
    let mut command = std::process::Command::new(native_bead_path());
    command
        .env_clear()
        .current_dir(workspace)
        .env("PATH", "/run/current-system/sw/bin:/home/coding/.cargo/bin")
        .env("HOME", home)
        .env("NEEDLE_WS", workspace);
    command
}

fn fixture_bead_binary(workspace: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let bin_dir = workspace.join(".fixture-bin");
    fs::create_dir_all(&bin_dir)?;
    let shim = bin_dir.join("bead");
    if !shim.exists() {
        fs::write(
            &shim,
            format!(
                "#!/bin/sh\nexec env -i PATH=\"/run/current-system/sw/bin:/home/coding/.cargo/bin\" HOME=\"$PWD/.fixture-home\" NEEDLE_WS=\"$PWD\" \"{}\" \"$@\"\n",
                native_bead_path().display()
            ),
        )?;
        let mut permissions = fs::metadata(&shim)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&shim, permissions)?;
    }
    Ok(shim)
}

fn create_workspace(prefix: &str) -> Result<TempDir> {
    let fixture_root = Path::new("/home/coding/scratch");
    fs::create_dir_all(fixture_root).context("failed to create fixture root")?;
    let workspace = tempfile::Builder::new()
        .prefix(&format!("needle-strand-{prefix}-"))
        .tempdir_in(fixture_root)
        .context("failed to create strand fixture")?;
    let output = bead_command(workspace.path())
        .args(["init", "--prefix", "fixture", "--skip-foreign-workspace"])
        .output()
        .context("failed to initialize native bead-rs fixture")?;
    if !output.status.success() {
        anyhow::bail!(
            "native bead-rs init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Explore's health contract requires a repository-shaped boundary. The
    // fixture does not need a real Git history; workspace validation only
    // needs the .git directory to exist.
    fs::create_dir(workspace.path().join(".git"))?;
    let fixture_binary = fixture_bead_binary(workspace.path())?;
    fs::write(
        workspace.path().join(".needle.yaml"),
        format!(
            "bead_cli:\n  backend: bead-rs\n  path: {}\n",
            serde_json::to_string(&fixture_binary)?
        ),
    )?;
    Ok(workspace)
}

fn store_for(workspace: &Path) -> Result<CliBeadStore> {
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .context("built-in bead-rs descriptor is missing")?;
    CliBeadStore::new(
        backend,
        fixture_bead_binary(workspace)?,
        workspace.to_path_buf(),
        None,
        None,
        None,
    )
}

fn create_bead(workspace: &Path, title: &str, body: &str) -> Result<BeadId> {
    let output = bead_command(workspace)
        .args(["create", "--title", title, "--description", body])
        .output()
        .context("failed to create fixture bead")?;
    if !output.status.success() {
        anyhow::bail!(
            "native bead-rs create failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(BeadId::from(String::from_utf8(output.stdout)?.trim()))
}

fn add_label(workspace: &Path, bead_id: &BeadId, label: &str) -> Result<()> {
    let output = bead_command(workspace)
        .args(["label", "add", bead_id.as_ref(), "--label", label])
        .output()
        .context("failed to label fixture bead")?;
    if !output.status.success() {
        anyhow::bail!(
            "native bead-rs label failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn write_readme(workspace: &Path) -> Result<()> {
    fs::write(
        workspace.join("README.md"),
        "# Fixture\n\nThe documentation intentionally leaves one implementation gap.\n",
    )?;
    Ok(())
}

fn weave_config() -> WeaveConfig {
    WeaveConfig {
        enabled: true,
        max_beads_per_run: 1,
        cooldown_hours: 24,
        ..WeaveConfig::default()
    }
}

fn generation_config() -> GenerationConfig {
    GenerationConfig {
        enabled: true,
        low_water_reserve: 1,
        lease_ttl_secs: 300,
    }
}

fn explore_config(workspace: &Path) -> ExploreConfig {
    ExploreConfig {
        enabled: true,
        workspaces: vec![workspace.to_path_buf()],
        workspace_root: workspace.parent().unwrap_or(workspace).to_path_buf(),
        scan_interval_cycles: 1,
        max_scan_interval_cycles: 1,
        ..ExploreConfig::default()
    }
}

fn telemetry(worker: &str, root: &Path) -> Telemetry {
    Telemetry::with_log_dir(worker.to_string(), &root.join("logs"))
}

fn read_events(log_dir: &Path) -> Vec<serde_json::Value> {
    let mut events = Vec::new();
    let Ok(entries) = fs::read_dir(log_dir) else {
        return events;
    };
    for entry in entries.flatten() {
        if entry.path().extension().and_then(|value| value.to_str()) != Some("jsonl") {
            continue;
        }
        let content = fs::read_to_string(entry.path()).expect("telemetry log should be readable");
        events.extend(
            content
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str(line).expect("telemetry line should be JSON")),
        );
    }
    events
}

struct MockWeaveAgent {
    response: String,
}

#[async_trait]
impl needle::strand::weave::WeaveAgent for MockWeaveAgent {
    async fn analyze_gaps(&self, _prompt: &str, _workspace: &Path) -> Result<String> {
        Ok(self.response.clone())
    }
}

struct FailingWeaveAgent;

#[async_trait]
impl needle::strand::weave::WeaveAgent for FailingWeaveAgent {
    async fn analyze_gaps(&self, _prompt: &str, _workspace: &Path) -> Result<String> {
        anyhow::bail!("deterministic generator failure")
    }
}

struct MockUnravelAgent;

#[async_trait]
impl needle::strand::unravel::UnravelAgent for MockUnravelAgent {
    async fn propose_alternatives(&self, _prompt: &str, _workspace: &Path) -> Result<String> {
        Ok(r#"[{"title":"should never be created","body":"unexpected"}]"#.to_string())
    }
}

fn generated_response(title: &str) -> String {
    serde_json::json!([{
        "title": title,
        "body": "Generated work for the self-replenishment contract.",
        "priority": 1
    }])
    .to_string()
}

fn test_bead(id: &str, workspace: &Path) -> Bead {
    let now = chrono::Utc::now();
    Bead {
        id: BeadId::from(id),
        title: format!("fallback {id}"),
        body: Some("fallback candidate".to_string()),
        priority: 1,
        status: BeadStatus::Open,
        assignee: None,
        labels: Vec::new(),
        workspace: workspace.to_path_buf(),
        dependencies: Vec::new(),
        dependents: Vec::new(),
        comments: Vec::new(),
        created_at: now,
        updated_at: now,
    }
}

/// A generator-shaped no-op used to prove the runner's terminal accounting.
struct NoWorkGenerator(&'static str);

#[async_trait]
impl Strand for NoWorkGenerator {
    fn name(&self) -> &str {
        self.0
    }

    fn is_generator(&self) -> bool {
        true
    }

    async fn evaluate(
        &self,
        _store: &dyn BeadStore,
        _exclusions: &HashSet<BeadId>,
    ) -> StrandResult {
        StrandResult::NoWork
    }
}

struct StaticFinder(Bead);

#[async_trait]
impl Strand for StaticFinder {
    fn name(&self) -> &str {
        "fallback-finder"
    }

    async fn evaluate(
        &self,
        _store: &dyn BeadStore,
        _exclusions: &HashSet<BeadId>,
    ) -> StrandResult {
        StrandResult::BeadFound(vec![self.0.clone()])
    }
}

#[tokio::test]
async fn local_empty_remote_ready_and_invalid_duplicate_workspaces_are_safe() {
    let home = create_workspace("explore-home").unwrap();
    let remote = create_workspace("explore-remote").unwrap();
    let remote_id = create_bead(remote.path(), "remote-ready", "remote work").unwrap();
    let remote_store = Arc::new(store_for(remote.path()).unwrap());
    let before = remote_store.ready(&Filters::default()).await.unwrap().len();
    assert_eq!(before, 1);

    let invalid = home.path().join("missing-workspace");
    let config = ExploreConfig {
        enabled: true,
        workspaces: vec![
            invalid,
            remote.path().to_path_buf(),
            remote.path().to_path_buf(),
        ],
        workspace_root: home.path().to_path_buf(),
        scan_interval_cycles: 1,
        max_scan_interval_cycles: 1,
        ..ExploreConfig::default()
    };
    let state = tempfile::tempdir().unwrap();
    let strand = ExploreStrand::new(
        config,
        home.path().to_path_buf(),
        needle::registry::Registry::new(state.path()),
        telemetry("explore-contract", state.path()),
        "explore-contract-worker".to_string(),
    );
    let home_store = store_for(home.path()).unwrap();
    let result = strand.evaluate(&home_store, &HashSet::new()).await;
    let candidates = match result {
        StrandResult::BeadFound(candidates) => candidates,
        other => panic!("Explore should survive invalid and duplicate paths: {other:?}"),
    };
    let unique_ids: HashSet<_> = candidates.iter().map(|bead| bead.id.clone()).collect();
    assert_eq!(unique_ids, HashSet::from([remote_id]));
    assert_eq!(
        remote_store.ready(&Filters::default()).await.unwrap().len(),
        before
    );
}

#[tokio::test]
async fn fleet_empty_weave_restarts_pluck_and_claims_real_generated_work() {
    let home = create_workspace("fleet-home").unwrap();
    let remote = create_workspace("fleet-target").unwrap();
    write_readme(remote.path()).unwrap();
    let state = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let telemetry = telemetry("fleet-replenishment", logs.path());
    telemetry.start();

    let home_store = Arc::new(store_for(home.path()).unwrap());
    let explore = ExploreStrand::new(
        explore_config(remote.path()),
        home.path().to_path_buf(),
        needle::registry::Registry::new(state.path()),
        telemetry.clone(),
        "fleet-replenishment-worker".to_string(),
    );
    let weave = FleetWeaveStrand::new(
        weave_config(),
        explore_config(remote.path()),
        state.path().to_path_buf(),
        Box::new(MockWeaveAgent {
            response: generated_response("generated-real-work"),
        }),
        telemetry.clone(),
        generation_config(),
        Vec::new(),
        "fleet-replenishment-worker".to_string(),
    );
    let runner = StrandRunner::with_telemetry(
        vec![
            Box::new(PluckStrand::new(Vec::new(), telemetry.clone())),
            Box::new(explore),
            Box::new(weave),
        ],
        telemetry.clone(),
    );

    let outcome = runner
        .select(home_store.as_ref(), &HashSet::new())
        .await
        .unwrap();
    let (generated, strand) = outcome.bead.expect("restart should select generated work");
    assert_eq!(strand, "explore");
    assert_eq!(generated.title, "generated-real-work");
    assert_eq!(generated.workspace, remote.path());
    assert_eq!(outcome.waterfall_restarts, 1);
    assert_eq!(outcome.restart_triggers, vec!["weave"]);

    let remote_store = Arc::new(store_for(remote.path()).unwrap());
    let claimed = Claimer::new(
        remote_store,
        state.path().to_path_buf(),
        1,
        0,
        telemetry.clone(),
    )
    .claim_one(
        &generated.id,
        "fleet-replenishment-worker",
        &HashSet::new(),
        Some("explore"),
    )
    .await
    .unwrap();
    assert!(matches!(claimed, ClaimResult::Claimed(bead) if bead.id == generated.id));
    telemetry
        .force_flush_async(std::time::Duration::from_secs(2))
        .await
        .unwrap();
    telemetry.shutdown().await;
}

#[tokio::test]
async fn multiple_empty_workers_share_one_generation_lease() {
    let home = create_workspace("lease-home").unwrap();
    let target = create_workspace("lease-target").unwrap();
    write_readme(target.path()).unwrap();
    let state = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let first_telemetry = telemetry("lease-first", logs.path());
    let second_telemetry = telemetry("lease-second", logs.path());
    first_telemetry.start();
    second_telemetry.start();
    let home_store = Arc::new(store_for(home.path()).unwrap());
    let config = explore_config(target.path());
    let first = FleetWeaveStrand::new(
        weave_config(),
        config.clone(),
        state.path().to_path_buf(),
        Box::new(MockWeaveAgent {
            response: generated_response("one-leased-work-item"),
        }),
        first_telemetry.clone(),
        generation_config(),
        Vec::new(),
        "lease-first".to_string(),
    );
    let second = FleetWeaveStrand::new(
        weave_config(),
        config,
        state.path().to_path_buf(),
        Box::new(MockWeaveAgent {
            response: generated_response("one-leased-work-item"),
        }),
        second_telemetry.clone(),
        generation_config(),
        Vec::new(),
        "lease-second".to_string(),
    );

    let exclusions = HashSet::new();
    let (first_result, second_result) = tokio::join!(
        first.evaluate(home_store.as_ref(), &exclusions),
        second.evaluate(home_store.as_ref(), &exclusions),
    );
    assert!(
        matches!(first_result, StrandResult::WorkCreated)
            || matches!(second_result, StrandResult::WorkCreated),
        "one worker must own the generation lease: {first_result:?}, {second_result:?}"
    );
    assert!(
        matches!(
            first_result,
            StrandResult::NoWork | StrandResult::Skipped { .. }
        ) || matches!(
            second_result,
            StrandResult::NoWork | StrandResult::Skipped { .. }
        ),
        "the losing worker must not create a second item: {first_result:?}, {second_result:?}"
    );
    let target_store = store_for(target.path()).unwrap();
    let generated = target_store.list_all().await.unwrap();
    assert_eq!(generated.len(), 1);
    assert_eq!(generated[0].title, "one-leased-work-item");
    first_telemetry.shutdown().await;
    second_telemetry.shutdown().await;
}

#[tokio::test]
async fn low_water_overrides_cooldown_but_healthy_reserve_does_not() {
    let workspace = create_workspace("low-water").unwrap();
    write_readme(workspace.path()).unwrap();
    let state = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let telemetry = telemetry("low-water-contract", logs.path());
    telemetry.start();
    let store = Arc::new(store_for(workspace.path()).unwrap());

    // Seed a recent run while generation is disabled. The next runs therefore
    // exercise the same cooldown window rather than a first-run special case.
    let seeded = WeaveStrand::new(
        weave_config(),
        workspace.path().to_path_buf(),
        state.path().to_path_buf(),
        Box::new(MockWeaveAgent {
            response: "NO_GAPS".to_string(),
        }),
        telemetry.clone(),
    );
    assert!(matches!(
        seeded.evaluate(store.as_ref(), &HashSet::new()).await,
        StrandResult::NoWork
    ));

    create_bead(workspace.path(), "reserve-one", "reserve").unwrap();
    create_bead(workspace.path(), "reserve-two", "reserve").unwrap();
    let healthy = WeaveStrand::new(
        weave_config(),
        workspace.path().to_path_buf(),
        state.path().to_path_buf(),
        Box::new(MockWeaveAgent {
            response: generated_response("created-after-low-water"),
        }),
        telemetry.clone(),
    )
    .with_generation(generation_config(), Vec::new());
    assert!(matches!(
        healthy.evaluate(store.as_ref(), &HashSet::new()).await,
        StrandResult::NoWork
    ));
    assert_eq!(store.list_all().await.unwrap().len(), 2);

    for bead in store.list_all().await.unwrap() {
        assert!(matches!(
            store.claim(&bead.id, "reserve-worker").await.unwrap(),
            ClaimResult::Claimed(_)
        ));
    }
    let low_water = WeaveStrand::new(
        weave_config(),
        workspace.path().to_path_buf(),
        state.path().to_path_buf(),
        Box::new(MockWeaveAgent {
            response: generated_response("created-after-low-water"),
        }),
        telemetry.clone(),
    )
    .with_generation(generation_config(), Vec::new());
    assert!(matches!(
        low_water.evaluate(store.as_ref(), &HashSet::new()).await,
        StrandResult::WorkCreated
    ));
    assert_eq!(store.list_all().await.unwrap().len(), 3);
    telemetry
        .force_flush_async(std::time::Duration::from_secs(2))
        .await
        .unwrap();
    telemetry.shutdown().await;

    let events = read_events(&logs.path().join("logs"));
    let gate_reasons: Vec<_> = events
        .iter()
        .filter(|event| event["event_type"] == "generation.gate_evaluated")
        .filter_map(|event| event["data"]["reason"].as_str())
        .collect();
    assert!(gate_reasons.contains(&"backlog_healthy"));
    assert!(gate_reasons.contains(&"low_water"));
}

#[tokio::test]
async fn no_work_generators_emit_one_terminal_idle_and_create_no_bead() {
    let workspace = create_workspace("terminal-idle").unwrap();
    let state = tempfile::tempdir().unwrap();
    let telemetry = telemetry("terminal-idle", state.path());
    telemetry.start();
    let store = store_for(workspace.path()).unwrap();
    let runner = StrandRunner::with_telemetry(
        vec![
            Box::new(NoWorkGenerator("generator-a")),
            Box::new(NoWorkGenerator("generator-b")),
            Box::new(KnotStrand::new(
                KnotConfig {
                    exhaustion_threshold: 1,
                    starvation_backoff_minutes: 0,
                    ..KnotConfig::default()
                },
                telemetry.clone(),
            )),
        ],
        telemetry.clone(),
    );
    let outcome = runner.select(&store, &HashSet::new()).await.unwrap();
    assert!(outcome.bead.is_none());
    assert_eq!(outcome.strand_evaluations.len(), 3);
    assert!(outcome
        .strand_evaluations
        .iter()
        .take(2)
        .all(|evaluation| evaluation.result == "no_work"));
    assert!(store.list_all().await.unwrap().is_empty());
    telemetry
        .force_flush_async(std::time::Duration::from_secs(2))
        .await
        .unwrap();
    telemetry.shutdown().await;

    let terminal_events: Vec<_> = read_events(&state.path().join("logs"))
        .into_iter()
        .filter(|event| event["event_type"] == "cycle.outcome")
        .filter(|event| event["data"]["outcome"] == "terminal_idle")
        .collect();
    assert_eq!(terminal_events.len(), 1);
}

#[tokio::test]
async fn recoverable_generator_error_falls_through_and_is_observable() {
    let home = create_workspace("creator-error-home").unwrap();
    let target = create_workspace("creator-error-target").unwrap();
    write_readme(target.path()).unwrap();
    let state = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let telemetry = telemetry("creator-error", logs.path());
    telemetry.start();
    let store = store_for(home.path()).unwrap();
    let fallback = test_bead("fallback-after-error", target.path());
    let weave = FleetWeaveStrand::new(
        weave_config(),
        explore_config(target.path()),
        state.path().to_path_buf(),
        Box::new(FailingWeaveAgent),
        telemetry.clone(),
        generation_config(),
        Vec::new(),
        "creator-error-worker".to_string(),
    );
    let outcome = StrandRunner::with_telemetry(
        vec![Box::new(weave), Box::new(StaticFinder(fallback.clone()))],
        telemetry.clone(),
    )
    .select(&store, &HashSet::new())
    .await
    .unwrap();
    assert_eq!(outcome.bead.map(|(bead, _)| bead.id), Some(fallback.id));
    assert!(outcome
        .strand_evaluations
        .iter()
        .any(|evaluation| evaluation.strand_name == "weave" && evaluation.result == "error"));
    assert!(store.list_all().await.unwrap().is_empty());
    telemetry
        .force_flush_async(std::time::Duration::from_secs(2))
        .await
        .unwrap();
    telemetry.shutdown().await;
    assert!(read_events(&logs.path().join("logs")).iter().any(|event| {
        event["event_type"] == "generation.creator_failed"
            && event["data"]["error"]
                .as_str()
                .is_some_and(|error| error.contains("deterministic generator failure"))
    }));
}

#[tokio::test]
async fn pluck_reports_starvation_without_persisting_a_target_bead() {
    let workspace = create_workspace("pluck-no-alert-bead").unwrap();
    let alert = create_bead(
        workspace.path(),
        "Starvation alert: beads invisible to worker",
        "Workspace has open beads but Pluck found none.",
    )
    .unwrap();
    add_label(workspace.path(), &alert, "alert").unwrap();
    let before = store_for(workspace.path())
        .unwrap()
        .list_all()
        .await
        .unwrap();
    let state = tempfile::tempdir().unwrap();
    let telemetry = telemetry("pluck-no-alert-bead", state.path());
    let strand = PluckStrand::with_persistent_records(
        Vec::new(),
        0,
        telemetry,
        state.path().to_path_buf(),
        true,
    );
    let store = store_for(workspace.path()).unwrap();
    assert!(matches!(
        strand.evaluate(&store, &HashSet::new()).await,
        StrandResult::NoWork
    ));
    let after = store.list_all().await.unwrap();
    assert_eq!(after.len(), before.len());
    assert_eq!(after[0].id, alert);
    assert!(after.iter().all(|bead| !bead.title.contains("Knot")));
}

#[tokio::test]
async fn unravel_ignores_historical_internal_alert_shapes() {
    let workspace = create_workspace("unravel-history").unwrap();
    let alert = create_bead(
        workspace.path(),
        "Starvation alert: beads invisible to worker",
        "Workspace has open beads but Pluck found none; ready beads are invisible.",
    )
    .unwrap();
    add_label(workspace.path(), &alert, "human").unwrap();
    let state = tempfile::tempdir().unwrap();
    let telemetry = telemetry("unravel-history", state.path());
    let store = store_for(workspace.path()).unwrap();
    let strand = UnravelStrand::new(
        UnravelConfig {
            enabled: true,
            max_beads_per_run: 1,
            max_alternatives_per_bead: 1,
            cooldown_hours: 0,
            ..UnravelConfig::default()
        },
        workspace.path().to_path_buf(),
        state.path().to_path_buf(),
        Box::new(MockUnravelAgent),
        telemetry,
    );
    assert!(matches!(
        strand.evaluate(&store, &HashSet::new()).await,
        StrandResult::NoWork
    ));
    assert_eq!(store.list_all().await.unwrap().len(), 1);
}

#[tokio::test]
async fn atomic_claim_honors_exclusion_and_retries_after_a_race() {
    let workspace = create_workspace("claim-contract").unwrap();
    let first = create_bead(workspace.path(), "already-claimed", "claim").unwrap();
    let second = create_bead(workspace.path(), "retry-target", "claim").unwrap();
    let excluded = create_bead(workspace.path(), "excluded-target", "claim").unwrap();
    let store = Arc::new(store_for(workspace.path()).unwrap());
    let first_snapshot = store.show(&first).await.unwrap();
    let second_snapshot = store.show(&second).await.unwrap();
    let excluded_snapshot = store.show(&excluded).await.unwrap();
    assert!(matches!(
        store.claim(&first, "other-worker").await.unwrap(),
        ClaimResult::Claimed(_)
    ));

    let state = tempfile::tempdir().unwrap();
    let outcome = Claimer::new(
        store.clone(),
        state.path().to_path_buf(),
        2,
        0,
        telemetry("claim-contract", state.path()),
    )
    .claim_next(
        &[first_snapshot, second_snapshot, excluded_snapshot],
        "retry-worker",
        &HashSet::from([excluded.clone()]),
        "pluck",
    )
    .await
    .unwrap();
    assert!(matches!(outcome, ClaimOutcome::Claimed(bead) if bead.id == second));
    assert!(matches!(
        store.show(&first).await.unwrap().status,
        BeadStatus::InProgress
    ));
    assert!(matches!(
        store.show(&second).await.unwrap().status,
        BeadStatus::InProgress
    ));
    assert_eq!(
        store.show(&excluded).await.unwrap().status,
        BeadStatus::Open
    );
}
