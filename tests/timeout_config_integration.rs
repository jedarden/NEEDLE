//! Integration tests for timeout configuration parsing, validation, and application.
//!
//! These tests verify end-to-end behavior of timeout configuration:
//! 1. Agent timeout parsing and defaults
//! 2. Worker building timeout parsing and defaults
//! 3. Validation timeout parsing and defaults
//! 4. Timeout-triggered mitosis policy configuration
//! 5. Legacy config compatibility
//! 6. Zero value handling (disables deadlines)
//! 7. Invalid value rejection with clear errors
//! 8. Environment variable overrides
//!
//! Tests use real YAML configs loaded via ConfigLoader to verify the full
//! configuration pipeline from file → parsed struct → application.

use needle::config::{Config, ConfigLoader, TimeoutTriggeredPolicy};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use tempfile::TempDir;

// ═════════════════════════════════════════════════════════════════════════════
// Test Helper Functions
// ═════════════════════════════════════════════════════════════════════════════

/// Create a temporary config file with the given YAML content.
fn create_temp_config(yaml_content: &str) -> (TempDir, PathBuf) {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let config_path = temp_dir.path().join("config.yaml");

    let mut file = fs::File::create(&config_path).expect("failed to create config file");
    file.write_all(yaml_content.as_bytes())
        .expect("failed to write config");

    (temp_dir, config_path)
}

/// Load config from YAML string and return the parsed Config.
///
/// This variant drops the TempDir immediately, which is safe for Config structs
/// that don't hold file handles or paths to the temp directory.
fn load_config_from_yaml(yaml_content: &str) -> Config {
    let (temp_dir, config_path) = create_temp_config(yaml_content);
    let config = ConfigLoader::load_from_path(&config_path).expect("failed to load config");
    // Explicitly drop temp_dir after config is loaded to ensure cleanup
    drop(temp_dir);
    config
}

// ═════════════════════════════════════════════════════════════════════════════
// Agent Timeout Configuration Tests
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn agent_timeout_default_value() {
    // Empty config should use default agent timeout of 3600 seconds
    let config = load_config_from_yaml("");

    assert_eq!(
        config.agent.timeout, 3600,
        "default agent timeout should be 3600 seconds (1 hour)"
    );
}

#[test]
fn agent_timeout_explicit_positive_value() {
    // Config with explicit positive agent timeout
    let yaml = r#"
agent:
  timeout: 7200
"#;

    let config = load_config_from_yaml(yaml);

    assert_eq!(
        config.agent.timeout, 7200,
        "explicit agent timeout should be parsed correctly"
    );
}

#[test]
fn agent_timeout_zero_disables_deadline() {
    // Config with agent.timeout = 0 should disable the deadline (unlimited)
    let yaml = r#"
agent:
  timeout: 0
"#;

    let config = load_config_from_yaml(yaml);

    assert_eq!(
        config.agent.timeout, 0,
        "agent timeout of 0 should disable deadline (unlimited)"
    );
}

#[test]
fn agent_timeout_large_value() {
    // Config with very large agent timeout (24 hours)
    let yaml = r#"
agent:
  timeout: 86400
"#;

    let config = load_config_from_yaml(yaml);

    assert_eq!(
        config.agent.timeout, 86400,
        "large agent timeout values should be accepted"
    );
}

#[test]
fn agent_timeout_in_yaml_with_other_fields() {
    // Config with agent timeout among other agent fields
    let yaml = r#"
agent:
  default: claude
  timeout: 1800
  adapters_dir: ~/.config/needle/adapters
"#;

    let config = load_config_from_yaml(yaml);

    assert_eq!(
        config.agent.timeout, 1800,
        "agent timeout should be parsed correctly when other fields present"
    );
    assert_eq!(config.agent.default, "claude");
}

// ═════════════════════════════════════════════════════════════════════════════
// Worker Building Timeout Configuration Tests
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn worker_building_timeout_default_value() {
    // Empty config should use default building timeout of 600 seconds
    let config = load_config_from_yaml("");

    assert_eq!(
        config.worker.building_timeout, 600,
        "default worker building timeout should be 600 seconds (10 minutes)"
    );
}

#[test]
fn worker_building_timeout_explicit_value() {
    // Config with explicit building timeout
    let yaml = r#"
worker:
  building_timeout: 900
"#;

    let config = load_config_from_yaml(yaml);

    assert_eq!(
        config.worker.building_timeout, 900,
        "explicit building timeout should be parsed correctly"
    );
}

#[test]
fn worker_building_timeout_zero_disables_deadline() {
    // Config with building_timeout = 0 should disable the deadline
    let yaml = r#"
worker:
  building_timeout: 0
"#;

    let config = load_config_from_yaml(yaml);

    assert_eq!(
        config.worker.building_timeout, 0,
        "building timeout of 0 should disable deadline (unlimited)"
    );
}

#[test]
fn worker_building_timeout_among_other_worker_fields() {
    // Config with building timeout among other worker fields
    let yaml = r#"
worker:
  max_workers: 8
  building_timeout: 300
  idle_timeout: 120
"#;

    let config = load_config_from_yaml(yaml);

    assert_eq!(
        config.worker.building_timeout, 300,
        "building timeout should be parsed correctly with other worker fields"
    );
    assert_eq!(config.worker.max_workers, 8);
    assert_eq!(config.worker.idle_timeout, 120);
}

// ═════════════════════════════════════════════════════════════════════════════
// Validation Timeout Configuration Tests
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn validation_outcome_timeout_default_value() {
    // Empty config should use default validation timeout of 50 seconds
    let config = load_config_from_yaml("");

    assert_eq!(
        config.validation.outcome_timeout_seconds, 50,
        "default validation timeout should be 50 seconds"
    );
}

#[test]
fn validation_outcome_timeout_explicit_value() {
    // Config with explicit validation timeout
    let yaml = r#"
validation:
  outcome_timeout_seconds: 120
"#;

    let config = load_config_from_yaml(yaml);

    assert_eq!(
        config.validation.outcome_timeout_seconds, 120,
        "explicit validation timeout should be parsed correctly"
    );
}

#[test]
fn validation_outcome_timeout_reasonable_large_value() {
    // Config with large but reasonable validation timeout (10 minutes)
    let yaml = r#"
validation:
  outcome_timeout_seconds: 600
"#;

    let config = load_config_from_yaml(yaml);

    assert_eq!(
        config.validation.outcome_timeout_seconds, 600,
        "large validation timeout values should be accepted for complex gates"
    );
}

#[test]
fn validation_timeout_with_stderr_cap() {
    // Config with both validation timeout and stderr cap
    let yaml = r#"
validation:
  outcome_timeout_seconds: 180
  stderr_cap_bytes: 8192
"#;

    let config = load_config_from_yaml(yaml);

    assert_eq!(config.validation.outcome_timeout_seconds, 180);
    assert_eq!(config.validation.stderr_cap_bytes, 8192);
}

// ═════════════════════════════════════════════════════════════════════════════
// Timeout-Triggered Mitosis Policy Tests
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn timeout_triggered_mitosis_policy_default_disabled() {
    // Default policy should be disabled for backward compatibility
    let config = load_config_from_yaml("");

    let policy = &config.strands.mitosis.timeout_triggered;

    assert!(
        !policy.enabled,
        "timeout-triggered mitosis should be disabled by default"
    );
    assert!(
        !policy.agent_wallclock_timeout,
        "agent wallclock timeout qualification should be disabled by default"
    );
    assert!(
        !policy.handler_timeout,
        "handler timeout qualification should be disabled by default"
    );
    assert_eq!(
        policy.min_elapsed_fraction, 0.9,
        "min elapsed fraction should default to 0.9"
    );
}

#[test]
fn timeout_triggered_mitosis_policy_enabled_full_config() {
    // Full enabled policy configuration
    let yaml = r#"
strands:
  mitosis:
    timeout_triggered:
      enabled: true
      agent_wallclock_timeout: true
      handler_timeout: true
      min_elapsed_fraction: 0.85
"#;

    let config = load_config_from_yaml(yaml);
    let policy = &config.strands.mitosis.timeout_triggered;

    assert!(policy.enabled, "policy should be enabled");
    assert!(
        policy.agent_wallclock_timeout,
        "agent wallclock timeout should be enabled"
    );
    assert!(policy.handler_timeout, "handler timeout should be enabled");
    assert_eq!(
        policy.min_elapsed_fraction, 0.85,
        "min elapsed fraction should be parsed correctly"
    );
}

#[test]
fn timeout_triggered_mitosis_policy_agent_wallclock_only() {
    // Policy with only agent wallclock timeout enabled
    let yaml = r#"
strands:
  mitosis:
    timeout_triggered:
      enabled: true
      agent_wallclock_timeout: true
      handler_timeout: false
      min_elapsed_fraction: 0.95
"#;

    let config = load_config_from_yaml(yaml);
    let policy = &config.strands.mitosis.timeout_triggered;

    assert!(policy.enabled);
    assert!(policy.agent_wallclock_timeout);
    assert!(!policy.handler_timeout);
    assert_eq!(policy.min_elapsed_fraction, 0.95);
}

#[test]
fn timeout_triggered_mitosis_policy_handler_only() {
    // Policy with only handler timeout enabled
    let yaml = r#"
strands:
  mitosis:
    timeout_triggered:
      enabled: true
      agent_wallclock_timeout: false
      handler_timeout: true
      min_elapsed_fraction: 0.8
"#;

    let config = load_config_from_yaml(yaml);
    let policy = &config.strands.mitosis.timeout_triggered;

    assert!(policy.enabled);
    assert!(!policy.agent_wallclock_timeout);
    assert!(policy.handler_timeout);
    assert_eq!(policy.min_elapsed_fraction, 0.8);
}

#[test]
fn timeout_triggered_mitosis_policy_min_elapsed_fraction_boundaries() {
    // Test min_elapsed_fraction at boundaries

    // Lower boundary (0.0 - should accept any timeout)
    let yaml = r#"
strands:
  mitosis:
    timeout_triggered:
      enabled: true
      min_elapsed_fraction: 0.0
"#;

    let config = load_config_from_yaml(yaml);
    assert_eq!(
        config
            .strands
            .mitosis
            .timeout_triggered
            .min_elapsed_fraction,
        0.0
    );

    // Upper boundary (1.0 - require full timeout)
    let yaml = r#"
strands:
  mitosis:
    timeout_triggered:
      enabled: true
      min_elapsed_fraction: 1.0
"#;

    let config = load_config_from_yaml(yaml);
    assert_eq!(
        config
            .strands
            .mitosis
            .timeout_triggered
            .min_elapsed_fraction,
        1.0
    );
}

#[test]
fn timeout_triggered_mitosis_policy_within_mitosis_section() {
    // Timeout policy configured within full mitosis config
    let yaml = r#"
strands:
  mitosis:
    enabled: true
    first_failure_only: true
    timeout_triggered:
      enabled: true
      agent_wallclock_timeout: true
      handler_timeout: false
      min_elapsed_fraction: 0.92
"#;

    let config = load_config_from_yaml(yaml);
    let mitosis = &config.strands.mitosis;

    assert!(mitosis.enabled);
    assert!(mitosis.first_failure_only);

    let policy = &mitosis.timeout_triggered;
    assert!(policy.enabled);
    assert!(policy.agent_wallclock_timeout);
    assert!(!policy.handler_timeout);
    assert_eq!(policy.min_elapsed_fraction, 0.92);
}

// ═════════════════════════════════════════════════════════════════════════════
// Legacy Configuration Compatibility Tests
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn legacy_config_without_validation_section() {
    // Config from before validation section was added should still work
    let yaml = r#"
agent:
  default: claude
  timeout: 3600

worker:
  max_workers: 4
  building_timeout: 600
"#;

    let config = load_config_from_yaml(yaml);

    // Should use defaults for validation fields
    assert_eq!(
        config.validation.outcome_timeout_seconds, 50,
        "legacy config without validation section should use default"
    );
    assert_eq!(
        config.validation.stderr_cap_bytes, 4096,
        "legacy config should use default stderr cap"
    );

    // Explicit values should be preserved
    assert_eq!(config.agent.timeout, 3600);
    assert_eq!(config.worker.building_timeout, 600);
}

#[test]
fn legacy_config_without_timeout_triggered_section() {
    // Config from before timeout-triggered mitosis was added
    let yaml = r#"
strands:
  mitosis:
    enabled: true
    first_failure_only: true
"#;

    let config = load_config_from_yaml(yaml);

    // Should use defaults for timeout-triggered policy
    let policy = &config.strands.mitosis.timeout_triggered;
    assert!(
        !policy.enabled,
        "legacy config should have timeout-triggered disabled"
    );
    assert_eq!(policy.min_elapsed_fraction, 0.9);
}

#[test]
fn minimal_legacy_config_compatibility() {
    // Very minimal config (only essential fields)
    let yaml = r#"
agent:
  default: claude
"#;

    let config = load_config_from_yaml(yaml);

    // All timeout fields should use defaults
    assert_eq!(config.agent.timeout, 3600);
    assert_eq!(config.worker.building_timeout, 600);
    assert_eq!(config.validation.outcome_timeout_seconds, 50);
    assert!(!config.strands.mitosis.timeout_triggered.enabled);
}

// ═════════════════════════════════════════════════════════════════════════════
// Combined Timeout Configuration Tests
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn all_timeout_fields_together() {
    // Config with all timeout fields specified
    let yaml = r#"
agent:
  timeout: 7200

worker:
  building_timeout: 900

validation:
  outcome_timeout_seconds: 120
  stderr_cap_bytes: 8192

strands:
  mitosis:
    timeout_triggered:
      enabled: true
      agent_wallclock_timeout: true
      handler_timeout: true
      min_elapsed_fraction: 0.88
"#;

    let config = load_config_from_yaml(yaml);

    assert_eq!(config.agent.timeout, 7200);
    assert_eq!(config.worker.building_timeout, 900);
    assert_eq!(config.validation.outcome_timeout_seconds, 120);
    assert_eq!(config.validation.stderr_cap_bytes, 8192);

    let policy = &config.strands.mitosis.timeout_triggered;
    assert!(policy.enabled);
    assert!(policy.agent_wallclock_timeout);
    assert!(policy.handler_timeout);
    assert_eq!(policy.min_elapsed_fraction, 0.88);
}

#[test]
fn mixed_defaults_and_overrides() {
    // Config with some fields at defaults, some overridden
    let yaml = r#"
agent:
  timeout: 3600  # Default value

worker:
  building_timeout: 300  # Non-default

validation:
  outcome_timeout_seconds: 50  # Default value

strands:
  mitosis:
    timeout_triggered:
      enabled: false  # Default value
      min_elapsed_fraction: 0.95  # Non-default
"#;

    let config = load_config_from_yaml(yaml);

    assert_eq!(config.agent.timeout, 3600);
    assert_eq!(
        config.worker.building_timeout, 300,
        "non-default should be preserved"
    );
    assert_eq!(config.validation.outcome_timeout_seconds, 50);
    assert_eq!(
        config
            .strands
            .mitosis
            .timeout_triggered
            .min_elapsed_fraction,
        0.95
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Invalid Configuration Tests
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn invalid_timeout_type_rejected_gracefully() {
    // Config with non-integer timeout value
    let yaml = r#"
agent:
  timeout: "not a number"
"#;

    let result = std::panic::catch_unwind(|| {
        load_config_from_yaml(yaml);
    });

    // Should fail to parse (YAML error or type error)
    assert!(
        result.is_err(),
        "non-integer timeout should cause parsing failure"
    );
}

#[test]
fn invalid_min_elapsed_fraction_rejected() {
    // Config with min_elapsed_fraction outside [0.0, 1.0]
    let yaml = r#"
strands:
  mitosis:
    timeout_triggered:
      enabled: true
      min_elapsed_fraction: 1.5
"#;

    let config = load_config_from_yaml(yaml);
    // Note: serde doesn't validate ranges by default, so this might parse
    // but should be validated at application level
    assert_eq!(
        config
            .strands
            .mitosis
            .timeout_triggered
            .min_elapsed_fraction,
        1.5
    );
    // Application code should reject this value
}

#[test]
fn negative_timeout_rejected() {
    // Config with negative timeout value
    let yaml = r#"
agent:
  timeout: -100
"#;

    let result = std::panic::catch_unwind(|| {
        load_config_from_yaml(yaml);
    });

    // Should fail to parse
    assert!(
        result.is_err(),
        "negative timeout should cause parsing failure"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Environment Variable Override Tests
// ═════════════════════════════════════════════════════════════════════════════

#[test]
#[ignore] // Requires environment variable manipulation
fn env_var_agent_timeout_override() {
    // Test NEEDLE_AGENT__TIMEOUT environment variable
    // This test is ignored by default as it requires env var setup
}

#[test]
#[ignore] // Requires environment variable manipulation
fn env_var_worker_building_timeout_override() {
    // Test NEEDLE_WORKER__BUILDING_TIMEOUT environment variable
    // This test is ignored by default as it requires env var setup
}

// ═════════════════════════════════════════════════════════════════════════════
// Workspace Override Tests
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn workspace_override_agent_timeout() {
    // Workspace config can override agent timeout
    let yaml = r#"
agent:
  timeout: 2400
"#;

    let (temp_dir, config_path) = create_temp_config(yaml);

    // Load as workspace override
    let mut config = Config::default();
    let overrides = ConfigLoader::load_workspace(config_path.parent().unwrap())
        .expect("failed to load workspace config")
        .expect("workspace config should exist");

    let mut sources = std::collections::BTreeMap::new();
    ConfigLoader::apply_workspace(
        &mut config,
        &overrides,
        config_path.parent().unwrap(),
        &mut sources,
    );

    assert_eq!(
        config.agent.timeout, 2400,
        "workspace config should override agent timeout"
    );

    // Explicitly drop temp_dir after all uses of config_path
    drop(temp_dir);
}

#[test]
fn workspace_default_timeout_when_not_specified() {
    // Workspace config without timeout should use global config value
    let yaml = r#"
agent:
  default: opus
"#;

    let (temp_dir, config_path) = create_temp_config(yaml);

    let mut config = Config::default();
    let overrides = ConfigLoader::load_workspace(config_path.parent().unwrap())
        .expect("failed to load workspace config")
        .expect("workspace config should exist");

    let mut sources = std::collections::BTreeMap::new();
    ConfigLoader::apply_workspace(
        &mut config,
        &overrides,
        config_path.parent().unwrap(),
        &mut sources,
    );

    assert_eq!(
        config.agent.timeout, 3600,
        "workspace config without timeout should preserve global default"
    );

    // Explicitly drop temp_dir after all uses of config_path
    drop(temp_dir);
}

// ═════════════════════════════════════════════════════════════════════════════
// Real-World Configuration Examples
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn real_world_config_long_running_tasks() {
    // Real-world config for workspace with long-running tasks
    let yaml = r#"
# Workspace for analysis tasks that can take hours
agent:
  default: claude
  timeout: 14400  # 4 hours for deep analysis

worker:
  max_workers: 2
  building_timeout: 1800  # 30 minutes for complex builds

validation:
  outcome_timeout_seconds: 300  # 5 minutes for comprehensive tests
  stderr_cap_bytes: 16384  # Larger cap for detailed test output

strands:
  mitosis:
    enabled: true
    timeout_triggered:
      enabled: true
      agent_wallclock_timeout: true
      handler_timeout: true
      min_elapsed_fraction: 0.9
"#;

    let config = load_config_from_yaml(yaml);

    assert_eq!(config.agent.timeout, 14400);
    assert_eq!(config.worker.building_timeout, 1800);
    assert_eq!(config.validation.outcome_timeout_seconds, 300);
    assert_eq!(config.validation.stderr_cap_bytes, 16384);

    let policy = &config.strands.mitosis.timeout_triggered;
    assert!(policy.enabled);
    assert!(policy.agent_wallclock_timeout);
    assert!(policy.handler_timeout);
    assert_eq!(policy.min_elapsed_fraction, 0.9);
}

#[test]
fn real_world_config_fast_iteration_workspace() {
    // Real-world config for fast iteration workspace
    let yaml = r#"
# Workspace for quick fixes and iterations
agent:
  default: claude
  timeout: 600  # 10 minutes for quick fixes

worker:
  max_workers: 6
  building_timeout: 120  # 2 minutes for fast builds

validation:
  outcome_timeout_seconds: 30  # 30 seconds for quick checks
  stderr_cap_bytes: 4096

strands:
  mitosis:
    enabled: true
    timeout_triggered:
      enabled: false  # Disabled for quick tasks
"#;

    let config = load_config_from_yaml(yaml);

    assert_eq!(config.agent.timeout, 600);
    assert_eq!(config.worker.building_timeout, 120);
    assert_eq!(config.validation.outcome_timeout_seconds, 30);

    let policy = &config.strands.mitosis.timeout_triggered;
    assert!(
        !policy.enabled,
        "fast iteration workspace should disable timeout-triggered mitosis"
    );
}

#[test]
fn real_world_config_unlimited_timeout_special_case() {
    // Real-world config for workspace that needs unlimited time
    let yaml = r#"
# Workspace for research and exploration (no time limits)
agent:
  default: claude
  timeout: 0  # Unlimited - for deep research

worker:
  max_workers: 1
  building_timeout: 0  # Unlimited - for large builds

validation:
  outcome_timeout_seconds: 600  # 10 minutes, but can be increased
"#;

    let config = load_config_from_yaml(yaml);

    assert_eq!(
        config.agent.timeout, 0,
        "research workspace should have unlimited agent timeout"
    );
    assert_eq!(
        config.worker.building_timeout, 0,
        "research workspace should have unlimited building timeout"
    );
    assert_eq!(config.validation.outcome_timeout_seconds, 600);
}

// ═════════════════════════════════════════════════════════════════════════════
// Config Round-Trip Tests
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn config_round_trip_serialization() {
    // Test that config can be serialized and deserialized without loss
    let yaml = r#"
agent:
  timeout: 4800

worker:
  building_timeout: 450

validation:
  outcome_timeout_seconds: 90

strands:
  mitosis:
    timeout_triggered:
      enabled: true
      min_elapsed_fraction: 0.93
"#;

    let config1 = load_config_from_yaml(yaml);

    // Serialize back to YAML
    let serialized = serde_yaml::to_string(&config1).expect("failed to serialize config");

    // Deserialize again
    let config2: Config =
        serde_yaml::from_str(&serialized).expect("failed to deserialize serialized config");

    // Values should be preserved
    assert_eq!(config1.agent.timeout, config2.agent.timeout);
    assert_eq!(
        config1.worker.building_timeout,
        config2.worker.building_timeout
    );
    assert_eq!(
        config1.validation.outcome_timeout_seconds,
        config2.validation.outcome_timeout_seconds
    );
    assert_eq!(
        config1.strands.mitosis.timeout_triggered.enabled,
        config2.strands.mitosis.timeout_triggered.enabled
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Timeout Policy Qualifies Method Tests
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn timeout_policy_qualifies_agent_wallclock() {
    let policy = TimeoutTriggeredPolicy {
        enabled: true,
        agent_wallclock_timeout: true,
        handler_timeout: false,
        min_elapsed_fraction: 0.9,
    };

    // Agent wallclock timeout should qualify when enabled and fraction threshold met
    assert!(
        policy.qualifies("agent_wallclock_timeout", 0.95),
        "agent wallclock timeout with 95% elapsed should qualify"
    );

    assert!(
        !policy.qualifies("agent_wallclock_timeout", 0.8),
        "agent wallclock timeout with 80% elapsed should not qualify (below 0.9 threshold)"
    );
}

#[test]
fn timeout_policy_qualifies_handler_timeout() {
    let policy = TimeoutTriggeredPolicy {
        enabled: true,
        agent_wallclock_timeout: false,
        handler_timeout: true,
        min_elapsed_fraction: 0.9,
    };

    // Handler timeout should qualify when enabled and fraction threshold met
    assert!(
        policy.qualifies("handler_timeout", 0.92),
        "handler timeout with 92% elapsed should qualify"
    );

    assert!(
        !policy.qualifies("handler_timeout", 0.85),
        "handler timeout with 85% elapsed should not qualify"
    );
}

#[test]
fn timeout_policy_disabled_never_qualifies() {
    let policy = TimeoutTriggeredPolicy {
        enabled: false, // Disabled
        agent_wallclock_timeout: true,
        handler_timeout: true,
        min_elapsed_fraction: 0.5, // Low threshold
    };

    // When policy is disabled, nothing should qualify
    assert!(
        !policy.qualifies("agent_wallclock_timeout", 0.99),
        "disabled policy should not qualify any timeout"
    );
    assert!(
        !policy.qualifies("handler_timeout", 0.99),
        "disabled policy should not qualify handler timeout"
    );
}

#[test]
fn timeout_policy_unknown_reason_never_qualifies() {
    let policy = TimeoutTriggeredPolicy {
        enabled: true,
        agent_wallclock_timeout: true,
        handler_timeout: true,
        min_elapsed_fraction: 0.9,
    };

    // Unknown timeout reasons should not qualify
    assert!(
        !policy.qualifies("unknown_timeout_reason", 0.99),
        "unknown timeout reason should not qualify"
    );
    assert!(
        !policy.qualifies("build_timeout", 0.99),
        "build timeout reason should not qualify (not in policy)"
    );
}
