//! Unit tests for ProcessLimits config parsing and validation
//!
//! Tests cover:
//! - ProcessLimits with hard_deadline field parsing
//! - Validation ensures hard_deadline.duration_secs > 0 when enabled
//! - Config parsing with valid and invalid hard_deadline values

use needle::config::ProcessLimits;
use needle::types::HardDeadline;

#[test]
fn test_process_limits_with_enabled_hard_deadline_and_positive_duration() {
    let limits = ProcessLimits {
        idle_timeout: Some(300),
        hard_deadline: HardDeadline::with_duration(600),
    };

    assert!(limits.validate().is_ok());
}

#[test]
fn test_process_limits_with_disabled_hard_deadline() {
    let limits = ProcessLimits {
        idle_timeout: Some(300),
        hard_deadline: HardDeadline::disabled(),
    };

    assert!(limits.validate().is_ok());
}

#[test]
fn test_process_limits_with_enabled_hard_deadline_and_zero_duration_fails() {
    let limits = ProcessLimits {
        idle_timeout: Some(300),
        hard_deadline: HardDeadline::new(true, 0),
    };

    let result = limits.validate();
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .contains("hard_deadline is enabled but duration_secs is 0"));
}

#[test]
fn test_process_limits_with_no_idle_timeout_and_valid_hard_deadline() {
    let limits = ProcessLimits {
        idle_timeout: None,
        hard_deadline: HardDeadline::with_duration(1800),
    };

    assert!(limits.validate().is_ok());
}

#[test]
fn test_process_limits_with_no_idle_timeout_and_disabled_hard_deadline() {
    let limits = ProcessLimits {
        idle_timeout: None,
        hard_deadline: HardDeadline::disabled(),
    };

    assert!(limits.validate().is_ok());
}

#[test]
fn test_process_limits_default_is_valid() {
    let limits = ProcessLimits::default();
    assert!(limits.validate().is_ok());
}

#[test]
fn test_process_limits_serde_with_valid_hard_deadline() {
    let yaml = r#"
idle_timeout: 300s
hard_deadline:
    enabled: true
    duration_secs: 600
"#;

    let limits: ProcessLimits = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(limits.idle_timeout, Some(300));
    assert!(limits.hard_deadline.enabled);
    assert_eq!(limits.hard_deadline.duration_secs, 600);
    assert!(limits.validate().is_ok());
}

#[test]
fn test_process_limits_serde_with_disabled_hard_deadline() {
    let yaml = r#"
idle_timeout: 300s
hard_deadline:
    enabled: false
    duration_secs: 0
"#;

    let limits: ProcessLimits = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(limits.idle_timeout, Some(300));
    assert!(!limits.hard_deadline.enabled);
    assert_eq!(limits.hard_deadline.duration_secs, 0);
    assert!(limits.validate().is_ok());
}

#[test]
fn test_process_limits_serde_with_invalid_hard_deadline() {
    let yaml = r#"
idle_timeout: 300s
hard_deadline:
    enabled: true
    duration_secs: 0
"#;

    let limits: ProcessLimits = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(limits.idle_timeout, Some(300));
    assert!(limits.hard_deadline.enabled);
    assert_eq!(limits.hard_deadline.duration_secs, 0);

    let result = limits.validate();
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .contains("hard_deadline is enabled but duration_secs is 0"));
}

#[test]
fn test_process_limits_serde_without_hard_deadline_field() {
    let yaml = r#"
idle_timeout: 300s
"#;

    let limits: ProcessLimits = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(limits.idle_timeout, Some(300));
    // hard_deadline should default to disabled
    assert!(!limits.hard_deadline.enabled);
    assert_eq!(limits.hard_deadline.duration_secs, 0);
    assert!(limits.validate().is_ok());
}

#[test]
fn test_process_limits_serde_with_only_hard_deadline() {
    let yaml = r#"
hard_deadline:
    enabled: true
    duration_secs: 3600
"#;

    let limits: ProcessLimits = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(limits.idle_timeout, None);
    assert!(limits.hard_deadline.enabled);
    assert_eq!(limits.hard_deadline.duration_secs, 3600);
    assert!(limits.validate().is_ok());
}
