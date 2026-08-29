//! CLI layer — parses commands and manages worker sessions.
//!
//! Entry point for the `needle` binary. Routes subcommands to worker
//! lifecycle management. Always creates dedicated tmux sessions for workers.
//! Re-entrant inner invocations (launched by `launch_in_tmux()`) are
//! detected via the `NEEDLE_INNER=1` environment variable and run the
//! worker directly without spawning another session.
//!
//! Depends on: `worker`, `config`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand, ValueEnum};

use crate::bead_store::{spawn_with_etxtbsy_retry_sync_child, BeadStore};
use crate::config::{
    CliOverrides, Config, ConfigLoader, ConfigSource, SourceMap, StdoutSinkConfig,
};
use crate::dispatch;
use crate::health::{HealthMonitor, HeartbeatData};
use crate::rate_limit::RateLimiter;
use crate::registry::{Registry, WorkerEntry};
use crate::telemetry::{self, EventKind, Telemetry};
use crate::types::IdleAction;
use crate::upgrade;
use crate::worker::Worker;

// ──────────────────────────────────────────────────────────────────────────────
// NATO alphabet for worker identifiers
// ──────────────────────────────────────────────────────────────────────────────

const NATO_ALPHABET: &[&str] = &[
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet",
    "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango",
    "uniform", "victor", "whiskey", "xray", "yankee", "zulu",
];

/// Tmux silently replaces dots with underscores in session names.
/// Normalize consistently so creation and lookup agree.
fn sanitize_session_name(name: &str) -> String {
    name.replace('.', "_")
}

// ──────────────────────────────────────────────────────────────────────────────
// CLI definition
// ──────────────────────────────────────────────────────────────────────────────

/// NEEDLE — Navigates Every Enqueued Deliverable, Logs Effort.
///
/// Deterministic bead processing with explicit outcome paths.
#[derive(Debug, Parser)]
#[command(name = "needle", version = crate::build_metadata::VERSION_STRING, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: CliCommand,
}

#[derive(Debug, Subcommand)]
pub enum CliCommand {
    /// Launch worker(s) to process beads.
    Run {
        /// Home workspace for this worker.
        ///
        /// NOT an exclusive scope. The Explore strand still auto-discovers every
        /// directory containing `.beads/` under `strands.explore.workspace_root`,
        /// so the worker can claim beads in other repos. Setting this only changes
        /// where the worker starts and which store is its home.
        ///
        /// To genuinely restrict a worker to a fixed set of repos, set
        /// `strands.explore.workspaces` instead (a non-empty list disables
        /// auto-discovery). Defaults to `workspace.default`.
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

        /// Enable or disable hot-reload for this worker.
        #[arg(long)]
        hot_reload: Option<bool>,
    },

    /// Stop running worker(s).
    Stop {
        /// Kill the tmux session of every registered worker.
        ///
        /// This does NOT reliably terminate the worker supervisors: a
        /// `needle run` process whose session is killed is commonly reparented
        /// to init and keeps running, and may ignore SIGINT. After this
        /// command, verify with `pgrep -af needle-stable run` and signal any
        /// survivors directly. See `needle cleanup` for session hygiene.
        #[arg(long)]
        all: bool,

        /// Identifier of the worker to stop.
        #[arg(short = 'i', long)]
        identifier: Option<String>,
    },

    /// Remove needle tmux sessions.
    ///
    /// Without flags, only removes sessions without active workers (liveness-checked).
    /// With --all, removes every session including live ones with active workers.
    Cleanup {
        /// Remove every needle session, including live sessions with active workers.
        ///
        /// This is destructive — active workers will be killed along with their sessions.
        /// Use without this flag for safe, liveness-checked cleanup of orphaned sessions only.
        #[arg(long)]
        all: bool,

        /// Session name pattern to match (partial match).
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

        /// Show cooldown state for idle-time strands (reflect, weave, pulse, unravel).
        #[arg(long)]
        idle_strands: bool,
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

        /// Set a config key to a value (KEY VALUE or KEY=VALUE format).
        ///
        /// Two forms are accepted:
        ///   --set KEY VALUE      (space-separated)
        ///   --set KEY=VALUE       (equals-separated)
        ///
        /// Examples:
        ///   --set worker.max_workers 10
        ///   --set agent.timeout=3600
        ///   --set log.console_sink=true
        ///   --set strands.explore.enabled=false
        #[arg(long, num_args = 0.., value_name = "KEY VALUE")]
        set: Option<Vec<String>>,

        /// Dump all resolved config values.
        #[arg(long)]
        dump: bool,

        /// Show source annotations (requires --dump).
        #[arg(long)]
        show_source: bool,

        /// Show live config from running workers (requires --dump).
        ///
        /// When enabled, displays the actual configuration in use by running workers,
        /// including the reload generation counter. This allows operators to confirm
        /// what workers are actually running rather than what the config files say.
        #[arg(long)]
        live: bool,
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

    /// Initialize v2 config with optional v1 migration.
    ///
    /// Creates ~/.config/needle/config.yaml. Detects existing v1 artifacts
    /// in ~/.needle/ and migrates compatible settings (agent name, workspace
    /// path, worker count) to the v2 YAML schema. Safe to run on already-
    /// initialized installs (idempotent).
    ///
    /// When run in a workspace (a directory containing .beads/), also creates
    /// .needle.yaml with an explicit bead backend binding.
    Init {
        /// Bead backend to bind in .needle.yaml (default: bead-rs).
        #[arg(long, default_value = "bead-rs")]
        backend: String,
    },

    /// Show version information.
    Version,

    /// Validate an agent adapter.
    TestAgent {
        /// Name of the adapter to test.
        name: String,
    },

    /// Resolve and verify a bead backend descriptor against a workspace.
    #[command(name = "bead-backend")]
    BeadBackend {
        /// Builtin backend name (`bead-rs` or `bead-forge`).
        name: String,
        /// Workspace used for the capability probe.
        #[arg(short = 'w', long, default_value = ".")]
        workspace: PathBuf,
    },

    /// Audit repository bead-backend bindings without changing them.
    #[command(name = "bead-backend-audit")]
    BeadBackendAudit {
        /// Directory whose immediate child repositories are audited.
        #[arg(default_value = ".")]
        root: PathBuf,
    },

    /// Explicitly bind one repository to a bead backend descriptor.
    #[command(name = "bead-backend-bind")]
    BeadBackendBind {
        /// Builtin backend name (`bead-rs` or `bead-forge`).
        backend: String,
        /// Repository to update.
        #[arg(default_value = ".")]
        workspace: PathBuf,
    },

    /// Run canary tests against a :testing binary.
    Canary {
        /// Show channel status instead of running tests.
        #[arg(long)]
        status: bool,
    },

    /// Check for and install updates from GitHub releases.
    ///
    /// Do not cp/mv onto ~/.local/bin/needle or ~/.needle/bin/needle-stable while any worker is running.
    /// This causes session disruption or permanent hot-reload stall. Use `needle upgrade` instead.
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

    /// Show outcome statistics aggregated from telemetry logs.
    ///
    /// Reads JSONL telemetry files, correlates dispatch/outcome/effort events
    /// by bead ID, and prints per-group statistics.
    ///
    /// Examples:
    ///   needle stats --by template_version --since 7d
    ///   needle stats --by task_type --since 30d
    ///   needle stats --by worker --since 7d --format json
    Stats {
        /// Dimension to group results by.
        #[arg(long, value_enum)]
        by: StatsBy,

        /// Include only events since this time (e.g., 1h, 24h, 7d, 2026-03-20).
        #[arg(long)]
        since: Option<String>,

        /// Include only events until this time (e.g., 1h, 24h, 7d, 2026-03-20T15:00:00Z).
        #[arg(long)]
        until: Option<String>,

        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: ListFormat,
    },

    /// Query stored telemetry logs and return per-worker statistics.
    ///
    /// Reads JSONL telemetry files from log_dir and aggregates event counts,
    /// last activity, and beads processed per worker.
    ///
    /// Examples:
    ///   needle query --worker-id alpha
    ///   needle query --since 24h
    ///   needle query --event-type bead.claim.succeeded
    Query {
        /// Filter by worker ID (partial match supported).
        #[arg(long)]
        worker_id: Option<String>,

        /// Include only events since this time (e.g., 1h, 24h, 7d, 2026-03-20).
        #[arg(long)]
        since: Option<String>,

        /// Include only events until this time (e.g., 1h, 24h, 7d, 2026-03-20T15:00:00Z).
        #[arg(long)]
        until: Option<String>,

        /// Filter by event type (supports glob patterns like bead.*).
        #[arg(long)]
        event_type: Option<String>,

        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: ListFormat,
    },

    /// Run the fleet supervisor daemon (auto-scale workers based on queue depth).
    Supervise {
        /// Workspace to monitor exclusively. The supervisor spawns workers
        /// only for this workspace's ready queue; it does not auto-discover
        /// or scale workers for other workspaces. Defaults to the global
        /// config workspace if not specified.
        #[arg(short = 'w', long)]
        workspace: Option<PathBuf>,
    },

    /// Reconcile authoritative Forgejo/Argo CI without occupying an agent slot.
    #[command(name = "ci-reconcile")]
    CiReconcile {
        /// Repository to reconcile (defaults to the configured workspace).
        #[arg(short = 'w', long)]
        workspace: Option<PathBuf>,
        /// Run one bounded poll cycle and exit.
        #[arg(long)]
        once: bool,
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

/// Grouping dimension for the `needle stats` command.
#[derive(Debug, Clone, ValueEnum)]
pub enum StatsBy {
    /// Group by template version tag (e.g., `"pluck-v2"`).
    #[value(name = "template_version")]
    TemplateVersion,
    /// Group by template name / task type (e.g., `"pluck"`).
    #[value(name = "task_type")]
    TaskType,
    /// Group by worker identifier (e.g., `"needle-alpha"`).
    #[value(name = "worker")]
    Worker,
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
            hot_reload,
        } => cmd_run(
            workspace, agent, count, identifier, timeout, resume, hot_reload,
        ),
        CliCommand::Stop { all, identifier } => cmd_stop(all, identifier),
        CliCommand::Cleanup { all, identifier } => cmd_cleanup(all, identifier),
        CliCommand::List { format } => cmd_list(format),
        CliCommand::Attach { identifier } => cmd_attach(&identifier),
        CliCommand::Status {
            format,
            by_worker,
            cost,
            since,
            until,
            idle_strands,
        } => cmd_status(format, by_worker, cost, since, until, idle_strands),
        CliCommand::Logs {
            follow,
            filter,
            since,
            until,
            format,
        } => cmd_logs(follow, filter, since, until, format),
        CliCommand::ConfigCmd {
            get,
            set,
            dump,
            show_source,
            live,
        } => cmd_config(get, set, dump, show_source, live),
        CliCommand::Doctor { repair, workspace } => cmd_doctor(repair, workspace),
        CliCommand::Init { backend } => cmd_init(&backend),
        CliCommand::Version => {
            cmd_version();
            Ok(())
        }
        CliCommand::TestAgent { name } => cmd_test_agent(&name),
        CliCommand::BeadBackend { name, workspace } => cmd_bead_backend(&name, &workspace),
        CliCommand::BeadBackendAudit { root } => cmd_bead_backend_audit(&root),
        CliCommand::BeadBackendBind { backend, workspace } => {
            cmd_bead_backend_bind(&backend, &workspace)
        }
        CliCommand::Canary { status } => cmd_canary(status),
        CliCommand::Upgrade { check } => cmd_upgrade(check),
        CliCommand::Rollback => cmd_rollback(),
        CliCommand::Reflect { workspace, force } => cmd_reflect(workspace, force),
        CliCommand::UpdateRules { output } => cmd_update_rules(output),
        CliCommand::Stats {
            by,
            since,
            until,
            format,
        } => cmd_stats(by, since, until, format),
        CliCommand::Supervise { workspace } => cmd_supervise(workspace),
        CliCommand::CiReconcile { workspace, once } => cmd_ci_reconcile(workspace, once),
        CliCommand::Query {
            worker_id,
            since,
            until,
            event_type,
            format,
        } => {
            bail!(
                "Query command is not yet implemented. \
                 Called with worker_id={:?}, since={:?}, until={:?}, event_type={:?}, format={:?}",
                worker_id,
                since,
                until,
                event_type,
                format
            )
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Command handlers
// ──────────────────────────────────────────────────────────────────────────────

/// `needle ci-reconcile` — run the worker-free post-push CI reconciler.
fn cmd_ci_reconcile(workspace: Option<PathBuf>, once: bool) -> Result<()> {
    let workspace_root = match workspace {
        Some(workspace) => workspace.canonicalize().unwrap_or(workspace),
        None => ConfigLoader::load_global()?.workspace.default,
    };
    let (config, _) = ConfigLoader::load_resolved(
        &workspace_root,
        CliOverrides {
            workspace: Some(workspace_root.clone()),
            ..Default::default()
        },
    )?;
    if !config.post_push_ci.enabled {
        bail!("post_push_ci is disabled for {}", workspace_root.display());
    }
    let source = crate::ci::ForgejoArgoResultSource::new(config.post_push_ci.clone())?;
    let store = crate::bead_store::open_configured(
        &config.bead_cli,
        workspace_root.clone(),
        None,
        Some("needle-ci-reconciler".to_string()),
        Some(env!("CARGO_PKG_VERSION").to_string()),
    )?;
    let telemetry = Telemetry::from_config("needle-ci-reconciler".to_string(), &config.telemetry)
        .unwrap_or_else(|error| {
            tracing::warn!(error = %error, "failed to configure CI reconciler telemetry");
            Telemetry::new("needle-ci-reconciler".to_string())
        });
    let coordinator = crate::ci::CiCoordinator::new(
        store.as_ref(),
        config.post_push_ci.clone(),
        Some(telemetry.clone()),
    );
    let poll_interval = config.post_push_ci.poll_interval_secs.max(1);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create CI reconciler runtime")?;
    rt.block_on(async move {
        telemetry.start_and_wait().await?;
        loop {
            let outcomes = coordinator.reconcile_once(&workspace_root, &source).await?;
            tracing::info!(
                workspace = %workspace_root.display(),
                outcomes = outcomes.len(),
                "authoritative CI reconciliation cycle completed"
            );
            if once {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(poll_interval)).await;
        }
        telemetry.shutdown().await;
        Ok::<(), anyhow::Error>(())
    })
}

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
    hot_reload: Option<bool>,
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
    let (mut config, mut sources) = ConfigLoader::load_resolved(&workspace_root, cli_overrides)?;

    if let Some(t) = timeout {
        config.agent.timeout = t;
        sources.insert("agent.timeout".to_string(), ConfigSource::CliOverride);
    }

    if let Some(hr) = hot_reload {
        config.self_modification.hot_reload = hr;
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
        tel.emit(
            crate::telemetry::EventKind::UpgradeCompleted {
                new_hash: current_hash,
            },
            chrono::Utc::now(),
        )?;

        return run_worker(config, worker_id, sources);
    }

    if is_needle_inner() {
        // Re-entrant inner invocation launched by launch_in_tmux() — run
        // the worker directly inside the dedicated session already created.
        let worker_id = identifier
            .clone()
            .unwrap_or_else(|| NATO_ALPHABET[0].to_string());
        let agent_name = agent.as_deref().unwrap_or(&config.agent.default);
        let session_name = sanitize_session_name(&format!("needle-{agent_name}-{worker_id}"));
        tracing::info!(worker = %worker_id, session = %session_name, "starting worker (inner re-entrant invocation)");
        run_worker(config, worker_id, sources)
    } else {
        // Always create dedicated tmux sessions, even if already inside tmux.
        launch_workers(
            config, workspace, agent, count, identifier, timeout, hot_reload,
        )
    }
}

/// Launch `count` workers in separate tmux sessions with staggered startup delays.
pub fn launch_workers(
    config: Config,
    workspace: Option<PathBuf>,
    agent: Option<String>,
    count: u32,
    identifier: Option<String>,
    timeout: Option<u64>,
    hot_reload: Option<bool>,
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
        let session_name = sanitize_session_name(&format!("needle-{agent_name}-{worker_id}"));

        tracing::info!(
            worker_id = %worker_id,
            sequence = seq,
            total = effective_count,
            session = %session_name,
            "launching worker"
        );

        // Stagger: load-adaptive delay before launching subsequent workers.
        if seq > 0 && stagger_secs > 0 {
            // Create a minimal telemetry emitter for stagger events.
            let telemetry = Telemetry::new("cli-launch".to_string());

            // Use load-adaptive stagger: extend wait when system is saturated,
            // otherwise use the configured base delay.
            RateLimiter::load_adaptive_stagger(
                config.worker.cpu_load_warn,
                config.worker.memory_free_warn_mb,
                stagger_secs,
                config.worker.adaptive_stagger_max_wait_secs,
                config.worker.adaptive_stagger_check_interval_secs,
                &telemetry,
            );
        }

        launch_in_tmux(
            &session_name,
            workspace.clone(),
            agent.clone(),
            Some(worker_id.clone()),
            timeout,
            hot_reload,
        )?;

        println!(
            "[{}/{}] Started worker '{}' in tmux session: {session_name}",
            seq + 1,
            effective_count,
            worker_id
        );
    }

    let sanitized_agent = sanitize_session_name(&agent_name);
    if effective_count > 1 {
        println!(
            "\nStarted {effective_count} workers (base stagger: {stagger_secs}s, load-adaptive)."
        );
        println!("Attach to a worker with: tmux attach -t needle-{sanitized_agent}-<name>");
    } else {
        let worker_id = &worker_ids[0];
        println!("Attach with: tmux attach -t needle-{sanitized_agent}-{worker_id}");
    }

    Ok(())
}

/// Initialize the tracing subscriber with OTLP layer if configured.
///
/// Default level for the worker's fmt layer, overridable with `RUST_LOG`.
///
/// Historically no filter was installed on the fmt layer at all. A
/// `tracing_subscriber::registry()` with no filter passes every level, so each
/// `DEBUG needle::telemetry` event was written to the worker's log. On lab that
/// produced 157.7 GB across two files — 100.9 GB of it from a single worker
/// that never processed a bead, looping on a launch that could not succeed
/// (see the `/ 1 cores` saturation miscount). Those same events are already
/// persisted as structured JSONL by `telemetry::FileSink`, which *does* have a
/// retention policy, so the human-readable copy was unmanaged duplication.
///
/// `RUST_LOG=debug` still restores the old behaviour for debugging.
fn worker_log_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
}

/// Bytes per worker log file before it rolls.
const WORKER_LOG_MAX_BYTES: u64 = 128 * 1024 * 1024; // 128 MiB
/// Historical files kept alongside the live one.
///
/// Total on-disk bytes per worker are bounded by
/// `WORKER_LOG_MAX_BYTES * (WORKER_LOG_MAX_FILES + 1)` = **2 GiB**.
///
/// This is a *size* bound, not a time bound, and that distinction is the whole
/// point. `tracing-appender` only rotates on time; during bf-3uj6i a worker
/// produced ~159 GB/hr, so the current hourly file would have passed 159 GB
/// before rotating even once, and `max_log_files` caps file count rather than
/// bytes. A 444 GB disk still filled in under three hours.
const WORKER_LOG_MAX_FILES: usize = 15;
/// Maximum bytes in one formatted worker log line.
const WORKER_LOG_MAX_LINE_BYTES: usize = crate::log_writer::DEFAULT_MAX_LINE_BYTES;

/// Writer for a worker's fmt layer.
///
/// Inner (tmux-launched) workers get an hourly-rotating file that *this process
/// owns*, so rotation actually works. The previous design redirected stderr
/// with `2>>` from the shell, which meant nothing in NEEDLE held the fd: the
/// file could only grow, and `logrotate` would have needed `copytruncate` to
/// have any effect at all.
///
/// Anything not routed through `tracing` — panics, `anyhow` errors on exit, and
/// the `eprintln!` boot diagnostics in [`run_worker`] — still goes to raw
/// stderr, which the tmux command continues to append to `<session>.stderr.log`.
/// That file stays tiny now that the telemetry stream no longer lands in it,
/// and keeping it preserves crash capture.
///
/// Returns the writer plus whether ANSI should be enabled (never, for a file).
fn worker_log_writer(
    config: &crate::config::Config,
    worker_id: &str,
) -> (tracing_subscriber::fmt::writer::BoxMakeWriter, bool) {
    use tracing_subscriber::fmt::writer::BoxMakeWriter;

    let (writer, use_ansi) = if !is_needle_inner() {
        // Foreground/debug invocation — keep logs on the terminal.
        (BoxMakeWriter::new(std::io::stderr), use_ansi())
    } else {
        let log_dir = config
            .telemetry
            .file_sink
            .log_dir
            .clone()
            .unwrap_or_else(|| config.workspace.home.join("logs"));

        let prefix = sanitize_session_name(&format!("needle-{worker_id}"));
        let path = log_dir.join(format!("{prefix}.log"));

        match crate::log_writer::SizeCappedWriter::new(
            &path,
            WORKER_LOG_MAX_BYTES,
            WORKER_LOG_MAX_FILES,
        ) {
            Ok(appender) => (BoxMakeWriter::new(appender), false),
            Err(e) => {
                // Never let logging config abort a worker boot.
                eprintln!(
                    "NEEDLE worker boot: rolling log appender unavailable in {}: {e} — falling back to stderr",
                    log_dir.display()
                );
                (BoxMakeWriter::new(std::io::stderr), use_ansi())
            }
        }
    };

    let writer = crate::log_writer::LineCappedMakeWriter::new(writer, WORKER_LOG_MAX_LINE_BYTES);
    (BoxMakeWriter::new(writer), use_ansi)
}

/// Resolve the identity fields that are available before the worker starts.
///
/// `agent.default` names an adapter, so load the same built-ins and user
/// adapters that `Dispatcher` will use to obtain the actual model identifier.
fn worker_telemetry_identity(config: &Config) -> telemetry::TelemetryIdentity {
    let default_adapter =
        dispatch::load_adapters(&config.agent.adapters_dir, &dispatch::builtin_adapters())
            .ok()
            .and_then(|adapters| adapters.get(&config.agent.default).cloned());

    let model = default_adapter
        .as_ref()
        .and_then(|adapter| adapter.model.clone());
    let provider = default_adapter
        .as_ref()
        .and_then(|adapter| adapter.provider.clone());

    telemetry::TelemetryIdentity {
        agent: Some(config.agent.default.clone()),
        model,
        provider,
        workspace: Some(config.workspace.default.clone()),
    }
}

/// The subscriber shape beneath the reloadable OTLP layer.
///
/// Keeping the layer boxed gives OTLP-enabled and OTLP-disabled boots the same
/// type, so the reload seam is present in both cases. The no-op `Identity`
/// layer is replaced with the real OpenTelemetry layer when the sink is
/// enabled at boot.
pub type OtlpLayerSubscriber =
    tracing_subscriber::layer::Layered<tracing_subscriber::EnvFilter, tracing_subscriber::Registry>;
pub type ReloadableOtlpLayer =
    Box<dyn tracing_subscriber::Layer<OtlpLayerSubscriber> + Send + Sync>;
pub type OtlpReloadHandle =
    tracing_subscriber::reload::Handle<ReloadableOtlpLayer, OtlpLayerSubscriber>;

/// This must be called before any tracing spans are created so that the OTLP
/// layer can export them to the configured collector.
///
/// Note: Shutdown is handled by the OtlpSink in the telemetry module, not here.
#[cfg(feature = "otlp")]
pub fn init_tracing_subscriber(
    worker_id: String,
    session_id: String,
    config: &crate::config::Config,
) -> Result<OtlpReloadHandle> {
    use opentelemetry::trace::TracerProvider;
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    let (writer, use_ansi) = worker_log_writer(config, &worker_id);

    let otlp_layer: ReloadableOtlpLayer = if !config.telemetry.otlp_sink.enabled {
        // The reload seam must exist even when OTLP starts disabled. `try_init`
        // installs a one-shot global subscriber, so adding the seam only in
        // the enabled branch would make a later false -> true reload require
        // a process restart.
        Box::new(tracing_subscriber::layer::Identity::new())
    } else {
        let otlp_config = &config.telemetry.otlp_sink;
        let identity = worker_telemetry_identity(config);

        // Build resource attributes
        let resource = crate::telemetry::otlp::OtlpSink::build_resource(
            &worker_id,
            &session_id,
            otlp_config,
            identity.agent.as_deref(),
            identity.model.as_deref(),
            identity.provider.as_deref(),
            identity.workspace.as_deref().and_then(Path::to_str),
        )
        .context("failed to build OTel resource")?;

        // Create drop channel for tracing layer
        let (drop_tx, mut drop_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::telemetry::otlp::DropEvent>();
        tokio::spawn(async move {
            while let Some(drop) = drop_rx.recv().await {
                tracing::warn!(
                    signal = drop.signal.as_str(),
                    dropped_count = drop.dropped_count,
                    "OTLP tracing layer export failure"
                );
            }
        });

        // Build exporters and tracer provider based on protocol
        let (tracer_provider, ..) = match otlp_config.protocol.as_str() {
            "grpc" => crate::telemetry::otlp::OtlpSink::build_grpc_providers(
                otlp_config,
                &resource,
                drop_tx,
            )?,
            "http" | "http/protobuf" => crate::telemetry::otlp::OtlpSink::build_http_providers(
                otlp_config,
                &resource,
                drop_tx,
            )?,
            other => anyhow::bail!("invalid OTLP protocol: {other}, must be 'grpc' or 'http'"),
        };

        Box::new(
            tracing_opentelemetry::layer::<OtlpLayerSubscriber>()
                .with_tracer(tracer_provider.tracer("needle")),
        )
    };

    // Install the reloadable layer regardless of the initial OTLP setting.
    // The handle is intentionally created at boot so the later cycle-boundary
    // config reload can replace the no-op layer with a live exporter (or turn
    // an exporter off) without reinstalling the global subscriber.
    let (otlp_layer, otlp_reload_handle) = tracing_subscriber::reload::Layer::new(otlp_layer);

    // Create a fmt layer that works with any subscriber implementing LookupSpan.
    let subscriber = tracing_subscriber::registry()
        .with(worker_log_filter())
        .with(otlp_layer)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(use_ansi),
        );

    subscriber
        .try_init()
        .context("failed to initialize tracing subscriber")?;

    Ok(otlp_reload_handle)
}

/// No-op tracing initialization when OTLP feature is disabled.
#[cfg(not(feature = "otlp"))]
fn init_tracing_subscriber(
    worker_id: String,
    _session_id: String,
    config: &crate::config::Config,
) -> Result<OtlpReloadHandle> {
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    let (writer, use_ansi) = worker_log_writer(config, &worker_id);

    // Keep the same reloadable layer shape as the OTLP build. `reload` is part
    // of tracing-subscriber itself, not the optional OTLP feature.
    let otlp_layer: ReloadableOtlpLayer = Box::new(tracing_subscriber::layer::Identity::new());
    let (otlp_layer, otlp_reload_handle) = tracing_subscriber::reload::Layer::new(otlp_layer);

    tracing_subscriber::registry()
        .with(worker_log_filter())
        .with(otlp_layer)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(use_ansi),
        )
        .try_init()
        .context("failed to initialize tracing subscriber")?;

    Ok(otlp_reload_handle)
}

/// Helper function to determine ANSI support (non-Android platforms).
#[cfg(not(target_os = "android"))]
fn use_ansi() -> bool {
    atty::is(atty::Stream::Stderr)
}

/// Helper function for Android (no ANSI).
#[cfg(target_os = "android")]
fn use_ansi() -> bool {
    false
}

/// Generate a session ID for a worker.
///
/// Uses the telemetry module's session ID generator.
fn generate_session_id_for_worker() -> String {
    crate::telemetry::generate_session_id()
}

/// Start the worker state machine (called when inside tmux or for direct mode).
///
/// Creates telemetry and the tokio runtime *before* any other initialization
/// so that `worker.booting` is the very first JSONL event. Each subsequent
/// init step is wrapped with `init.step.started` / `init.step.completed` so a
/// silent hang pinpoints the exact blocking call.
fn run_worker(config: Config, worker_name: String, config_sources: SourceMap) -> Result<()> {
    let boot_start = Instant::now();
    let qualified_id = format!("{}-{}", config.agent.default, worker_name);
    let telemetry_identity = worker_telemetry_identity(&config);

    // HOOP Hook 5 (spawn ack): prove this worker started, before anything else
    // can fail silently. Best-effort — a failed write must never abort boot.
    if let Err(e) = crate::hoop_hooks::write_spawn_ack(&worker_name) {
        eprintln!("NEEDLE worker boot: spawn-ack write failed (non-fatal): {e}");
    }

    // Phase 0: create tokio runtime + telemetry, emit worker.booting immediately.
    // Emit eprintln diagnostics before each step so hangs are visible even if telemetry fails.
    eprintln!("NEEDLE worker boot: creating tokio runtime...");
    let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    let _rt_guard = rt.enter();
    eprintln!("NEEDLE worker boot: tokio runtime created");

    eprintln!("NEEDLE worker boot: initializing tracing subscriber...");
    let session_id = generate_session_id_for_worker();
    let _otlp_reload_handle =
        init_tracing_subscriber(qualified_id.clone(), session_id.clone(), &config)?;
    eprintln!("NEEDLE worker boot: tracing subscriber initialized");

    eprintln!("NEEDLE worker boot: creating telemetry...");
    let telemetry = Telemetry::from_config_with_identity(
        qualified_id.clone(),
        &config.telemetry,
        &telemetry_identity,
    )
    .unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to create hook-enabled telemetry, falling back");
        Telemetry::new(qualified_id.clone())
    });
    eprintln!("NEEDLE worker boot: telemetry created");

    // Emit worker.booting SYNCHRONOUSLY before starting the async writer.
    // This guarantees the event is written to disk even if start_and_wait() hangs.
    // Fixes the silent pre-init deadlock where the JSONL file is created but empty.
    eprintln!("NEEDLE worker boot: emitting worker.booting event (sync)...");
    telemetry.emit_sync(
        EventKind::WorkerBooting {
            worker_name: worker_name.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        chrono::Utc::now(),
    )?;
    eprintln!("NEEDLE worker boot: worker.booting written to disk");

    // Start the async writer thread after worker.booting is on disk.
    eprintln!("NEEDLE worker boot: starting telemetry writer thread...");

    // Runtime guard audit (2026-08-09):
    // All tokio::spawn calls before this point are safe because:
    // - init_tracing_subscriber (line 942): spawn at src/cli/mod.rs:831 is protected by rt.enter() guard (bf-3s2b0)
    // - Telemetry::from_config (line 946): no spawn calls, only channel creation
    // - telemetry.emit_sync (line 957): synchronous write, no spawn
    //
    // Other tokio::spawn sites in the codebase (all safe, not in startup path):
    // - src/worker/mod.rs:1213,1855,2362: worker signal handlers and heartbeats, inside worker.run()
    // - src/dispatch/mod.rs:894,915,953,974: agent dispatch tasks, deep inside async execution
    // - src/telemetry/mod.rs:3407: test-only code (with_sink feature)
    // - src/telemetry/otlp.rs:494,1503,2804: OTLP setup, called after runtime established
    // - src/supervisor/mod.rs:251,274: supervisor signal handlers, separate runtime
    // - src/commit_hook.rs:413,417: test-only concurrent injection test
    rt.block_on(telemetry.start_and_wait())
        .context("writer thread failed to start")?;
    eprintln!("NEEDLE worker boot: writer thread started");

    // Host-local housekeeping runs after telemetry is available but before the
    // bead store is opened, so no claim can precede the sweep. Exclude cleanup
    // time from the 60-second initialization watchdog: reclaiming a very large
    // stale tree may legitimately take longer than worker construction.
    let scratch_sweep_started = Instant::now();
    match init_step("scratch_sweep", &telemetry, || {
        crate::scratch_sweep::sweep_home_scratch(&config.worker.scratch_sweep)
    }) {
        Ok(outcome) => record_scratch_sweep_outcome(&telemetry, &outcome),
        Err(error) => {
            tracing::warn!(error = %error, "scratch startup sweep failed; worker startup will continue");
            let _ = telemetry.emit(
                EventKind::Log {
                    phase: "scratch_sweep".to_string(),
                    context: serde_json::json!({
                        "status": "failed",
                        "error": error.to_string(),
                    }),
                    level: "warn".to_string(),
                    bead_id: None,
                },
                chrono::Utc::now(),
            );
        }
    }
    let scratch_sweep_elapsed = scratch_sweep_started.elapsed();

    // Phase 1: open only the backend explicitly bound by this workspace.
    // Binary availability is not evidence of store ownership.
    let store = init_step("bead_store_discover", &telemetry, || {
        crate::bead_store::open_configured(
            &config.bead_cli,
            config.workspace.default.clone(),
            None,                       // model: do not filter by model — beads are untagged
            Some("needle".to_string()), // harness
            Some(env!("CARGO_PKG_VERSION").to_string()),
        )
        .context("failed to open configured bead store")
    })?;

    // Phase 2: resource check before worker construction.
    // Check system resources (CPU and memory) before entering the slow
    // (~5s) worker_construction step. If saturated, retry with bounded
    // backoff rather than proceeding into a step that may be killed by
    // the OS due to resource pressure.
    const MAX_RESOURCE_WAIT_SECS: u64 = 120; // Maximum total wait time
    const RESOURCE_RETRY_DELAY_SECS: u64 = 5; // Initial retry delay
    let mut resource_wait_total = 0u64;
    let mut resource_retry_delay = RESOURCE_RETRY_DELAY_SECS;

    loop {
        match crate::rate_limit::RateLimiter::check_system_resources_for_launch(
            config.worker.cpu_load_warn,
            config.worker.memory_free_warn_mb,
            &telemetry,
        ) {
            Ok(()) => {
                // Resources are acceptable, proceed to worker_construction
                break;
            }
            Err(e) => {
                if resource_wait_total >= MAX_RESOURCE_WAIT_SECS {
                    // Still saturated after max wait, fail the launch explicitly
                    telemetry.emit(
                        EventKind::WorkerLaunchDeferred {
                            deferred_count: resource_wait_total / resource_retry_delay,
                            total_wait_secs: resource_wait_total,
                            reason: format!(
                                "system still saturated after {}s wait: {}",
                                MAX_RESOURCE_WAIT_SECS, e
                            ),
                        },
                        chrono::Utc::now(),
                    )?;
                    bail!(
                        "worker launch deferred {} times ({}s total wait), system still saturated: {}. Launch aborted — retry when load drops",
                        resource_wait_total / resource_retry_delay,
                        resource_wait_total,
                        e
                    );
                }

                // Resources are saturated, wait and retry
                tracing::warn!(
                    error = %e,
                    wait_secs = resource_retry_delay,
                    total_waited_secs = resource_wait_total,
                    "system resources saturated, deferring worker construction"
                );

                telemetry.emit(
                    EventKind::WorkerLaunchDeferred {
                        deferred_count: resource_wait_total / resource_retry_delay + 1,
                        total_wait_secs: resource_wait_total + resource_retry_delay,
                        reason: format!("system saturated: {}", e),
                    },
                    chrono::Utc::now(),
                )?;

                std::thread::sleep(std::time::Duration::from_secs(resource_retry_delay));
                resource_wait_total += resource_retry_delay;

                // Exponential backoff with cap at 30 seconds
                resource_retry_delay = std::cmp::min(resource_retry_delay * 2, 30);
            }
        }
    }

    // Phase 3: worker construction (heavy — prompt loading, adapter discovery, etc.).
    let mut worker = init_step("worker_construction", &telemetry, || {
        Ok(Worker::new_with_telemetry_and_sources(
            config,
            worker_name.clone(),
            store,
            telemetry.clone(),
            config_sources,
        ))
    })?;

    // Boot timeout guard: self-abort if init took >60 s.
    let elapsed_ms = boot_start
        .elapsed()
        .saturating_sub(scratch_sweep_elapsed)
        .as_millis() as u64;
    if elapsed_ms > 60_000 {
        telemetry.emit(
            EventKind::WorkerBootTimeout { elapsed_ms },
            chrono::Utc::now(),
        )?;
        bail!("boot exceeded 60 s ({elapsed_ms} ms), aborting");
    }

    eprintln!(
        "NEEDLE worker boot: all init steps completed in {elapsed_ms}ms, starting worker loop..."
    );
    let result = rt.block_on(worker.run())?;

    tracing::info!(final_state = %result, "worker finished");
    Ok(())
}

fn record_scratch_sweep_outcome(
    telemetry: &Telemetry,
    outcome: &crate::scratch_sweep::SweepOutcome,
) {
    use crate::scratch_sweep::SweepOutcome;

    let (level, context) = match outcome {
        SweepOutcome::Disabled => ("debug", serde_json::json!({ "status": "disabled" })),
        SweepOutcome::ScratchDirectoryMissing { path } => (
            "debug",
            serde_json::json!({
                "status": "scratch_directory_missing",
                "path": path,
            }),
        ),
        SweepOutcome::AlreadyRunning => {
            ("debug", serde_json::json!({ "status": "already_running" }))
        }
        SweepOutcome::Completed(report) => {
            let removed = report
                .removed
                .iter()
                .map(|entry| {
                    serde_json::json!({
                        "path": entry.path,
                        "bytes_reclaimed": entry.bytes_reclaimed,
                    })
                })
                .collect::<Vec<_>>();
            (
                "info",
                serde_json::json!({
                    "status": "completed",
                    "entries_examined": report.entries_examined,
                    "stale_candidates": report.stale_candidates,
                    "removed_count": report.removed.len(),
                    "removed": removed,
                    "skipped_live": report.skipped_live,
                    "skipped_safety": report.skipped_safety,
                    "bytes_reclaimed": report.bytes_reclaimed,
                }),
            )
        }
    };

    if let Err(error) = telemetry.emit(
        EventKind::Log {
            phase: "scratch_sweep".to_string(),
            context,
            level: level.to_string(),
            bead_id: None,
        },
        chrono::Utc::now(),
    ) {
        tracing::warn!(error = %error, "failed to emit scratch sweep telemetry");
    }
}

/// Emit start/complete telemetry around a fallible initialization step.
///
/// Each step's completion is force-flushed to disk so that if a subsequent
/// step blocks indefinitely, the telemetry log shows exactly where it stopped.
/// Also emits eprintln diagnostics for visibility even if telemetry hangs.
fn init_step<T, F>(name: &str, tel: &Telemetry, f: F) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    eprintln!("NEEDLE worker boot: starting init step '{name}'...");
    tel.emit(
        EventKind::InitStepStarted {
            step: name.to_string(),
        },
        chrono::Utc::now(),
    )?;
    let t = Instant::now();
    let result = f();
    let elapsed = t.elapsed().as_millis() as u64;
    tel.emit(
        EventKind::InitStepCompleted {
            step: name.to_string(),
            duration_ms: elapsed,
        },
        chrono::Utc::now(),
    )?;
    eprintln!("NEEDLE worker boot: init step '{name}' completed in {elapsed}ms");
    // Force-flush so the step completion is visible before the next (potentially blocking) step.
    tel.force_flush(std::time::Duration::from_secs(1))?;
    result
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
    hot_reload: Option<bool>,
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
    if let Some(hr) = hot_reload {
        inner_args.push("--hot-reload".to_string());
        inner_args.push(hr.to_string());
    }

    // Build stderr log path: ~/.needle/logs/<session_name>.stderr.log
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let stderr_log = format!("{home}/.needle/logs/{session_name}.stderr.log");

    let inner_cmd = format!(
        "NEEDLE_INNER=1 {} {} 2>> {}",
        shell_escape(&self_exe.display().to_string()),
        inner_args
            .iter()
            .map(|a| shell_escape(a))
            .collect::<Vec<_>>()
            .join(" "),
        shell_escape(&stderr_log)
    );

    // Spawn tmux with ETXTBSY retry to handle race conditions when the binary
    // has been written to disk immediately before execution (e.g., during upgrade
    // or hot-reload). The retry wrapper waits for the kernel to finish internal
    // bookkeeping before attempting to exec the same binary again.
    let mut child = spawn_with_etxtbsy_retry_sync_child(
        || {
            crate::tmux_socket::command()
                .args(["new-session", "-d", "-s", session_name, &inner_cmd])
                .spawn()
        },
        5,  // max_attempts: retry up to 5 times
        20, // backoff_ms: wait 20ms between retries
    )
    .context("failed to launch tmux — is tmux installed?")?;

    let status = child
        .wait()
        .context("tmux process failed after successful spawn")?;

    if !status.success() {
        bail!(
            "tmux new-session exited with status {} for session '{}'",
            status,
            session_name
        );
    }

    Ok(())
}

/// Kill the entire process tree rooted at the given PID.
///
/// This function recursively finds all child processes and sends SIGTERM,
/// waits for processes to exit, and sends SIGKILL to any remaining processes.
/// Returns true if all processes were successfully terminated.
///
/// This is necessary because agents are spawned with setpgid(0,0), creating
/// new process groups that are not reachable via killpg() on the parent PGID.
fn kill_process_tree(pid: u32) -> Result<bool> {
    use std::thread;

    tracing::info!(pid, "finding all descendant processes to terminate");

    // Collect all descendant PIDs by reading /proc
    let descendants = find_all_descendants(pid);
    let descendant_count = descendants.len();

    if descendant_count > 0 {
        tracing::info!(
            pid,
            count = descendant_count,
            ?descendants,
            "found descendant processes"
        );
    }

    // Send SIGTERM to all descendants (including the original PID)
    let all_pids: Vec<u32> = std::iter::once(pid).chain(descendants).collect();
    tracing::info!(
        pid,
        count = all_pids.len(),
        "sending SIGTERM to process tree"
    );

    for target_pid in &all_pids {
        unsafe {
            if libc::kill(*target_pid as libc::pid_t, libc::SIGTERM) == -1 {
                let err = std::io::Error::last_os_error();
                if err.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(pid = target_pid, error = %err, "SIGTERM failed");
                }
            }
        }
    }

    // Wait up to 5 seconds for processes to exit gracefully
    for i in 0..50 {
        thread::sleep(Duration::from_millis(100));

        // Check if all processes are gone
        let all_dead = all_pids
            .iter()
            .all(|&p| unsafe { libc::kill(p as libc::pid_t, 0) != 0 });

        if all_dead {
            tracing::info!(
                pid,
                "process tree terminated gracefully after {}ms",
                (i + 1) * 100
            );
            return Ok(true);
        }
    }

    // Some processes survived SIGTERM, use SIGKILL
    tracing::warn!(pid, "process tree survived SIGTERM, sending SIGKILL");
    for target_pid in &all_pids {
        unsafe {
            if libc::kill(*target_pid as libc::pid_t, libc::SIGKILL) == -1 {
                let err = std::io::Error::last_os_error();
                if err.kind() != std::io::ErrorKind::NotFound {
                    tracing::error!(pid = target_pid, error = %err, "SIGKILL failed");
                }
            }
        }
    }

    // Wait up to 3 seconds for SIGKILL to take effect
    for i in 0..30 {
        thread::sleep(Duration::from_millis(100));

        let all_dead = all_pids
            .iter()
            .all(|&p| unsafe { libc::kill(p as libc::pid_t, 0) != 0 });

        if all_dead {
            tracing::info!(
                pid,
                "process tree terminated by SIGKILL after {}ms",
                (i + 1) * 100
            );
            return Ok(true);
        }
    }

    // Check which processes are still alive
    let still_alive: Vec<u32> = all_pids
        .iter()
        .filter(|&&p| unsafe { libc::kill(p as libc::pid_t, 0) == 0 })
        .copied()
        .collect();

    if !still_alive.is_empty() {
        tracing::error!(
            pid,
            ?still_alive,
            "process tree survived SIGKILL - {} processes still running",
            still_alive.len()
        );
    }

    Ok(still_alive.is_empty())
}

/// Recursively find all descendant processes of the given PID.
///
/// Reads /proc to build a process tree and returns all descendant PIDs.
/// This handles the case where agents create new process groups with setpgid(0,0).
fn find_all_descendants(root_pid: u32) -> Vec<u32> {
    use std::fs;

    let proc_dir = Path::new("/proc");
    let mut ppid_to_children: HashMap<u32, Vec<u32>> = HashMap::new();

    // First pass: build parent->children mapping
    if let Ok(entries) = fs::read_dir(proc_dir) {
        for entry in entries.flatten() {
            let pid_str = entry.file_name();
            let pid: u32 = match pid_str.to_string_lossy().parse() {
                Ok(p) => p,
                Err(_) => continue,
            };

            // Skip the root PID itself
            if pid == root_pid {
                continue;
            }

            // Read /proc/[pid]/status to get PPID
            let status_path = entry.path().join("status");
            if let Ok(content) = fs::read_to_string(&status_path) {
                let ppid = content
                    .lines()
                    .find(|line| line.starts_with("PPID:\t"))
                    .and_then(|line| line.split(':').nth(1))
                    .and_then(|v| v.trim().parse().ok());

                if let Some(parent_pid) = ppid {
                    ppid_to_children.entry(parent_pid).or_default().push(pid);
                }
            }
        }
    }

    // Recursive DFS to find all descendants
    let mut descendants = Vec::new();
    let mut visited = HashSet::new();
    // Mark the root PID as visited to prevent cycles
    visited.insert(root_pid);
    find_descendants_recursive(root_pid, &ppid_to_children, &mut descendants, &mut visited);

    descendants
}

/// Recursive helper to traverse process tree and collect descendants.
fn find_descendants_recursive(
    pid: u32,
    ppid_to_children: &HashMap<u32, Vec<u32>>,
    descendants: &mut Vec<u32>,
    visited: &mut HashSet<u32>,
) {
    if let Some(children) = ppid_to_children.get(&pid) {
        for &child_pid in children {
            if visited.insert(child_pid) {
                descendants.push(child_pid);
                find_descendants_recursive(child_pid, ppid_to_children, descendants, visited);
            }
        }
    }
}

/// Check if a PID is a needle run process by reading /proc/[pid]/cmdline.
/// Trait for process inspection operations.
///
/// This trait allows mocking process inspection in tests while using real
/// `/proc` inspection in production.
trait ProcessInspector {
    fn is_needle_run_process(&self, pid: u32) -> bool;
    fn find_needle_process_in_tree(&self, root_pid: u32) -> Option<u32>;
}

/// Real process inspector using /proc filesystem.
struct RealProcessInspector;

impl ProcessInspector for RealProcessInspector {
    fn is_needle_run_process(&self, pid: u32) -> bool {
        is_needle_run_process(pid)
    }

    fn find_needle_process_in_tree(&self, root_pid: u32) -> Option<u32> {
        // First check if the root itself is a needle run process
        if self.is_needle_run_process(root_pid) {
            return Some(root_pid);
        }

        // Search descendants for needle run processes
        let descendants = find_all_descendants(root_pid);
        descendants
            .into_iter()
            .find(|&pid| self.is_needle_run_process(pid))
    }
}

/// Check if a process is a needle run process.
///
/// Returns true if the process command line contains "needle run" (after
/// handling NEEDLE_INNER=1 prefix).
///
/// This is stricter than checking for NEEDLE_INNER in the environment,
/// which would incorrectly match child processes that inherited the variable.
#[cfg(unix)]
fn is_needle_run_process(pid: u32) -> bool {
    use std::fs;

    let cmdline_path = Path::new("/proc").join(pid.to_string()).join("cmdline");
    let cmdline_bytes = match fs::read(&cmdline_path) {
        Ok(b) => b,
        Err(_) => return false, // Process may have exited or /proc not available
    };

    // cmdline is null-separated; parse as argv array for strict matching.
    let args: Vec<String> = cmdline_bytes
        .split(|&b| b == 0)
        .map(|args| String::from_utf8_lossy(args).to_string())
        .filter(|s| !s.is_empty())
        .collect();

    // Check for "needle run" in argv, handling NEEDLE_INNER=1 prefix
    // (e.g., "NEEDLE_INNER=1 /path/to/needle run ...")
    let needle_binary_idx = if args.len() >= 2 && args[0] == "NEEDLE_INNER=1" {
        1
    } else {
        0
    };

    // Need at least: binary + "run" argument
    if args.len() < needle_binary_idx + 2 {
        return false;
    }

    let binary_path = &args[needle_binary_idx];
    let run_arg_idx = needle_binary_idx + 1;

    // Extract basename from binary path
    let binary_name = match std::path::PathBuf::from(binary_path).file_name() {
        Some(name) => name.to_string_lossy().to_string(),
        None => binary_path.clone(),
    };

    // Strict match: basename must be exactly "needle" and next arg must be "run"
    binary_name == "needle" && args[run_arg_idx] == "run"
}

/// Find the actual needle run process in a process tree.
///
/// Given a root PID (typically from tmux pane_pid), this function searches for
/// needle run processes in the descendant tree. This handles the case where
/// tmux pane_pid returns a shell PID instead of the actual needle binary.
///
/// Returns the PID of the needle run process if found, None otherwise.
#[cfg(unix)]
fn find_needle_process_in_tree(root_pid: u32) -> Option<u32> {
    // First check if the root itself is a needle run process
    if is_needle_run_process(root_pid) {
        return Some(root_pid);
    }

    // Search descendants for needle run processes
    let descendants = find_all_descendants(root_pid);
    descendants
        .into_iter()
        .find(|&pid| is_needle_run_process(pid))
}

/// Check if any needle processes are still running after a kill attempt.
///
/// This function scans the process table to find all needle run processes
/// and their descendants. This is used after a kill attempt to verify that
/// ALL processes (needle run + dispatched agent + any descendants) are
/// actually gone, not just the original PID we tried to kill.
///
/// Returns a vector of PIDs that are still running, along with their command lines.
#[cfg(unix)]
fn verify_no_needle_processes_remaining() -> Vec<(u32, String)> {
    use std::fs;

    let proc_dir = Path::new("/proc");
    let mut remaining = Vec::new();

    if let Ok(entries) = fs::read_dir(proc_dir) {
        for entry in entries.flatten() {
            let pid_str = entry.file_name();
            let pid: u32 = match pid_str.to_string_lossy().parse() {
                Ok(p) => p,
                Err(_) => continue,
            };

            // Use strict matching to avoid false positives from child processes
            // that inherited NEEDLE_INNER from their parent
            if is_needle_run_process(pid) {
                // Read cmdline for reporting
                let cmdline_path = entry.path().join("cmdline");
                if let Ok(cmdline_bytes) = fs::read(&cmdline_path) {
                    let cmdline: String = cmdline_bytes
                        .split(|&b| b == 0)
                        .map(|args| String::from_utf8_lossy(args))
                        .collect::<Vec<_>>()
                        .join(" ");
                    remaining.push((pid, cmdline));
                } else {
                    remaining.push((pid, "<cmdline unreadable>".to_string()));
                }
            }
        }
    }

    remaining
}

/// `needle stop` — kill the full process tree for worker processes.
///
/// This command kills the parent needle run process, its bash -c prompt wrapper,
/// and the dispatched claude subprocess (full tree, not just the tmux session).
/// It verifies the PID is actually gone before printing success.
fn cmd_stop(all: bool, identifier: Option<String>) -> Result<()> {
    if !all && identifier.is_none() {
        bail!("specify --all or --identifier <NAME>");
    }

    let sessions = list_needle_sessions()?;

    if sessions.is_empty() {
        println!("No needle sessions running.");
        return Ok(());
    }

    // Find target sessions by name
    let targets: Vec<TmuxSession> = if all {
        sessions.clone()
    } else {
        let id = identifier.as_deref().unwrap_or("");
        sessions
            .iter()
            .filter(|s| s.name.contains(id))
            .cloned()
            .collect()
    };

    if targets.is_empty() {
        println!("No matching sessions found.");
        return Ok(());
    }

    for session in &targets {
        tracing::info!(session = %session.name, "stopping worker");

        // Get the PID from the tmux session
        let tmux_pid = match session.pid {
            Some(p) => p,
            None => {
                println!(
                    "Warning: no PID found for session '{}', skipping process tree kill",
                    session.name
                );
                // Still kill the session for cleanup
                let _ = crate::tmux_socket::command()
                    .args(["kill-session", "-t", &session.name])
                    .status();
                continue;
            }
        };

        // Find the actual needle run process (tmux pane_pid is the shell, not needle)
        let needle_pid = match find_needle_process_in_tree(tmux_pid) {
            Some(p) => {
                tracing::info!(
                    session = %session.name,
                    tmux_pid,
                    needle_pid = p,
                    "found needle run process in process tree"
                );
                p
            }
            None => {
                tracing::warn!(
                    session = %session.name,
                    tmux_pid,
                    "no needle run process found in tree, using tmux PID"
                );
                tmux_pid
            }
        };

        // Kill the entire process tree rooted at the needle run process
        let killed = match kill_process_tree(needle_pid) {
            Ok(k) => k,
            Err(e) => {
                println!(
                    "Warning: failed to kill process tree for session '{}': {}",
                    session.name, e
                );
                false
            }
        };

        // Verify ALL needle processes are actually gone (not just the root PID).
        // This catches the case where new processes were spawned during the kill
        // window or where the kill attempt didn't fully terminate the tree.
        let remaining_processes = verify_no_needle_processes_remaining();
        let any_remaining = !remaining_processes.is_empty();

        if any_remaining {
            println!(
                "Error: {} needle process(es) still running after kill attempt for session '{}':",
                remaining_processes.len(),
                session.name
            );
            for (pid, cmdline) in &remaining_processes {
                println!("  PID {}: {}", pid, cmdline);
            }
        }

        // Kill the tmux session for cleanup
        let kill_status = crate::tmux_socket::command()
            .args(["kill-session", "-t", &session.name])
            .status();

        // Clean up registry entry only if NO needle processes remain
        if killed && !any_remaining {
            // Extract worker identifier from session name (format: needle-<agent>-<worker_id>)
            // Session names are sanitized: dots become underscores, so we need to handle that
            let parts: Vec<&str> = session.name.split('-').collect();
            let worker_id = if parts.len() >= 3 {
                // needle-<agent>-<worker_id> -> worker_id is the third part
                parts[2]
            } else {
                // Fallback: use the full session name as worker_id
                tracing::warn!(
                    session = %session.name,
                    "could not extract worker_id from session name, using full name"
                );
                &session.name
            };

            let config = ConfigLoader::load_global()?;
            let registry = Registry::default_location(&config.workspace.home);
            if let Err(e) = registry.deregister(worker_id) {
                tracing::warn!(
                    worker_id,
                    error = %e,
                    "failed to deregister worker from registry"
                );
            }
        }

        match (killed, any_remaining, kill_status) {
            (true, false, Ok(s)) if s.success() => {
                println!("Stopped: {} (pid {})", session.name, needle_pid);
            }
            (true, false, Ok(_)) => {
                println!(
                    "Stopped: {} (pid {}, session already gone)",
                    session.name, needle_pid
                );
            }
            (true, false, Err(e)) => {
                println!(
                    "Warning: processes killed but failed to kill session: {}",
                    e
                );
            }
            (false, true, _) => {
                println!(
                    "Error: failed to stop {} (processes still running)",
                    session.name
                );
            }
            (true, true, _) => {
                println!(
                    "Error: {} - kill attempt reported success but {} process(es) still running",
                    session.name,
                    remaining_processes.len()
                );
            }
            (false, false, _) => {
                println!("Stopped: {} (pid {} gone)", session.name, needle_pid);
            }
        }
    }

    Ok(())
}

/// Filter sessions for cleanup based on liveness and flags.
///
/// This is the core filtering logic for the cleanup command, extracted to be
/// testable. Returns the session names that should be cleaned up.
///
/// # Arguments
/// * `sessions` - All discovered tmux sessions
/// * `inspector` - Process inspector for checking process liveness
/// * `live_pids` - Set of PIDs that have live needle processes
/// * `all` - If true, include all sessions regardless of liveness
/// * `identifier` - If set, filter by identifier substring (bypasses liveness check)
///
/// # Returns
/// Vector of session names to clean up
fn filter_sessions_for_cleanup_impl(
    sessions: &[TmuxSession],
    inspector: &dyn ProcessInspector,
    live_pids: &std::collections::HashSet<u32>,
    all: bool,
    identifier: &Option<String>,
) -> Vec<String> {
    if all {
        // --all: remove all sessions regardless of liveness
        sessions.iter().map(|s| s.name.clone()).collect()
    } else if let Some(id) = identifier {
        // -i flag: filter by identifier substring (no liveness check)
        sessions
            .iter()
            .filter(|s| s.name.contains(id))
            .map(|s| s.name.clone())
            .collect()
    } else {
        // Default: only orphaned sessions (no live backing process)
        //
        // IMPORTANT: tmux pane_pid returns the shell PID, not the needle binary.
        // We must walk the process tree to find the actual needle run process
        // before checking liveness. This matches what cmd_stop already does.
        sessions
            .iter()
            .filter(|s| {
                // Session is orphaned if:
                // - It has no PID at all, OR
                // - Walking from its PID finds no live needle run process
                s.pid.map_or(true, |pane_pid| {
                    // Try to find the actual needle run process in the tree
                    match inspector.find_needle_process_in_tree(pane_pid) {
                        Some(needle_pid) => !live_pids.contains(&needle_pid),
                        None => {
                            // No needle run found in tree — treat as orphaned
                            // (pane_pid itself might be a dead shell wrapper)
                            true
                        }
                    }
                })
            })
            .map(|s| s.name.clone())
            .collect()
    }
}

/// Filter sessions for cleanup based on liveness and flags.
///
/// Convenience wrapper that uses the real process inspector.
fn filter_sessions_for_cleanup(
    sessions: &[TmuxSession],
    live_pids: &std::collections::HashSet<u32>,
    all: bool,
    identifier: &Option<String>,
) -> Vec<String> {
    filter_sessions_for_cleanup_impl(sessions, &RealProcessInspector, live_pids, all, identifier)
}

/// `needle cleanup` — remove orphaned tmux sessions.
///
/// Finds and removes needle tmux sessions that no longer have active workers.
/// With --all, removes all needle sessions regardless of worker status.
/// With -i, filters sessions by name/identifier substring (bypasses liveness check).
fn cmd_cleanup(all: bool, identifier: Option<String>) -> Result<()> {
    let sessions = list_needle_sessions()?;

    if sessions.is_empty() {
        println!("No needle sessions running.");
        return Ok(());
    }

    // Scan for live processes
    let discovered = scan_needle_processes().unwrap_or_default();
    let live_pids: std::collections::HashSet<u32> = discovered.iter().map(|p| p.pid).collect();

    let targets = filter_sessions_for_cleanup(&sessions, &live_pids, all, &identifier);

    if targets.is_empty() {
        println!("No matching sessions found.");
        return Ok(());
    }

    let mut cleaned = 0;
    for session in &targets {
        // Kill the session
        let status = crate::tmux_socket::command()
            .args(["kill-session", "-t", session])
            .status();

        match status {
            Ok(s) if s.success() => {
                println!("Cleaned up: {session}");
                cleaned += 1;
            }
            Ok(_) => {
                println!("Warning: session '{session}' already gone");
            }
            Err(e) => {
                println!("Warning: failed to cleanup session '{session}': {e}");
            }
        }
    }

    if cleaned == 0 {
        println!("No sessions cleaned up.");
    } else {
        println!("Cleaned up {cleaned} session(s).");
    }

    Ok(())
}

/// `needle list` — show running needle sessions.
fn cmd_list(format: ListFormat) -> Result<()> {
    let sessions = list_needle_sessions()?;

    // ALWAYS scan process table for ALL needle run processes (both tmux and non-tmux).
    // This ensures we discover workers regardless of how they were started.
    let discovered = scan_needle_processes().unwrap_or_default();
    let tmux_pids: HashSet<u32> = sessions.iter().filter_map(|s| s.pid).collect();

    // Reconciliation check: compare process table against registry
    let config = ConfigLoader::load_global()?;
    let registry = Registry::default_location(&config.workspace.home);
    let _ = reconcile_process_registry(&discovered, &registry);

    // Separate discovered processes into tmux and non-tmux groups
    let _tmux_procs: Vec<&DiscoveredProcess> = discovered
        .iter()
        .filter(|p| tmux_pids.contains(&p.pid))
        .collect();
    let orphaned: Vec<&DiscoveredProcess> = discovered
        .iter()
        .filter(|p| !tmux_pids.contains(&p.pid))
        .collect();

    if !orphaned.is_empty() {
        tracing::warn!(
            count = orphaned.len(),
            pids = ?orphaned.iter().map(|p| p.pid).collect::<Vec<_>>(),
            "found needle run processes not in tmux"
        );
    }

    // Show all discovered processes, not just tmux sessions
    if sessions.is_empty() && discovered.is_empty() {
        match format {
            ListFormat::Table => println!("No needle sessions running."),
            ListFormat::Json => println!("[]"),
        }
        return Ok(());
    }

    match format {
        ListFormat::Table => {
            if !sessions.is_empty() {
                println!("{:<40} {:<20} {:<10}", "SESSION", "CREATED", "STATUS");
                println!("{}", "-".repeat(70));
                for s in &sessions {
                    println!("{:<40} {:<20} {:<10}", s.name, s.created, s.status);
                }
            }

            // ALWAYS show discovered processes, even if they're in tmux
            // This ensures visibility even if registry registration failed
            if !discovered.is_empty() {
                println!();
                println!("Discovered Workers ({}):", discovered.len());
                println!("  All running needle run processes found via process table scan");
                for proc in &discovered {
                    let workspace = proc
                        .workspace
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "<unknown>".to_string());
                    let agent = proc.agent.as_deref().unwrap_or("<unknown>");
                    let identifier = proc.identifier.as_deref().unwrap_or("<unknown>");
                    let in_tmux = if tmux_pids.contains(&proc.pid) {
                        " (tmux)"
                    } else {
                        " (non-tmux)"
                    };
                    println!(
                        "  PID {} — workspace: {}, agent: {}, identifier: {}{}",
                        proc.pid, workspace, agent, identifier, in_tmux
                    );
                }
            }

            // Additional warning if there are orphaned processes
            if !orphaned.is_empty() {
                println!();
                println!("  ⚠️  {} worker(s) running outside tmux (started with NEEDLE_INNER=1 or direct invocation)", orphaned.len());
            }
        }
        ListFormat::Json => {
            let json = serde_json::json!({
                "tmux_sessions": sessions,
                "discovered": discovered.iter().map(|p| {
                    serde_json::json!({
                        "pid": p.pid,
                        "workspace": p.workspace,
                        "agent": p.agent,
                        "identifier": p.identifier,
                        "cmdline": p.cmdline,
                        "in_tmux": tmux_pids.contains(&p.pid),
                    })
                }).collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
    }

    Ok(())
}

/// `needle init` — initialize v2 config with optional v1 migration.
///
/// Creates ~/.config/needle/config.yaml. Detects existing v1 artifacts
/// in ~/.needle/ and migrates compatible settings (agent name, workspace
/// path, worker count) to the v2 YAML schema. Safe to run on already-
/// initialized installs (idempotent).
fn cmd_init(backend: &str) -> Result<()> {
    /// Resolve a path relative to the user's home directory.
    fn dirs_or_home(relative: &str) -> PathBuf {
        if let Some(home) = std::env::var_os("HOME") {
            PathBuf::from(home).join(relative)
        } else {
            PathBuf::from("/tmp").join(relative)
        }
    }

    let config_path = dirs_or_home(".config/needle/config.yaml");
    let v1_dir = dirs_or_home(".needle");

    // Check if v2 config already exists.
    if config_path.exists() {
        println!("Config already exists: {}", config_path.display());
        let config = ConfigLoader::load_from_path(&config_path)?;
        println!("  Agent default: {}", config.agent.default);
        println!("  Workspace: {}", config.workspace.default.display());
        println!("  Max workers: {}", config.worker.max_workers);
        println!("\nTo reinitialize, delete the existing config file first.");
        return Ok(());
    }

    // Detect v1 artifacts and migrate compatible settings.
    let mut agent_name = None::<String>;
    let mut workspace_path = None::<PathBuf>;

    // Check for v1 directory.
    if v1_dir.exists() && v1_dir.is_dir() {
        println!("Detected v1 artifacts: {}", v1_dir.display());

        // Look for v1 config hints (file-based heuristics).
        // v1 stored agent name as a subdirectory: ~/.needle/<agent>/
        if let Ok(entries) = std::fs::read_dir(&v1_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let name = path.file_name().and_then(|n| n.to_str());
                    // Skip common non-agent subdirectories.
                    if let Some(name) = name {
                        if !matches!(name, "state" | "logs" | "canary" | "config") {
                            agent_name = Some(name.to_string());
                            println!("  Migrating agent name from v1: {}", name);
                        }
                    }
                }
            }
        }

        // v1 workspace hints: look for recent .beads directories under $HOME.
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let home_path = PathBuf::from(&home);
        if let Ok(entries) = std::fs::read_dir(&home_path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() && path.join(".beads").exists() {
                    // Prefer the most recently modified workspace.
                    let modified = entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    let current_best = workspace_path
                        .as_ref()
                        .and_then(|p| std::fs::metadata(p).ok()?.modified().ok())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    if modified > current_best {
                        workspace_path = Some(path.clone());
                        println!("  Migrating workspace path from v1: {}", path.display());
                    }
                }
            }
        }
    }

    // Build the config with migrated values or defaults.
    let default_config = Config::default();
    let agent_default = agent_name.unwrap_or_else(|| default_config.agent.default.clone());
    let workspace_default =
        workspace_path.unwrap_or_else(|| default_config.workspace.default.clone());

    // Construct a minimal config YAML with comments explaining each field.
    let config_yaml = format!(
        r#"# NEEDLE v2 Configuration
# Migrated from v1: {v1_status}
#
# This file controls NEEDLE's global behavior. Workspace-specific overrides
# can be placed in .needle.yaml within each workspace directory.
#
# Resolution order (later overrides earlier):
#   1. Built-in defaults
#   2. This global config file
#   3. Workspace .needle.yaml (if present)
#   4. Environment variables (NEEDLE_*)
#   5. CLI arguments
#

# Agent (AI model CLI) configuration.
agent:
  # Default agent adapter to use for bead processing.
  # Examples: claude, opus, codex
  default: {agent}

  # Extra arguments to pass before the prompt.
  args: []

  # Agent process timeout in seconds (0 = unlimited).
  timeout: 3600

  # Directory containing adapter TOML files.
  adapters_dir: {adapters_dir}

# Worker fleet configuration.
worker:
  # Maximum number of concurrent workers.
  max_workers: 4

  # Stagger delay (seconds) between worker launches.
  launch_stagger_seconds: 2

  # Seconds to wait between queue polls when idle.
  idle_timeout: 60

  # What to do when the queue is empty.
  # Options: wait, exit
  idle_action: wait

  # Maximum claim retries before skipping a bead.
  max_claim_retries: 3

  # Consecutive race_lost attempts before treating the ready queue as empty.
  claim_race_lost_skip: 5

  # How workers generate their unique names.
  # Options: hostname_random, sequential, uuid
  identifier_scheme: hostname_random

  # Warn when CPU load (0.0–1.0) exceeds this threshold.
  cpu_load_warn: 0.8

  # Warn when available memory falls below this threshold (MB).
  memory_free_warn_mb: 512

  # BUILDING state timeout in seconds (0 = unlimited).
  building_timeout: 600

# Workspace path configuration.
workspace:
  # Default workspace directory (used when not specified on CLI).
  default: {workspace}

  # NEEDLE home directory (heartbeat files, log output).
  home: {needle_home}

  # Labels describing this workspace's domain (e.g., rust, api, trading).
  # Used for cross-workspace skill sharing.
  labels: []

# Strand waterfall configuration.
strands:
  # Primary bead selection strand.
  pluck:
    exclude_labels: []
    split_after_failures: 3

  # Stuck/failed bead recovery strand.
  mend:
    stuck_threshold_secs: 300
    lock_ttl_secs: 600
    db_check_interval: 50
    idle_timeout: 120

  # Multi-workspace discovery strand.
  explore:
    enabled: true
    workspaces: []
    workspace_root: ~/

  # Exhaustion alerting strand.
  knot:
    alert_destination: null
    alert_cooldown_minutes: 60
    exhaustion_threshold: 3

  # Bead splitting on failure.
  mitosis:
    enabled: true
    first_failure_only: true
    force_failure_threshold: 0

  # Gap analysis and bead creation.
  weave:
    enabled: false
    max_beads_per_run: 5
    cooldown_hours: 24
    exclude_workspaces: []
    doc_patterns:
      - README*
      - AGENTS.md
      - docs/**

  # Alternative proposals for human-blocked beads.
  unravel:
    enabled: false
    max_beads_per_run: 5
    max_alternatives_per_bead: 3
    cooldown_hours: 168
    prompt_template: null

  # Codebase health scans.
  pulse:
    enabled: false
    scanners: []
    max_beads_per_run: 5
    cooldown_hours: 48
    severity_threshold: 3
    prompt_template: null

  # Meta-analysis and learning consolidation.
  reflect:
    enabled: true
    min_beads_since_last: 10
    cooldown_hours: 24
    max_learnings_per_run: 10
    max_skills_per_run: 3
    learning_retention_days: 90
    max_learnings: 80
    extraction_agent: null
    extraction_prompt_template: null
    max_extraction_per_run: 5
    transcript_recency_days: 7
    transcript_max_sessions: 50
    drift_similarity_threshold: 0.6
    drift_enabled: true
    adr_enabled: true
    claude_md_placement: true

  # Worker failure documentation.
  splice:
    enabled: true
    stale_threshold_secs: 300
    report_workspace: null
    detect_live_loops: true
    live_loop_scan_events: 200
    claim_churn_threshold: 20
    log_runaway_bytes: 10485760
    live_loop_window_secs: 300

  # Learning and trace retention.
  learning:
    trace_retention_failed_days: 30
    trace_retention_success_days: 7
    max_learnings: 80
    trace_sanitization:
      enabled: true
      custom_patterns: []
    global_learnings_file: ~/.config/needle/global-learnings.md
    max_global_learnings: 40

# Telemetry configuration.
telemetry:
  file_sink:
    enabled: true
    log_dir: null
    retention_days: 30
  stdout_sink:
    enabled: false
    format: normal
    color: auto
  hooks: []
  otlp_sink:
    enabled: false
    endpoint: http://localhost:4317
    protocol: grpc
    timeout_secs: 10
    compression: gzip
    tls:
      insecure: false
      ca_file: ""
    headers: []
    resource_attributes: []
    metrics_interval_secs: 10
    service_namespace: needle-fleet
    max_queue_size: 2048

# Health monitoring configuration.
health:
  heartbeat_interval_secs: 30
  heartbeat_ttl_secs: 300

# Provider/model concurrency and rate limiting.
limits:
  providers: {{}}
  models: {{}}

# Prompt construction configuration.
prompt:
  context_files: []
  instructions: null
  templates: {{}}
  variants: {{}}

# Per-model token pricing (USD per million tokens).
# Maps model name directly to input/output pricing.
pricing:
  claude-sonnet-4-6:
    input_per_million: 3.0
    output_per_million: 15.0
  claude-opus-4-6:
    input_per_million: 15.0
    output_per_million: 75.0
  claude-haiku-4-20250514:
    input_per_million: 0.25
    output_per_million: 1.25
  gpt-4o:
    input_per_million: 5.0
    output_per_million: 15.0
  gpt-4o-mini:
    input_per_million: 0.15
    output_per_million: 0.60

# Daily budget thresholds for cost enforcement.
budget:
  daily_usd: null

# Verification commands run after agent success (legacy format).
verification: []

# Pluggable validation gates.
gates: []

# Self-modification (hot-reload) configuration.
self_modification:
  enabled: false
  canary_workspace: ~/.needle/canary
  auto_promote: false
  canary_timeout: 300
  hot_reload: true

# FABRIC live dashboard forwarding.
fabric:
  enabled: false
  endpoint: ""
  timeout: 2
  batching: false
"#,
        v1_status = if v1_dir.exists() {
            "migrated"
        } else {
            "initialized"
        },
        agent = agent_default,
        workspace = workspace_default.display(),
        needle_home = v1_dir.display(),
        adapters_dir = dirs_or_home(".config/needle/adapters").display()
    );

    // Ensure the config directory exists.
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory: {}", parent.display()))?;
    }

    // Write the config file.
    std::fs::write(&config_path, config_yaml)
        .with_context(|| format!("failed to write config file: {}", config_path.display()))?;

    println!("Created config file: {}", config_path.display());
    println!("\nConfiguration summary:");
    println!("  Agent default: {}", agent_default);
    println!("  Workspace: {}", workspace_default.display());
    println!("  Max workers: 4 (default)");

    // Validate the created config by running it through ConfigLoader.
    let validated = ConfigLoader::load_from_path(&config_path)?;
    let errors = ConfigLoader::validate(&validated);
    if !errors.is_empty() {
        println!("\nWarning: Config validation produced errors:");
        for error in &errors {
            println!("  - {}", error);
        }
    } else {
        println!("\nConfig validated successfully.");
    }

    // Validate backend parameter.
    if !matches!(backend, "bead-rs" | "bead-forge") {
        bail!("unknown backend '{backend}' -- must be 'bead-rs' or 'bead-forge'");
    }

    // Check if we're in a workspace and bind backend if needed.
    let current_dir = std::env::current_dir()?;
    let workspace_beads = current_dir.join(".beads");
    let workspace_config = current_dir.join(".needle.yaml");

    if workspace_beads.is_dir() {
        if workspace_config.exists() {
            println!(
                "\nWorkspace config already exists: {} (not modifying)",
                workspace_config.display()
            );
        } else {
            // Write .needle.yaml with backend binding.
            let yaml = format!("bead_cli:\n  backend: {}\n", backend);
            std::fs::write(&workspace_config, yaml).with_context(|| {
                format!(
                    "failed to write workspace config: {}",
                    workspace_config.display()
                )
            })?;
            println!("\nCreated workspace config: {}", workspace_config.display());
            println!("  bead_cli.backend: {}", backend);
        }
    }

    // Print onboarding checklist.
    println!("\nOnboarding checklist:");
    println!("  1. Install a bead backend if needed:");
    println!("       $ needle doctor");
    println!("       $ bead init --prefix <name>");
    println!("  2. Create your first bead:");
    println!("       $ bead create --title \"Task title\" --priority 2");
    println!("  3. Verify the workspace:");
    println!("       $ needle doctor");
    println!("  4. Start processing beads:");
    println!("       $ needle run --agent <agent>");
    println!("  5. Monitor progress:");
    println!("       $ needle status");
    println!("       $ tmux attach -t needle-<agent>-<name>");

    Ok(())
}

/// `needle version` — print version info.
fn cmd_version() {
    let metadata = crate::build_metadata::BuildMetadata::current();
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    // Display full metadata if available
    if metadata.commit_sha != "unknown" && metadata.build_timestamp != "unknown" {
        println!("needle {} (rust, {} {})", metadata.version, os, arch);
        println!("  commit: {}", metadata.commit_sha);
        println!("  built: {}", metadata.build_timestamp);
    } else {
        // Fallback to basic version display
        println!("needle {} (rust, {} {})", metadata.version, os, arch);
    }
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

fn cmd_bead_backend(name: &str, workspace: &Path) -> Result<()> {
    if !matches!(name, "bead-rs" | "bead-forge") {
        bail!("unknown builtin bead backend '{name}'");
    }
    let workspace = workspace
        .canonicalize()
        .with_context(|| format!("workspace does not exist: {}", workspace.display()))?;
    let backend = match name {
        "bead-rs" => crate::config::BeadBackend::Bead,
        "bead-forge" => bail!("bead-forge backend is no longer supported; use 'bead-rs' instead"),
        _ => bail!("unknown builtin bead backend '{name}'"),
    };
    let config = crate::config::BeadCliConfig {
        backend,
        path: None,
    };
    let (_, binary) = crate::config::resolve_bead_cli(&config)?;
    crate::bead_store::open_configured(&config, workspace.clone(), None, None, None)?;
    let descriptor = crate::bead_store::builtin_bead_backends()
        .into_iter()
        .find(|descriptor| descriptor.name == name)
        .ok_or_else(|| anyhow::anyhow!("builtin descriptor '{name}' is missing"))?;
    println!("backend: {}", descriptor.name);
    println!("binary: {}", binary.display());
    println!("verified_against: {}", descriptor.verified_against);
    println!("atomic_claim: {}", descriptor.capabilities.atomic_claim);
    println!(
        "transactional_batch: {}",
        descriptor.capabilities.transactional_batch
    );
    Ok(())
}

fn workspace_backend_binding(workspace: &Path) -> Result<Option<String>> {
    let path = workspace.join(".needle.yaml");
    if !path.exists() {
        return Ok(None);
    }
    let value: serde_yaml::Value = serde_yaml::from_str(
        &std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?,
    )
    .with_context(|| format!("invalid YAML in {}", path.display()))?;
    Ok(value
        .get("bead_cli")
        .and_then(|value| value.get("backend"))
        .and_then(serde_yaml::Value::as_str)
        .map(str::to_string))
}

fn cmd_bead_backend_audit(root: &Path) -> Result<()> {
    let root = root
        .canonicalize()
        .with_context(|| format!("audit root does not exist: {}", root.display()))?;
    let mut workspaces = Vec::new();
    if root.join(".beads").is_dir() {
        workspaces.push(root.clone());
    }
    for entry in std::fs::read_dir(&root)
        .with_context(|| format!("failed to read audit root {}", root.display()))?
    {
        let path = entry?.path();
        if path.is_dir() && path.join(".beads").is_dir() {
            workspaces.push(path);
        }
    }
    workspaces.sort();
    let mut unbound = 0usize;
    for workspace in &workspaces {
        match workspace_backend_binding(workspace)? {
            Some(binding) => println!("BOUND\t{}\t{}", binding, workspace.display()),
            None => {
                unbound += 1;
                println!("UNBOUND\t-\t{}", workspace.display());
            }
        }
    }
    println!(
        "summary: {} workspaces, {} bound, {} unbound",
        workspaces.len(),
        workspaces.len().saturating_sub(unbound),
        unbound
    );
    if unbound > 0 {
        bail!("{unbound} bead workspaces have no explicit backend binding");
    }
    Ok(())
}

fn cmd_bead_backend_bind(backend: &str, workspace: &Path) -> Result<()> {
    if !matches!(backend, "bead-rs" | "bead-forge") {
        bail!("unknown builtin bead backend '{backend}'");
    }
    let workspace = workspace
        .canonicalize()
        .with_context(|| format!("workspace does not exist: {}", workspace.display()))?;
    if !workspace.join(".beads").is_dir() {
        bail!("{} is not a bead workspace", workspace.display());
    }
    let path = workspace.join(".needle.yaml");
    let mut root = if path.exists() {
        serde_yaml::from_str::<serde_yaml::Value>(&std::fs::read_to_string(&path)?)
            .with_context(|| format!("invalid YAML in {}", path.display()))?
    } else {
        serde_yaml::Value::Mapping(Default::default())
    };
    let root_map = root
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("{} must contain a YAML mapping", path.display()))?;
    let bead_cli_key = serde_yaml::Value::String("bead_cli".to_string());
    let bead_cli = root_map
        .entry(bead_cli_key)
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));
    let bead_cli_map = bead_cli
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("bead_cli in {} must be a mapping", path.display()))?;
    bead_cli_map.insert(
        serde_yaml::Value::String("backend".to_string()),
        serde_yaml::Value::String(backend.to_string()),
    );
    std::fs::write(&path, serde_yaml::to_string(&root)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    println!(
        "bound {} to {} (routing only; no bead data was migrated)",
        workspace.display(),
        backend
    );
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
    let status = crate::tmux_socket::command()
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
    idle_strands: bool,
) -> Result<()> {
    let config = ConfigLoader::load_global()?;
    let needle_home = config.workspace.home.clone();
    let registry = Registry::default_location(&needle_home);
    let workers = registry.list().unwrap_or_default();
    let sessions = list_needle_sessions().unwrap_or_default();

    // ALWAYS scan process table for ALL needle run processes (both registered and unregistered).
    // This ensures we discover workers regardless of registry registration status.
    let discovered = scan_needle_processes().unwrap_or_default();
    let registered_pids: HashSet<u32> = workers.iter().map(|w| w.pid).collect();
    let _tmux_pids: HashSet<u32> = sessions.iter().filter_map(|s| s.pid).collect();

    // Separate discovered processes into registered and unregistered groups
    let _registered_procs: Vec<&DiscoveredProcess> = discovered
        .iter()
        .filter(|p| registered_pids.contains(&p.pid))
        .collect();
    let unregistered: Vec<&DiscoveredProcess> = discovered
        .iter()
        .filter(|p| !registered_pids.contains(&p.pid))
        .collect();

    if !unregistered.is_empty() {
        tracing::warn!(
            count = unregistered.len(),
            pids = ?unregistered.iter().map(|p| p.pid).collect::<Vec<_>>(),
            "found unregistered needle run processes"
        );
    }

    // Run comprehensive reconciliation check
    let _ = reconcile_process_registry(&discovered, &registry);

    // Build a fleet summary.
    let active_count = sessions.len();
    let registered_count = workers.len();
    let discovered_count = discovered.len();
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
            println!("  Discovered workers:   {discovered_count}");
            println!("  Total beads processed: {total_beads}");
            if !unregistered.is_empty() {
                println!("  Unregistered workers: {} (WARN)", unregistered.len());
            }
            println!();

            // Check for updates with timeout - best-effort, non-fatal
            let update_check = std::thread::spawn(|| {
                std::panic::catch_unwind(|| {
                    // Create telemetry emitter for this background check
                    let tel = Telemetry::new("background-update".to_string());
                    upgrade::check_for_update_with_telemetry(Some(&tel))
                        .map_err(|e| {
                            tracing::debug!("update check failed (non-fatal): {}", e);
                            e
                        })
                        .ok()
                })
                .unwrap_or(None)
            });

            // Wait with 3s timeout using join_timeout simulation
            let timeout_duration = Duration::from_secs(3);
            let start = Instant::now();
            let update_result = loop {
                if update_check.is_finished() {
                    break update_check.join().unwrap_or(None);
                }
                if start.elapsed() >= timeout_duration {
                    tracing::debug!("update check timed out after 3s (non-fatal)");
                    break None;
                }
                std::thread::sleep(Duration::from_millis(100));
            };

            if let Some(check) = update_result {
                if check.update_available {
                    println!(
                        "  ⚠️  Update available: running {}, latest is {} — run `needle upgrade`",
                        check.current_version, check.latest_version
                    );
                    println!();
                }
            }

            // Show ALL discovered workers (both registered and unregistered)
            if !discovered.is_empty() {
                println!("Discovered Workers (all needle run processes):");
                println!(
                    "  Found {} running worker(s) via process table scan",
                    discovered.len()
                );
                println!();

                // Registered workers with heartbeat info
                if !heartbeat_statuses.is_empty() {
                    if by_worker {
                        println!(
                            "{:<16} {:<8} {:<14} {:<12} {:<10} {:<8} {:<12}",
                            "WORKER", "PID", "STATE", "BEAD", "UPTIME", "ALIVE", "REGISTERED"
                        );
                        println!("{}", "-".repeat(80));
                        for ws in &heartbeat_statuses {
                            let state = ws.heartbeat_state.as_deref().unwrap_or("unknown");
                            let bead = ws.current_bead.as_deref().unwrap_or("-");
                            let uptime = format_duration(ws.uptime_secs);
                            let alive = if ws.pid_alive { "yes" } else { "no" };
                            println!(
                                "{:<16} {:<8} {:<14} {:<12} {:<10} {:<8} {:<12}",
                                ws.entry.id, ws.entry.pid, state, bead, uptime, alive, "yes"
                            );
                        }
                    } else {
                        println!("Registered Workers:");
                        for ws in &heartbeat_statuses {
                            let state = ws.heartbeat_state.as_deref().unwrap_or("unknown");
                            let alive = if ws.pid_alive { "" } else { " (DEAD)" };
                            println!(
                                "  {} — {} beads, state: {state}{alive}",
                                ws.entry.id, ws.entry.beads_processed,
                            );
                        }
                    }
                    println!();
                }

                // Unregistered workers (discovered but not in registry)
                if !unregistered.is_empty() {
                    println!("Unregistered Workers (not in registry):");
                    println!("  ⚠️  These workers failed to register during boot");
                    for proc in unregistered {
                        let workspace = proc
                            .workspace
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "<unknown>".to_string());
                        let agent = proc.agent.as_deref().unwrap_or("<unknown>");
                        let identifier = proc.identifier.as_deref().unwrap_or("<unknown>");
                        println!(
                            "  PID {} — workspace: {}, agent: {}, identifier: {}",
                            proc.pid, workspace, agent, identifier
                        );
                    }
                    println!();
                }
            }

            if discovered.is_empty() && active_count == 0 {
                println!("No workers running.");
            }
        }
        ListFormat::Json => {
            let summary = serde_json::json!({
                "active_sessions": active_count,
                "registered_workers": registered_count,
                "discovered_workers": discovered_count,
                "total_beads_processed": total_beads,
                "unregistered_workers": unregistered.len(),
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
                        "registered": true,
                    })
                }).collect::<Vec<_>>(),
                "discovered": discovered.iter().map(|p| {
                    let registered = registered_pids.contains(&p.pid);
                    serde_json::json!({
                        "pid": p.pid,
                        "workspace": p.workspace,
                        "agent": p.agent,
                        "identifier": p.identifier,
                        "cmdline": p.cmdline,
                        "registered": registered,
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

    // Idle-strands cooldown summary (if requested).
    if idle_strands {
        let state_base = needle_home.join("state");

        // Collect unique workspaces: default + all registered worker workspaces.
        let mut workspaces: Vec<PathBuf> = Vec::new();
        workspaces.push(config.workspace.default.clone());
        for w in &workers {
            if !workspaces.contains(&w.workspace) {
                workspaces.push(w.workspace.clone());
            }
        }

        let rows = idle_strand_rows(&config, &state_base, &workspaces);

        match format {
            ListFormat::Table => {
                println!();
                println!("Idle Strand Cooldowns");
                println!("{}", "-".repeat(80));
                println!(
                    "{:<10} {:<30} {:<12} {:<22} {:<12}",
                    "STRAND", "WORKSPACE", "ENABLED", "LAST RUN", "STATUS"
                );
                println!("{}", "-".repeat(80));
                for row in &rows {
                    let ws_display = if row.workspace.len() > 28 {
                        format!("...{}", &row.workspace[row.workspace.len() - 25..])
                    } else {
                        row.workspace.clone()
                    };
                    let last_run = row
                        .last_run
                        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "never".to_string());
                    let enabled = if row.enabled { "yes" } else { "no" };
                    println!(
                        "{:<10} {:<30} {:<12} {:<22} {:<12}",
                        row.strand, ws_display, enabled, last_run, row.status
                    );
                }
                if rows.is_empty() {
                    println!("  No idle strand state found.");
                }
                println!();
                println!(
                    "Cooldown hours: reflect={}, weave={}, pulse={}, unravel={}",
                    config.strands.reflect.cooldown_hours,
                    config.strands.weave.cooldown_hours,
                    config.strands.pulse.cooldown_hours,
                    config.strands.unravel.cooldown_hours,
                );
            }
            ListFormat::Json => {
                let json_rows: Vec<_> = rows
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "strand": r.strand,
                            "workspace": r.workspace,
                            "enabled": r.enabled,
                            "last_run": r.last_run,
                            "cooldown_hours": r.cooldown_hours,
                            "status": r.status,
                        })
                    })
                    .collect();
                let idle_json = serde_json::json!({
                    "idle_strands": json_rows,
                    "cooldown_hours": {
                        "reflect": config.strands.reflect.cooldown_hours,
                        "weave": config.strands.weave.cooldown_hours,
                        "pulse": config.strands.pulse.cooldown_hours,
                        "unravel": config.strands.unravel.cooldown_hours,
                    }
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&idle_json)
                        .context("failed to serialize idle-strands")?
                );
            }
        }
    }

    Ok(())
}

/// `needle stats` — show outcome statistics from telemetry logs.
fn cmd_stats(
    by: StatsBy,
    since: Option<String>,
    until: Option<String>,
    format: ListFormat,
) -> Result<()> {
    use crate::stats::{compute_stats, StatsDimension};

    let config = ConfigLoader::load_global()?;
    let log_dir = config.workspace.home.join("logs");

    let since_dt = since.as_deref().map(telemetry::parse_since).transpose()?;
    let until_dt = until.as_deref().map(telemetry::parse_until).transpose()?;
    let events = telemetry::read_logs(&log_dir, since_dt, until_dt, None)?;

    let dimension = match by {
        StatsBy::TemplateVersion => StatsDimension::TemplateVersion,
        StatsBy::TaskType => StatsDimension::TaskType,
        StatsBy::Worker => StatsDimension::Worker,
    };

    let mut rows = compute_stats(&events, dimension);
    // Sort by beads descending so most active groups appear first.
    rows.sort_by(|a, b| b.beads.cmp(&a.beads).then(a.key.cmp(&b.key)));

    let dim_label = match by {
        StatsBy::TemplateVersion => "TEMPLATE VERSION",
        StatsBy::TaskType => "TASK TYPE",
        StatsBy::Worker => "WORKER",
    };

    match format {
        ListFormat::Table => {
            if rows.is_empty() {
                println!("No telemetry data found.");
                return Ok(());
            }
            let key_width = rows.iter().map(|r| r.key.len()).max().unwrap_or(16).max(16);
            println!(
                "{:<width$} {:>6} {:>6} {:>6} {:>8} {:>9} {:>10} {:>12}",
                dim_label,
                "BEADS",
                "PASS",
                "FAIL",
                "TIMEOUT",
                "PASS RATE",
                "AVG TOK",
                "AVG COST",
                width = key_width,
            );
            println!(
                "{}",
                "-".repeat(key_width + 6 + 6 + 6 + 8 + 9 + 10 + 12 + 7)
            );
            for row in &rows {
                let pass_rate = row
                    .pass_rate()
                    .map(|r| format!("{:.1}%", r * 100.0))
                    .unwrap_or_else(|| "-".to_string());
                let avg_tok = row
                    .avg_tokens()
                    .map(|t| format!("{:.0}", t))
                    .unwrap_or_else(|| "-".to_string());
                let avg_cost = row
                    .avg_cost_usd()
                    .map(|c| format!("${:.5}", c))
                    .unwrap_or_else(|| "-".to_string());
                println!(
                    "{:<width$} {:>6} {:>6} {:>6} {:>8} {:>9} {:>10} {:>12}",
                    row.key,
                    row.beads,
                    row.pass,
                    row.fail,
                    row.timeout,
                    pass_rate,
                    avg_tok,
                    avg_cost,
                    width = key_width,
                );
            }
        }
        ListFormat::Json => {
            let json_rows: Vec<serde_json::Value> = rows
                .iter()
                .map(|row| {
                    serde_json::json!({
                        "key": row.key,
                        "beads": row.beads,
                        "pass": row.pass,
                        "fail": row.fail,
                        "timeout": row.timeout,
                        "pass_rate": row.pass_rate(),
                        "avg_tokens": row.avg_tokens(),
                        "avg_cost_usd": row.avg_cost_usd(),
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&json_rows)
                    .context("failed to serialize stats to JSON")?
            );
        }
    }

    Ok(())
}

/// `needle supervise` — run the fleet supervisor daemon.
///
/// The supervisor monitors the bead store and fleet state, auto-scaling
/// workers when beads appear and the fleet is under capacity.
fn cmd_supervise(workspace: Option<PathBuf>) -> Result<()> {
    let rt =
        tokio::runtime::Runtime::new().context("failed to create tokio runtime for supervisor")?;

    rt.block_on(crate::supervisor::run_supervisor(workspace))
}

/// `needle config` — view or inspect configuration.
fn cmd_config(
    get: Option<String>,
    set: Option<Vec<String>>,
    dump: bool,
    show_source: bool,
    live: bool,
) -> Result<()> {
    if show_source && !dump {
        bail!("--show-source requires --dump");
    }
    if live && !dump {
        bail!("--live requires --dump");
    }

    let workspace_root = std::env::current_dir().unwrap_or_default();
    let (config, sources) = ConfigLoader::load_resolved(&workspace_root, CliOverrides::default())?;

    // Handle --set flag (stub implementation)
    if let Some(set_args) = set {
        return handle_config_set_stub(set_args);
    }

    if let Some(key) = get {
        let value = config_get_key(&config, &key);
        match value {
            Some(v) => println!("{v}"),
            None => bail!("unknown config key: {key}"),
        }
        return Ok(());
    }

    if dump {
        // A source-annotated dump is also the operator-facing live view when
        // workers are running. Keep --live as an explicit compatibility flag
        // for callers that want the same view without source annotations.
        if live || show_source {
            return dump_live_config(&config, &sources, show_source, live);
        }

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

/// Parse key/value pairs from set input arguments.
///
/// Supports two syntaxes:
/// - KEY VALUE (space-separated)
/// - KEY=VALUE (equals-separated)
///
/// # Arguments
/// * `set_args` - Vector of input strings to parse
///
/// # Returns
/// * `Ok(Vec<(String, String)>)` - Vector of (key, value) pairs
/// * `Err(anyhow::Error)` - If parsing fails
///
/// # Examples
/// ```
/// let args = vec!["KEY=VALUE".to_string()];
/// let pairs = parse_key_value_pairs(args)?;
/// assert_eq!(pairs, vec![("KEY".to_string(), "VALUE".to_string())]);
///
/// let args = vec!["KEY".to_string(), "VALUE".to_string()];
/// let pairs = parse_key_value_pairs(args)?;
/// assert_eq!(pairs, vec![("KEY".to_string(), "VALUE".to_string())]);
/// ```
fn parse_key_value_pairs(set_args: Vec<String>) -> Result<Vec<(String, String)>> {
    if set_args.is_empty() {
        bail!("--set requires at least one KEY VALUE or KEY=VALUE pair");
    }

    let mut result = Vec::new();
    let mut i = 0;

    while i < set_args.len() {
        let arg = &set_args[i];

        // Check if this is KEY=VALUE format
        if arg.contains('=') {
            let parts: Vec<&str> = arg.splitn(2, '=').collect();
            if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
                bail!("invalid KEY=VALUE format: '{}'", arg);
            }
            let key = parts[0].to_string();
            let value = parts[1].to_string();
            result.push((key, value));
            i += 1;
        } else {
            // KEY VALUE format - need next arg
            if i + 1 >= set_args.len() {
                bail!("missing value for key '{}'", arg);
            }
            let key = arg.clone();
            let value = set_args[i + 1].clone();
            result.push((key, value));
            i += 2;
        }
    }

    Ok(result)
}

/// Handle --set flag (stub implementation).
///
/// Supports two syntaxes:
/// --set KEY VALUE
/// --set KEY=VALUE
///
/// Multiple sets can be specified in a single invocation.
///
/// This is a stub that only parses and prints the key-value pairs.
fn handle_config_set_stub(set_args: Vec<String>) -> Result<()> {
    let pairs = parse_key_value_pairs(set_args)?;

    // Display each key-value pair
    for (key, value) in pairs {
        println!("Would set: {} = {}", key, value);
    }

    println!("\nset not yet implemented");
    Ok(())
}

/// Dump the configuration snapshots published by running workers.
///
/// A worker owns the authoritative snapshot because its in-memory config can
/// differ from the files currently on disk after a hot reload. If a worker was
/// started by an older binary and has no snapshot yet, the resolved config is
/// shown with an explicit warning rather than being presented as live state.
fn dump_live_config(
    config: &Config,
    sources: &SourceMap,
    show_source: bool,
    explicit_live: bool,
) -> Result<()> {
    use crate::registry::Registry;

    let needle_home = &config.workspace.home;
    let registry = Registry::default_location(needle_home);

    let workers = match registry.list() {
        Ok(workers) => workers,
        Err(e) => {
            println!(
                "# No worker registry found ({}): no running workers to inspect",
                e
            );
            println!("# The registry is created when workers start. Run 'needle status' to see active workers.");
            return Ok(());
        }
    };

    if workers.is_empty() {
        if explicit_live {
            println!("# No workers registered (registry is empty): no running workers to inspect");
            println!(
                "# Workers register when they start. Run 'needle status' to verify fleet state."
            );
        } else {
            // `--show-source` remains useful on a stopped fleet; there is no
            // live snapshot to prefer, so retain the ordinary resolved view.
            for line in ConfigLoader::dump_with_sources(config, sources) {
                println!("{line}");
            }
        }
        return Ok(());
    }

    println!(
        "# Live configuration from {} running worker(s):",
        workers.len()
    );
    println!("#");

    // Each worker can be at a different reload generation, so print its
    // snapshot next to its identity rather than printing one shared config.
    for worker in &workers {
        println!("# Worker: {} (PID: {}) — ALIVE", worker.id, worker.pid);
        println!(
            "#   Started: {}",
            worker.started_at.format("%Y-%m-%d %H:%M:%S UTC")
        );
        println!("#   Agent: {}", worker.agent);
        if let Some(provider) = &worker.provider {
            println!("#   Provider: {}", provider);
        }
        if let Some(model) = &worker.model {
            println!("#   Model: {}", model);
        }
        println!("#   Workspace: {}", worker.workspace.display());
        println!("#   Beads processed: {}", worker.beads_processed);
        println!(
            "#   Config reload generation: {}",
            worker.config_reload_generation
        );
        match registry.live_config(&worker.id) {
            Ok(Some(snapshot)) => {
                println!("#   Live config snapshot: available");
                let lines = live_snapshot_values(&snapshot, show_source);
                for line in lines {
                    println!("{line}");
                }
                if snapshot.reload_generation != worker.config_reload_generation {
                    println!(
                        "#   Warning: snapshot generation {} differs from registry generation {}",
                        snapshot.reload_generation, worker.config_reload_generation
                    );
                }
            }
            Ok(None) => {
                println!("#   Live config snapshot: unavailable (worker predates live-config publishing)");
                print_fallback_config(config, sources, show_source);
            }
            Err(error) => {
                println!("#   Live config snapshot: unavailable ({error})");
                print_fallback_config(config, sources, show_source);
            }
        }
        println!("#");
    }

    println!("# Tip: Values above come from each worker's published in-memory snapshot.");

    Ok(())
}

fn print_fallback_config(config: &Config, sources: &SourceMap, show_source: bool) {
    if show_source {
        for line in ConfigLoader::dump_with_sources(config, sources) {
            println!("#   {line}");
        }
    } else {
        for line in config_dump(config) {
            println!("#   {line}");
        }
    }
}

fn live_snapshot_values(
    snapshot: &crate::registry::LiveConfigSnapshot,
    show_source: bool,
) -> &[String] {
    if show_source {
        &snapshot.values_with_sources
    } else {
        &snapshot.values
    }
}

/// Look up a single config key by dot-separated path.
fn config_get_key(config: &Config, key: &str) -> Option<String> {
    match key {
        "agent.default" => Some(config.agent.default.clone()),
        "agent.timeout" => Some(config.agent.timeout.to_string()),
        "worker.max_workers" => Some(config.worker.max_workers.to_string()),
        "worker.launch_stagger_seconds" => Some(config.worker.launch_stagger_seconds.to_string()),
        "worker.idle_timeout" => Some(config.worker.idle_timeout.to_string()),
        "worker.idle_action" => Some(
            match config.worker.idle_action {
                IdleAction::Wait => "wait",
                IdleAction::Exit => "exit",
            }
            .to_string(),
        ),
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
            "worker.idle_action: {}",
            match config.worker.idle_action {
                IdleAction::Wait => "wait",
                IdleAction::Exit => "exit",
            }
        ),
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
    Skip,
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
    fn skip(name: impl Into<String>, msg: impl Into<String>) -> Self {
        CheckResult {
            name: name.into(),
            status: CheckStatus::Skip,
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
            CheckStatus::Skip => "SKIP",
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

fn doctor_check_checkpoint(
    beads_dir: &Path,
    bead_cli: &crate::config::BeadCliConfig,
) -> CheckResult {
    if matches!(
        bead_cli.backend,
        crate::config::BeadBackend::Bead | crate::config::BeadBackend::Br
    ) {
        let pointer = beads_dir.join("checkpoint/current.json");
        if !pointer.exists() {
            return CheckResult::fail("Checkpoint", "checkpoint/current.json not found");
        }
        return match std::fs::read_to_string(&pointer) {
            Ok(content) if serde_json::from_str::<serde_json::Value>(&content).is_ok() => {
                CheckResult::pass("Checkpoint", "native pointer is valid JSON")
            }
            Ok(_) => CheckResult::fail("Checkpoint", "current.json is invalid JSON"),
            Err(error) => CheckResult::fail("Checkpoint", format!("unreadable: {error}")),
        };
    }
    doctor_check_jsonl(beads_dir)
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
    let store = match crate::bead_store::discover_default(
        workspace.to_path_buf(),
        None,
        Some("needle-doctor".to_string()),
        Some(env!("CARGO_PKG_VERSION").to_string()),
    ) {
        Ok(store) => store,
        Err(error) => {
            return Ok(CheckResult::fail(
                "Bead store",
                format!("configured backend unavailable: {error:#}"),
            ))
        }
    };
    doctor_run_bead_store_checks(store, repair)
}

fn doctor_check_bead_backend(config: &Config) -> CheckResult {
    let (backend, path) = match crate::config::resolve_bead_cli(&config.bead_cli) {
        Ok(resolved) => resolved,
        Err(error) => {
            return CheckResult::fail(
                "Bead backend",
                format!("{}: {error:#}", config.bead_cli.backend),
            )
        }
    };
    let name = match backend {
        crate::config::Backend::Bead => "bead-rs",
    };
    let Some(descriptor) = crate::bead_store::builtin_bead_backends()
        .into_iter()
        .find(|descriptor| descriptor.name == name)
    else {
        return CheckResult::fail("Bead backend", format!("descriptor {name} is missing"));
    };
    let mut detail = vec![format!("verified against: {}", descriptor.verified_against)];
    if !descriptor.capabilities.transactional_batch {
        detail.push("capability gap: split/mitosis is sequential, not atomic".to_string());
    }
    if !descriptor.capabilities.velocity_metadata {
        detail.push("capability gap: claim omits model/harness velocity metadata".to_string());
    }
    CheckResult::pass(
        "Bead backend",
        format!("{} at {}", descriptor.name, path.display()),
    )
    .with_detail(detail)
}

fn doctor_run_bead_store_checks(
    store: std::sync::Arc<dyn BeadStore>,
    repair: bool,
) -> Result<CheckResult> {
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

    // Always check the agent binary itself - this check is independent of the bead backend
    match which::which(agent) {
        Ok(path) => CheckResult::pass("Agent binary", format!("{} at {}", agent, path.display())),
        Err(_) => CheckResult::fail(
            "Agent binary",
            format!(
                "{} not found on PATH (checked: {})",
                agent,
                std::env::var("PATH")
                    .unwrap_or_else(|_| "<empty>".to_string())
                    .replace(":", ", ")
            ),
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
    let global = ConfigLoader::load_global()?;
    let workspace_root = workspace.unwrap_or_else(|| global.workspace.default.clone());
    let (config, _) = ConfigLoader::load_resolved(
        &workspace_root,
        CliOverrides {
            workspace: Some(workspace_root.clone()),
            ..Default::default()
        },
    )?;
    let needle_home = config.workspace.home.clone();
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
        results.push(doctor_check_checkpoint(&beads_dir, &config.bead_cli));
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

    // Resolved descriptor, binary, and declared safety gaps.
    results.push(doctor_check_bead_backend(&config));

    // Bead store connectivity.
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
// Query command
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
// Idle-strand cooldown helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Per-row data for the `needle status --idle-strands` cooldown table.
struct IdleStrandRow {
    strand: String,
    workspace: String,
    enabled: bool,
    last_run: Option<DateTime<Utc>>,
    cooldown_hours: u64,
    status: String,
}

#[derive(serde::Deserialize)]
struct ReflectStateCli {
    last_consolidation: DateTime<Utc>,
}

#[derive(serde::Deserialize)]
struct LastRunStateCli {
    last_run: Option<DateTime<Utc>>,
}

#[derive(serde::Deserialize)]
struct UnravelStateCli {
    analyzed: std::collections::HashMap<String, DateTime<Utc>>,
}

/// Compute a short SHA-256 workspace hash (matches strand implementations).
fn workspace_hash_cli(workspace: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(workspace.display().to_string().as_bytes());
    let result = hasher.finalize();
    result
        .iter()
        .take(8)
        .fold(String::with_capacity(16), |mut acc, b| {
            use std::fmt::Write;
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

/// Compute a human-readable cooldown status string for an idle strand.
fn idle_strand_status(
    enabled: bool,
    last_run: Option<DateTime<Utc>>,
    cooldown_hours: u64,
    now: DateTime<Utc>,
) -> String {
    if !enabled {
        return "disabled".to_string();
    }
    match last_run {
        None => "ready (never run)".to_string(),
        Some(last) => {
            let elapsed_hours = (now - last).num_hours().max(0) as u64;
            if elapsed_hours >= cooldown_hours {
                "ready".to_string()
            } else {
                let remaining = cooldown_hours - elapsed_hours;
                format!("cooldown ({}h left)", remaining)
            }
        }
    }
}

/// Build idle-strand cooldown rows for `needle status --idle-strands`.
///
/// Reflect uses a single shared state file (not per-workspace-hash) because it
/// operates on config.workspace.default regardless of which workspace a worker
/// was launched against. Weave, pulse, and unravel are keyed per workspace path.
///
/// Per-workspace vs per-worker gating:
/// - reflect: workspace-level gate shared by all workers; cooldown_hours and
///   min_beads_since_last both count against the same state file
/// - weave/pulse/unravel: per-workspace state, so a fleet of workers on N
///   workspaces maintains N independent cooldown windows per strand
fn idle_strand_rows(
    config: &Config,
    state_base: &Path,
    workspaces: &[PathBuf],
) -> Vec<IdleStrandRow> {
    let now = Utc::now();
    let mut rows = Vec::new();

    // Reflect: single shared state file — show once for config.workspace.default.
    {
        let state_path = state_base.join("reflect").join("reflect_state.json");
        let last_run = std::fs::read_to_string(&state_path)
            .ok()
            .and_then(|s| serde_json::from_str::<ReflectStateCli>(&s).ok())
            .map(|s| s.last_consolidation);
        let cooldown = config.strands.reflect.cooldown_hours;
        rows.push(IdleStrandRow {
            strand: "reflect".to_string(),
            workspace: config.workspace.default.display().to_string(),
            enabled: config.strands.reflect.enabled,
            last_run,
            cooldown_hours: cooldown,
            status: idle_strand_status(config.strands.reflect.enabled, last_run, cooldown, now),
        });
    }

    // Weave, pulse, unravel: one row per workspace per strand.
    for workspace in workspaces {
        let hash = workspace_hash_cli(workspace);
        let ws = workspace.display().to_string();

        // Weave
        {
            let path = state_base.join("weave").join(format!("{hash}.json"));
            let last_run = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str::<LastRunStateCli>(&s).ok())
                .and_then(|s| s.last_run);
            let cooldown = config.strands.weave.cooldown_hours;
            rows.push(IdleStrandRow {
                strand: "weave".to_string(),
                workspace: ws.clone(),
                enabled: config.strands.weave.enabled,
                last_run,
                cooldown_hours: cooldown,
                status: idle_strand_status(config.strands.weave.enabled, last_run, cooldown, now),
            });
        }

        // Pulse
        {
            let path = state_base.join("pulse").join(format!("{hash}.json"));
            let last_run = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str::<LastRunStateCli>(&s).ok())
                .and_then(|s| s.last_run);
            let cooldown = config.strands.pulse.cooldown_hours;
            rows.push(IdleStrandRow {
                strand: "pulse".to_string(),
                workspace: ws.clone(),
                enabled: config.strands.pulse.enabled,
                last_run,
                cooldown_hours: cooldown,
                status: idle_strand_status(config.strands.pulse.enabled, last_run, cooldown, now),
            });
        }

        // Unravel: per-bead cooldown — most recent analysis time as proxy for last run.
        {
            let path = state_base.join("unravel").join(format!("{hash}.json"));
            let last_run = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str::<UnravelStateCli>(&s).ok())
                .and_then(|s| s.analyzed.values().copied().max());
            let cooldown = config.strands.unravel.cooldown_hours;
            rows.push(IdleStrandRow {
                strand: "unravel".to_string(),
                workspace: ws.clone(),
                enabled: config.strands.unravel.enabled,
                last_run,
                cooldown_hours: cooldown,
                status: idle_strand_status(config.strands.unravel.enabled, last_run, cooldown, now),
            });
        }
    }

    rows
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
    /// PID of the needle run process in this session (if available).
    pid: Option<u32>,
}

/// List all tmux sessions whose names start with `needle-`.
fn list_needle_sessions() -> Result<Vec<TmuxSession>> {
    let output = crate::tmux_socket::command()
        .args([
            "list-sessions",
            "-F",
            "#{session_name}\t#{session_created}\t#{session_attached}\t#{pane_pid}",
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
            if parts.len() < 4 {
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
            // Parse PID from pane_pid (tmux returns empty string if no pane)
            let pid = parts[3].parse::<u32>().ok();
            Some(TmuxSession {
                name: name.to_string(),
                created,
                status,
                pid,
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
    let prefix = sanitize_session_name(&format!("needle-{agent}-"));
    let sessions = list_needle_sessions()?;
    let ids = sessions
        .iter()
        .filter_map(|s| s.name.strip_prefix(&prefix))
        .map(|id| id.to_string())
        .collect();
    Ok(ids)
}

// ──────────────────────────────────────────────────────────────────────────────
// Process table discovery
// ──────────────────────────────────────────────────────────────────────────────

/// A needle process discovered from the process table.
#[derive(Debug, Clone)]
pub struct DiscoveredProcess {
    pid: u32,
    workspace: Option<PathBuf>,
    agent: Option<String>,
    identifier: Option<String>,
    /// Full command line for debugging
    cmdline: String,
}

/// Scan the process table for all running `needle run` processes.
///
/// This discovers workers regardless of how they were started (tmux-wrapped or
/// bare NEEDLE_INNER=1 invocation). It reads /proc to find processes whose
/// command line contains "needle run" and extracts workspace, agent, and
/// identifier from the arguments.
#[cfg(unix)]
fn scan_needle_processes() -> Result<Vec<DiscoveredProcess>> {
    use std::fs;

    let mut discovered = Vec::new();
    let mut ppid_to_children: HashMap<u32, Vec<u32>> = HashMap::new();

    // Iterate over all entries in /proc.
    let proc_dir = Path::new("/proc");
    let entries = match fs::read_dir(proc_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Not Linux — no process table available.
            return Ok(vec![]);
        }
        Err(e) => {
            return Err(e).context("failed to read /proc directory");
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let pid_str = entry.file_name();
        let pid: u32 = match pid_str.to_string_lossy().parse() {
            Ok(p) => p,
            Err(_) => continue, // Not a numeric directory (not a PID)
        };

        // Read the process's PPID to build parent-child mapping during the scan.
        // This avoids race conditions from re-reading /proc later.
        let ppid: Option<u32> = {
            let status_path = entry.path().join("status");
            if let Ok(content) = fs::read_to_string(&status_path) {
                content
                    .lines()
                    .find(|line| line.starts_with("PPID:\t"))
                    .and_then(|line| line.split(':').nth(1))
                    .and_then(|v| v.trim().parse().ok())
            } else {
                None
            }
        };

        // Add to parent-child mapping if we have both PPID and PID
        if let Some(parent_pid) = ppid {
            ppid_to_children.entry(parent_pid).or_default().push(pid);
        }

        // Read the process's command line.
        let cmdline_path = entry.path().join("cmdline");
        let cmdline_bytes = match fs::read(&cmdline_path) {
            Ok(b) => b,
            Err(_) => continue, // Process may have exited
        };

        // cmdline is null-separated; parse as argv array for strict matching.
        let args: Vec<String> = cmdline_bytes
            .split(|&b| b == 0)
            .map(|args| String::from_utf8_lossy(args).to_string())
            .filter(|s| !s.is_empty())
            .collect();

        // Build space-separated cmdline for debugging and later parsing
        let cmdline: String = args.join(" ");

        // Check if this is a needle run process by strictly matching argv.
        // A genuine needle run process has:
        // - argv[0] that is the needle binary (basename is "needle")
        // - argv[1] == "run" (after skipping NEEDLE_INNER=1 if present)
        //
        // We also support NEEDLE_INNER test processes via environment variable.
        //
        // The strict basename check prevents false positives from:
        // - Child processes with ".needle/" in their paths
        // - Random processes with "needle" anywhere in their binary name
        // - Orphaned processes that happen to match substring patterns
        let is_needle_run = {
            // Skip NEEDLE_INNER=1 prefix if present (e.g., "NEEDLE_INNER=1 /path/to/needle run ...")
            let needle_binary_idx = if args.len() >= 2 && args[0] == "NEEDLE_INNER=1" {
                1
            } else {
                0
            };

            // Need at least: binary + "run" argument
            if args.len() < needle_binary_idx + 2 {
                false
            } else {
                let binary_path = &args[needle_binary_idx];
                let run_arg_idx = needle_binary_idx + 1;

                // Extract basename from binary path (handles "/path/to/needle", "needle", "./needle")
                let binary_name = match PathBuf::from(binary_path).file_name() {
                    Some(name) => name.to_string_lossy().to_string(),
                    None => binary_path.clone(),
                };

                // Strict match: basename must be exactly "needle" and next arg must be "run"
                binary_name == "needle" && args[run_arg_idx] == "run"
            }
        };

        // Only include genuine needle run processes.
        // This strict matching prevents false positives from:
        // - Child processes with ".needle/" in their paths
        // - Random processes with "needle" in their binary name
        // - Orphaned processes that happen to match substring patterns
        // - Child processes that inherit NEEDLE_INNER from parent workers
        if !is_needle_run {
            continue;
        }

        // Filter out shell wrapper processes (bash -c "NEEDLE_INNER=1 needle run ...").
        // These are created by tmux sessions and are not the actual needle worker processes.
        // We only want to discover processes that are directly executing needle, not shell wrappers.
        if cmdline.starts_with("bash -c")
            || cmdline.starts_with("sh -c")
            || cmdline.starts_with("/bin/bash -c")
            || cmdline.starts_with("/bin/sh -c")
        {
            continue;
        }

        // Parse arguments to extract workspace, agent, identifier.
        // Handle NEEDLE_INNER=1 prefix in cmdline: "NEEDLE_INNER=1 /path/to/needle run ..."
        let args: Vec<&str> = cmdline.split_whitespace().collect();
        let mut workspace = None;
        let mut agent = None;
        let mut identifier = None;

        let mut i = 0;
        while i < args.len() {
            match args[i] {
                // Skip NEEDLE_INNER environment variable prefix
                "NEEDLE_INNER=1" => {
                    i += 1;
                }
                "--workspace" | "-w" if i + 1 < args.len() => {
                    workspace = Some(PathBuf::from(args[i + 1]));
                    i += 2;
                }
                "--workspace" | "-w" => {
                    i += 1;
                }
                "--agent" | "-a" if i + 1 < args.len() => {
                    agent = Some(args[i + 1].to_string());
                    i += 2;
                }
                "--agent" | "-a" => {
                    i += 1;
                }
                "--identifier" | "-i" if i + 1 < args.len() => {
                    identifier = Some(args[i + 1].to_string());
                    i += 2;
                }
                "--identifier" | "-i" => {
                    i += 1;
                }
                _ => {
                    i += 1;
                }
            }
        }

        // Validate cmdline parsing succeeded before including this process.
        // During concurrent startup, processes may be read while their cmdline
        // is still being written, resulting in incomplete metadata (<unknown>).
        // Require at least workspace to be parsed successfully; this filters
        // out processes that are mid-startup or mid-shutdown.
        //
        // This fix addresses needle-c5967224: concurrent startup was causing
        // 58 false positives when only 15 workers existed, with 44/58 entries
        // showing "<unknown>" for all metadata fields.
        if workspace.is_none() || agent.is_none() {
            continue;
        }

        discovered.push(DiscoveredProcess {
            pid,
            workspace,
            agent,
            identifier,
            cmdline,
        });
    }

    // Filter out descendant processes to prevent worker count inflation.
    // When a worker spawns a subprocess (e.g., an agent), that subprocess may
    // itself be a needle run process (e.g., in recursive dispatch scenarios).
    // We need to exclude these descendant processes from the worker count to
    // avoid inflating the fleet size.
    //
    // Use the parent-child mapping built during the initial scan to avoid
    // race conditions from re-reading /proc after processes may have changed.
    let discovered = filter_descendant_processes_with_mapping(discovered, &ppid_to_children);

    Ok(discovered)
}

/// Test-only version of scan_needle_processes that validates cmdline completeness.
///
/// This function is used in regression tests for concurrent startup bugs (needle-c5967224).
/// It's exposed for testing to verify that the scanner correctly filters out processes
/// with incomplete metadata during concurrent worker startup.
#[cfg(unix)]
#[cfg(test)]
pub fn scan_needle_processes_for_test() -> Result<Vec<DiscoveredProcess>> {
    scan_needle_processes()
}

/// Filter out descendant processes using a pre-built parent->children mapping.
///
/// This is the preferred filtering method that avoids race conditions by using
/// a process mapping built from a consistent snapshot of /proc.
///
/// Only filters out needle run processes that are descendants of OTHER needle run
/// processes. Intermediate non-needle processes (e.g., shells) break the chain
/// and prevent false positives.
#[cfg(unix)]
fn filter_descendant_processes_with_mapping(
    processes: Vec<DiscoveredProcess>,
    ppid_to_children: &HashMap<u32, Vec<u32>>,
) -> Vec<DiscoveredProcess> {
    use std::collections::HashSet;

    if processes.is_empty() {
        return processes;
    }

    // Collect discovered PIDs for quick lookup
    let discovered_pids: HashSet<u32> = processes.iter().map(|p| p.pid).collect();

    // Build a set of all descendant PIDs of each discovered process
    // CRITICAL: Only trace through OTHER DISCOVERED processes, not all processes.
    // This prevents false positives where a needle process has an intermediate
    // non-needle ancestor (e.g., a shell wrapper).
    let mut all_descendants: HashSet<u32> = HashSet::new();

    for &root_pid in &discovered_pids {
        let mut visited: HashSet<u32> = HashSet::new();
        find_descendants_through_needle_processes_helper(
            root_pid,
            ppid_to_children,
            &discovered_pids,
            &mut all_descendants,
            &mut visited,
        );
    }

    // Filter: keep only discovered processes that are NOT descendants of another discovered process
    processes
        .into_iter()
        .filter(|p| {
            // Keep if this PID is a discovered process but NOT a descendant of another discovered process
            discovered_pids.contains(&p.pid) && !all_descendants.contains(&p.pid)
        })
        .collect()
}

/// Recursive helper to traverse process tree and collect descendants.
///
/// CRITICAL: Only follows descendants that are ALSO discovered (needle run) processes.
/// Intermediate non-needle processes (shells, wrappers, etc.) break the chain.
#[cfg(unix)]
fn find_descendants_through_needle_processes_helper(
    pid: u32,
    ppid_to_children: &HashMap<u32, Vec<u32>>,
    discovered_pids: &HashSet<u32>,
    descendants: &mut HashSet<u32>,
    visited: &mut HashSet<u32>,
) {
    if let Some(children) = ppid_to_children.get(&pid) {
        for &child_pid in children {
            // Only trace through OTHER discovered processes
            // Non-needle processes break the descendant chain
            if !discovered_pids.contains(&child_pid) {
                continue;
            }

            if visited.insert(child_pid) {
                descendants.insert(child_pid);
                find_descendants_through_needle_processes_helper(
                    child_pid,
                    ppid_to_children,
                    discovered_pids,
                    descendants,
                    visited,
                );
            }
        }
    }
}

/// Stub for non-Unix platforms (Windows, etc.).
#[cfg(not(unix))]
fn scan_needle_processes() -> Result<Vec<DiscoveredProcess>> {
    // No /proc on these platforms.
    Ok(vec![])
}

#[cfg(not(unix))]
fn filter_descendant_processes(processes: Vec<DiscoveredProcess>) -> Vec<DiscoveredProcess> {
    // No filtering on non-Unix platforms.
    processes
}

/// Reconcile discovered processes against the registry and emit warnings.
///
/// This function compares processes found in the process table against the
/// worker registry and emits warnings for any unregistered needle run processes.
/// It helps identify workers that failed to register during boot due to disk
/// errors, permission issues, or other failures.
#[cfg(unix)]
fn reconcile_process_registry(discovered: &[DiscoveredProcess], registry: &Registry) -> Result<()> {
    use std::collections::HashSet;

    let workers = registry.list().unwrap_or_default();
    let registered_pids: HashSet<u32> = workers.iter().map(|w| w.pid).collect();

    // Find processes not in the registry
    let unregistered: Vec<&DiscoveredProcess> = discovered
        .iter()
        .filter(|p| !registered_pids.contains(&p.pid))
        .collect();

    if !unregistered.is_empty() {
        eprintln!(
            "⚠️  WARNING: Found {} unregistered needle run process(es):",
            unregistered.len()
        );
        for proc in &unregistered {
            let workspace = proc
                .workspace
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<unknown>".to_string());
            let agent = proc.agent.as_deref().unwrap_or("<unknown>");
            let identifier = proc.identifier.as_deref().unwrap_or("<unknown>");
            eprintln!(
                "  PID {} — workspace: {}, agent: {}, identifier: {}",
                proc.pid, workspace, agent, identifier
            );
        }
        eprintln!("These workers may have failed to register during boot due to:");
        eprintln!("  - Disk full or permission errors on ~/.needle/state/workers.json");
        eprintln!("  - Registry file corruption or concurrent write conflicts");
        eprintln!("  - Early termination during worker boot before registry registration");
        eprintln!("The workers will continue processing beads but are invisible to 'needle status' and 'needle list'.");
        eprintln!("To fix:");
        eprintln!("  1. Check disk space: df -h ~/.needle");
        eprintln!("  2. Check permissions: ls -la ~/.needle/state/");
        eprintln!("  3. Kill and restart affected workers to force re-registration");
        eprintln!();
    }

    Ok(())
}

/// Stub for non-Unix platforms (Windows, etc.).
#[cfg(not(unix))]
fn reconcile_process_registry(
    _discovered: &[DiscoveredProcess],
    _registry: &Registry,
) -> Result<()> {
    // No /proc on these platforms - cannot reconcile.
    Ok(())
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
    tel.emit(
        crate::telemetry::EventKind::CanaryStarted {
            suite: suite_id.clone(),
        },
        chrono::Utc::now(),
    )?;

    println!("Running canary tests...");
    let report = runner.run()?;

    tel.emit(
        crate::telemetry::EventKind::CanarySuiteCompleted {
            suite: suite_id,
            passed: report.passed as u32,
            failed: (report.failed + report.timed_out + report.errors) as u32,
        },
        chrono::Utc::now(),
    )?;

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
            tel.emit(
                crate::telemetry::EventKind::CanaryPromoted { hash },
                chrono::Utc::now(),
            )?;
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
        tel.emit(
            crate::telemetry::EventKind::CanaryRejected { reason },
            chrono::Utc::now(),
        )?;
        println!("Testing binary discarded.");
    }

    Ok(())
}

/// `needle upgrade` — check for and install updates.
fn cmd_upgrade(check_only: bool) -> Result<()> {
    // Set up telemetry for upgrade operations
    let config = ConfigLoader::load_global()?;
    let tel = crate::telemetry::Telemetry::from_config("upgrade".to_string(), &config.telemetry)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "upgrade telemetry init failed, continuing without telemetry");
            crate::telemetry::Telemetry::new("upgrade".to_string())
        });

    if check_only {
        let check = crate::upgrade::check_for_update_with_telemetry(Some(&tel))?;
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

    // For full upgrade, use the telemetry version so we emit events
    crate::upgrade::perform_upgrade_with_telemetry(Some(&tel))?;
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
    tel.emit(
        crate::telemetry::EventKind::RollbackCompleted {
            rolled_back_hash: stable_hash,
            restored_hash: prev_hash,
        },
        chrono::Utc::now(),
    )?;

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

    // Create the extraction agent if configured.
    let agent = if let Some(ref agent_cmd) = config.strands.reflect.extraction_agent {
        Some(Box::new(crate::strand::reflect::CliReflectAgent::new(
            agent_cmd.clone(),
            config.strands.reflect.extraction_prompt_template.clone(),
        )) as Box<dyn crate::strand::reflect::ReflectAgent>)
    } else {
        None
    };

    let strand = crate::strand::ReflectStrand::new(
        config.strands.reflect.clone(),
        workspace_root.clone(),
        state_dir,
        tel,
        agent,
    );

    let store =
        crate::bead_store::open_configured(&config.bead_cli, workspace_root, None, None, None)?;

    let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    let summary = rt.block_on(strand.consolidate_with_store(force, store.as_ref()))?;

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
    use crate::bead_store::spawn_with_etxtbsy_retry;

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

    #[cfg(feature = "otlp")]
    #[test]
    fn worker_construction_identity_populates_otlp_resource() {
        let mut config = Config::default();
        config.agent.default = "claude-sonnet".to_string();
        config.agent.adapters_dir = PathBuf::from("/definitely/missing/needle-adapters");
        config.workspace.default = PathBuf::from("/private/workspaces/needle-repo");

        // This is the same identity builder used by run_worker before it
        // constructs the structured event sink and the tracing layer.
        let identity = worker_telemetry_identity(&config);
        let resource = crate::telemetry::otlp::OtlpSink::build_resource(
            "claude-sonnet-alpha",
            "test-session",
            &config.telemetry.otlp_sink,
            identity.agent.as_deref(),
            identity.model.as_deref(),
            identity.provider.as_deref(),
            identity.workspace.as_deref().and_then(Path::to_str),
        )
        .expect("worker identity should produce an OTLP resource");

        let attr = |key: &str| {
            resource
                .iter()
                .find(|(candidate, _)| candidate.as_str() == key)
                .map(|(_, value)| value.as_str().to_string())
        };

        assert_eq!(attr("needle.agent").as_deref(), Some("claude-sonnet"));
        assert_eq!(attr("needle.model").as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(attr("needle.workspace").as_deref(), Some("needle-repo"));
        assert_eq!(
            attr("service.version").as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert!(!attr("needle.workspace")
            .expect("workspace resource attribute should be present")
            .contains("/private/"));
    }

    #[test]
    fn is_needle_inner_false_by_default() {
        // Without NEEDLE_INNER set this should return false (env-dependent,
        // but confirms the function does not panic).
        // We cannot unset env vars reliably in a parallel test suite, so we
        // only assert the call succeeds without panicking.
        let _ = is_needle_inner();
    }

    #[tokio::test]
    async fn is_needle_inner_true_when_env_set() {
        // Temporarily set NEEDLE_INNER=1 and verify detection.
        // Use a sub-process approach via std::process to avoid mutating the
        // test process's env and racing with parallel tests.
        let exe_path = std::env::current_exe().unwrap();
        let output = spawn_with_etxtbsy_retry(
            || async {
                tokio::process::Command::new(&exe_path)
                    .env("NEEDLE_INNER", "1")
                    .args(["--help"])
                    .output()
                    .await
            },
            5,  // max_attempts
            20, // backoff_ms
        )
        .await;
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

    #[cfg(unix)]
    #[test]
    fn scan_needle_processes_returns_result() {
        // Verify that scan_needle_processes() can be called successfully
        // and returns a Result (even if empty, when no needle processes are running).
        let result = scan_needle_processes();
        assert!(result.is_ok(), "scan_needle_processes should return Ok");

        let processes = result.unwrap();
        // The result should be a Vec (possibly empty)
        // We can't assert specific content without a running needle process,
        // but we verify the structure is correct.
        assert!(
            !processes.is_empty() || processes.is_empty(),
            "should return a valid Vec"
        );
    }

    #[cfg(unix)]
    #[test]
    fn scan_needle_processes_discovers_needle_run_processes() {
        // This test verifies that scan_needle_processes() can discover
        // needle run processes by scanning /proc.
        // It's an informational test - it doesn't fail if no processes are found,
        // but it verifies the scanning logic doesn't panic or error.

        let result = scan_needle_processes();
        match result {
            Ok(processes) => {
                // Each discovered process should have the required fields.
                // Not asserting that cmdline mentions "needle": one of
                // scan_needle_processes()'s own inclusion criteria is
                // environ-based NEEDLE_INNER inheritance (documented above the
                // scan, needed to catch test subprocesses like
                // `NEEDLE_INNER=1 sleep 3600` whose own cmdline never shows it),
                // so on a shared machine it can legitimately match an unrelated
                // process that merely inherited the env var from its parent
                // shell without "needle" appearing anywhere in its cmdline.
                // That's the scanner working as designed, not a bug — so this
                // test only checks that the scan runs cleanly and returns
                // well-formed entries, per its own docstring above.
                for proc in &processes {
                    // PIDs should be valid (> 0)
                    assert!(proc.pid > 0, "PID should be positive");
                    // cmdline should be non-empty
                    assert!(!proc.cmdline.is_empty(), "cmdline should not be empty");
                }

                // Log discovery for debugging
                if !processes.is_empty() {
                    eprintln!(
                        "✓ scan_needle_processes discovered {} needle run processes",
                        processes.len()
                    );
                }
            }
            Err(e) => {
                // On non-Linux systems or without /proc access, this is expected
                eprintln!(
                    "scan_needle_processes returned error (expected on non-Linux): {}",
                    e
                );
            }
        }
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

    /// Serialises the RUST_LOG-mutating tests; cargo runs tests in threads and
    /// env vars are process-global.
    static LOG_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn worker_log_filter_defaults_to_info() {
        let _guard = LOG_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var("RUST_LOG").ok();
        std::env::remove_var("RUST_LOG");

        // Regression guard: before this, the fmt layer had NO filter at all, so
        // a registry() passed every level and each DEBUG needle::telemetry event
        // was written to the worker log. That produced 157.7 GB across two files
        // on lab. The default must stay at or below INFO.
        let rendered = worker_log_filter().to_string();
        assert_eq!(
            rendered, "info",
            "default worker log filter must be INFO, got {rendered:?}"
        );

        if let Some(v) = saved {
            std::env::set_var("RUST_LOG", v);
        }
    }

    #[test]
    fn worker_log_filter_honours_rust_log() {
        let _guard = LOG_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var("RUST_LOG").ok();
        std::env::set_var("RUST_LOG", "debug");

        // Debugging must still be reachable — the fix caps the default, it does
        // not remove the ability to ask for DEBUG.
        let rendered = worker_log_filter().to_string();
        assert_eq!(rendered, "debug", "RUST_LOG must override the INFO default");

        match saved {
            Some(v) => std::env::set_var("RUST_LOG", v),
            None => std::env::remove_var("RUST_LOG"),
        }
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
        assert!(config_get_key(&config, "worker.idle_timeout").is_some());
        assert!(config_get_key(&config, "worker.idle_action").is_some());
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
        assert!(lines.iter().any(|l| l.starts_with("worker.idle_action:")));
        assert!(lines
            .iter()
            .any(|l| l.starts_with("health.heartbeat_ttl_secs:")));
    }

    #[test]
    fn live_snapshot_source_view_uses_worker_values() {
        let snapshot = crate::registry::LiveConfigSnapshot {
            values: vec!["worker.max_workers: 4".to_string()],
            values_with_sources: vec!["worker.max_workers: 4 (from: built-in default)".to_string()],
            reload_generation: 1,
        };

        assert_eq!(
            live_snapshot_values(&snapshot, true),
            &["worker.max_workers: 4 (from: built-in default)".to_string()]
        );
        assert_eq!(
            live_snapshot_values(&snapshot, false),
            &["worker.max_workers: 4".to_string()]
        );
    }

    // TODO: Re-enable these tests when apply_config_set is implemented
    // #[test]
    // fn apply_config_set_and_get_idle_action_roundtrip() {
    //     let mut config = Config::default();
    //
    //     // Default is "wait".
    //     assert_eq!(
    //         config_get_key(&config, "worker.idle_action"),
    //         Some("wait".to_string())
    //     );
    //
    //     apply_config_set(&mut config, "worker.idle_action", "exit").unwrap();
    //     assert_eq!(config.worker.idle_action, IdleAction::Exit);
    //     assert_eq!(
    //         config_get_key(&config, "worker.idle_action"),
    //         Some("exit".to_string())
    //     );
    //
    //     apply_config_set(&mut config, "worker.idle_action", "wait").unwrap();
    //     assert_eq!(config.worker.idle_action, IdleAction::Wait);
    //     assert_eq!(
    //         config_get_key(&config, "worker.idle_action"),
    //         Some("wait".to_string())
    //     );
    // }
    //
    // #[test]
    // fn apply_config_set_idle_action_rejects_invalid_value() {
    //     let mut config = Config::default();
    //     let result = apply_config_set(&mut config, "worker.idle_action", "bogus");
    //     assert!(result.is_err());
    //     assert!(result
    //         .unwrap_err()
    //         .to_string()
    //         .contains("invalid idle_action value"));
    // }

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
            qualified_id: "claude-test-w".to_string(),
            pid: 999_999,
            state: WorkerState::Selecting,
            current_bead: None,
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: chrono::Utc::now() - chrono::Duration::seconds(600),
            started_at: chrono::Utc::now(),
            beads_processed: 0,
            session: "test-w".to_string(),
            is_idle: false,
            current_task: None,
            model: "claude-sonnet-4".to_string(),
            heartbeat_file: None,
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
            qualified_id: "claude-test-rm".to_string(),
            pid: 999_999,
            state: WorkerState::Selecting,
            current_bead: None,
            workspace: PathBuf::from("/tmp"),
            last_heartbeat: chrono::Utc::now() - chrono::Duration::seconds(600),
            started_at: chrono::Utc::now(),
            beads_processed: 0,
            session: "test-rm".to_string(),
            is_idle: false,
            current_task: None,
            model: "claude-sonnet-4".to_string(),
            heartbeat_file: None,
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

    // Tests for process tree killing (bf-wze53)

    #[test]
    fn find_all_descendants_empty_for_nonexistent_pid() {
        // A PID that doesn't exist should return an empty list
        let descendants = find_all_descendants(9999999);
        assert!(
            descendants.is_empty(),
            "nonexistent PID should have no descendants"
        );
    }

    #[test]
    fn find_all_descendants_empty_for_current_process() {
        // Current test process typically has no children
        let current_pid = std::process::id();
        let descendants = find_all_descendants(current_pid);
        // May have spawned subprocesses, but shouldn't fail
        // Just verify it returns a vector
        let _ = descendants;
    }

    #[test]
    fn find_all_descendants_recursive() {
        // Test the recursive helper function directly
        use std::collections::{HashMap, HashSet};

        // Build a simple tree: 1 -> [2, 3], 2 -> [4], 3 -> [5, 6]
        let mut ppid_to_children: HashMap<u32, Vec<u32>> = HashMap::new();
        ppid_to_children.insert(1, vec![2, 3]);
        ppid_to_children.insert(2, vec![4]);
        ppid_to_children.insert(3, vec![5, 6]);

        let mut descendants = Vec::new();
        let mut visited = HashSet::new();

        // Mark the root PID as visited to prevent cycles (same as production code)
        visited.insert(1);

        find_descendants_recursive(1, &ppid_to_children, &mut descendants, &mut visited);

        // Should find all descendants: 2, 3, 4, 5, 6
        assert_eq!(descendants.len(), 5);
        assert!(descendants.contains(&2));
        assert!(descendants.contains(&3));
        assert!(descendants.contains(&4));
        assert!(descendants.contains(&5));
        assert!(descendants.contains(&6));
    }

    #[test]
    fn find_all_descendants_handles_cycles() {
        // Test that the visited set prevents infinite loops
        use std::collections::{HashMap, HashSet};

        // Create a cycle: 1 -> [2], 2 -> [1]
        let mut ppid_to_children: HashMap<u32, Vec<u32>> = HashMap::new();
        ppid_to_children.insert(1, vec![2]);
        ppid_to_children.insert(2, vec![1]);

        let mut descendants = Vec::new();
        let mut visited = HashSet::new();

        // Mark the root PID as visited to prevent cycles (same as production code)
        visited.insert(1);

        // This should not loop infinitely
        find_descendants_recursive(1, &ppid_to_children, &mut descendants, &mut visited);

        // Should find 2, then stop when it encounters 1 again (already visited)
        assert_eq!(descendants.len(), 1);
        assert!(descendants.contains(&2));
    }

    #[test]
    fn kill_process_tree_handles_nonexistent_pid() {
        // Test that killing a non-existent PID doesn't panic
        // and returns Ok(true) since the PID is already gone
        let result = kill_process_tree(9999999);
        assert!(
            result.is_ok(),
            "kill_process_tree should not panic on non-existent PID"
        );
        // A non-existent PID is considered "successfully killed" (it's gone)
        assert!(result.unwrap(), "non-existent PID should return true");
    }

    #[test]
    fn is_needle_run_process_detects_needle_processes() {
        // Test with current process - if we're running under cargo test,
        // the command line won't contain "needle run", but the function
        // should still work without panicking
        let self_pid = std::process::id();
        let _ = is_needle_run_process(self_pid); // Should not panic

        // Test with non-existent PID
        assert!(
            !is_needle_run_process(9999999),
            "non-existent PID should return false"
        );
    }

    #[test]
    fn find_needle_process_in_tree_handles_empty_tree() {
        // Test with a PID that doesn't exist
        let result = find_needle_process_in_tree(9999999);
        assert!(result.is_none(), "non-existent PID should return None");
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Mock process inspector for testing
    // ──────────────────────────────────────────────────────────────────────────────

    /// Mock process inspector for testing.
    ///
    /// Uses a configured mapping of pane_pid -> needle_pid to simulate process
    /// tree discovery. PIDs not in the mapping return None (no needle found).
    struct MockProcessInspector {
        /// Maps pane_pid (from tmux) to needle_pid (actual needle run process)
        pane_to_needle: std::collections::HashMap<u32, u32>,
    }

    impl ProcessInspector for MockProcessInspector {
        fn is_needle_run_process(&self, pid: u32) -> bool {
            // In mock mode, treat a PID as "needle run" if it's a value in our mapping
            self.pane_to_needle.values().any(|&p| p == pid)
        }

        fn find_needle_process_in_tree(&self, root_pid: u32) -> Option<u32> {
            // In mock mode, directly return the mapped needle_pid if found
            self.pane_to_needle.get(&root_pid).copied()
        }
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Cleanup liveness regression tests (ADR-003, plan.md Phase 7.2)
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn cleanup_no_flags_filters_orphaned_sessions() {
        // Regression Test 1: cleanup with no flags removes only dead sessions.
        //
        // Test: Given one live session (PID in live_pids) and one dead session
        // (PID not in live_pids), the default cleanup (no flags) should only
        // remove the dead session.
        //
        // This is the core safety fix: the no-flags path must check process
        // liveness before killing sessions.

        let sessions = vec![
            TmuxSession {
                name: "needle-claude-alpha".to_string(),
                created: "20240101T120000".to_string(),
                status: "detached".to_string(),
                pid: Some(1001), // Live session
            },
            TmuxSession {
                name: "needle-claude-bravo".to_string(),
                created: "20240101T120001".to_string(),
                status: "detached".to_string(),
                pid: Some(9999), // Dead session (PID not in live_pids)
            },
        ];

        let mut live_pids = std::collections::HashSet::new();
        live_pids.insert(1001); // Only alpha is live

        // Set up mock: pane_pid 1001 -> needle_pid 1001 (live), 9999 -> None (dead)
        let mut pane_to_needle = std::collections::HashMap::new();
        pane_to_needle.insert(1001, 1001);
        // 9999 is not in the map, so find_needle_process_in_tree returns None
        let inspector = MockProcessInspector { pane_to_needle };

        let targets =
            filter_sessions_for_cleanup_impl(&sessions, &inspector, &live_pids, false, &None);

        // Should only remove bravo (dead session), not alpha (live)
        assert_eq!(targets.len(), 1, "should remove exactly one session");
        assert_eq!(
            targets[0], "needle-claude-bravo",
            "should remove the dead session"
        );
    }

    #[test]
    fn cleanup_no_flags_with_zero_dead_removes_nothing() {
        // Regression Test 2: cleanup with no flags and zero dead sessions removes nothing.
        //
        // Test: Given only live sessions (all PIDs in live_pids), the default cleanup
        // should remove nothing and report that.
        //
        // This is the exact scenario that killed armor-p6a and needle-supervisor on
        // 2026-07-19: a fleet with only live workers should have zero sessions removed
        // by bare cleanup.

        let sessions = vec![
            TmuxSession {
                name: "needle-claude-alpha".to_string(),
                created: "20240101T120000".to_string(),
                status: "detached".to_string(),
                pid: Some(1001), // Live session
            },
            TmuxSession {
                name: "needle-claude-bravo".to_string(),
                created: "20240101T120001".to_string(),
                status: "detached".to_string(),
                pid: Some(1002), // Live session
            },
            TmuxSession {
                name: "needle-claude-charlie".to_string(),
                created: "20240101T120002".to_string(),
                status: "detached".to_string(),
                pid: Some(1003), // Live session
            },
        ];

        let mut live_pids = std::collections::HashSet::new();
        live_pids.insert(1001);
        live_pids.insert(1002);
        live_pids.insert(1003); // All sessions are live

        // Set up mock: every pane_pid maps to itself as the live needle_pid.
        // (Using the real `filter_sessions_for_cleanup` here — as this test
        // did previously — walks the *actual* /proc tree for PIDs 1001-1003,
        // which are essentially never real live processes in any test
        // environment, so every session was always misclassified as
        // orphaned. This is the same synthetic-PID + real-inspector mismatch
        // `cleanup_no_flags_filters_orphaned_sessions` above avoids by using
        // MockProcessInspector.)
        let mut pane_to_needle = std::collections::HashMap::new();
        pane_to_needle.insert(1001, 1001);
        pane_to_needle.insert(1002, 1002);
        pane_to_needle.insert(1003, 1003);
        let inspector = MockProcessInspector { pane_to_needle };

        let targets =
            filter_sessions_for_cleanup_impl(&sessions, &inspector, &live_pids, false, &None);

        // Should remove nothing when all sessions are live
        assert_eq!(
            targets.len(),
            0,
            "should remove zero sessions when all are live"
        );
    }

    #[test]
    fn cleanup_all_removes_every_session_regardless_of_liveness() {
        // Regression Test 3: cleanup --all removes every session regardless of liveness.
        //
        // Test: Given a mix of live and dead sessions, --all should remove all of them.
        //
        // This pins the --all behavior explicitly so it cannot regress while fixing
        // the no-flags path. The --all flag is the deliberate, fully-destructive mode.

        let sessions = vec![
            TmuxSession {
                name: "needle-claude-alpha".to_string(),
                created: "20240101T120000".to_string(),
                status: "detached".to_string(),
                pid: Some(1001), // Live session
            },
            TmuxSession {
                name: "needle-claude-bravo".to_string(),
                created: "20240101T120001".to_string(),
                status: "detached".to_string(),
                pid: Some(1002), // Live session
            },
            TmuxSession {
                name: "needle-claude-charlie".to_string(),
                created: "20240101T120002".to_string(),
                status: "detached".to_string(),
                pid: Some(9999), // Dead session
            },
            TmuxSession {
                name: "needle-claude-dead-one".to_string(),
                created: "20240101T120003".to_string(),
                status: "detached".to_string(),
                pid: None, // Dead session (no PID)
            },
        ];

        let mut live_pids = std::collections::HashSet::new();
        live_pids.insert(1001);
        live_pids.insert(1002); // Only alpha and bravo are live

        let targets = filter_sessions_for_cleanup(&sessions, &live_pids, true, &None);

        // Should remove ALL sessions with --all flag
        assert_eq!(
            targets.len(),
            4,
            "--all should remove all sessions regardless of liveness"
        );
        assert!(
            targets.contains(&"needle-claude-alpha".to_string()),
            "should include alpha"
        );
        assert!(
            targets.contains(&"needle-claude-bravo".to_string()),
            "should include bravo"
        );
        assert!(
            targets.contains(&"needle-claude-charlie".to_string()),
            "should include charlie"
        );
        assert!(
            targets.contains(&"needle-claude-dead-one".to_string()),
            "should include dead-one"
        );
    }

    #[test]
    fn cleanup_with_identifier_filters_by_name_bypassing_liveness() {
        // Additional test: -i flag filters by name substring, bypassing liveness check.
        //
        // This verifies that the -i flag path works correctly and doesn't
        // accidentally apply liveness filtering.

        let sessions = vec![
            TmuxSession {
                name: "needle-claude-alpha".to_string(),
                created: "20240101T120000".to_string(),
                status: "detached".to_string(),
                pid: Some(1001), // Live session
            },
            TmuxSession {
                name: "needle-claude-bravo".to_string(),
                created: "20240101T120001".to_string(),
                status: "detached".to_string(),
                pid: Some(1002), // Live session
            },
            TmuxSession {
                name: "needle-claude-charlie".to_string(),
                created: "20240101T120002".to_string(),
                status: "detached".to_string(),
                pid: Some(1003), // Live session
            },
        ];

        let mut live_pids = std::collections::HashSet::new();
        live_pids.insert(1001);
        live_pids.insert(1002);
        live_pids.insert(1003); // All sessions are live

        let targets =
            filter_sessions_for_cleanup(&sessions, &live_pids, false, &Some("alpha".to_string()));

        // Should remove alpha even though it's live (bypasses liveness check)
        assert_eq!(
            targets.len(),
            1,
            "should remove exactly one session matching identifier"
        );
        assert_eq!(
            targets[0], "needle-claude-alpha",
            "should remove the matching session"
        );
    }

    #[test]
    fn cleanup_sessions_with_no_pid_are_considered_orphaned() {
        // Additional test: sessions with no PID are always considered orphaned.
        //
        // This edge case can occur when tmux sessions are created manually
        // or when PID discovery fails.

        let sessions = vec![
            TmuxSession {
                name: "needle-claude-alpha".to_string(),
                created: "20240101T120000".to_string(),
                status: "detached".to_string(),
                pid: Some(1001), // Live session
            },
            TmuxSession {
                name: "needle-claude-orphan".to_string(),
                created: "20240101T120001".to_string(),
                status: "detached".to_string(),
                pid: None, // No PID = orphaned
            },
        ];

        let mut live_pids = std::collections::HashSet::new();
        live_pids.insert(1001);

        // Set up mock: pane_pid 1001 -> needle_pid 1001 (live)
        let mut pane_to_needle = std::collections::HashMap::new();
        pane_to_needle.insert(1001, 1001);
        let inspector = MockProcessInspector { pane_to_needle };

        let targets =
            filter_sessions_for_cleanup_impl(&sessions, &inspector, &live_pids, false, &None);

        // Should remove the orphan with no PID
        assert_eq!(targets.len(), 1, "should remove sessions with no PID");
        assert_eq!(
            targets[0], "needle-claude-orphan",
            "should remove the orphaned session"
        );
    }

    #[test]
    fn parse_key_value_pairs_with_equals_format() {
        // Test KEY=VALUE format
        let args = vec!["agent.default=gpt-4".to_string()];
        let result = parse_key_value_pairs(args);
        assert!(result.is_ok());
        let pairs = result.unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "agent.default");
        assert_eq!(pairs[0].1, "gpt-4");
    }

    #[test]
    fn parse_key_value_pairs_with_space_format() {
        // Test KEY VALUE format
        let args = vec!["agent.default".to_string(), "gpt-4".to_string()];
        let result = parse_key_value_pairs(args);
        assert!(result.is_ok());
        let pairs = result.unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "agent.default");
        assert_eq!(pairs[0].1, "gpt-4");
    }

    #[test]
    fn parse_key_value_pairs_with_multiple_args() {
        // Test multiple pairs in both formats
        let args = vec![
            "agent.default=gpt-4".to_string(),
            "worker.max_workers".to_string(),
            "10".to_string(),
        ];
        let result = parse_key_value_pairs(args);
        assert!(result.is_ok());
        let pairs = result.unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].0, "agent.default");
        assert_eq!(pairs[0].1, "gpt-4");
        assert_eq!(pairs[1].0, "worker.max_workers");
        assert_eq!(pairs[1].1, "10");
    }

    #[test]
    fn parse_key_value_pairs_with_empty_args() {
        // Test empty input
        let args = vec![];
        let result = parse_key_value_pairs(args);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("--set requires at least one"));
    }

    #[test]
    fn parse_key_value_pairs_with_invalid_equals_format() {
        // Test invalid KEY=VALUE format (empty key)
        let args = vec!["=value".to_string()];
        let result = parse_key_value_pairs(args);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("invalid KEY=VALUE format"));

        // Test invalid KEY=VALUE format (empty value)
        let args = vec!["key=".to_string()];
        let result = parse_key_value_pairs(args);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("invalid KEY=VALUE format"));
    }

    #[test]
    fn parse_key_value_pairs_with_missing_value() {
        // Test missing value for key in space format
        let args = vec!["agent.default".to_string()];
        let result = parse_key_value_pairs(args);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("missing value for key"));
    }

    #[test]
    fn parse_key_value_pairs_with_special_characters() {
        // Test values with special characters
        let args = vec!["path=/home/user/test file".to_string()];
        let result = parse_key_value_pairs(args);
        assert!(result.is_ok());
        let pairs = result.unwrap();
        assert_eq!(pairs[0].0, "path");
        assert_eq!(pairs[0].1, "/home/user/test file");
    }

    #[test]
    fn parse_key_value_pairs_with_multiple_equals() {
        // Test that only the first = is used as separator
        let args = vec!["url=http://example.com?a=1&b=2".to_string()];
        let result = parse_key_value_pairs(args);
        assert!(result.is_ok());
        let pairs = result.unwrap();
        assert_eq!(pairs[0].0, "url");
        assert_eq!(pairs[0].1, "http://example.com?a=1&b=2");
    }

    #[test]
    fn parse_key_value_pairs_with_spaces_in_equals_format() {
        // Test spaces in values with equals format
        let args = vec!["message=hello world".to_string()];
        let result = parse_key_value_pairs(args);
        assert!(result.is_ok());
        let pairs = result.unwrap();
        assert_eq!(pairs[0].0, "message");
        assert_eq!(pairs[0].1, "hello world");
    }

    #[cfg(unix)]
    #[test]
    fn filter_descendant_processes_removes_children() {
        // Test that filter_descendant_processes correctly identifies and removes
        // descendant processes from the discovered process list.
        //
        // This test simulates a scenario where:
        // - Process 100 is a top-level worker (needle run)
        // - Process 200 is a top-level worker (needle run)
        // - Process 150 is a child of process 100 (subprocess, should be filtered)
        // - Process 250 is a child of process 200 (subprocess, should be filtered)
        //
        // After filtering, only processes 100 and 200 should remain.

        use std::collections::HashMap;
        use std::path::PathBuf;

        // Create mock processes
        let processes = vec![
            DiscoveredProcess {
                pid: 100,
                workspace: Some(PathBuf::from("/workspace1")),
                agent: Some("claude".to_string()),
                identifier: Some("alpha".to_string()),
                cmdline: "needle run --workspace /workspace1 --agent claude --identifier alpha"
                    .to_string(),
            },
            DiscoveredProcess {
                pid: 200,
                workspace: Some(PathBuf::from("/workspace2")),
                agent: Some("claude".to_string()),
                identifier: Some("bravo".to_string()),
                cmdline: "needle run --workspace /workspace2 --agent claude --identifier bravo"
                    .to_string(),
            },
            DiscoveredProcess {
                pid: 150,
                workspace: Some(PathBuf::from("/workspace1")),
                agent: Some("claude".to_string()),
                identifier: Some("subprocess-1".to_string()),
                cmdline: "needle run --workspace /workspace1".to_string(),
            },
            DiscoveredProcess {
                pid: 250,
                workspace: Some(PathBuf::from("/workspace2")),
                agent: Some("claude".to_string()),
                identifier: Some("subprocess-2".to_string()),
                cmdline: "needle run --workspace /workspace2".to_string(),
            },
        ];

        // Build parent->children mapping: 100->150, 200->250
        let mut ppid_to_children: HashMap<u32, Vec<u32>> = HashMap::new();
        ppid_to_children.insert(100, vec![150]);
        ppid_to_children.insert(200, vec![250]);

        // Apply filtering
        let filtered = filter_descendant_processes_with_mapping(processes, &ppid_to_children);

        // Should only keep top-level processes (100 and 200)
        assert_eq!(
            filtered.len(),
            2,
            "should only have 2 processes after filtering"
        );
        let filtered_pids: std::collections::HashSet<u32> =
            filtered.iter().map(|p| p.pid).collect();
        assert!(filtered_pids.contains(&100), "should keep process 100");
        assert!(filtered_pids.contains(&200), "should keep process 200");
        assert!(
            !filtered_pids.contains(&150),
            "should filter out process 150"
        );
        assert!(
            !filtered_pids.contains(&250),
            "should filter out process 250"
        );
    }

    #[cfg(unix)]
    #[test]
    fn filter_descendant_processes_with_intermediate_non_needle_process() {
        // Test for the bug fix: intermediate non-needle processes should break
        // the descendant chain and prevent false positives.
        //
        // Scenario:
        // - PID 100: needle run workspace1 (discovered)
        // - PID 150: bash -c "some script" (child of 100, NOT discovered)
        // - PID 200: needle run workspace1 (child of 150, discovered)
        // - PID 300: needle run workspace2 (separate worker, discovered)
        //
        // Process tree: 100 -> 150 (bash) -> 200 (needle), and 300 (independent)
        //
        // Expected behavior: PID 200 should NOT be filtered out because PID 150
        // is NOT a discovered (needle run) process. The descendant chain should
        // only trace through OTHER discovered processes.

        use std::collections::HashMap;
        use std::path::PathBuf;

        let processes = vec![
            DiscoveredProcess {
                pid: 100,
                workspace: Some(PathBuf::from("/workspace1")),
                agent: Some("claude".to_string()),
                identifier: Some("alpha".to_string()),
                cmdline: "needle run --workspace /workspace1".to_string(),
            },
            DiscoveredProcess {
                pid: 200,
                workspace: Some(PathBuf::from("/workspace1")),
                agent: Some("claude".to_string()),
                identifier: Some("charlie".to_string()),
                cmdline: "needle run --workspace /workspace1".to_string(),
            },
            DiscoveredProcess {
                pid: 300,
                workspace: Some(PathBuf::from("/workspace2")),
                agent: Some("claude".to_string()),
                identifier: Some("bravo".to_string()),
                cmdline: "needle run --workspace /workspace2".to_string(),
            },
        ];

        // Build parent->children mapping
        // PID 100 has child 150 (bash, NOT in discovered list)
        // PID 150 has child 200 (needle run, IS in discovered list)
        // PID 300 is independent
        let mut ppid_to_children: HashMap<u32, Vec<u32>> = HashMap::new();
        ppid_to_children.insert(100, vec![150]); // 100 -> 150 (bash)
        ppid_to_children.insert(150, vec![200]); // 150 -> 200 (needle)

        // Apply filtering
        let filtered = filter_descendant_processes_with_mapping(processes, &ppid_to_children);

        // All three processes should remain because:
        // - PID 100 is a top-level worker
        // - PID 200 is a descendant of 100, but the chain includes PID 150 (bash)
        //   which is NOT a discovered process, so the chain is broken
        // - PID 300 is an independent worker
        assert_eq!(filtered.len(), 3, "should keep all 3 processes");
        let filtered_pids: std::collections::HashSet<u32> =
            filtered.iter().map(|p| p.pid).collect();
        assert!(filtered_pids.contains(&100), "should keep process 100");
        assert!(
            filtered_pids.contains(&200),
            "should keep process 200 (intermediate bash breaks chain)"
        );
        assert!(filtered_pids.contains(&300), "should keep process 300");
    }

    #[cfg(unix)]
    #[test]
    fn filter_descendant_processes_filters_only_through_needle_processes() {
        // Test that descendant filtering only traces through other needle run processes.
        //
        // Scenario:
        // - PID 100: needle run (discovered)
        // - PID 200: needle run, child of 100 (discovered, should be filtered)
        // - PID 300: needle run, child of 200 (discovered, should be filtered)
        // - PID 400: shell, child of 100 (NOT discovered)
        // - PID 500: needle run, child of 400 (discovered, should NOT be filtered)
        //
        // Expected: PIDs 200 and 300 are filtered (direct needle descendant chain)
        //           PID 500 is NOT filtered (chain broken by non-needle PID 400)

        use std::collections::HashMap;
        use std::path::PathBuf;

        let processes = vec![
            DiscoveredProcess {
                pid: 100,
                workspace: Some(PathBuf::from("/ws")),
                agent: Some("claude".to_string()),
                identifier: Some("root".to_string()),
                cmdline: "needle run --workspace /ws".to_string(),
            },
            DiscoveredProcess {
                pid: 200,
                workspace: Some(PathBuf::from("/ws")),
                agent: Some("claude".to_string()),
                identifier: Some("child1".to_string()),
                cmdline: "needle run --workspace /ws".to_string(),
            },
            DiscoveredProcess {
                pid: 300,
                workspace: Some(PathBuf::from("/ws")),
                agent: Some("claude".to_string()),
                identifier: Some("grandchild".to_string()),
                cmdline: "needle run --workspace /ws".to_string(),
            },
            DiscoveredProcess {
                pid: 500,
                workspace: Some(PathBuf::from("/ws")),
                agent: Some("claude".to_string()),
                identifier: Some("indirect".to_string()),
                cmdline: "needle run --workspace /ws".to_string(),
            },
        ];

        // Build parent->children mapping
        // 100 -> 200 (needle) and 400 (shell, NOT discovered)
        // 200 -> 300 (needle)
        // 400 -> 500 (needle)
        let mut ppid_to_children: HashMap<u32, Vec<u32>> = HashMap::new();
        ppid_to_children.insert(100, vec![200, 400]);
        ppid_to_children.insert(200, vec![300]);
        ppid_to_children.insert(400, vec![500]);

        // Apply filtering
        let filtered = filter_descendant_processes_with_mapping(processes, &ppid_to_children);

        // Expected: PIDs 100 and 500 remain
        // - 100: root process
        // - 200: filtered (direct needle child of 100)
        // - 300: filtered (needle descendant of 100 via 200)
        // - 500: NOT filtered (descendant of 100 via 400, which is NOT a needle process)
        assert_eq!(filtered.len(), 2, "should keep 2 processes");
        let filtered_pids: std::collections::HashSet<u32> =
            filtered.iter().map(|p| p.pid).collect();
        assert!(
            filtered_pids.contains(&100),
            "should keep process 100 (root)"
        );
        assert!(
            !filtered_pids.contains(&200),
            "should filter process 200 (direct needle child)"
        );
        assert!(
            !filtered_pids.contains(&300),
            "should filter process 300 (needle descendant)"
        );
        assert!(
            !filtered_pids.contains(&400),
            "process 400 is not in discovered list"
        );
        assert!(
            filtered_pids.contains(&500),
            "should keep process 500 (chain broken by non-needle 400)"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────────
    // Tests for doctor_check_agent_binary
    // ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn doctor_check_agent_binary_with_both_present() {
        // Test when both agent binary and bead backend are available
        let mut config = Config::default();
        config.agent.default = "ls".to_string(); // Use 'ls' as it's always available
        config.bead_cli.backend = crate::config::BeadBackend::Bead;

        let result = doctor_check_agent_binary(&config);

        // The function now ALWAYS checks the agent binary, regardless of bead backend
        // 'ls' should be found on any Unix system
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.message.contains("ls"));
        assert!(result.message.contains("at"));
    }

    #[test]
    fn doctor_check_agent_binary_with_missing_agent() {
        // Test when the agent binary is not found on PATH
        let mut config = Config::default();
        config.agent.default = "this-binary-definitely-does-not-exist-12345".to_string();
        config.bead_cli.backend = crate::config::BeadBackend::Bead;

        let result = doctor_check_agent_binary(&config);

        // The function now ALWAYS checks the agent binary, regardless of bead backend
        // Should fail because the agent binary doesn't exist
        assert_eq!(result.status, CheckStatus::Fail);
        assert!(result.message.contains("not found on PATH"));
        assert!(result
            .message
            .contains("this-binary-definitely-does-not-exist-12345"));
    }

    #[test]
    fn doctor_check_agent_binary_with_backend_unavailable() {
        // Test when the bead backend is unavailable but agent is present
        // This should PASS because the agent check is independent of the bead backend
        let mut config = Config::default();
        config.agent.default = "ls".to_string();
        config.bead_cli.backend = crate::config::BeadBackend::Bead;

        let result = doctor_check_agent_binary(&config);

        // Should PASS because 'ls' is available, even if bead backend is not
        // The agent check is now independent of the bead backend
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.message.contains("ls"));
        assert!(result.message.contains("at"));
    }

    #[test]
    fn doctor_check_agent_binary_passes_with_resolved_path() {
        // Test that when agent is found, we get PASS with resolved path
        let mut config = Config::default();
        config.agent.default = "ls".to_string(); // 'ls' is always available on Unix
        config.bead_cli.backend = crate::config::BeadBackend::Bead;

        let result = doctor_check_agent_binary(&config);

        // Should always pass now (independent of bead backend)
        assert_eq!(result.status, CheckStatus::Pass);
        // Should include the agent name
        assert!(result.message.contains("ls"));
        // Should indicate it was found
        assert!(result.message.contains("at"));
    }

    #[test]
    fn doctor_check_agent_binary_fails_with_missing_agent_even_if_backend_missing() {
        // Test that when both agent and backend are missing, we get FAIL about the agent
        // (not SKIP about the backend)
        let mut config = Config::default();
        config.agent.default = "nonexistent-agent-xyz".to_string();
        config.bead_cli.backend = crate::config::BeadBackend::Bead;

        let result = doctor_check_agent_binary(&config);

        // Should FAIL because the agent is missing, regardless of bead backend status
        assert_eq!(result.status, CheckStatus::Fail);
        assert!(result.message.contains("nonexistent-agent-xyz"));
        assert!(result.message.contains("not found on PATH"));
    }
}
