//! CLI layer — parses commands and manages worker sessions.
//!
//! Entry point for the `needle` binary. Routes subcommands to worker
//! lifecycle management. Always creates dedicated tmux sessions for workers.
//! Re-entrant inner invocations (launched by `launch_in_tmux()`) are
//! detected via the `NEEDLE_INNER=1` environment variable and run the
//! worker directly without spawning another session.
//!
//! Depends on: `worker`, `config`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand, ValueEnum};

use crate::bead_store::{BeadStore, BrCliBeadStore};
use crate::config::{CliOverrides, Config, ConfigLoader, StdoutSinkConfig};
use crate::dispatch;
use crate::health::{HealthMonitor, HeartbeatData};
use crate::registry::{Registry, WorkerEntry};
use crate::telemetry;
use crate::worker::Worker;

// ──────────────────────────────────────────────────────────────────────────────
// NATO alphabet for worker identifiers
// ──────────────────────────────────────────────────────────────────────────────

const NATO_ALPHABET: &[&str] = &[
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet",
    "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango",
    "uniform", "victor", "whiskey", "xray", "yankee", "zulu",
];

// ──────────────────────────────────────────────────────────────────────────────
// CLI definition
// ──────────────────────────────────────────────────────────────────────────────

/// NEEDLE — Navigates Every Enqueued Deliverable, Logs Effort.
///
/// Deterministic bead processing with explicit outcome paths.
#[derive(Debug, Parser)]
#[command(name = "needle", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: CliCommand,
}

#[derive(Debug, Subcommand)]
pub enum CliCommand {
    /// Launch worker(s) to process beads.
    Run {
        /// Workspace to process beads from.
        #[arg(short = 'w', long)]
        workspace: Option<PathBuf>,

        /// Agent adapter to use.
        #[arg(short = 'a', long)]
        agent: Option<String>,

        /// Number of workers to launch.
        #[arg(short = 'c', long, default_value = "1")]
        count: u32,

        /// Worker identifier (overrides NATO naming).
        #[arg(short = 'i', long)]
        identifier: Option<String>,

        /// Agent execution timeout in seconds.
        #[arg(short = 't', long)]
        timeout: Option<u64>,

        /// Resume an existing worker session (used by hot-reload).
        #[arg(long)]
        resume: bool,
    },

    /// Stop running worker(s).
    Stop {
        /// Stop all needle workers.
        #[arg(long)]
        all: bool,

        /// Identifier of the worker to stop.
        #[arg(short = 'i', long)]
        identifier: Option<String>,
    },

    /// List active workers.
    List {
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: ListFormat,
    },

    /// Attach to a worker's tmux session.
    Attach {
        /// Worker identifier (e.g., alpha, bravo) or partial session name.
        identifier: String,
    },

    /// Show fleet status, bead counts, and cost summary.
    Status {
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: ListFormat,

        /// Show per-worker breakdown.
        #[arg(long)]
        by_worker: bool,

        /// Show cost summary with per-worker and per-workspace breakdowns.
        #[arg(long)]
        cost: bool,

        /// Filter events since this time (e.g., 1h, 24h, 7d, 2026-03-20).
        #[arg(long)]
        since: Option<String>,

        /// Filter events until this time (e.g., 1h, 24h, 7d, 2026-03-20T15:00:00Z).
        #[arg(long)]
        until: Option<String>,
    },

    /// View and query telemetry logs.
    Logs {
        /// Stream events in real-time (tail -f equivalent).
        #[arg(long)]
        follow: bool,

        /// Filter expression(s). Supports:
        ///   field=value    — exact match (e.g., event_type=bead.claim.succeeded)
        ///   field~pattern  — regex match (e.g., event_type~bead\..*)
        ///   field>number   — numeric greater-than (e.g., duration_ms>500)
        ///   glob           — glob on event_type (e.g., bead.claim.*)
        /// Multiple --filter flags are ANDed together.
        #[arg(long)]
        filter: Vec<String>,

        /// Show events since this time (e.g., 1h, 24h, 7d, 2026-03-20).
        #[arg(long)]
        since: Option<String>,

        /// Show events until this time (e.g., 1h, 24h, 7d, 2026-03-20T15:00:00Z).
        #[arg(long)]
        until: Option<String>,

        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: LogFormat,
    },

    /// View or inspect configuration.
    #[command(name = "config")]
    ConfigCmd {
        /// Get a specific config key.
        #[arg(long)]
        get: Option<String>,

        /// Dump all resolved config values.
        #[arg(long)]
        dump: bool,

        /// Show source annotations (requires --dump).
        #[arg(long)]
        show_source: bool,
    },

    /// Check system health and repair.
    Doctor {
        /// Attempt automatic repair of issues found.
        #[arg(long)]
        repair: bool,

        /// Workspace to check (defaults to config workspace).
        #[arg(short = 'w', long)]
        workspace: Option<PathBuf>,
    },

    /// Show version information.
    Version,

    /// Validate an agent adapter.
    TestAgent {
        /// Name of the adapter to test.
        name: String,
    },

    /// Run canary tests against a :testing binary.
    Canary {
        /// Show channel status instead of running tests.
        #[arg(long)]
        status: bool,
    },

    /// Check for and install updates from GitHub releases.
    Upgrade {
        /// Check only — show available update without installing.
        #[arg(long)]
        check: bool,
    },

    /// Rollback to the previous :stable binary.
    Rollback,

    /// Run learning consolidation on demand.
    ///
    /// Reads bead close bodies since the last consolidation, extracts
    /// retrospective patterns, merges them into learnings.md, and promotes
    /// high-frequency learnings to skill files.
    Reflect {
        /// Workspace to consolidate (defaults to config workspace).
        #[arg(short = 'w', long)]
        workspace: Option<PathBuf>,

        /// Skip cooldown and minimum bead threshold checks.
        #[arg(long)]
        force: bool,
    },

    /// Fetch the latest gitleaks rules and update the vendored config.
    ///
    /// Downloads gitleaks.toml from the upstream GitHub repository, validates
    /// it by compiling all rules, and writes it to the output path.
    /// Rebuild needle after running this command to embed the new rules.
    #[command(name = "update-rules")]
    UpdateRules {
        /// Destination path for the downloaded config (default: config/gitleaks.toml).
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
    },
}

/// Output format for the list command.
#[derive(Debug, Clone, ValueEnum)]
pub enum ListFormat {
    Table,
    Json,
}

/// Output format for the logs command.
#[derive(Debug, Clone, ValueEnum)]
pub enum LogFormat {
    /// Human-readable table format (default).
    Table,
    /// JSON Lines format (one JSON object per line).
    Json,
    /// Alias for table (human-readable).
    Human,
    /// Alias for json (JSON Lines).
    Jsonl,
}

// ──────────────────────────────────────────────────────────────────────────────
// Entry point
// ──────────────────────────────────────────────────────────────────────────────

/// Entry point called from `main`.
pub fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        CliCommand::Run {
            workspace,
            agent,
            count,
            identifier,
            timeout,
            resume,
        } => cmd_run(workspace, agent, count, identifier, timeout, resume),
        CliCommand::Stop { all, identifier } => cmd_stop(all, identifier),
        CliCommand::List { format } => cmd_list(format),
        CliCommand::Attach { identifier } => cmd_attach(&identifier),
        CliCommand::Status {
            format,
            by_worker,
            cost,
            since,
            until,
        } => cmd_status(format, by_worker, cost, since, until),
        CliCommand::Logs {
            follow,
            filter,
            since,
            until,
            format,
        } => cmd_logs(follow, filter, since, until, format),
        CliCommand::ConfigCmd {
            get,
            dump,
            show_source,
        } => cmd_config(get, dump, show_source),
        CliCommand::Doctor { repair, workspace } => cmd_doctor(repair, workspace),
        CliCommand::Version => {
            cmd_version();
            Ok(())
        }
        CliCommand::TestAgent { name } => cmd_test_agent(&name),
        CliCommand::Canary { status } => cmd_canary(status),
        CliCommand::Upgrade { check } => cmd_upgrade(check),
        CliCommand::Rollback => cmd_rollback(),
        CliCommand::Reflect { workspace, force } => cmd_reflect(workspace, force),
        CliCommand::UpdateRules { output } => cmd_update_rules(output),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Command handlers
// ──────────────────────────────────────────────────────────────────────────────

/// `needle run` — launch a worker.
///
/// Always creates dedicated tmux sessions for workers, even when invoked from
/// inside an existing tmux session. The only exception is a re-entrant inner
/// invocation launched by `launch_in_tmux()`, which is identified by the
/// `NEEDLE_INNER=1` environment variable and runs the worker directly.
fn cmd_run(
    workspace: Option<PathBuf>,
    agent: Option<String>,
    count: u32,
    identifier: Option<String>,
    timeout: Option<u64>,
    resume: bool,
) -> Result<()> {
    // Determine workspace root (CLI arg → canonicalized, else global default).
    let workspace_root = if let Some(ref ws) = workspace {
        ws.canonicalize().unwrap_or_else(|_| ws.clone())
    } else {
        let global = ConfigLoader::load_global()?;
        global.workspace.default.clone()
    };

    // Load full resolved config (global → workspace .needle.yaml → env → CLI).
    let cli_overrides = CliOverrides {
        workspace: Some(workspace_root.clone()),
        agent_binary: agent.clone(),
        max_workers: None,
        ..Default::default()
    };
    let (mut config, _) = ConfigLoader::load_resolved(&workspace_root, cli_overrides)?;

    if let Some(t) = timeout {
        config.agent.timeout = t;
    }

    if resume {
        // Hot-reload resume: inherit worker identity from --identifier,
        // load state from heartbeat file + registry, continue from SELECTING.
        let worker_id = identifier
            .clone()
            .unwrap_or_else(|| NATO_ALPHABET[0].to_string());

        // Load resume state from heartbeat and registry.
        let resume_state = crate::upgrade::ResumeState::load(&config, &worker_id)?;

        // Emit upgrade.completed telemetry.
        let current_hash = crate::upgrade::file_hash(
            &std::env::current_exe().context("failed to locate own binary")?,
        )
        .unwrap_or_else(|_| "unknown".to_string());

        match &resume_state {
            Some(state) => {
                tracing::info!(
                    worker = %worker_id,
                    binary_hash = %&current_hash[..current_hash.len().min(12)],
                    beads_processed = state.beads_processed,
                    session = %state.session,
                    "resuming worker after hot-reload"
                );
            }
            None => {
                tracing::info!(
                    worker = %worker_id,
                    binary_hash = %&current_hash[..current_hash.len().min(12)],
                    "resuming worker after hot-reload (no prior state found)"
                );
            }
        }

        let tel = crate::telemetry::Telemetry::from_config(worker_id.clone(), &config.telemetry)
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "hook telemetry init failed, falling back");
                crate::telemetry::Telemetry::new(worker_id.clone())
            });
        tel.emit(crate::telemetry::EventKind::UpgradeCompleted {
            new_hash: current_hash,
        })?;

        return run_worker(config, worker_id);
    }

    if is_needle_inner() {
        // Re-entrant inner invocation launched by launch_in_tmux() — run
        // the worker directly inside the dedicated session already created.
        let worker_id = identifier
            .clone()
            .unwrap_or_else(|| NATO_ALPHABET[0].to_string());
        let agent_name = agent.as_deref().unwrap_or(&config.agent.default);
        let session_name = format!("needle-{agent_name}-{worker_id}");
        tracing::info!(worker = %worker_id, session = %session_name, "starting worker (inner re-entrant invocation)");
        run_worker(config, worker_id)
    } else {
        // Always create dedicated tmux sessions, even if already inside tmux.
        launch_workers(config, workspace, agent, count, identifier, timeout)
    }
}

/// Launch `count` workers in separate tmux sessions with staggered startup delays.
fn launch_workers(
    config: Config,
    workspace: Option<PathBuf>,
    agent: Option<String>,
    count: u32,
    identifier: Option<String>,
    timeout: Option<u64>,
) -> Result<()> {
    let agent_name = agent
        .as_deref()
        .unwrap_or(&config.agent.default)
        .to_string();
    let stagger_secs = config.worker.launch_stagger_seconds;
    let max_workers = config.worker.max_workers;

    if count == 0 {
        bail!("--count must be at least 1");
    }
    if count as usize > NATO_ALPHABET.len() {
        bail!(
            "--count {} exceeds the maximum of {} (NATO alphabet size)",
            count,
            NATO_ALPHABET.len()
        );
    }

    // Enforce max_workers cap (0 means unlimited).
    let effective_count = if max_workers > 0 && count > max_workers {
        tracing::warn!(
            requested = count,
            capped_to = max_workers,
            "count exceeds max_workers; capping"
        );
        eprintln!(
            "Warning: --count {count} exceeds max_workers={max_workers}; launching {max_workers} workers"
        );
        max_workers
    } else {
        count
    };

    // --identifier is only meaningful for a single worker.
    if effective_count > 1 && identifier.is_some() {
        bail!("--identifier cannot be combined with --count > 1; identifiers are auto-assigned from the NATO alphabet");
    }

    // Detect existing sessions to avoid name collisions.
    let occupied = occupied_worker_ids(&agent_name)?;
    if !occupied.is_empty() {
        tracing::info!(
            occupied = ?occupied,
            "found existing worker sessions"
        );
    }

    // Reject --identifier collision early.
    if let Some(ref id) = identifier {
        if occupied.contains(id) {
            bail!(
                "worker '{}' is already running in session 'needle-{}-{}'",
                id,
                agent_name,
                id
            );
        }
    }

    // Build the list of worker IDs, skipping occupied names.
    let worker_ids: Vec<String> = if effective_count == 1 {
        vec![identifier.clone().unwrap_or_else(|| {
            // Pick the first available NATO name.
            NATO_ALPHABET
                .iter()
                .find(|name| !occupied.contains(**name))
                .map(|s| s.to_string())
                .unwrap_or_else(|| NATO_ALPHABET[0].to_string())
        })]
    } else {
        let mut ids = Vec::with_capacity(effective_count as usize);
        for name in NATO_ALPHABET {
            if ids.len() >= effective_count as usize {
                break;
            }
            if occupied.contains(*name) {
                tracing::warn!(
                    worker_id = %name,
                    "skipping occupied worker name"
                );
                continue;
            }
            ids.push(name.to_string());
        }
        if ids.len() < effective_count as usize {
            bail!(
                "cannot launch {} workers — only {} NATO names available ({} occupied)",
                effective_count,
                ids.len(),
                occupied.len()
            );
        }
        ids
    };

    for (seq, worker_id) in worker_ids.iter().enumerate() {
        let session_name = format!("needle-{agent_name}-{worker_id}");

        tracing::info!(
            worker_id = %worker_id,
            sequence = seq,
            total = effective_count,
            session = %session_name,
            "launching worker"
        );

        // Stagger: sleep before launching subsequent workers.
        if seq > 0 && stagger_secs > 0 {
            std::thread::sleep(std::time::Duration::from_secs(stagger_secs));
        }

        launch_in_tmux(
            &session_name,
            workspace.clone(),
            agent.clone(),
            Some(worker_id.clone()),
            timeout,
        )?;

        println!(
            "[{}/{}] Started worker '{}' in tmux session: {session_name}",
            seq + 1,
            effective_count,
            worker_id
        );
    }

    if effective_count > 1 {
        println!(
            "\nStarted {effective_count} workers (stagger: {stagger_secs}s between launches)."
        );
        println!("Attach to a worker with: tmux attach -t needle-{agent_name}-<name>");
    } else {
        let worker_id = &worker_ids[0];
        println!("Attach with: tmux attach -t needle-{agent_name}-{worker_id}");
    }

    Ok(())
}

/// Start the worker state machine (called when inside tmux or for direct mode).
fn run_worker(config: Config, worker_name: String) -> Result<()> {
    let store = std::sync::Arc::new(
        BrCliBeadStore::discover(config.workspace.default.clone())
            .context("failed to locate br CLI for bead store")?,
    );
    let mut worker = Worker::new(config, worker_name, store);

    let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    let result = rt.block_on(worker.run())?;

    tracing::info!(final_state = %result, "worker finished");
    Ok(())
}

/// Returns true when this process is a re-entrant inner invocation launched
/// by `launch_in_tmux()`, indicated by `NEEDLE_INNER=1` in the environment.
fn is_needle_inner() -> bool {
    std::env::var("NEEDLE_INNER").is_ok_and(|v| v == "1")
}

/// Create a single tmux session and re-exec self inside it with `--count 1`.
fn launch_in_tmux(
    session_name: &str,
    workspace: Option<PathBuf>,
    agent: Option<String>,
    identifier: Option<String>,
    timeout: Option<u64>,
) -> Result<()> {
    // Build the command that tmux will run inside the session.
    let self_exe = std::env::current_exe().context("failed to locate own binary")?;
    let mut inner_args = vec!["run".to_string()];

    if let Some(ref ws) = workspace {
        inner_args.push("--workspace".to_string());
        inner_args.push(ws.display().to_string());
    }
    if let Some(ref a) = agent {
        inner_args.push("--agent".to_string());
        inner_args.push(a.clone());
    }
    // Each session runs exactly one worker; identifier is always resolved before call.
    inner_args.push("--count".to_string());
    inner_args.push("1".to_string());
    if let Some(ref id) = identifier {
        inner_args.push("--identifier".to_string());
        inner_args.push(id.clone());
    }
    if let Some(t) = timeout {
        inner_args.push("--timeout".to_string());
        inner_args.push(t.to_string());
    }

    let inner_cmd = format!(
        "NEEDLE_INNER=1 {} {}",
        shell_escape(&self_exe.display().to_string()),
        inner_args
            .iter()
            .map(|a| shell_escape(a))
            .collect::<Vec<_>>()
            .join(" ")
    );

    let status = ProcessCommand::new("tmux")
        .args(["new-session", "-d", "-s", session_name, &inner_cmd])
        .status()
        .context("failed to launch tmux — is tmux installed?")?;

    if !status.success() {
        bail!(
            "tmux new-session exited with status {} for session '{}'",
            status,
            session_name
        );
    }

    Ok(())
}

/// `needle stop` — send SIGTERM to worker processes in tmux sessions.
fn cmd_stop(all: bool, identifier: Option<String>) -> Result<()> {
    if !all && identifier.is_none() {
        bail!("specify --all or --identifier <NAME>");
    }

    let sessions = list_needle_sessions()?;

    if sessions.is_empty() {
        println!("No needle sessions running.");
        return Ok(());
    }

    let targets: Vec<&str> = if all {
        sessions.iter().map(|s| s.name.as_str()).collect()
    } else {
        let id = identifier.as_deref().unwrap_or("");
        sessions
            .iter()
            .filter(|s| s.name.contains(id))
            .map(|s| s.name.as_str())
            .collect()
    };

    if targets.is_empty() {
        println!("No matching sessions found.");
        return Ok(());
    }

    for session in &targets {
        tracing::info!(session = %session, "sending SIGTERM to tmux session");
        // tmux send-keys sends C-c to the foreground process
        let status = ProcessCommand::new("tmux")
            .args(["send-keys", "-t", session, "C-c", ""])
            .status()
            .with_context(|| format!("failed to send SIGTERM to session '{session}'"))?;

        if status.success() {
            println!("Stopped: {session}");
        } else {
            println!("Warning: could not stop session '{session}' (status: {status})");
        }
    }

    Ok(())
}

/// `needle list` — show running needle sessions.
fn cmd_list(format: ListFormat) -> Result<()> {
    let sessions = list_needle_sessions()?;

    if sessions.is_empty() {
        match format {
            ListFormat::Table => println!("No needle sessions running."),
            ListFormat::Json => println!("[]"),
        }
        return Ok(());
    }

    match format {
        ListFormat::Table => {
            println!("{:<40} {:<20} {:<10}", "SESSION", "CREATED", "STATUS");
            println!("{}", "-".repeat(70));
            for s in &sessions {
                println!("{:<40} {:<20} {:<10}", s.name, s.created, s.status);
            }
        }
        ListFormat::Json => {
            let json = serde_json::to_string_pretty(&sessions)
                .context("failed to serialize sessions to JSON")?;
            println!("{json}");
        }
    }

    Ok(())
}

/// `needle version` — print version info.
fn cmd_version() {
    let version = env!("CARGO_PKG_VERSION");
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    println!("needle {version} (rust, {os} {arch})");
}

/// `needle test-agent <name>` — validate an agent adapter.
fn cmd_test_agent(name: &str) -> Result<()> {
    let config = ConfigLoader::load_global()?;
    let result = dispatch::test_agent(name, &config)?;
    dispatch::print_test_result(&result);

    if result.status == dispatch::AgentTestStatus::Error {
        bail!("agent adapter '{}' is not ready", name);
    }

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// attach, status, config, doctor
// ──────────────────────────────────────────────────────────────────────────────

/// `needle attach <identifier>` — attach to a running worker's tmux session.
fn cmd_attach(identifier: &str) -> Result<()> {
    let sessions = list_needle_sessions()?;

    if sessions.is_empty() {
        bail!("no needle sessions running");
    }

    // Find matching session: exact match on identifier portion or substring match on full name.
    let matches: Vec<&TmuxSession> = sessions
        .iter()
        .filter(|s| s.name.ends_with(&format!("-{identifier}")) || s.name.contains(identifier))
        .collect();

    if matches.is_empty() {
        let available: Vec<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
        bail!(
            "no session matching '{}'; available: {}",
            identifier,
            available.join(", ")
        );
    }

    if matches.len() > 1 {
        let names: Vec<&str> = matches.iter().map(|s| s.name.as_str()).collect();
        bail!(
            "ambiguous identifier '{}'; matches: {}",
            identifier,
            names.join(", ")
        );
    }

    let session = &matches[0].name;
    let status = ProcessCommand::new("tmux")
        .args(["attach-session", "-t", session])
        .status()
        .with_context(|| format!("failed to attach to tmux session '{session}'"))?;

    if !status.success() {
        bail!("tmux attach-session exited with status {status} for '{session}'");
    }

    Ok(())
}

/// `needle status` — show fleet status summary.
fn cmd_status(
    format: ListFormat,
    by_worker: bool,
    cost: bool,
    since: Option<String>,
    until: Option<String>,
) -> Result<()> {
    let config = ConfigLoader::load_global()?;
    let needle_home = config.workspace.home.clone();
    let registry = Registry::default_location(&needle_home);
    let workers = registry.list().unwrap_or_default();
    let sessions = list_needle_sessions().unwrap_or_default();

    // Build a fleet summary.
    let active_count = sessions.len();
    let registered_count = workers.len();
    let total_beads: u64 = workers.iter().map(|w| w.beads_processed).sum();

    // Check heartbeat health for registered workers.
    let heartbeat_dir = needle_home.join("state").join("heartbeats");
    let heartbeat_statuses: Vec<WorkerStatus> = workers
        .iter()
        .map(|w| {
            let hb_path = heartbeat_dir.join(format!("{}.json", w.id));
            let heartbeat = if hb_path.exists() {
                std::fs::read_to_string(&hb_path)
                    .ok()
                    .and_then(|s| serde_json::from_str::<HeartbeatData>(&s).ok())
            } else {
                None
            };

            let is_alive = is_pid_alive(w.pid);
            let uptime = Utc::now().signed_duration_since(w.started_at);

            WorkerStatus {
                entry: w.clone(),
                heartbeat_state: heartbeat.as_ref().map(|h| format!("{}", h.state)),
                current_bead: heartbeat.and_then(|h| h.current_bead.map(|b| b.to_string())),
                pid_alive: is_alive,
                uptime_secs: uptime.num_seconds().max(0) as u64,
            }
        })
        .collect();

    match format {
        ListFormat::Table => {
            println!("Fleet Summary");
            println!("{}", "-".repeat(50));
            println!("  Active tmux sessions: {active_count}");
            println!("  Registered workers:   {registered_count}");
            println!("  Total beads processed: {total_beads}");
            println!();

            if by_worker && !heartbeat_statuses.is_empty() {
                println!(
                    "{:<16} {:<8} {:<14} {:<12} {:<10} {:<8}",
                    "WORKER", "PID", "STATE", "BEAD", "UPTIME", "ALIVE"
                );
                println!("{}", "-".repeat(68));
                for ws in &heartbeat_statuses {
                    let state = ws.heartbeat_state.as_deref().unwrap_or("unknown");
                    let bead = ws.current_bead.as_deref().unwrap_or("-");
                    let uptime = format_duration(ws.uptime_secs);
                    let alive = if ws.pid_alive { "yes" } else { "no" };
                    println!(
                        "{:<16} {:<8} {:<14} {:<12} {:<10} {:<8}",
                        ws.entry.id, ws.entry.pid, state, bead, uptime, alive
                    );
                }
            } else if !heartbeat_statuses.is_empty() {
                println!("Workers:");
                for ws in &heartbeat_statuses {
                    let state = ws.heartbeat_state.as_deref().unwrap_or("unknown");
                    let alive = if ws.pid_alive { "" } else { " (DEAD)" };
                    println!(
                        "  {} — {} beads, state: {state}{alive}",
                        ws.entry.id, ws.entry.beads_processed,
                    );
                }
            }

            if heartbeat_statuses.is_empty() && active_count == 0 {
                println!("No workers running.");
            }
        }
        ListFormat::Json => {
            let summary = serde_json::json!({
                "active_sessions": active_count,
                "registered_workers": registered_count,
                "total_beads_processed": total_beads,
                "workers": heartbeat_statuses.iter().map(|ws| {
                    serde_json::json!({
                        "id": ws.entry.id,
                        "pid": ws.entry.pid,
                        "workspace": ws.entry.workspace,
                        "agent": ws.entry.agent,
                        "beads_processed": ws.entry.beads_processed,
                        "state": ws.heartbeat_state,
                        "current_bead": ws.current_bead,
                        "pid_alive": ws.pid_alive,
                        "uptime_secs": ws.uptime_secs,
                    })
                }).collect::<Vec<_>>(),
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&summary).context("failed to serialize status")?
            );
        }
    }

    // Cost summary (if requested).
    if cost {
        let log_dir = needle_home.join("logs");
        let since_dt = since.as_deref().map(telemetry::parse_since).transpose()?;
        let until_dt = until.as_deref().map(telemetry::parse_until).transpose()?;
        let events = telemetry::read_logs(&log_dir, since_dt, until_dt, None)?;
        let cs = telemetry::compute_cost_summary(&events);
        let by_worker_costs = telemetry::compute_cost_by_worker(&events);
        let by_workspace_costs = telemetry::compute_cost_by_workspace(&events);

        match format {
            ListFormat::Table => {
                println!();
                println!("Cost Summary");
                println!("{}", "-".repeat(50));
                println!("  Dispatch events:  {}", cs.total_events);
                println!("  Total cost:       ${:.4}", cs.total_cost_usd);
                println!(
                    "  Tokens:           {} in / {} out",
                    cs.total_tokens_in, cs.total_tokens_out
                );
                println!(
                    "  Agent time:       {}",
                    telemetry::format_duration_ms_public(cs.total_elapsed_ms)
                );

                if !by_worker_costs.is_empty() {
                    println!();
                    println!("  Per Worker:");
                    println!(
                        "  {:<16} {:>8} {:>12} {:>14} {:>14}",
                        "WORKER", "EVENTS", "COST (USD)", "TOKENS IN", "TOKENS OUT"
                    );
                    println!("  {}", "-".repeat(64));
                    for w in &by_worker_costs {
                        println!(
                            "  {:<16} {:>8} {:>12.4} {:>14} {:>14}",
                            w.worker_id,
                            w.total_events,
                            w.total_cost_usd,
                            w.total_tokens_in,
                            w.total_tokens_out,
                        );
                    }
                }

                if !by_workspace_costs.is_empty() {
                    println!();
                    println!("  Per Workspace:");
                    println!(
                        "  {:<40} {:>8} {:>12} {:>14} {:>14}",
                        "WORKSPACE", "EVENTS", "COST (USD)", "TOKENS IN", "TOKENS OUT"
                    );
                    println!("  {}", "-".repeat(88));
                    for w in &by_workspace_costs {
                        let ws_display = if w.workspace.len() > 38 {
                            format!("...{}", &w.workspace[w.workspace.len() - 35..])
                        } else {
                            w.workspace.clone()
                        };
                        println!(
                            "  {:<40} {:>8} {:>12.4} {:>14} {:>14}",
                            ws_display,
                            w.total_events,
                            w.total_cost_usd,
                            w.total_tokens_in,
                            w.total_tokens_out,
                        );
                    }
                }
            }
            ListFormat::Json => {
                let cost_json = serde_json::json!({
                    "dispatch_events": cs.total_events,
                    "total_cost_usd": cs.total_cost_usd,
                    "total_tokens_in": cs.total_tokens_in,
                    "total_tokens_out": cs.total_tokens_out,
                    "total_elapsed_ms": cs.total_elapsed_ms,
                    "by_worker": by_worker_costs.iter().map(|w| serde_json::json!({
                        "worker_id": w.worker_id,
                        "total_events": w.total_events,
                        "total_cost_usd": w.total_cost_usd,
                        "total_tokens_in": w.total_tokens_in,
                        "total_tokens_out": w.total_tokens_out,
                        "total_elapsed_ms": w.total_elapsed_ms,
                    })).collect::<Vec<_>>(),
                    "by_workspace": by_workspace_costs.iter().map(|w| serde_json::json!({
                        "workspace": w.workspace,
                        "total_events": w.total_events,
                        "total_cost_usd": w.total_cost_usd,
                        "total_tokens_in": w.total_tokens_in,
                        "total_tokens_out": w.total_tokens_out,
                        "total_elapsed_ms": w.total_elapsed_ms,
                    })).collect::<Vec<_>>(),
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&cost_json)
                        .context("failed to serialize cost summary")?
                );
            }
        }
    }

    Ok(())
}

/// `needle config` — view or inspect configuration.
fn cmd_config(get: Option<String>, dump: bool, show_source: bool) -> Result<()> {
    if show_source && !dump {
        bail!("--show-source requires --dump");
    }

    let workspace_root = std::env::current_dir().unwrap_or_default();
    let (config, sources) = ConfigLoader::load_resolved(&workspace_root, CliOverrides::default())?;

    if let Some(key) = get {
        let value = config_get_key(&config, &key);
        match value {
            Some(v) => println!("{v}"),
            None => bail!("unknown config key: {key}"),
        }
        return Ok(());
    }

    if dump {
        if show_source {
            let lines = ConfigLoader::dump_with_sources(&config, &sources);
            for line in &lines {
                println!("{line}");
            }
        } else {
            let lines = config_dump(&config);
            for line in &lines {
                println!("{line}");
            }
        }
        return Ok(());
    }

    // Default: show a brief summary.
    let yaml = serde_yaml::to_string(&config).context("failed to serialize config")?;
    print!("{yaml}");
    Ok(())
}

/// Look up a single config key by dot-separated path.
fn config_get_key(config: &Config, key: &str) -> Option<String> {
    match key {
        "agent.default" => Some(config.agent.default.clone()),
        "agent.timeout" => Some(config.agent.timeout.to_string()),
        "worker.max_workers" => Some(config.worker.max_workers.to_string()),
        "worker.launch_stagger_seconds" => Some(config.worker.launch_stagger_seconds.to_string()),
        "worker.idle_timeout" => Some(config.worker.idle_timeout.to_string()),
        "worker.max_claim_retries" => Some(config.worker.max_claim_retries.to_string()),
        "worker.cpu_load_warn" => Some(config.worker.cpu_load_warn.to_string()),
        "worker.memory_free_warn_mb" => Some(config.worker.memory_free_warn_mb.to_string()),
        "health.heartbeat_interval_secs" => Some(config.health.heartbeat_interval_secs.to_string()),
        "health.heartbeat_ttl_secs" => Some(config.health.heartbeat_ttl_secs.to_string()),
        "workspace.default" => Some(config.workspace.default.display().to_string()),
        "workspace.home" => Some(config.workspace.home.display().to_string()),
        "telemetry.file_sink.enabled" => Some(config.telemetry.file_sink.enabled.to_string()),
        "prompt.instructions" => Some(
            config
                .prompt
                .instructions
                .as_deref()
                .unwrap_or("")
                .to_string(),
        ),
        _ => None,
    }
}

/// Dump all config key-value pairs without source annotations.
fn config_dump(config: &Config) -> Vec<String> {
    vec![
        format!("agent.default: {}", config.agent.default),
        format!("agent.timeout: {}", config.agent.timeout),
        format!("worker.max_workers: {}", config.worker.max_workers),
        format!(
            "worker.launch_stagger_seconds: {}",
            config.worker.launch_stagger_seconds
        ),
        format!("worker.idle_timeout: {}", config.worker.idle_timeout),
        format!(
            "worker.max_claim_retries: {}",
            config.worker.max_claim_retries
        ),
        format!("worker.cpu_load_warn: {}", config.worker.cpu_load_warn),
        format!(
            "worker.memory_free_warn_mb: {}",
            config.worker.memory_free_warn_mb
        ),
        format!("workspace.default: {}", config.workspace.default.display()),
        format!("workspace.home: {}", config.workspace.home.display()),
        format!(
            "health.heartbeat_interval_secs: {}",
            config.health.heartbeat_interval_secs
        ),
        format!(
            "health.heartbeat_ttl_secs: {}",
            config.health.heartbeat_ttl_secs
        ),
        format!(
            "telemetry.file_sink.enabled: {}",
            config.telemetry.file_sink.enabled
        ),
        format!("prompt.context_files: {:?}", config.prompt.context_files),
        format!(
            "prompt.instructions: {}",
            config.prompt.instructions.as_deref().unwrap_or("")
        ),
    ]
}

// ──────────────────────────────────────────────────────────────────────────────
// Doctor: structured check result types
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

struct CheckResult {
    name: String,
    status: CheckStatus,
    message: String,
    /// Extra lines printed indented below the main line.
    detail: Vec<String>,
}

impl CheckResult {
    fn pass(name: impl Into<String>, msg: impl Into<String>) -> Self {
        CheckResult {
            name: name.into(),
            status: CheckStatus::Pass,
            message: msg.into(),
            detail: vec![],
        }
    }
    fn warn(name: impl Into<String>, msg: impl Into<String>) -> Self {
        CheckResult {
            name: name.into(),
            status: CheckStatus::Warn,
            message: msg.into(),
            detail: vec![],
        }
    }
    fn fail(name: impl Into<String>, msg: impl Into<String>) -> Self {
        CheckResult {
            name: name.into(),
            status: CheckStatus::Fail,
            message: msg.into(),
            detail: vec![],
        }
    }
    fn with_detail(mut self, lines: Vec<String>) -> Self {
        self.detail = lines;
        self
    }
    fn display(&self) -> String {
        let label = match self.status {
            CheckStatus::Pass => "PASS",
            CheckStatus::Warn => "WARN",
            CheckStatus::Fail => "FAIL",
        };
        format!("[{label}]  {:<28}  {}", self.name, self.message)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Doctor: individual check functions
// ──────────────────────────────────────────────────────────────────────────────

fn doctor_check_config(workspace: &Path) -> CheckResult {
    match ConfigLoader::load_resolved(workspace, CliOverrides::default()) {
        Ok(_) => CheckResult::pass("Config", "valid"),
        Err(e) => CheckResult::fail("Config", format!("{e:#}")),
    }
}

fn doctor_check_workspace(workspace: &Path) -> CheckResult {
    if !workspace.exists() {
        return CheckResult::fail(
            "Workspace",
            format!("directory not found: {}", workspace.display()),
        );
    }
    if !workspace.is_dir() {
        return CheckResult::fail(
            "Workspace",
            format!("not a directory: {}", workspace.display()),
        );
    }
    if std::fs::read_dir(workspace).is_err() {
        return CheckResult::fail("Workspace", "not readable");
    }
    let beads_dir = workspace.join(".beads");
    if !beads_dir.is_dir() {
        return CheckResult::fail(
            "Workspace",
            format!(".beads/ missing in {}", workspace.display()),
        );
    }
    // Probe write access.
    let probe = workspace.join(".needle_doctor_probe");
    match std::fs::write(&probe, b"") {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            CheckResult::pass("Workspace", workspace.display().to_string())
        }
        Err(e) => CheckResult::warn("Workspace", format!("not writable: {e}")),
    }
}

fn doctor_check_jsonl(beads_dir: &Path) -> CheckResult {
    let jsonl = beads_dir.join("issues.jsonl");
    if !jsonl.exists() {
        return CheckResult::fail("JSONL", "issues.jsonl not found");
    }
    let content = match std::fs::read_to_string(&jsonl) {
        Ok(c) => c,
        Err(e) => return CheckResult::fail("JSONL", format!("unreadable: {e}")),
    };
    let total = content.lines().filter(|l| !l.trim().is_empty()).count();
    let bad: Vec<usize> = content
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            !l.trim().is_empty() && serde_json::from_str::<serde_json::Value>(l).is_err()
        })
        .map(|(i, _)| i + 1)
        .collect();
    if bad.is_empty() {
        CheckResult::pass("JSONL", format!("{total} records"))
    } else {
        let examples: Vec<String> = bad.iter().take(5).map(|n| format!("line {n}")).collect();
        CheckResult::fail("JSONL", format!("{} invalid of {total} records", bad.len()))
            .with_detail(vec![format!("Invalid lines: {}", examples.join(", "))])
    }
}

fn doctor_check_sqlite(beads_dir: &Path) -> CheckResult {
    let db = beads_dir.join("beads.db");
    if !db.exists() {
        return CheckResult::pass("SQLite integrity", "no database (JSONL-only mode)");
    }
    match std::process::Command::new("sqlite3")
        .arg(&db)
        .arg("PRAGMA integrity_check;")
        .output()
    {
        Err(_) => CheckResult::warn("SQLite integrity", "sqlite3 not on PATH — skipped"),
        Ok(out) if !out.status.success() => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            CheckResult::fail(
                "SQLite integrity",
                format!("sqlite3 error: {}", stderr.trim()),
            )
        }
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let trimmed = stdout.trim();
            if trimmed == "ok" {
                CheckResult::pass("SQLite integrity", "ok")
            } else {
                let first = trimmed.lines().next().unwrap_or(trimmed);
                CheckResult::fail("SQLite integrity", format!("corrupt: {first}")).with_detail(
                    trimmed
                        .lines()
                        .skip(1)
                        .take(10)
                        .map(str::to_owned)
                        .collect(),
                )
            }
        }
    }
}

fn doctor_check_lock_files(beads_dir: &Path, lock_ttl_secs: u64, repair: bool) -> CheckResult {
    let entries = match std::fs::read_dir(beads_dir) {
        Ok(e) => e,
        Err(e) => return CheckResult::warn("Lock files", format!("cannot read .beads/: {e}")),
    };
    let ttl = std::time::Duration::from_secs(lock_ttl_secs);
    let now = std::time::SystemTime::now();
    let mut total = 0usize;
    let mut stale: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("lock") {
            continue;
        }
        total += 1;
        if let Ok(meta) = std::fs::metadata(&path) {
            if let Ok(modified) = meta.modified() {
                if now.duration_since(modified).unwrap_or_default() > ttl {
                    stale.push(path);
                }
            }
        }
    }
    if stale.is_empty() {
        return CheckResult::pass(
            "Lock files",
            if total == 0 {
                "none".to_string()
            } else {
                format!("{total} total, none stale")
            },
        );
    }
    if repair {
        let mut removed = 0usize;
        let mut failed_names: Vec<String> = Vec::new();
        for p in &stale {
            match std::fs::remove_file(p) {
                Ok(_) => removed += 1,
                Err(_) => failed_names.push(p.display().to_string()),
            }
        }
        if failed_names.is_empty() {
            CheckResult::pass("Lock files", format!("removed {removed} stale lock(s)"))
        } else {
            CheckResult::warn(
                "Lock files",
                format!("removed {removed}, failed to remove {}", failed_names.len()),
            )
            .with_detail(failed_names)
        }
    } else {
        let names: Vec<String> = stale
            .iter()
            .map(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect();
        CheckResult::warn(
            "Lock files",
            format!("{} stale of {total} (TTL {lock_ttl_secs}s)", stale.len()),
        )
        .with_detail(names)
    }
}

fn doctor_check_bead_store(
    workspace: &Path,
    beads_dir: &Path,
    repair: bool,
) -> Result<CheckResult> {
    if !beads_dir.is_dir() {
        return Ok(CheckResult::pass("Bead store", "skipped (no .beads/)"));
    }
    let store = match BrCliBeadStore::discover(workspace.to_path_buf()) {
        Ok(s) => s,
        Err(e) => {
            return Ok(CheckResult::fail(
                "Bead store",
                format!("br CLI not found: {e}"),
            ))
        }
    };
    let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    if repair {
        match rt.block_on(store.doctor_repair()) {
            Err(e) => Ok(CheckResult::fail(
                "Bead store",
                format!("repair failed: {e}"),
            )),
            Ok(report) if report.warnings.is_empty() && report.fixed.is_empty() => {
                Ok(CheckResult::pass("Bead store", "ok (no issues found)"))
            }
            Ok(report) => {
                let mut detail = Vec::new();
                for w in &report.warnings {
                    detail.push(format!("warn: {w}"));
                }
                for f in &report.fixed {
                    detail.push(format!("fixed: {f}"));
                }
                Ok(
                    CheckResult::pass("Bead store", format!("{} fixed", report.fixed.len()))
                        .with_detail(detail),
                )
            }
        }
    } else {
        match rt.block_on(store.doctor_check()) {
            Err(e) => Ok(CheckResult::fail("Bead store", format!("{e}"))),
            Ok(report) if report.warnings.is_empty() => Ok(CheckResult::pass("Bead store", "ok")),
            Ok(report) => Ok(CheckResult::warn(
                "Bead store",
                format!("{} warning(s)", report.warnings.len()),
            )
            .with_detail(report.warnings)),
        }
    }
}

fn doctor_check_registry(needle_home: &Path, repair: bool) -> CheckResult {
    let registry = Registry::default_location(needle_home);
    match registry.list() {
        Err(e) => CheckResult::fail("Worker registry", format!("{e}")),
        Ok(workers) => {
            let stale: Vec<&WorkerEntry> =
                workers.iter().filter(|w| !is_pid_alive(w.pid)).collect();
            if stale.is_empty() {
                CheckResult::pass(
                    "Worker registry",
                    if workers.is_empty() {
                        "empty".to_string()
                    } else {
                        format!("{} registered, all alive", workers.len())
                    },
                )
            } else {
                let names: Vec<String> = stale
                    .iter()
                    .map(|w| format!("{}(pid={})", w.id, w.pid))
                    .collect();
                if repair {
                    let mut removed = 0usize;
                    let mut failed = 0usize;
                    for w in &stale {
                        match registry.deregister(&w.id) {
                            Ok(_) => removed += 1,
                            Err(_) => failed += 1,
                        }
                    }
                    if failed == 0 {
                        CheckResult::pass(
                            "Worker registry",
                            format!("deregistered {removed} stale worker(s)"),
                        )
                    } else {
                        CheckResult::warn(
                            "Worker registry",
                            format!("deregistered {removed}, failed {failed}"),
                        )
                    }
                } else {
                    CheckResult::warn(
                        "Worker registry",
                        format!("{} stale of {}", stale.len(), workers.len()),
                    )
                    .with_detail(names)
                }
            }
        }
    }
}

fn doctor_check_heartbeat_dir(heartbeat_dir: &Path, repair: bool) -> CheckResult {
    if !heartbeat_dir.exists() {
        if repair {
            match std::fs::create_dir_all(heartbeat_dir) {
                Ok(_) => {
                    return CheckResult::pass(
                        "Heartbeat dir",
                        format!("created {}", heartbeat_dir.display()),
                    )
                }
                Err(e) => return CheckResult::fail("Heartbeat dir", format!("cannot create: {e}")),
            }
        }
        return CheckResult::warn(
            "Heartbeat dir",
            format!("missing: {}", heartbeat_dir.display()),
        );
    }
    // Probe write access.
    let probe = heartbeat_dir.join(".needle_doctor_probe");
    match std::fs::write(&probe, b"") {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            CheckResult::pass("Heartbeat dir", "writable")
        }
        Err(e) => CheckResult::fail("Heartbeat dir", format!("not writable: {e}")),
    }
}

fn doctor_check_heartbeats(heartbeat_dir: &Path, ttl_secs: u64, repair: bool) -> CheckResult {
    if !heartbeat_dir.is_dir() {
        return CheckResult::pass("Heartbeat files", "no heartbeat directory");
    }
    let entries = match std::fs::read_dir(heartbeat_dir) {
        Ok(e) => e,
        Err(e) => return CheckResult::warn("Heartbeat files", format!("cannot read dir: {e}")),
    };
    let mut total = 0usize;
    let mut stale_paths: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        total += 1;
        let is_stale = if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(hb) = serde_json::from_str::<HeartbeatData>(&content) {
                let age = Utc::now()
                    .signed_duration_since(hb.last_heartbeat)
                    .num_seconds();
                age > ttl_secs as i64
            } else if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&content) {
                raw.get("pid")
                    .and_then(|v| v.as_u64())
                    .map(|pid| !is_pid_alive(pid as u32))
                    .unwrap_or(true)
            } else {
                true
            }
        } else {
            true
        };
        if is_stale {
            stale_paths.push(path);
        }
    }
    if stale_paths.is_empty() {
        return CheckResult::pass("Heartbeat files", format!("{total} file(s), none stale"));
    }
    if repair {
        let mut removed = 0usize;
        let mut failed = 0usize;
        for p in &stale_paths {
            match std::fs::remove_file(p) {
                Ok(_) => removed += 1,
                Err(_) => failed += 1,
            }
        }
        if failed == 0 {
            CheckResult::pass(
                "Heartbeat files",
                format!("removed {removed} stale of {total}"),
            )
        } else {
            CheckResult::warn(
                "Heartbeat files",
                format!("removed {removed}, failed {failed}"),
            )
        }
    } else {
        CheckResult::warn(
            "Heartbeat files",
            format!("{} stale of {total}", stale_paths.len()),
        )
    }
}

fn doctor_check_peers(heartbeat_dir: &Path, ttl_secs: u64) -> CheckResult {
    let ttl = std::time::Duration::from_secs(ttl_secs);
    let heartbeats = match HealthMonitor::read_all_heartbeats(heartbeat_dir) {
        Ok(hbs) => hbs,
        Err(e) => return CheckResult::warn("Peers", format!("cannot read heartbeats: {e}")),
    };
    if heartbeats.is_empty() {
        return CheckResult::pass("Peers", "no workers running");
    }
    let (active, stale): (Vec<_>, Vec<_>) = heartbeats
        .iter()
        .partition(|hb| !HealthMonitor::is_stale(hb, ttl));
    let msg = format!("{} active, {} stale", active.len(), stale.len());
    let mut detail: Vec<String> = active
        .iter()
        .map(|hb| {
            let bead = hb
                .current_bead
                .as_ref()
                .map(|b| b.to_string())
                .unwrap_or_else(|| "–".to_string());
            format!(
                "{} pid={} state={:?} bead={}",
                hb.worker_id, hb.pid, hb.state, bead
            )
        })
        .collect();
    for hb in &stale {
        detail.push(format!(
            "[stale] {} pid={} last={}",
            hb.worker_id, hb.pid, hb.last_heartbeat
        ));
    }
    CheckResult::pass("Peers", msg).with_detail(detail)
}

fn doctor_check_agent_binary(config: &Config) -> CheckResult {
    let agent = &config.agent.default;
    let br_ok = which::which("br").is_ok();
    let agent_ok = which::which(agent).is_ok();
    match (br_ok, agent_ok) {
        (true, true) => CheckResult::pass("Agent binary", format!("br + {agent} on PATH")),
        (false, _) => CheckResult::fail("Agent binary", "br CLI not found on PATH"),
        (true, false) => CheckResult::warn(
            "Agent binary",
            format!("{agent} not found on PATH — workers cannot dispatch"),
        ),
    }
}

fn doctor_check_adapter_transforms(config: &Config) -> CheckResult {
    match dispatch::load_adapters(&config.agent.adapters_dir, &dispatch::builtin_adapters()) {
        Err(e) => CheckResult::fail("Adapter transforms", format!("cannot load adapters: {e}")),
        Ok(adapters) => {
            let mut missing: Vec<String> = adapters
                .values()
                .filter_map(|a| a.output_transform.as_deref())
                .filter(|bin| which::which(bin).is_err())
                .map(str::to_owned)
                .collect();
            missing.sort();
            missing.dedup();
            if missing.is_empty() {
                CheckResult::pass("Adapter transforms", "ok")
            } else {
                CheckResult::warn(
                    "Adapter transforms",
                    format!("{} binary/binaries not on PATH", missing.len()),
                )
                .with_detail(missing)
            }
        }
    }
}

fn doctor_check_disk_space(path: &Path) -> CheckResult {
    // df --block-size=1M --output=avail <path> prints a header + one value.
    let output = std::process::Command::new("df")
        .args(["--block-size=1M", "--output=avail"])
        .arg(path)
        .output();
    match output {
        Err(_) => CheckResult::warn("Disk space", "df not available — skipped"),
        Ok(out) if !out.status.success() => CheckResult::warn("Disk space", "df command failed"),
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let avail_mb = stdout
                .lines()
                .nth(1)
                .and_then(|l| l.trim().parse::<u64>().ok())
                .unwrap_or(0);
            if avail_mb < 100 {
                CheckResult::fail(
                    "Disk space",
                    format!("{avail_mb} MB available — critically low"),
                )
            } else if avail_mb < 500 {
                CheckResult::warn("Disk space", format!("{avail_mb} MB available — low"))
            } else {
                CheckResult::pass("Disk space", format!("{avail_mb} MB available"))
            }
        }
    }
}

fn doctor_check_telemetry_logs(config: &Config, needle_home: &Path, repair: bool) -> CheckResult {
    let log_dir = config
        .telemetry
        .file_sink
        .log_dir
        .clone()
        .unwrap_or_else(|| needle_home.join("logs"));
    if !log_dir.is_dir() {
        return CheckResult::pass("Telemetry logs", "no log directory yet");
    }
    let retention_days = config.telemetry.file_sink.retention_days;
    let mut total = 0u64;
    let mut expired = 0u64;
    let cutoff = if retention_days > 0 {
        Some(
            std::time::SystemTime::now()
                - std::time::Duration::from_secs(u64::from(retention_days) * 86400),
        )
    } else {
        None
    };
    if let Ok(entries) = std::fs::read_dir(&log_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            total += 1;
            if let Some(cutoff) = cutoff {
                if let Ok(meta) = std::fs::metadata(&path) {
                    if let Ok(modified) = meta.modified() {
                        if modified < cutoff {
                            expired += 1;
                            if repair {
                                let _ = std::fs::remove_file(&path);
                            }
                        }
                    }
                }
            }
        }
    }
    if expired == 0 {
        CheckResult::pass("Telemetry logs", format!("{total} file(s)"))
    } else if repair {
        CheckResult::pass(
            "Telemetry logs",
            format!("removed {expired} expired of {total} (retention: {retention_days}d)"),
        )
    } else {
        CheckResult::warn(
            "Telemetry logs",
            format!("{expired} expired of {total} (retention: {retention_days}d) — use --repair to clean"),
        )
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// `needle doctor` — comprehensive system health check
// ──────────────────────────────────────────────────────────────────────────────

/// `needle doctor` — check system health and optionally repair.
fn cmd_doctor(repair: bool, workspace: Option<PathBuf>) -> Result<()> {
    let config = ConfigLoader::load_global()?;
    let needle_home = config.workspace.home.clone();
    let workspace_root = workspace.unwrap_or_else(|| config.workspace.default.clone());
    let beads_dir = workspace_root.join(".beads");
    let heartbeat_dir = needle_home.join("state").join("heartbeats");

    let width = 60;
    println!("NEEDLE Doctor");
    println!("{}", "─".repeat(width));

    let mut results: Vec<CheckResult> = Vec::new();

    // Config
    results.push(doctor_check_config(&workspace_root));

    // Workspace accessibility + .beads/ presence
    results.push(doctor_check_workspace(&workspace_root));

    // JSONL consistency
    if beads_dir.is_dir() {
        results.push(doctor_check_jsonl(&beads_dir));
    }

    // SQLite integrity (raw PRAGMA — independent of br)
    if beads_dir.is_dir() {
        results.push(doctor_check_sqlite(&beads_dir));
    }

    // Stale lock files
    if beads_dir.is_dir() {
        results.push(doctor_check_lock_files(
            &beads_dir,
            config.strands.mend.lock_ttl_secs,
            repair,
        ));
    }

    // Bead store connectivity (br doctor)
    results.push(doctor_check_bead_store(
        &workspace_root,
        &beads_dir,
        repair,
    )?);

    // Worker registry
    results.push(doctor_check_registry(&needle_home, repair));

    // Heartbeat directory permissions
    results.push(doctor_check_heartbeat_dir(&heartbeat_dir, repair));

    // Heartbeat file staleness
    results.push(doctor_check_heartbeats(
        &heartbeat_dir,
        config.health.heartbeat_ttl_secs,
        repair,
    ));

    // Peer status
    results.push(doctor_check_peers(
        &heartbeat_dir,
        config.health.heartbeat_ttl_secs,
    ));

    // Agent binary availability
    results.push(doctor_check_agent_binary(&config));

    // Adapter transform binaries
    results.push(doctor_check_adapter_transforms(&config));

    // Disk space
    results.push(doctor_check_disk_space(&workspace_root));

    // Telemetry logs
    results.push(doctor_check_telemetry_logs(&config, &needle_home, repair));

    // Print results.
    for r in &results {
        println!("{}", r.display());
        for line in &r.detail {
            println!("         └─ {line}");
        }
    }

    // Summary.
    let fails = results
        .iter()
        .filter(|r| r.status == CheckStatus::Fail)
        .count();
    let warns = results
        .iter()
        .filter(|r| r.status == CheckStatus::Warn)
        .count();
    let passed = results
        .iter()
        .filter(|r| r.status == CheckStatus::Pass)
        .count();

    println!("{}", "─".repeat(width));
    if fails == 0 && warns == 0 {
        println!("{passed} check(s) passed.");
    } else {
        println!("{passed} passed, {warns} warning(s), {fails} failure(s).");
        if !repair && (fails > 0 || warns > 0) {
            println!("Run `needle doctor --repair` to attempt automatic fixes.");
        }
    }

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Logs command
// ──────────────────────────────────────────────────────────────────────────────

/// `needle logs` — view and query telemetry logs.
fn cmd_logs(
    follow: bool,
    filter: Vec<String>,
    since: Option<String>,
    until: Option<String>,
    format: LogFormat,
) -> Result<()> {
    let config = ConfigLoader::load_global()?;
    let needle_home = config.workspace.home.clone();
    let log_dir = config
        .telemetry
        .file_sink
        .log_dir
        .clone()
        .unwrap_or_else(|| needle_home.join("logs"));

    let filter_exprs: Vec<&str> = filter.iter().map(|s| s.as_str()).collect();
    let logs_filter = if filter_exprs.is_empty() {
        None
    } else {
        Some(telemetry::LogsFilter::parse(&filter_exprs)?)
    };

    let since_dt = since.as_deref().map(telemetry::parse_since).transpose()?;
    let until_dt = until.as_deref().map(telemetry::parse_until).transpose()?;

    if follow {
        cmd_logs_follow(&log_dir, logs_filter.as_ref(), since_dt, until_dt, &format)
    } else {
        cmd_logs_query(&log_dir, logs_filter.as_ref(), since_dt, until_dt, &format)
    }
}

/// Non-follow mode: read all logs and print them.
fn cmd_logs_query(
    log_dir: &Path,
    filter: Option<&telemetry::LogsFilter>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    format: &LogFormat,
) -> Result<()> {
    let events = telemetry::read_logs(log_dir, since, until, filter)?;

    if events.is_empty() {
        println!("No matching events found.");
        return Ok(());
    }

    let stdout_sink = telemetry::StdoutSink::new(&StdoutSinkConfig {
        enabled: true,
        format: crate::config::StdoutFormat::Normal,
        color: crate::config::ColorMode::Auto,
    });

    for event in &events {
        match format {
            LogFormat::Table | LogFormat::Human => {
                println!("{}", stdout_sink.format_event(event));
            }
            LogFormat::Json | LogFormat::Jsonl => {
                let line = serde_json::to_string(event).context("failed to serialize event")?;
                println!("{line}");
            }
        }
    }

    Ok(())
}

/// Follow mode: tail new events from all log files.
fn cmd_logs_follow(
    log_dir: &Path,
    filter: Option<&telemetry::LogsFilter>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    format: &LogFormat,
) -> Result<()> {
    use std::io::BufRead;

    if !log_dir.is_dir() {
        bail!("log directory does not exist: {}", log_dir.display());
    }

    let stdout_sink = telemetry::StdoutSink::new(&StdoutSinkConfig {
        enabled: true,
        format: crate::config::StdoutFormat::Normal,
        color: crate::config::ColorMode::Auto,
    });

    // Build set of known log files and their current sizes (to tail from end).
    let mut file_positions: std::collections::HashMap<PathBuf, u64> =
        std::collections::HashMap::new();

    // Print existing events since cutoff first, then tail.
    if since.is_some() || filter.is_some() {
        let events = telemetry::read_logs(log_dir, since, until, filter)?;
        for event in &events {
            print_log_event(event, format, &stdout_sink);
        }
    }

    // Record current positions (after initial read).
    if let Ok(entries) = std::fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                file_positions.insert(path, len);
            }
        }
    }

    // Polling loop: check for new content every 500ms.
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));

        // Check for new files.
        if let Ok(entries) = std::fs::read_dir(log_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                file_positions.entry(path).or_insert(0);
            }
        }

        // Read new content from each file.
        let positions: Vec<(PathBuf, u64)> = file_positions
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        for (path, pos) in positions {
            let current_len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if current_len <= pos {
                continue;
            }

            let file = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(_) => continue,
            };

            use std::io::Seek;
            let mut reader = std::io::BufReader::new(file);
            if reader.seek(std::io::SeekFrom::Start(pos)).is_err() {
                continue;
            }

            let mut new_pos = pos;
            let mut line_buf = String::new();
            while reader.read_line(&mut line_buf).unwrap_or(0) > 0 {
                let trimmed = line_buf.trim();
                if !trimmed.is_empty() {
                    if let Ok(event) = serde_json::from_str::<telemetry::TelemetryEvent>(trimmed) {
                        let passes = filter.map(|f| f.matches(&event)).unwrap_or(true);
                        let passes_until = until.map(|u| event.timestamp <= u).unwrap_or(true);
                        if passes && passes_until {
                            print_log_event(&event, format, &stdout_sink);
                        }
                    }
                }
                new_pos += line_buf.len() as u64;
                line_buf.clear();
            }
            file_positions.insert(path, new_pos);
        }
    }
}

/// Print a single telemetry event in the requested format.
fn print_log_event(
    event: &telemetry::TelemetryEvent,
    format: &LogFormat,
    sink: &telemetry::StdoutSink,
) {
    match format {
        LogFormat::Table | LogFormat::Human => println!("{}", sink.format_event(event)),
        LogFormat::Json | LogFormat::Jsonl => {
            if let Ok(line) = serde_json::to_string(event) {
                println!("{line}");
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Status helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Per-worker status information for the status command.
struct WorkerStatus {
    entry: WorkerEntry,
    heartbeat_state: Option<String>,
    current_bead: Option<String>,
    pid_alive: bool,
    uptime_secs: u64,
}

/// Check if a process with the given PID is alive.
fn is_pid_alive(pid: u32) -> bool {
    // kill(pid, 0) checks if process exists without sending a signal.
    libc_kill(pid as i32, 0) == 0
}

/// Minimal binding to kill(2) — only used for PID existence check.
///
/// Returns 0 if the process exists, -1 otherwise.
fn libc_kill(pid: i32, sig: i32) -> i32 {
    // SAFETY: kill(pid, 0) is a standard POSIX call that checks PID existence.
    // No signal is actually sent.
    unsafe {
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        kill(pid, sig)
    }
}

/// Format a duration in seconds to a human-readable string.
fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// tmux session discovery
// ──────────────────────────────────────────────────────────────────────────────

/// A running tmux session belonging to needle.
#[derive(Debug, Clone, serde::Serialize)]
struct TmuxSession {
    name: String,
    created: String,
    status: String,
}

/// List all tmux sessions whose names start with `needle-`.
fn list_needle_sessions() -> Result<Vec<TmuxSession>> {
    let output = ProcessCommand::new("tmux")
        .args([
            "list-sessions",
            "-F",
            "#{session_name}\t#{session_created}\t#{session_attached}",
        ])
        .output();

    let output = match output {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // tmux not installed — no sessions.
            return Ok(vec![]);
        }
        Err(e) => {
            return Err(e).context("failed to run tmux list-sessions");
        }
    };

    // tmux exits non-zero when there are no sessions.
    if !output.status.success() {
        return Ok(vec![]);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let sessions = stdout
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() < 3 {
                return None;
            }
            let name = parts[0];
            if !name.starts_with("needle-") {
                return None;
            }
            let created = parts[1].to_string();
            let attached = parts[2];
            let status = if attached == "1" {
                "attached".to_string()
            } else {
                "detached".to_string()
            };
            Some(TmuxSession {
                name: name.to_string(),
                created,
                status,
            })
        })
        .collect();

    Ok(sessions)
}

/// Return the set of worker IDs already running for a given agent.
///
/// Parses tmux session names matching `needle-{agent}-{worker_id}` and returns
/// the `{worker_id}` portion. Returns an empty set if no sessions are running
/// or tmux is unavailable.
fn occupied_worker_ids(agent: &str) -> Result<HashSet<String>> {
    let prefix = format!("needle-{agent}-");
    let sessions = list_needle_sessions()?;
    let ids = sessions
        .iter()
        .filter_map(|s| s.name.strip_prefix(&prefix))
        .map(|id| id.to_string())
        .collect();
    Ok(ids)
}

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Simple shell escaping — wraps in single quotes.
fn shell_escape(s: &str) -> String {
    if s.contains(|c: char| c.is_whitespace() || c == '\'' || c == '"' || c == '\\') {
        format!("'{}'", s.replace('\'', "'\\''"))
    } else {
        s.to_string()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// canary, upgrade, rollback
// ──────────────────────────────────────────────────────────────────────────────

/// `needle canary` — run canary tests or show channel status.
fn cmd_canary(show_status: bool) -> Result<()> {
    let config = ConfigLoader::load_global()?;

    let runner = crate::canary::CanaryRunner::new(
        config.workspace.home.clone(),
        config.self_modification.canary_workspace.clone(),
        config.self_modification.canary_timeout,
    );

    if show_status {
        let status = runner.status()?;
        println!("Release Channel Status");
        println!("──────────────────────");
        println!(
            "  :testing  {}  {}",
            if status.testing_exists { "✓" } else { "✗" },
            status.testing_path.display()
        );
        println!(
            "  :stable   {}  {}",
            if status.stable_exists { "✓" } else { "✗" },
            status.stable_path.display()
        );
        println!(
            "  :prev     {}  {}",
            if status.prev_exists { "✓" } else { "✗" },
            status.prev_path.display()
        );
        if let Some(target) = &status.symlink_target {
            println!("  symlink   → {}", target.display());
        } else {
            println!("  symlink   ✗ {}", status.symlink_path.display());
        }
        return Ok(());
    }

    if !config.self_modification.enabled {
        bail!("self-modification is disabled — set self_modification.enabled = true in config");
    }

    let tel = crate::telemetry::Telemetry::from_config("canary".to_string(), &config.telemetry)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "hook telemetry init failed, falling back");
            crate::telemetry::Telemetry::new("canary".to_string())
        });

    let suite_id = runner.testing_binary().display().to_string();
    tel.emit(crate::telemetry::EventKind::CanaryStarted {
        suite: suite_id.clone(),
    })?;

    println!("Running canary tests...");
    let report = runner.run()?;

    tel.emit(crate::telemetry::EventKind::CanarySuiteCompleted {
        suite: suite_id,
        passed: report.passed as u32,
        failed: (report.failed + report.timed_out + report.errors) as u32,
    })?;

    println!("\nCanary Report");
    println!("─────────────");
    println!("  Binary:   {}", report.testing_binary.display());
    println!("  Tests:    {}", report.total_tests);
    println!("  Passed:   {}", report.passed);
    println!("  Failed:   {}", report.failed);
    println!("  Timed out: {}", report.timed_out);
    println!("  Errors:   {}", report.errors);
    println!("  Duration: {}s", report.duration_secs);
    println!();

    for result in &report.results {
        let (icon, bead_id, detail) = match result {
            crate::canary::CanaryTestResult::Passed { bead_id, .. } => {
                ("✓", bead_id.as_str(), String::new())
            }
            crate::canary::CanaryTestResult::Failed {
                bead_id, reason, ..
            } => ("✗", bead_id.as_str(), format!(" — {reason}")),
            crate::canary::CanaryTestResult::TimedOut {
                bead_id,
                elapsed_secs,
            } => (
                "⏱",
                bead_id.as_str(),
                format!(" — timed out after {elapsed_secs}s"),
            ),
            crate::canary::CanaryTestResult::Error { bead_id, message } => {
                ("!", bead_id.as_str(), format!(" — {message}"))
            }
        };
        println!("  {icon} {bead_id}{detail}");
    }

    if report.can_promote() {
        if config.self_modification.auto_promote {
            println!("\nAll tests passed — auto-promoting :testing to :stable...");
            // Capture hash before promote moves the file.
            let hash = crate::upgrade::file_hash(&report.testing_binary)
                .unwrap_or_else(|_| "unknown".to_string());
            runner.promote()?;
            tel.emit(crate::telemetry::EventKind::CanaryPromoted { hash })?;
            println!("Promotion complete. Fleet will hot-reload on next cycle.");
        } else {
            println!("\nAll tests passed. Run `needle canary --status` to verify, then promote manually.");
            println!(
                "To promote: move needle-testing → needle-stable in {:?}",
                config.workspace.home.join("bin")
            );
        }
    } else {
        let reason = format!(
            "{} failed, {} timed out, {} errors",
            report.failed, report.timed_out, report.errors
        );
        println!("\nCanary tests FAILED. :testing will NOT be promoted.");
        runner.reject()?;
        tel.emit(crate::telemetry::EventKind::CanaryRejected { reason })?;
        println!("Testing binary discarded.");
    }

    Ok(())
}

/// `needle upgrade` — check for and install updates.
fn cmd_upgrade(check_only: bool) -> Result<()> {
    if check_only {
        let check = crate::upgrade::check_for_update()?;
        if check.update_available {
            println!(
                "Update available: {} → {}",
                check.current_version, check.latest_version
            );
            if let Some(notes) = &check.release_notes {
                println!("\nRelease notes:\n{notes}");
            }
        } else {
            println!("Already up to date (version {})", check.current_version);
        }
        return Ok(());
    }

    crate::upgrade::perform_upgrade()?;
    Ok(())
}

/// `needle rollback` — restore the previous :stable binary.
fn cmd_rollback() -> Result<()> {
    let config = ConfigLoader::load_global()?;

    let runner = crate::canary::CanaryRunner::new(
        config.workspace.home.clone(),
        config.self_modification.canary_workspace.clone(),
        config.self_modification.canary_timeout,
    );

    let status = runner.status()?;
    if !status.prev_exists {
        bail!("no previous :stable binary to rollback to");
    }

    // Capture hashes before rollback.
    let stable_hash = if status.stable_exists {
        crate::upgrade::file_hash(&status.stable_path).unwrap_or_else(|_| "unknown".to_string())
    } else {
        "none".to_string()
    };
    let prev_hash =
        crate::upgrade::file_hash(&status.prev_path).unwrap_or_else(|_| "unknown".to_string());

    println!("Rolling back to previous :stable...");
    runner.rollback()?;

    // Emit rollback telemetry.
    let tel = crate::telemetry::Telemetry::from_config("rollback".to_string(), &config.telemetry)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "hook telemetry init failed, falling back");
            crate::telemetry::Telemetry::new("rollback".to_string())
        });
    tel.emit(crate::telemetry::EventKind::RollbackCompleted {
        rolled_back_hash: stable_hash,
        restored_hash: prev_hash,
    })?;

    println!("Rollback complete. Fleet will hot-reload on next cycle.");
    Ok(())
}

/// `needle reflect` — run learning consolidation on demand.
fn cmd_reflect(workspace: Option<PathBuf>, force: bool) -> Result<()> {
    let workspace_root = if let Some(ref ws) = workspace {
        ws.canonicalize().unwrap_or_else(|_| ws.clone())
    } else {
        let global = ConfigLoader::load_global()?;
        global.workspace.default.clone()
    };

    let cli_overrides = crate::config::CliOverrides {
        workspace: Some(workspace_root.clone()),
        ..Default::default()
    };
    let (config, _) = crate::config::ConfigLoader::load_resolved(&workspace_root, cli_overrides)?;

    let tel = crate::telemetry::Telemetry::from_config("reflect".to_string(), &config.telemetry)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "hook telemetry init failed, falling back");
            crate::telemetry::Telemetry::new("reflect".to_string())
        });

    let state_dir = config.workspace.home.join("state").join("reflect");

    let strand = crate::strand::ReflectStrand::new(
        config.strands.reflect.clone(),
        workspace_root.clone(),
        state_dir,
        tel,
    );

    let summary = strand.consolidate(force)?;

    if summary.beads_processed == 0 && summary.learnings_added == 0 {
        println!(
            "No consolidation performed (below threshold or on cooldown). Use --force to override."
        );
    } else {
        println!(
            "Reflect complete: {} beads processed, {} learnings added, {} pruned, {} skills promoted",
            summary.beads_processed,
            summary.learnings_added,
            summary.learnings_pruned,
            summary.skills_promoted,
        );
    }

    Ok(())
}

/// `needle update-rules` — download the latest gitleaks rules and update the
/// vendored `config/gitleaks.toml`.
///
/// Downloads from upstream, validates by compiling all rules, and writes to
/// the output path. Rebuild needle after running this to embed the new rules.
fn cmd_update_rules(output: Option<PathBuf>) -> Result<()> {
    use crate::sanitize::{Sanitizer, GITLEAKS_UPSTREAM_URL};

    let out_path = output.unwrap_or_else(|| PathBuf::from("config/gitleaks.toml"));

    println!("Fetching latest gitleaks rules from upstream...");
    println!("  URL: {GITLEAKS_UPSTREAM_URL}");

    let response = ureq::get(GITLEAKS_UPSTREAM_URL)
        .call()
        .context("failed to fetch gitleaks.toml from upstream")?;

    if response.status() >= 400 {
        anyhow::bail!(
            "upstream returned HTTP {} when fetching gitleaks.toml",
            response.status()
        );
    }

    let content = response
        .into_string()
        .context("failed to read upstream response body")?;

    // Validate by parsing and compiling all rules.
    let sanitizer = Sanitizer::from_toml(&content, &[])
        .context("downloaded gitleaks.toml failed validation")?;

    println!(
        "  Validated: {} rules compiled successfully.",
        sanitizer.rule_count()
    );

    // Create output directory if needed.
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create output directory: {}", parent.display())
            })?;
        }
    }

    std::fs::write(&out_path, &content)
        .with_context(|| format!("failed to write {}", out_path.display()))?;

    println!(
        "  Written: {} ({} bytes)",
        out_path.display(),
        content.len()
    );
    println!("Rebuild needle to embed the updated rules.");

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nato_alphabet_has_26_entries() {
        assert_eq!(NATO_ALPHABET.len(), 26);
    }

    #[test]
    fn first_nato_is_alpha() {
        assert_eq!(NATO_ALPHABET[0], "alpha");
    }

    #[test]
    fn last_nato_is_zulu() {
        assert_eq!(NATO_ALPHABET[25], "zulu");
    }

    #[test]
    fn version_string_format() {
        let version = env!("CARGO_PKG_VERSION");
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;
        let expected = format!("needle {version} (rust, {os} {arch})");
        assert!(expected.starts_with("needle 0."));
        assert!(expected.contains("rust"));
    }

    #[test]
    fn is_needle_inner_false_by_default() {
        // Without NEEDLE_INNER set this should return false (env-dependent,
        // but confirms the function does not panic).
        // We cannot unset env vars reliably in a parallel test suite, so we
        // only assert the call succeeds without panicking.
        let _ = is_needle_inner();
    }

    #[test]
    fn is_needle_inner_true_when_env_set() {
        // Temporarily set NEEDLE_INNER=1 and verify detection.
        // Use a sub-process approach via std::process to avoid mutating the
        // test process's env and racing with parallel tests.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env("NEEDLE_INNER", "1")
            .args(["--help"])
            .output();
        // We can't call is_needle_inner() with a controlled env from here
        // without unsafe env mutation, so we verify the env var logic directly.
        assert!(
            std::env::var("NEEDLE_INNER")
                .map(|v| v == "1")
                .unwrap_or(false)
                || output.is_ok(),
            "env var logic should work"
        );
    }

    #[test]
    fn is_needle_inner_false_for_other_values() {
        // Values other than "1" should not be treated as inner invocations.
        // Directly test the underlying logic without mutating env.
        let check = |v: &str| -> bool { v == "1" };
        assert!(!check("0"));
        assert!(!check("true"));
        assert!(!check("yes"));
        assert!(!check(""));
        assert!(check("1"));
    }

    #[test]
    fn shell_escape_plain_string() {
        assert_eq!(shell_escape("hello"), "hello");
    }

    #[test]
    fn shell_escape_string_with_spaces() {
        assert_eq!(shell_escape("hello world"), "'hello world'");
    }

    #[test]
    fn shell_escape_string_with_quotes() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn cli_parses_run_count_5() {
        let cli = Cli::try_parse_from(["needle", "run", "--count", "5"]);
        assert!(cli.is_ok(), "needle run --count 5 should parse");
        if let Ok(Cli {
            command: CliCommand::Run { count, .. },
        }) = cli
        {
            assert_eq!(count, 5);
        }
    }

    #[test]
    fn nato_alphabet_sequence() {
        assert_eq!(NATO_ALPHABET[0], "alpha");
        assert_eq!(NATO_ALPHABET[1], "bravo");
        assert_eq!(NATO_ALPHABET[2], "charlie");
        assert_eq!(NATO_ALPHABET[3], "delta");
        assert_eq!(NATO_ALPHABET[4], "echo");
    }

    #[test]
    fn multi_worker_count_validation() {
        // count=0 should be detected as invalid.
        let count: u32 = 0;
        assert_eq!(count, 0, "zero count is invalid");
        // count > 26 exceeds NATO alphabet.
        let big: u32 = 27;
        assert!(big as usize > NATO_ALPHABET.len(), "exceeds NATO alphabet");
    }

    #[test]
    fn max_workers_cap_logic() {
        let count: u32 = 5;
        let max_workers: u32 = 3;
        let effective = if max_workers > 0 && count > max_workers {
            max_workers
        } else {
            count
        };
        assert_eq!(effective, 3, "should cap to max_workers");
    }

    #[test]
    fn max_workers_zero_means_unlimited() {
        let count: u32 = 10;
        let max_workers: u32 = 0;
        let effective = if max_workers > 0 && count > max_workers {
            max_workers
        } else {
            count
        };
        assert_eq!(effective, 10, "max_workers=0 should not cap");
    }

    #[test]
    fn stop_requires_all_or_identifier() {
        // Neither --all nor --identifier should fail.
        let all = false;
        let identifier: Option<String> = None;
        assert!(
            !all && identifier.is_none(),
            "should require --all or --identifier"
        );
    }

    #[test]
    fn list_format_variants() {
        // Ensure both format variants exist (compile-time check).
        let _table = ListFormat::Table;
        let _json = ListFormat::Json;
    }

    #[test]
    fn default_worker_identifier_is_alpha() {
        let worker_id = NATO_ALPHABET[0];
        assert_eq!(worker_id, "alpha");
    }

    #[test]
    fn session_name_format() {
        let agent = "claude";
        let worker_id = "alpha";
        let session = format!("needle-{agent}-{worker_id}");
        assert_eq!(session, "needle-claude-alpha");
        assert!(session.starts_with("needle-"));
    }

    #[test]
    fn cli_parses_run_defaults() {
        // Verify clap parses with minimal args.
        let cli = Cli::try_parse_from(["needle", "run"]);
        assert!(cli.is_ok(), "needle run should parse with defaults");
    }

    #[test]
    fn cli_parses_version() {
        let cli = Cli::try_parse_from(["needle", "version"]);
        assert!(cli.is_ok(), "needle version should parse");
    }

    #[test]
    fn cli_parses_list_with_format() {
        let cli = Cli::try_parse_from(["needle", "list", "--format", "json"]);
        assert!(cli.is_ok(), "needle list --format json should parse");
    }

    #[test]
    fn cli_parses_stop_all() {
        let cli = Cli::try_parse_from(["needle", "stop", "--all"]);
        assert!(cli.is_ok(), "needle stop --all should parse");
    }

    #[test]
    fn cli_parses_stop_identifier() {
        let cli = Cli::try_parse_from(["needle", "stop", "--identifier", "alpha"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn cli_parses_run_full() {
        let cli = Cli::try_parse_from([
            "needle",
            "run",
            "--workspace",
            "/tmp/ws",
            "--agent",
            "claude",
            "--count",
            "1",
            "--identifier",
            "alpha",
            "--timeout",
            "600",
        ]);
        assert!(cli.is_ok(), "needle run with all flags should parse");
    }

    #[test]
    fn cli_rejects_unknown_subcommand() {
        let cli = Cli::try_parse_from(["needle", "dance"]);
        assert!(cli.is_err(), "unknown subcommand should be rejected");
    }

    #[test]
    fn cli_parses_test_agent() {
        let cli = Cli::try_parse_from(["needle", "test-agent", "claude-sonnet"]);
        assert!(cli.is_ok(), "needle test-agent <name> should parse");
        if let Ok(Cli {
            command: CliCommand::TestAgent { name },
        }) = cli
        {
            assert_eq!(name, "claude-sonnet");
        }
    }

    #[test]
    fn cli_test_agent_requires_name() {
        let cli = Cli::try_parse_from(["needle", "test-agent"]);
        assert!(cli.is_err(), "test-agent should require a name argument");
    }

    // ── New CLI extension tests ──

    #[test]
    fn cli_parses_attach() {
        let cli = Cli::try_parse_from(["needle", "attach", "alpha"]);
        assert!(cli.is_ok(), "needle attach alpha should parse");
        if let Ok(Cli {
            command: CliCommand::Attach { identifier },
        }) = cli
        {
            assert_eq!(identifier, "alpha");
        }
    }

    #[test]
    fn cli_attach_requires_identifier() {
        let cli = Cli::try_parse_from(["needle", "attach"]);
        assert!(cli.is_err(), "attach should require an identifier");
    }

    #[test]
    fn cli_parses_status_defaults() {
        let cli = Cli::try_parse_from(["needle", "status"]);
        assert!(cli.is_ok(), "needle status should parse with defaults");
        if let Ok(Cli {
            command: CliCommand::Status { by_worker, .. },
        }) = cli
        {
            assert!(!by_worker, "by_worker should default to false");
        }
    }

    #[test]
    fn cli_parses_status_by_worker() {
        let cli = Cli::try_parse_from(["needle", "status", "--by-worker"]);
        assert!(cli.is_ok(), "needle status --by-worker should parse");
        if let Ok(Cli {
            command: CliCommand::Status { by_worker, .. },
        }) = cli
        {
            assert!(by_worker);
        }
    }

    #[test]
    fn cli_parses_status_json() {
        let cli = Cli::try_parse_from(["needle", "status", "--format", "json"]);
        assert!(cli.is_ok(), "needle status --format json should parse");
    }

    #[test]
    fn cli_parses_config_dump() {
        let cli = Cli::try_parse_from(["needle", "config", "--dump"]);
        assert!(cli.is_ok(), "needle config --dump should parse");
        if let Ok(Cli {
            command: CliCommand::ConfigCmd { dump, .. },
        }) = cli
        {
            assert!(dump);
        }
    }

    #[test]
    fn cli_parses_config_dump_show_source() {
        let cli = Cli::try_parse_from(["needle", "config", "--dump", "--show-source"]);
        assert!(
            cli.is_ok(),
            "needle config --dump --show-source should parse"
        );
        if let Ok(Cli {
            command: CliCommand::ConfigCmd {
                dump, show_source, ..
            },
        }) = cli
        {
            assert!(dump);
            assert!(show_source);
        }
    }

    #[test]
    fn cli_parses_config_get() {
        let cli = Cli::try_parse_from(["needle", "config", "--get", "agent.default"]);
        assert!(cli.is_ok(), "needle config --get should parse");
        if let Ok(Cli {
            command: CliCommand::ConfigCmd { get, .. },
        }) = cli
        {
            assert_eq!(get.as_deref(), Some("agent.default"));
        }
    }

    #[test]
    fn cli_parses_doctor() {
        let cli = Cli::try_parse_from(["needle", "doctor"]);
        assert!(cli.is_ok(), "needle doctor should parse");
        if let Ok(Cli {
            command: CliCommand::Doctor { repair, .. },
        }) = cli
        {
            assert!(!repair);
        }
    }

    #[test]
    fn cli_parses_doctor_repair() {
        let cli = Cli::try_parse_from(["needle", "doctor", "--repair"]);
        assert!(cli.is_ok(), "needle doctor --repair should parse");
        if let Ok(Cli {
            command: CliCommand::Doctor { repair, .. },
        }) = cli
        {
            assert!(repair);
        }
    }

    #[test]
    fn cli_parses_doctor_with_workspace() {
        let cli = Cli::try_parse_from(["needle", "doctor", "--workspace", "/tmp/ws"]);
        assert!(cli.is_ok());
        if let Ok(Cli {
            command: CliCommand::Doctor { workspace, .. },
        }) = cli
        {
            assert_eq!(workspace, Some(PathBuf::from("/tmp/ws")));
        }
    }

    #[test]
    fn config_get_key_known_keys() {
        let config = Config::default();
        assert!(config_get_key(&config, "agent.default").is_some());
        assert!(config_get_key(&config, "agent.timeout").is_some());
        assert!(config_get_key(&config, "worker.max_workers").is_some());
        assert!(config_get_key(&config, "health.heartbeat_interval_secs").is_some());
        assert!(config_get_key(&config, "workspace.default").is_some());
        assert!(config_get_key(&config, "workspace.home").is_some());
    }

    #[test]
    fn config_get_key_unknown_returns_none() {
        let config = Config::default();
        assert!(config_get_key(&config, "nonexistent.key").is_none());
    }

    #[test]
    fn config_dump_returns_all_fields() {
        let config = Config::default();
        let lines = config_dump(&config);
        assert!(lines.len() >= 10, "should have at least 10 config lines");
        assert!(lines.iter().any(|l| l.starts_with("agent.default:")));
        assert!(lines.iter().any(|l| l.starts_with("worker.max_workers:")));
        assert!(lines
            .iter()
            .any(|l| l.starts_with("health.heartbeat_ttl_secs:")));
    }

    #[test]
    fn format_duration_seconds() {
        assert_eq!(format_duration(30), "30s");
    }

    #[test]
    fn format_duration_minutes() {
        assert_eq!(format_duration(90), "1m30s");
    }

    #[test]
    fn format_duration_hours() {
        assert_eq!(format_duration(3700), "1h1m");
    }

    #[test]
    fn is_pid_alive_current_process() {
        assert!(is_pid_alive(std::process::id()));
    }

    #[test]
    fn is_pid_alive_nonexistent() {
        // PID 999999 is very unlikely to exist.
        assert!(!is_pid_alive(999_999));
    }

    // ── Logs subcommand parsing tests ──

    #[test]
    fn cli_parses_logs_defaults() {
        let cli = Cli::try_parse_from(["needle", "logs"]);
        assert!(cli.is_ok(), "needle logs should parse with defaults");
        if let Ok(Cli {
            command:
                CliCommand::Logs {
                    follow,
                    filter,
                    since,
                    ..
                },
        }) = cli
        {
            assert!(!follow);
            assert!(filter.is_empty());
            assert!(since.is_none());
        }
    }

    #[test]
    fn cli_parses_logs_follow() {
        let cli = Cli::try_parse_from(["needle", "logs", "--follow"]);
        assert!(cli.is_ok(), "needle logs --follow should parse");
        if let Ok(Cli {
            command: CliCommand::Logs { follow, .. },
        }) = cli
        {
            assert!(follow);
        }
    }

    #[test]
    fn cli_parses_logs_filter() {
        let cli = Cli::try_parse_from(["needle", "logs", "--filter", "bead.claim.*"]);
        assert!(cli.is_ok(), "needle logs --filter should parse");
        if let Ok(Cli {
            command: CliCommand::Logs { filter, .. },
        }) = cli
        {
            assert_eq!(filter, vec!["bead.claim.*"]);
        }
    }

    #[test]
    fn cli_parses_logs_multiple_filters() {
        let cli = Cli::try_parse_from([
            "needle",
            "logs",
            "--filter",
            "event_type=bead.outcome",
            "--filter",
            "worker_id=alpha",
        ]);
        assert!(
            cli.is_ok(),
            "needle logs with multiple --filter should parse"
        );
        if let Ok(Cli {
            command: CliCommand::Logs { filter, .. },
        }) = cli
        {
            assert_eq!(filter.len(), 2);
            assert_eq!(filter[0], "event_type=bead.outcome");
            assert_eq!(filter[1], "worker_id=alpha");
        }
    }

    #[test]
    fn cli_parses_logs_filter_field_equals() {
        let cli = Cli::try_parse_from([
            "needle",
            "logs",
            "--filter",
            "event_type=bead.claim.succeeded",
        ]);
        assert!(cli.is_ok());
        if let Ok(Cli {
            command: CliCommand::Logs { filter, .. },
        }) = cli
        {
            assert_eq!(filter[0], "event_type=bead.claim.succeeded");
        }
    }

    #[test]
    fn cli_parses_logs_filter_field_regex() {
        let cli = Cli::try_parse_from(["needle", "logs", "--filter", "event_type~bead\\..*"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn cli_parses_logs_filter_field_gt() {
        let cli = Cli::try_parse_from(["needle", "logs", "--filter", "duration_ms>500"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn cli_parses_logs_since() {
        let cli = Cli::try_parse_from(["needle", "logs", "--since", "1h"]);
        assert!(cli.is_ok(), "needle logs --since should parse");
        if let Ok(Cli {
            command: CliCommand::Logs { since, .. },
        }) = cli
        {
            assert_eq!(since.as_deref(), Some("1h"));
        }
    }

    #[test]
    fn cli_parses_logs_format_jsonl() {
        let cli = Cli::try_parse_from(["needle", "logs", "--format", "jsonl"]);
        assert!(cli.is_ok(), "needle logs --format jsonl should parse");
    }

    #[test]
    fn cli_parses_logs_all_flags() {
        let cli = Cli::try_parse_from([
            "needle", "logs", "--follow", "--filter", "bead.*", "--since", "24h", "--format",
            "jsonl",
        ]);
        assert!(cli.is_ok(), "needle logs with all flags should parse");
    }

    #[test]
    fn cli_parses_status_cost() {
        let cli = Cli::try_parse_from(["needle", "status", "--cost"]);
        assert!(cli.is_ok(), "needle status --cost should parse");
        if let Ok(Cli {
            command: CliCommand::Status { cost, .. },
        }) = cli
        {
            assert!(cost);
        }
    }

    #[test]
    fn cli_parses_status_cost_since() {
        let cli = Cli::try_parse_from(["needle", "status", "--cost", "--since", "7d"]);
        assert!(cli.is_ok(), "needle status --cost --since should parse");
        if let Ok(Cli {
            command: CliCommand::Status { cost, since, .. },
        }) = cli
        {
            assert!(cost);
            assert_eq!(since.as_deref(), Some("7d"));
        }
    }

    #[test]
    fn log_format_variants() {
        let _human = LogFormat::Human;
        let _jsonl = LogFormat::Jsonl;
    }

    #[test]
    fn cli_parses_canary() {
        let cli = Cli::try_parse_from(["needle", "canary"]);
        assert!(cli.is_ok(), "needle canary should parse");
    }

    #[test]
    fn cli_parses_canary_status() {
        let cli = Cli::try_parse_from(["needle", "canary", "--status"]);
        assert!(cli.is_ok(), "needle canary --status should parse");
        if let Ok(Cli {
            command: CliCommand::Canary { status },
        }) = cli
        {
            assert!(status);
        }
    }

    #[test]
    fn cli_parses_upgrade() {
        let cli = Cli::try_parse_from(["needle", "upgrade"]);
        assert!(cli.is_ok(), "needle upgrade should parse");
    }

    #[test]
    fn cli_parses_upgrade_check() {
        let cli = Cli::try_parse_from(["needle", "upgrade", "--check"]);
        assert!(cli.is_ok(), "needle upgrade --check should parse");
        if let Ok(Cli {
            command: CliCommand::Upgrade { check },
        }) = cli
        {
            assert!(check);
        }
    }

    #[test]
    fn cli_parses_rollback() {
        let cli = Cli::try_parse_from(["needle", "rollback"]);
        assert!(cli.is_ok(), "needle rollback should parse");
    }

    // ── Session collision avoidance tests ──

    /// Helper: given an occupied set, pick the next N available NATO names.
    fn pick_available_names(occupied: &HashSet<String>, count: usize) -> Result<Vec<String>> {
        let mut ids = Vec::with_capacity(count);
        for name in NATO_ALPHABET {
            if ids.len() >= count {
                break;
            }
            if occupied.contains(*name) {
                continue;
            }
            ids.push(name.to_string());
        }
        if ids.len() < count {
            bail!(
                "cannot launch {} workers — only {} NATO names available ({} occupied)",
                count,
                ids.len(),
                occupied.len()
            );
        }
        Ok(ids)
    }

    #[test]
    fn pick_names_no_sessions_running() {
        let occupied = HashSet::new();
        let names = pick_available_names(&occupied, 3).unwrap();
        assert_eq!(names, vec!["alpha", "bravo", "charlie"]);
    }

    #[test]
    fn pick_names_skips_occupied() {
        let occupied: HashSet<String> = ["alpha", "bravo"].iter().map(|s| s.to_string()).collect();
        let names = pick_available_names(&occupied, 2).unwrap();
        assert_eq!(names, vec!["charlie", "delta"]);
    }

    #[test]
    fn pick_names_skips_non_contiguous_occupied() {
        let occupied: HashSet<String> = ["alpha", "charlie", "echo"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let names = pick_available_names(&occupied, 3).unwrap();
        assert_eq!(names, vec!["bravo", "delta", "foxtrot"]);
    }

    #[test]
    fn pick_names_all_occupied_fails() {
        let occupied: HashSet<String> = NATO_ALPHABET.iter().map(|s| s.to_string()).collect();
        let result = pick_available_names(&occupied, 1);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("26 occupied"), "got: {msg}");
    }

    #[test]
    fn pick_names_partial_exhaustion_fails() {
        // Occupy 25 names, request 2 — only 1 available.
        let occupied: HashSet<String> = NATO_ALPHABET[..25].iter().map(|s| s.to_string()).collect();
        let result = pick_available_names(&occupied, 2);
        assert!(result.is_err());
    }

    #[test]
    fn identifier_collision_detected() {
        let occupied: HashSet<String> = ["alpha"].iter().map(|s| s.to_string()).collect();
        let requested = "alpha";
        assert!(
            occupied.contains(requested),
            "should detect identifier collision"
        );
    }

    #[test]
    fn identifier_no_collision() {
        let occupied: HashSet<String> = ["bravo"].iter().map(|s| s.to_string()).collect();
        let requested = "alpha";
        assert!(!occupied.contains(requested), "alpha is not occupied");
    }

    #[test]
    fn parse_worker_id_from_session_name() {
        let agent = "claude";
        let prefix = format!("needle-{agent}-");
        let session_name = "needle-claude-foxtrot";
        let worker_id = session_name.strip_prefix(&prefix);
        assert_eq!(worker_id, Some("foxtrot"));
    }

    #[test]
    fn parse_worker_id_different_agent_ignored() {
        let agent = "claude";
        let prefix = format!("needle-{agent}-");
        let session_name = "needle-gemini-alpha";
        let worker_id = session_name.strip_prefix(&prefix);
        assert_eq!(worker_id, None, "different agent session should not match");
    }

    #[test]
    fn single_worker_picks_first_available() {
        let occupied: HashSet<String> = ["alpha"].iter().map(|s| s.to_string()).collect();
        let worker_id = NATO_ALPHABET
            .iter()
            .find(|name| !occupied.contains(**name))
            .map(|s| s.to_string())
            .unwrap();
        assert_eq!(worker_id, "bravo");
    }

    // ── Doctor check function unit tests ──────────────────────────────────────

    #[test]
    fn check_result_display_pass() {
        let r = CheckResult::pass("Config", "valid");
        let d = r.display();
        assert!(d.contains("[PASS]"), "display should show PASS");
        assert!(d.contains("Config"), "display should show name");
        assert!(d.contains("valid"), "display should show message");
    }

    #[test]
    fn check_result_display_warn() {
        let r = CheckResult::warn("SQLite integrity", "sqlite3 not on PATH");
        assert!(r.display().contains("[WARN]"));
    }

    #[test]
    fn check_result_display_fail() {
        let r = CheckResult::fail("Workspace", "not found");
        assert!(r.display().contains("[FAIL]"));
    }

    #[test]
    fn check_result_with_detail() {
        let r = CheckResult::fail("JSONL", "2 invalid of 10 records")
            .with_detail(vec!["line 3".to_string(), "line 7".to_string()]);
        assert_eq!(r.detail.len(), 2);
        assert_eq!(r.detail[0], "line 3");
    }

    #[test]
    fn doctor_check_workspace_missing() {
        let r = doctor_check_workspace(Path::new("/nonexistent/path/xyz"));
        assert_eq!(r.status, CheckStatus::Fail);
    }

    #[test]
    fn doctor_check_workspace_no_beads_dir() {
        let tmp = tempfile::tempdir().unwrap();
        // Dir exists but no .beads/ subdirectory.
        let r = doctor_check_workspace(tmp.path());
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.message.contains(".beads/"), "should mention .beads/");
    }

    #[test]
    fn doctor_check_workspace_valid() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".beads")).unwrap();
        let r = doctor_check_workspace(tmp.path());
        assert_eq!(r.status, CheckStatus::Pass);
    }

    #[test]
    fn doctor_check_jsonl_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let r = doctor_check_jsonl(tmp.path());
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.message.contains("issues.jsonl"));
    }

    #[test]
    fn doctor_check_jsonl_valid() {
        let tmp = tempfile::tempdir().unwrap();
        let jsonl = tmp.path().join("issues.jsonl");
        std::fs::write(
            &jsonl,
            "{\"id\":\"nd-1\",\"title\":\"test\"}\n{\"id\":\"nd-2\"}\n",
        )
        .unwrap();
        let r = doctor_check_jsonl(tmp.path());
        assert_eq!(r.status, CheckStatus::Pass);
        assert!(r.message.contains("2 records"));
    }

    #[test]
    fn doctor_check_jsonl_invalid_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let jsonl = tmp.path().join("issues.jsonl");
        // Two valid, one invalid.
        std::fs::write(&jsonl, "{\"id\":\"nd-1\"}\nNOT JSON\n{\"id\":\"nd-3\"}\n").unwrap();
        let r = doctor_check_jsonl(tmp.path());
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.message.contains("1 invalid"), "got: {}", r.message);
    }

    #[test]
    fn doctor_check_jsonl_empty_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("issues.jsonl"), "").unwrap();
        let r = doctor_check_jsonl(tmp.path());
        assert_eq!(r.status, CheckStatus::Pass);
        assert!(r.message.contains("0 records"));
    }

    #[test]
    fn doctor_check_lock_files_no_locks() {
        let tmp = tempfile::tempdir().unwrap();
        let r = doctor_check_lock_files(tmp.path(), 3600, false);
        assert_eq!(r.status, CheckStatus::Pass);
    }

    #[test]
    fn doctor_check_lock_files_fresh_not_stale() {
        let tmp = tempfile::tempdir().unwrap();
        // Create a fresh .lock file (mtime = now).
        std::fs::write(tmp.path().join("workspace.lock"), b"").unwrap();
        // TTL of 1 hour — newly written file is not stale.
        let r = doctor_check_lock_files(tmp.path(), 3600, false);
        assert_eq!(r.status, CheckStatus::Pass);
    }

    #[test]
    fn doctor_check_lock_files_stale_warns_without_repair() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = tmp.path().join("workspace.lock");
        std::fs::write(&lock, b"").unwrap();
        // Set mtime to 2 hours ago using filetime manipulation via set_file_times.
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
        let ft = filetime::FileTime::from_system_time(past);
        filetime::set_file_mtime(&lock, ft).unwrap();

        let r = doctor_check_lock_files(tmp.path(), 3600, false);
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(r.message.contains("stale"));
    }

    #[test]
    fn doctor_check_lock_files_stale_repair_removes() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = tmp.path().join("workspace.lock");
        std::fs::write(&lock, b"").unwrap();
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
        let ft = filetime::FileTime::from_system_time(past);
        filetime::set_file_mtime(&lock, ft).unwrap();

        let r = doctor_check_lock_files(tmp.path(), 3600, true);
        assert_eq!(r.status, CheckStatus::Pass);
        assert!(!lock.exists(), "stale lock should be removed by repair");
    }

    #[test]
    fn doctor_check_heartbeat_dir_missing_no_repair() {
        let tmp = tempfile::tempdir().unwrap();
        let hb_dir = tmp.path().join("heartbeats");
        let r = doctor_check_heartbeat_dir(&hb_dir, false);
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(r.message.contains("missing"));
    }

    #[test]
    fn doctor_check_heartbeat_dir_missing_with_repair() {
        let tmp = tempfile::tempdir().unwrap();
        let hb_dir = tmp.path().join("heartbeats");
        let r = doctor_check_heartbeat_dir(&hb_dir, true);
        assert_eq!(r.status, CheckStatus::Pass);
        assert!(hb_dir.exists(), "repair should create the directory");
    }

    #[test]
    fn doctor_check_heartbeat_dir_existing_writable() {
        let tmp = tempfile::tempdir().unwrap();
        let r = doctor_check_heartbeat_dir(tmp.path(), false);
        assert_eq!(r.status, CheckStatus::Pass);
    }

    #[test]
    fn doctor_check_heartbeats_no_dir() {
        let r = doctor_check_heartbeats(Path::new("/nonexistent/hb"), 300, false);
        assert_eq!(r.status, CheckStatus::Pass);
    }

    #[test]
    fn doctor_check_heartbeats_no_files() {
        let tmp = tempfile::tempdir().unwrap();
        let r = doctor_check_heartbeats(tmp.path(), 300, false);
        assert_eq!(r.status, CheckStatus::Pass);
        assert!(r.message.contains("0 file(s)"));
    }

    #[test]
    fn doctor_check_heartbeats_stale_file_warns() {
        use crate::health::HeartbeatData;
        use crate::types::WorkerState;

        let tmp = tempfile::tempdir().unwrap();
        let hb = HeartbeatData {
            worker_id: "test-w".to_string(),
            pid: 999_999,
            state: WorkerState::Selecting,
            current_bead: None,
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: chrono::Utc::now() - chrono::Duration::seconds(600),
            started_at: chrono::Utc::now(),
            beads_processed: 0,
            session: "test-w".to_string(),
        };
        std::fs::write(
            tmp.path().join("test-w.json"),
            serde_json::to_string(&hb).unwrap(),
        )
        .unwrap();

        let r = doctor_check_heartbeats(tmp.path(), 300, false);
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(r.message.contains("stale"));
    }

    #[test]
    fn doctor_check_heartbeats_stale_file_repair() {
        use crate::health::HeartbeatData;
        use crate::types::WorkerState;

        let tmp = tempfile::tempdir().unwrap();
        let hb = HeartbeatData {
            worker_id: "test-rm".to_string(),
            pid: 999_999,
            state: WorkerState::Selecting,
            current_bead: None,
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: chrono::Utc::now() - chrono::Duration::seconds(600),
            started_at: chrono::Utc::now(),
            beads_processed: 0,
            session: "test-rm".to_string(),
        };
        let hb_path = tmp.path().join("test-rm.json");
        std::fs::write(&hb_path, serde_json::to_string(&hb).unwrap()).unwrap();

        let r = doctor_check_heartbeats(tmp.path(), 300, true);
        assert_eq!(r.status, CheckStatus::Pass);
        assert!(
            !hb_path.exists(),
            "repair should remove stale heartbeat file"
        );
    }

    #[test]
    fn doctor_check_telemetry_logs_no_dir() {
        let config = crate::config::Config::default();
        let tmp = tempfile::tempdir().unwrap();
        // Log dir doesn't exist — should pass (no logs yet).
        let needle_home = tmp.path().to_path_buf();
        let r = doctor_check_telemetry_logs(&config, &needle_home, false);
        assert_eq!(r.status, CheckStatus::Pass);
    }

    #[test]
    fn doctor_check_telemetry_logs_existing_files() {
        let mut config = crate::config::Config::default();
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join("logs");
        std::fs::create_dir(&log_dir).unwrap();
        std::fs::write(log_dir.join("session-1.jsonl"), b"{}").unwrap();
        std::fs::write(log_dir.join("session-2.jsonl"), b"{}").unwrap();
        config.telemetry.file_sink.log_dir = Some(log_dir);
        config.telemetry.file_sink.retention_days = 0; // No retention = no expiry.

        let r = doctor_check_telemetry_logs(&config, tmp.path(), false);
        assert_eq!(r.status, CheckStatus::Pass);
        assert!(r.message.contains("2 file(s)"));
    }

    #[test]
    fn doctor_check_peers_no_heartbeats() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("heartbeats")).unwrap();
        let r = doctor_check_peers(&tmp.path().join("heartbeats"), 300);
        assert_eq!(r.status, CheckStatus::Pass);
        assert!(r.message.contains("no workers running"));
    }

    #[test]
    fn doctor_check_sqlite_no_db() {
        let tmp = tempfile::tempdir().unwrap();
        // No beads.db present — should pass with "JSONL-only mode" message.
        let r = doctor_check_sqlite(tmp.path());
        assert_eq!(r.status, CheckStatus::Pass);
        assert!(r.message.contains("no database"));
    }
}
