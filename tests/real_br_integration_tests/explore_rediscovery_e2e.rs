//! End-to-end coverage for Explore's rediscovery and starvation-safe scan loop.
//!
//! These tests use real bead-rs stores in isolated temporary Git workspaces.
//! They deliberately keep the home workspace outside the discovery root so a
//! test can prove that remote work is found without restarting the strand.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use chrono::Utc;
use needle::bead_store::{builtin_bead_backends, CliBeadStore};
use needle::config::{ExploreConfig, PluckLaneConfig};
use needle::registry::Registry;
use needle::strand::{ExploreStrand, Strand};
use needle::telemetry::{Telemetry, TelemetryEvent};
use needle::types::{BeadId, StrandResult};
use tempfile::{Builder, TempDir};

#[path = "../p3_integration_tests/strand_replenishment_contract.rs"]
mod strand_replenishment_contract;

/// Locate a native bead-rs binary without depending on the operator's HOME.
///
/// Some hosts put a queue-fence wrapper ahead of the native binary. Probing
/// each candidate with isolated state keeps these tests about Explore rather
/// than about that host policy.
fn isolated_tempdir() -> Result<TempDir> {
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".needle"));
    }
    candidates.push(std::env::temp_dir());

    for base in candidates {
        if fs::create_dir_all(&base).is_err() {
            continue;
        }

        let mut ancestor = Some(base.as_path());
        let has_bead_store_ancestor = std::iter::from_fn(|| {
            let current = ancestor?;
            ancestor = current.parent();
            Some(current.join(".beads").exists())
        })
        .any(|exists| exists);
        if has_bead_store_ancestor {
            continue;
        }

        return Builder::new()
            .prefix("needle-explore-e2e-")
            .tempdir_in(&base)
            .with_context(|| {
                format!(
                    "failed to create isolated Explore fixture under {}",
                    base.display()
                )
            });
    }

    anyhow::bail!("could not find a temporary parent without a .beads ancestor")
}

fn native_bead_path() -> PathBuf {
    static NATIVE_BEAD: OnceLock<PathBuf> = OnceLock::new();

    NATIVE_BEAD
        .get_or_init(|| {
            let mut candidates = Vec::new();
            if let Some(configured) = std::env::var_os("BEAD_RS_BIN") {
                candidates.push(PathBuf::from(configured));
            }
            if let Ok(paths) = which::which_all("bead") {
                candidates.extend(paths);
            }

            let mut seen = HashSet::new();
            for candidate in candidates {
                let identity = fs::canonicalize(&candidate).unwrap_or(candidate.clone());
                if !seen.insert(identity) || !candidate.is_file() {
                    continue;
                }

                let Ok(probe) = isolated_tempdir() else {
                    continue;
                };
                let workspace = probe.path().join("workspace");
                let home = probe.path().join("home");
                if fs::create_dir_all(&workspace).is_err() || fs::create_dir_all(&home).is_err() {
                    continue;
                }
                let usable = std::process::Command::new(&candidate)
                    .current_dir(&workspace)
                    .env("HOME", &home)
                    .args([
                        "init",
                        "--prefix",
                        "probe",
                        "--skip-foreign-workspace",
                        "--no-auto-flush",
                    ])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success());
                if usable {
                    return candidate;
                }
            }

            panic!("a native bead-rs CLI must be installed for Explore e2e tests");
        })
        .clone()
}

fn bead_command(workspace: &Path, home: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(native_bead_path());
    command.current_dir(workspace).env("HOME", home);
    command
}

fn init_workspace(parent: &Path, name: &str, home: &Path) -> Result<PathBuf> {
    let workspace = parent.join(name);
    fs::create_dir_all(&workspace)
        .with_context(|| format!("failed to create workspace {}", workspace.display()))?;

    let git = std::process::Command::new("git")
        .current_dir(&workspace)
        .args(["init", "--quiet"])
        .output()
        .context("failed to initialize workspace Git repository")?;
    if !git.status.success() {
        anyhow::bail!(
            "git init failed for {}: {}",
            workspace.display(),
            String::from_utf8_lossy(&git.stderr)
        );
    }

    fs::write(
        workspace.join(".needle.yaml"),
        format!(
            "bead_cli:\n  backend: bead-rs\n  path: {:?}\n",
            native_bead_path()
        ),
    )?;

    let output = bead_command(&workspace, home)
        .args(["init", "--prefix", "e2e", "--skip-foreign-workspace"])
        .output()
        .context("failed to initialize bead-rs workspace")?;
    if !output.status.success() {
        anyhow::bail!(
            "bead init failed for {}: {}",
            workspace.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(workspace)
}

fn create_bead(workspace: &Path, home: &Path, title: &str, priority: u8) -> Result<BeadId> {
    let priority = priority.to_string();
    let output = bead_command(workspace, home)
        .args([
            "create",
            "--title",
            title,
            "--description",
            title,
            "--priority",
            &priority,
        ])
        .output()
        .with_context(|| format!("failed to create bead in {}", workspace.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "bead create failed for {}: {}",
            workspace.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(BeadId::from(
        String::from_utf8(output.stdout)?.trim().to_string(),
    ))
}

fn add_label(workspace: &Path, home: &Path, bead_id: &BeadId, label: &str) -> Result<()> {
    let output = bead_command(workspace, home)
        .args(["label", "add", bead_id.as_ref(), "--label", label])
        .output()
        .context("failed to add candidate label")?;
    if !output.status.success() {
        anyhow::bail!(
            "bead label add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn store_for_workspace(workspace: &Path) -> Result<CliBeadStore> {
    let backend = builtin_bead_backends()
        .into_iter()
        .find(|backend| backend.name == "bead-rs")
        .context("built-in bead-rs backend descriptor is missing")?;
    CliBeadStore::new(
        backend,
        native_bead_path(),
        workspace.to_path_buf(),
        None,
        None,
        None,
    )
}

fn explore_config(
    workspace_root: &Path,
    rediscovery_cycles: u32,
    scan_interval_cycles: u32,
    max_scan_interval_cycles: u32,
) -> ExploreConfig {
    ExploreConfig {
        enabled: true,
        workspaces: Vec::new(),
        workspace_root: workspace_root.to_path_buf(),
        rediscovery_cycles,
        starvation_threshold_minutes: 1,
        scan_interval_cycles,
        max_scan_interval_cycles,
        stale_claim_ttl: 300,
    }
}

fn read_events(log_dir: &Path) -> Result<Vec<TelemetryEvent>> {
    let mut files = fs::read_dir(log_dir)
        .with_context(|| format!("failed to read telemetry directory {}", log_dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect::<Vec<_>>();
    files.sort();

    let mut events = Vec::new();
    for file in files {
        for line in fs::read_to_string(&file)?.lines() {
            events.push(
                serde_json::from_str(line).with_context(|| {
                    format!("failed to parse telemetry row in {}", file.display())
                })?,
            );
        }
    }
    Ok(events)
}

async fn stop_and_read(telemetry: &Telemetry, log_dir: &Path) -> Result<Vec<TelemetryEvent>> {
    telemetry.shutdown().await;
    read_events(log_dir)
}

#[tokio::test]
async fn recursive_discovery_finds_new_bead_workspace_without_restart_and_wakes_store_scan(
) -> Result<()> {
    let fixture = isolated_tempdir()?;
    let scan_root = fixture.path().join("scan-root");
    let home_root = fixture.path().join("home-root");
    fs::create_dir_all(&scan_root)?;
    fs::create_dir_all(&home_root)?;

    let home_workspace = init_workspace(&home_root, "home", fixture.path())?;
    let first_workspace = init_workspace(&scan_root, "remote-one", fixture.path())?;
    create_bead(&first_workspace, fixture.path(), "first remote bead", 2)?;

    let home_store = store_for_workspace(&home_workspace)?;
    let log_dir = fixture.path().join("logs");
    fs::create_dir_all(&log_dir)?;
    let telemetry = Telemetry::with_log_dir("explore-rediscovery-e2e".to_string(), &log_dir);
    telemetry.start_and_wait().await?;
    let config = explore_config(&scan_root, 60, 8, 8);
    assert!(
        config.workspaces.is_empty(),
        "recursive discovery coverage must exercise the empty workspace-list default"
    );
    let explore = ExploreStrand::new(
        config,
        home_workspace.clone(),
        Registry::new(&fixture.path().join("state")),
        telemetry.clone(),
        "rediscovery-e2e".to_string(),
    );

    let first = explore.evaluate(&home_store, &HashSet::new()).await;
    assert!(
        matches!(first, StrandResult::BeadFound(_)),
        "initial remote workspace should be selectable"
    );

    // The worker remains alive while a new bead workspace appears. The root
    // mtime wake bypasses the long empty-scan interval; no reconstruction of
    // ExploreStrand occurs here.
    let second_workspace = init_workspace(&scan_root, "remote-two", fixture.path())?;
    let second_id = create_bead(&second_workspace, fixture.path(), "new remote bead", 1)?;
    let second = explore.evaluate(&home_store, &HashSet::new()).await;
    let StrandResult::BeadFound(candidates) = second else {
        anyhow::bail!("new workspace was not visible without restarting Explore");
    };
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.id == second_id && candidate.workspace == second_workspace),
        "newly created workspace must contribute its bead"
    );

    // Put the next bead in an already-known store. This exercises the store
    // watcher rather than the discovery-surface watcher.
    let third_id = create_bead(&first_workspace, fixture.path(), "post-startup bead", 1)?;
    let third = explore.evaluate(&home_store, &HashSet::new()).await;
    let StrandResult::BeadFound(candidates) = third else {
        anyhow::bail!("a changed known store did not wake Explore");
    };
    assert!(
        candidates.iter().any(|candidate| candidate.id == third_id),
        "changed known store must be rescanned before adaptive backoff expires"
    );

    drop(explore);
    let events = stop_and_read(&telemetry, &log_dir).await?;
    assert!(
        events
            .iter()
            .filter(|event| event.event_type == "explore.scan_summary")
            .count()
            >= 3,
        "each wakeup scan should emit a scan summary"
    );
    Ok(())
}

#[tokio::test]
async fn scan_rotation_visits_every_workspace_and_unavailable_workspace_cannot_starve_healthy_work(
) -> Result<()> {
    let fixture = isolated_tempdir()?;
    let scan_root = fixture.path().join("scan-root");
    let home_root = fixture.path().join("home-root");
    fs::create_dir_all(&scan_root)?;
    fs::create_dir_all(&home_root)?;
    let home_workspace = init_workspace(&home_root, "home", fixture.path())?;
    let home_store = store_for_workspace(&home_workspace)?;

    let mut workspaces = Vec::new();
    for index in 0..4 {
        let workspace = init_workspace(&scan_root, &format!("remote-{index}"), fixture.path())?;
        create_bead(
            &workspace,
            fixture.path(),
            &format!("remote bead {index}"),
            1,
        )?;
        workspaces.push(workspace);
    }

    // Each worker gets a fresh telemetry stream, but all scan the same real
    // stores. At least two worker identities must produce different rotated
    // orders; every order must still contain every workspace.
    let mut observed_orders = HashSet::new();
    for worker_index in 0..8 {
        let log_dir = fixture.path().join(format!("rotation-logs-{worker_index}"));
        fs::create_dir_all(&log_dir)?;
        let telemetry = Telemetry::with_log_dir(format!("rotation-{worker_index}"), &log_dir);
        telemetry.start_and_wait().await?;
        let explore = ExploreStrand::new(
            explore_config(&scan_root, 60, 1, 1),
            home_workspace.clone(),
            Registry::new(
                &fixture
                    .path()
                    .join(format!("rotation-state-{worker_index}")),
            ),
            telemetry.clone(),
            format!("rotation-worker-{worker_index}"),
        );
        let result = explore.evaluate(&home_store, &HashSet::new()).await;
        assert!(matches!(result, StrandResult::BeadFound(_)));
        drop(explore);
        let events = stop_and_read(&telemetry, &log_dir).await?;
        let summary = events
            .iter()
            .find(|event| event.event_type == "explore.scan_summary")
            .context("Explore must emit a scan summary")?;
        let visited = summary.data["workspaces_visited"]
            .as_array()
            .context("scan summary must list visited workspaces")?
            .iter()
            .map(|path| path.as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>();
        assert_eq!(visited.len(), workspaces.len());
        assert!(
            workspaces
                .iter()
                .all(|workspace| visited.contains(&workspace.display().to_string())),
            "rotation must not make any workspace unreachable"
        );
        observed_orders.insert(visited);
    }
    assert!(
        observed_orders.len() >= 2,
        "different worker identities should rotate the scan order"
    );

    // A malformed store is discovered alongside the healthy store. Explore's
    // validated first pass must skip it and continue into the healthy frontier.
    let unavailable = init_workspace(&scan_root, "unavailable", fixture.path())?;
    fs::write(
        unavailable.join(".beads").join("beads.db"),
        b"not a sqlite database",
    )?;
    let log_dir = fixture.path().join("unavailable-logs");
    fs::create_dir_all(&log_dir)?;
    let telemetry = Telemetry::with_log_dir("unavailable-e2e".to_string(), &log_dir);
    telemetry.start_and_wait().await?;
    let explore = ExploreStrand::new(
        explore_config(&scan_root, 1, 1, 1),
        home_workspace,
        Registry::new(&fixture.path().join("unavailable-state")),
        telemetry.clone(),
        "unavailable-worker".to_string(),
    );
    let result = explore.evaluate(&home_store, &HashSet::new()).await;
    let StrandResult::BeadFound(candidates) = result else {
        anyhow::bail!("an unavailable workspace stalled healthy Explore work");
    };
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.workspace == workspaces[0]),
        "healthy workspace must remain selectable when a peer cannot be opened"
    );
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.workspace != unavailable),
        "unavailable workspace must not contribute candidates"
    );
    drop(explore);
    let events = stop_and_read(&telemetry, &log_dir).await?;
    assert!(
        events.iter().any(|event| {
            event.event_type == "explore.workspace_quarantined"
                && event.data["workspace"] == unavailable.display().to_string()
        }),
        "unavailable workspace should be represented in quarantine telemetry"
    );
    Ok(())
}

#[tokio::test]
async fn excluded_candidates_retry_at_floor_and_event_driven_store_changes_do_not_starve(
) -> Result<()> {
    let fixture = isolated_tempdir()?;
    let scan_root = fixture.path().join("scan-root");
    let home_root = fixture.path().join("home-root");
    fs::create_dir_all(&scan_root)?;
    fs::create_dir_all(&home_root)?;
    let home_workspace = init_workspace(&home_root, "home", fixture.path())?;
    let remote = init_workspace(&scan_root, "remote", fixture.path())?;
    let excluded_id = create_bead(&remote, fixture.path(), "lane mismatch", 1)?;
    add_label(&remote, fixture.path(), &excluded_id, "other-lane")?;
    let home_store = store_for_workspace(&home_workspace)?;

    let log_dir = fixture.path().join("logs");
    fs::create_dir_all(&log_dir)?;
    let telemetry = Telemetry::with_log_dir("excluded-cadence-e2e".to_string(), &log_dir);
    telemetry.start_and_wait().await?;
    let explore = ExploreStrand::new(
        explore_config(&scan_root, 60, 2, 8),
        home_workspace,
        Registry::new(&fixture.path().join("state")),
        telemetry.clone(),
        "excluded-cadence-worker".to_string(),
    )
    .with_lane(Some(PluckLaneConfig {
        label: "required-lane".to_string(),
        workers: vec!["excluded-cadence-worker".to_string()],
        when_empty: needle::config::LaneWhenEmpty::Normal,
    }));

    assert!(matches!(
        explore.evaluate(&home_store, &HashSet::new()).await,
        StrandResult::NoWork
    ));
    // One floor interval is two selection cycles. The second call is the
    // cadence wait; the third call must rescan instead of ramping toward the
    // empty-scan ceiling merely because a candidate was excluded.
    assert!(matches!(
        explore.evaluate(&home_store, &HashSet::new()).await,
        StrandResult::NoWork
    ));
    let third = explore.evaluate(&home_store, &HashSet::new()).await;
    assert!(matches!(third, StrandResult::NoWork));

    // A real store mutation wakes the scan immediately even though the
    // adaptive cadence would otherwise be waiting. The new bead is in the
    // required lane, so it must become selectable without a restart.
    let admitted_id = create_bead(&remote, fixture.path(), "required lane bead", 1)?;
    add_label(&remote, fixture.path(), &admitted_id, "required-lane")?;
    let result = explore.evaluate(&home_store, &HashSet::new()).await;
    let StrandResult::BeadFound(candidates) = result else {
        anyhow::bail!("store change did not wake Explore from adaptive backoff");
    };
    assert!(candidates
        .iter()
        .any(|candidate| candidate.id == admitted_id));

    drop(explore);
    let events = stop_and_read(&telemetry, &log_dir).await?;
    let summaries = events
        .iter()
        .filter(|event| event.event_type == "explore.scan_summary")
        .collect::<Vec<_>>();
    assert!(
        summaries.len() >= 3,
        "excluded and changed-store scans are observable"
    );
    assert!(summaries.iter().any(|summary| {
        summary.data["exclusion_reasons"]
            .as_array()
            .is_some_and(|reasons| reasons.iter().any(|reason| reason == "filtered_1"))
    }));
    Ok(())
}

#[tokio::test]
async fn explore_scan_with_ready_work_emits_starvation_alarm_without_target_write() -> Result<()> {
    let fixture = isolated_tempdir()?;
    let scan_root = fixture.path().join("scan-root");
    let home_root = fixture.path().join("home-root");
    fs::create_dir_all(&scan_root)?;
    fs::create_dir_all(&home_root)?;
    let home_workspace = init_workspace(&home_root, "home", fixture.path())?;
    let remote = init_workspace(&scan_root, "remote", fixture.path())?;
    create_bead(&remote, fixture.path(), "unclaimed remote work", 1)?;
    let home_store = store_for_workspace(&home_workspace)?;

    let log_dir = fixture.path().join("logs");
    fs::create_dir_all(&log_dir)?;
    let telemetry = Telemetry::with_log_dir("starvation-e2e".to_string(), &log_dir);
    telemetry.configure_explore_starvation(1);
    telemetry.start_and_wait().await?;
    let explore = ExploreStrand::new(
        explore_config(&scan_root, 60, 1, 1),
        home_workspace,
        Registry::new(&fixture.path().join("state")),
        telemetry.clone(),
        "starvation-worker".to_string(),
    );

    let result = explore.evaluate(&home_store, &HashSet::new()).await;
    assert!(matches!(result, StrandResult::BeadFound(_)));

    // Explore emitted the first real scan summary above. Advance the summary
    // clock by one threshold without sleeping for a minute; this mirrors the
    // worker's next scan and proves the telemetry liveness transition without
    // mutating the target store or claiming the bead.
    telemetry.emit(
        needle::telemetry::EventKind::ExploreScanSummary {
            workspaces_visited: vec![remote.display().to_string()],
            workspaces_with_candidates: vec![remote.display().to_string()],
            total_candidates: 1,
            exclusion_reasons: Vec::new(),
            duration_ms: 1,
            scan_start_at: Utc::now().to_rfc3339(),
        },
        Utc::now() + chrono::Duration::minutes(1),
    )?;

    drop(explore);
    let events = stop_and_read(&telemetry, &log_dir).await?;
    let alarms = events
        .iter()
        .filter(|event| event.event_type == "explore.starvation_alarm")
        .collect::<Vec<_>>();
    assert_eq!(alarms.len(), 1, "one ready episode should emit one alarm");
    assert_eq!(alarms[0].data["threshold_minutes"], 1);
    assert_eq!(alarms[0].data["ready_beads_count"], 1);
    assert_eq!(
        alarms[0].data["workspaces_with_ready"],
        serde_json::json!([remote.display().to_string()])
    );
    assert!(
        events
            .iter()
            .all(|event| event.event_type != "bead.created"),
        "starvation telemetry must not create target-store work"
    );
    Ok(())
}
