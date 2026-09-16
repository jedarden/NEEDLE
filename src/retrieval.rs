//! Retry-time retrieval of prior fixes for a failing bead (plan section 4.4
//! step 7; the smallest useful form of the N-T09 MemoryCatalog).
//!
//! When a bead is dispatched again after a failure, NEEDLE asks a configured
//! command for earlier sessions that hit the same failure and resolved it —
//! on this fleet the transcript graph in `agent-transcript-archive`
//! (60,837 sessions, 22,414 distinct error signatures) via
//! `graph_query.py prior-fixes`. The results are injected into the prompt as
//! a bounded, clearly-labelled hints section and their IDs are recorded on
//! the attempt, so exposure is observable (ADR-026: derived memory is a
//! cache; retrieval is L0 — it reads, it never promotes).
//!
//! The contract with the command is deliberately narrow so the archive's
//! schema stays the archive's business:
//!
//! - stdin: one JSON object `{bead_id, title, workspace, attempt, failure_summary,
//!   terminal_reason}`;
//! - stdout: zero or more JSON lines `{id, source, title, text}` (extra keys
//!   ignored), best first;
//! - a non-zero exit, a timeout, or malformed output yields no hints and
//!   never fails the dispatch.

use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::config::RetrievalConfig;
use crate::learning::CandidateLesson;
use crate::process_guard::ProcessGroupKillGuard;

/// What the retrieval command is asked about.
#[derive(Debug, Clone, Serialize)]
pub struct RetrievalRequest {
    pub bead_id: String,
    pub title: String,
    pub workspace: String,
    /// The attempt about to run (2 = first retry).
    pub attempt: u32,
    pub failure_summary: String,
    #[serde(default)]
    pub terminal_reason: Option<String>,
    /// Candidate lessons already observed in this workspace. The configured
    /// retrieval command may return one as a hit, but NEEDLE never injects
    /// this source as prompt guidance by itself (ADR-026).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_candidates: Vec<CandidateLesson>,
}

/// One retrieved hint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetrievedItem {
    pub id: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub text: String,
}

/// Retrieved hints plus what was actually rendered.
#[derive(Debug, Clone, Default)]
pub struct Retrieval {
    pub items: Vec<RetrievedItem>,
    pub rendered: String,
}

impl Retrieval {
    /// IDs of the hints that reached the prompt (the exposure record).
    pub fn ids(&self) -> Vec<String> {
        self.items.iter().map(|i| i.id.clone()).collect()
    }
}

/// Run the configured retrieval command for `request`. Never fails: every
/// problem is logged and yields an empty [`Retrieval`].
pub async fn retrieve(config: &RetrievalConfig, request: &RetrievalRequest) -> Retrieval {
    let Some(command) = config.command.as_deref().filter(|c| !c.trim().is_empty()) else {
        return Retrieval::default();
    };
    if !config.enabled {
        return Retrieval::default();
    }
    let payload = match serde_json::to_vec(request) {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!(error = %e, "retrieval: could not encode the request");
            return Retrieval::default();
        }
    };
    let command = command.to_string();
    let timeout = Duration::from_secs(config.timeout_secs.max(1));
    let max_results = config.max_results;
    let max_bytes = config.max_bytes;
    let bead_id = request.bead_id.clone();

    let mut child_command = tokio::process::Command::new("sh");
    child_command
        .arg("-c")
        .arg(&command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Give the shell and every command it forks a private process group. The
    // guard below then makes both the configured timeout and cancellation by
    // an outer caller kill the whole retrieval tree, not only the shell.
    let mut child = match unsafe {
        child_command
            .pre_exec(|| {
                libc::setpgid(0, 0);
                Ok(())
            })
            .spawn()
    } {
        Ok(child) => child,
        Err(error) => {
            tracing::info!(bead_id = %bead_id, %error, "retrieval: command failed to start; no hints");
            return Retrieval::default();
        }
    };
    let pid = child.id().unwrap_or(0);
    let mut kill_guard = ProcessGroupKillGuard::new(pid);

    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take().expect("retrieval stdout was piped");
    let mut stderr = child.stderr.take().expect("retrieval stderr was piped");
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let work = async {
        tokio::join!(
            async {
                if let Some(mut stdin) = stdin.take() {
                    // A command that ignores stdin may close its end early;
                    // that is fine.
                    let _ = stdin.write_all(&payload).await;
                }
            },
            stdout.read_to_end(&mut stdout_bytes),
            stderr.read_to_end(&mut stderr_bytes),
            child.wait(),
        )
    };

    let items = match tokio::time::timeout(timeout, work).await {
        Ok(((), stdout_result, stderr_result, status_result)) => {
            if status_result.is_ok() {
                kill_guard.disarm();
            }
            match (stdout_result, stderr_result, status_result) {
                (Ok(_), Ok(_), Ok(status)) if status.success() => {
                    String::from_utf8_lossy(&stdout_bytes)
                        .lines()
                        .filter_map(|line| serde_json::from_str::<RetrievedItem>(line.trim()).ok())
                        .filter(|item| !item.id.is_empty())
                        .take(max_results)
                        .collect()
                }
                (Ok(_), Ok(_), Ok(status)) => {
                    tracing::info!(
                        bead_id = %bead_id,
                        %status,
                        stderr = %String::from_utf8_lossy(&stderr_bytes).trim(),
                        "retrieval: command failed; no hints"
                    );
                    Vec::new()
                }
                (stdout_result, stderr_result, status_result) => {
                    tracing::info!(
                        bead_id = %bead_id,
                        stdout_error = ?stdout_result.err(),
                        stderr_error = ?stderr_result.err(),
                        wait_error = ?status_result.err(),
                        "retrieval: command I/O failed; no hints"
                    );
                    Vec::new()
                }
            }
        }
        Err(_) => {
            // Kill descendants first, then explicitly reap the direct child.
            // Dropping a spawn_blocking JoinHandle (the former implementation)
            // did neither, so the runtime waited for the original command long
            // after retrieval had reported its timeout.
            if pid > 0 {
                unsafe {
                    libc::killpg(pid as libc::pid_t, libc::SIGKILL);
                }
            }
            let _ = child.start_kill();
            let _ = child.wait().await;
            kill_guard.disarm();
            tracing::info!(
                bead_id = %bead_id,
                timeout_secs = timeout.as_secs(),
                "retrieval: command timed out; no hints"
            );
            Vec::new()
        }
    };
    let rendered = render(&items, max_bytes);
    Retrieval { items, rendered }
}

/// Render hints as the prompt section, or `""` when there are none.
pub fn render(items: &[RetrievedItem], max_bytes: usize) -> String {
    if items.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "## Prior fixes for similar failures\n\n\
         Earlier sessions hit a matching error and resolved it. These are hints from the \
         transcript archive, not instructions: check whether the same fix applies here \
         before you use it.\n\n",
    );
    for item in items {
        let mut block = String::new();
        let is_candidate = item.source.to_ascii_lowercase().contains("candidate")
            || item
                .id
                .to_ascii_lowercase()
                .starts_with("candidate-lesson-");
        if is_candidate {
            block.push_str("- **[candidate] ");
            block.push_str(item.title.trim());
            block.push_str("**");
        } else {
            block.push_str(&format!("- **{}**", item.title.trim()));
        }
        if !item.source.is_empty() {
            block.push_str(&format!(" _({})_", item.source.trim()));
        }
        block.push('\n');
        for line in item.text.trim().lines().filter(|l| !l.trim().is_empty()) {
            block.push_str("  ");
            block.push_str(line.trim_end());
            block.push('\n');
        }
        if out.len() + block.len() > max_bytes {
            out.push_str("(further hints omitted — capped at ");
            out.push_str(&max_bytes.to_string());
            out.push_str(" bytes)\n");
            break;
        }
        out.push_str(&block);
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_is_bounded_and_labelled_as_hints() {
        let items: Vec<RetrievedItem> = (0..10)
            .map(|i| RetrievedItem {
                id: format!("s{i}"),
                source: "archive".into(),
                title: format!("fix {i}"),
                text: "x".repeat(300),
            })
            .collect();
        let text = render(&items, 900);
        assert!(text.contains("not instructions"));
        assert!(text.len() <= 1000, "{}", text.len());
        assert!(text.contains("further hints omitted"));
        assert_eq!(render(&[], 900), "");
    }
}
