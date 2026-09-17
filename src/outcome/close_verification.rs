//! Close-evidence verification: re-run what the close reason claims.
//!
//! A close reason must carry evidence, and NEEDLE re-runs it. The pluck
//! prompt requires every close reason to end with a fenced `verified:` block
//! listing the exact commands the agent ran and their exit codes; this module
//! parses that block, filters it through a fixed allow-list of verifiable
//! command prefixes, adds any `go test`/`cargo test` acceptance commands
//! quoted in the bead's own description, and re-runs the result in a clean
//! extraction of the workspace's committed state — so an agent can neither
//! close without evidence nor quietly drop the test its bead names.
//!
//! Contract (see `docs/close-verification.md`):
//!
//! - No `verified:` block in the close reason → the close is rejected
//!   (reopen + release, reason "close reason carries no verification
//!   evidence").
//! - Commands outside the allow-list are ignored with a WARN and never
//!   executed; the block still counts as evidence.
//! - Any re-run command that exits non-zero fails the close: the failure
//!   reason names the command and its first 20 lines of output.
//! - Acceptance commands mined from the bead's description re-run only where
//!   they can apply: `go test` needs a `go.mod`/`go.work` in the workspace
//!   and `cargo test` a `Cargo.toml` — the same build-marker logic the
//!   default gates use to pick a language — and a span containing an
//!   ellipsis is a template, not a command, and is never re-run. Commands
//!   claimed in the `verified:` block are not scoped this way: an agent
//!   claiming `go test` ran in a Rust repo is claiming something false, and
//!   the re-run exists to catch that.
//! - A clean extraction that cannot be built is an execution error, not a
//!   verdict: the bead is released without a failure-count penalty (ADR-023).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::process_runner::{ProcessRequest, ProcessRunner};
use crate::types::Bead;
use crate::validation::GateReport;

/// Gate name used in telemetry and failure reports for this check.
pub(crate) const GATE_NAME: &str = "close_verification";

/// Rejection reason for a close reason without a `verified:` block.
pub(crate) const MISSING_EVIDENCE_REASON: &str = "close reason carries no verification evidence";

/// Per-command ceiling for a re-run. The whole handling pass is additionally
/// bounded by `validation.outcome_timeout_seconds`, the same way command gates
/// are.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How many lines of a failed command's output the failure reason carries.
const FAILURE_OUTPUT_LINES: usize = 20;

/// Command prefixes a close reason may claim and NEEDLE will re-run.
///
/// Everything else is ignored with a WARN and never spawned: a close reason
/// is not a shell prompt, and `needle` must not become an execution oracle
/// for whatever text an agent (or a bead author) pastes into a reason.
const ALLOWED_PREFIXES: &[&[&str]] = &[
    &["go", "test"],
    &["go", "vet"],
    &["go", "build"],
    &["cargo", "test"],
    &["cargo", "build"],
    &["npm", "test"],
    &["pytest"],
    &["make", "test"],
    &["scripts/definition-of-done.sh"],
];

/// Outcome of judging one closed bead's close-reason evidence.
#[derive(Debug)]
pub(crate) enum CloseEvidenceVerdict {
    /// The backend cannot expose close reasons — the gate does not apply.
    Skipped,
    /// Evidence carried and every re-run passed (or nothing was re-runnable).
    Pass,
    /// Evidence missing or a re-run failed — reopen and release.
    Fail(GateReport),
    /// The check itself could not run — release without a penalty.
    ExecutionError { command: String, reason: String },
}

/// The executor side of the close-evidence gate.
///
/// Production re-runs claimed commands with the captured-process runner in a
/// fresh extraction of the bead workspace's committed HEAD. Tests inject a
/// fake runner and a pre-made directory so no real child and no git checkout
/// is needed.
#[derive(Clone)]
pub(crate) struct CloseVerificationRuntime {
    runner: Arc<dyn ProcessRunner>,
    /// Test seam: run re-runs here instead of extracting committed state.
    /// `None` in production.
    extraction_dir: Option<PathBuf>,
}

impl CloseVerificationRuntime {
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

    /// Re-run `commands` in the clean extraction, stopping at the first
    /// failure. An empty command list passes without building an extraction.
    pub(crate) async fn verify(&self, bead: &Bead, commands: &[String]) -> CloseEvidenceVerdict {
        if commands.is_empty() {
            return CloseEvidenceVerdict::Pass;
        }

        let (extraction_dir, created_here) = match &self.extraction_dir {
            Some(dir) => (dir.clone(), false),
            None => {
                match crate::validation::extract_head_for_verification(&bead.workspace, &bead.id)
                    .await
                {
                    Ok(dir) => (dir, true),
                    Err(e) => {
                        return CloseEvidenceVerdict::ExecutionError {
                            command: "git archive HEAD".to_string(),
                            reason: format!("failed to extract committed state: {e:#}"),
                        };
                    }
                }
            }
        };

        let environment = crate::validation::GateEnvironment::capture();
        for command in commands {
            let mut request = ProcessRequest::new("sh")
                .args(["-c", command.as_str()])
                .current_dir(&extraction_dir);
            for (key, value) in environment.env_pairs() {
                request = request.env(key, value);
            }
            request = request
                .env("NEEDLE_BEAD_ID", bead.id.to_string())
                .env("NEEDLE_WORKSPACE", extraction_dir.display().to_string());

            match self.runner.output(request, COMMAND_TIMEOUT).await {
                Ok(output) if output.success => continue,
                Ok(output) => {
                    let combined = format!(
                        "{}\n{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                    tracing::warn!(
                        bead_id = %bead.id,
                        command = %command,
                        exit_code = ?output.exit_code,
                        "close-evidence re-run failed — rejecting the close"
                    );
                    return CloseEvidenceVerdict::Fail(GateReport::single_failure(
                        GATE_NAME,
                        format!(
                            "verification command '{}' exited {:?}; output (first {} lines):\n{}",
                            command,
                            output.exit_code,
                            FAILURE_OUTPUT_LINES,
                            first_lines(&combined, FAILURE_OUTPUT_LINES),
                        ),
                    ));
                }
                Err(e) => {
                    return CloseEvidenceVerdict::ExecutionError {
                        command: command.clone(),
                        reason: format!("failed to run verification command: {e:#}"),
                    };
                }
            }
        }

        // Only an extraction this runtime created itself is its own to clean
        // up; a test-seam directory belongs to whoever passed it in.
        if created_here {
            let _ = tokio::fs::remove_dir_all(&extraction_dir).await;
        }
        CloseEvidenceVerdict::Pass
    }
}

/// First `max` lines of `output`, joined back with newlines.
fn first_lines(output: &str, max: usize) -> String {
    output.lines().take(max).collect::<Vec<_>>().join("\n")
}

/// Whether a fence marker line opens/ closes a fenced block.
fn fence_marker(trimmed: &str) -> Option<&'static str> {
    if trimmed.starts_with("```") {
        Some("```")
    } else if trimmed.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

/// Whether a fence opening line's info string names the verified block.
fn is_verified_fence(trimmed: &str) -> bool {
    let Some(marker) = fence_marker(trimmed) else {
        return false;
    };
    let info = trimmed[marker.len()..].trim();
    matches!(info, "verified" | "verified:")
}

/// Whether a line is a bare `verified:` label — evidence without a fence.
///
/// Agents reliably write the label bare: five consecutive evidence-free
/// rejections on 2026-09-17 (fingerprint `ea603be160bd`, workspace degraded)
/// were all fenced-form parses of reasons whose evidence was present but
/// unfenced. The fence stays the prompt's requested shape; this only stops
/// the parser from discarding evidence that is actually there.
fn is_bare_verified_label(trimmed: &str) -> bool {
    trimmed == "verified:"
}

/// Parse the `verified:` evidence out of a close reason.
///
/// Two shapes are accepted: the prompt's fenced block, and a bare
/// `verified:` line whose claim is the contiguous run of non-blank lines
/// after it (stopped by the first blank line, a fence marker, or the end of
/// the reason — prose in a later paragraph is never swallowed into the
/// claim). In both, the claim ends where the reason ends, so the bare form
/// can only recognise evidence the contract already places last.
///
/// Returns `None` when the reason carries no verified evidence, and
/// `Some(commands)` — possibly empty — when it does. The exit codes the
/// agent claims are stripped and deliberately not trusted: every command is
/// re-run, which is the entire point of the block.
pub(crate) fn parse_verified_block(reason: &str) -> Option<Vec<String>> {
    let mut lines = reason.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if is_bare_verified_label(trimmed) {
            let mut commands = Vec::new();
            for inner in lines.by_ref() {
                let inner_trimmed = inner.trim();
                if inner_trimmed.is_empty() || fence_marker(inner_trimmed).is_some() {
                    break;
                }
                if let Some(command) = normalize_claimed_command(inner_trimmed) {
                    commands.push(command);
                }
            }
            return Some(commands);
        }
        if !is_verified_fence(trimmed) {
            continue;
        }
        let marker = fence_marker(trimmed).unwrap_or("```");
        let mut commands = Vec::new();
        for inner in lines.by_ref() {
            let inner_trimmed = inner.trim();
            if inner_trimmed.starts_with(marker) {
                break;
            }
            if let Some(command) = normalize_claimed_command(inner_trimmed) {
                commands.push(command);
            }
        }
        return Some(commands);
    }
    None
}

/// Normalize one claimed line into a bare command, dropping the claimed exit
/// code and shell-prompt decoration. `None` for blank lines.
fn normalize_claimed_command(line: &str) -> Option<String> {
    let mut line = line.strip_prefix("$ ").map(str::trim).unwrap_or(line);
    // Trailing claimed exit code: `exit=0`, `exit 0` — informational only.
    // The separator must start its own token so a real flag like
    // `--exit=0` is never cut.
    for separator in ["exit=", "exit "] {
        if let Some(position) = line.rfind(separator) {
            let at_token_start = position == 0
                || line[..position]
                    .chars()
                    .last()
                    .is_some_and(char::is_whitespace);
            let tail = &line[position + separator.len()..];
            if at_token_start && tail.trim().parse::<i64>().is_ok() {
                line = line[..position].trim_end();
            }
        }
    }
    let command = line.trim();
    if command.is_empty() {
        None
    } else {
        Some(command.to_string())
    }
}

/// Whether a command's first tokens match one of the verifiable prefixes.
pub(crate) fn is_allow_listed(command: &str) -> bool {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    ALLOWED_PREFIXES
        .iter()
        .any(|prefix| tokens.starts_with(prefix))
}

/// Whether a command is one of the acceptance-command shapes scanned out of
/// bead descriptions (`go test …` / `cargo test …`).
fn is_acceptance_command(command: &str) -> bool {
    command_starts_with(command, &["go", "test"])
        || command_starts_with(command, &["cargo", "test"])
}

/// Scan a bead description for `go test`/`cargo test` acceptance commands.
///
/// Only explicitly quoted commands are taken — fenced code blocks and inline
/// code spans — never bare prose lines, so "cargo test passes (push to main
/// and wait for CI)" is not misread as the command
/// `cargo test passes (push to main and wait for CI)`. Ellipsis templates
/// (`go test …`) are dropped too: they name a family of commands, not one
/// runnable command, and re-running one verbatim would only manufacture a
/// failure.
pub(crate) fn extract_acceptance_commands(description: &str) -> Vec<String> {
    let mut commands = Vec::new();
    for candidate in quoted_candidates(description) {
        if is_command_template(&candidate) {
            continue;
        }
        let normalized = candidate.split_whitespace().collect::<Vec<_>>().join(" ");
        if is_acceptance_command(&normalized) && !commands.contains(&normalized) {
            commands.push(normalized);
        }
    }
    commands
}

/// Whether a quoted candidate is a placeholder for a family of commands
/// rather than one runnable command: a Unicode ellipsis anywhere, or a
/// standalone `...` token. Go's recursive package pattern `./...` is a real
/// argument, not a template, and must survive this filter.
fn is_command_template(candidate: &str) -> bool {
    candidate.contains('…') || candidate.split_whitespace().any(|token| token == "...")
}

/// Yield command-shaped candidates: every line inside a fenced code block,
/// plus every inline code span on every line.
fn quoted_candidates(text: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut in_fence: Option<&'static str> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        match in_fence {
            Some(marker) => {
                if trimmed.starts_with(marker) {
                    in_fence = None;
                } else {
                    candidates.push(trimmed.to_string());
                }
            }
            None => {
                if let Some(marker) = fence_marker(trimmed) {
                    in_fence = Some(marker);
                } else {
                    for span in trimmed.split('`').skip(1).step_by(2) {
                        let span = span.trim();
                        if !span.is_empty() {
                            candidates.push(span.to_string());
                        }
                    }
                }
            }
        }
    }
    candidates
}

/// Merge the claimed (allow-listed) commands with the bead description's
/// acceptance commands, dropping duplicates and WARN-ing on every claimed
/// command outside the allow-list.
///
/// Acceptance commands mined from the description are scoped to where they
/// can apply — `go test` needs a `go.mod`/`go.work` in the workspace,
/// `cargo test` a `Cargo.toml`, the same build-marker logic the default
/// gates use to pick a language — so a Rust workspace whose bead quotes
/// `go test ./...` in prose does not get that command re-run and failed,
/// exactly as its gates would not run a Go vet on it. Claimed commands are
/// deliberately not scoped: an agent claiming `go test` ran in a Rust repo
/// is claiming something false, and the re-run exists to catch that.
pub(crate) fn collect_rerun_commands(
    bead_id: &str,
    claimed: &[String],
    description: Option<&str>,
    workspace: &Path,
) -> Vec<String> {
    let mut commands = Vec::new();
    for claimed_command in claimed {
        let command = normalize_whitespace(claimed_command);
        if is_allow_listed(&command) {
            if !commands.contains(&command) {
                commands.push(command);
            }
        } else {
            tracing::warn!(
                bead_id = %bead_id,
                command = %command,
                "close-evidence command outside the allow-list — ignoring, never executed"
            );
        }
    }
    if let Some(description) = description {
        for command in extract_acceptance_commands(description) {
            if commands.contains(&command) {
                continue;
            }
            if !acceptance_command_applies(&command, workspace) {
                tracing::debug!(
                    bead_id = %bead_id,
                    command = %command,
                    "bead description names a command this workspace cannot run — not re-running it"
                );
                continue;
            }
            commands.push(command);
        }
    }
    commands
}

/// Collapse runs of whitespace so a claimed command and the same command
/// mined from the description dedupe against each other.
fn normalize_whitespace(command: &str) -> String {
    command.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether an acceptance command can run in `workspace` at all, by the same
/// build-marker logic the default gates use to pick a language.
fn acceptance_command_applies(command: &str, workspace: &Path) -> bool {
    if command_starts_with(command, &["go", "test"]) {
        workspace.join("go.mod").is_file() || workspace.join("go.work").is_file()
    } else if command_starts_with(command, &["cargo", "test"]) {
        workspace.join("Cargo.toml").is_file()
    } else {
        false
    }
}

/// Whether a command's first tokens are exactly `prefix`.
fn command_starts_with(command: &str, prefix: &[&str]) -> bool {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    tokens.starts_with(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_verified_block ──

    #[test]
    fn missing_block_parses_to_none() {
        assert_eq!(parse_verified_block("did the work, all green"), None);
        assert_eq!(parse_verified_block(""), None);
    }

    #[test]
    fn verified_block_yields_commands_with_claimed_exit_codes_stripped() {
        let reason = "Implemented the crypto helpers.\n\n```verified:\ngo test -short ./internal/crypto/ exit=0\ncargo test --lib exit=0\n```\n";
        assert_eq!(
            parse_verified_block(reason),
            Some(vec![
                "go test -short ./internal/crypto/".to_string(),
                "cargo test --lib".to_string(),
            ])
        );
    }

    #[test]
    fn verified_fence_without_colon_and_without_exit_codes_parses() {
        let reason = "intro\n```verified\npytest -k auth\nmake test\n```\ntrailer";
        assert_eq!(
            parse_verified_block(reason),
            Some(vec!["pytest -k auth".to_string(), "make test".to_string()])
        );
    }

    #[test]
    fn shell_prompt_prefix_and_exit_space_form_are_tolerated() {
        let reason = "```\nnot-verified: ignored marker\n```\n$ go vet ./... exit 1";
        assert_eq!(parse_verified_block(reason), None);

        let reason = "```verified:\n$ go vet ./... exit=1\n```";
        assert_eq!(
            parse_verified_block(reason),
            Some(vec!["go vet ./...".to_string()])
        );
    }

    #[test]
    fn plain_fence_is_not_evidence() {
        let reason = "```\ncargo test --lib\n```";
        assert_eq!(parse_verified_block(reason), None);
    }

    #[test]
    fn empty_verified_block_is_still_evidence() {
        assert_eq!(parse_verified_block("```verified:\n```"), Some(vec![]));
    }

    #[test]
    fn unterminated_verified_block_is_accepted_leniently() {
        assert_eq!(
            parse_verified_block("```verified:\ngo build ./...\n"),
            Some(vec!["go build ./...".to_string()])
        );
    }

    // ── bare (unfenced) `verified:` label — the shape agents actually write ──

    #[test]
    fn bare_verified_label_with_commands_is_evidence() {
        // Reduced from claudepr-a847d4de's rejected close (fingerprint
        // ea603be160bd): evidence present, label bare, block unfenced.
        let reason = "Added tests/stop_delayed_payload_e2e.rs (commit de77ab9, pushed).\n\
                      Mutation-verified: early read_fd drop fails both binary tests.\n\
                      \n\
                      verified:\n\
                      cargo test --test stop_delayed_payload_e2e exit=0\n\
                      cargo build exit=0";
        assert_eq!(
            parse_verified_block(reason),
            Some(vec![
                "cargo test --test stop_delayed_payload_e2e".to_string(),
                "cargo build".to_string(),
            ])
        );
    }

    #[test]
    fn bare_label_claim_stops_at_blank_line_so_later_prose_is_not_claimed() {
        let reason = "Shipped in 72f3249.\n\n\
                      verified:\n\
                      cargo build exit=0\n\
                      \n\
                      No new commit this attempt — docs-only work was already committed.\n\
                      cargo test of the prose paragraph below must never be re-run.";
        assert_eq!(
            parse_verified_block(reason),
            Some(vec!["cargo build".to_string()])
        );
    }

    #[test]
    fn bare_label_mid_sentence_is_not_evidence() {
        for reason in [
            "Everything verified: tests green, work pushed.",
            "Re-verification attempt: all green.",
            "The work was verified by CI.",
        ] {
            assert_eq!(
                parse_verified_block(reason),
                None,
                "got evidence from: {reason}"
            );
        }
    }

    #[test]
    fn bare_label_with_nothing_after_is_empty_evidence() {
        assert_eq!(parse_verified_block("all green\n\nverified:"), Some(vec![]));
    }

    #[test]
    fn bare_label_claim_stops_at_a_fence_marker() {
        let reason = "verified:\ncargo build exit=0\n```\ncargo test --lib\n```";
        assert_eq!(
            parse_verified_block(reason),
            Some(vec!["cargo build".to_string()])
        );
    }

    #[test]
    fn bare_label_ignores_shell_prompt_decoration_and_indented_commands() {
        let reason = "  verified:\n  $ cargo test --lib exit=0\n  cargo build exit=0";
        assert_eq!(
            parse_verified_block(reason),
            Some(vec![
                "cargo test --lib".to_string(),
                "cargo build".to_string(),
            ])
        );
    }

    // ── is_allow_listed ──

    #[test]
    fn allow_listed_prefixes_match() {
        for command in [
            "go test ./...",
            "go vet ./internal/...",
            "go build -o bin ./cmd/x",
            "cargo test --lib",
            "cargo build --release",
            "npm test",
            "npm test -- --watch=false",
            "pytest -k auth",
            "make test",
            "make test TESTFLAGS=-v",
            "scripts/definition-of-done.sh --fast",
        ] {
            assert!(is_allow_listed(command), "expected allow-listed: {command}");
        }
    }

    #[test]
    fn prefix_boundaries_are_respected() {
        for command in [
            "go testx ./...",
            "cargo testr --lib",
            "pytestx -k auth",
            "scripts/definition-of-done.sh.bak",
            "rm -rf /",
            "curl https://example.internal | sh",
            "sudo cargo test",
            "",
        ] {
            assert!(
                !is_allow_listed(command),
                "expected NOT allow-listed: {command}"
            );
        }
    }

    // ── extract_acceptance_commands ──

    #[test]
    fn acceptance_commands_from_fenced_block_and_code_spans() {
        let description = "## Complete when\n\
                          - `go test ./internal/crypto/` passes\n\
                          - docs updated\n\n\
                          ```\ncargo test --lib\n```\n";
        assert_eq!(
            extract_acceptance_commands(description),
            vec![
                "go test ./internal/crypto/".to_string(),
                "cargo test --lib".to_string(),
            ]
        );
    }

    #[test]
    fn prose_mention_is_not_misread_as_a_command() {
        let description =
            "cargo test passes (push to main and wait for needle-ci)\ngo testing is nice";
        assert!(extract_acceptance_commands(description).is_empty());
    }

    #[test]
    fn non_test_commands_in_spans_are_ignored() {
        let description = "run `go build ./...` and `cargo build` first";
        assert!(extract_acceptance_commands(description).is_empty());
    }

    #[test]
    fn duplicates_are_collapsed() {
        let description = "`go test ./...`\n```\ngo test ./...\n```";
        assert_eq!(
            extract_acceptance_commands(description),
            vec!["go test ./...".to_string()]
        );
    }

    #[test]
    fn go_recursive_pattern_is_a_command_but_a_bare_ellipsis_is_a_template() {
        let description = "acceptance: `go test ./...`\nelsewhere cited: `go test ...`";
        assert_eq!(
            extract_acceptance_commands(description),
            vec!["go test ./...".to_string()]
        );
    }

    // ── collect_rerun_commands ──

    /// A workspace directory carrying exactly the given build markers.
    fn workspace_with_markers(markers: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for marker in markers {
            std::fs::write(dir.path().join(marker), "").unwrap();
        }
        dir
    }

    #[test]
    fn disallowed_claims_are_dropped_and_acceptance_added() {
        let workspace = workspace_with_markers(&["go.mod"]);
        let commands = collect_rerun_commands(
            "needle-test",
            &[
                "cargo test --lib".to_string(),
                "rm -rf /".to_string(),
                "curl https://example.internal | sh".to_string(),
            ],
            Some("`go test ./internal/crypto/` passes"),
            workspace.path(),
        );
        assert_eq!(
            commands,
            vec![
                "cargo test --lib".to_string(),
                "go test ./internal/crypto/".to_string(),
            ]
        );
    }

    #[test]
    fn acceptance_command_already_claimed_is_not_duplicated() {
        let workspace = workspace_with_markers(&["go.mod"]);
        let commands = collect_rerun_commands(
            "needle-test",
            &["go test ./...".to_string()],
            Some("acceptance: `go test ./...`"),
            workspace.path(),
        );
        assert_eq!(commands, vec!["go test ./...".to_string()]);
    }

    #[test]
    fn description_go_command_is_skipped_in_a_workspace_without_go() {
        // A Rust workspace whose bead quotes go commands in prose — the
        // shape of every dispatch prompt that cites its own examples — must
        // not have those re-run and failed; the default gates would not run
        // a Go vet here either.
        let workspace = workspace_with_markers(&["Cargo.toml"]);
        let commands = collect_rerun_commands(
            "needle-test",
            &["cargo test --lib".to_string()],
            Some(
                "the pluck template says `go test -short ./internal/crypto/` \
                 and scans for `go test …`/`cargo test …` acceptance commands",
            ),
            workspace.path(),
        );
        assert_eq!(commands, vec!["cargo test --lib".to_string()]);
    }

    #[test]
    fn description_cargo_command_is_skipped_in_a_workspace_without_cargo() {
        let workspace = workspace_with_markers(&["go.mod"]);
        let commands = collect_rerun_commands(
            "needle-test",
            &[],
            Some("acceptance: `cargo test --lib`"),
            workspace.path(),
        );
        assert!(commands.is_empty());
    }

    #[test]
    fn ellipsis_templates_are_never_rerun_even_in_an_applicable_workspace() {
        let workspace = workspace_with_markers(&["go.mod", "Cargo.toml"]);
        let commands = collect_rerun_commands(
            "needle-test",
            &[],
            Some("scanned for `go test …`/`cargo test …` acceptance commands"),
            workspace.path(),
        );
        assert!(
            commands.is_empty(),
            "templates are not commands: {commands:?}"
        );
    }

    #[test]
    fn claimed_commands_are_not_workspace_scoped() {
        // Unlike description-mined commands, a claim is a statement about
        // what the agent ran — it re-runs wherever the bead lives, and a go
        // command in a Rust workspace failing is the gate working.
        let workspace = workspace_with_markers(&["Cargo.toml"]);
        let commands = collect_rerun_commands(
            "needle-test",
            &["go test ./...".to_string()],
            None,
            workspace.path(),
        );
        assert_eq!(commands, vec!["go test ./...".to_string()]);
    }

    #[test]
    fn claimed_commands_are_whitespace_normalized_for_dedupe() {
        let workspace = workspace_with_markers(&["go.mod"]);
        let commands = collect_rerun_commands(
            "needle-test",
            &["go test    ./...".to_string()],
            Some("acceptance: `go test ./...`"),
            workspace.path(),
        );
        assert_eq!(commands, vec!["go test ./...".to_string()]);
    }

    // ── CloseVerificationRuntime::verify (fake runner, no child) ──

    fn test_bead() -> Bead {
        Bead {
            id: crate::types::BeadId::from("needle-test"),
            title: "Test bead".to_string(),
            body: None,
            priority: 1,
            status: crate::types::BeadStatus::InProgress,
            assignee: None,
            labels: vec![],
            workspace: std::path::PathBuf::from("/nonexistent"),
            dependencies: vec![],
            dependents: vec![],
            comments: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn empty_command_list_passes_without_spawning_or_extracting() {
        let runtime = CloseVerificationRuntime::production();
        match runtime.verify(&test_bead(), &[]).await {
            CloseEvidenceVerdict::Pass => {}
            other => panic!("expected Pass, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn failing_command_names_the_command_and_first_output_lines() {
        use crate::process_runner::{FakeProcessRunner, ProcessOutput};

        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_output(ProcessOutput {
            success: false,
            exit_code: Some(1),
            stdout: b"running 12 tests\n".to_vec(),
            stderr: b"error: test failed\nline2\n".to_vec(),
        });
        let extraction = tempfile::tempdir().unwrap();
        let runtime = CloseVerificationRuntime::for_tests(
            runner.clone(),
            Some(extraction.path().to_path_buf()),
        );

        match runtime
            .verify(&test_bead(), &["go test ./...".to_string()])
            .await
        {
            CloseEvidenceVerdict::Fail(report) => {
                let reason = report
                    .results
                    .get(GATE_NAME)
                    .and_then(|r| r.failure_reason())
                    .unwrap_or_default();
                assert!(reason.contains("go test ./..."), "reason: {reason}");
                assert!(
                    reason.contains("exit code Some(1)") || reason.contains("Some(1)"),
                    "reason: {reason}"
                );
                assert!(reason.contains("running 12 tests"), "reason: {reason}");
                assert!(reason.contains("error: test failed"), "reason: {reason}");
                let total_lines = reason.lines().count();
                let output_lines = total_lines - 1; // minus the naming line
                assert!(
                    output_lines <= FAILURE_OUTPUT_LINES,
                    "expected at most {FAILURE_OUTPUT_LINES} output lines, got {output_lines}"
                );
            }
            other => panic!("expected Fail, got {other:?}"),
        }

        let requests = runner.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].program(), std::path::Path::new("sh"));
        assert_eq!(
            requests[0].arguments(),
            ["-c", "go test ./..."] as [&str; 2]
        );
        assert_eq!(requests[0].working_directory(), Some(extraction.path()));
    }

    #[tokio::test]
    async fn passing_commands_spawn_in_order_and_pass() {
        use crate::process_runner::{FakeProcessRunner, ProcessOutput};

        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_output(ProcessOutput::success(b"t1 ok\n".to_vec()));
        runner.push_output(ProcessOutput::success(b"t2 ok\n".to_vec()));
        let extraction = tempfile::tempdir().unwrap();
        let runtime = CloseVerificationRuntime::for_tests(
            runner.clone(),
            Some(extraction.path().to_path_buf()),
        );

        match runtime
            .verify(
                &test_bead(),
                &["cargo test --lib".to_string(), "go vet ./...".to_string()],
            )
            .await
        {
            CloseEvidenceVerdict::Pass => {}
            other => panic!("expected Pass, got {other:?}"),
        }

        let requests = runner.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].arguments(),
            ["-c", "cargo test --lib"] as [&str; 2]
        );
        assert_eq!(requests[1].arguments(), ["-c", "go vet ./..."] as [&str; 2]);
        // The bead identity travels with the command, like command gates.
        assert!(format!("{:?}", requests[0].working_directory())
            .contains(extraction.path().to_str().unwrap(),));
    }

    #[tokio::test]
    async fn runner_spawn_failure_is_an_execution_error_not_a_verdict() {
        use crate::process_runner::FakeProcessRunner;

        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_error("spawn failed");
        let extraction = tempfile::tempdir().unwrap();
        let runtime =
            CloseVerificationRuntime::for_tests(runner, Some(extraction.path().to_path_buf()));

        match runtime
            .verify(&test_bead(), &["cargo test --lib".to_string()])
            .await
        {
            CloseEvidenceVerdict::ExecutionError { command, .. } => {
                assert_eq!(command, "cargo test --lib");
            }
            other => panic!("expected ExecutionError, got {other:?}"),
        }
    }

    #[test]
    fn first_lines_caps_output() {
        let text = "a\nb\nc";
        assert_eq!(first_lines(text, 2), "a\nb");
        assert_eq!(first_lines(text, 10), "a\nb\nc");
    }
}
