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
        backend: BeadBackend::Bf,
        path: None,
    };

    let json = serde_json::to_string(&config).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed["backend"], "bead-forge");
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
    let json = r#"{"backend": "bead-forge"}"#;
    let config: BeadCliConfig = serde_json::from_str(json).unwrap();

    assert_eq!(config.backend, BeadBackend::Bf);
    assert!(config.path.is_none());
}

#[test]
fn test_deserialize_bf_alias() {
    let json = r#"{"backend": "bf"}"#;
    let config: BeadCliConfig = serde_json::from_str(json).unwrap();

    assert_eq!(config.backend, BeadBackend::Bf);
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
    let json = r#"{"backend": "bead-forge", "explicit_path": "/usr/bin/bf"}"#;
    let config: BeadCliConfig = serde_json::from_str(json).unwrap();

    assert_eq!(config.backend, BeadBackend::Bf);
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
        backend: BeadBackend::Bf,
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
    let backends = vec![
        BeadBackend::Auto,
        BeadBackend::Bf,
        BeadBackend::Br,
        BeadBackend::Bead,
    ];

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
        ("bead-forge", BeadBackend::Bf),
        ("bf", BeadBackend::Bf),
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
