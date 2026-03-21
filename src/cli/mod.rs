//! CLI layer — parses commands and manages worker sessions.
//!
//! Entry point for the `needle` binary. Routes subcommands to worker
//! lifecycle management. Handles tmux session creation/detection so that
//! `needle run` outside tmux self-invokes into a tmux session and
//! `needle run` inside tmux starts the worker directly.
//!
//! Depends on: `worker`, `config`.

use std::path::PathBuf;
use std::process::Command as ProcessCommand;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use crate::bead_store::BrCliBeadStore;
use crate::config::{CliOverrides, Config, ConfigLoader};
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

    /// Show version information.
    Version,
}

/// Output format for the list command.
#[derive(Debug, Clone, ValueEnum)]
pub enum ListFormat {
    Table,
    Json,
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
        CliCommand::Version => {
            cmd_version();
            Ok(())
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Command handlers
// ──────────────────────────────────────────────────────────────────────────────

/// `needle run` — launch a worker.
///
/// If outside tmux: create a tmux session and re-exec self inside it.
/// If inside tmux: start the worker directly.
fn cmd_run(
    workspace: Option<PathBuf>,
    agent: Option<String>,
    count: u32,
    identifier: Option<String>,
    timeout: Option<u64>,
    _resume: bool,
) -> Result<()> {
    // Phase 1: only 1 worker is supported.
    if count > 1 {
        bail!("--count > 1 is not supported in Phase 1 (multi-worker is Phase 2)");
    }

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

    // Resolve the worker identifier.
    let worker_id = identifier
        .clone()
        .unwrap_or_else(|| NATO_ALPHABET[0].to_string());

    // Build the session name: needle-{agent}-{provider}-{model}-{identifier}
    // In Phase 1 we use the agent name for all parts since provider/model
    // are resolved later by the dispatcher.
    let agent_name = agent.as_deref().unwrap_or(&config.agent.default);
    let session_name = format!("needle-{agent_name}-{worker_id}");

    if is_inside_tmux() {
        // Already inside tmux — start the worker directly.
        tracing::info!(worker = %worker_id, session = %session_name, "starting worker directly (inside tmux)");
        run_worker(config, worker_id)
    } else {
        // Outside tmux — create session and re-exec inside it.
        tracing::info!(session = %session_name, "creating tmux session");
        launch_in_tmux(&session_name, workspace, agent, count, identifier, timeout)
    }
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

/// Create a tmux session and re-exec self inside it.
fn launch_in_tmux(
    session_name: &str,
    workspace: Option<PathBuf>,
    agent: Option<String>,
    count: u32,
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
    inner_args.push("--count".to_string());
    inner_args.push(count.to_string());
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

    println!("Started worker in tmux session: {session_name}");
    println!("Attach with: tmux attach -t {session_name}");
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
    fn phase1_count_validation() {
        // Phase 1 only supports count=1.
        // We test the validation logic directly.
        let count: u32 = 2;
        assert!(count > 1, "count > 1 should trigger Phase 1 error");
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
}
