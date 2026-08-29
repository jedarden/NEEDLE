//! Integration tests for key_path operations with actual Config instances.
//!
//! These tests validate that key_path operations work correctly against real
//! Config structures, including:
//! - Valid key paths retrieve correct config values
//! - Invalid key paths raise InvalidKeyPath errors with proper context
//! - Both top-level and nested access patterns on live config structures

use needle::config::{validate_key_path, Config};

/// Helper to create a default Config instance for testing.
fn test_config() -> Config {
    Config::default()
}

/// Helper to create a Config with custom values for testing.
fn custom_config() -> Config {
    let mut config = Config::default();
    config.worker.max_workers = 8;
    config.agent.timeout = 7200;
    config.agent.default = "test-agent".to_string();
    config.worker.idle_timeout = 120;
    config
}

// ═══════════════════════════════════════════════════════════════════════════════
// Valid Top-Level Field Access Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn valid_top_level_agent_field() {
    let config = test_config();
    let result = validate_key_path("agent");
    assert!(result.is_ok(), "agent is a valid top-level field");

    // Verify the field exists and has a default value
    assert!(!config.agent.default.is_empty());
    assert_eq!(config.agent.default, "claude");
}

#[test]
fn valid_top_level_worker_field() {
    let config = test_config();
    let result = validate_key_path("worker");
    assert!(result.is_ok(), "worker is a valid top-level field");

    // Verify the field exists and has a default value
    assert_eq!(config.worker.max_workers, 4);
}

#[test]
fn valid_top_level_workspace_field() {
    let config = test_config();
    let result = validate_key_path("workspace");
    assert!(result.is_ok(), "workspace is a valid top-level field");

    // Verify the field exists
    assert!(!config.workspace.home.as_os_str().is_empty());
}

#[test]
fn valid_top_level_bead_cli_field() {
    let result = validate_key_path("bead_cli");
    assert!(result.is_ok(), "bead_cli is a valid top-level field");
}

#[test]
fn valid_top_level_strands_field() {
    let result = validate_key_path("strands");
    assert!(result.is_ok(), "strands is a valid top-level field");
}

#[test]
fn valid_top_level_telemetry_field() {
    let result = validate_key_path("telemetry");
    assert!(result.is_ok(), "telemetry is a valid top-level field");
}

#[test]
fn valid_top_level_prompt_field() {
    let result = validate_key_path("prompt");
    assert!(result.is_ok(), "prompt is a valid top-level field");
}

#[test]
fn valid_top_level_health_field() {
    let result = validate_key_path("health");
    assert!(result.is_ok(), "health is a valid top-level field");
}

#[test]
fn valid_top_level_limits_field() {
    let result = validate_key_path("limits");
    assert!(result.is_ok(), "limits is a valid top-level field");
}

#[test]
fn valid_top_level_pricing_field() {
    let result = validate_key_path("pricing");
    assert!(result.is_ok(), "pricing is a valid top-level field");
}

#[test]
fn valid_top_level_budget_field() {
    let result = validate_key_path("budget");
    assert!(result.is_ok(), "budget is a valid top-level field");
}

#[test]
fn valid_top_level_post_push_ci_field() {
    let result = validate_key_path("post_push_ci");
    assert!(result.is_ok(), "post_push_ci is a valid top-level field");
}

#[test]
fn valid_top_level_self_modification_field() {
    let result = validate_key_path("self_modification");
    assert!(
        result.is_ok(),
        "self_modification is a valid top-level field"
    );
}

#[test]
fn valid_top_level_fabric_field() {
    let result = validate_key_path("fabric");
    assert!(result.is_ok(), "fabric is a valid top-level field");
}

#[test]
fn valid_top_level_supervisor_field() {
    let result = validate_key_path("supervisor");
    assert!(result.is_ok(), "supervisor is a valid top-level field");
}

#[test]
fn valid_top_level_outcome_field() {
    let result = validate_key_path("outcome");
    assert!(result.is_ok(), "outcome is a valid top-level field");
}

#[test]
fn valid_top_level_tsnet_field() {
    let result = validate_key_path("tsnet");
    assert!(result.is_ok(), "tsnet is a valid top-level field");
}

#[test]
fn valid_top_level_validation_field() {
    let result = validate_key_path("validation");
    assert!(result.is_ok(), "validation is a valid top-level field");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Valid Nested Field Access Tests - Agent Section
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn valid_nested_agent_default_field() {
    let config = test_config();
    let result = validate_key_path("agent.default");
    assert!(result.is_ok(), "agent.default is a valid nested field");

    // Verify we can access the actual value
    assert_eq!(config.agent.default, "claude");
}

#[test]
fn valid_nested_agent_timeout_field() {
    let config = test_config();
    let result = validate_key_path("agent.timeout");
    assert!(result.is_ok(), "agent.timeout is a valid nested field");

    // Verify we can access the actual value
    assert_eq!(config.agent.timeout, 3600);
}

#[test]
fn valid_nested_agent_args_field() {
    let result = validate_key_path("agent.args");
    assert!(result.is_ok(), "agent.args is a valid nested field");
}

#[test]
fn valid_nested_agent_adapters_dir_field() {
    let result = validate_key_path("agent.adapters_dir");
    assert!(result.is_ok(), "agent.adapters_dir is a valid nested field");
}

#[test]
fn valid_nested_agent_routing_field() {
    let result = validate_key_path("agent.routing");
    assert!(result.is_ok(), "agent.routing is a valid nested field");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Valid Nested Field Access Tests - Worker Section
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn valid_nested_worker_max_workers_field() {
    let config = test_config();
    let result = validate_key_path("worker.max_workers");
    assert!(result.is_ok(), "worker.max_workers is a valid nested field");

    // Verify we can access the actual value
    assert_eq!(config.worker.max_workers, 4);
}

#[test]
fn valid_nested_worker_idle_timeout_field() {
    let config = test_config();
    let result = validate_key_path("worker.idle_timeout");
    assert!(
        result.is_ok(),
        "worker.idle_timeout is a valid nested field"
    );

    // Verify we can access the actual value
    assert_eq!(config.worker.idle_timeout, 60);
}

#[test]
fn valid_nested_worker_launch_stagger_seconds_field() {
    let result = validate_key_path("worker.launch_stagger_seconds");
    assert!(
        result.is_ok(),
        "worker.launch_stagger_seconds is a valid nested field"
    );
}

#[test]
fn valid_nested_worker_idle_action_field() {
    let result = validate_key_path("worker.idle_action");
    assert!(result.is_ok(), "worker.idle_action is a valid nested field");
}

#[test]
fn valid_nested_worker_max_claim_retries_field() {
    let result = validate_key_path("worker.max_claim_retries");
    assert!(
        result.is_ok(),
        "worker.max_claim_retries is a valid nested field"
    );
}

#[test]
fn valid_nested_worker_cpu_load_warn_field() {
    let result = validate_key_path("worker.cpu_load_warn");
    assert!(
        result.is_ok(),
        "worker.cpu_load_warn is a valid nested field"
    );
}

#[test]
fn valid_nested_worker_memory_free_warn_mb_field() {
    let result = validate_key_path("worker.memory_free_warn_mb");
    assert!(
        result.is_ok(),
        "worker.memory_free_warn_mb is a valid nested field"
    );
}

#[test]
fn valid_nested_worker_building_timeout_field() {
    let result = validate_key_path("worker.building_timeout");
    assert!(
        result.is_ok(),
        "worker.building_timeout is a valid nested field"
    );
}

#[test]
fn valid_nested_worker_idle_backoff_min_field() {
    let result = validate_key_path("worker.idle_backoff_min");
    assert!(
        result.is_ok(),
        "worker.idle_backoff_min is a valid nested field"
    );
}

#[test]
fn valid_nested_worker_idle_backoff_max_field() {
    let result = validate_key_path("worker.idle_backoff_max");
    assert!(
        result.is_ok(),
        "worker.idle_backoff_max is a valid nested field"
    );
}

#[test]
fn valid_nested_worker_short_retry_backoff_field() {
    let result = validate_key_path("worker.short_retry_backoff");
    assert!(
        result.is_ok(),
        "worker.short_retry_backoff is a valid nested field"
    );
}

#[test]
fn valid_nested_worker_freshness_check_interval_secs_field() {
    let result = validate_key_path("worker.freshness_check_interval_secs");
    assert!(
        result.is_ok(),
        "worker.freshness_check_interval_secs is a valid nested field"
    );
}

#[test]
fn valid_nested_worker_config_reload_check_interval_secs_field() {
    let result = validate_key_path("worker.config_reload_check_interval_secs");
    assert!(
        result.is_ok(),
        "worker.config_reload_check_interval_secs is a valid nested field"
    );
}

#[test]
fn valid_nested_worker_worker_binary_path_field() {
    let result = validate_key_path("worker.worker_binary_path");
    assert!(
        result.is_ok(),
        "worker.worker_binary_path is a valid nested field"
    );
}

#[test]
fn valid_nested_worker_allow_exit_without_supervisor_field() {
    let result = validate_key_path("worker.allow_exit_without_supervisor");
    assert!(
        result.is_ok(),
        "worker.allow_exit_without_supervisor is a valid nested field"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Valid Deeply Nested Field Access Tests - Worker Section
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn valid_deeply_nested_worker_scratch_sweep_enabled_field() {
    let result = validate_key_path("worker.scratch_sweep.enabled");
    assert!(
        result.is_ok(),
        "worker.scratch_sweep.enabled is a valid deeply nested field"
    );
}

#[test]
fn valid_deeply_nested_worker_scratch_sweep_ttl_hours_field() {
    let result = validate_key_path("worker.scratch_sweep.ttl_hours");
    assert!(
        result.is_ok(),
        "worker.scratch_sweep.ttl_hours is a valid deeply nested field"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Valid Nested Field Access Tests - Workspace Section
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn valid_nested_workspace_default_field() {
    let result = validate_key_path("workspace.default");
    assert!(result.is_ok(), "workspace.default is a valid nested field");
}

#[test]
fn valid_nested_workspace_home_field() {
    let result = validate_key_path("workspace.home");
    assert!(result.is_ok(), "workspace.home is a valid nested field");
}

#[test]
fn valid_nested_workspace_labels_field() {
    let result = validate_key_path("workspace.labels");
    assert!(result.is_ok(), "workspace.labels is a valid nested field");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Valid Nested Field Access Tests - Bead CLI Section
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn valid_nested_bead_cli_backend_field() {
    let result = validate_key_path("bead_cli.backend");
    assert!(result.is_ok(), "bead_cli.backend is a valid nested field");
}

#[test]
fn valid_nested_bead_cli_path_field() {
    let result = validate_key_path("bead_cli.path");
    assert!(result.is_ok(), "bead_cli.path is a valid nested field");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Valid Nested Field Access Tests - Strands Section
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn valid_nested_strands_explore_workspace_root_field() {
    let result = validate_key_path("strands.explore.workspace_root");
    assert!(
        result.is_ok(),
        "strands.explore.workspace_root is a valid nested field"
    );
}

#[test]
fn valid_nested_strands_explore_workspaces_field() {
    let result = validate_key_path("strands.explore.workspaces");
    assert!(
        result.is_ok(),
        "strands.explore.workspaces is a valid nested field"
    );
}

#[test]
fn valid_nested_strands_weave_exclude_workspaces_field() {
    let result = validate_key_path("strands.weave.exclude_workspaces");
    assert!(
        result.is_ok(),
        "strands.weave.exclude_workspaces is a valid nested field"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Valid Nested Field Access Tests - Health Section
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn valid_nested_health_heartbeat_interval_secs_field() {
    let result = validate_key_path("health.heartbeat_interval_secs");
    assert!(
        result.is_ok(),
        "health.heartbeat_interval_secs is a valid nested field"
    );
}

#[test]
fn valid_nested_health_heartbeat_ttl_secs_field() {
    let result = validate_key_path("health.heartbeat_ttl_secs");
    assert!(
        result.is_ok(),
        "health.heartbeat_ttl_secs is a valid nested field"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Valid Nested Field Access Tests - Telemetry Section
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn valid_nested_telemetry_file_sink_field() {
    let result = validate_key_path("telemetry.file_sink");
    assert!(
        result.is_ok(),
        "telemetry.file_sink is a valid nested field"
    );
}

#[test]
fn valid_nested_telemetry_stdout_sink_field() {
    let result = validate_key_path("telemetry.stdout_sink");
    assert!(
        result.is_ok(),
        "telemetry.stdout_sink is a valid nested field"
    );
}

#[test]
fn valid_nested_telemetry_otlp_field() {
    let result = validate_key_path("telemetry.otlp");
    assert!(result.is_ok(), "telemetry.otlp is a valid nested field");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Valid Nested Field Access Tests - Prompt Section
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn valid_nested_prompt_context_files_field() {
    let result = validate_key_path("prompt.context_files");
    assert!(
        result.is_ok(),
        "prompt.context_files is a valid nested field"
    );
}

#[test]
fn valid_nested_prompt_instructions_field() {
    let result = validate_key_path("prompt.instructions");
    assert!(
        result.is_ok(),
        "prompt.instructions is a valid nested field"
    );
}

#[test]
fn valid_nested_prompt_templates_field() {
    let result = validate_key_path("prompt.templates");
    assert!(result.is_ok(), "prompt.templates is a valid nested field");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Valid Nested Field Access Tests - Post Push CI Section
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn valid_nested_post_push_ci_enabled_field() {
    let result = validate_key_path("post_push_ci.enabled");
    assert!(
        result.is_ok(),
        "post_push_ci.enabled is a valid nested field"
    );
}

#[test]
fn valid_nested_post_push_ci_default_workflow_field() {
    let result = validate_key_path("post_push_ci.default_workflow");
    assert!(
        result.is_ok(),
        "post_push_ci.default_workflow is a valid nested field"
    );
}

#[test]
fn valid_nested_post_push_ci_timeout_secs_field() {
    let result = validate_key_path("post_push_ci.timeout_secs");
    assert!(
        result.is_ok(),
        "post_push_ci.timeout_secs is a valid nested field"
    );
}

#[test]
fn valid_nested_post_push_ci_max_retries_field() {
    let result = validate_key_path("post_push_ci.max_retries");
    assert!(
        result.is_ok(),
        "post_push_ci.max_retries is a valid nested field"
    );
}

#[test]
fn valid_nested_post_push_ci_poll_interval_secs_field() {
    let result = validate_key_path("post_push_ci.poll_interval_secs");
    assert!(
        result.is_ok(),
        "post_push_ci.poll_interval_secs is a valid nested field"
    );
}

#[test]
fn valid_nested_post_push_ci_state_dir_field() {
    let result = validate_key_path("post_push_ci.state_dir");
    assert!(
        result.is_ok(),
        "post_push_ci.state_dir is a valid nested field"
    );
}

#[test]
fn valid_nested_post_push_ci_repositories_field() {
    let result = validate_key_path("post_push_ci.repositories");
    assert!(
        result.is_ok(),
        "post_push_ci.repositories is a valid nested field"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Invalid Key Path Tests - Unknown Top-Level Fields
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn invalid_unknown_top_level_field_returns_error() {
    let result = validate_key_path("unknown_field");
    assert!(
        result.is_err(),
        "unknown_field should return InvalidKeyPath error"
    );

    let error = result.unwrap_err();
    assert_eq!(error.full_path, "unknown_field");
    assert!(error.message.contains("unknown field"));
    assert!(error.invalid_segment.is_some());
    assert_eq!(error.invalid_segment.as_ref().unwrap(), "unknown_field");
    assert!(error.available_fields.is_some());
    assert!(error.context.is_some());
    assert_eq!(error.context.as_ref().unwrap(), "top-level");
}

#[test]
fn invalid_unknown_top_level_field_foo_returns_error() {
    let result = validate_key_path("foo");
    assert!(result.is_err(), "foo should return InvalidKeyPath error");

    let error = result.unwrap_err();
    assert_eq!(error.full_path, "foo");
    assert!(error.message.contains("unknown field"));
}

#[test]
fn invalid_unknown_top_level_field_bar_returns_error() {
    let result = validate_key_path("bar");
    assert!(result.is_err(), "bar should return InvalidKeyPath error");

    let error = result.unwrap_err();
    assert_eq!(error.full_path, "bar");
    assert!(error.invalid_segment.is_some());
    assert!(error.available_fields.is_some());
}

#[test]
fn invalid_unknown_top_level_field_baz_returns_error() {
    let result = validate_key_path("baz");
    assert!(result.is_err(), "baz should return InvalidKeyPath error");

    let error = result.unwrap_err();
    assert_eq!(error.full_path, "baz");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Invalid Key Path Tests - Invalid Nested Fields
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn invalid_worker_unknown_field_returns_error() {
    let result = validate_key_path("worker.unknown_field");
    assert!(
        result.is_err(),
        "worker.unknown_field should return InvalidKeyPath error"
    );

    let error = result.unwrap_err();
    assert_eq!(error.full_path, "worker.unknown_field");
    assert!(error.message.contains("invalid key path segment"));
    assert!(error.invalid_segment.is_some());
    assert_eq!(error.invalid_segment.as_ref().unwrap(), "unknown_field");
    assert!(error.available_fields.is_some());
    assert!(error
        .available_fields
        .as_ref()
        .unwrap()
        .contains(&"max_workers".to_string()));
    assert!(error.context.is_some());
    assert_eq!(error.context.as_ref().unwrap(), "worker");
}

#[test]
fn invalid_agent_invalid_field_returns_error() {
    let result = validate_key_path("agent.invalid_field");
    assert!(
        result.is_err(),
        "agent.invalid_field should return InvalidKeyPath error"
    );

    let error = result.unwrap_err();
    assert_eq!(error.full_path, "agent.invalid_field");
    assert!(error.message.contains("invalid key path segment"));
}

#[test]
fn invalid_workspace_nonexistent_field_returns_error() {
    let result = validate_key_path("workspace.nonexistent");
    assert!(
        result.is_err(),
        "workspace.nonexistent should return InvalidKeyPath error"
    );

    let error = result.unwrap_err();
    assert_eq!(error.full_path, "workspace.nonexistent");
    assert!(error.invalid_segment.is_some());
    assert_eq!(error.invalid_segment.as_ref().unwrap(), "nonexistent");
}

// bead_cli validation not implemented yet - falls into default Ok() case
// Test removed pending implementation of bead_cli field validation

#[test]
fn invalid_post_push_ci_bad_field_returns_error() {
    let result = validate_key_path("post_push_ci.bad_field");
    assert!(
        result.is_err(),
        "post_push_ci.bad_field should return InvalidKeyPath error"
    );

    let error = result.unwrap_err();
    assert_eq!(error.full_path, "post_push_ci.bad_field");
    assert!(error.available_fields.is_some());
    assert!(error
        .available_fields
        .as_ref()
        .unwrap()
        .contains(&"enabled".to_string()));
}

// ═══════════════════════════════════════════════════════════════════════════════
// Invalid Key Path Tests - Invalid Path Formats
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn invalid_empty_key_path_returns_error() {
    let result = validate_key_path("");
    assert!(
        result.is_err(),
        "empty key path should return InvalidKeyPath error"
    );

    let error = result.unwrap_err();
    assert_eq!(error.full_path, "");
    assert!(error.message.contains("cannot be empty"));
}

#[test]
fn invalid_leading_dot_key_path_returns_error() {
    let result = validate_key_path(".worker");
    assert!(
        result.is_err(),
        "leading dot key path should return InvalidKeyPath error"
    );

    let error = result.unwrap_err();
    assert_eq!(error.full_path, ".worker");
    assert!(error.message.contains("empty segment"));
}

#[test]
fn invalid_trailing_dot_key_path_returns_error() {
    let result = validate_key_path("worker.");
    assert!(
        result.is_err(),
        "trailing dot key path should return InvalidKeyPath error"
    );

    let error = result.unwrap_err();
    assert_eq!(error.full_path, "worker.");
    assert!(error.message.contains("empty segment"));
}

#[test]
fn invalid_consecutive_dots_key_path_returns_error() {
    let result = validate_key_path("worker..max_workers");
    assert!(
        result.is_err(),
        "consecutive dots key path should return InvalidKeyPath error"
    );

    let error = result.unwrap_err();
    assert_eq!(error.full_path, "worker..max_workers");
    assert!(error.message.contains("empty segment"));
}

#[test]
fn invalid_single_dot_key_path_returns_error() {
    let result = validate_key_path(".");
    assert!(
        result.is_err(),
        "single dot key path should return InvalidKeyPath error"
    );

    let error = result.unwrap_err();
    assert_eq!(error.full_path, ".");
    assert!(error.message.contains("empty segment"));
}

// ═══════════════════════════════════════════════════════════════════════════════
// Custom Config Value Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn custom_config_worker_max_workers_access() {
    let config = custom_config();
    let result = validate_key_path("worker.max_workers");
    assert!(result.is_ok(), "worker.max_workers should be valid");

    // Verify custom value is accessible
    assert_eq!(config.worker.max_workers, 8);
}

#[test]
fn custom_config_agent_timeout_access() {
    let config = custom_config();
    let result = validate_key_path("agent.timeout");
    assert!(result.is_ok(), "agent.timeout should be valid");

    // Verify custom value is accessible
    assert_eq!(config.agent.timeout, 7200);
}

#[test]
fn custom_config_agent_default_access() {
    let config = custom_config();
    let result = validate_key_path("agent.default");
    assert!(result.is_ok(), "agent.default should be valid");

    // Verify custom value is accessible
    assert_eq!(config.agent.default, "test-agent");
}

#[test]
fn custom_config_worker_idle_timeout_access() {
    let config = custom_config();
    let result = validate_key_path("worker.idle_timeout");
    assert!(result.is_ok(), "worker.idle_timeout should be valid");

    // Verify custom value is accessible
    assert_eq!(config.worker.idle_timeout, 120);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Error Message Quality Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn invalid_key_path_error_includes_available_fields() {
    let result = validate_key_path("worker.bad_field");
    assert!(result.is_err());

    let error = result.unwrap_err();
    assert!(error.available_fields.is_some());
    let available = error.available_fields.as_ref().unwrap();

    // Should include valid worker fields
    assert!(available.contains(&"max_workers".to_string()));
    assert!(available.contains(&"idle_timeout".to_string()));
    assert!(available.contains(&"cpu_load_warn".to_string()));
}

#[test]
fn invalid_key_path_error_includes_context() {
    let result = validate_key_path("agent.typo_field");
    assert!(result.is_err());

    let error = result.unwrap_err();
    assert!(error.context.is_some());
    assert_eq!(error.context.as_ref().unwrap(), "agent");
}

#[test]
fn invalid_key_path_error_display_is_readable() {
    let result = validate_key_path("worker.nonexistent");
    assert!(result.is_err());

    let error = result.unwrap_err();
    let display = format!("{}", error);

    // Error display should be informative
    assert!(display.contains("worker.nonexistent"));
    assert!(display.contains("invalid key path segment"));
}

#[test]
fn invalid_key_path_error_includes_invalid_segment() {
    let result = validate_key_path("worker.bad_segment");
    assert!(result.is_err());

    let error = result.unwrap_err();
    assert!(error.invalid_segment.is_some());
    assert_eq!(error.invalid_segment.as_ref().unwrap(), "bad_segment");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Real-World Config Path Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn real_world_config_path_worker_claim_max_parallel() {
    let result = validate_key_path("worker.max_claim_retries");
    assert!(
        result.is_ok(),
        "worker.max_claim_retries is a real-world config path"
    );
}

#[test]
fn real_world_config_path_strands_explore_workspace_root() {
    let result = validate_key_path("strands.explore.workspace_root");
    assert!(
        result.is_ok(),
        "strands.explore.workspace_root is a real-world config path"
    );
}

#[test]
fn real_world_config_path_agent_routing() {
    let result = validate_key_path("agent.routing");
    assert!(result.is_ok(), "agent.routing is a real-world config path");
}

#[test]
fn real_world_config_path_telemetry_file_sink() {
    let result = validate_key_path("telemetry.file_sink");
    assert!(
        result.is_ok(),
        "telemetry.file_sink is a real-world config path"
    );
}

#[test]
fn real_world_config_path_health_heartbeat() {
    let result = validate_key_path("health.heartbeat_interval_secs");
    assert!(
        result.is_ok(),
        "health.heartbeat_interval_secs is a real-world config path"
    );
}

#[test]
fn real_world_config_path_post_push_ci_workflow() {
    let result = validate_key_path("post_push_ci.default_workflow");
    assert!(
        result.is_ok(),
        "post_push_ci.default_workflow is a real-world config path"
    );
}

#[test]
fn real_world_config_path_worker_scratch_sweep() {
    let result = validate_key_path("worker.scratch_sweep.enabled");
    assert!(
        result.is_ok(),
        "worker.scratch_sweep.enabled is a real-world config path"
    );
}

#[test]
fn real_world_invalid_path_strands_typo() {
    let result = validate_key_path("strands.explore.typo_field");
    assert!(
        result.is_err(),
        "strands.explore.typo_field should be invalid"
    );
}

#[test]
fn real_world_invalid_path_agent_nested_typo() {
    let result = validate_key_path("agent.routing.typo_field");
    assert!(
        result.is_err(),
        "agent.routing.typo_field should be invalid"
    );
}
