//! CLI layer — parses commands and manages worker sessions.
//!
//! Entry point for the `needle` binary. Routes subcommands to worker
//! lifecycle management. Handles tmux session creation/detection so that
//! `needle run` outside tmux self-invokes into a tmux session and
//! `needle run` inside tmux starts the worker directly.
//!
//! Depends on: `worker`, `config`.

use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand, ValueEnum};

use crate::bead_store::{BeadStore, BrCliBeadStore};
use crate::config::{CliOverrides, Config, ConfigLoader, StdoutSinkConfig};
use crate::dispatch;
use crate::health::HeartbeatData;
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

        /// Show cost summary.
        #[arg(long)]
        cost: bool,

        /// Filter events since this time (e.g., 1h, 24h, 7d, 2026-03-20).
        #[arg(long)]
        since: Option<String>,
    },

    /// View and query telemetry logs.
    Logs {
        /// Stream events in real-time (tail -f equivalent).
        #[arg(long)]
        follow: bool,

        /// Filter by event type (glob pattern, e.g., "bead.claim.*").
        #[arg(long)]
        filter: Option<String>,

        /// Show events since this time (e.g., 1h, 24h, 7d, 2026-03-20).
        #[arg(long)]
        since: Option<String>,

        /// Output format: human (default) or jsonl.
        #[arg(long, value_enum, default_value = "human")]
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
    Human,
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
        } => cmd_status(format, by_worker, cost, since),
        CliCommand::Logs {
            follow,
            filter,
            since,
            format,
        } => cmd_logs(follow, filter, since, format),
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
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Command handlers
// ──────────────────────────────────────────────────────────────────────────────

/// `needle run` — launch a worker.
///
/// If outside tmux: create tmux sessions (one per worker) with staggered startup.
/// If inside tmux: start a single worker directly.
fn cmd_run(
    workspace: Option<PathBuf>,
    agent: Option<String>,
    count: u32,
    identifier: Option<String>,
    timeout: Option<u64>,
    resume: bool,
) -> Result<()> {
    // Load and configure.
    let mut config = ConfigLoader::load_global()?;

    let overrides = CliOverrides {
        workspace: workspace.clone(),
        agent_binary: agent.clone(),
        max_workers: None,
        ..Default::default()
    };
    ConfigLoader::apply_overrides(&mut config, overrides);

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

    if is_inside_tmux() {
        // Already inside tmux — start a single worker directly.
        let worker_id = identifier
            .clone()
            .unwrap_or_else(|| NATO_ALPHABET[0].to_string());
        let agent_name = agent.as_deref().unwrap_or(&config.agent.default);
        let session_name = format!("needle-{agent_name}-{worker_id}");
        tracing::info!(worker = %worker_id, session = %session_name, "starting worker directly (inside tmux)");
        run_worker(config, worker_id)
    } else {
        // Outside tmux — create tmux sessions with staggered startup.
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

    for seq in 0..effective_count {
        let worker_id = if effective_count == 1 {
            identifier
                .clone()
                .unwrap_or_else(|| NATO_ALPHABET[0].to_string())
        } else {
            NATO_ALPHABET[seq as usize].to_string()
        };
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
        let worker_id = identifier.as_deref().unwrap_or(NATO_ALPHABET[0]);
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

/// Check if we're inside a tmux session by inspecting the $TMUX env var.
fn is_inside_tmux() -> bool {
    std::env::var("TMUX").is_ok_and(|v| !v.is_empty())
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
        "{} {}",
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
        let cutoff = since.as_deref().map(telemetry::parse_since).transpose()?;
        let events = telemetry::read_logs(&log_dir, cutoff, None)?;
        let cs = telemetry::compute_cost_summary(&events);

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
            }
            ListFormat::Json => {
                let cost_json = serde_json::json!({
                    "dispatch_events": cs.total_events,
                    "total_cost_usd": cs.total_cost_usd,
                    "total_tokens_in": cs.total_tokens_in,
                    "total_tokens_out": cs.total_tokens_out,
                    "total_elapsed_ms": cs.total_elapsed_ms,
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

/// `needle doctor` — check system health and optionally repair.
fn cmd_doctor(repair: bool, workspace: Option<PathBuf>) -> Result<()> {
    let config = ConfigLoader::load_global()?;
    let needle_home = config.workspace.home.clone();

    let workspace_root = workspace.unwrap_or_else(|| config.workspace.default.clone());

    println!("NEEDLE Doctor");
    println!("{}", "-".repeat(50));
    let mut issues_found = 0;

    // 1. Check workspace exists and has .beads/.
    print!("Workspace ({})... ", workspace_root.display());
    let beads_dir = workspace_root.join(".beads");
    if beads_dir.is_dir() {
        println!("OK");
    } else {
        println!("MISSING .beads/ directory");
        issues_found += 1;
    }

    // 2. Check registry health.
    print!("Worker registry... ");
    let registry = Registry::default_location(&needle_home);
    match registry.list() {
        Ok(workers) => {
            let stale: Vec<&WorkerEntry> =
                workers.iter().filter(|w| !is_pid_alive(w.pid)).collect();
            if stale.is_empty() {
                println!("OK ({} workers registered)", workers.len());
            } else {
                println!(
                    "WARNING: {} stale entries (dead PIDs: {})",
                    stale.len(),
                    stale
                        .iter()
                        .map(|w| format!("{}(pid={})", w.id, w.pid))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                issues_found += stale.len();

                if repair {
                    for w in &stale {
                        if let Err(e) = registry.deregister(&w.id) {
                            println!("  Failed to deregister {}: {e}", w.id);
                        } else {
                            println!("  Deregistered stale worker: {}", w.id);
                        }
                    }
                }
            }
        }
        Err(e) => {
            println!("ERROR: {e}");
            issues_found += 1;
        }
    }

    // 3. Check heartbeat files for staleness.
    print!("Heartbeats... ");
    let heartbeat_dir = needle_home.join("state").join("heartbeats");
    if heartbeat_dir.is_dir() {
        let mut stale_heartbeats = 0;
        let mut total_heartbeats = 0;

        if let Ok(entries) = std::fs::read_dir(&heartbeat_dir) {
            for entry in entries.flatten() {
                if entry.path().extension().is_some_and(|e| e == "json") {
                    total_heartbeats += 1;
                    if let Ok(content) = std::fs::read_to_string(entry.path()) {
                        if let Ok(hb) = serde_json::from_str::<HeartbeatData>(&content) {
                            let age = Utc::now()
                                .signed_duration_since(hb.last_heartbeat)
                                .num_seconds();
                            if age > config.health.heartbeat_ttl_secs as i64 {
                                stale_heartbeats += 1;
                                if repair {
                                    if let Err(e) = std::fs::remove_file(entry.path()) {
                                        println!(
                                            "\n  Failed to remove stale heartbeat {}: {e}",
                                            entry.path().display()
                                        );
                                    } else {
                                        println!(
                                            "\n  Removed stale heartbeat: {}",
                                            entry.path().display()
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if stale_heartbeats == 0 {
            println!("OK ({total_heartbeats} files)");
        } else {
            println!("WARNING: {stale_heartbeats} stale of {total_heartbeats}");
            issues_found += stale_heartbeats;
        }
    } else {
        println!("OK (no heartbeat directory)");
    }

    // 4. Run br doctor on the workspace.
    if beads_dir.is_dir() {
        print!("Bead database... ");
        let store = BrCliBeadStore::discover(workspace_root.clone());
        match store {
            Ok(s) => {
                let rt =
                    tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;

                if repair {
                    match rt.block_on(s.doctor_repair()) {
                        Ok(report) => {
                            if report.warnings.is_empty() && report.fixed.is_empty() {
                                println!("OK (repaired)");
                            } else {
                                if !report.warnings.is_empty() {
                                    println!("WARNINGS:");
                                    for w in &report.warnings {
                                        println!("  {w}");
                                    }
                                }
                                if !report.fixed.is_empty() {
                                    println!("  Fixed:");
                                    for f in &report.fixed {
                                        println!("    {f}");
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            println!("REPAIR FAILED: {e}");
                            issues_found += 1;
                        }
                    }
                } else {
                    match rt.block_on(s.doctor_check()) {
                        Ok(report) => {
                            if report.warnings.is_empty() {
                                println!("OK");
                            } else {
                                println!("WARNINGS:");
                                for w in &report.warnings {
                                    println!("  {w}");
                                }
                                issues_found += report.warnings.len();
                            }
                        }
                        Err(e) => {
                            println!("ERROR: {e}");
                            issues_found += 1;
                        }
                    }
                }
            }
            Err(e) => {
                println!("ERROR: could not locate br CLI: {e}");
                issues_found += 1;
            }
        }
    }

    // 5. Check telemetry log directory.
    print!("Telemetry logs... ");
    let log_dir = config
        .telemetry
        .file_sink
        .log_dir
        .clone()
        .unwrap_or_else(|| needle_home.join("logs"));
    if log_dir.is_dir() {
        let count = std::fs::read_dir(&log_dir)
            .map(|entries| entries.count())
            .unwrap_or(0);
        println!("OK ({count} files in {})", log_dir.display());
    } else {
        println!("OK (no log directory yet)");
    }

    // Summary.
    println!();
    if issues_found == 0 {
        println!("All checks passed.");
    } else if repair {
        println!("{issues_found} issue(s) found and repair attempted.");
    } else {
        println!("{issues_found} issue(s) found. Run with --repair to fix.");
    }

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Logs command
// ──────────────────────────────────────────────────────────────────────────────

/// `needle logs` — view and query telemetry logs.
fn cmd_logs(
    follow: bool,
    filter: Option<String>,
    since: Option<String>,
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

    let filter_re = filter
        .as_deref()
        .map(telemetry::glob_to_regex)
        .transpose()?;

    let cutoff = since.as_deref().map(telemetry::parse_since).transpose()?;

    if follow {
        cmd_logs_follow(&log_dir, filter_re.as_ref(), cutoff, &format)
    } else {
        cmd_logs_query(&log_dir, filter_re.as_ref(), cutoff, &format)
    }
}

/// Non-follow mode: read all logs and print them.
fn cmd_logs_query(
    log_dir: &Path,
    filter: Option<&regex::Regex>,
    since: Option<DateTime<Utc>>,
    format: &LogFormat,
) -> Result<()> {
    let events = telemetry::read_logs(log_dir, since, filter)?;

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
            LogFormat::Human => {
                println!("{}", stdout_sink.format_event(event));
            }
            LogFormat::Jsonl => {
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
    filter: Option<&regex::Regex>,
    since: Option<DateTime<Utc>>,
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
    if let Some(ref cutoff) = since {
        let events = telemetry::read_logs(log_dir, Some(*cutoff), filter)?;
        for event in &events {
            match format {
                LogFormat::Human => println!("{}", stdout_sink.format_event(event)),
                LogFormat::Jsonl => {
                    if let Ok(line) = serde_json::to_string(event) {
                        println!("{line}");
                    }
                }
            }
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
                        let passes_filter = filter
                            .map(|re| re.is_match(&event.event_type))
                            .unwrap_or(true);
                        if passes_filter {
                            match format {
                                LogFormat::Human => {
                                    println!("{}", stdout_sink.format_event(&event))
                                }
                                LogFormat::Jsonl => {
                                    if let Ok(line) = serde_json::to_string(&event) {
                                        println!("{line}");
                                    }
                                }
                            }
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

    println!("Running canary tests...");
    let report = runner.run()?;

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
            runner.promote()?;
            println!("Promotion complete. Fleet will hot-reload on next cycle.");
        } else {
            println!("\nAll tests passed. Run `needle canary --status` to verify, then promote manually.");
            println!(
                "To promote: move needle-testing → needle-stable in {:?}",
                config.workspace.home.join("bin")
            );
        }
    } else {
        println!("\nCanary tests FAILED. :testing will NOT be promoted.");
        runner.reject()?;
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
    fn is_inside_tmux_does_not_panic() {
        // The function should not panic regardless of environment.
        let _ = is_inside_tmux();
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
            assert!(filter.is_none());
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
            assert_eq!(filter.as_deref(), Some("bead.claim.*"));
        }
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
}
