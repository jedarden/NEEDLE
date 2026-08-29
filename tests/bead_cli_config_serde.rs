//! Comprehensive serde serialization tests for BeadCliConfig
//!
//! Tests cover:
//! - Serialization preserves backend value (all variants)
//! - Serialization preserves path value
//! - Round-trip deserialization works correctly
//! - JSON and other supported formats
//! - Field aliases work correctly

use needle::config::BeadBackend;
use needle::config::BeadCliConfig;
use std::path::PathBuf;

#[test]
fn test_serialize_auto_backend() {
    let config = BeadCliConfig {
        backend: BeadBackend::Auto,
        path: None,
    };

    let json = serde_json::to_string(&config).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed["backend"], "auto");
    assert!(parsed.get("path").is_none());
}

#[test]
fn test_serialize_bf_backend() {
    let config = BeadCliConfig {
        backend: BeadBackend::Br,
        path: None,
    };

    let json = serde_json::to_string(&config).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed["backend"], "br");
    assert!(parsed.get("path").is_none());
}

#[test]
fn test_serialize_br_backend() {
    let config = BeadCliConfig {
        backend: BeadBackend::Br,
        path: None,
    };

    let json = serde_json::to_string(&config).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed["backend"], "br");
    assert!(parsed.get("path").is_none());
}

#[test]
fn test_serialize_bead_backend() {
    let config = BeadCliConfig {
        backend: BeadBackend::Bead,
        path: None,
    };

    let json = serde_json::to_string(&config).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed["backend"], "bead-rs");
    assert!(parsed.get("path").is_none());
}

#[test]
fn test_serialize_with_path() {
    let config = BeadCliConfig {
        backend: BeadBackend::Auto,
        path: Some(PathBuf::from("/usr/local/bin/bf")),
    };

    let json = serde_json::to_string(&config).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed["backend"], "auto");
    assert_eq!(parsed["path"], "/usr/local/bin/bf");
}

#[test]
fn test_serialize_path_with_none_is_skipped() {
    let config = BeadCliConfig {
        backend: BeadBackend::Bead,
        path: None,
    };

    let json = serde_json::to_string(&config).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    // Verify that 'path' key is not present when None
    assert!(parsed.get("path").is_none());
}

#[test]
fn test_deserialize_auto_backend() {
    let json = r#"{"backend": "auto"}"#;
    let config: BeadCliConfig = serde_json::from_str(json).unwrap();

    assert_eq!(config.backend, BeadBackend::Auto);
    assert!(config.path.is_none());
}

#[test]
fn test_deserialize_bead_forge_backend() {
    let json = r#"{"backend": "br"}"#;
    let config: BeadCliConfig = serde_json::from_str(json).unwrap();

    assert_eq!(config.backend, BeadBackend::Br);
    assert!(config.path.is_none());
}

#[test]
fn test_deserialize_bf_alias() {
    let json = r#"{"backend": "bf"}"#;
    let config: BeadCliConfig = serde_json::from_str(json).unwrap();

    assert_eq!(config.backend, BeadBackend::Br);
    assert!(config.path.is_none());
}

#[test]
fn test_deserialize_br_backend() {
    let json = r#"{"backend": "br"}"#;
    let config: BeadCliConfig = serde_json::from_str(json).unwrap();

    assert_eq!(config.backend, BeadBackend::Br);
    assert!(config.path.is_none());
}

#[test]
fn test_deserialize_bead_rs_backend() {
    let json = r#"{"backend": "bead-rs"}"#;
    let config: BeadCliConfig = serde_json::from_str(json).unwrap();

    assert_eq!(config.backend, BeadBackend::Bead);
    assert!(config.path.is_none());
}

#[test]
fn test_deserialize_bead_alias() {
    let json = r#"{"backend": "bead"}"#;
    let config: BeadCliConfig = serde_json::from_str(json).unwrap();

    assert_eq!(config.backend, BeadBackend::Bead);
    assert!(config.path.is_none());
}

#[test]
fn test_deserialize_with_path() {
    let json = r#"{"backend": "auto", "path": "/custom/path/bf"}"#;
    let config: BeadCliConfig = serde_json::from_str(json).unwrap();

    assert_eq!(config.backend, BeadBackend::Auto);
    assert_eq!(config.path, Some(PathBuf::from("/custom/path/bf")));
}

#[test]
fn test_deserialize_explicit_path_alias() {
    // Test the "explicit_path" alias for the path field
    let json = r#"{"backend": "br", "explicit_path": "/usr/bin/bf"}"#;
    let config: BeadCliConfig = serde_json::from_str(json).unwrap();

    assert_eq!(config.backend, BeadBackend::Br);
    assert_eq!(config.path, Some(PathBuf::from("/usr/bin/bf")));
}

#[test]
fn test_round_trip_auto_backend() {
    let original = BeadCliConfig {
        backend: BeadBackend::Auto,
        path: None,
    };

    let json = serde_json::to_string(&original).unwrap();
    let deserialized: BeadCliConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(original, deserialized);
}

#[test]
fn test_round_trip_bf_backend_with_path() {
    let original = BeadCliConfig {
        backend: BeadBackend::Br,
        path: Some(PathBuf::from("/opt/bin/bf")),
    };

    let json = serde_json::to_string(&original).unwrap();
    let deserialized: BeadCliConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(original, deserialized);
}

#[test]
fn test_round_trip_bead_backend() {
    let original = BeadCliConfig {
        backend: BeadBackend::Bead,
        path: Some(PathBuf::from("/usr/local/bin/bead")),
    };

    let json = serde_json::to_string(&original).unwrap();
    let deserialized: BeadCliConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(original, deserialized);
}

#[test]
fn test_round_trip_br_backend() {
    let original = BeadCliConfig {
        backend: BeadBackend::Br,
        path: None,
    };

    let json = serde_json::to_string(&original).unwrap();
    let deserialized: BeadCliConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(original, deserialized);
}

#[test]
fn test_deserialize_with_missing_backend_uses_default() {
    // When backend is missing, it should use the default (Auto)
    let json = r#"{}"#;
    let config: BeadCliConfig = serde_json::from_str(json).unwrap();

    assert_eq!(config.backend, BeadBackend::Auto);
    assert!(config.path.is_none());
}

#[test]
fn test_deserialize_with_missing_path_uses_default() {
    // When path is missing, it should default to None
    let json = r#"{"backend": "bead-rs"}"#;
    let config: BeadCliConfig = serde_json::from_str(json).unwrap();

    assert_eq!(config.backend, BeadBackend::Bead);
    assert!(config.path.is_none());
}

#[test]
fn test_default_config_serializes_correctly() {
    let config = BeadCliConfig::default();

    let json = serde_json::to_string(&config).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed["backend"], "auto");
    assert!(parsed.get("path").is_none());
}

#[test]
fn test_complex_path_round_trip() {
    // Test with a more complex path containing special characters
    let path_str = "/usr/local/bin/my bead cli";
    let original = BeadCliConfig {
        backend: BeadBackend::Bead,
        path: Some(PathBuf::from(path_str)),
    };

    let json = serde_json::to_string(&original).unwrap();
    let deserialized: BeadCliConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(original, deserialized);
    assert_eq!(deserialized.path, Some(PathBuf::from(path_str)));
}

#[test]
fn test_all_backend_variants_serializable() {
    // Test that all backend variants can be serialized
    let backends = vec![BeadBackend::Auto, BeadBackend::Br, BeadBackend::Bead];

    for backend in backends {
        let config = BeadCliConfig {
            backend,
            path: None,
        };
        let json = serde_json::to_string(&config).unwrap();
        let _parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    }
}

#[test]
fn test_all_backend_variants_deserializable() {
    // Test that all backend variants can be deserialized
    let backend_strings = vec![
        ("auto", BeadBackend::Auto),
        ("br", BeadBackend::Br),
        ("bead-rs", BeadBackend::Bead),
        ("bead", BeadBackend::Bead),
    ];

    for (json_str, expected_backend) in backend_strings {
        let json = format!(r#"{{"backend": "{}"}}"#, json_str);
        let config: BeadCliConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.backend, expected_backend);
    }
}

#[test]
fn test_round_trip_all_backend_path_combinations() {
    // Comprehensive round-trip test covering all backend variants with both path states
    let test_cases = vec![
        // Auto backend
        (BeadBackend::Auto, None, "auto with no path"),
        (
            BeadBackend::Auto,
            Some(PathBuf::from("/usr/local/bin/bf")),
            "auto with path",
        ),
        // Br backend
        (BeadBackend::Br, None, "br with no path"),
        (
            BeadBackend::Br,
            Some(PathBuf::from("/opt/bin/bf")),
            "br with path",
        ),
        // Bead backend
        (BeadBackend::Bead, None, "bead with no path"),
        (
            BeadBackend::Bead,
            Some(PathBuf::from("/usr/local/bin/bead")),
            "bead with path",
        ),
    ];

    for (backend, path, description) in test_cases {
        let original = BeadCliConfig {
            backend: backend.clone(),
            path: path.clone(),
        };

        // Serialize
        let json = serde_json::to_string(&original).unwrap();

        // Deserialize
        let deserialized: BeadCliConfig = serde_json::from_str(&json).unwrap();

        // Verify all fields are preserved
        assert_eq!(
            deserialized.backend, original.backend,
            "{}: backend mismatch",
            description
        );
        assert_eq!(
            deserialized.path, original.path,
            "{}: path mismatch",
            description
        );

        // Verify full equality
        assert_eq!(
            original, deserialized,
            "{}: full config mismatch",
            description
        );
    }
}

// ============================================================================
// YAML Format Tests
// ============================================================================

#[test]
fn test_yaml_serialize_auto_backend() {
    let config = BeadCliConfig {
        backend: BeadBackend::Auto,
        path: None,
    };

    let yaml = serde_yaml::to_string(&config).unwrap();
    let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();

    assert_eq!(parsed["backend"], "auto");
    assert!(parsed.get("path").is_none());
}

#[test]
fn test_yaml_serialize_bead_backend() {
    let config = BeadCliConfig {
        backend: BeadBackend::Bead,
        path: Some(PathBuf::from("/usr/local/bin/bead")),
    };

    let yaml = serde_yaml::to_string(&config).unwrap();
    let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();

    assert_eq!(parsed["backend"], "bead-rs");
    assert_eq!(parsed["path"], "/usr/local/bin/bead");
}

#[test]
fn test_yaml_deserialize_auto_backend() {
    let yaml = "backend: auto";
    let config: BeadCliConfig = serde_yaml::from_str(yaml).unwrap();

    assert_eq!(config.backend, BeadBackend::Auto);
    assert!(config.path.is_none());
}

#[test]
fn test_yaml_deserialize_bead_rs_backend() {
    let yaml = "backend: bead-rs";
    let config: BeadCliConfig = serde_yaml::from_str(yaml).unwrap();

    assert_eq!(config.backend, BeadBackend::Bead);
    assert!(config.path.is_none());
}

#[test]
fn test_yaml_deserialize_with_path() {
    let yaml = "backend: auto\npath: /custom/path/bf";
    let config: BeadCliConfig = serde_yaml::from_str(yaml).unwrap();

    assert_eq!(config.backend, BeadBackend::Auto);
    assert_eq!(config.path, Some(PathBuf::from("/custom/path/bf")));
}

#[test]
fn test_yaml_round_trip_all_backends() {
    let test_cases = vec![
        (BeadBackend::Auto, None::<PathBuf>),
        (BeadBackend::Auto, Some(PathBuf::from("/usr/bin/bf"))),
        (BeadBackend::Br, None::<PathBuf>),
        (BeadBackend::Br, Some(PathBuf::from("/opt/bin/bf"))),
        (BeadBackend::Bead, None::<PathBuf>),
        (
            BeadBackend::Bead,
            Some(PathBuf::from("/usr/local/bin/bead")),
        ),
    ];

    for (backend, path) in test_cases {
        let original = BeadCliConfig {
            backend: backend.clone(),
            path: path.clone(),
        };

        let yaml = serde_yaml::to_string(&original).unwrap();
        let deserialized: BeadCliConfig = serde_yaml::from_str(&yaml).unwrap();

        assert_eq!(deserialized.backend, original.backend);
        assert_eq!(deserialized.path, original.path);
        assert_eq!(original, deserialized);
    }
}

#[test]
fn test_yaml_default_config() {
    let config = BeadCliConfig::default();

    let yaml = serde_yaml::to_string(&config).unwrap();
    let deserialized: BeadCliConfig = serde_yaml::from_str(&yaml).unwrap();

    assert_eq!(config, deserialized);
}

// ============================================================================
// TOML Format Tests
// ============================================================================

#[test]
fn test_toml_serialize_auto_backend() {
    let config = BeadCliConfig {
        backend: BeadBackend::Auto,
        path: None,
    };

    let toml = toml::to_string_pretty(&config).unwrap();
    let parsed: toml::Value = toml::from_str(&toml).unwrap();

    assert_eq!(parsed["backend"], "auto".into());
    assert!(parsed.get("path").is_none());
}

#[test]
fn test_toml_serialize_bead_backend_with_path() {
    let config = BeadCliConfig {
        backend: BeadBackend::Bead,
        path: Some(PathBuf::from("/usr/local/bin/bead")),
    };

    let toml = toml::to_string_pretty(&config).unwrap();
    let parsed: toml::Value = toml::from_str(&toml).unwrap();

    assert_eq!(parsed["backend"], "bead-rs".into());
    assert_eq!(parsed["path"], "/usr/local/bin/bead".into());
}

#[test]
fn test_toml_deserialize_auto_backend() {
    let toml = r#"backend = "auto""#;
    let config: BeadCliConfig = toml::from_str(toml).unwrap();

    assert_eq!(config.backend, BeadBackend::Auto);
    assert!(config.path.is_none());
}

#[test]
fn test_toml_deserialize_br_backend() {
    let toml = r#"backend = "br""#;
    let config: BeadCliConfig = toml::from_str(toml).unwrap();

    assert_eq!(config.backend, BeadBackend::Br);
    assert!(config.path.is_none());
}

#[test]
fn test_toml_deserialize_bead_rs_backend() {
    let toml = r#"backend = "bead-rs""#;
    let config: BeadCliConfig = toml::from_str(toml).unwrap();

    assert_eq!(config.backend, BeadBackend::Bead);
    assert!(config.path.is_none());
}

#[test]
fn test_toml_deserialize_with_path() {
    let toml = r#"
backend = "auto"
path = "/custom/path/bf"
"#;
    let config: BeadCliConfig = toml::from_str(toml).unwrap();

    assert_eq!(config.backend, BeadBackend::Auto);
    assert_eq!(config.path, Some(PathBuf::from("/custom/path/bf")));
}

#[test]
fn test_toml_round_trip_all_backends() {
    let test_cases = vec![
        (BeadBackend::Auto, None::<PathBuf>),
        (BeadBackend::Auto, Some(PathBuf::from("/usr/bin/bf"))),
        (BeadBackend::Br, None::<PathBuf>),
        (BeadBackend::Br, Some(PathBuf::from("/opt/bin/bf"))),
        (BeadBackend::Bead, None::<PathBuf>),
        (
            BeadBackend::Bead,
            Some(PathBuf::from("/usr/local/bin/bead")),
        ),
    ];

    for (backend, path) in test_cases {
        let original = BeadCliConfig {
            backend: backend.clone(),
            path: path.clone(),
        };

        let toml_str = toml::to_string_pretty(&original).unwrap();
        let deserialized: BeadCliConfig = toml::from_str(&toml_str).unwrap();

        assert_eq!(deserialized.backend, original.backend);
        assert_eq!(deserialized.path, original.path);
        assert_eq!(original, deserialized);
    }
}

#[test]
fn test_toml_default_config() {
    let config = BeadCliConfig::default();

    let toml_str = toml::to_string_pretty(&config).unwrap();
    let deserialized: BeadCliConfig = toml::from_str(&toml_str).unwrap();

    assert_eq!(config, deserialized);
}

// ============================================================================
// Cross-Format Consistency Tests
// ============================================================================

#[test]
fn test_cross_format_consistency_auto_backend() {
    let config = BeadCliConfig {
        backend: BeadBackend::Auto,
        path: None,
    };

    let json = serde_json::to_string(&config).unwrap();
    let yaml = serde_yaml::to_string(&config).unwrap();
    let toml = toml::to_string_pretty(&config).unwrap();

    let from_json: BeadCliConfig = serde_json::from_str(&json).unwrap();
    let from_yaml: BeadCliConfig = serde_yaml::from_str(&yaml).unwrap();
    let from_toml: BeadCliConfig = toml::from_str(&toml).unwrap();

    assert_eq!(config, from_json);
    assert_eq!(config, from_yaml);
    assert_eq!(config, from_toml);
    assert_eq!(from_json, from_yaml);
    assert_eq!(from_json, from_toml);
    assert_eq!(from_yaml, from_toml);
}

#[test]
fn test_cross_format_consistency_with_path() {
    let config = BeadCliConfig {
        backend: BeadBackend::Bead,
        path: Some(PathBuf::from("/usr/local/bin/bead")),
    };

    let json = serde_json::to_string(&config).unwrap();
    let yaml = serde_yaml::to_string(&config).unwrap();
    let toml = toml::to_string_pretty(&config).unwrap();

    let from_json: BeadCliConfig = serde_json::from_str(&json).unwrap();
    let from_yaml: BeadCliConfig = serde_yaml::from_str(&yaml).unwrap();
    let from_toml: BeadCliConfig = toml::from_str(&toml).unwrap();

    assert_eq!(config, from_json);
    assert_eq!(config, from_yaml);
    assert_eq!(config, from_toml);
    assert_eq!(from_json, from_yaml);
    assert_eq!(from_json, from_toml);
    assert_eq!(from_yaml, from_toml);
}

#[test]
fn test_all_formats_handle_missing_backend_uses_default() {
    // JSON
    let json_config: BeadCliConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(json_config.backend, BeadBackend::Auto);

    // YAML
    let yaml_config: BeadCliConfig = serde_yaml::from_str("").unwrap();
    assert_eq!(yaml_config.backend, BeadBackend::Auto);

    // TOML - empty table
    let toml_config: BeadCliConfig = toml::from_str("").unwrap();
    assert_eq!(toml_config.backend, BeadBackend::Auto);
}

#[test]
fn test_all_formats_preserve_special_path_characters() {
    let path_str = "/usr/local/bin/my bead cli-v2.0";
    let config = BeadCliConfig {
        backend: BeadBackend::Bead,
        path: Some(PathBuf::from(path_str)),
    };

    // JSON
    let json = serde_json::to_string(&config).unwrap();
    let from_json: BeadCliConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(from_json.path, Some(PathBuf::from(path_str)));

    // YAML
    let yaml = serde_yaml::to_string(&config).unwrap();
    let from_yaml: BeadCliConfig = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(from_yaml.path, Some(PathBuf::from(path_str)));

    // TOML
    let toml = toml::to_string_pretty(&config).unwrap();
    let from_toml: BeadCliConfig = toml::from_str(&toml).unwrap();
    assert_eq!(from_toml.path, Some(PathBuf::from(path_str)));
}
