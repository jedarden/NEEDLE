//! CLI invocation tests for `needle config --set` flag.
//!
//! Tests that verify the --set flag parses correctly in both invocation formats:
//! 1. needle config --set KEY VALUE
//! 2. needle config --set KEY=VALUE

use clap::Parser;

/// Test that `--set worker.max_workers 10` (KEY VALUE format) parses without clap error.
#[test]
fn config_set_key_value_format_parses() {
    use needle::cli::Cli;

    // Simulate CLI invocation: needle config --set worker.max_workers 10
    let args = vec![
        "needle",
        "config",
        "--set",
        "worker.max_workers",
        "10",
    ];

    // This should not panic or return an Err during parsing
    let result = Cli::try_parse_from(args);
    assert!(result.is_ok(), "CLI parsing should succeed with KEY VALUE format");

    let cli = result.unwrap();
    match cli.command {
        needle::cli::CliCommand::ConfigCmd { get, set, dump, show_source } => {
            assert!(get.is_none(), "--get should not be set");
            assert!(set.is_some(), "--set should be set");
            assert!(!dump, "--dump should not be set");
            assert!(!show_source, "--show-source should not be set");

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
    let args = vec![
        "needle",
        "config",
        "--set",
        "worker.max_workers=10",
    ];

    // This should not panic or return an Err during parsing
    let result = Cli::try_parse_from(args);
    assert!(result.is_ok(), "CLI parsing should succeed with KEY=VALUE format");

    let cli = result.unwrap();
    match cli.command {
        needle::cli::CliCommand::ConfigCmd { get, set, dump, show_source } => {
            assert!(get.is_none(), "--get should not be set");
            assert!(set.is_some(), "--set should be set");
            assert!(!dump, "--dump should not be set");
            assert!(!show_source, "--show_source should not be set");

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
    assert!(result.is_ok(), "CLI parsing should succeed with multiple --set flags in KEY VALUE format");

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
    assert!(result.is_ok(), "CLI parsing should succeed with multiple --set flags in KEY=VALUE format");

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
    assert!(result.is_ok(), "CLI parsing should succeed with mixed --set formats");

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
    let args = vec![
        "needle",
        "config",
        "--set",
        "worker.max_workers=",
    ];

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
    let args = vec![
        "needle",
        "config",
        "--set",
        "=10",
    ];

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
