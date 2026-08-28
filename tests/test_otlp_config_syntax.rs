//! Test OTLP telemetry configuration syntax and loading

use needle::config::TelemetryConfig;

#[test]
fn test_otlp_enabled_true_parses_successfully() {
    let yaml = r#"
    telemetry:
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
    telemetry:
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
    telemetry:
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
    telemetry:
      otlp_sink: {}
    "#;

    let config: TelemetryConfig = serde_yaml::from_str(yaml)
        .expect("OTLP config with empty otlp_sink should parse successfully");

    assert!(
        !config.otlp_sink.enabled,
        "OTLP should be disabled by default"
    );
}
