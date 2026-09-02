//! Test that ensures commands in llms.txt appear verbatim in README.md Quickstart
//! This prevents documentation drift between agent-facing and human-facing docs.

use std::path::Path;

#[test]
fn llms_txt_commands_match_readme_quickstart() {
    let llms_path = Path::new("llms.txt");
    let readme_path = Path::new("README.md");

    // Read both files
    let llms_content = std::fs::read_to_string(llms_path).expect("llms.txt must exist");
    let readme_content = std::fs::read_to_string(readme_path).expect("README.md must exist");

    // Extract executable commands from llms.txt (lines that start with a command)
    let llms_commands: Vec<&str> = llms_content
        .lines()
        .filter(|line| {
            // Skip comments, empty lines, and section headers
            !line.trim().is_empty()
                && !line.trim().starts_with('#')
                && !line.trim().starts_with('-')
                && !line.trim().starts_with('*')
        })
        .map(|line| line.trim())
        .collect();

    // Extract commands from README Quickstart section
    let quickstart_start = readme_content
        .find("## 🚀 Quickstart")
        .expect("README must contain Quickstart section");

    let readme_after_quickstart = &readme_content[quickstart_start..];

    // Check each llms.txt command appears verbatim in README Quickstart
    let mut missing_commands = Vec::new();
    for command in llms_commands {
        // Skip descriptive-only lines from llms.txt (like "## Install")
        if command.starts_with("##") || command.starts_with("*") {
            continue;
        }

        // Handle multi-line commands (tmux attach -t appears on separate line in README)
        if command.contains("tmux attach") {
            // Check for either combined or separate format
            let combined_check = readme_after_quickstart.contains(command);
            let separate_check = command.contains("needle status")
                && readme_after_quickstart.contains("needle status")
                && readme_after_quickstart.contains("tmux attach");

            if !combined_check && !separate_check {
                missing_commands.push(command);
            }
        } else if !readme_after_quickstart.contains(command) {
            missing_commands.push(command);
        }
    }

    if !missing_commands.is_empty() {
        panic!(
            "Commands in llms.txt not found in README Quickstart:\n\
             {}\n\
             \n\
             Keep llms.txt and README Quickstart in sync. The agent-facing docs must \
             match the human-facing docs exactly.",
            missing_commands.join("\n")
        );
    }
}
