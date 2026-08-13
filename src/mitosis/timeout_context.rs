//! Timeout decomposition context capture for mitosis.
//!
//! When an agent times out on a qualifying long-running bead, this module
//! captures the context needed to split the bead before releasing it back to
//! the pool. The context includes:
//!
//! - Bead definition (title, body, workspace)
//! - Timeout reason and timing (from timeout eligibility analysis)
//! - Trace/transcript reference (path to stdout/stderr trace files)
//! - Pre-dispatch Git state (HEAD SHA, notes hash from predispatch snapshot)
//! - Post-attempt Git state (current HEAD, dirty paths)
//! - Committed work (git log between pre and post HEAD)
//! - Remaining dirty paths (uncommitted changes)
//!
//! The context is stored on disk under `.beads/timeout-context/<bead-id>.json`
//! and can be loaded later by a mitosis agent to perform the actual split.
//!
//! Depends on: `types`, `mitosis::timeout_eligibility`, `validation::predispatch`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::mitosis::timeout_eligibility::TimeoutEligibility;
use crate::types::{Bead, BeadId};
use crate::validation::predispatch::{self, PreDispatch};

/// Captured context for a timeout that may qualify for mitosis.
///
/// This structure contains all the information needed to understand
/// what work was in progress when the agent timed out, so that a later
/// mitosis analysis can intelligently split the bead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeoutDecompositionContext {
    /// When this context was captured.
    pub captured_at: DateTime<Utc>,

    /// Bead definition at the time of timeout.
    pub bead_def: BeadDefinition,

    /// Timeout classification and reasoning.
    pub timeout: TimeoutContext,

    /// Reference to agent execution trace files.
    pub trace_reference: TraceReference,

    /// Git state before and after the agent attempt.
    pub git_state: GitStateContext,

    /// Whether the timeout qualifies for mitosis (based on eligibility analysis).
    pub qualifies_for_mitosis: bool,
}

/// Summary of the bead that timed out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeadDefinition {
    /// Bead ID.
    pub bead_id: BeadId,
    /// Bead title.
    pub title: String,
    /// First 500 chars of bead body (truncated to keep context small).
    pub body_preview: String,
    /// Workspace directory.
    pub workspace: PathBuf,
    /// Bead labels.
    pub labels: Vec<String>,
}

/// Classification and reasoning for the timeout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeoutContext {
    /// Eligibility analysis result.
    pub eligibility: TimeoutEligibilityRecord,
    /// Wall-clock duration of the agent execution.
    pub duration_secs: u64,
}

/// Simplified eligibility record (serializable version of TimeoutEligibility).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeoutEligibilityRecord {
    /// Whether the timeout qualifies for mitosis.
    pub is_eligible: bool,
    /// Human-readable reason for the decision.
    pub reason: String,
    /// Classification of timeout origin.
    pub origin: TimeoutOriginRecord,
}

/// Simplified timeout origin (serializable version).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TimeoutOriginRecord {
    AgentWallclock { timeout_duration_secs: u64 },
    HandlerTimeout { gate_name: Option<String> },
    BeadStoreTimeout,
    OutcomeProcessingTimeout,
}

/// Reference to agent execution trace files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceReference {
    /// Directory containing trace files (relative to workspace).
    pub trace_dir: PathBuf,
    /// Path to stdout trace file.
    pub stdout_path: PathBuf,
    /// Path to stderr trace file.
    pub stderr_path: PathBuf,
}

/// Git state before and after the agent attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitStateContext {
    /// Pre-dispatch snapshot (if available).
    pub pre_dispatch: Option<PreDispatchRecord>,
    /// Post-attempt Git state.
    pub post_attempt: PostAttemptGitState,
}

/// Simplified pre-dispatch record (serializable version).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreDispatchRecord {
    /// Git HEAD SHA before agent ran.
    pub head_sha: Option<String>,
    /// SHA-256 of bead notes before agent ran.
    pub notes_hash: Option<String>,
}

/// Git state after the agent attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostAttemptGitState {
    /// Git HEAD SHA after agent ran.
    pub head_sha: Option<String>,
    /// Paths with uncommitted changes (dirty paths).
    pub dirty_paths: BTreeSet<String>,
    /// Summary of committed work (if HEAD changed).
    pub committed_work: Option<CommittedWorkSummary>,
}

/// Summary of work committed between pre and post dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommittedWorkSummary {
    /// Number of commits created during the attempt.
    pub commit_count: usize,
    /// Commit subjects (for context, max 10).
    pub commit_subjects: Vec<String>,
    /// Estimated lines changed (from git diff --stat).
    pub lines_changed: Option<String>,
}

impl From<predispatch::PreDispatch> for PreDispatchRecord {
    fn from(pre: PreDispatch) -> Self {
        PreDispatchRecord {
            head_sha: pre.head_sha,
            notes_hash: pre.notes_hash,
        }
    }
}

impl TimeoutContext {
    /// Create a new timeout context from eligibility analysis and duration.
    pub fn new(eligibility: TimeoutEligibility, duration_secs: u64) -> Self {
        let (is_eligible, reason) = match &eligibility {
            TimeoutEligibility::Eligible { reason } => (true, reason.clone()),
            TimeoutEligibility::NotEligible { reason } => (false, reason.clone()),
        };

        TimeoutContext {
            eligibility: TimeoutEligibilityRecord {
                is_eligible,
                reason,
                // Extract origin from the eligibility reason (best-effort parsing)
                origin: extract_origin_from_reason(&eligibility),
            },
            duration_secs,
        }
    }
}

/// Extract timeout origin from an eligibility decision.
fn extract_origin_from_reason(eligibility: &TimeoutEligibility) -> TimeoutOriginRecord {
    // Parse the reason string to infer origin (best-effort)
    let reason = eligibility.reason();
    let reason_lower = reason.to_lowercase();

    if reason_lower.contains("agent wall-clock timeout") {
        TimeoutOriginRecord::AgentWallclock {
            timeout_duration_secs: 3600, // Default, not critical for context
        }
    } else if reason_lower.contains("handler timeout") {
        TimeoutOriginRecord::HandlerTimeout {
            gate_name: extract_gate_name_from_reason(reason),
        }
    } else if reason_lower.contains("bead-store timeout") {
        TimeoutOriginRecord::BeadStoreTimeout
    } else if reason_lower.contains("outcome-processing timeout") {
        TimeoutOriginRecord::OutcomeProcessingTimeout
    } else {
        // Default to agent wall-clock timeout
        TimeoutOriginRecord::AgentWallclock {
            timeout_duration_secs: 3600,
        }
    }
}

/// Extract gate name from a handler timeout reason string.
fn extract_gate_name_from_reason(reason: &str) -> Option<String> {
    // Look for patterns like "gate 'cargo-test' timed out"
    let patterns = [
        "gate '",
        "gate \"",
        "validation gate '",
        "validation gate \"",
    ];

    for pattern in &patterns {
        if let Some(idx) = reason.find(pattern) {
            let start = idx + pattern.len();
            // Find the next quote character by iterating chars
            for (offset, c) in reason[start..].char_indices() {
                if c == '\'' || c == '"' {
                    return Some(reason[start..start + offset].to_string());
                }
            }
        }
    }

    None
}

/// Path where timeout context is stored for a bead.
fn context_path(workspace: &Path, bead_id: &BeadId) -> PathBuf {
    workspace
        .join(".beads")
        .join("timeout-context")
        .join(format!("{}.json", bead_id.as_ref()))
}

/// Capture timeout decomposition context for a bead.
///
/// This function captures all available context about a timeout event,
/// including Git state, trace references, and eligibility analysis.
/// Errors are handled gracefully — a failure to capture context should
/// not block the worker from releasing the bead.
///
/// # Arguments
///
/// * `bead` - The bead that timed out
/// * `workspace` - The workspace directory
/// * `eligibility` - Timeout eligibility analysis result
/// * `duration_secs` - Wall-clock duration of the agent execution
///
/// # Returns
///
/// * `Ok(Some(context))` - Context was captured successfully
/// * `Ok(None)` - Context capture was skipped (e.g., not a git repo)
/// * `Err(e)` - Context capture failed (but this should not block timeout handling)
pub async fn capture_timeout_context(
    bead: &Bead,
    workspace: &Path,
    eligibility: TimeoutEligibility,
    duration_secs: u64,
) -> Result<Option<TimeoutDecompositionContext>> {
    tracing::info!(
        bead_id = %bead.id,
        workspace = %workspace.display(),
        "capturing timeout decomposition context"
    );

    // Load pre-dispatch snapshot (if available)
    let pre_dispatch = predispatch::load(workspace, &bead.id)
        .await
        .map(|pre| Some(PreDispatchRecord::from(pre)))
        .unwrap_or(None);

    // Capture post-attempt Git state
    let post_attempt = capture_post_attempt_git_state(workspace, &pre_dispatch).await?;

    // Create trace reference
    let trace_dir = workspace
        .join(".beads")
        .join("traces")
        .join(bead.id.as_ref());
    let trace_reference = TraceReference {
        trace_dir: trace_dir.clone(),
        stdout_path: trace_dir.join("stdout.txt"),
        stderr_path: trace_dir.join("stderr.txt"),
    };

    // Create bead definition (truncate body to keep context small)
    let body_preview = bead
        .body
        .as_ref()
        .map(|b| {
            let chars: Vec<char> = b.chars().take(500).collect();
            let truncated: String = chars.into_iter().collect();
            if b.len() > 500 {
                format!("{}...", truncated)
            } else {
                truncated
            }
        })
        .unwrap_or_default();

    let bead_def = BeadDefinition {
        bead_id: bead.id.clone(),
        title: bead.title.clone(),
        body_preview,
        workspace: workspace.to_path_buf(),
        labels: bead.labels.clone(),
    };

    // Determine if this qualifies for mitosis
    let qualifies_for_mitosis = eligibility.is_eligible();

    let context = TimeoutDecompositionContext {
        captured_at: Utc::now(),
        bead_def,
        timeout: TimeoutContext::new(eligibility, duration_secs),
        trace_reference,
        git_state: GitStateContext {
            pre_dispatch,
            post_attempt,
        },
        qualifies_for_mitosis,
    };

    Ok(Some(context))
}

/// Capture the post-attempt Git state of a workspace.
///
/// Returns the current HEAD SHA and a list of dirty paths.
/// If the workspace is not a git repository, returns a default state.
async fn capture_post_attempt_git_state(
    workspace: &Path,
    pre_dispatch: &Option<PreDispatchRecord>,
) -> Result<PostAttemptGitState> {
    let head_sha = git_head(workspace).await;
    let dirty_paths = git_dirty_paths(workspace).await;

    // Compute committed work by comparing pre and post HEAD
    let committed_work = if let (Some(current_head), Some(pre_head)) = (
        &head_sha,
        pre_dispatch.as_ref().and_then(|p| p.head_sha.as_ref()),
    ) {
        // HEAD changed - compute commit summary
        compute_committed_work(workspace, pre_head, current_head).await
    } else {
        None
    };

    Ok(PostAttemptGitState {
        head_sha,
        dirty_paths,
        committed_work,
    })
}

/// Get the current Git HEAD SHA for a workspace.
async fn git_head(workspace: &Path) -> Option<String> {
    run_git(workspace, &["rev-parse", "HEAD"]).await
}

/// Get a list of dirty (modified) paths in the workspace.
async fn git_dirty_paths(workspace: &Path) -> BTreeSet<String> {
    let output = match run_git_raw(workspace, &["status", "--porcelain"]).await {
        Some(out) => out,
        None => return BTreeSet::new(),
    };

    output
        .lines()
        .filter_map(|line| {
            // git status --porcelain format: XY PATH
            // X = staged, Y = worktree
            // Untracked entries (Y == '?') are excluded; everything else the
            // agent touched is reported, including staged-only changes
            // (Y == ' '), which still represent work the agent performed.
            let chars: Vec<char> = line.chars().collect();
            if chars.len() < 4 {
                return None;
            }

            // Skip untracked files (??), only show modified/changed files
            let worktree_status = chars[1];
            if worktree_status == '?' {
                return None;
            }

            // Extract path (skip first 3 chars: "XY " or "XY\t")
            let path = line.chars().skip(3).collect::<String>();
            Some(path)
        })
        .collect()
}

/// Run a git command and return stdout if successful.
async fn run_git(workspace: &Path, args: &[&str]) -> Option<String> {
    Some(run_git_raw(workspace, args).await?.trim().to_string())
}

/// Run a git command and return stdout exactly as produced.
///
/// Callers that parse a column-oriented format must use this rather than
/// [`run_git`]: `git status --porcelain` encodes the staged/worktree state in
/// the first two columns, so trimming the combined stdout silently deletes the
/// leading space of the *first* line whenever the file has no staged change —
/// which shifts that line's path by one character.
async fn run_git_raw(workspace: &Path, args: &[&str]) -> Option<String> {
    let git_dir = workspace.join(".git");
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(workspace)
        .env("GIT_DIR", &git_dir)
        .env("GIT_WORK_TREE", workspace)
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Compute summary of committed work between two Git SHAs.
///
/// Returns None if the SHAs are the same or git commands fail.
async fn compute_committed_work(
    workspace: &Path,
    pre_sha: &str,
    post_sha: &str,
) -> Option<CommittedWorkSummary> {
    // Count commits between pre and post
    let commit_count = run_git(
        workspace,
        &[
            "rev-list",
            "--count",
            &format!("{}...{}", pre_sha, post_sha),
        ],
    )
    .await?
    .parse::<usize>()
    .unwrap_or(0);

    if commit_count == 0 {
        return None;
    }

    // Get commit subjects (max 10)
    let commit_subjects = run_git(
        workspace,
        &[
            "log",
            "--pretty=format:%s",
            &format!("{}...{}", pre_sha, post_sha),
            "-n",
            "10",
        ],
    )
    .await
    .unwrap_or_default()
    .lines()
    .map(|s| s.to_string())
    .collect();

    // Get diff --stat for lines changed
    let lines_changed = run_git(
        workspace,
        &["diff", "--stat", &format!("{}...{}", pre_sha, post_sha)],
    )
    .await;

    Some(CommittedWorkSummary {
        commit_count,
        commit_subjects,
        lines_changed,
    })
}

/// Write timeout context to disk.
///
/// Creates the `.beads/timeout-context/` directory if it doesn't exist
/// and writes the context as JSON.
pub async fn write_timeout_context(
    workspace: &Path,
    bead_id: &BeadId,
    context: &TimeoutDecompositionContext,
) -> Result<()> {
    let path = context_path(workspace, bead_id);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating timeout-context directory {}", parent.display()))?;
    }

    let encoded =
        serde_json::to_vec_pretty(context).context("serializing timeout decomposition context")?;

    tokio::fs::write(&path, encoded)
        .await
        .with_context(|| format!("writing timeout context to {}", path.display()))?;

    tracing::debug!(
        bead_id = %bead_id,
        path = %path.display(),
        "timeout decomposition context written"
    );

    Ok(())
}

/// Load timeout context from disk.
///
/// Returns None if the context file doesn't exist or can't be parsed.
pub async fn load_timeout_context(
    workspace: &Path,
    bead_id: &BeadId,
) -> Option<TimeoutDecompositionContext> {
    let path = context_path(workspace, bead_id);
    let raw = tokio::fs::read(&path).await.ok()?;
    serde_json::from_slice(&raw).ok()
}

/// Remove timeout context from disk (cleanup after successful mitosis).
pub async fn clear_timeout_context(workspace: &Path, bead_id: &BeadId) {
    let path = context_path(workspace, bead_id);
    let _ = tokio::fs::remove_file(&path).await;
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mitosis::timeout_eligibility::TimeoutEligibility;
    use crate::types::{Bead, BeadId, BeadStatus};
    use chrono::Utc;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn test_bead() -> Bead {
        Bead {
            id: BeadId::from("bf-test"),
            title: "Test bead".to_string(),
            body: Some("Test body that is reasonably long to test truncation but not extremely long so it fits".to_string()),
            priority: 1,
            status: BeadStatus::InProgress,
            assignee: Some("worker-01".to_string()),
            labels: vec!["timeout".to_string()],
            workspace: PathBuf::from("/tmp/test"),
            dependencies: vec![],
            dependents: vec![],
            comments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn timeout_context_new_eligible() {
        let eligibility = TimeoutEligibility::Eligible {
            reason: "agent wall-clock timeout with substantial work".to_string(),
        };

        let context = TimeoutContext::new(eligibility, 3600);

        assert!(context.eligibility.is_eligible);
        assert!(context.eligibility.reason.contains("substantial work"));
        assert_eq!(context.duration_secs, 3600);
    }

    #[test]
    fn timeout_context_new_not_eligible() {
        let eligibility = TimeoutEligibility::NotEligible {
            reason: "insufficient elapsed time".to_string(),
        };

        let context = TimeoutContext::new(eligibility, 300);

        assert!(!context.eligibility.is_eligible);
        assert!(context.eligibility.reason.contains("insufficient"));
        assert_eq!(context.duration_secs, 300);
    }

    #[test]
    fn extract_gate_name_from_reason_handler() {
        let reason = "handler timeout on gate 'cargo-test' exceeded budget";
        let gate = extract_gate_name_from_reason(reason);
        assert_eq!(gate, Some("cargo-test".to_string()));
    }

    #[test]
    fn extract_gate_name_from_reason_no_gate() {
        let reason = "agent wall-clock timeout with substantial work";
        let gate = extract_gate_name_from_reason(reason);
        assert_eq!(gate, None);
    }

    #[tokio::test]
    async fn write_and_load_timeout_context() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();

        let bead = test_bead();
        let eligibility = TimeoutEligibility::Eligible {
            reason: "agent wall-clock timeout".to_string(),
        };

        let context = capture_timeout_context(&bead, workspace, eligibility, 3600)
            .await
            .unwrap()
            .expect("context capture failed");

        write_timeout_context(workspace, &bead.id, &context)
            .await
            .expect("write failed");

        let loaded = load_timeout_context(workspace, &bead.id)
            .await
            .expect("load failed");

        assert_eq!(loaded.bead_def.bead_id, bead.id);
        assert_eq!(loaded.bead_def.title, bead.title);
        assert!(loaded.qualifies_for_mitosis);
    }

    #[tokio::test]
    async fn load_missing_context_returns_none() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();
        let bead_id: BeadId = "bf-missing".into();

        let loaded = load_timeout_context(workspace, &bead_id).await;
        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn clear_removes_context_file() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();

        let bead = test_bead();
        let eligibility = TimeoutEligibility::Eligible {
            reason: "test".to_string(),
        };

        let context = capture_timeout_context(&bead, workspace, eligibility, 100)
            .await
            .unwrap()
            .expect("capture failed");

        write_timeout_context(workspace, &bead.id, &context)
            .await
            .expect("write failed");

        clear_timeout_context(workspace, &bead.id).await;

        let loaded = load_timeout_context(workspace, &bead.id).await;
        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn git_dirty_paths_filters_untracked() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();
        let git_dir = workspace.join(".git");

        // Initialize git repo with proper isolation
        tokio::process::Command::new("git")
            .args(["init"])
            .current_dir(workspace)
            .env("GIT_DIR", &git_dir)
            .env("GIT_WORK_TREE", workspace)
            .kill_on_drop(true)
            .output()
            .await
            .expect("git init failed");

        // Create a file and stage it (worktree clean)
        let file_path = workspace.join("clean.txt");
        tokio::fs::write(&file_path, "content")
            .await
            .expect("write failed");

        tokio::process::Command::new("git")
            .args(["add", "clean.txt"])
            .current_dir(workspace)
            .env("GIT_DIR", &git_dir)
            .env("GIT_WORK_TREE", workspace)
            .kill_on_drop(true)
            .output()
            .await
            .expect("git add failed");

        // Commit it. A staged-but-uncommitted add is still a reported change,
        // so the "worktree clean" premise only holds once it is committed.
        tokio::process::Command::new("git")
            .args([
                "-c",
                "user.email=github@jedarden.com",
                "-c",
                "user.name=jedarden",
                "commit",
                "-m",
                "seed",
            ])
            .current_dir(workspace)
            .env("GIT_DIR", &git_dir)
            .env("GIT_WORK_TREE", workspace)
            .kill_on_drop(true)
            .output()
            .await
            .expect("git commit failed");

        // Create a file and don't stage it (untracked)
        let untracked_path = workspace.join("untracked.txt");
        tokio::fs::write(&untracked_path, "untracked")
            .await
            .expect("write failed");

        let dirty_paths = git_dirty_paths(workspace).await;

        // Untracked files should not appear in dirty paths
        assert!(!dirty_paths.contains("untracked.txt"));
        assert!(dirty_paths.is_empty());
    }

    #[tokio::test]
    async fn git_dirty_paths_captures_modified() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();
        let git_dir = workspace.join(".git");

        // Initialize git repo with proper isolation
        tokio::process::Command::new("git")
            .args(["init"])
            .current_dir(workspace)
            .env("GIT_DIR", &git_dir)
            .env("GIT_WORK_TREE", workspace)
            .kill_on_drop(true)
            .output()
            .await
            .expect("git init failed");

        // Create, commit a file
        let file_path = workspace.join("test.txt");
        tokio::fs::write(&file_path, "original")
            .await
            .expect("write failed");

        tokio::process::Command::new("git")
            .args(["add", "test.txt"])
            .current_dir(workspace)
            .env("GIT_DIR", &git_dir)
            .env("GIT_WORK_TREE", workspace)
            .kill_on_drop(true)
            .output()
            .await
            .expect("git add failed");

        tokio::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(workspace)
            .env("GIT_DIR", &git_dir)
            .env("GIT_WORK_TREE", workspace)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .kill_on_drop(true)
            .output()
            .await
            .expect("git commit failed");

        // Modify the file (dirty)
        tokio::fs::write(&file_path, "modified")
            .await
            .expect("write failed");

        let dirty_paths = git_dirty_paths(workspace).await;

        // Modified file should appear in dirty paths
        assert!(dirty_paths.contains("test.txt"));
    }
}
