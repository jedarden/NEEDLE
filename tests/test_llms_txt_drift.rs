// Test that llms.txt commands match README.md Quickstart verbatim
// This prevents documentation drift between the agent-readable quickstart and the human README

use std::fs;
use std::path::Path;

#[test]
fn llms_txt_commands_match_readme_quickstart() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let llms_txt_path = repo_root.join("llms.txt");
    let readme_path = repo_root.join("README.md");

    // Read both files
    let llms_content = fs::read_to_string(&llms_txt_path).expect("llms.txt must exist");
    let readme_content = fs::read_to_string(&readme_path).expect("README.md must exist");

    // Extract commands from llms.txt (lines that start with common shell commands)
    let llms_commands: Vec<String> = llms_content
        .lines()
        .filter(|line| {
            line.starts_with("curl ")
                || line.starts_with("cd ")
                || line.starts_with("bead ")
                || line.starts_with("needle ")
                || line.starts_with("tmux ")
        })
        .map(|s| s.to_string())
        .collect();

    // Extract commands from README.md Quickstart section
    let quickstart_start = readme_content
        .find("## 🚀 Quickstart")
        .expect("README must have Quickstart section");

    let quickstart_section = &readme_content[quickstart_start..];
    let code_block_start = quickstart_start
        + quickstart_section
            .find("```bash")
            .expect("Quickstart must have bash code block");

    let code_block_section = &readme_content[code_block_start..];
    let code_block_end = code_block_section.find("```").expect("Code block must end");

    let code_block = &code_block_section[..code_block_end];

    let readme_commands: Vec<String> = code_block
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| {
            // Keep only actual command lines (skip empty, comments, whitespace-only)
            !line.is_empty()
                && !line.starts_with('#')
                && !line.starts_with("//")
                && !line.chars().all(char::is_whitespace)
                && (line.starts_with("curl ")
                    || line.starts_with("cd ")
                    || line.starts_with("bead ")
                    || line.starts_with("needle ")
                    || line.starts_with("tmux "))
        })
        .collect();

    // Check that every llms.txt command appears in README
    for llms_cmd in &llms_commands {
        let cmd_normalized = llms_cmd.trim();
        if !readme_commands.iter().any(|r| r.trim() == cmd_normalized) {
            panic!(
                "Command in llms.txt not found in README Quickstart: '{}'\n\
                 llms.txt commands: {:?}\n\
                 README commands: {:?}",
                cmd_normalized, llms_commands, readme_commands
            );
        }
    }

    // Note: We only check that llms.txt commands appear in README Quickstart.
    // README may have additional commands in other sections (examples, docs, etc.),
    // which is fine - llms.txt is the minimal quickstart reference.
}
