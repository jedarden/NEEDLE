//! Test OTLP telemetry configuration syntax and loading

use needle::config::TelemetryConfig;

#[test]
fn test_otlp_enabled_true_parses_successfully() {
    let yaml = r#"
    otlp_sink:
      enabled: true
    "#;

    let config: TelemetryConfig = serde_yaml::from_str(yaml)
        .expect("OTLP config with enabled: true should parse successfully");

    assert!(config.otlp_sink.enabled, "OTLP should be enabled");
}

#[test]
fn test_otlp_with_all_fields_parses_successfully() {
    let yaml = r#"
    otlp_sink:
      enabled: true
      endpoint: http://localhost:4317
      protocol: grpc
      timeout_ms: 5000
      compression: gzip
    "#;

    let config: TelemetryConfig =
        serde_yaml::from_str(yaml).expect("OTLP config with all fields should parse successfully");

    assert!(config.otlp_sink.enabled, "OTLP should be enabled");
    assert_eq!(config.otlp_sink.endpoint, "http://localhost:4317");
    assert_eq!(config.otlp_sink.protocol, "grpc");
    assert_eq!(config.otlp_sink.timeout_ms, 5000);
    assert_eq!(config.otlp_sink.compression, "gzip");
}

#[test]
fn test_otlp_defaults_when_disabled() {
    let yaml = r#"
    otlp_sink:
      enabled: false
    "#;

    let config: TelemetryConfig = serde_yaml::from_str(yaml)
        .expect("OTLP config with enabled: false should parse successfully");

    assert!(!config.otlp_sink.enabled, "OTLP should be disabled");
    assert_eq!(
        config.otlp_sink.endpoint, "http://localhost:4317",
        "default endpoint"
    );
    assert_eq!(config.otlp_sink.protocol, "grpc", "default protocol");
}

#[test]
fn test_otlp_disabled_by_default() {
    let yaml = r#"
    otlp_sink: {}
    "#;

    let config: TelemetryConfig = serde_yaml::from_str(yaml)
        .expect("OTLP config with empty otlp_sink should parse successfully");

    assert!(
        !config.otlp_sink.enabled,
        "OTLP should be disabled by default"
    );
}

#[test]
fn test_otlp_unknown_field_rejected() {
    let yaml = r#"
    otlp_sink:
      enabled: true
      unknown_field: "should fail"
    "#;

    let result: Result<TelemetryConfig, _> = serde_yaml::from_str(yaml);
    assert!(result.is_err(), "Unknown field should be rejected");

    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("unknown_field") || error_msg.contains("unknown field"),
        "Error message should mention unknown field"
    );
}

#[test]
fn test_hook_unknown_field_rejected() {
    let yaml = r#"
    hooks:
      - event_filter: "outcome.*"
        command: "/bin/sh"
        unknown_field: "should fail"
    "#;

    let result: Result<TelemetryConfig, _> = serde_yaml::from_str(yaml);
    assert!(result.is_err(), "Unknown field in hook should be rejected");
}

#[test]
fn test_otlp_tls_unknown_field_rejected() {
    let yaml = r#"
    otlp_sink:
      enabled: true
      tls:
        insecure: false
        ca_file: "/etc/ssl/certs/ca.pem"
        unknown_tls_field: "should fail"
    "#;

    let result: Result<TelemetryConfig, _> = serde_yaml::from_str(yaml);
    assert!(
        result.is_err(),
        "Unknown field in TLS config should be rejected"
    );

    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("unknown_tls_field") || error_msg.contains("unknown field"),
        "Error message should mention unknown TLS field"
    );
}

#[test]
fn test_otlp_signals_unknown_field_rejected() {
    let yaml = r#"
    otlp_sink:
      enabled: true
      signals:
        traces: true
        unknown_signal: "should fail"
    "#;

    let result: Result<TelemetryConfig, _> = serde_yaml::from_str(yaml);
    assert!(
        result.is_err(),
        "Unknown field in signals config should be rejected"
    );
}

#[test]
fn test_file_sink_unknown_field_rejected() {
    let yaml = r#"
    file_sink:
      enabled: true
      unknown_field: "should fail"
    "#;

    let result: Result<TelemetryConfig, _> = serde_yaml::from_str(yaml);
    assert!(
        result.is_err(),
        "Unknown field in file_sink config should be rejected"
    );
}

#[test]
fn test_stdout_sink_unknown_field_rejected() {
    let yaml = r#"
    stdout_sink:
      enabled: true
      unknown_field: "should fail"
    "#;

    let result: Result<TelemetryConfig, _> = serde_yaml::from_str(yaml);
    assert!(
        result.is_err(),
        "Unknown field in stdout_sink config should be rejected"
    );
}
