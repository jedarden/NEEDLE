//! Fallback verification: the built-in gate a gate-less workspace runs.
//!
//! Part 2 of the needle-66b015d6 split. A workspace that declares neither
//! `gates:` nor `verification:` used to be waved through on the agent's exit
//! code. It is now judged by [`crate::validation::fallback::select_verifier`]
//! — the workspace's own files pick the verifier:
//!
//! - `scripts/definition-of-done.sh`, or a language marker, names a command:
//!   it runs in the clean extraction ([`crate::dispatch::extract_clean_workspace`],
//!   the needle-736cc4e0 extraction) of the workspace's committed state, so
//!   the verdict is about what shipped, not about whatever else is in flight
//!   in the shared checkout. The timeout and the stderr cap are the host's
//!   `validation.outcome_timeout_seconds` / `validation.stderr_cap_bytes` —
//!   the same ceiling every command gate answers to.
//! - Nothing detected: the only check that costs nothing and lies about
//!   nothing is that the extraction would carry the whole dispatch — i.e.
//!   `git status --porcelain` in the workspace is empty. (The extraction
//!   itself is built from committed state and has no `.git`, so the porcelain
//!   check runs where the dirt can actually be: in the workspace the
//!   extraction is cut from. Empty means the committed state *is* the whole
//!   work; anything uncommitted would silently vanish from the extraction.)
//!   Clean passes with the `no verifier for this workspace` WARN, counted by
//!   `gate.no_verifier` in the outcome handler; dirty fails the dispatch.
//!
//! Verdicts (see [`FallbackVerdict`]): a command that exits non-zero and a
//! dirty tree are verdicts — the dispatch is released with the
//! `verification-failed` label through the ordinary failed-verification path.
//! An extraction that cannot be built or a command that cannot spawn is not a
//! verdict: it surfaces as [`GateResult::ExecutionError`], which routes to
//! the GateError path (release, failure-count untouched, needle-4aaa010c) —
//! the same ADR-023 split the close-evidence re-run uses.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::process_runner::{ProcessRequest, ProcessRunner};
use crate::types::Bead;
use crate::validation::{GateReport, GateResult};

/// Prefixes of dirty paths that do not count against a clean tree.
///
/// `.beads/` churns as a side effect of every bead operation in the very
/// dispatch being judged (checkpoint writes, heartbeats) and is committed by
/// the fleet's own checkpoint protocol, not by the agent; the predispatch
/// snapshot filter excludes exactly these and so does the shipped-work gate.
const TREE_NOISE_PREFIXES: &[&str] = &[".beads/", ".needle-predispatch-sha"];

/// Worker segment for extraction directory names.
///
/// The outcome handler has no worker identity — that lives one layer up — so
/// the fallback gate's extractions name themselves by what ran them instead.
/// The bead id and timestamp keep them unique; the name is diagnostic only.
const EXTRACTION_WORKER_ID: &str = "outcome-fallback";

/// The gate name for the clean-tree check in reports and telemetry.
pub(crate) const CLEAN_TREE_GATE_NAME: &str = "fallback_clean_tree";

/// Outcome of judging a gate-less workspace's dispatch.
#[derive(Debug)]
pub(crate) enum FallbackVerdict {
    /// A selected verifier ran green in the clean extraction.
    Pass(GateReport),
    /// No verifier for this workspace and the tree the extraction is cut
    /// from is clean (or there is no git tree to judge at all). The caller
    /// emits the `no verifier for this workspace` WARN and counts it.
    NoVerifierPass,
    /// A verdict against the dispatch: a selected verifier failed, or the
    /// tree is dirty. Released with `verification-failed`.
    Fail(GateReport),
    /// The check itself could not run (extraction failed, spawn failed,
    /// timed out) — release without a failure-count penalty.
    ExecutionError(GateReport),
}

/// The executor side of the fallback gate.
///
/// Production runs the selected verifier with the captured-process runner in
/// a fresh extraction of the bead workspace's committed state. Tests inject
/// a fake runner and a pre-made directory so no real child and no git
/// checkout is needed.
#[derive(Clone)]
pub(crate) struct FallbackVerificationRuntime {
    runner: Arc<dyn ProcessRunner>,
    /// Test seam: run the verifier here instead of extracting committed
    /// state. `None` in production.
    extraction_dir: Option<PathBuf>,
}

impl FallbackVerificationRuntime {
    pub(crate) fn production() -> Self {
        Self {
            runner: Arc::new(crate::process_runner::TokioProcessRunner),
            extraction_dir: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_tests(
        runner: Arc<dyn ProcessRunner>,
        extraction_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            runner,
            extraction_dir,
        }
    }

    /// Judge the dispatch of a workspace that declares no gates.
    ///
    /// `timeout` and `stderr_cap_bytes` are the host's standard gate values
    /// (`validation.outcome_timeout_seconds` / `validation.stderr_cap_bytes`),
    /// passed in so this runtime stays config-free.
    pub(crate) async fn verify(
        &self,
        bead: &Bead,
        timeout: Duration,
        stderr_cap_bytes: usize,
    ) -> FallbackVerdict {
        let verifier = crate::validation::fallback::select_verifier(&bead.workspace);
        let Some(command) = verifier.command() else {
            return self.verify_clean_tree(bead).await;
        };
        let gate_name = gate_name(&verifier);

        let (extraction_dir, created_here) = match &self.extraction_dir {
            Some(dir) => (dir.clone(), false),
            None => {
                match crate::dispatch::extract_clean_workspace(
                    &bead.workspace,
                    crate::dispatch::ExtractionConfig::new(
                        EXTRACTION_WORKER_ID.to_string(),
                        bead.id.to_string(),
                    ),
                )
                .await
                {
                    Ok(result) => (result.extraction_path, true),
                    Err(e) => {
                        return FallbackVerdict::ExecutionError(execution_error_report(
                            &gate_name,
                            "git archive HEAD",
                            format!("failed to extract committed state: {e:#}"),
                        ));
                    }
                }
            }
        };

        let environment = crate::validation::GateEnvironment::capture();
        let command_environment = environment.for_command(command);
        let mut request = ProcessRequest::new("sh")
            .args(["-c", command])
            .current_dir(&extraction_dir);
        for (key, value) in command_environment.env_pairs() {
            request = request.env(key, value);
        }
        request = request
            .env("NEEDLE_BEAD_ID", bead.id.to_string())
            .env("NEEDLE_WORKSPACE", extraction_dir.display().to_string());

        match self.runner.output(request, timeout).await {
            Ok(output) if output.success => {
                // Only an extraction this runtime created itself is its own
                // to clean up; a test-seam directory belongs to whoever
                // passed it in.
                if created_here {
                    let _ = crate::dispatch::cleanup_extraction(&extraction_dir).await;
                }
                FallbackVerdict::Pass(pass_report(&gate_name))
            }
            Ok(output) => {
                // A failed gate leaves the extraction in place for diagnosis,
                // with the path in the reason — the same contract the command
                // gates and the dispatch extraction itself document.
                let stderr = String::from_utf8_lossy(&output.stderr);
                let reason = format!(
                    "fallback verifier '{}' exited {:?} in {}; stderr (capped at \
                     {stderr_cap_bytes} bytes):\n{}\n(extraction preserved at {})",
                    command,
                    output.exit_code,
                    bead.workspace.display(),
                    cap_bytes(&stderr, stderr_cap_bytes),
                    extraction_dir.display(),
                );
                tracing::warn!(
                    bead_id = %bead.id,
                    gate = %gate_name,
                    command = %command,
                    exit_code = ?output.exit_code,
                    "fallback gate failed — releasing bead"
                );
                FallbackVerdict::Fail(GateReport::single_failure(&gate_name, reason))
            }
            Err(e) => FallbackVerdict::ExecutionError(execution_error_report(
                &gate_name,
                command,
                format!("{e:#}"),
            )),
        }
    }

    /// The no-verifier branch: the extraction must not silently drop work.
    ///
    /// `git status --porcelain` in the bead's workspace — not in the
    /// extraction, which is a `.git`-less archive of committed state and
    /// could not answer. Empty means the committed state the extraction
    /// carries is the entire dispatch.
    async fn verify_clean_tree(&self, bead: &Bead) -> FallbackVerdict {
        let output = tokio::process::Command::new("git")
            .args(["status", "--porcelain", "--untracked-files=all"])
            .current_dir(&bead.workspace)
            .kill_on_drop(true)
            .output()
            .await;

        let stdout = match output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).into_owned()
            }
            Ok(output) => {
                tracing::debug!(
                    bead_id = %bead.id,
                    workspace = %bead.workspace.display(),
                    stderr = %String::from_utf8_lossy(&output.stderr),
                    "no verifier for this workspace and git could not judge its tree — \
                     passing on the agent's exit code alone"
                );
                return FallbackVerdict::NoVerifierPass;
            }
            Err(error) => {
                tracing::debug!(
                    bead_id = %bead.id,
                    workspace = %bead.workspace.display(),
                    error = %error,
                    "no verifier for this workspace and git could not be spawned to judge \
                     its tree — passing on the agent's exit code alone"
                );
                return FallbackVerdict::NoVerifierPass;
            }
        };

        let dirty: Vec<String> = stdout
            .lines()
            .filter_map(|line| line.get(3..))
            .map(|path| path.rsplit(" -> ").next().unwrap_or(path).trim())
            .filter(|path| !path.is_empty())
            .filter(|path| {
                !TREE_NOISE_PREFIXES
                    .iter()
                    .any(|prefix| path.starts_with(prefix))
            })
            .map(str::to_string)
            .collect();

        if dirty.is_empty() {
            FallbackVerdict::NoVerifierPass
        } else {
            let reason = format!(
                "no verifier for this workspace, so the dispatch is judged on its \
                 committed state alone — but these paths carry uncommitted changes \
                 that the clean extraction would not contain:\n{}",
                dirty.join("\n")
            );
            tracing::warn!(
                bead_id = %bead.id,
                workspace = %bead.workspace.display(),
                dirty_paths = ?dirty,
                "fallback clean-tree check failed — uncommitted changes would be \
                 absent from the extraction"
            );
            FallbackVerdict::Fail(GateReport::single_failure(CLEAN_TREE_GATE_NAME, reason))
        }
    }
}

/// The gate name a verifier's result is reported under.
fn gate_name(verifier: &crate::validation::fallback::Verifier) -> String {
    use crate::validation::fallback::Verifier;
    match verifier {
        Verifier::DefinitionOfDone => "fallback_definition_of_done".to_string(),
        Verifier::Marker(marker) => format!("fallback_{}", marker.language),
        Verifier::NoVerifier => CLEAN_TREE_GATE_NAME.to_string(),
    }
}

fn pass_report(gate_name: &str) -> GateReport {
    GateReport::new(HashMap::from([(gate_name.to_string(), GateResult::Pass)]))
}

fn execution_error_report(gate_name: &str, command: &str, reason: String) -> GateReport {
    GateReport::new(HashMap::from([(
        gate_name.to_string(),
        GateResult::ExecutionError {
            command: command.to_string(),
            reason,
        },
    )]))
}

/// Cap `text` at `max` bytes without splitting a UTF-8 character.
fn cap_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… [truncated at {max} bytes]", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_runner::{FakeProcessRunner, ProcessOutput};

    fn test_bead_in(workspace: &std::path::Path) -> Bead {
        Bead {
            id: crate::types::BeadId::from("needle-test"),
            title: "Test bead".to_string(),
            body: None,
            priority: 1,
            status: crate::types::BeadStatus::InProgress,
            assignee: None,
            labels: vec![],
            workspace: workspace.to_path_buf(),
            dependencies: vec![],
            dependents: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    const TIMEOUT: Duration = Duration::from_secs(30);

    /// Commit `files` in a fresh git repo so the extraction has a HEAD to
    /// archive. Returns the workspace directory.
    fn git_workspace(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, contents) in files {
            let path = dir.path().join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, contents).unwrap();
        }
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["-c", "user.email=needle@example.test"])
                .args(["-c", "user.name=needle-test"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .output()
                .unwrap()
        };
        let init = git(&["init", "-q"]);
        assert!(
            init.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&init.stderr)
        );
        let add = git(&["add", "-A"]);
        assert!(
            add.status.success(),
            "git add failed: {}",
            String::from_utf8_lossy(&add.stderr)
        );
        let commit = git(&["commit", "-q", "-m", "fixture"]);
        assert!(
            commit.status.success(),
            "git commit failed: {}",
            String::from_utf8_lossy(&commit.stderr)
        );
        dir
    }

    // ── a selected verifier runs in the clean extraction ──

    #[tokio::test]
    async fn failing_verifier_names_command_exit_and_capped_stderr() {
        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_output(ProcessOutput {
            success: false,
            exit_code: Some(3),
            stdout: b"running\n".to_vec(),
            stderr: b"definition of done failed\n".to_vec(),
        });
        let extraction = tempfile::tempdir().unwrap();
        let runtime = FallbackVerificationRuntime::for_tests(
            runner.clone(),
            Some(extraction.path().to_path_buf()),
        );
        let workspace = git_workspace(&[("Cargo.toml", "[package]\nname = \"x\"\n")]);

        match runtime
            .verify(&test_bead_in(workspace.path()), TIMEOUT, 4096)
            .await
        {
            FallbackVerdict::Fail(report) => {
                assert!(!report.passed());
                let (name, result) = report.results.iter().next().unwrap();
                assert_eq!(name, "fallback_rust", "the marker names the gate");
                let GateResult::Fail(reason) = result else {
                    panic!("expected Fail, got {result:?}")
                };
                assert!(reason.contains("cargo build --all-targets && cargo test"));
                assert!(reason.contains("Some(3)"));
                assert!(reason.contains("definition of done failed"));
            }
            other => panic!("expected Fail, got {other:?}"),
        }

        let requests = runner.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].program(), std::path::Path::new("sh"));
        assert_eq!(
            requests[0].arguments(),
            ["-c", "cargo build --all-targets && cargo test"] as [&str; 2]
        );
        assert_eq!(requests[0].working_directory(), Some(extraction.path()));
    }

    #[tokio::test]
    async fn passing_verifier_reports_pass_and_carries_bead_identity() {
        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_output(ProcessOutput::success(b"ok\n".to_vec()));
        let extraction = tempfile::tempdir().unwrap();
        let runtime = FallbackVerificationRuntime::for_tests(
            runner.clone(),
            Some(extraction.path().to_path_buf()),
        );
        let workspace = git_workspace(&[("scripts/definition-of-done.sh", "#!/bin/sh\n")]);

        match runtime
            .verify(&test_bead_in(workspace.path()), TIMEOUT, 4096)
            .await
        {
            FallbackVerdict::Pass(report) => {
                assert!(report.passed());
                assert!(
                    report.results.contains_key("fallback_definition_of_done"),
                    "the pass names its gate: {:?}",
                    report.results.keys().collect::<Vec<_>>()
                );
            }
            other => panic!("expected Pass, got {other:?}"),
        }

        let requests = runner.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].arguments(),
            ["-c", "scripts/definition-of-done.sh"] as [&str; 2]
        );
        assert_eq!(requests[0].working_directory(), Some(extraction.path()));
    }

    #[tokio::test]
    async fn verifier_spawn_failure_is_an_execution_error_not_a_verdict() {
        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_error("spawn failed");
        let extraction = tempfile::tempdir().unwrap();
        let runtime =
            FallbackVerificationRuntime::for_tests(runner, Some(extraction.path().to_path_buf()));
        let workspace = git_workspace(&[("go.mod", "module example.com/x\n")]);

        match runtime
            .verify(&test_bead_in(workspace.path()), TIMEOUT, 4096)
            .await
        {
            FallbackVerdict::ExecutionError(report) => {
                let (name, result) = report.results.iter().next().unwrap();
                assert_eq!(name, "fallback_go");
                let GateResult::ExecutionError { command, .. } = result else {
                    panic!("expected ExecutionError, got {result:?}")
                };
                assert_eq!(
                    command,
                    "go build ./... && go vet ./... && go test -short ./..."
                );
            }
            other => panic!("expected ExecutionError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn verifier_timeout_is_an_execution_error_not_a_verdict() {
        // The runner surfaces a timeout as an Err (TokioProcessRunner words it
        // "timed out after Nms"); the verdict must stay ExecutionError so the
        // GateError route releases without a failure-count penalty.
        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_error("captured process at sh timed out after 30000ms");
        let extraction = tempfile::tempdir().unwrap();
        let runtime =
            FallbackVerificationRuntime::for_tests(runner, Some(extraction.path().to_path_buf()));
        let workspace = git_workspace(&[("pytest.ini", "[pytest]\n")]);

        match runtime
            .verify(&test_bead_in(workspace.path()), TIMEOUT, 4096)
            .await
        {
            FallbackVerdict::ExecutionError(report) => {
                let (name, result) = report.results.iter().next().unwrap();
                assert_eq!(name, "fallback_python");
                let GateResult::ExecutionError { reason, .. } = result else {
                    panic!("expected ExecutionError, got {result:?}")
                };
                assert!(reason.contains("timed out"), "reason: {reason}");
            }
            other => panic!("expected ExecutionError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stderr_is_capped_at_the_configured_budget() {
        let huge = "x".repeat(100_000);
        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_output(ProcessOutput {
            success: false,
            exit_code: Some(1),
            stdout: Vec::new(),
            stderr: huge.into_bytes(),
        });
        let extraction = tempfile::tempdir().unwrap();
        let runtime =
            FallbackVerificationRuntime::for_tests(runner, Some(extraction.path().to_path_buf()));
        let workspace = git_workspace(&[("Cargo.toml", "[package]\n")]);

        match runtime
            .verify(&test_bead_in(workspace.path()), TIMEOUT, 512)
            .await
        {
            FallbackVerdict::Fail(report) => {
                let reason = report
                    .results
                    .values()
                    .next()
                    .and_then(|r| r.failure_reason())
                    .unwrap_or_default();
                assert!(
                    reason.len() < 1024,
                    "reason must carry the capped stderr, got {} bytes",
                    reason.len()
                );
                assert!(reason.contains("[truncated at 512 bytes]"));
            }
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn extraction_failure_is_an_execution_error_not_a_verdict() {
        // Production runtime, and a workspace whose committed state cannot be
        // archived: the marker is there but git is not — nothing to extract.
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("Cargo.toml"), "[package]\n").unwrap();

        match FallbackVerificationRuntime::production()
            .verify(&test_bead_in(workspace.path()), TIMEOUT, 4096)
            .await
        {
            FallbackVerdict::ExecutionError(report) => {
                let (name, result) = report.results.iter().next().unwrap();
                assert_eq!(name, "fallback_rust");
                let GateResult::ExecutionError { command, reason } = result else {
                    panic!("expected ExecutionError, got {result:?}")
                };
                assert_eq!(command, "git archive HEAD");
                assert!(
                    reason.contains("failed to extract committed state"),
                    "reason: {reason}"
                );
            }
            other => panic!("expected ExecutionError, got {other:?}"),
        }
    }

    // ── the clean-tree check (no verifier selected) ──

    #[tokio::test]
    async fn clean_committed_tree_passes_without_a_verifier() {
        let workspace = git_workspace(&[("README.md", "docs only\n")]);
        match FallbackVerificationRuntime::production()
            .verify(&test_bead_in(workspace.path()), TIMEOUT, 4096)
            .await
        {
            FallbackVerdict::NoVerifierPass => {}
            other => panic!("expected NoVerifierPass, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn uncommitted_changes_fail_the_clean_tree_check() {
        let workspace = git_workspace(&[("README.md", "docs only\n")]);
        std::fs::write(workspace.path().join("scratch.md"), "never committed\n").unwrap();
        std::fs::write(workspace.path().join("README.md"), "edited, uncommitted\n").unwrap();

        match FallbackVerificationRuntime::production()
            .verify(&test_bead_in(workspace.path()), TIMEOUT, 4096)
            .await
        {
            FallbackVerdict::Fail(report) => {
                let reason = report
                    .results
                    .get(CLEAN_TREE_GATE_NAME)
                    .and_then(|r| r.failure_reason())
                    .unwrap_or_default();
                assert!(
                    reason.contains("scratch.md") && reason.contains("README.md"),
                    "the reason must name the dirty paths: {reason}"
                );
            }
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bead_bookkeeping_noise_does_not_fail_the_clean_tree_check() {
        let workspace = git_workspace(&[("README.md", "docs only\n")]);
        // A dispatch's own bead operations dirty `.beads/` as a side effect;
        // the checkpoint protocol commits that separately.
        std::fs::create_dir_all(workspace.path().join(".beads")).unwrap();
        std::fs::write(workspace.path().join(".beads/events.jsonl"), "{}\n").unwrap();

        match FallbackVerificationRuntime::production()
            .verify(&test_bead_in(workspace.path()), TIMEOUT, 4096)
            .await
        {
            FallbackVerdict::NoVerifierPass => {}
            other => panic!("expected NoVerifierPass, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_workspace_without_git_passes_the_clean_tree_check() {
        // No git, no committed state, no extraction to speak of — nothing the
        // check could judge, so the dispatch passes and is counted instead of
        // failing every dispatch on a degenerate workspace forever.
        let workspace = tempfile::tempdir().unwrap();
        match FallbackVerificationRuntime::production()
            .verify(&test_bead_in(workspace.path()), TIMEOUT, 4096)
            .await
        {
            FallbackVerdict::NoVerifierPass => {}
            other => panic!("expected NoVerifierPass, got {other:?}"),
        }
    }

    // ── the byte cap helper ──

    #[test]
    fn cap_bytes_preserves_short_text_and_char_boundaries() {
        assert_eq!(cap_bytes("short", 100), "short");
        let multi_byte = "héllo".repeat(100);
        let capped = cap_bytes(&multi_byte, 7);
        assert!(capped.contains("[truncated at 7 bytes]"));
        // The prefix before the marker must be valid UTF-8 (no panic above
        // proves the boundary walk worked).
        let prefix = capped.split('\n').next().unwrap();
        assert!(prefix.chars().count() <= 7);
    }
}
