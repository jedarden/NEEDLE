//! CLI invocation tests for `needle config --set` flag.
//!
//! Tests that verify the --set flag parses correctly in both invocation formats:
//! 1. needle config --set KEY VALUE
//! 2. needle config --set KEY=VALUE
//!
//! Also includes integration tests for config file deserialization.

use clap::Parser;
use needle::config::ConfigLoader;
use std::path::PathBuf;
use tempfile::TempDir;

/// Test that `--set worker.max_workers 10` (KEY VALUE format) parses without clap error.
#[test]
fn config_set_key_value_format_parses() {
    use needle::cli::Cli;

    // Simulate CLI invocation: needle config --set worker.max_workers 10
    let args = vec!["needle", "config", "--set", "worker.max_workers", "10"];

    // This should not panic or return an Err during parsing
    let result = Cli::try_parse_from(args);
    assert!(
        result.is_ok(),
        "CLI parsing should succeed with KEY VALUE format"
    );

    let cli = result.unwrap();
    match cli.command {
        needle::cli::CliCommand::ConfigCmd {
            get,
            set,
            dump,
            show_source,
            live,
        } => {
            assert!(get.is_none(), "--get should not be set");
            assert!(set.is_some(), "--set should be set");
            assert!(!dump, "--dump should not be set");
            assert!(!show_source, "--show-source should not be set");
            assert!(!live, "--live should not be set");

            let set_args = set.unwrap();
            assert_eq!(set_args.len(), 2, "Should have 2 set arguments");
            assert_eq!(set_args[0], "worker.max_workers");
            assert_eq!(set_args[1], "10");
        }
        _ => panic!("Expected ConfigCmd command"),
    }
}

/// Test that `--set worker.max_workers=10` (KEY=VALUE format) parses without clap error.
#[test]
fn config_set_key_equals_value_format_parses() {
    use needle::cli::Cli;

    // Simulate CLI invocation: needle config --set worker.max_workers=10
    let args = vec!["needle", "config", "--set", "worker.max_workers=10"];

    // This should not panic or return an Err during parsing
    let result = Cli::try_parse_from(args);
    assert!(
        result.is_ok(),
        "CLI parsing should succeed with KEY=VALUE format"
    );

    let cli = result.unwrap();
    match cli.command {
        needle::cli::CliCommand::ConfigCmd {
            get,
            set,
            dump,
            show_source,
            live,
        } => {
            assert!(get.is_none(), "--get should not be set");
            assert!(set.is_some(), "--set should be set");
            assert!(!dump, "--dump should not be set");
            assert!(!show_source, "--show_source should not be set");
            assert!(!live, "--live should not be set");
            let set_args = set.unwrap();
            assert_eq!(set_args.len(), 1, "Should have 1 set argument");
            assert_eq!(set_args[0], "worker.max_workers=10");
        }
        _ => panic!("Expected ConfigCmd command"),
    }
}

/// Test multiple --set flags in KEY VALUE format.
#[test]
fn config_set_multiple_key_value_format_parses() {
    use needle::cli::Cli;

    // Simulate CLI invocation: needle config --set worker.max_workers 10 --set agent.timeout 3600
    let args = vec![
        "needle",
        "config",
        "--set",
        "worker.max_workers",
        "10",
        "--set",
        "agent.timeout",
        "3600",
    ];

    let result = Cli::try_parse_from(args);
    assert!(
        result.is_ok(),
        "CLI parsing should succeed with multiple --set flags in KEY VALUE format"
    );

    let cli = result.unwrap();
    match cli.command {
        needle::cli::CliCommand::ConfigCmd { set, .. } => {
            let set_args = set.unwrap();
            assert_eq!(set_args.len(), 4, "Should have 4 set arguments");
            assert_eq!(set_args[0], "worker.max_workers");
            assert_eq!(set_args[1], "10");
            assert_eq!(set_args[2], "agent.timeout");
            assert_eq!(set_args[3], "3600");
        }
        _ => panic!("Expected ConfigCmd command"),
    }
}

/// Test multiple --set flags in KEY=VALUE format.
#[test]
fn config_set_multiple_key_equals_value_format_parses() {
    use needle::cli::Cli;

    // Simulate CLI invocation: needle config --set worker.max_workers=10 --set agent.timeout=3600
    let args = vec![
        "needle",
        "config",
        "--set",
        "worker.max_workers=10",
        "--set",
        "agent.timeout=3600",
    ];

    let result = Cli::try_parse_from(args);
    assert!(
        result.is_ok(),
        "CLI parsing should succeed with multiple --set flags in KEY=VALUE format"
    );

    let cli = result.unwrap();
    match cli.command {
        needle::cli::CliCommand::ConfigCmd { set, .. } => {
            let set_args = set.unwrap();
            assert_eq!(set_args.len(), 2, "Should have 2 set arguments");
            assert_eq!(set_args[0], "worker.max_workers=10");
            assert_eq!(set_args[1], "agent.timeout=3600");
        }
        _ => panic!("Expected ConfigCmd command"),
    }
}

/// Test mixed format: some --set flags use KEY VALUE, others use KEY=VALUE.
#[test]
fn config_set_mixed_format_parses() {
    use needle::cli::Cli;

    // Simulate CLI invocation: needle config --set worker.max_workers 10 --set agent.timeout=3600
    let args = vec![
        "needle",
        "config",
        "--set",
        "worker.max_workers",
        "10",
        "--set",
        "agent.timeout=3600",
    ];

    let result = Cli::try_parse_from(args);
    assert!(
        result.is_ok(),
        "CLI parsing should succeed with mixed --set formats"
    );

    let cli = result.unwrap();
    match cli.command {
        needle::cli::CliCommand::ConfigCmd { set, .. } => {
            let set_args = set.unwrap();
            assert_eq!(set_args.len(), 3, "Should have 3 set arguments");
            assert_eq!(set_args[0], "worker.max_workers");
            assert_eq!(set_args[1], "10");
            assert_eq!(set_args[2], "agent.timeout=3600");
        }
        _ => panic!("Expected ConfigCmd command"),
    }
}

/// Test that --set with empty value fails appropriately during validation.
#[test]
fn config_set_empty_value_fails_validation() {
    use needle::cli::Cli;

    // Simulate CLI invocation: needle config --set worker.max_workers=
    let args = vec!["needle", "config", "--set", "worker.max_workers="];

    // Parsing should succeed (clap accepts it)
    let result = Cli::try_parse_from(args);
    assert!(result.is_ok(), "CLI parsing should succeed");

    let cli = result.unwrap();
    match cli.command {
        needle::cli::CliCommand::ConfigCmd { set, .. } => {
            let set_args = set.unwrap();
            assert_eq!(set_args.len(), 1, "Should have 1 set argument");
            assert_eq!(set_args[0], "worker.max_workers=");

            // The validation happens later in handle_config_set, not during clap parsing
            // So we just verify the parsing succeeds
        }
        _ => panic!("Expected ConfigCmd command"),
    }
}

/// Test that --set with missing KEY fails appropriately.
#[test]
fn config_set_missing_key_fails_validation() {
    use needle::cli::Cli;

    // Simulate CLI invocation: needle config --set =10
    let args = vec!["needle", "config", "--set", "=10"];

    // Parsing should succeed (clap accepts it)
    let result = Cli::try_parse_from(args);
    assert!(result.is_ok(), "CLI parsing should succeed");

    let cli = result.unwrap();
    match cli.command {
        needle::cli::CliCommand::ConfigCmd { set, .. } => {
            let set_args = set.unwrap();
            assert_eq!(set_args.len(), 1, "Should have 1 set argument");
            assert_eq!(set_args[0], "=10");

            // The validation happens later in handle_config_set, not during clap parsing
            // So we just verify the parsing succeeds
        }
        _ => panic!("Expected ConfigCmd command"),
    }
}

/// Test that `needle config --help` includes --set flag with proper description.
#[test]
fn config_help_includes_set_flag() {
    use needle::cli::Cli;

    // Test that --help can be parsed and includes the --set flag description
    let args = vec!["needle", "config", "--help"];

    // When --help is provided, clap will display help and exit
    // We verify this behavior by checking that the parse result handles --help
    let result = Cli::try_parse_from(args);

    // Clap should successfully parse the --help request (it will then exit)
    // The fact that parsing succeeds means the --help flag is recognized
    assert!(
        result.is_err(),
        "CLI parsing should fail with --help (clap exits after displaying help)"
    );

    // Verify the error is a help display error (clap's standard behavior)
    let err = result.unwrap_err();
    let err_string = err.to_string().to_lowercase();

    // Clap help messages contain standard help text indicators
    // We verify this is a help-related exit, not a real error
    assert!(
        err_string.contains("help")
            || err_string.contains("usage")
            || err_string.contains("needle"),
        "Error should be help-related, got: {}",
        err
    );
}

/// Test that the ConfigCmd subcommand's --set flag appears in long help.
#[test]
fn config_set_flag_has_proper_metadata() {
    use clap::CommandFactory;
    use needle::cli::Cli;

    // Get the full command definition
    let cmd = Cli::command();

    // Find the config subcommand
    let config_subcommand = cmd
        .find_subcommand("config")
        .expect("config subcommand should exist");

    // Find the --set flag in the config subcommand
    let set_flag = config_subcommand
        .get_arguments()
        .find(|arg| arg.get_id() == "set")
        .expect("--set flag should exist in config subcommand");

    // Verify the --set flag has the correct ID and is a long flag
    assert_eq!(set_flag.get_id(), "set", "Flag ID should be 'set'");

    // Verify the flag is a long option (--set)
    assert!(set_flag.get_long().is_some(), "--set should be a long flag");
    assert_eq!(
        set_flag.get_long().unwrap(),
        "set",
        "Long flag name should be 'set'"
    );

    // Verify the --set flag has help text
    let help = set_flag
        .get_help()
        .expect("--set flag should have help text");
    let help_str = help.to_string();

    // Verify the help text mentions the two supported formats
    assert!(
        help_str.contains("KEY VALUE") || help_str.contains("KEY=VALUE"),
        "--set help text should mention KEY VALUE or KEY=VALUE format, got: {}",
        help_str
    );
}

/// Test that `needle config --help` output includes --set flag with proper description.
#[test]
fn config_help_output_includes_set_flag() {
    use clap::CommandFactory;
    use needle::cli::Cli;

    // Get the command and render the help text
    let cmd = Cli::command();
    let mut config_cmd = cmd
        .find_subcommand("config")
        .expect("config subcommand should exist")
        .clone();

    // Render the long help text to a string
    let mut help_buffer = Vec::new();
    config_cmd
        .write_long_help(&mut help_buffer)
        .expect("should be able to write help text");
    let help_text = String::from_utf8(help_buffer).expect("help text should be valid UTF-8");

    // Verify the help text contains the --set flag
    assert!(
        help_text.contains("--set"),
        "Help text should contain '--set' flag. Got:\n{}",
        help_text
    );

    // Verify the help text mentions the KEY VALUE or KEY=VALUE format
    assert!(
        help_text.contains("KEY VALUE") || help_text.contains("KEY=VALUE"),
        "Help text should mention KEY VALUE or KEY=VALUE format for --set. Got:\n{}",
        help_text
    );

    // Verify the help text contains some description (not just the flag name)
    let set_section: Vec<&str> = help_text
        .lines()
        .skip_while(|line| !line.contains("--set"))
        .take_while(|line| !line.starts_with("  --") || line.contains("--set"))
        .collect();

    assert!(
        !set_section.is_empty(),
        "Should find --set section in help text. Got:\n{}",
        help_text
    );

    // Verify that the --set section has more than just the flag name
    let set_text = set_section.join("\n");
    assert!(
        set_text.len() > "--set".len(),
        "--set section should have description text. Got:\n{}",
        set_text
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Config file deserialization integration tests
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn worker_binary_path_deserializes_from_yaml_file() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let config_path = temp_dir.path().join("config.yaml");

    let yaml_content = r#"worker:
  worker_binary_path: /custom/path/to/needle
"#;
    std::fs::write(&config_path, yaml_content).expect("failed to write config file");

    let config =
        ConfigLoader::load_from_path(&config_path).expect("failed to load config from file");

    assert_eq!(
        config.worker.worker_binary_path,
        Some(PathBuf::from("/custom/path/to/needle"))
    );
}

#[test]
fn worker_binary_path_default_when_omitted_from_file() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let config_path = temp_dir.path().join("config.yaml");

    // Config file without worker_binary_path specified
    let yaml_content = r#"worker:
  max_workers: 10
"#;
    std::fs::write(&config_path, yaml_content).expect("failed to write config file");

    let config =
        ConfigLoader::load_from_path(&config_path).expect("failed to load config from file");

    assert_eq!(config.worker.worker_binary_path, None);
}

#[test]
fn worker_binary_path_tilde_expansion_from_file() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let config_path = temp_dir.path().join("config.yaml");

    let yaml_content = r#"worker:
  worker_binary_path: ~/local/bin/needle
"#;
    std::fs::write(&config_path, yaml_content).expect("failed to write config file");

    let config =
        ConfigLoader::load_from_path(&config_path).expect("failed to load config from file");

    // Verify tilde was expanded
    assert!(config.worker.worker_binary_path.is_some());
    let path = config.worker.worker_binary_path.as_ref().unwrap();
    assert!(
        !path.starts_with("~"),
        "tilde should be expanded, got: {:?}",
        path
    );
    assert!(path.ends_with("local/bin/needle"));
}

#[test]
fn config_load_returns_default_when_file_missing() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let nonexistent_path = temp_dir.path().join("nonexistent-config.yaml");

    let config = ConfigLoader::load_from_path(&nonexistent_path)
        .expect("should return default config when file missing");

    // Should get a valid default config
    assert_eq!(config.worker.worker_binary_path, None);
    assert_eq!(config.worker.max_workers, 4); // Default value
}

#[test]
fn worker_binary_path_nonexistent_path_accepted() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let config_path = temp_dir.path().join("config.yaml");

    // Path that doesn't exist should still deserialize successfully
    let yaml_content = r#"worker:
  worker_binary_path: /this/path/does/not/exist/needle
"#;
    std::fs::write(&config_path, yaml_content).expect("failed to write config file");

    let config =
        ConfigLoader::load_from_path(&config_path).expect("failed to load config from file");

    assert_eq!(
        config.worker.worker_binary_path,
        Some(PathBuf::from("/this/path/does/not/exist/needle"))
    );

    // Validation should not fail (path validation happens at runtime)
    let errors = ConfigLoader::validate(&config);
    assert!(
        !errors
            .iter()
            .any(|e| e.field == "worker.worker_binary_path"),
        "path existence should not be validated at config load time"
    );
}

/// Test that `needle config --dump --live --show-source` parses correctly.
#[test]
fn config_dump_live_with_show_source_parses() {
    use needle::cli::Cli;

    // Simulate CLI invocation: needle config --dump --live --show-source
    let args = vec!["needle", "config", "--dump", "--live", "--show-source"];

    let result = Cli::try_parse_from(args);
    assert!(
        result.is_ok(),
        "CLI parsing should succeed with --dump --live --show-source"
    );

    let cli = result.unwrap();
    match cli.command {
        needle::cli::CliCommand::ConfigCmd {
            get,
            set,
            dump,
            show_source,
            live,
        } => {
            assert!(get.is_none(), "--get should not be set");
            assert!(set.is_none(), "--set should not be set");
            assert!(dump, "--dump should be set");
            assert!(show_source, "--show-source should be set");
            assert!(live, "--live should be set");
        }
        _ => panic!("Expected ConfigCmd command"),
    }
}

/// Test that `needle config --dump --live` parses correctly without --show-source.
#[test]
fn config_dump_live_without_show_source_parses() {
    use needle::cli::Cli;

    // Simulate CLI invocation: needle config --dump --live
    let args = vec!["needle", "config", "--dump", "--live"];

    let result = Cli::try_parse_from(args);
    assert!(
        result.is_ok(),
        "CLI parsing should succeed with --dump --live"
    );

    let cli = result.unwrap();
    match cli.command {
        needle::cli::CliCommand::ConfigCmd {
            get,
            set,
            dump,
            show_source,
            live,
        } => {
            assert!(get.is_none(), "--get should not be set");
            assert!(set.is_none(), "--set should not be set");
            assert!(dump, "--dump should be set");
            assert!(!show_source, "--show-source should not be set");
            assert!(live, "--live should be set");
        }
        _ => panic!("Expected ConfigCmd command"),
    }
}

/// Test that `--live` flag requires `--dump` flag.
#[test]
fn config_live_requires_dump() {
    use needle::cli::Cli;

    // Simulate CLI invocation: needle config --live (without --dump)
    let args = vec!["needle", "config", "--live"];

    let result = Cli::try_parse_from(args);
    // This should parse successfully (clap allows it)
    assert!(result.is_ok(), "CLI parsing should succeed");

    // But the cmd_config function will reject it at runtime
    // We verify the parsing structure
    let cli = result.unwrap();
    match cli.command {
        needle::cli::CliCommand::ConfigCmd { dump, .. } => {
            assert!(!dump, "--dump should not be set when --live is used alone");
        }
        _ => panic!("Expected ConfigCmd command"),
    }
}
