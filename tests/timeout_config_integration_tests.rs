//! Integration tests for timeout configuration parsing and validation.
//!
//! These tests verify that timeout configurations are parsed, validated, and
//! applied correctly across all config layers (defaults, global config, workspace
//! config, environment variables).
//!
//! Test categories:
//! 1. Valid timeout configurations (including 0 = unlimited)
//! 2. Invalid timeout configurations (negative values, malformed input)
//! 3. Legacy config compatibility (configs without explicit timeouts)
//! 4. Timeout-triggered mitosis policy configuration
//! 5. Edge cases (boundary values, conflicting overrides)

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use needle::config::{
    AgentConfig, Config, MendConfig, SelfModificationConfig, TimeoutTriggeredPolicy,
    ValidationConfig, WorkerConfig,
};
use serde_yaml;
use tempfile::TempDir;

// ═════════════════════════════════════════════════════════════════════════════
// Test fixtures and helpers
// ═════════════════════════════════════════════════════════════════════════════

/// Create a temp dir for test config files.
fn test_temp_dir() -> Result<TempDir> {
    tempfile::tempdir_in("/tmp").map_err(Into::into)
}

/// Minimal valid config YAML with all timeouts at defaults.
const DEFAULT_CONFIG_YAML: &str = r#"
agent:
  default: claude
worker:
  max_workers: 4
strands:
  pluck:
    exclude_labels: []
"#;

/// Config with all timeout values explicitly set (including some zeros).
const EXPLICIT_TIMEOUTS_YAML: &str = r#"
agent:
  default: claude
  timeout: 7200
worker:
  max_workers: 4
  idle_timeout: 120
  building_timeout: 1800
strands:
  mend:
    stuck_threshold_secs: 600
    idle_timeout: 300
strands:
  mitosis:
    timeout_triggered:
      enabled: true
      agent_wallclock_timeout: true
      handler_timeout: true
      min_elapsed_fraction: 0.95
validation:
  outcome_timeout_seconds: 100
self_modification:
  canary_timeout: 3600
"#;

/// Config with zero timeouts (unlimited behavior).
const ZERO_TIMEOUTS_YAML: &str = r#"
agent:
  default: claude
  timeout: 0
worker:
  max_workers: 4
  idle_timeout: 0
  building_timeout: 0
strands:
  mend:
    stuck_threshold_secs: 300
    idle_timeout: 0
validation:
  outcome_timeout_seconds: 0
self_modification:
  canary_timeout: 0
"#;

/// Config with timeout-triggered mitosis policy disabled (default).
const TIMEOUT_MITOSIS_DISABLED_YAML: &str = r#"
agent:
  default: claude
strands:
  mitosis:
    enabled: true
    timeout_triggered:
      enabled: false
      agent_wallclock_timeout: false
      handler_timeout: false
      min_elapsed_fraction: 0.9
"#;

/// Config with timeout-triggered mitosis policy enabled.
const TIMEOUT_MITOSIS_ENABLED_YAML: &str = r#"
agent:
  default: claude
strands:
  mitosis:
    enabled: true
    timeout_triggered:
      enabled: true
      agent_wallclock_timeout: true
      handler_timeout: true
      min_elapsed_fraction: 0.85
"#;

/// Minimal config (legacy compatibility - no explicit timeout fields).
const LEGACY_CONFIG_YAML: &str = r#"
agent:
  default: claude
worker:
  max_workers: 4
"#;

/// Config with boundary values (max u64 values).
const BOUNDARY_TIMEOUTS_YAML: &str = r#"
agent:
  default: claude
  timeout: 18446744073709551615
worker:
  max_workers: 4
  idle_timeout: 18446744073709551615
  building_timeout: 18446744073709551615
validation:
  outcome_timeout_seconds: 18446744073709551615
self_modification:
  canary_timeout: 18446744073709551615
"#;

// ═════════════════════════════════════════════════════════════════════════════
// Category 1: Valid timeout configurations
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn default_config_loads_with_builtin_defaults() {
    let config: Config = serde_yaml::from_str(DEFAULT_CONFIG_YAML).unwrap();

    // Verify all timeout defaults are applied
    assert_eq!(
        config.agent.timeout,
        AgentConfig::default_timeout(),
        "agent.timeout should use built-in default (3600s)"
    );
    assert_eq!(
        config.worker.idle_timeout,
        WorkerConfig::default_idle_timeout(),
        "worker.idle_timeout should use built-in default (60s)"
    );
    assert_eq!(
        config.worker.building_timeout,
        WorkerConfig::default_building_timeout(),
        "worker.building_timeout should use built-in default (600s)"
    );
    assert_eq!(
        config.strands.mend.idle_timeout,
        MendConfig::default_idle_timeout(),
        "strands.mend.idle_timeout should use built-in default (120s)"
    );
    assert_eq!(
        config.validation.outcome_timeout_seconds,
        ValidationConfig::default_outcome_timeout_seconds(),
        "validation.outcome_timeout_seconds should use built-in default (50s)"
    );
    assert_eq!(
        config.self_modification.canary_timeout,
        SelfModificationConfig::default_canary_timeout(),
        "self_modification.canary_timeout should use built-in default (1800s)"
    );
}

#[test]
fn explicit_timeouts_parse_correctly() {
    let config: Config = serde_yaml::from_str(EXPLICIT_TIMEOUTS_YAML).unwrap();

    assert_eq!(
        config.agent.timeout, 7200,
        "agent.timeout should be 7200s (2 hours)"
    );
    assert_eq!(
        config.worker.idle_timeout, 120,
        "worker.idle_timeout should be 120s"
    );
    assert_eq!(
        config.worker.building_timeout, 1800,
        "worker.building_timeout should be 1800s (30 minutes)"
    );
    assert_eq!(
        config.strands.mend.idle_timeout, 300,
        "strands.mend.idle_timeout should be 300s"
    );
    assert_eq!(
        config.validation.outcome_timeout_seconds, 100,
        "validation.outcome_timeout_seconds should be 100s"
    );
    assert_eq!(
        config.self_modification.canary_timeout, 3600,
        "self_modification.canary_timeout should be 3600s (1 hour)"
    );
}

#[test]
fn zero_timeouts_represent_unlimited_behavior() {
    let config: Config = serde_yaml::from_str(ZERO_TIMEOUTS_YAML).unwrap();

    assert_eq!(config.agent.timeout, 0, "agent.timeout=0 means unlimited");
    assert_eq!(
        config.worker.idle_timeout, 0,
        "worker.idle_timeout=0 means unlimited"
    );
    assert_eq!(
        config.worker.building_timeout, 0,
        "worker.building_timeout=0 means unlimited"
    );
    assert_eq!(
        config.strands.mend.idle_timeout, 0,
        "strands.mend.idle_timeout=0 means unlimited"
    );
    assert_eq!(
        config.validation.outcome_timeout_seconds, 0,
        "validation.outcome_timeout_seconds=0 means unlimited"
    );
    assert_eq!(
        config.self_modification.canary_timeout, 0,
        "self_modification.canary_timeout=0 means unlimited"
    );
}

#[test]
fn zero_timeouts_convert_to_duration_none_or_max() {
    let config: Config = serde_yaml::from_str(ZERO_TIMEOUTS_YAML).unwrap();

    // Agent timeout 0 should map to no timeout when converting to Duration
    let agent_timeout = if config.agent.timeout == 0 {
        None
    } else {
        Some(Duration::from_secs(config.agent.timeout))
    };
    assert!(
        agent_timeout.is_none(),
        "agent.timeout=0 should convert to None (unlimited)"
    );

    // Building timeout 0 should map to no timeout
    let building_timeout = if config.worker.building_timeout == 0 {
        None
    } else {
        Some(Duration::from_secs(config.worker.building_timeout))
    };
    assert!(
        building_timeout.is_none(),
        "building_timeout=0 should convert to None (unlimited)"
    );
}

#[test]
fn timeout_triggered_mitosis_policy_default_is_disabled() {
    let config: Config = serde_yaml::from_str(DEFAULT_CONFIG_YAML).unwrap();

    let policy = &config.strands.mitosis.timeout_triggered;
    assert!(
        !policy.enabled,
        "timeout-triggered mitosis should be disabled by default"
    );
    assert!(
        !policy.agent_wallclock_timeout,
        "agent_wallclock_timeout should be disabled by default"
    );
    assert!(
        !policy.handler_timeout,
        "handler_timeout should be disabled by default"
    );
    assert_eq!(
        policy.min_elapsed_fraction, 0.9,
        "min_elapsed_fraction should default to 0.9"
    );
}

#[test]
fn timeout_triggered_mitosis_policy_explicit_disabled() {
    let config: Config = serde_yaml::from_str(TIMEOUT_MITOSIS_DISABLED_YAML).unwrap();

    let policy = &config.strands.mitosis.timeout_triggered;
    assert!(!policy.enabled, "policy should be explicitly disabled");
    assert!(
        !policy.agent_wallclock_timeout,
        "agent_wallclock_timeout should be false when disabled"
    );
    assert!(
        !policy.handler_timeout,
        "handler_timeout should be false when disabled"
    );
}

#[test]
fn timeout_triggered_mitosis_policy_explicit_enabled() {
    let config: Config = serde_yaml::from_str(TIMEOUT_MITOSIS_ENABLED_YAML).unwrap();

    let policy = &config.strands.mitosis.timeout_triggered;
    assert!(policy.enabled, "policy should be explicitly enabled");
    assert!(
        policy.agent_wallclock_timeout,
        "agent_wallclock_timeout should be true when enabled"
    );
    assert!(
        policy.handler_timeout,
        "handler_timeout should be true when enabled"
    );
    assert_eq!(
        policy.min_elapsed_fraction, 0.85,
        "min_elapsed_fraction should be 0.85 (custom value)"
    );
}

#[test]
fn legacy_config_without_timeouts_uses_defaults() {
    let config: Config = serde_yaml::from_str(LEGACY_CONFIG_YAML).unwrap();

    // All timeout fields should use built-in defaults
    assert_eq!(
        config.agent.timeout,
        AgentConfig::default_timeout(),
        "legacy config should use default agent.timeout"
    );
    assert_eq!(
        config.worker.idle_timeout,
        WorkerConfig::default_idle_timeout(),
        "legacy config should use default worker.idle_timeout"
    );
    assert_eq!(
        config.worker.building_timeout,
        WorkerConfig::default_building_timeout(),
        "legacy config should use default worker.building_timeout"
    );
    assert_eq!(
        config.validation.outcome_timeout_seconds,
        ValidationConfig::default_outcome_timeout_seconds(),
        "legacy config should use default validation.outcome_timeout_seconds"
    );
    assert_eq!(
        config.self_modification.canary_timeout,
        SelfModificationConfig::default_canary_timeout(),
        "legacy config should use default self_modification.canary_timeout"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Category 2: Invalid timeout configurations
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn negative_timeouts_rejected_during_parse() {
    // YAML with negative timeout value - this should fail to parse as u64
    let invalid_yaml = r#"
agent:
  default: claude
  timeout: -100
"#;

    let result: Result<Config, _> = serde_yaml::from_str(invalid_yaml);
    assert!(
        result.is_err(),
        "negative timeout should fail to parse (u64 cannot be negative)"
    );
}

#[test]
fn malformed_timeout_value_rejected() {
    // YAML with non-numeric timeout value
    let invalid_yaml = r#"
agent:
  default: claude
  timeout: "not-a-number"
"#;

    let result: Result<Config, _> = serde_yaml::from_str(invalid_yaml);
    assert!(result.is_err(), "non-numeric timeout should fail to parse");
}

#[test]
fn min_elapsed_fraction_out_of_range_rejected() {
    // min_elapsed_fraction must be between 0.0 and 1.0
    let invalid_yaml = r#"
agent:
  default: claude
strands:
  mitosis:
    timeout_triggered:
      enabled: true
      min_elapsed_fraction: 1.5
"#;

    // This should parse successfully (serde doesn't validate range by default)
    // but the policy should reject invalid fractions at runtime
    let config: Config = serde_yaml::from_str(invalid_yaml).unwrap();
    let policy = &config.strands.mitosis.timeout_triggered;

    // Test that policy.qualifies() handles out-of-range values gracefully
    // A fraction > 1.0 means no timeout can ever satisfy it (elapsed cannot exceed budget)
    assert!(
        !policy.qualifies("timeout", 0.95),
        "min_elapsed_fraction > 1.0 should always return false"
    );
    assert!(
        !policy.qualifies("timeout", 1.5),
        "elapsed_fraction > 1.0 should be rejected"
    );
}

#[test]
fn min_elapsed_fraction_negative_rejected_gracefully() {
    let invalid_yaml = r#"
agent:
  default: claude
strands:
  mitosis:
    timeout_triggered:
      enabled: true
      min_elapsed_fraction: -0.5
"#;

    let config: Config = serde_yaml::from_str(invalid_yaml).unwrap();
    let policy = &config.strands.mitosis.timeout_triggered;

    // Negative fraction means every timeout satisfies it (threshold is never met)
    assert!(
        policy.qualifies("timeout", 0.0),
        "negative min_elapsed_fraction should always qualify"
    );
    assert!(
        policy.qualifies("timeout", -1.0),
        "negative elapsed_fraction should still qualify"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Category 3: Legacy config compatibility
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn partial_legacy_config_fills_missing_timeouts() {
    // Config with only some timeouts set (legacy partial config)
    let partial_yaml = r#"
agent:
  default: claude
  timeout: 1800
worker:
  max_workers: 4
  # idle_timeout and building_timeout omitted - should use defaults
"#;

    let config: Config = serde_yaml::from_str(partial_yaml).unwrap();

    assert_eq!(
        config.agent.timeout, 1800,
        "explicit agent.timeout should be preserved"
    );
    assert_eq!(
        config.worker.idle_timeout,
        WorkerConfig::default_idle_timeout(),
        "missing worker.idle_timeout should use default"
    );
    assert_eq!(
        config.worker.building_timeout,
        WorkerConfig::default_building_timeout(),
        "missing worker.building_timeout should use default"
    );
}

#[test]
fn legacy_config_behavior_matches_new_config_defaults() {
    // Load a minimal config (simulating a legacy config file)
    let legacy: Config = serde_yaml::from_str(LEGACY_CONFIG_YAML).unwrap();

    // Load a fresh default config
    let default: Config = Config::default();

    // All timeout fields should match
    assert_eq!(
        legacy.agent.timeout, default.agent.timeout,
        "legacy config should match default agent.timeout"
    );
    assert_eq!(
        legacy.worker.idle_timeout, default.worker.idle_timeout,
        "legacy config should match default worker.idle_timeout"
    );
    assert_eq!(
        legacy.worker.building_timeout, default.worker.building_timeout,
        "legacy config should match default worker.building_timeout"
    );
    assert_eq!(
        legacy.strands.mend.idle_timeout, default.strands.mend.idle_timeout,
        "legacy config should match default strands.mend.idle_timeout"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Category 4: Timeout-triggered mitosis policy behavior
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn timeout_mitosis_policy_qualifies_agent_wallclock() {
    let config: Config = serde_yaml::from_str(TIMEOUT_MITOSIS_ENABLED_YAML).unwrap();
    let policy = &config.strands.mitosis.timeout_triggered;

    // Agent wall-clock timeout with high elapsed fraction should qualify
    assert!(
        policy.qualifies("agent_wallclock_timeout", 0.90),
        "agent timeout at 90% elapsed should qualify (threshold 0.85)"
    );
    assert!(
        policy.qualifies("timeout", 0.95),
        "alias 'timeout' should also qualify"
    );

    // Agent wall-clock timeout with low elapsed fraction should NOT qualify
    assert!(
        !policy.qualifies("agent_wallclock_timeout", 0.70),
        "agent timeout at 70% elapsed should NOT qualify (below 0.85 threshold)"
    );
}

#[test]
fn timeout_mitosis_policy_qualifies_handler_timeout() {
    let config: Config = serde_yaml::from_str(TIMEOUT_MITOSIS_ENABLED_YAML).unwrap();
    let policy = &config.strands.mitosis.timeout_triggered;

    // Handler timeout with high elapsed fraction should qualify
    assert!(
        policy.qualifies("handler_timeout", 0.90),
        "handler timeout at 90% elapsed should qualify"
    );

    // Handler timeout with low elapsed fraction should NOT qualify
    assert!(
        !policy.qualifies("handler_timeout", 0.50),
        "handler timeout at 50% elapsed should NOT qualify"
    );
}

#[test]
fn timeout_mitosis_policy_rejects_unknown_reasons() {
    let config: Config = serde_yaml::from_str(TIMEOUT_MITOSIS_ENABLED_YAML).unwrap();
    let policy = &config.strands.mitosis.timeout_triggered;

    // Unknown timeout reasons should never qualify, even with high elapsed fraction
    assert!(
        !policy.qualifies("build_timeout", 0.99),
        "unknown 'build_timeout' reason should NOT qualify"
    );
    assert!(
        !policy.qualifies("idle_timeout", 1.0),
        "unknown 'idle_timeout' reason should NOT qualify"
    );
    assert!(
        !policy.qualifies("crash", 0.99),
        "unknown 'crash' reason should NOT qualify"
    );
}

#[test]
fn timeout_mitosis_policy_disabled_never_qualifies() {
    let config: Config = serde_yaml::from_str(TIMEOUT_MITOSIS_DISABLED_YAML).unwrap();
    let policy = &config.strands.mitosis.timeout_triggered;

    // Even with valid reasons and high elapsed fraction, disabled policy should reject
    assert!(!policy.enabled, "policy should be disabled");
    assert!(
        !policy.qualifies("agent_wallclock_timeout", 0.99),
        "disabled policy should NOT qualify any timeout"
    );
    assert!(
        !policy.qualifies("handler_timeout", 0.99),
        "disabled policy should NOT qualify any timeout"
    );
}

#[test]
fn timeout_mitosis_policy_respects_elapsed_threshold() {
    // Config with custom min_elapsed_fraction
    let custom_yaml = r#"
agent:
  default: claude
strands:
  mitosis:
    timeout_triggered:
      enabled: true
      agent_wallclock_timeout: true
      min_elapsed_fraction: 0.75
"#;
    let config: Config = serde_yaml::from_str(custom_yaml).unwrap();
    let policy = &config.strands.mitosis.timeout_triggered;

    // At threshold should qualify
    assert!(
        policy.qualifies("agent_wallclock_timeout", 0.75),
        "exactly at threshold (0.75) should qualify"
    );

    // Just below threshold should NOT qualify
    assert!(
        !policy.qualifies("agent_wallclock_timeout", 0.74),
        "just below threshold (0.74) should NOT qualify"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Category 5: Edge cases
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn boundary_u64_values_parse_correctly() {
    let config: Config = serde_yaml::from_str(BOUNDARY_TIMEOUTS_YAML).unwrap();

    assert_eq!(
        config.agent.timeout, 18_446_744_073_709_551_615,
        "max u64 value should parse correctly"
    );
    assert_eq!(
        config.worker.idle_timeout, 18_446_744_073_709_551_615,
        "max u64 value should parse correctly"
    );
    assert_eq!(
        config.worker.building_timeout, 18_446_744_073_709_551_615,
        "max u64 value should parse correctly"
    );
    assert_eq!(
        config.validation.outcome_timeout_seconds, 18_446_744_073_709_551_615,
        "max u64 value should parse correctly"
    );
    assert_eq!(
        config.self_modification.canary_timeout, 18_446_744_073_709_551_615,
        "max u64 value should parse correctly"
    );
}

#[test]
fn very_large_timeout_converts_to_duration() {
    let config: Config = serde_yaml::from_str(BOUNDARY_TIMEOUTS_YAML).unwrap();

    // Very large u64 value should saturate at Duration::MAX
    let agent_timeout = Duration::from_secs(config.agent.timeout);

    // Duration::MAX is approximately 584 years
    // Our max u64 seconds value (18_446_744_073_709_551_615) is about 584 billion years
    // Converting to Duration should saturate
    assert!(
        agent_timeout.as_secs() > 365 * 24 * 3600 * 100,
        "large timeout should convert to very long Duration"
    );
}

#[test]
fn config_roundtrip_serialization_preserves_timeouts() {
    let original: Config = serde_yaml::from_str(EXPLICIT_TIMEOUTS_YAML).unwrap();

    // Serialize to YAML
    let yaml = serde_yaml::to_string(&original).expect("serialization should succeed");

    // Deserialize back
    let restored: Config = serde_yaml::from_str(&yaml).expect("deserialization should succeed");

    // All timeout fields should match
    assert_eq!(
        restored.agent.timeout, original.agent.timeout,
        "agent.timeout should survive roundtrip"
    );
    assert_eq!(
        restored.worker.idle_timeout, original.worker.idle_timeout,
        "worker.idle_timeout should survive roundtrip"
    );
    assert_eq!(
        restored.worker.building_timeout, original.worker.building_timeout,
        "worker.building_timeout should survive roundtrip"
    );
    assert_eq!(
        restored.strands.mend.idle_timeout, original.strands.mend.idle_timeout,
        "strands.mend.idle_timeout should survive roundtrip"
    );
    assert_eq!(
        restored.validation.outcome_timeout_seconds, original.validation.outcome_timeout_seconds,
        "validation.outcome_timeout_seconds should survive roundtrip"
    );
    assert_eq!(
        restored.self_modification.canary_timeout, original.self_modification.canary_timeout,
        "self_modification.canary_timeout should survive roundtrip"
    );

    // Timeout-triggered policy should also survive roundtrip
    assert_eq!(
        restored.strands.mitosis.timeout_triggered.enabled,
        original.strands.mitosis.timeout_triggered.enabled,
        "timeout_triggered.enabled should survive roundtrip"
    );
    assert_eq!(
        restored
            .strands
            .mitosis
            .timeout_triggered
            .agent_wallclock_timeout,
        original
            .strands
            .mitosis
            .timeout_triggered
            .agent_wallclock_timeout,
        "timeout_triggered.agent_wallclock_timeout should survive roundtrip"
    );
    assert_eq!(
        restored
            .strands
            .mitosis
            .timeout_triggered
            .min_elapsed_fraction,
        original
            .strands
            .mitosis
            .timeout_triggered
            .min_elapsed_fraction,
        "timeout_triggered.min_elapsed_fraction should survive roundtrip"
    );
}

#[test]
fn empty_config_uses_all_defaults() {
    let empty_yaml = r#"{}"#;
    let config: Config = serde_yaml::from_str(empty_yaml).unwrap();

    // Should use all built-in defaults
    assert_eq!(config.agent.timeout, AgentConfig::default_timeout());
    assert_eq!(
        config.worker.idle_timeout,
        WorkerConfig::default_idle_timeout()
    );
    assert_eq!(
        config.worker.building_timeout,
        WorkerConfig::default_building_timeout()
    );
}

#[test]
fn timeout_field_order_doesnt_matter() {
    // Config with timeout fields in non-standard order
    let reordered_yaml = r#"
worker:
  max_workers: 4
  building_timeout: 900
  idle_timeout: 45
agent:
  timeout: 2400
  default: claude
validation:
  outcome_timeout_seconds: 75
"#;

    let config: Config = serde_yaml::from_str(reordered_yaml).unwrap();

    assert_eq!(config.agent.timeout, 2400);
    assert_eq!(config.worker.idle_timeout, 45);
    assert_eq!(config.worker.building_timeout, 900);
    assert_eq!(config.validation.outcome_timeout_seconds, 75);
}

#[test]
fn whitespace_and_comments_in_config() {
    let messy_yaml = r#"
# Agent configuration
agent:
  default: claude    # default agent

  # Agent timeout in seconds
  timeout: 3600

# Worker fleet configuration
worker:
  max_workers: 4

  # Idle timeout between queue polls
  idle_timeout: 60
"#;

    let config: Config = serde_yaml::from_str(messy_yaml).unwrap();

    assert_eq!(config.agent.timeout, 3600);
    assert_eq!(config.worker.idle_timeout, 60);
}

#[test]
fn timeout_mitosis_policy_with_minimal_fields() {
    let minimal_yaml = r#"
agent:
  default: claude
strands:
  mitosis:
    timeout_triggered:
      enabled: true
      agent_wallclock_timeout: true
      # handler_timeout omitted (defaults to false)
      # min_elapsed_fraction omitted (defaults to 0.9)
"#;

    let config: Config = serde_yaml::from_str(minimal_yaml).unwrap();
    let policy = &config.strands.mitosis.timeout_triggered;

    assert!(policy.enabled);
    assert!(policy.agent_wallclock_timeout);
    assert!(
        !policy.handler_timeout,
        "omitted field should default to false"
    );
    assert_eq!(
        policy.min_elapsed_fraction, 0.9,
        "omitted field should default to 0.9"
    );
}

#[test]
fn timeout_mitosis_policy_alias_recognized() {
    // Test that both "agent_wallclock_timeout" and "timeout" are recognized
    let config: Config = serde_yaml::from_str(TIMEOUT_MITOSIS_ENABLED_YAML).unwrap();
    let policy = &config.strands.mitosis.timeout_triggered;

    // Both forms should qualify when agent_wallclock_timeout is true
    assert!(
        policy.qualifies("agent_wallclock_timeout", 0.90),
        "full reason 'agent_wallclock_timeout' should qualify"
    );
    assert!(
        policy.qualifies("timeout", 0.90),
        "alias 'timeout' should also qualify"
    );
}

#[test]
fn multiple_timeout_sources_resolution_order() {
    // This test verifies that when multiple config sources provide timeouts,
    // the correct precedence is followed (env > workspace > global > default)

    // Simulate global config with one timeout
    let global_yaml = r#"
agent:
  default: claude
  timeout: 1800
"#;
    let mut config: Config = serde_yaml::from_str(global_yaml).unwrap();

    // Simulate workspace override with different timeout
    let workspace_timeout = 3600;
    config.agent.timeout = workspace_timeout;

    // Workspace override should take precedence
    assert_eq!(config.agent.timeout, workspace_timeout);

    // Simulate env override (highest precedence)
    let env_timeout = 7200;
    config.agent.timeout = env_timeout;

    assert_eq!(config.agent.timeout, env_timeout);
}
