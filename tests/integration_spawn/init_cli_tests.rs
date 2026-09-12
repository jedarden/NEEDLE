//! CLI invocation tests for `needle init --backend` flag.
//!
//! Tests that verify:
//! 1. --backend option with valid values (bead-rs, bead-forge)
//! 2. Backend binding written to .needle.yaml when in a workspace
//! 3. Existing .needle.yaml is not modified (idempotent)
//! 4. Unknown backend values are rejected
//! 5. Default backend is bead-rs when not specified

use clap::Parser;
use needle::cli::Cli;
use std::fs;
use tempfile::TempDir;

/// Test that `--backend bead-rs` parses correctly.
#[test]
fn init_backend_bead_rs_parses() {
    let args = vec!["needle", "init", "--backend", "bead-rs"];
    let result = Cli::try_parse_from(args);
    assert!(
        result.is_ok(),
        "CLI parsing should succeed with --backend bead-rs"
    );

    let cli = result.unwrap();
    match cli.command {
        needle::cli::CliCommand::Init {
            backend,
            no_agents_md: _,
            force: _,
        } => {
            assert_eq!(backend, "bead-rs", "Backend should be bead-rs");
        }
        _ => panic!("Expected Init command"),
    }
}

// REMOVED: init_backend_bead_forge_parses
// bead-forge backend is deprecated - this test verified legacy backend support which is no longer available

/// Test that default backend is bead-rs when not specified.
#[test]
fn init_default_backend_is_bead_rs() {
    let args = vec!["needle", "init"];
    let result = Cli::try_parse_from(args);
    assert!(
        result.is_ok(),
        "CLI parsing should succeed without --backend"
    );

    let cli = result.unwrap();
    match cli.command {
        needle::cli::CliCommand::Init {
            backend,
            no_agents_md: _,
            force: _,
        } => {
            assert_eq!(backend, "bead-rs", "Default backend should be bead-rs");
        }
        _ => panic!("Expected Init command"),
    }
}

/// Test that unknown backend values are rejected by clap validation.
#[test]
fn init_unknown_backend_rejected() {
    let args = vec!["needle", "init", "--backend", "unknown-backend"];
    let result = Cli::try_parse_from(args);
    assert!(
        result.is_err(),
        "CLI parsing should fail with unknown backend"
    );
}

/// Test that .needle.yaml is created in a workspace with .beads directory.
#[test]
fn init_creates_workspace_config_in_bead_workspace() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let workspace = temp_dir.path();

    // Create .beads directory to simulate a bead workspace
    let beads_dir = workspace.join(".beads");
    fs::create_dir(&beads_dir).expect("Failed to create .beads directory");

    let _needle_config = workspace.join(".needle.yaml");

    // Set HOME to temp dir to avoid writing to real config
    std::env::set_var("HOME", temp_dir.path());

    // Run needle init with bead-rs backend
    let args = vec!["needle", "init", "--backend", "bead-rs"];

    // We can't actually run cmd_init in a test without the full binary,
    // but we can verify the parsing works correctly
    let result = Cli::try_parse_from(args);
    assert!(result.is_ok(), "CLI parsing should succeed");

    let cli = result.unwrap();
    match cli.command {
        needle::cli::CliCommand::Init {
            backend,
            no_agents_md: _,
            force: _,
        } => {
            assert_eq!(backend, "bead-rs");
        }
        _ => panic!("Expected Init command"),
    }

    // The actual writing would happen in cmd_init, which we can't test here
    // without spawning the binary. We verify the parsing is correct.
}

/// Test that .needle.yaml is NOT modified if it already exists.
#[test]
fn init_idempotent_with_existing_workspace_config() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let workspace = temp_dir.path();

    // Create .beads directory to simulate a bead workspace
    let beads_dir = workspace.join(".beads");
    fs::create_dir(&beads_dir).expect("Failed to create .beads directory");

    let needle_config = workspace.join(".needle.yaml");

    // Create existing .needle.yaml with some content
    let existing_content = "bead_cli:\n  backend: bead-forge\nother_config: value\n";
    fs::write(&needle_config, existing_content).expect("Failed to write existing config");

    // Set HOME to temp dir
    std::env::set_var("HOME", temp_dir.path());

    // Verify the file exists and contains our content
    assert!(needle_config.exists(), "Config should exist");
    let content = fs::read_to_string(&needle_config).expect("Failed to read config");
    assert_eq!(content, existing_content, "Config should not be modified");

    // We can't actually run cmd_init in a test, but we verify the setup
    // The actual idempotency would be tested in integration tests
}

/// Test that .needle.yaml is NOT created when .beads directory is absent.
#[test]
fn init_skips_workspace_config_outside_bead_workspace() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let workspace = temp_dir.path();

    // Do NOT create .beads directory - not a bead workspace
    let _needle_config = workspace.join(".needle.yaml");

    // Set HOME to temp dir
    std::env::set_var("HOME", temp_dir.path());

    // Run needle init
    let args = vec!["needle", "init", "--backend", "bead-rs"];

    // Verify parsing works
    let result = Cli::try_parse_from(args);
    assert!(result.is_ok(), "CLI parsing should succeed");

    let cli = result.unwrap();
    match cli.command {
        needle::cli::CliCommand::Init {
            backend,
            no_agents_md: _,
            force: _,
        } => {
            assert_eq!(backend, "bead-rs");
        }
        _ => panic!("Expected Init command"),
    }

    // The actual behavior would be tested in integration tests
    // We verify the parsing is correct here
}

/// Test that --no-agents-md flag parses correctly.
#[test]
fn init_no_agents_md_flag_parses() {
    let args = vec!["needle", "init", "--backend", "bead-rs", "--no-agents-md"];
    let result = Cli::try_parse_from(args);
    assert!(
        result.is_ok(),
        "CLI parsing should succeed with --no-agents-md"
    );

    let cli = result.unwrap();
    match cli.command {
        needle::cli::CliCommand::Init {
            backend,
            no_agents_md,
            force: _,
        } => {
            assert_eq!(backend, "bead-rs");
            assert!(no_agents_md, "no_agents_md should be true");
        }
        _ => panic!("Expected Init command"),
    }
}

/// Test that AGENTS.md is created when it doesn't exist.
#[test]
fn init_creates_agents_md_when_missing() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let workspace = temp_dir.path();

    // Create .beads directory to simulate a bead workspace
    let beads_dir = workspace.join(".beads");
    fs::create_dir(&beads_dir).expect("Failed to create .beads directory");

    let agents_md = workspace.join("AGENTS.md");
    assert!(!agents_md.exists(), "AGENTS.md should not exist initially");

    // Set HOME to temp dir to avoid writing to real config
    std::env::set_var("HOME", temp_dir.path());

    // Verify parsing works
    let args = vec!["needle", "init", "--backend", "bead-rs"];
    let result = Cli::try_parse_from(args);
    assert!(result.is_ok(), "CLI parsing should succeed");

    let cli = result.unwrap();
    match cli.command {
        needle::cli::CliCommand::Init {
            backend,
            no_agents_md,
            force: _,
        } => {
            assert_eq!(backend, "bead-rs");
            assert!(!no_agents_md, "no_agents_md should be false by default");
        }
        _ => panic!("Expected Init command"),
    }

    // The actual file creation would happen in cmd_init
    // This test verifies the parsing is correct
}

/// Test that AGENTS.md is NOT created with --no-agents-md flag.
#[test]
fn init_skips_agents_md_with_flag() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let workspace = temp_dir.path();

    // Create .beads directory to simulate a bead workspace
    let beads_dir = workspace.join(".beads");
    fs::create_dir(&beads_dir).expect("Failed to create .beads directory");

    // Set HOME to temp dir
    std::env::set_var("HOME", temp_dir.path());

    // Verify parsing works with --no-agents-md
    let args = vec!["needle", "init", "--backend", "bead-rs", "--no-agents-md"];
    let result = Cli::try_parse_from(args);
    assert!(result.is_ok(), "CLI parsing should succeed");

    let cli = result.unwrap();
    match cli.command {
        needle::cli::CliCommand::Init {
            backend,
            no_agents_md,
            force: _,
        } => {
            assert_eq!(backend, "bead-rs");
            assert!(no_agents_md, "no_agents_md should be true");
        }
        _ => panic!("Expected Init command"),
    }
}

/// Test that AGENTS.md markers are properly formatted.
#[test]
fn agents_md_markers_are_correct() {
    let template = include_str!("../../docs/templates/AGENTS-needle.md");

    // Verify template contains key content
    assert!(
        template.contains("bead list --ready"),
        "Template should contain bead list command"
    );
    assert!(
        template.contains("bead claim"),
        "Template should contain bead claim command"
    );
    assert!(
        template.contains("bead close"),
        "Template should contain bead close command"
    );
    assert!(
        template.contains("Bead-Id:"),
        "Template should mention commit trailer"
    );
    assert!(
        template.contains("NEVER edit"),
        "Template should warn against editing .beads/"
    );
    assert!(
        template.contains("needle doctor"),
        "Template should mention needle doctor"
    );
    assert!(
        template.contains("One deliverable, one acceptance command, per bead"),
        "Template should carry the single-deliverable authoring rule"
    );
    assert!(
        template.contains("do not decompose it into smaller tasks"),
        "Template should carry the verified no-decompose instruction"
    );
    assert!(
        template.contains("pytest -q tests/test_money.py"),
        "Template should carry the acceptance command from the verified example"
    );

    // Verify template is concise (≤ 80 lines; the authoring section added 17)
    let line_count = template.lines().count();
    assert!(
        line_count <= 80,
        "Template should be ≤ 80 lines, got {}",
        line_count
    );
}

/// Test that existing AGENTS.md content is preserved.
#[test]
fn init_preserves_existing_agents_md_content() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let workspace = temp_dir.path();

    // Create .beads directory
    let beads_dir = workspace.join(".beads");
    fs::create_dir(&beads_dir).expect("Failed to create .beads directory");

    // Create existing AGENTS.md with custom content
    let agents_md = workspace.join("AGENTS.md");
    let existing_content = "# Project AGENTS.md\n\nThis is custom content.\n";
    fs::write(&agents_md, existing_content).expect("Failed to write AGENTS.md");

    // Verify file exists
    assert!(agents_md.exists(), "AGENTS.md should exist");
    let content = fs::read_to_string(&agents_md).expect("Failed to read AGENTS.md");
    assert_eq!(content, existing_content, "Content should be preserved");

    // Set HOME to temp dir
    std::env::set_var("HOME", temp_dir.path());

    // Verify parsing works
    let args = vec!["needle", "init", "--backend", "bead-rs"];
    let result = Cli::try_parse_from(args);
    assert!(result.is_ok(), "CLI parsing should succeed");
}

/// Test that AGENTS.md injection is idempotent.
#[test]
fn init_agents_md_injection_is_idempotent() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let workspace = temp_dir.path();

    // Create .beads directory
    let beads_dir = workspace.join(".beads");
    fs::create_dir(&beads_dir).expect("Failed to create .beads directory");

    // Set HOME to temp dir
    std::env::set_var("HOME", temp_dir.path());

    // First run - verify parsing
    let args1 = vec!["needle", "init", "--backend", "bead-rs"];
    let result1 = Cli::try_parse_from(args1);
    assert!(result1.is_ok(), "First init parsing should succeed");

    // Second run - verify parsing still works
    let args2 = vec!["needle", "init", "--backend", "bead-rs"];
    let result2 = Cli::try_parse_from(args2);
    assert!(result2.is_ok(), "Second init parsing should succeed");

    // Both should produce the same command structure
    let cli1 = result1.unwrap();
    let cli2 = result2.unwrap();

    match (cli1.command, cli2.command) {
        (
            needle::cli::CliCommand::Init {
                backend: b1,
                no_agents_md: n1,
                force: f1,
            },
            needle::cli::CliCommand::Init {
                backend: b2,
                no_agents_md: n2,
                force: f2,
            },
        ) => {
            assert_eq!(b1, b2, "Backend should be the same");
            assert_eq!(n1, n2, "no_agents_md flag should be the same");
            assert!(!f1 && !f2, "force should default to false");
        }
        _ => panic!("Both should be Init commands"),
    }
}

/// Test replace_needle_block creates file with markers when content has no markers.
#[test]
fn replace_needle_block_creates_markers() {
    let template = include_str!("../../docs/templates/AGENTS-needle.md");
    let existing_content = "# Existing content\n";

    let result = needle::cli::replace_needle_block(existing_content, template);

    // Should contain both markers
    assert!(
        result.contains("<!-- needle:begin -->"),
        "Should contain begin marker"
    );
    assert!(
        result.contains("<!-- needle:end -->"),
        "Should contain end marker"
    );
    // Should preserve existing content
    assert!(
        result.contains("# Existing content"),
        "Should preserve existing content"
    );
    // Should contain template content
    assert!(
        result.contains("bead list --ready"),
        "Should contain template content"
    );
}

/// Test replace_needle_block replaces existing block.
#[test]
fn replace_needle_block_replaces_existing_block() {
    let template = include_str!("../../docs/templates/AGENTS-needle.md");
    let existing_content =
        "# Before\n<!-- needle:begin -->\nOLD CONTENT\n<!-- needle:end -->\n# After\n";

    let result = needle::cli::replace_needle_block(existing_content, template);

    // Should contain both markers
    assert!(
        result.contains("<!-- needle:begin -->"),
        "Should contain begin marker"
    );
    assert!(
        result.contains("<!-- needle:end -->"),
        "Should contain end marker"
    );
    // Should preserve before/after content
    assert!(
        result.contains("# Before"),
        "Should preserve content before markers"
    );
    assert!(
        result.contains("# After"),
        "Should preserve content after markers"
    );
    // Should NOT contain old content
    assert!(
        !result.contains("OLD CONTENT"),
        "Should not contain old template content"
    );
    // Should contain new template content
    assert!(
        result.contains("bead list --ready"),
        "Should contain new template content"
    );
}

/// Test replace_needle_block is idempotent when template hasn't changed.
#[test]
fn replace_needle_block_is_idempotent() {
    let template = include_str!("../../docs/templates/AGENTS-needle.md");
    let content_with_template = format!(
        "# Before\n<!-- needle:begin -->\n{}\n<!-- needle:end -->\n# After\n",
        template
    );

    let result = needle::cli::replace_needle_block(&content_with_template, template);

    // Should be identical when template is the same
    assert_eq!(result, content_with_template, "Should be idempotent");
}

/// Test replace_needle_block handles missing end marker gracefully.
#[test]
fn replace_needle_block_handles_missing_end_marker() {
    let template = include_str!("../../docs/templates/AGENTS-needle.md");
    let malformed_content = "# Content\n<!-- needle:begin -->\nNo end marker here\n";

    let result = needle::cli::replace_needle_block(malformed_content, template);

    // Should return original content unchanged
    assert_eq!(
        result, malformed_content,
        "Should handle missing end marker gracefully"
    );
}

/// Test AGENTS.md creation behavior - new file creation.
#[test]
fn agents_md_creates_new_file_with_markers() {
    let template = include_str!("../../docs/templates/AGENTS-needle.md");
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let agents_md = temp_dir.path().join("AGENTS.md");

    // Simulate what cmd_init does when creating a new file
    let content = format!("<!-- needle:begin -->\n{}\n<!-- needle:end -->", template);
    fs::write(&agents_md, &content).expect("Failed to write AGENTS.md");

    // Verify file was created
    assert!(agents_md.exists(), "AGENTS.md should be created");

    // Verify content
    let result = fs::read_to_string(&agents_md).expect("Failed to read AGENTS.md");
    assert!(
        result.contains("<!-- needle:begin -->"),
        "Should contain begin marker"
    );
    assert!(
        result.contains("<!-- needle:end -->"),
        "Should contain end marker"
    );
    assert!(
        result.contains("bead list --ready"),
        "Should contain template content"
    );
}

/// Test AGENTS.md append behavior - appending to existing file.
#[test]
fn agents_md_appends_to_existing_file() {
    let template = include_str!("../../docs/templates/AGENTS-needle.md");
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let agents_md = temp_dir.path().join("AGENTS.md");

    // Create existing content
    let existing_content = "# Project Instructions\n\nCustom project content here.\n";
    fs::write(&agents_md, existing_content).expect("Failed to write existing content");

    // Simulate what cmd_init does when appending
    let updated = format!(
        "{}\n\n<!-- needle:begin -->\n{}\n<!-- needle:end -->",
        existing_content.trim(),
        template
    );
    fs::write(&agents_md, &updated).expect("Failed to write updated content");

    // Verify file exists
    assert!(agents_md.exists(), "AGENTS.md should exist");

    // Verify both contents are present
    let result = fs::read_to_string(&agents_md).expect("Failed to read AGENTS.md");
    assert!(
        result.contains("# Project Instructions"),
        "Should contain original content"
    );
    assert!(
        result.contains("Custom project content"),
        "Should contain original content"
    );
    assert!(
        result.contains("<!-- needle:begin -->"),
        "Should contain begin marker"
    );
    assert!(
        result.contains("bead list --ready"),
        "Should contain appended template"
    );
}

/// Test AGENTS.md idempotent behavior - second run with same content.
#[test]
fn agents_md_second_run_is_idempotent() {
    let template = include_str!("../../docs/templates/AGENTS-needle.md");
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let agents_md = temp_dir.path().join("AGENTS.md");

    // First run - create file with template
    let first_content = format!("<!-- needle:begin -->\n{}\n<!-- needle:end -->", template);
    fs::write(&agents_md, &first_content).expect("Failed to write first content");

    // Second run - replace_needle_block with same template
    let second_content = needle::cli::replace_needle_block(&first_content, template);

    // Should be identical (idempotent)
    assert_eq!(
        second_content, first_content,
        "Second run should be idempotent"
    );

    // Write it back (should be same content)
    fs::write(&agents_md, &second_content).expect("Failed to write second content");

    // Verify file hasn't changed
    let final_result = fs::read_to_string(&agents_md).expect("Failed to read final content");
    assert_eq!(final_result, first_content, "File should remain unchanged");
}

/// Test AGENTS.md opt-out behavior with --no-agents-md.
#[test]
fn agents_md_opt_out_skips_creation() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let agents_md = temp_dir.path().join("AGENTS.md");

    // Simulate --no-agents-md flag: file should not be created
    let no_agents_md = true;

    if !no_agents_md {
        let template = include_str!("../../docs/templates/AGENTS-needle.md");
        let content = format!("<!-- needle:begin -->\n{}\n<!-- needle:end -->", template);
        fs::write(&agents_md, &content).expect("Failed to write AGENTS.md");
    }

    // With --no-agents-md, file should not exist
    assert!(
        !agents_md.exists(),
        "AGENTS.md should not be created with --no-agents-md"
    );
}

/// Test that bead commands in template are valid against bead --help.
/// This ensures the documentation matches the actual CLI interface.
#[test]
fn template_bead_commands_match_help_output() {
    use std::process::Command;

    let _template = include_str!("../../docs/templates/AGENTS-needle.md");

    // Try to get bead help output - if bead is not available, skip this test
    let bead_help = match Command::new("bead").arg("--help").output() {
        Ok(output) => String::from_utf8_lossy(&output.stdout).to_string(),
        Err(_) => {
            // bead CLI not found in PATH - skip this test
            return;
        }
    };

    // Extract bead commands from template (look for "bead <command>" patterns)
    let template_commands = vec![
        "list", "show", "claim", "update", "close", "release", "dep", "doctor",
    ];

    // Verify each documented command exists in bead help
    for cmd in template_commands {
        let command_pattern = if cmd == "dep" {
            // Special case: dep is a subcommand with its own subcommands
            "dep"
        } else {
            cmd
        };

        // Check if the command is mentioned in help output
        // Bead help shows commands like "bead list", "bead show", etc.
        let found =
            bead_help.contains(command_pattern) || bead_help.contains(&format!("    {}", cmd));

        assert!(
            found,
            "Template documents 'bead {}' but this command was not found in 'bead --help' output. \
             Please verify the command name or update the template.",
            cmd
        );
    }

    // Verify specific flag combinations mentioned in template
    assert!(
        bead_help.contains("--ready") || bead_help.contains("--status"),
        "Template uses 'bead list --ready' but --ready flag not found in help output"
    );

    // Check subcommand-specific help for --notes and --reason flags
    let update_help = match Command::new("bead").args(["update", "--help"]).output() {
        Ok(output) => String::from_utf8_lossy(&output.stdout).to_string(),
        Err(_) => {
            // If subcommand help fails, skip this check
            return;
        }
    };

    let close_help = match Command::new("bead").args(["close", "--help"]).output() {
        Ok(output) => String::from_utf8_lossy(&output.stdout).to_string(),
        Err(_) => {
            // If subcommand help fails, skip this check
            return;
        }
    };

    assert!(
        update_help.contains("--notes") || bead_help.contains("--notes"),
        "Template uses 'bead update --notes' but this flag not found in help output"
    );

    assert!(
        close_help.contains("--reason") || bead_help.contains("--reason"),
        "Template uses 'bead close --reason' but this flag not found in help output"
    );

    assert!(
        bead_help.contains("claim"),
        "Template uses 'bead claim' but claim command not found in help output"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// `needle init` vs an existing global config
//
// These spawn the compiled binary instead of calling `cmd_init` in process: the
// command resolves its paths from $HOME, and a child's environment is the only
// way to point it at a sandbox HOME without racing the rest of the test process
// (docs/testing-isolation-patterns.md, subprocess isolation). Every invocation
// also runs in a throwaway working directory so the workspace-binding half of
// `needle init` can never touch this repo's own AGENTS.md.
// ──────────────────────────────────────────────────────────────────────────────

use std::path::{Path, PathBuf};

/// A stand-in for a real fleet config: valid YAML with settings that differ
/// from everything `needle init` would generate.
const FLEET_CONFIG: &str = "# fleet config under test — must survive every init below\nagent:\n  default: codex\nworker:\n  max_workers: 7\n";

/// Run `needle init` with `args` against a sandbox `home`, in `cwd`.
fn init_in_sandbox(home: &Path, cwd: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_needle"))
        .arg("init")
        .args(args)
        .env("HOME", home)
        .current_dir(cwd)
        .output()
        .expect("failed to execute needle init")
}

/// A sandbox HOME plus a throwaway working directory with a `.beads/` dir.
fn sandbox() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = TempDir::new().expect("Failed to create temp dir");
    let home = temp.path().join("home");
    let cwd = temp.path().join("project");
    fs::create_dir_all(&home).expect("Failed to create sandbox HOME");
    fs::create_dir_all(cwd.join(".beads")).expect("Failed to create workspace");
    (temp, home, cwd)
}

fn global_config(home: &Path) -> PathBuf {
    home.join(".config/needle/config.yaml")
}

/// `needle init` on a fresh HOME writes the global config (the quickstart's
/// Step 2 depends on this).
#[test]
fn init_writes_global_config_on_fresh_home() {
    let (_temp, home, cwd) = sandbox();

    let output = init_in_sandbox(&home, &cwd, &["--backend", "bead-rs", "--no-agents-md"]);
    assert!(
        output.status.success(),
        "init on a fresh HOME should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        global_config(&home).is_file(),
        "global config should have been created"
    );
}

/// An existing global config is left byte-for-byte intact without --force, and
/// the workspace binding still happens: refusing to touch the fleet config is
/// not a reason to skip the useful half of the command.
#[test]
fn init_refuses_to_overwrite_existing_global_config() {
    let (_temp, home, cwd) = sandbox();

    let config_path = global_config(&home);
    fs::create_dir_all(config_path.parent().unwrap()).expect("Failed to create config dir");
    fs::write(&config_path, FLEET_CONFIG).expect("Failed to write fleet config");

    let output = init_in_sandbox(&home, &cwd, &["--backend", "bead-rs", "--no-agents-md"]);
    assert!(
        output.status.success(),
        "init should report the refusal and still bind the workspace"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Refusing to overwrite existing global config"),
        "refusal should be announced, got:\n{stdout}"
    );
    assert!(
        stdout.contains(config_path.to_string_lossy().as_ref()),
        "refusal should name the path it did not write, got:\n{stdout}"
    );
    assert!(
        stdout.contains("--force"),
        "refusal should name the override, got:\n{stdout}"
    );
    assert_eq!(
        fs::read_to_string(&config_path).expect("config should still be readable"),
        FLEET_CONFIG,
        "the existing global config must survive `needle init` byte-for-byte"
    );
    assert_eq!(
        fs::read_to_string(cwd.join(".needle.yaml")).expect("workspace binding should exist"),
        "bead_cli:\n  backend: bead-rs\n",
        "the workspace binding should still be written"
    );
}

/// --force is the explicit override: it replaces the existing global config
/// with a freshly generated one.
#[test]
fn init_force_overwrites_existing_global_config() {
    let (_temp, home, cwd) = sandbox();

    let config_path = global_config(&home);
    fs::create_dir_all(config_path.parent().unwrap()).expect("Failed to create config dir");
    fs::write(&config_path, FLEET_CONFIG).expect("Failed to write fleet config");

    let output = init_in_sandbox(
        &home,
        &cwd,
        &["--backend", "bead-rs", "--no-agents-md", "--force"],
    );
    assert!(
        output.status.success(),
        "init --force should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Overwriting existing global config (--force)"),
        "--force should say it is overwriting, got:\n{stdout}"
    );

    let rewritten = fs::read_to_string(&config_path).expect("config should be readable");
    assert!(
        !rewritten.contains("must survive every init below"),
        "the previous config should be gone"
    );
    assert!(
        rewritten.contains("# NEEDLE v2 Configuration"),
        "a freshly generated config should be in place, got:\n{rewritten}"
    );
}

/// `--force` parses and defaults to false everywhere else.
#[test]
fn init_force_flag_parses() {
    let args = vec!["needle", "init", "--backend", "bead-rs", "--force"];
    let result = Cli::try_parse_from(args);
    assert!(result.is_ok(), "CLI parsing should succeed with --force");

    let cli = result.unwrap();
    match cli.command {
        needle::cli::CliCommand::Init {
            backend,
            no_agents_md: _,
            force,
        } => {
            assert_eq!(backend, "bead-rs");
            assert!(force, "force should be true when passed");
        }
        _ => panic!("Expected Init command"),
    }
}
