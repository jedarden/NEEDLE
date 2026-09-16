//! End-to-end coverage for the Phase 19 escalation ladder.
//!
//! The worker is run as a real subprocess against an isolated bead-rs store.
//! The adapter and rung-4 analysis command are tiny fixture programs, while
//! every assertion about queue state comes from the real `bead` CLI.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde_json::Value;
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;
use tokio::process::Command as TokioCommand;
use tokio::time::{timeout, Duration};

fn needle_binary_path() -> PathBuf {
    std::env::var_os("NEXTEST_BIN_EXE_needle")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_needle")))
}

fn native_bead_path() -> PathBuf {
    let mut candidates = Vec::new();
    if let Some(configured) = std::env::var_os("BEAD_RS_BIN") {
        candidates.push(PathBuf::from(configured));
    }
    if let Ok(paths) = which::which_all("bead") {
        candidates.extend(paths);
    }
    candidates.push(PathBuf::from("/home/coding/.cargo/bin/bead"));

    let mut seen = HashSet::new();
    for candidate in candidates {
        let identity = fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone());
        if !seen.insert(identity) || !candidate.is_file() {
            continue;
        }

        let Ok(probe) = tempfile::tempdir() else {
            continue;
        };
        let workspace = probe.path().join("workspace");
        let home = probe.path().join("home");
        if fs::create_dir_all(&workspace).is_err() || fs::create_dir_all(&home).is_err() {
            continue;
        }
        let usable = Command::new(&candidate)
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

    panic!("a native bead-rs CLI is required for escalation_ladder")
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    bail!("escalation_ladder requires Unix process and PATH semantics")
}

struct Fixture {
    _root: TempDir,
    workspace: PathBuf,
    home: PathBuf,
    bead: PathBuf,
    failure_log: PathBuf,
    mitosis_log: PathBuf,
    analysis_log: PathBuf,
    gate_log: PathBuf,
    normal_path: OsString,
    gate_path: OsString,
}

impl Fixture {
    fn new() -> Result<Self> {
        let root = tempfile::Builder::new()
            .prefix("needle-escalation-ladder-")
            .tempdir()
            .context("failed to create escalation fixture root")?;
        let workspace = root.path().join("workspace");
        let home = root.path().join("home");
        let mock_bin = root.path().join("mock-bin");
        let gate_bin = root.path().join("gate-bin");
        for path in [&workspace, &home, &mock_bin, &gate_bin] {
            fs::create_dir_all(path).with_context(|| {
                format!("failed to create fixture directory {}", path.display())
            })?;
        }

        let native = native_bead_path();
        let bead = root.path().join("bead");
        fs::write(
            &bead,
            format!(
                "#!/bin/sh\nexport HOME='{}'\nexec '{}' \"$@\"\n",
                home.display(),
                native.display()
            ),
        )?;
        make_executable(&bead)?;

        let init = Command::new(&bead)
            .current_dir(&workspace)
            .args([
                "init",
                "--prefix",
                "ladder",
                "--skip-foreign-workspace",
                "--no-auto-flush",
            ])
            .output()?;
        ensure_success("bead init", init)?;

        let failure_log = root.path().join("failure.log");
        let mitosis_log = root.path().join("mitosis.log");
        let analysis_log = root.path().join("analysis.log");
        let gate_log = root.path().join("gate.log");

        let analysis_agent = mock_bin.join("ladder-agent");
        fs::write(
            &analysis_agent,
            "#!/usr/bin/env bash\nprintf '%s\\n' analysis >> \"$ANALYSIS_LOG\"\nprintf '%s\\n' '{\"decision\":\"rescope\",\"child\":{\"title\":\"Repair the external dependency\",\"body\":\"Resolve the dependency in a separately claimable task.\"}}'\n",
        )?;
        make_executable(&analysis_agent)?;

        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let mut normal_parts = vec![mock_bin.clone()];
        normal_parts.extend(std::env::split_paths(&old_path));
        let normal_path = std::env::join_paths(normal_parts)?;
        let bash_path = which::which("bash").context("bash is required for the fixture")?;
        let cat_path = which::which("cat").context("cat is required for the fixture")?;
        let gate_shell = gate_bin.join("bash");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&bash_path, &gate_shell)
            .context("failed to add bash to gate PATH")?;
        let gate_cat = gate_bin.join("cat");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&cat_path, &gate_cat)
            .context("failed to add cat to gate PATH")?;
        #[cfg(not(unix))]
        let _ = (gate_shell, gate_cat);
        let gate_path = std::env::join_paths([gate_bin.clone()])?;

        let fixture = Self {
            _root: root,
            workspace,
            home,
            bead,
            failure_log,
            mitosis_log,
            analysis_log,
            gate_log,
            normal_path,
            gate_path,
        };
        fixture.write_adapter()?;
        fixture.write_global_config(false)?;
        fixture.write_workspace_config(false)?;
        fs::create_dir_all(fixture.workspace.join("docs/plan"))?;
        fs::write(
            fixture.workspace.join("docs/plan/plan.md"),
            "# Fixture plan\n\nThe plan intentionally does not explain the external dependency.\n",
        )?;
        Ok(fixture)
    }

    fn write_adapter(&self) -> Result<()> {
        let adapter_dir = self.home.join(".config/needle/adapters");
        fs::create_dir_all(&adapter_dir)?;
        fs::write(
            adapter_dir.join("ladder-agent.yaml"),
            format!(
                "name: ladder-agent\ndescription: deterministic escalation fixture\nagent_cli: bash\ninvoke_template: |\n  prompt=$(cat \"{{prompt_file}}\")\n  if [ -f \"{{workspace}}/.gate-mode\" ]; then\n    printf '%s\\n' gate >> \"$GATE_LOG\"\n    exit 0\n  fi\n  case \"$prompt\" in\n    *'## Mitosis Analysis'*)\n      printf '%s\\n' mitosis >> \"$MITOSIS_LOG\"\n      printf '%s\\n' '{{\"splittable\":false,\"reason\":\"fixture is intentionally unsplittable\"}}'\n      exit 0\n      ;;\n    *)\n      printf '%s\\n' failure >> \"$FAILURE_LOG\"\n      exit 1\n      ;;\n  esac\ntimeout_secs: 30\nenvironment:\n  FAILURE_LOG: {}\n  MITOSIS_LOG: {}\n  ANALYSIS_LOG: {}\n  GATE_LOG: {}\n",
                self.failure_log.display(),
                self.mitosis_log.display(),
                self.analysis_log.display(),
                self.gate_log.display(),
            ),
        )?;
        Ok(())
    }

    fn write_global_config(&self, post_mitosis: bool) -> Result<()> {
        let config_dir = self.home.join(".config/needle");
        fs::create_dir_all(&config_dir)?;
        let (first_failure_only, force_failure_threshold) = if post_mitosis {
            ("true", "0")
        } else {
            ("false", "3")
        };
        fs::write(
            config_dir.join("config.yaml"),
            format!(
                "agent:\n  default: ladder-agent\n  timeout: 30\n  adapters_dir: {}/.config/needle/adapters\nworker:\n  max_workers: 1\n  idle_action: exit\n  allow_exit_without_supervisor: true\n  enforce_shipped_work: false\n  cpu_load_warn: 1.0\n  memory_free_warn_mb: 1\n  idle_backoff_min: 0\n  idle_backoff_max: 0\n  short_retry_backoff: 0\nworkspace:\n  home: {}/.needle\nstrands:\n  pluck:\n    split_after_failures: 0\n  explore:\n    enabled: false\n    workspace_root: {}\n    workspaces: []\n  weave:\n    enabled: false\n  unravel:\n    enabled: false\n  pulse:\n    enabled: false\n  reflect:\n    enabled: false\n  resolve:\n    enabled: false\n  splice:\n    enabled: false\n  mitosis:\n    enabled: true\n    first_failure_only: {}\n    force_failure_threshold: {}\n  analyze:\n    enabled: true\noutcome:\n  quarantine_after_failures: 5\nvalidation:\n  default_gates:\n    enabled: false\ntelemetry:\n  file_sink:\n    enabled: false\n",
                self.home.display(),
                self.home.display(),
                self.workspace.display(),
                first_failure_only,
                force_failure_threshold,
            ),
        )?;
        Ok(())
    }

    fn write_workspace_config(&self, gate_mode: bool) -> Result<()> {
        let gates = if gate_mode {
            "gates:\n  - type: command\n    commands:\n      - /definitely/missing-gate-command\n    run_in: workspace\n"
        } else {
            "gates: []\n"
        };
        fs::write(
            self.workspace.join(".needle.yaml"),
            format!(
                "bead_cli:\n  backend: bead-rs\n  path: {}\n{}",
                self.bead.display(),
                gates
            ),
        )?;
        Ok(())
    }

    fn cli(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.bead);
        command.current_dir(&self.workspace).env("HOME", &self.home);
        command.args(args);
        command
    }

    fn run_cli(&self, args: &[&str]) -> Result<Output> {
        let rendered = args.join(" ");
        let output = self
            .cli(args)
            .output()
            .with_context(|| format!("failed to run bead {rendered}"))?;
        ensure_success(&format!("bead {rendered}"), output)
    }

    fn create_bead(&self, title: &str, description: &str) -> Result<String> {
        let output = self.run_cli(&[
            "create",
            "--title",
            title,
            "--description",
            description,
            "--priority",
            "2",
        ])?;
        let id = String::from_utf8(output.stdout)?.trim().to_string();
        if id.is_empty() {
            bail!("bead create returned an empty id")
        }
        Ok(id)
    }

    fn show(&self, id: &str) -> Result<Value> {
        let output = self.run_cli(&["show", id, "--json"])?;
        let value: Value = serde_json::from_slice(&output.stdout)
            .with_context(|| format!("invalid bead show JSON for {id}"))?;
        Ok(value
            .as_array()
            .and_then(|values| values.first().cloned())
            .unwrap_or(value))
    }

    fn why(&self, id: &str) -> Result<Value> {
        let output = self.run_cli(&["why", "--id", id, "--json"])?;
        serde_json::from_slice(&output.stdout)
            .with_context(|| format!("invalid bead why JSON for {id}"))
    }

    fn ready(&self) -> Result<Vec<Value>> {
        let output = self.run_cli(&["list", "--ready", "--json"])?;
        let text = String::from_utf8(output.stdout)?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            return Ok(match value {
                Value::Array(values) => values,
                value => vec![value],
            });
        }
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).context("invalid bead list JSON line"))
            .collect()
    }

    fn labels(&self, id: &str) -> Result<Vec<String>> {
        Ok(self
            .show(id)?
            .get("labels")
            .and_then(Value::as_array)
            .map(|labels| {
                labels
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default())
    }

    fn failure_count(&self, id: &str) -> Result<u32> {
        Ok(self
            .labels(id)?
            .iter()
            .filter_map(|label| label.strip_prefix("failure-count:"))
            .filter_map(|count| count.parse::<u32>().ok())
            .max()
            .unwrap_or(0))
    }

    fn label_with_prefix(&self, id: &str, prefix: &str) -> Result<String> {
        self.labels(id)?
            .into_iter()
            .find(|label| label.starts_with(prefix))
            .ok_or_else(|| anyhow::anyhow!("{id} has no label with prefix {prefix}"))
    }

    fn assert_ready(&self, id: &str) -> Result<()> {
        let ready = self.ready()?;
        if ready
            .iter()
            .any(|bead| bead.get("id").and_then(Value::as_str) == Some(id))
        {
            Ok(())
        } else {
            bail!("bead {id} was not present in `bead list --ready --json`: {ready:?}")
        }
    }

    fn expire_and_allow_redispatch(&self, id: &str) -> Result<()> {
        let labels = self.labels(id)?;
        for label in labels.iter().filter(|label| {
            label.starts_with("quarantine-until:") || label.starts_with("quarantine:content-hash:")
        }) {
            self.run_cli(&["label", "remove", id, "--label", label])?;
        }
        let expired = (Utc::now() - ChronoDuration::seconds(1)).to_rfc3339();
        let expired_label = format!("quarantine-until:{expired}");
        self.run_cli(&["label", "add", id, "--label", &expired_label])?;
        Ok(())
    }

    fn expire_without_revision(&self, id: &str) -> Result<()> {
        let labels = self.labels(id)?;
        for label in labels
            .iter()
            .filter(|label| label.starts_with("quarantine-until:"))
        {
            self.run_cli(&["label", "remove", id, "--label", label])?;
        }
        let expired = (Utc::now() - ChronoDuration::seconds(1)).to_rfc3339();
        let expired_label = format!("quarantine-until:{expired}");
        self.run_cli(&["label", "add", id, "--label", &expired_label])?;
        Ok(())
    }

    async fn run_worker(&self, identifier: &str, path: &OsString) -> Result<()> {
        let mut command = TokioCommand::new(needle_binary_path());
        command
            .current_dir(&self.workspace)
            .env("HOME", &self.home)
            .env("PATH", path)
            .env("NEEDLE_INNER", "1")
            .env("ANALYSIS_LOG", &self.analysis_log)
            .args([
                "run",
                "--workspace",
                self.workspace.to_str().context("workspace is not UTF-8")?,
                "--agent",
                "ladder-agent",
                "--count",
                "1",
                "--identifier",
                identifier,
            ]);
        let output = timeout(Duration::from_secs(120), command.output())
            .await
            .with_context(|| format!("worker {identifier} timed out"))??;
        if !output.status.success() {
            bail!(
                "worker {identifier} failed with {}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        }
        Ok(())
    }

    fn log_lines(path: &Path) -> usize {
        fs::read_to_string(path)
            .map(|contents| contents.lines().count())
            .unwrap_or(0)
    }
}

fn ensure_success(operation: &str, output: Output) -> Result<Output> {
    if output.status.success() {
        Ok(output)
    } else {
        bail!(
            "{operation} failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }
}

fn assert_manual_blocked_is_false(why: &Value, id: &str) {
    assert_eq!(
        why.get("manual_blocked").and_then(Value::as_bool),
        Some(false),
        "bead why --id {id} must never use manual_blocked"
    );
}

fn assert_quarantine_window(
    fixture: &Fixture,
    id: &str,
    expected_round: u32,
    expected_seconds: i64,
) -> Result<()> {
    let labels = fixture.labels(id)?;
    assert!(labels.iter().any(|label| label == "quarantined"));
    assert!(labels
        .iter()
        .any(|label| label == &format!("quarantine-round:{expected_round}")));
    assert!(labels
        .iter()
        .any(|label| label.starts_with("quarantine:failure-count:")));
    let until_label = fixture.label_with_prefix(id, "quarantine-until:")?;
    let until: DateTime<Utc> = DateTime::parse_from_rfc3339(
        until_label
            .strip_prefix("quarantine-until:")
            .context("quarantine label lost its timestamp")?,
    )?
    .with_timezone(&Utc);
    let remaining = until - Utc::now();
    assert!(
        remaining >= ChronoDuration::seconds(expected_seconds - 15)
            && remaining <= ChronoDuration::seconds(expected_seconds + 15),
        "round {expected_round} backoff was {remaining:?}, expected about {expected_seconds}s"
    );
    Ok(())
}

fn assert_failure_count(fixture: &Fixture, id: &str, expected: u32) -> Result<()> {
    let actual = fixture.failure_count(id)?;
    assert_eq!(actual, expected, "unexpected failure-count for {id}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn escalation_ladder_end_to_end() -> Result<()> {
    let fixture = Fixture::new()?;
    let parent = fixture.create_bead(
        "Exercise the escalation ladder",
        "A single intentionally failing fixture task.",
    )?;

    // Rung 1 and the threshold crossing: each one-shot worker dispatch is a
    // real claim/release cycle. The fixture's retry cooldown is expired by a
    // real bead label edit between attempts so the test does not sleep.
    for attempt in 1..=5 {
        fixture
            .run_worker(&format!("ladder-failure-{attempt}"), &fixture.normal_path)
            .await?;
        assert_failure_count(&fixture, &parent, attempt)?;
        if attempt == 3 {
            // The force threshold is deliberately a one-shot fixture hook:
            // once rung 2 has been observed, ordinary retries must not spend
            // additional Mitosis dispatches before quarantine.
            fixture.write_global_config(true)?;
        }
        if attempt < 5 {
            fixture.expire_and_allow_redispatch(&parent)?;
        }
    }

    // Rung 2 is a post-failure Mitosis attempt at count 3. The fixture says
    // the bead is not splittable, so no child is created and the parent keeps
    // climbing toward quarantine.
    assert_eq!(Fixture::log_lines(&fixture.mitosis_log), 1);
    let beads = fixture.ready()?;
    assert!(beads.iter().any(|bead| {
        bead.get("title")
            .and_then(Value::as_str)
            .is_some_and(|title| title == "Exercise the escalation ladder")
    }));

    // Rung 3, round 1: the bead is labeled and remains open rather than
    // acquiring the backend's manual block state.
    assert_quarantine_window(&fixture, &parent, 1, 2 * 60 * 60)?;
    assert_manual_blocked_is_false(&fixture.why(&parent)?, &parent);
    assert_eq!(Fixture::log_lines(&fixture.failure_log), 5);

    // An active quarantine is not dispatched. `bead why` is the authoritative
    // backend explanation, while the unchanged mock log proves Pluck honored
    // the time-bounded label before the timestamp passed.
    fixture
        .run_worker("ladder-quarantine-active", &fixture.normal_path)
        .await?;
    assert_eq!(Fixture::log_lines(&fixture.failure_log), 5);
    assert_failure_count(&fixture, &parent, 5)?;
    assert_manual_blocked_is_false(&fixture.why(&parent)?, &parent);

    // Expiry with the content-hash evidence removed is the backend-compatible
    // redispatch path (bead-rs descriptions are immutable), preserving the
    // count. The next failed attempt enters round 2 and doubles the window.
    fixture.expire_and_allow_redispatch(&parent)?;
    fixture.assert_ready(&parent)?;
    fixture
        .run_worker("ladder-round-2", &fixture.normal_path)
        .await?;
    assert_failure_count(&fixture, &parent, 6)?;
    assert_quarantine_window(&fixture, &parent, 2, 4 * 60 * 60)?;
    assert_manual_blocked_is_false(&fixture.why(&parent)?, &parent);

    // Round 3 repeats the same expired-window -> redispatch transition and
    // doubles again.
    fixture.expire_and_allow_redispatch(&parent)?;
    fixture.assert_ready(&parent)?;
    fixture
        .run_worker("ladder-round-3", &fixture.normal_path)
        .await?;
    assert_failure_count(&fixture, &parent, 7)?;
    assert_quarantine_window(&fixture, &parent, 3, 8 * 60 * 60)?;
    assert_manual_blocked_is_false(&fixture.why(&parent)?, &parent);

    // The final expiry keeps the content unchanged. Pluck parks the exhausted
    // round-3 bead and the Analyze strand gets exactly one plan-grounded
    // dispatch, whose fixture conclusion creates the permitted re-scoped child.
    fixture.expire_without_revision(&parent)?;
    fixture.assert_ready(&parent)?;
    fixture
        .run_worker("ladder-analysis", &fixture.normal_path)
        .await?;
    assert_eq!(Fixture::log_lines(&fixture.analysis_log), 1);
    let final_show = fixture.show(&parent)?;
    let final_labels = fixture.labels(&parent)?;
    assert!(!final_labels.iter().any(|label| label == "quarantined"));
    assert!(!final_labels
        .iter()
        .any(|label| label.starts_with("quarantine-round:")));
    assert!(final_show.get("dependencies").is_some());
    assert_manual_blocked_is_false(&fixture.why(&parent)?, &parent);
    let ready_after_analysis = fixture.ready()?;
    let child = ready_after_analysis
        .iter()
        .find(|bead| {
            bead.get("title")
                .and_then(Value::as_str)
                .is_some_and(|title| title == "[Rung 4 re-scope] Repair the external dependency")
        })
        .context("rung-4 analysis did not create a claimable child bead")?;
    let child_labels = child
        .get("labels")
        .and_then(Value::as_array)
        .context("rung-4 child had no labels")?;
    assert!(child_labels.iter().any(|label| label == "split-child"));
    assert!(child_labels
        .iter()
        .any(|label| label == "escalation:rescoped"));

    // A second worker cannot spend another analysis dispatch on the same rung.
    fixture
        .run_worker("ladder-analysis-again", &fixture.normal_path)
        .await?;
    assert_eq!(Fixture::log_lines(&fixture.analysis_log), 1);

    // ADR-023: three GateError outcomes release a separate bead without ever
    // touching failure-count, then degrade the workspace and file one alert.
    fixture.write_workspace_config(true)?;
    fs::write(fixture.workspace.join(".gate-mode"), "enabled")?;
    let gate_bead = fixture.create_bead(
        "Gate error must not penalize work",
        "The command gate cannot be spawned in this fixture.",
    )?;
    for attempt in 1..=3 {
        fixture
            .run_worker(&format!("ladder-gate-errors-{attempt}"), &fixture.gate_path)
            .await?;
    }
    assert_eq!(Fixture::log_lines(&fixture.gate_log), 3);
    assert_failure_count(&fixture, &gate_bead, 0)?;
    let gate_why = fixture.why(&gate_bead)?;
    assert_manual_blocked_is_false(&gate_why, &gate_bead);

    let gate_health_dir = fixture.home.join(".needle/state/gate-health");
    let mut degraded = Vec::new();
    for entry in fs::read_dir(&gate_health_dir).with_context(|| {
        format!(
            "missing gate health directory {}",
            gate_health_dir.display()
        )
    })? {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let state: Value = serde_json::from_slice(&fs::read(&path)?)?;
        if state.get("workspace").and_then(Value::as_str) == fixture.workspace.to_str() {
            degraded.push(state);
        }
    }
    assert_eq!(degraded.len(), 1, "expected one gate-health state record");
    assert_eq!(degraded[0].get("degraded"), Some(&Value::Bool(true)));
    assert!(degraded[0]
        .get("consecutive_errors")
        .and_then(Value::as_u64)
        .is_some_and(|count| count >= 3));

    let ready_after_gate = fixture.ready()?;
    let gate_alerts: Vec<&Value> = ready_after_gate
        .iter()
        .filter(|bead| {
            bead.get("title")
                .and_then(Value::as_str)
                .is_some_and(|title| title.starts_with("Gate broken:"))
        })
        .collect();
    assert_eq!(gate_alerts.len(), 1, "gate degradation must file one alert");
    let gate_alert_id = gate_alerts[0]
        .get("id")
        .and_then(Value::as_str)
        .context("Gate broken alert had no id")?;
    assert_manual_blocked_is_false(&fixture.why(gate_alert_id)?, gate_alert_id);
    let gate_alert_labels = gate_alerts[0]
        .get("labels")
        .and_then(Value::as_array)
        .context("Gate broken alert had no labels")?;
    assert!(gate_alert_labels.iter().any(|label| label == "infra"));

    Ok(())
}
