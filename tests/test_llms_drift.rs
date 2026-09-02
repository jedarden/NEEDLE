//! Drift check: every command line in llms.txt must appear verbatim as a command
//! line in README.md's Quickstart block, so the agent-facing quickstart cannot
//! diverge from the human-facing one.

use std::fs;
use std::path::Path;

/// First word of a line that counts as a runnable quickstart command.
const COMMANDS: [&str; 6] = ["curl", "cd", "bead", "needle", "tmux", "git"];

fn command_lines(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|line| match line.split_whitespace().next() {
            Some(first) => COMMANDS.contains(&first),
            None => false,
        })
        .collect()
}

#[test]
fn llms_txt_commands_match_readme_quickstart() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let llms = fs::read_to_string(repo_root.join("llms.txt")).expect("llms.txt must exist");
    let readme = fs::read_to_string(repo_root.join("README.md")).expect("README.md must exist");

    // llms.txt is meant to stay a single-screen runbook.
    assert!(
        llms.lines().count() <= 40,
        "llms.txt must stay at or under 40 lines (it is {})",
        llms.lines().count()
    );

    // Scope the comparison to README's Quickstart bash block, so a command that
    // merely appears elsewhere in the README does not satisfy the check.
    let (_, quickstart) = readme
        .split_once("## 🚀 Quickstart")
        .expect("README must contain a '## 🚀 Quickstart' section");
    let opened = quickstart
        .find("```bash")
        .expect("Quickstart must contain a bash code block");
    let body = &quickstart[opened + "```bash".len()..];
    let (block, _) = body
        .split_once("```")
        .expect("Quickstart bash code block must be closed");

    let readme_commands = command_lines(block);
    assert!(
        !readme_commands.is_empty(),
        "Quickstart bash block must contain at least one command"
    );

    let missing: Vec<&str> = command_lines(&llms)
        .into_iter()
        .filter(|cmd| !readme_commands.contains(cmd))
        .collect();

    assert!(
        missing.is_empty(),
        "llms.txt commands missing from README Quickstart:\n  {}",
        missing.join("\n  ")
    );
}
