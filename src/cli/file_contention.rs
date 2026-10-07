//! `needle contention` — operate the checkout-local file-contention markers
//! (plan Phase 20, needle-f0552fec).
//!
//! These commands are the surface a harness pre-write hook (or an interactive
//! agent) calls. They operate only on `<repo>/.needle/locks/` and never stage
//! it. Identity comes from flags, falling back to the environment NEEDLE
//! exports to dispatched agents (`file_contention::env`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use crate::config::{file_contention_for_workspace, FileContentionConfig};
use crate::file_contention::assessment::{
    assess, classify, reclaim_or_acquire, Assessment, HolderStatus, LocalHolderStatus,
};
use crate::file_contention::env;
use crate::file_contention::git_safety;
use crate::file_contention::store::{
    AcquireOutcome, MarkerRecord, MarkerStore, Participant, PathIntent, WriteIntent,
};

/// Exit codes. Stable: hooks branch on them.
pub mod exit {
    /// Done: written, released, listed, or every checked path is writable.
    pub const OK: i32 = 0;
    /// Invalid request (bad path, missing identity).
    pub const USAGE: i32 = 2;
    /// Another participant is active on a requested path: do not write.
    pub const CONFLICT: i32 = 3;
    /// Stale-modified or ambiguous marker: preserve the file, route it.
    pub const NEEDS_ATTENTION: i32 = 4;
    /// The marker subsystem could not be used: coverage is degraded, the path
    /// was NOT checked.
    pub const DEGRADED: i32 = 5;
}

/// Schema tag on every `--json` document.
pub const OUTPUT_SCHEMA: &str = "needle.file_contention.v1";

#[derive(Debug, Subcommand)]
pub enum ContentionCommand {
    /// Assess write-intent paths without recording anything.
    Check {
        #[arg(long = "path", required = true)]
        paths: Vec<PathBuf>,
        #[command(flatten)]
        identity: IdentityArgs,
    },
    /// Record write intent for one or more paths (all-or-none).
    Acquire {
        #[arg(long = "path")]
        paths: Vec<PathBuf>,
        #[arg(long, value_enum, default_value_t = IntentArg::Modify)]
        intent: IntentArg,
        /// Rename source; recorded together with --rename-to.
        #[arg(long, requires = "rename_to")]
        rename_from: Option<PathBuf>,
        /// Rename destination; recorded together with --rename-from.
        #[arg(long, requires = "rename_from")]
        rename_to: Option<PathBuf>,
        #[command(flatten)]
        identity: IdentityArgs,
    },
    /// Renew markers owned by the caller (all, or only --path).
    Renew {
        #[arg(long = "path")]
        paths: Vec<PathBuf>,
        #[command(flatten)]
        identity: IdentityArgs,
    },
    /// Remove markers owned by the caller (all, or only --path).
    Release {
        #[arg(long = "path")]
        paths: Vec<PathBuf>,
        #[command(flatten)]
        identity: IdentityArgs,
    },
    /// List markers in this checkout with holder and assessment.
    List {
        #[command(flatten)]
        identity: IdentityArgs,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum IntentArg {
    Create,
    Modify,
    Delete,
}

impl From<IntentArg> for WriteIntent {
    fn from(value: IntentArg) -> Self {
        match value {
            IntentArg::Create => WriteIntent::Create,
            IntentArg::Modify => WriteIntent::Modify,
            IntentArg::Delete => WriteIntent::Delete,
        }
    }
}

/// Caller identity and repository. Each flag falls back to the variable
/// NEEDLE exports to dispatched agents.
#[derive(Debug, Clone, Default, Args)]
pub struct IdentityArgs {
    /// Repository root (default: $NEEDLE_FILE_CONTENTION_REPO, then the git
    /// top-level of the current directory).
    #[arg(long)]
    pub repo: Option<PathBuf>,
    /// Bead being worked (default: $NEEDLE_BEAD_ID). Absent for interactive sessions.
    #[arg(long)]
    pub bead: Option<String>,
    /// Attempt identity (default: $NEEDLE_ATTEMPT_ID).
    #[arg(long)]
    pub attempt: Option<String>,
    /// Interactive session identity when no attempt exists (default: $NEEDLE_SESSION_ID).
    #[arg(long)]
    pub session: Option<String>,
    /// Worker identity (default: $NEEDLE_WORKER_ID, else "interactive").
    #[arg(long)]
    pub worker: Option<String>,
    /// Holder pid recorded for liveness (default: this command's parent,
    /// i.e. the harness that ran the hook).
    #[arg(long)]
    pub pid: Option<u32>,
    /// Lease override in seconds (default: $NEEDLE_FILE_CONTENTION_LEASE_SECS,
    /// then the workspace's file_contention.lease_secs).
    #[arg(long)]
    pub lease_secs: Option<u64>,
    /// Machine-readable output.
    #[arg(long)]
    pub json: bool,
}

/// Environment lookup, injectable so tests never mutate process env.
pub type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Process environment lookup for the real CLI.
pub fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Everything a command needs, resolved once.
struct Resolved {
    store: MarkerStore,
    workspace: FileContentionConfig,
    lease: Duration,
    json: bool,
    identity: IdentityArgs,
}

/// Execute a command. Returns (exit code, rendered output).
pub fn execute(
    command: &ContentionCommand,
    lookup: EnvLookup<'_>,
    status: &dyn HolderStatus,
    now: DateTime<Utc>,
) -> (i32, String) {
    match execute_inner(command, lookup, status, now) {
        Ok(result) => result,
        Err(err) => {
            let json = identity_of(command).json;
            let degraded = err.downcast_ref::<Degraded>().is_some();
            let code = if degraded {
                exit::DEGRADED
            } else {
                exit::USAGE
            };
            let rendered = if json {
                serde_json::json!({
                    "schema": OUTPUT_SCHEMA,
                    "result": if degraded { "degraded" } else { "error" },
                    "error": format!("{err:#}"),
                })
                .to_string()
            } else {
                format!("error: {err:#}")
            };
            (code, rendered)
        }
    }
}

/// Run the real CLI command: process environment and local liveness.
pub fn run(command: ContentionCommand) -> Result<()> {
    let config = crate::config::ConfigLoader::load_global().unwrap_or_default();
    let heartbeats = crate::state_dir::root_for(&config.workspace.home)
        .join("state")
        .join("heartbeats");
    let status = LocalHolderStatus::new(Some(&heartbeats), Duration::from_secs(120));
    let (code, rendered) = execute(&command, &process_env, &status, Utc::now());
    println!("{rendered}");
    std::process::exit(code);
}

#[derive(Debug)]
struct Degraded(String);
impl std::fmt::Display for Degraded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "file-contention markers unavailable: {}", self.0)
    }
}
impl std::error::Error for Degraded {}

fn identity_of(command: &ContentionCommand) -> &IdentityArgs {
    match command {
        ContentionCommand::Check { identity, .. }
        | ContentionCommand::Acquire { identity, .. }
        | ContentionCommand::Renew { identity, .. }
        | ContentionCommand::Release { identity, .. }
        | ContentionCommand::List { identity } => identity,
    }
}

fn execute_inner(
    command: &ContentionCommand,
    lookup: EnvLookup<'_>,
    status: &dyn HolderStatus,
    now: DateTime<Utc>,
) -> Result<(i32, String)> {
    let ctx = resolve(identity_of(command), lookup)?;
    match command {
        ContentionCommand::Check { paths, .. } => check(&ctx, paths, lookup, status, now),
        ContentionCommand::Acquire {
            paths,
            intent,
            rename_from,
            rename_to,
            ..
        } => {
            let mut wanted: Vec<PathIntent> = paths
                .iter()
                .map(|p| PathIntent {
                    path: p.clone(),
                    intent: (*intent).into(),
                })
                .collect();
            if let (Some(from), Some(to)) = (rename_from, rename_to) {
                wanted.push(PathIntent {
                    path: from.clone(),
                    intent: WriteIntent::RenameFrom,
                });
                wanted.push(PathIntent {
                    path: to.clone(),
                    intent: WriteIntent::RenameTo,
                });
            }
            if wanted.is_empty() {
                bail!("acquire needs at least one --path or a --rename-from/--rename-to pair");
            }
            acquire(&ctx, &wanted, lookup, status, now)
        }
        ContentionCommand::Renew { paths, .. } => renew(&ctx, paths, lookup, now),
        ContentionCommand::Release { paths, .. } => release(&ctx, paths, lookup),
        ContentionCommand::List { .. } => list(&ctx, lookup, status, now),
    }
}

fn resolve(identity: &IdentityArgs, lookup: EnvLookup<'_>) -> Result<Resolved> {
    let repo = match identity
        .repo
        .clone()
        .or_else(|| lookup(env::REPO).map(PathBuf::from))
    {
        Some(repo) => repo,
        None => git_toplevel(Path::new("."))?,
    };
    let store = MarkerStore::open(&repo).map_err(|e| Degraded(format!("{e:#}")))?;
    let workspace = file_contention_for_workspace(store.repo_root())?;
    let lease_secs = identity
        .lease_secs
        .or_else(|| lookup(env::LEASE_SECS).and_then(|v| v.parse().ok()))
        .unwrap_or(workspace.lease_secs);
    if lease_secs < FileContentionConfig::MIN_LEASE_SECS {
        bail!(
            "lease must be at least {}s (got {lease_secs})",
            FileContentionConfig::MIN_LEASE_SECS
        );
    }
    Ok(Resolved {
        store,
        workspace,
        lease: Duration::from_secs(lease_secs),
        json: identity.json,
        identity: identity.clone(),
    })
}

fn participant(ctx: &Resolved, lookup: EnvLookup<'_>) -> Result<Participant> {
    let identity = &ctx.identity;
    let pick = |flag: &Option<String>, var: &str| flag.clone().or_else(|| lookup(var));
    let participant = Participant {
        bead_id: pick(&identity.bead, env::BEAD_ID),
        attempt_id: pick(&identity.attempt, env::ATTEMPT_ID),
        session_id: pick(&identity.session, env::SESSION_ID),
        worker_id: pick(&identity.worker, env::WORKER_ID)
            .unwrap_or_else(|| "interactive".to_string()),
        host: gethostname::gethostname().to_string_lossy().into_owned(),
        pid: identity
            .pid
            .or_else(|| lookup(env::HOLDER_PID).and_then(|v| v.parse().ok()))
            .unwrap_or_else(parent_pid),
    };
    participant
        .validate()
        .context("identify the caller with --attempt (worker) or --session (interactive agent)")?;
    Ok(participant)
}

fn parent_pid() -> u32 {
    // SAFETY: getppid has no preconditions and cannot fail.
    let ppid = unsafe { libc::getppid() };
    u32::try_from(ppid).unwrap_or(0)
}

fn git_toplevel(dir: &Path) -> Result<PathBuf> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("run git rev-parse --show-toplevel")?;
    if !output.status.success() {
        bail!("not inside a git checkout; pass --repo");
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

#[derive(Debug, Serialize)]
struct HolderView {
    bead_id: Option<String>,
    attempt_id: Option<String>,
    session_id: Option<String>,
    worker_id: String,
    host: String,
    pid: u32,
    intent: WriteIntent,
    claimed_at: DateTime<Utc>,
    last_renewed_at: DateTime<Utc>,
    lease_expires_at: DateTime<Utc>,
    age_secs: i64,
}

impl HolderView {
    fn of(record: &MarkerRecord, now: DateTime<Utc>) -> Self {
        Self {
            bead_id: record.bead_id.clone(),
            attempt_id: record.attempt_id.clone(),
            session_id: record.session_id.clone(),
            worker_id: record.worker_id.clone(),
            host: record.host.clone(),
            pid: record.pid,
            intent: record.intent,
            claimed_at: record.claimed_at,
            last_renewed_at: record.last_renewed_at,
            lease_expires_at: record.lease_expires_at,
            age_secs: (now - record.claimed_at).num_seconds(),
        }
    }
}

#[derive(Debug, Serialize)]
struct PathView {
    path: String,
    assessment: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    holder: Option<HolderView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

fn path_view(path: String, assessment: &Assessment, now: DateTime<Utc>) -> PathView {
    let detail = match assessment {
        Assessment::ActiveOther { lease_overdue: true, .. } => {
            Some("holder is live but past its nominal lease".to_string())
        }
        Assessment::StaleModified { holder, .. } => Some(match &holder.bead_id {
            Some(bead) => format!("file changed after the claim; resume through bead {bead}"),
            None => "file changed after the claim by an interactive session; inspect before writing".to_string(),
        }),
        Assessment::VerifiedClosed { file, .. } if file.is_modified() => {
            Some("owning bead is closed but the file changed after the claim; file a follow-up inspection".to_string())
        }
        Assessment::Ambiguous { reason, .. } => Some(reason.clone()),
        _ => None,
    };
    PathView {
        path,
        assessment: assessment.kind(),
        holder: assessment.holder().map(|h| HolderView::of(h, now)),
        detail,
    }
}

/// Exit code for a set of assessments: conflict beats attention beats ok.
fn exit_for(assessments: &[&Assessment]) -> i32 {
    if assessments
        .iter()
        .any(|a| matches!(a, Assessment::ActiveOther { .. }))
    {
        exit::CONFLICT
    } else if assessments.iter().any(|a| !a.permits_write()) {
        exit::NEEDS_ATTENTION
    } else {
        exit::OK
    }
}

fn render(ctx: &Resolved, value: serde_json::Value, human: String) -> String {
    if ctx.json {
        value.to_string()
    } else {
        human
    }
}

fn human_paths(views: &[PathView]) -> String {
    views
        .iter()
        .map(|v| {
            let holder = v
                .holder
                .as_ref()
                .map(|h| {
                    format!(
                        " held by {} (bead {}, {} {}, claimed {})",
                        h.worker_id,
                        h.bead_id.as_deref().unwrap_or("-"),
                        if h.attempt_id.is_some() {
                            "attempt"
                        } else {
                            "session"
                        },
                        h.attempt_id
                            .as_deref()
                            .or(h.session_id.as_deref())
                            .unwrap_or("-"),
                        h.claimed_at.to_rfc3339()
                    )
                })
                .unwrap_or_default();
            let detail = v
                .detail
                .as_ref()
                .map(|d| format!(" — {d}"))
                .unwrap_or_default();
            format!("{:<16} {}{holder}{detail}", v.assessment, v.path)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn check(
    ctx: &Resolved,
    paths: &[PathBuf],
    lookup: EnvLookup<'_>,
    status: &dyn HolderStatus,
    now: DateTime<Utc>,
) -> Result<(i32, String)> {
    // A caller identity is optional for check: without one, nothing is "ours".
    let caller = participant(ctx, lookup).ok();
    let mut assessed = Vec::new();
    for path in paths {
        let rel = ctx.store.normalize(path)?;
        let assessment = assess(&ctx.store, &rel, caller.as_ref(), status, now)?;
        assessed.push((rel, assessment));
    }
    let code = exit_for(&assessed.iter().map(|(_, a)| a).collect::<Vec<_>>());
    let views: Vec<_> = assessed
        .iter()
        .map(|(p, a)| path_view(p.clone(), a, now))
        .collect();
    let value = serde_json::json!({
        "schema": OUTPUT_SCHEMA,
        "command": "check",
        "coverage": coverage_name(&ctx.workspace),
        "result": result_name(code),
        "paths": views,
    });
    let human = human_paths(&views);
    Ok((code, render(ctx, value, human)))
}

fn acquire(
    ctx: &Resolved,
    wanted: &[PathIntent],
    lookup: EnvLookup<'_>,
    status: &dyn HolderStatus,
    now: DateTime<Utc>,
) -> Result<(i32, String)> {
    let caller = participant(ctx, lookup)?;
    // Validate every path before deciding anything.
    for p in wanted {
        ctx.store.normalize(&p.path)?;
    }
    if !ctx.workspace.enabled {
        let value = serde_json::json!({
            "schema": OUTPUT_SCHEMA,
            "command": "acquire",
            "coverage": "supported_disabled",
            "result": "disabled",
            "written": false,
        });
        let human =
            "file contention is not enabled for this workspace; no marker written".to_string();
        return Ok((exit::OK, render(ctx, value, human)));
    }
    git_safety::ensure_local_exclude(ctx.store.repo_root())
        .map_err(|e| Degraded(format!("{e:#}")))?;
    let outcome = reclaim_or_acquire(&ctx.store, &caller, wanted, ctx.lease, status, now)
        .map_err(|e| Degraded(format!("{e:#}")))?;
    match outcome {
        AcquireOutcome::Acquired(records) => {
            let paths: Vec<_> = records.iter().map(|r| r.path.clone()).collect();
            let value = serde_json::json!({
                "schema": OUTPUT_SCHEMA,
                "command": "acquire",
                "coverage": "enabled",
                "result": "acquired",
                "written": true,
                "paths": paths,
                "lease_expires_at": records.first().map(|r| r.lease_expires_at),
            });
            let human = format!("acquired {}", paths.join(", "));
            Ok((exit::OK, render(ctx, value, human)))
        }
        AcquireOutcome::Conflicts(conflicts) => {
            let mut assessed = Vec::new();
            for conflict in &conflicts {
                let assessment = classify(
                    &ctx.store,
                    &conflict.path,
                    Some(&conflict.existing),
                    Some(&caller),
                    status,
                    now,
                )?;
                assessed.push((conflict.path.clone(), assessment));
            }
            let code = exit_for(&assessed.iter().map(|(_, a)| a).collect::<Vec<_>>());
            let code = if code == exit::OK {
                exit::NEEDS_ATTENTION
            } else {
                code
            };
            let views: Vec<_> = assessed
                .iter()
                .map(|(p, a)| path_view(p.clone(), a, now))
                .collect();
            let value = serde_json::json!({
                "schema": OUTPUT_SCHEMA,
                "command": "acquire",
                "coverage": "enabled",
                "result": "conflict",
                "written": false,
                "conflicts": views,
            });
            let human = format!("NOT acquired (nothing written):\n{}", human_paths(&views));
            Ok((code, render(ctx, value, human)))
        }
    }
}

fn renew(
    ctx: &Resolved,
    paths: &[PathBuf],
    lookup: EnvLookup<'_>,
    now: DateTime<Utc>,
) -> Result<(i32, String)> {
    let caller = participant(ctx, lookup)?;
    if !ctx.workspace.enabled {
        let value = serde_json::json!({
            "schema": OUTPUT_SCHEMA, "command": "renew",
            "coverage": "supported_disabled", "result": "disabled", "renewed": [],
        });
        return Ok((
            exit::OK,
            render(
                ctx,
                value,
                "file contention is not enabled for this workspace".into(),
            ),
        ));
    }
    let only = (!paths.is_empty()).then_some(paths);
    let renewed = ctx
        .store
        .renew(&caller, only, ctx.lease, now)
        .map_err(|e| Degraded(format!("{e:#}")))?;
    let value = serde_json::json!({
        "schema": OUTPUT_SCHEMA, "command": "renew",
        "coverage": "enabled", "result": "ok", "renewed": renewed,
    });
    let human = format!("renewed {} marker(s)", renewed.len());
    Ok((exit::OK, render(ctx, value, human)))
}

fn release(ctx: &Resolved, paths: &[PathBuf], lookup: EnvLookup<'_>) -> Result<(i32, String)> {
    let caller = participant(ctx, lookup)?;
    let only = (!paths.is_empty()).then_some(paths);
    let released = ctx
        .store
        .release(&caller, only)
        .map_err(|e| Degraded(format!("{e:#}")))?;
    let value = serde_json::json!({
        "schema": OUTPUT_SCHEMA, "command": "release",
        "result": "ok", "released": released,
    });
    let human = format!("released {} marker(s)", released.len());
    Ok((exit::OK, render(ctx, value, human)))
}

fn list(
    ctx: &Resolved,
    lookup: EnvLookup<'_>,
    status: &dyn HolderStatus,
    now: DateTime<Utc>,
) -> Result<(i32, String)> {
    let caller = participant(ctx, lookup).ok();
    let markers = ctx.store.list().map_err(|e| Degraded(format!("{e:#}")))?;
    let mut views = Vec::new();
    for (rel, read) in &markers {
        let assessment = classify(&ctx.store, rel, Some(read), caller.as_ref(), status, now)?;
        views.push(path_view(rel.clone(), &assessment, now));
    }
    let value = serde_json::json!({
        "schema": OUTPUT_SCHEMA,
        "command": "list",
        "coverage": coverage_name(&ctx.workspace),
        "result": "ok",
        "markers": views,
    });
    let human = if views.is_empty() {
        "no file-contention markers".to_string()
    } else {
        human_paths(&views)
    };
    Ok((exit::OK, render(ctx, value, human)))
}

/// Workspace view of coverage for CLI output: the CLI is itself a capable
/// participant, so only the workspace opt-in decides.
fn coverage_name(workspace: &FileContentionConfig) -> &'static str {
    if workspace.enabled {
        "enabled"
    } else {
        "supported_disabled"
    }
}

fn result_name(code: i32) -> &'static str {
    match code {
        exit::OK => "ok",
        exit::CONFLICT => "conflict",
        exit::NEEDS_ATTENTION => "needs_attention",
        exit::DEGRADED => "degraded",
        _ => "error",
    }
}
